//! Live recognition fed the way the recorder feeds it: 100 ms blocks with the
//! index of their first sample.

use std::sync::Arc;
use std::time::{Duration, Instant};

use mc_asr::{
    AsrError, EngineFactory, FakeAsr, LiveAsr, LiveAsrEvent, LiveConfig, RawSegment, Recognized,
    TranscribeCtx, Transcriber, read_asr_jsonl,
};
use mc_audio::AudioChunk;

fn speech(ms: usize, from: usize) -> Vec<i16> {
    (from..from + ms * 16)
        .map(|n| (0.3 * (n as f32 * 0.25).sin() * 32767.0) as i16)
        .collect()
}

/// Sends `samples` as 100 ms blocks starting at `start`; returns the next index.
fn feed(live: &LiveAsr, ch: &str, start: u64, samples: &[i16]) -> u64 {
    let tx = live.sender(ch).expect("channel exists");
    let mut at = start;
    for block in samples.chunks(1600) {
        tx.send(AudioChunk {
            ch: ch.to_string(),
            start_frame: at,
            samples: block.to_vec(),
        })
        .unwrap();
        at += block.len() as u64;
    }
    at
}

fn numbered() -> EngineFactory {
    Arc::new(|_| Ok(Box::new(FakeAsr::numbered()) as Box<dyn Transcriber + Send>))
}

fn collect(live: &LiveAsr, want: usize) -> Vec<LiveAsrEvent> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut all = Vec::new();
    while all
        .iter()
        .filter(|e| matches!(e, LiveAsrEvent::Segment(_)))
        .count()
        < want
    {
        assert!(Instant::now() < deadline, "timed out; got {all:?}");
        all.extend(live.poll());
        std::thread::sleep(Duration::from_millis(5));
    }
    all
}

fn segments(events: &[LiveAsrEvent]) -> Vec<&mc_asr::AsrSegment> {
    events
        .iter()
        .filter_map(|e| match e {
            LiveAsrEvent::Segment(s) => Some(s),
            _ => None,
        })
        .collect()
}

#[test]
fn utterances_are_recognised_on_the_session_timeline_and_saved_as_they_arrive() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("asr_live.jsonl");
    let cfg = LiveConfig {
        jsonl: Some(path.clone()),
        ..LiveConfig::default()
    };
    let live = LiveAsr::start(&["mic"], numbered(), cfg);
    let mut at = feed(&live, "mic", 0, &vec![0; 8000]); // 0.5 s of silence
    at = feed(&live, "mic", at, &speech(1000, 0));
    at = feed(&live, "mic", at, &vec![0; 24_000]);
    at = feed(&live, "mic", at, &speech(800, 0));
    feed(&live, "mic", at, &vec![0; 16_000]);

    let events = collect(&live, 2);
    let found = segments(&events);
    assert_eq!(found.len(), 2);
    assert_eq!(
        (found[0].text.as_str(), found[0].ch.as_str()),
        ("utterance 1", "mic")
    );
    assert!((found[0].start_ms - 300.0).abs() <= 40.0, "{:?}", found[0]);
    assert!(
        (found[1].start_ms - 2_800.0).abs() <= 40.0,
        "{:?}",
        found[1]
    );

    let (summary, rest) = live.stop(Duration::from_secs(2));
    assert_eq!(summary.segments, 2);
    assert!(!summary.stuck && summary.dropped_utterances == 0);
    assert!(segments(&rest).is_empty());
    assert_eq!(read_asr_jsonl(&path).unwrap().len(), 2);
}

#[test]
fn a_hole_in_the_audio_ends_the_utterance_and_keeps_later_times_right() {
    let live = LiveAsr::start(&["system"], numbered(), LiveConfig::default());
    let at = feed(&live, "system", 0, &speech(900, 0));
    // The recorder padded 20 s of silence the consumer never sees.
    let start = at + 20 * 16_000;
    feed(&live, "system", start, &speech(900, 0));
    feed(&live, "system", start + 14_400, &vec![0; 16_000]);
    let events = collect(&live, 2);
    let found = segments(&events);
    assert!(found[0].end_ms < 1_300.0, "{:?}", found[0]);
    assert!(
        (found[1].start_ms - (20_900.0)).abs() <= 400.0,
        "{:?}",
        found[1]
    );
    live.stop(Duration::from_secs(2));
}

#[test]
fn channels_are_independent() {
    let live = LiveAsr::start(&["mic", "system"], numbered(), LiveConfig::default());
    for ch in ["mic", "system"] {
        let at = feed(&live, ch, 0, &speech(800, 0));
        feed(&live, ch, at, &vec![0; 16_000]);
    }
    let events = collect(&live, 2);
    let mut channels: Vec<&str> = segments(&events).iter().map(|s| s.ch.as_str()).collect();
    channels.sort_unstable();
    assert_eq!(channels, ["mic", "system"]);
    assert!(live.sender("radio").is_none());
    live.stop(Duration::from_secs(2));
}

struct Slow(Duration);

impl Transcriber for Slow {
    fn engine(&self) -> &str {
        "slow"
    }
    fn model(&self) -> &str {
        "slow"
    }
    fn transcribe(
        &mut self,
        audio: &[f32],
        _ctx: &TranscribeCtx<'_>,
    ) -> Result<Recognized, AsrError> {
        std::thread::sleep(self.0);
        Ok(vec![RawSegment::text(0.0, audio.len() as f64 / 16.0, "lento")].into())
    }
}

#[test]
fn an_engine_that_cannot_keep_up_drops_utterances_instead_of_growing_a_backlog() {
    let cfg = LiveConfig {
        queue: 1,
        ..LiveConfig::default()
    };
    let factory: EngineFactory =
        Arc::new(|_| Ok(Box::new(Slow(Duration::from_millis(400))) as Box<dyn Transcriber + Send>));
    let live = LiveAsr::start(&["mic"], factory, cfg);
    let mut at = 0;
    for _ in 0..8 {
        at = feed(&live, "mic", at, &speech(500, 0));
        at = feed(&live, "mic", at, &vec![0; 16_000]);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut dropped = 0;
    let mut got = 0;
    while Instant::now() < deadline && got + dropped < 8 {
        for e in live.poll() {
            match e {
                LiveAsrEvent::Dropped { ch, ms } => {
                    assert_eq!(ch, "mic");
                    assert!(ms > 400.0);
                    dropped += 1;
                }
                LiveAsrEvent::Segment(_) => got += 1,
                LiveAsrEvent::Error { message, .. } => panic!("{message}"),
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(dropped >= 3, "dropped {dropped}, recognised {got}");
    assert!(got >= 1);
    let (summary, _) = live.stop(Duration::from_secs(2));
    assert_eq!(summary.dropped_utterances as usize, dropped);
}

#[test]
fn stopping_does_not_wait_for_a_blocked_engine() {
    let factory: EngineFactory =
        Arc::new(|_| Ok(Box::new(Slow(Duration::from_secs(3))) as Box<dyn Transcriber + Send>));
    let live = LiveAsr::start(&["mic"], factory, LiveConfig::default());
    let at = feed(&live, "mic", 0, &speech(800, 0));
    feed(&live, "mic", at, &vec![0; 16_000]);
    std::thread::sleep(Duration::from_millis(500)); // the engine is inside the call
    let started = Instant::now();
    let (summary, _) = live.stop(Duration::from_millis(300));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(summary.stuck);
}

#[test]
fn an_engine_that_fails_to_load_is_reported_and_nothing_crashes() {
    let factory: EngineFactory = Arc::new(|_| Err(AsrError::Engine("no model".into())));
    let live = LiveAsr::start(&["mic"], factory, LiveConfig::default());
    let at = feed(&live, "mic", 0, &speech(800, 0));
    feed(&live, "mic", at, &vec![0; 16_000]);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut errors = Vec::new();
    while errors.is_empty() && Instant::now() < deadline {
        errors.extend(
            live.poll()
                .into_iter()
                .filter(|e| matches!(e, LiveAsrEvent::Error { .. })),
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        matches!(&errors[0], LiveAsrEvent::Error { ch, message } if ch == "mic" && message.contains("no model"))
    );
    let (summary, _) = live.stop(Duration::from_secs(2));
    assert_eq!(summary.segments, 0);
}

#[test]
fn memory_stays_bounded_over_a_long_stretch_of_silence() {
    let live = LiveAsr::start(&["mic"], numbered(), LiveConfig::default());
    let mut at = 0;
    for _ in 0..600 {
        // Ten minutes of silence in 1 s pieces.
        at = feed(&live, "mic", at, &vec![0; 16_000]);
    }
    let (summary, rest) = live.stop(Duration::from_secs(5));
    assert_eq!(summary.segments, 0);
    assert!(rest.is_empty());
}

struct Panicking;

impl Transcriber for Panicking {
    fn engine(&self) -> &str {
        "panicking"
    }
    fn model(&self) -> &str {
        "none"
    }
    fn transcribe(
        &mut self,
        _samples: &[f32],
        _ctx: &TranscribeCtx,
    ) -> Result<Recognized, AsrError> {
        panic!("engine blew up");
    }
}

#[test]
fn an_engine_panic_becomes_an_error_event_and_stop_still_returns() {
    let factory: EngineFactory =
        Arc::new(|_| Ok(Box::new(Panicking) as Box<dyn Transcriber + Send>));
    let live = LiveAsr::start(&["mic"], factory, LiveConfig::default());
    let at = feed(&live, "mic", 0, &speech(800, 0));
    feed(&live, "mic", at, &vec![0; 16_000]);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut errors = Vec::new();
    while errors.is_empty() && Instant::now() < deadline {
        errors.extend(
            live.poll()
                .into_iter()
                .filter(|e| matches!(e, LiveAsrEvent::Error { .. })),
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!errors.is_empty(), "the panic was not reported");
    let (summary, _) = live.stop(Duration::from_secs(2));
    assert!(!summary.stuck);
    assert_eq!(summary.segments, 0);
}

#[test]
fn audio_still_queued_when_stopping_is_recognised_not_lost() {
    let live = LiveAsr::start(&["mic"], numbered(), LiveConfig::default());
    let at = feed(&live, "mic", 0, &speech(900, 0));
    feed(&live, "mic", at, &vec![0; 16_000]);
    // Stop immediately: the utterance may still be in the channel or the queue.
    let (summary, rest) = live.stop(Duration::from_secs(3));
    let found = segments(&rest).len() as u64;
    assert_eq!(summary.segments, 1, "{summary:?}");
    assert!(found <= 1);
    assert!(!summary.stuck);
}
