//! The capture controller end to end with fake sources: recording, stop
//! paths (`q`, STOP file, `--until`, source end) and `Open` coalescing.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mc_captions::{Discovery, FakeSource, FakeStep};
use mc_engine::controller::{Capture, CaptureSpec, RecordSpec, start};
use mc_engine::live::{
    DebounceClock, EndReason, LiveConfig, LiveEvent, LiveSummary, fake_from_corpus, once,
};
use mc_store::manifest::Manifest;

fn corpus(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/corpus")
        .join(name)
}

fn record() -> RecordSpec {
    RecordSpec {
        platform: "fake".to_string(),
        capture_source: "FakeSource".to_string(),
        meeting_name: None,
        consent_method: "flag",
        audio: None,
    }
}

fn cfg(poll: Duration) -> LiveConfig {
    LiveConfig {
        poll,
        rediscover: Duration::from_millis(5),
        ..LiveConfig::default()
    }
}

/// A source that never ends by itself: `frames` one-observation frames, each
/// with its own text so the transcript keeps changing.
fn long_source(frames: usize) -> FakeSource {
    let texts: Vec<String> = (0..frames).map(|i| format!("frase numero {i}")).collect();
    let steps = texts
        .iter()
        .map(|t| vec![(Some("Ana"), Some(t.as_str()))])
        .collect();
    FakeSource::frames(steps)
}

/// Polls until `Ended` arrives (or `limit` passes), collecting every event.
fn run_to_end(
    capture: &Capture,
    limit: Duration,
    poll: Duration,
) -> (Vec<LiveEvent>, Option<Instant>) {
    let started = Instant::now();
    let mut all = Vec::new();
    let mut ended_at = None;
    while started.elapsed() < limit {
        let events = capture.poll(poll);
        if events.iter().any(|e| matches!(e, LiveEvent::Ended { .. })) {
            ended_at = Some(Instant::now());
            all.extend(events);
            break;
        }
        all.extend(events);
    }
    (all, ended_at)
}

/// Polls until the capture reports its session folder (the meeting was found).
fn session_dir_of(capture: &Capture) -> PathBuf {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(10) {
        capture.poll(Duration::from_millis(10));
        if let Some(dir) = capture.session_dir() {
            return dir.to_path_buf();
        }
    }
    panic!("the session folder never appeared");
}

fn ended_reason(events: &[LiveEvent]) -> Option<EndReason> {
    events.iter().find_map(|e| match e {
        LiveEvent::Ended { reason, .. } => Some(*reason),
        _ => None,
    })
}

fn spec(
    source: FakeSource,
    record: Option<RecordSpec>,
    stop_file: bool,
    until: Option<Duration>,
    poll: Duration,
) -> CaptureSpec {
    CaptureSpec {
        source: once(source),
        cfg: cfg(poll),
        record,
        stop_file,
        until,
        analyst: None,
    }
}

#[test]
fn recording_a_corpus_ends_at_source_end_with_a_clean_session_folder() {
    let root = tempfile::tempdir().unwrap();
    let corpus_root = root.path().join("corpus");
    let fake = fake_from_corpus(corpus("synthetic_basic")).unwrap();
    let frames = fake.steps.len();
    let expected_name = fake.meeting_name.clone();
    let source = fake.source();
    let spec = CaptureSpec {
        source: once(source),
        cfg: LiveConfig {
            poll: Duration::from_millis(1),
            debounce: DebounceClock::Mono,
            ..cfg(Duration::from_millis(1))
        },
        record: Some(record()),
        stop_file: true,
        until: None,
        analyst: None,
    };
    let capture = start(spec, &corpus_root).unwrap();
    let (events, ended_at) =
        run_to_end(&capture, Duration::from_secs(30), Duration::from_millis(20));
    assert!(ended_at.is_some(), "the fake source should end");
    assert_eq!(ended_reason(&events), Some(EndReason::SourceEnded));

    let summary: LiveSummary = capture.finish().expect("capture thread did not panic");
    assert_eq!(summary.reason, EndReason::SourceEnded);
    assert_eq!(summary.frames as usize, frames);
    let dir = summary
        .session_dir
        .clone()
        .expect("a recorded session has a folder");
    assert!(dir.starts_with(&corpus_root));
    assert!(dir.join("captions.jsonl").is_file());
    let manifest = Manifest::load(dir.join("manifest.json")).unwrap();
    assert!(manifest.ended_cleanly());
    assert_eq!(manifest.session_id, summary.session_id.clone().unwrap());
    // The source's meeting name fills in when the user chose none.
    assert_eq!(manifest.meeting_name, expected_name);
}

#[test]
fn record_none_writes_no_folder_at_all() {
    let root = tempfile::tempdir().unwrap();
    let corpus_root = root.path().join("corpus");
    let capture = start(
        spec(
            FakeSource::frames(vec![vec![(Some("Ana"), Some("hola"))]]),
            None,
            true,
            None,
            Duration::from_millis(1),
        ),
        &corpus_root,
    )
    .unwrap();
    assert!(capture.session_dir().is_none());
    let (events, _) = run_to_end(&capture, Duration::from_secs(10), Duration::from_millis(20));
    assert_eq!(ended_reason(&events), Some(EndReason::SourceEnded));
    let summary = capture.finish().unwrap();
    assert!(summary.session_dir.is_none());
    assert!(summary.session_id.is_none());
    assert!(
        !corpus_root.exists(),
        "nothing may be created without a record"
    );
}

#[test]
fn a_stop_file_in_the_session_folder_stops_a_long_capture_quickly() {
    let root = tempfile::tempdir().unwrap();
    let corpus_root = root.path().join("corpus");
    let poll = Duration::from_millis(20);
    let capture = start(
        spec(long_source(2000), Some(record()), true, None, poll),
        &corpus_root,
    )
    .unwrap();
    let dir = session_dir_of(&capture);

    // Let it run for a moment before the user asks it to stop.
    let warm = Instant::now();
    while warm.elapsed() < Duration::from_millis(300) {
        capture.poll(poll);
    }
    std::fs::write(dir.join("STOP"), "").unwrap();
    let asked = Instant::now();

    let (events, ended_at) = run_to_end(&capture, Duration::from_secs(5), poll);
    let ended_at = ended_at.expect("STOP must end the capture");
    assert!(ended_at.duration_since(asked) < Duration::from_secs(3));
    assert_eq!(ended_reason(&events), Some(EndReason::Stop));
    assert!(
        dir.join("STOP").exists(),
        "the file is removed only by finish()"
    );

    let summary = capture.finish().unwrap();
    assert_eq!(summary.reason, EndReason::Stop);
    assert!(summary.frames < 2000, "the source was not exhausted");
    assert!(!dir.join("STOP").exists(), "finish() removes the STOP file");
    let manifest = Manifest::load(dir.join("manifest.json")).unwrap();
    assert!(manifest.ended_cleanly());
}

#[test]
fn without_a_stop_file_option_a_stop_file_is_ignored() {
    let root = tempfile::tempdir().unwrap();
    let corpus_root = root.path().join("corpus");
    let poll = Duration::from_millis(5);
    let capture = start(
        spec(long_source(60), Some(record()), false, None, poll),
        &corpus_root,
    )
    .unwrap();
    let dir = session_dir_of(&capture);
    std::fs::write(dir.join("STOP"), "").unwrap();
    let (events, _) = run_to_end(&capture, Duration::from_secs(10), poll);
    assert_eq!(ended_reason(&events), Some(EndReason::SourceEnded));
    let summary = capture.finish().unwrap();
    assert_eq!(summary.reason, EndReason::SourceEnded);
    assert!(
        dir.join("STOP").exists(),
        "nothing removes a file it did not watch for"
    );
}

#[test]
fn until_deadline_stops_a_long_capture() {
    let poll = Duration::from_millis(20);
    let capture = start(
        spec(
            long_source(2000),
            None,
            false,
            Some(Duration::from_millis(300)),
            poll,
        ),
        Path::new("unused"),
    )
    .unwrap();
    let started = Instant::now();
    let (events, ended_at) = run_to_end(&capture, Duration::from_secs(5), poll);
    assert!(ended_at.is_some(), "the deadline must end the capture");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(ended_reason(&events), Some(EndReason::Stop));
    let summary = capture.finish().unwrap();
    assert_eq!(summary.reason, EndReason::Stop);
    assert!(summary.frames < 2000);
}

#[test]
fn a_slow_consumer_gets_at_most_one_open_per_batch() {
    let poll = Duration::from_millis(2);
    let frames = 300;
    let capture = start(
        spec(long_source(frames), None, false, None, poll),
        Path::new("unused"),
    )
    .unwrap();

    let started = Instant::now();
    let mut batches = 0usize;
    let mut opens = 0usize;
    let mut ended = false;
    while started.elapsed() < Duration::from_secs(30) && !ended {
        // A slow renderer: each batch waits for the next frame to be drawn.
        std::thread::sleep(Duration::from_millis(20));
        let batch = capture.poll(Duration::from_millis(5));
        let batch_opens = batch
            .iter()
            .filter(|e| matches!(e, LiveEvent::Open(_)))
            .count();
        assert!(
            batch_opens <= 1,
            "a batch carried {batch_opens} Open events"
        );
        opens += batch_opens;
        if !batch.is_empty() {
            batches += 1;
        }
        ended = batch.iter().any(|e| matches!(e, LiveEvent::Ended { .. }));
    }
    assert!(ended);
    assert!(batches > 0);
    assert!(
        opens < frames,
        "the consumer saw {opens} Open updates for {frames} changing frames: coalescing did nothing"
    );
    capture.finish().unwrap();
}

// ------------------------------------------------- audit regressions

#[test]
fn dropping_a_capture_stops_it_and_closes_the_session() {
    let root = tempfile::tempdir().unwrap();
    let capture = start(
        CaptureSpec {
            source: once(long_source(5000)),
            cfg: cfg(Duration::from_millis(5)),
            record: Some(record()),
            stop_file: false,
            until: None,
            analyst: None,
        },
        root.path(),
    )
    .unwrap();
    let dir = session_dir_of(&capture);
    let _ = capture.poll(Duration::from_millis(100));
    drop(capture);
    let manifest = Manifest::load(dir.join("manifest.json")).unwrap();
    assert!(manifest.ended_cleanly(), "the dropped capture was closed");
    assert_eq!(manifest.end_reason.as_deref(), Some("stop"));
}

/// A source stuck inside `poll`, like a native call that never returns.
struct Stuck {
    release: std::sync::Arc<std::sync::atomic::AtomicBool>,
    entered: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl mc_captions::CaptureSource for Stuck {
    fn name(&self) -> &str {
        "Stuck"
    }
    fn discover(&mut self) -> Result<mc_captions::Discovery, mc_captions::SourceError> {
        Ok(mc_captions::Discovery::Attached {
            window: "stuck".into(),
        })
    }
    fn poll(&mut self) -> Result<Vec<mc_core::RawObservation>, mc_captions::SourceError> {
        self.entered
            .store(true, std::sync::atomic::Ordering::Relaxed);
        while !self.release.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(Vec::new())
    }
}

#[test]
fn a_stuck_source_is_reported_and_does_not_block_stop() {
    let release = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handle = release.clone();
    let entered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = entered.clone();
    let capture = start(
        CaptureSpec {
            source: once(Stuck {
                release: handle,
                entered,
            }),
            cfg: LiveConfig {
                poll_timeout: Duration::from_millis(200),
                ..cfg(Duration::from_millis(5))
            },
            record: None,
            stop_file: false,
            until: None,
            analyst: None,
        },
        std::env::temp_dir().as_path(),
    )
    .unwrap();
    // Stop only once the source is really inside its blocking call.
    let mut events = Vec::new();
    while !seen.load(std::sync::atomic::Ordering::Relaxed) {
        events.extend(capture.poll(Duration::from_millis(10)));
    }
    let waiting = Instant::now();
    while waiting.elapsed() < Duration::from_secs(5)
        && !events
            .iter()
            .any(|e| matches!(e, LiveEvent::SourceError(m) if m.contains("not responding")))
    {
        events.extend(capture.poll(Duration::from_millis(20)));
    }
    assert!(
        events
            .iter()
            .any(|e| matches!(e, LiveEvent::SourceError(m) if m.contains("not responding"))),
        "the stuck source must be reported"
    );
    capture.request_stop();
    let started = Instant::now();
    let summary = capture
        .finish_timeout(Duration::from_secs(3))
        .expect("stop must not wait for the stuck source");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(summary.reason, EndReason::Stop);
    release.store(true, std::sync::atomic::Ordering::Relaxed); // let the source thread end
}

#[test]
fn searching_forever_leaves_nothing_on_disk() {
    let root = tempfile::tempdir().unwrap();
    let corpus_root = root.path().join("corpus");
    let capture = start(
        spec(
            FakeSource::new(std::iter::repeat_n(
                FakeStep::Discovery(Discovery::Searching),
                1000,
            )),
            Some(record()),
            true,
            None,
            Duration::from_millis(5),
        ),
        &corpus_root,
    )
    .unwrap();
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(300) {
        capture.poll(Duration::from_millis(10));
    }
    assert!(capture.session_dir().is_none());
    capture.request_stop();
    let summary = capture.finish().unwrap();
    assert_eq!(summary.reason, EndReason::Stop);
    assert!(summary.session_dir.is_none());
    assert!(!corpus_root.exists(), "no meeting, no session folder");
}

#[test]
fn the_session_appears_when_the_meeting_is_found() {
    let root = tempfile::tempdir().unwrap();
    let corpus_root = root.path().join("corpus");
    let mut steps: Vec<FakeStep> = (0..3)
        .map(|_| FakeStep::Discovery(Discovery::Searching))
        .collect();
    steps.push(FakeStep::Discovery(Discovery::Attached {
        window: "w".into(),
    }));
    steps.push(FakeStep::Frame {
        nodes: vec![mc_core::RawObservation {
            speaker: Some("Ana".into()),
            text: Some("hola".into()),
            node_id: None,
        }],
        read_ms: None,
    });
    let capture = start(
        spec(
            FakeSource::new(steps),
            Some(record()),
            false,
            None,
            Duration::from_millis(1),
        ),
        &corpus_root,
    )
    .unwrap();
    let (events, _) = run_to_end(&capture, Duration::from_secs(10), Duration::from_millis(10));
    let position = |wanted: fn(&LiveEvent) -> bool| events.iter().position(wanted);
    let searching = position(|e| matches!(e, LiveEvent::Source(Discovery::Searching)));
    let session = position(|e| matches!(e, LiveEvent::Session { .. }));
    let attached = position(|e| matches!(e, LiveEvent::Source(Discovery::Attached { .. })));
    assert!(searching < session, "searching comes first: {events:?}");
    assert!(session < attached, "the session exists before attaching");
    let summary = capture.finish().unwrap();
    let dir = summary.session_dir.expect("the meeting was found");
    assert!(
        Manifest::load(dir.join("manifest.json"))
            .unwrap()
            .ended_cleanly()
    );
}

#[test]
fn a_stop_file_in_the_corpus_folder_stops_a_capture_still_searching() {
    let root = tempfile::tempdir().unwrap();
    let poll = Duration::from_millis(5);
    let searching = FakeSource::new(std::iter::repeat_n(
        FakeStep::Discovery(Discovery::Searching),
        100_000,
    ));
    let capture = start(
        spec(searching, Some(record()), true, None, poll),
        root.path(),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    std::fs::write(root.path().join("STOP"), "").unwrap();
    let (events, ended_at) = run_to_end(&capture, Duration::from_secs(5), poll);
    assert!(
        ended_at.is_some(),
        "STOP must end a capture with no meeting"
    );
    assert_eq!(ended_reason(&events), Some(EndReason::Stop));
    capture.finish().unwrap();
    assert!(!root.path().join("STOP").exists(), "finish() consumes it");
}

#[test]
fn an_unwritable_corpus_folder_fails_at_start() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("not-a-folder");
    std::fs::write(&file, "x").unwrap();
    let result = start(
        spec(
            long_source(2),
            Some(record()),
            false,
            None,
            Duration::from_millis(5),
        ),
        &file.join("corpus"),
    );
    assert!(result.is_err());
}
