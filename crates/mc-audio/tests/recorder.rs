//! The channel threads and the corpus recorder against scripted sources.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use mc_audio::source::{AudioSource, Read, StreamInfo, sine_block};
use mc_audio::wav::read_wav;
use mc_audio::{
    AudioBackend, AudioCorpusRecorder, AudioError, ChannelConfig, FakeAudio, FakeBackend,
    SilenceAlert, Step,
};
use mc_core::clock::{Clock, FakeClock};
use serde_json::Value;

fn clock() -> (Arc<FakeClock>, Arc<dyn Clock>) {
    let fake = Arc::new(FakeClock::new(1_700_000_000_000));
    let dynamic: Arc<dyn Clock> = fake.clone();
    (fake, dynamic)
}

fn block(frames: usize, level: f32, advance_ms: f64) -> Step {
    Step::Block {
        data: vec![level; frames],
        advance_ms,
    }
}

fn fast_config() -> ChannelConfig {
    ChannelConfig {
        reopen_wait: Duration::from_millis(1),
        sync_interval_ms: 1_000_000.0,
        ..ChannelConfig::default()
    }
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn events(dir: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join("audio_sync.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn names(events: &[Value], ch: &str) -> Vec<String> {
    events
        .iter()
        .filter(|e| e["ch"] == ch)
        .map(|e| e["event"].as_str().unwrap().to_string())
        .collect()
}

fn recorder(
    dir: &std::path::Path,
    clock: Arc<dyn Clock>,
    backend: impl AudioBackend + 'static,
    cfg: ChannelConfig,
) -> AudioCorpusRecorder {
    AudioCorpusRecorder::new(dir, clock, Some(Arc::new(backend)), cfg)
}

fn mic_frames(rec: &AudioCorpusRecorder) -> u64 {
    rec.info()["mic"]["frames_written"].as_u64().unwrap_or(0)
}

#[test]
fn records_on_the_timeline_pads_gaps_and_reports_manifest_info() {
    let dir = tempfile::tempdir().unwrap();
    let (fake, dynamic) = clock();
    let mic = FakeAudio::mono(
        16_000,
        vec![
            block(1600, 0.5, 100.0),
            block(1600, 0.5, 100.0),
            // A 1 s stall before the third block.
            block(1600, 0.5, 1100.0),
        ],
        Some(fake),
    );
    let backend = FakeBackend::new(
        vec![Ok(mic)],
        vec![Err(AudioError::Unavailable("no loopback".into()))],
    );
    let (tx, rx) = mpsc::sync_channel(16);
    let mut rec = recorder(dir.path(), dynamic, backend, fast_config()).tap("mic", tx);
    rec.start().unwrap();
    wait_for("20800 frames", || mic_frames(&rec) == 20_800);
    assert!(rec.recording());
    let info = rec.stop(Duration::from_secs(3));
    assert!(!rec.recording());

    let mic = &info["mic"];
    assert_eq!(mic["status"], "ok");
    assert_eq!(mic["frames_written"], 20_800);
    assert_eq!(mic["padded_frames"], 16_000);
    assert_eq!(mic["samplerate"], 16_000);
    assert_eq!(mic["device_samplerate"], 16_000);
    assert_eq!(mic["file"], "mic.wav");
    assert_eq!(mic["sample_format"], "s16le");
    assert_eq!(mic["latency_ms"], 10.0);
    assert_eq!(info["system"]["status"], "unavailable");
    assert_eq!(info["system"]["error"], "no loopback");

    let (rate, samples) = read_wav(&dir.path().join("mic.wav")).unwrap();
    assert_eq!(rate, 16_000);
    assert_eq!(samples.len(), 20_800);
    assert!(samples[..3200].iter().all(|s| *s == 16_383));
    assert!(samples[3200..19_200].iter().all(|s| *s == 0));
    assert!(samples[19_200..].iter().all(|s| *s == 16_383));

    let log = events(dir.path());
    assert_eq!(names(&log, "mic"), ["start", "gap", "stop"]);
    let gap = log.iter().find(|e| e["event"] == "gap").unwrap();
    assert_eq!(gap["pad_frames"], 16_000);
    assert_eq!(gap["frames_written"], 20_800);
    assert!(gap["mono_ms"].is_number() && gap["wall_ms"].is_number());
    assert_eq!(names(&log, "system"), ["error", "stop"]);

    let chunks: Vec<_> = rx.try_iter().collect();
    assert_eq!(
        chunks.iter().map(|c| c.start_frame).collect::<Vec<_>>(),
        [0, 1600, 19_200]
    );
    assert!(
        chunks
            .iter()
            .all(|c| c.ch == "mic" && c.samples.len() == 1600)
    );
}

#[test]
fn resamples_a_stereo_48k_device_to_the_wav_rate() {
    let dir = tempfile::tempdir().unwrap();
    let (fake, dynamic) = clock();
    let steps = (0..10)
        .map(|n| Step::Block {
            data: sine_block(48_000, 2, 1000.0, 0.5, n * 4800, 4800),
            advance_ms: 100.0,
        })
        .collect();
    let info = StreamInfo {
        name: "stereo".into(),
        device_id: None,
        samplerate: 48_000,
        channels: 2,
        latency_s: None,
    };
    let backend = FakeBackend::new(vec![Ok(FakeAudio::new(info, steps, Some(fake)))], vec![]);
    let mut rec = recorder(dir.path(), dynamic, backend, fast_config());
    rec.start().unwrap();
    wait_for("resampled audio", || mic_frames(&rec) >= 15_000);
    let info = rec.stop(Duration::from_secs(3));

    assert_eq!(info["mic"]["samplerate"], 16_000);
    assert_eq!(info["mic"]["device_samplerate"], 48_000);
    assert_eq!(info["mic"]["channels_in"], 2);
    let (rate, samples) = read_wav(&dir.path().join("mic.wav")).unwrap();
    assert_eq!(rate, 16_000);
    // The filter delays the last ~1.5 ms; the first block is padded by about that much.
    assert!(
        (15_900..=16_020).contains(&samples.len()),
        "{}",
        samples.len()
    );
    assert!(info["mic"]["padded_frames"].as_u64().unwrap() <= 40);
    let rms = (samples[2000..14_000]
        .iter()
        .map(|s| (f64::from(*s) / 32767.0).powi(2))
        .sum::<f64>()
        / 12_000.0)
        .sqrt();
    assert!((rms - 0.5 / 2f64.sqrt()).abs() < 0.02, "{rms}");
}

#[test]
fn reopens_a_lost_device_even_if_its_format_changes() {
    let dir = tempfile::tempdir().unwrap();
    let (fake, dynamic) = clock();
    let first = FakeAudio::mono(
        16_000,
        vec![
            block(1600, 0.25, 100.0),
            Step::Fail("device unplugged".into()),
        ],
        Some(fake.clone()),
    );
    // The replacement runs at 8 kHz: only the conversion changes, not the timeline.
    let second = FakeAudio::mono(8_000, vec![block(800, 0.25, 100.0)], Some(fake));
    let backend = FakeBackend::new(vec![Ok(first), Ok(second)], vec![]);
    let mut rec = recorder(dir.path(), dynamic, backend, fast_config());
    rec.start().unwrap();
    wait_for("both devices", || mic_frames(&rec) >= 3_000);
    let info = rec.stop(Duration::from_secs(3));

    assert_eq!(info["mic"]["status"], "ok");
    assert_eq!(info["mic"]["device_samplerate"], 8_000);
    let log = events(dir.path());
    let mic = names(&log, "mic");
    assert_eq!(mic, ["start", "error", "reopen", "stop"]);
    let error = log
        .iter()
        .find(|e| e["event"] == "error" && e["ch"] == "mic");
    assert!(
        error.unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("device unplugged")
    );
}

#[test]
fn gives_up_after_the_reopen_budget() {
    let dir = tempfile::tempdir().unwrap();
    let (_, dynamic) = clock();
    let first = FakeAudio::mono(16_000, vec![Step::Fail("gone".into())], None);
    let lost = || Err(AudioError::Device("busy".into()));
    let backend = FakeBackend::new(vec![Ok(first), lost(), lost(), lost()], vec![]);
    let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&seen);
    let mut rec = recorder(dir.path(), dynamic, backend, fast_config()).on_status(Arc::new(
        move |m: &str| sink.lock().unwrap().push(m.to_string()),
    ));
    rec.start().unwrap();
    wait_for("failure", || rec.info()["mic"]["status"] == "failed");
    let info = rec.stop(Duration::from_secs(3));

    assert_eq!(
        info["mic"]["error"],
        "audio device lost and could not be reopened"
    );
    let reopen_errors = events(dir.path())
        .iter()
        .filter(|e| e["event"] == "reopen" && e["error"] == "busy")
        .count();
    assert_eq!(reopen_errors, 3);
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|m| m.starts_with("Audio mic:"))
    );
}

/// Holds every read until `release`; `entered` says one is under way.
#[derive(Clone, Default)]
struct Gate {
    release: Arc<AtomicBool>,
    entered: Arc<AtomicBool>,
}

struct Blocked(StreamInfo, Gate);

impl AudioSource for Blocked {
    fn info(&self) -> &StreamInfo {
        &self.0
    }

    fn read(&mut self, _n_frames: usize) -> Result<Read, AudioError> {
        // Blocked until the test lets go (or a generous safety limit).
        self.1.entered.store(true, Ordering::Relaxed);
        let limit = Instant::now() + Duration::from_secs(20);
        while !self.1.release.load(Ordering::Relaxed) && Instant::now() < limit {
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(Read::Idle)
    }
}

struct BlockedBackend(Gate);

impl AudioBackend for BlockedBackend {
    fn open_mic(&self, _device: Option<&str>) -> Result<Box<dyn AudioSource>, AudioError> {
        Ok(Box::new(Blocked(
            StreamInfo {
                name: "stuck".into(),
                device_id: None,
                samplerate: 16_000,
                channels: 1,
                latency_s: None,
            },
            self.0.clone(),
        )))
    }

    fn open_system(&self, _device: Option<&str>) -> Result<Box<dyn AudioSource>, AudioError> {
        Err(AudioError::Unavailable("none".into()))
    }
}

#[test]
fn a_blocked_read_is_reported_as_stuck_within_the_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let (_, dynamic) = clock();
    let gate = Gate::default();
    let mut rec = recorder(
        dir.path(),
        dynamic,
        BlockedBackend(gate.clone()),
        fast_config(),
    );
    rec.start().unwrap();
    wait_for("recording", || rec.recording());
    // A stop asked before the read begins ends the channel cleanly instead.
    wait_for("a blocked read", || gate.entered.load(Ordering::Relaxed));
    let started = Instant::now();
    let info = rec.stop(Duration::from_millis(150));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(info["mic"]["status"], "stuck");
    assert_eq!(info["mic"]["stop_timeout"], true);
    assert_eq!(info["system"]["status"], "unavailable");
    // The live view agrees: nothing shows it as recording any more.
    assert!(!rec.recording());
    assert_eq!(rec.info()["mic"]["status"], "stuck");
    // Let the abandoned read end before the temp dir goes away.
    gate.release.store(true, Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(100));
}

#[test]
fn without_a_backend_both_channels_are_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let (_, dynamic) = clock();
    let mut rec = AudioCorpusRecorder::new(dir.path(), dynamic, None, fast_config());
    rec.start().unwrap();
    wait_for("both unavailable", || {
        let i = rec.info();
        i["mic"]["status"] == "unavailable" && i["system"]["status"] == "unavailable"
    });
    let info = rec.stop(Duration::from_secs(1));
    assert_eq!(info["mic"]["error"], "no audio backend available");
    // Single use: a second start changes nothing.
    rec.start().unwrap();
}

#[test]
fn the_wav_header_is_refreshed_at_each_sync_point() {
    let dir = tempfile::tempdir().unwrap();
    let (fake, dynamic) = clock();
    let mic = FakeAudio::mono(
        16_000,
        vec![block(1600, 0.1, 100.0), block(1600, 0.1, 100.0)],
        Some(fake),
    );
    let backend = FakeBackend::new(vec![Ok(mic)], vec![]);
    let cfg = ChannelConfig {
        sync_interval_ms: 150.0,
        ..fast_config()
    };
    let mut rec = recorder(dir.path(), dynamic, backend, cfg);
    rec.start().unwrap();
    wait_for("two blocks", || mic_frames(&rec) == 3_200);
    // Still recording: the WAV on disk already claims the audio up to the last sync.
    let (_, samples) = read_wav(&dir.path().join("mic.wav")).unwrap();
    assert_eq!(samples.len(), 3_200);
    let log = events(dir.path());
    let sync = log.iter().find(|e| e["event"] == "sync").unwrap();
    assert_eq!(sync["frames_written"], 3_200);
    assert_eq!(sync["expected_frames"], 3_200);
    rec.stop(Duration::from_secs(1));
}

#[test]
fn the_resampler_tail_is_written_and_a_bad_block_does_not_kill_the_channel() {
    let dir = tempfile::tempdir().unwrap();
    let (fake, dynamic) = clock();
    let info = StreamInfo {
        name: "stereo".into(),
        device_id: None,
        samplerate: 48_000,
        channels: 2,
        latency_s: None,
    };
    let steps = vec![
        Step::Block {
            data: sine_block(48_000, 2, 500.0, 0.5, 0, 4800),
            advance_ms: 100.0,
        },
        // Odd sample count: not a whole number of stereo frames.
        Step::Block {
            data: vec![0.1; 3],
            advance_ms: 0.0,
        },
        Step::Block {
            data: sine_block(48_000, 2, 500.0, 0.5, 4800, 4800),
            advance_ms: 100.0,
        },
    ];
    let backend = FakeBackend::new(vec![Ok(FakeAudio::new(info, steps, Some(fake)))], vec![]);
    let mut rec = recorder(dir.path(), dynamic, backend, fast_config());
    rec.start().unwrap();
    wait_for("both good blocks", || mic_frames(&rec) >= 3_100);
    let info = rec.stop(Duration::from_secs(3));
    // 9600 input frames -> exactly 3200 output frames once the tail is flushed.
    assert_eq!(info["mic"]["status"], "ok");
    let (_, samples) = read_wav(&dir.path().join("mic.wav")).unwrap();
    assert_eq!(samples.len() as u64, info["mic"]["frames_written"]);
    assert!(
        (3_195..=3_220).contains(&samples.len()),
        "{}",
        samples.len()
    );
    let log = events(dir.path());
    assert!(log.iter().any(|e| {
        e["error"]
            .as_str()
            .is_some_and(|m| m.starts_with("malformed block dropped"))
    }));
}

#[test]
fn dropping_the_recorder_ends_its_threads() {
    let dir = tempfile::tempdir().unwrap();
    let (_, dynamic) = clock();
    let gate = Gate::default();
    let mut rec = recorder(
        dir.path(),
        dynamic,
        BlockedBackend(gate.clone()),
        fast_config(),
    );
    rec.start().unwrap();
    wait_for("recording", || rec.recording());
    let started = Instant::now();
    drop(rec);
    assert!(started.elapsed() < Duration::from_secs(3));
    gate.release.store(true, Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(100));
}

/// A fake backend that explains zeros on the mic like the macOS one does.
struct ExplainsSilence(FakeBackend);

impl AudioBackend for ExplainsSilence {
    fn open_mic(&self, device: Option<&str>) -> Result<Box<dyn AudioSource>, AudioError> {
        self.0.open_mic(device)
    }

    fn open_system(&self, device: Option<&str>) -> Result<Box<dyn AudioSource>, AudioError> {
        self.0.open_system(device)
    }

    fn silence_alert(&self, channel: &str, _system_device: Option<&str>) -> Option<SilenceAlert> {
        (channel == "mic").then(|| SilenceAlert {
            after_s: 0.5,
            message: "mic is all zeros".to_string(),
            playing: None,
        })
    }
}

#[test]
fn a_mic_that_starts_with_exact_zeros_raises_the_backends_warning() {
    let dir = tempfile::tempdir().unwrap();
    let (fake, dynamic) = clock();
    let mic = FakeAudio::mono(
        16_000,
        (0..8).map(|_| block(1600, 0.0, 100.0)).collect(),
        Some(fake),
    );
    let backend = ExplainsSilence(FakeBackend::new(
        vec![Ok(mic)],
        vec![Err(AudioError::Unavailable("no loopback".into()))],
    ));
    let messages = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&messages);
    let mut rec = recorder(dir.path(), dynamic, backend, fast_config())
        .on_status(Arc::new(move |m| seen.lock().unwrap().push(m.to_string())));
    rec.start().unwrap();
    wait_for("12800 frames", || mic_frames(&rec) == 12_800);
    let info = rec.stop(Duration::from_secs(3));

    assert_eq!(info["mic"]["warning"], "mic is all zeros");
    assert!(info["system"].get("warning").is_none());
    let log = events(dir.path());
    assert_eq!(names(&log, "mic"), ["start", "warning", "stop"]);
    let said = messages.lock().unwrap().clone();
    assert!(
        said.contains(&"Audio mic: mic is all zeros".to_string()),
        "{said:?}"
    );
}

#[test]
fn real_audio_never_raises_the_silence_warning() {
    let dir = tempfile::tempdir().unwrap();
    let (fake, dynamic) = clock();
    let mic = FakeAudio::mono(
        16_000,
        (0..8).map(|_| block(1600, 0.25, 100.0)).collect(),
        Some(fake),
    );
    let backend = ExplainsSilence(FakeBackend::new(
        vec![Ok(mic)],
        vec![Err(AudioError::Unavailable("no loopback".into()))],
    ));
    let mut rec = recorder(dir.path(), dynamic, backend, fast_config());
    rec.start().unwrap();
    wait_for("12800 frames", || mic_frames(&rec) == 12_800);
    let info = rec.stop(Duration::from_secs(3));
    assert!(info["mic"].get("warning").is_none());
}

/// Like `ExplainsSilence`, but the warning only counts while `playing` says so.
struct Gated(FakeBackend, bool);

impl AudioBackend for Gated {
    fn open_mic(&self, device: Option<&str>) -> Result<Box<dyn AudioSource>, AudioError> {
        self.0.open_mic(device)
    }

    fn open_system(&self, device: Option<&str>) -> Result<Box<dyn AudioSource>, AudioError> {
        self.0.open_system(device)
    }

    fn silence_alert(&self, channel: &str, _system_device: Option<&str>) -> Option<SilenceAlert> {
        let playing = self.1;
        (channel == "mic").then(|| SilenceAlert {
            after_s: 0.5,
            message: "mic is all zeros".to_string(),
            playing: Some(Arc::new(move || playing)),
        })
    }
}

fn record_mic(
    levels: &[f32],
    backend: impl Fn(FakeAudio) -> Gated,
) -> (Value, Vec<Value>, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let (fake, dynamic) = clock();
    let steps = levels.iter().map(|l| block(1600, *l, 100.0)).collect();
    let mic = FakeAudio::mono(16_000, steps, Some(fake));
    let backend = backend(mic);
    let messages = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&messages);
    let mut rec = recorder(dir.path(), dynamic, backend, fast_config())
        .on_status(Arc::new(move |m| seen.lock().unwrap().push(m.to_string())));
    rec.start().unwrap();
    let frames = levels.len() as u64 * 1600;
    wait_for("the frames", || mic_frames(&rec) == frames);
    let info = rec.stop(Duration::from_secs(3));
    let said = messages.lock().unwrap().clone();
    (info, events(dir.path()), said)
}

fn gated(playing: bool) -> impl Fn(FakeAudio) -> Gated {
    move |mic| {
        Gated(
            FakeBackend::new(
                vec![Ok(mic)],
                vec![Err(AudioError::Unavailable("none".into()))],
            ),
            playing,
        )
    }
}

#[test]
fn the_silence_warning_clears_when_real_audio_arrives() {
    let mut levels = vec![0.0; 8];
    levels.extend([0.25; 2]);
    let (info, log, said) = record_mic(&levels, gated(true));
    assert!(info["mic"].get("warning").is_none());
    assert_eq!(
        names(&log, "mic"),
        ["start", "warning", "warning_cleared", "stop"]
    );
    assert!(
        said.contains(&"Audio mic: audio now flowing".to_string()),
        "{said:?}"
    );
}

#[test]
fn zeros_while_nothing_plays_are_not_a_warning() {
    let (info, log, said) = record_mic(&[0.0; 8], gated(false));
    assert!(info["mic"].get("warning").is_none());
    assert_eq!(names(&log, "mic"), ["start", "stop"]);
    assert!(!said.iter().any(|m| m.starts_with("Audio mic")), "{said:?}");
}
