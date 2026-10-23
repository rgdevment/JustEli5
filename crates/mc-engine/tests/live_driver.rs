//! The live driver against scripted sources and fake clocks: stopping, pacing,
//! error handling, panics, dropped receivers, dedup and wall-clock jumps.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mc_captions::{CaptureSource, Discovery, FakeSource, FakeStep, SourceError};
use mc_core::{Clock, FakeClock, RawObservation, Stamp};
use mc_engine::corpus::{CorpusSession, SessionSpec};
use mc_engine::live::{DebounceClock, EndReason, LiveConfig, LiveEvent, LiveSummary, once, spawn};
use mc_store::jsonl::{CaptionLine, ParsedLine, read_file};
use mc_store::manifest::Manifest;

const T0: i64 = 1_700_000_000_000;

fn cfg(poll_ms: u64) -> LiveConfig {
    LiveConfig {
        poll: Duration::from_millis(poll_ms),
        rediscover: Duration::from_millis(5),
        ..LiveConfig::default()
    }
}

fn frame(speaker: &str, text: &str) -> FakeStep {
    FakeStep::Frame {
        nodes: vec![RawObservation {
            speaker: Some(speaker.to_string()),
            text: Some(text.to_string()),
            node_id: None,
        }],
        read_ms: None,
    }
}

/// Wraps a source and moves a fake clock forward on every poll, so the
/// debounce sees a steady timeline that the test controls.
struct Ticking<S> {
    inner: S,
    clock: Arc<FakeClock>,
    step_ms: f64,
}

impl<S: CaptureSource> CaptureSource for Ticking<S> {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn discover(&mut self) -> Result<Discovery, SourceError> {
        self.inner.discover()
    }
    fn poll(&mut self) -> Result<Vec<RawObservation>, SourceError> {
        self.clock.advance_ms(self.step_ms);
        self.inner.poll()
    }
    fn meeting_name(&self) -> Option<String> {
        self.inner.meeting_name()
    }
    fn read_ms_override(&self) -> Option<f64> {
        self.inner.read_ms_override()
    }
}

fn ticking(steps: Vec<FakeStep>, clock: &Arc<FakeClock>, step_ms: f64) -> Ticking<FakeSource> {
    Ticking {
        inner: FakeSource::new(steps),
        clock: Arc::clone(clock),
        step_ms,
    }
}

fn session(root: &std::path::Path, platform: &str) -> CorpusSession {
    CorpusSession::create(
        root,
        &SessionSpec {
            platform,
            capture_source: "FakeSource",
            meeting_name: None,
            consent_method: "flag",
            t0_wall_ms: T0,
            debounce_clock: "mono",
        },
    )
    .expect("session")
}

/// Runs the driver to its end and returns every event and the summary.
fn run<S>(
    source: S,
    cfg: LiveConfig,
    clock: Arc<dyn Clock>,
    session: Option<CorpusSession>,
    stop: Arc<AtomicBool>,
) -> (Vec<LiveEvent>, LiveSummary)
where
    S: CaptureSource + Send + 'static,
{
    let factory = once(source);
    let (handle, rx) = spawn(factory, cfg, clock, T0, session, stop);
    let events: Vec<LiveEvent> = rx.iter().collect();
    let summary = handle.join().expect("capture thread must not panic");
    (events, summary)
}

fn ended(events: &[LiveEvent]) -> Option<(EndReason, u64, u64)> {
    events.iter().rev().find_map(|e| match e {
        LiveEvent::Ended {
            reason,
            frames,
            records,
        } => Some((*reason, *frames, *records)),
        _ => None,
    })
}

fn source_events(events: &[LiveEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            LiveEvent::Source(d) => Some(match d {
                Discovery::Searching => "searching".to_string(),
                Discovery::Ended => "ended".to_string(),
                Discovery::Attached { window } => format!("attached:{window}"),
            }),
            _ => None,
        })
        .collect()
}

fn segments(events: &[LiveEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            LiveEvent::Segment { segment, .. } => Some(segment.line()),
            _ => None,
        })
        .collect()
}

fn v2_lines(dir: &std::path::Path) -> Vec<CaptionLine> {
    read_file(dir.join("captions.jsonl"))
        .expect("captions.jsonl")
        .lines
        .into_iter()
        .filter_map(|(_, l)| match l {
            ParsedLine::V2(line) => Some(line),
            ParsedLine::V1(_) => None,
        })
        .collect()
}

fn manifest(dir: &std::path::Path) -> Manifest {
    Manifest::load(dir.join("manifest.json")).expect("manifest")
}

/// Never has text; keeps polling until stopped.
struct Silent;

impl CaptureSource for Silent {
    fn name(&self) -> &str {
        "Silent"
    }
    fn discover(&mut self) -> Result<Discovery, SourceError> {
        Ok(Discovery::Attached {
            window: "silent".to_string(),
        })
    }
    fn poll(&mut self) -> Result<Vec<RawObservation>, SourceError> {
        Ok(Vec::new())
    }
}

#[test]
fn stop_while_running_ends_with_stop_in_time() {
    let root = tempfile::tempdir().unwrap();
    let session = session(root.path(), "teams");
    let dir = session.dir().to_path_buf();
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(T0));
    let stop = Arc::new(AtomicBool::new(false));
    let poll = Duration::from_millis(5);

    let (handle, rx) = spawn(
        once(Silent),
        cfg(5),
        clock,
        T0,
        Some(session),
        Arc::clone(&stop),
    );
    let stopper = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            let at = Instant::now();
            stop.store(true, Ordering::Relaxed);
            at
        })
    };
    let mut ended_at = None;
    let mut events = Vec::new();
    for event in rx {
        if matches!(event, LiveEvent::Ended { .. }) {
            ended_at = Some(Instant::now());
        }
        events.push(event);
    }
    let stopped_at = stopper.join().unwrap();
    let summary = handle.join().unwrap();

    assert_eq!(summary.reason, EndReason::Stop);
    let latency = ended_at
        .expect("Ended delivered")
        .duration_since(stopped_at);
    let budget = poll * 2 + Duration::from_millis(100);
    assert!(
        latency <= budget,
        "stop took {latency:?}, budget {budget:?}"
    );
    assert!(summary.frames > 0, "it was polling");

    let m = manifest(&dir);
    assert!(m.ended_cleanly());
    assert_eq!(m.end_reason.as_deref(), Some("stop"));
    assert_eq!(ended(&events).map(|e| e.0), Some(EndReason::Stop));
}

#[test]
fn a_slow_source_is_not_polled_in_bursts() {
    struct Slow {
        polls: Arc<Mutex<Vec<Instant>>>,
        delay: Duration,
        limit: usize,
        stop: Arc<AtomicBool>,
    }
    impl CaptureSource for Slow {
        fn name(&self) -> &str {
            "Slow"
        }
        fn discover(&mut self) -> Result<Discovery, SourceError> {
            Ok(Discovery::Attached {
                window: "slow".to_string(),
            })
        }
        fn poll(&mut self) -> Result<Vec<RawObservation>, SourceError> {
            let count = {
                let mut polls = self.polls.lock().unwrap();
                polls.push(Instant::now());
                polls.len()
            };
            std::thread::sleep(self.delay);
            if count >= self.limit {
                self.stop.store(true, Ordering::Relaxed);
            }
            Ok(Vec::new())
        }
    }

    let delay = Duration::from_millis(40);
    let limit = 8;
    let polls = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let source = Slow {
        polls: Arc::clone(&polls),
        delay,
        limit,
        stop: Arc::clone(&stop),
    };
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(T0));
    let started = Instant::now();
    let (_, summary) = run(source, cfg(5), clock, None, stop);
    let elapsed = started.elapsed();

    let polls = polls.lock().unwrap();
    assert_eq!(polls.len(), limit);
    assert_eq!(summary.frames, limit as u64);
    for pair in polls.windows(2) {
        let gap = pair[1].duration_since(pair[0]);
        assert!(
            gap + Duration::from_millis(2) >= delay,
            "polls {gap:?} apart: a burst after an overrun"
        );
    }
    // Roughly one frame per delay, never more than the time allows.
    let allowed = (elapsed.as_millis() / delay.as_millis()) as u64 + 1;
    assert!(
        summary.frames <= allowed,
        "{} frames in {elapsed:?}",
        summary.frames
    );
}

#[test]
fn a_transient_error_skips_the_frame_only() {
    let clock = Arc::new(FakeClock::new(T0));
    let source = ticking(
        vec![
            frame("Ana", "uno"),
            FakeStep::Error(SourceError::Transient("blip".into())),
            frame("Ana", "dos"),
        ],
        &clock,
        100.0,
    );
    let (events, summary) = run(
        source,
        cfg(1),
        clock.clone(),
        None,
        Arc::new(AtomicBool::new(false)),
    );

    assert!(
        events
            .iter()
            .any(|e| matches!(e, LiveEvent::SourceError(m) if m == "blip")),
        "{events:?}"
    );
    assert_eq!(summary.frames, 2, "the error is not a frame");
    assert_eq!(summary.reason, EndReason::SourceEnded);
}

#[test]
fn a_lost_window_is_searched_for_and_attached_again() {
    let clock = Arc::new(FakeClock::new(T0));
    let source = ticking(
        vec![
            frame("Ana", "uno"),
            FakeStep::Error(SourceError::Lost("gone".into())),
            FakeStep::Discovery(Discovery::Attached {
                window: "Again".into(),
            }),
            frame("Ana", "dos"),
        ],
        &clock,
        100.0,
    );
    let (events, summary) = run(
        source,
        cfg(1),
        clock.clone(),
        None,
        Arc::new(AtomicBool::new(false)),
    );

    let states = source_events(&events);
    assert_eq!(states, vec!["attached:FakeSource", "attached:Again"]);
    assert_eq!(summary.frames, 2);
    assert_eq!(summary.reason, EndReason::SourceEnded);
}

#[test]
fn a_fatal_error_ends_the_capture_and_closes_the_manifest() {
    let root = tempfile::tempdir().unwrap();
    let session = session(root.path(), "teams");
    let dir = session.dir().to_path_buf();
    let clock = Arc::new(FakeClock::new(T0));
    let source = ticking(
        vec![
            frame("Ana", "uno"),
            FakeStep::Error(SourceError::Fatal("broken".into())),
        ],
        &clock,
        100.0,
    );
    let (events, summary) = run(
        source,
        cfg(1),
        clock.clone(),
        Some(session),
        Arc::new(AtomicBool::new(false)),
    );

    assert_eq!(summary.reason, EndReason::SourceFatal);
    assert_eq!(ended(&events).map(|e| e.0), Some(EndReason::SourceFatal));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, LiveEvent::SourceError(m) if m == "broken"))
    );
    let m = manifest(&dir);
    assert!(m.ended_cleanly(), "the driver closes what it opened");
    assert_eq!(m.end_reason.as_deref(), Some("source_fatal"));
}

#[test]
fn a_panicking_source_is_recorded_as_a_panic_and_the_session_is_closed() {
    struct Panicky {
        polls: u32,
    }
    impl CaptureSource for Panicky {
        fn name(&self) -> &str {
            "Panicky"
        }
        fn discover(&mut self) -> Result<Discovery, SourceError> {
            Ok(Discovery::Attached {
                window: "p".to_string(),
            })
        }
        fn poll(&mut self) -> Result<Vec<RawObservation>, SourceError> {
            self.polls += 1;
            if self.polls > 2 {
                panic!("source exploded on purpose (expected in this test)");
            }
            Ok(vec![RawObservation {
                speaker: Some("Ana".into()),
                text: Some("hola".into()),
                node_id: None,
            }])
        }
    }

    let root = tempfile::tempdir().unwrap();
    let session = session(root.path(), "teams");
    let dir = session.dir().to_path_buf();
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(T0));
    let (events, summary) = run(
        Panicky { polls: 0 },
        cfg(1),
        clock,
        Some(session),
        Arc::new(AtomicBool::new(false)),
    );

    assert_eq!(summary.reason, EndReason::Panic);
    assert_eq!(
        ended(&events).map(|e| e.0),
        Some(EndReason::Panic),
        "the Ended event is still delivered"
    );
    assert!(matches!(events.last(), Some(LiveEvent::Ended { .. })));
    let m = manifest(&dir);
    assert!(m.ended_cleanly());
    assert_eq!(m.end_reason.as_deref(), Some("panic"));
}

#[test]
fn dropping_the_receiver_does_not_stop_the_recording() {
    let root = tempfile::tempdir().unwrap();
    let session = session(root.path(), "teams");
    let dir = session.dir().to_path_buf();
    let clock = Arc::new(FakeClock::new(T0));
    let steps: Vec<FakeStep> = (0..30)
        .map(|i| {
            if i % 2 == 0 {
                frame("Ana", "Hola a todos, vamos a empezar la reunión")
            } else {
                frame("Luis", "Yo tengo un bloqueo con el deploy de staging")
            }
        })
        .collect();
    let source = ticking(steps, &clock, 100.0);
    let (handle, rx) = spawn(
        once(source),
        cfg(1),
        clock.clone(),
        T0,
        Some(session),
        Arc::new(AtomicBool::new(false)),
    );
    drop(rx);
    let summary = handle.join().expect("no panic when nobody listens");

    assert_eq!(summary.reason, EndReason::SourceEnded);
    assert_eq!(summary.frames, 30);
    let lines = v2_lines(&dir);
    assert!(!lines.is_empty(), "captions were still written");
    assert!(lines.iter().any(|l| l.decision == "committed"));
    let m = manifest(&dir);
    assert_eq!(m.end_reason.as_deref(), Some("source_ended"));
}

#[test]
fn the_same_frame_ten_times_is_one_segment() {
    let root = tempfile::tempdir().unwrap();
    let session = session(root.path(), "teams");
    let dir = session.dir().to_path_buf();
    let clock = Arc::new(FakeClock::new(T0));
    let steps: Vec<FakeStep> = (0..10).map(|_| frame("Ana", "Hola a todos")).collect();
    let source = ticking(steps, &clock, 100.0);
    let (events, summary) = run(
        source,
        cfg(1),
        clock.clone(),
        Some(session),
        Arc::new(AtomicBool::new(false)),
    );

    assert_eq!(summary.reason, EndReason::SourceEnded);
    let lines = v2_lines(&dir);
    let decisions: Vec<&str> = lines.iter().map(|l| l.decision.as_str()).collect();
    assert_eq!(
        decisions.iter().filter(|d| **d == "committed").count(),
        1,
        "{decisions:?}"
    );
    assert!(
        decisions
            .iter()
            .filter(|d| **d != "committed")
            .all(|d| *d == "new_open" || *d == "dup_exact"),
        "{decisions:?}"
    );
    assert!(decisions.iter().filter(|d| **d == "dup_exact").count() >= 8);
    assert_eq!(segments(&events), vec!["[Ana]: Hola a todos".to_string()]);
}

/// Clock whose wall time goes back by a minute after a while, while its
/// monotonic time keeps advancing by a fixed step per read.
struct JumpyClock {
    calls: Mutex<u64>,
    step_ms: f64,
    jump_after: Option<u64>,
}

impl Clock for JumpyClock {
    fn now(&self) -> Stamp {
        let n = {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            *calls
        };
        let mono_ms = n as f64 * self.step_ms;
        let mut wall_ms = T0 + mono_ms.round_ties_even() as i64;
        if self.jump_after.is_some_and(|after| n > after) {
            wall_ms -= 60_000;
        }
        Stamp { wall_ms, mono_ms }
    }
}

fn dialogue() -> Vec<FakeStep> {
    let mut steps = Vec::new();
    for _ in 0..4 {
        steps.push(frame("Ana", "Buenos días a todos"));
    }
    for _ in 0..4 {
        steps.push(frame("Luis", "Yo tengo un bloqueo con el deploy"));
    }
    for _ in 0..3 {
        steps.push(frame("Ana", "Perfecto, seguimos"));
    }
    steps
}

fn shape(lines: &[CaptionLine]) -> Vec<(u64, String, String, String, f64)> {
    lines
        .iter()
        .map(|l| {
            (
                l.seq,
                l.speaker.clone(),
                l.text.clone(),
                l.decision.clone(),
                l.mono_ms,
            )
        })
        .collect()
}

#[test]
fn a_wall_clock_jump_backwards_does_not_change_mono_debounced_output() {
    let mut results = Vec::new();
    for jump_after in [None, Some(5)] {
        let root = tempfile::tempdir().unwrap();
        let session = session(root.path(), "teams");
        let dir = session.dir().to_path_buf();
        let clock: Arc<dyn Clock> = Arc::new(JumpyClock {
            calls: Mutex::new(0),
            step_ms: 300.0,
            jump_after,
        });
        let (events, summary) = run(
            FakeSource::new(dialogue()),
            LiveConfig {
                debounce: DebounceClock::Mono,
                ..cfg(1)
            },
            clock,
            Some(session),
            Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(summary.reason, EndReason::SourceEnded);
        let lines = v2_lines(&dir);
        let verdict = mc_engine::replay::verify(&dir, true).unwrap();
        assert!(
            verdict.passed,
            "replay of a mono-debounced live session diverges"
        );
        results.push((shape(&lines), segments(&events), summary.records));
    }
    let (steady, jumped) = (&results[0], &results[1]);
    assert_eq!(steady.1.len(), 3, "three speaker turns: {:?}", steady.1);
    assert_eq!(steady.0, jumped.0, "captions differ after a wall jump");
    assert_eq!(steady.1, jumped.1, "segments differ after a wall jump");
    assert_eq!(steady.2, jumped.2);
}

#[test]
fn without_a_session_segments_are_still_emitted_and_nothing_is_reported_as_written() {
    let clock = Arc::new(FakeClock::new(T0));
    let source = ticking(dialogue(), &clock, 300.0);
    let (events, summary) = run(
        source,
        cfg(1),
        clock.clone(),
        None,
        Arc::new(AtomicBool::new(false)),
    );

    assert_eq!(summary.reason, EndReason::SourceEnded);
    assert!(summary.session_dir.is_none());
    assert!(summary.session_id.is_none());
    assert!(matches!(
        events.first(),
        Some(LiveEvent::Started {
            session_dir: None,
            session_id: None,
            ..
        })
    ));
    assert_eq!(segments(&events).len(), 3);
}

#[test]
fn a_source_factory_error_is_fatal_and_reported() {
    let root = tempfile::tempdir().unwrap();
    let session = session(root.path(), "teams");
    let dir = session.dir().to_path_buf();
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(T0));
    let (handle, rx) = spawn(
        Box::new(|| Err(SourceError::Fatal("no meeting window".into()))),
        cfg(1),
        clock,
        T0,
        Some(session),
        Arc::new(AtomicBool::new(false)),
    );
    let events: Vec<LiveEvent> = rx.iter().collect();
    let summary = handle.join().unwrap();

    assert_eq!(summary.reason, EndReason::SourceFatal);
    assert!(
        events.iter().any(|e| matches!(
            e,
            LiveEvent::SourceError(m) if m.contains("no meeting window")
        )),
        "{events:?}"
    );
    assert_eq!(ended(&events).map(|e| e.0), Some(EndReason::SourceFatal));
    assert_eq!(manifest(&dir).end_reason.as_deref(), Some("source_fatal"));
}

/// Mono stamps with fractional milliseconds are rounded before the debounce
/// sees them, so the recording replays exactly.
#[test]
fn a_mono_debounced_session_with_fractional_stamps_replays_strictly() {
    for step_ms in [123.4567, 99.9996, 300.0004] {
        let root = tempfile::tempdir().unwrap();
        let session = session(root.path(), "teams");
        let dir = session.dir().to_path_buf();
        let clock = Arc::new(FakeClock::new(T0));
        let source = ticking(dialogue(), &clock, step_ms);
        let (_events, summary) = run(
            source,
            LiveConfig {
                debounce: DebounceClock::Mono,
                ..cfg(1)
            },
            clock.clone(),
            Some(session),
            Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(summary.reason, EndReason::SourceEnded);
        let verdict = mc_engine::replay::verify(&dir, true).unwrap();
        assert!(verdict.passed, "step {step_ms}: {:?}", verdict.timing);
        assert_eq!(verdict.timing.len() + verdict.content.len(), 0);
    }
}

fn not_responding(events: &[LiveEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, LiveEvent::SourceError(m) if m.contains("not responding")))
        .count()
}

/// Hands out `sources` in order, one per build.
fn rebuilding(sources: Vec<FakeSource>) -> mc_engine::live::SourceFactory {
    let mut queue = std::collections::VecDeque::from(sources);
    Box::new(move || {
        queue
            .pop_front()
            .map(|s| Box::new(s) as Box<dyn CaptureSource>)
            .ok_or_else(|| SourceError::Fatal("no more sources".to_string()))
    })
}

fn hung_source() -> FakeSource {
    FakeSource::new(vec![FakeStep::Delay(Duration::from_secs(30))])
}

fn fast_restart() -> LiveConfig {
    LiveConfig {
        poll_timeout: Duration::from_millis(100),
        restart_backoff: Duration::from_millis(10),
        ..cfg(1)
    }
}

#[test]
fn a_hung_source_is_abandoned_and_rebuilt_so_captions_resume() {
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(T0));
    let factory = rebuilding(vec![
        hung_source(),
        FakeSource::new(vec![frame("Ana", "hola")]),
    ]);
    let (handle, rx) = spawn(
        factory,
        fast_restart(),
        clock,
        T0,
        None::<CorpusSession>,
        Arc::new(AtomicBool::new(false)),
    );
    let events: Vec<LiveEvent> = rx.iter().collect();
    let summary = handle.join().unwrap();
    assert_eq!(not_responding(&events), 1, "{events:?}");
    assert_eq!(summary.reason, EndReason::SourceEnded);
    assert_eq!(summary.frames, 1);
    assert_eq!(segments(&events).len(), 1);
}

#[test]
fn a_source_that_keeps_hanging_is_given_up_on() {
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(T0));
    let factory = rebuilding((0..10).map(|_| hung_source()).collect());
    let (handle, rx) = spawn(
        factory,
        fast_restart(),
        clock,
        T0,
        None::<CorpusSession>,
        Arc::new(AtomicBool::new(false)),
    );
    let events: Vec<LiveEvent> = rx.iter().collect();
    assert_eq!(handle.join().unwrap().reason, EndReason::SourceFatal);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, LiveEvent::SourceError(m) if m.contains("giving up")))
    );
    assert_eq!(not_responding(&events), 3);
}

#[test]
fn a_panicking_source_is_still_closed() {
    struct Boom(Arc<AtomicBool>);
    impl CaptureSource for Boom {
        fn name(&self) -> &str {
            "Boom"
        }
        fn discover(&mut self) -> Result<Discovery, SourceError> {
            panic!("expected in this test")
        }
        fn poll(&mut self) -> Result<Vec<RawObservation>, SourceError> {
            Ok(Vec::new())
        }
        fn close(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let closed = Arc::new(AtomicBool::new(false));
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(T0));
    let (events, summary) = run(
        Boom(Arc::clone(&closed)),
        cfg(1),
        clock,
        None,
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(summary.reason, EndReason::Panic, "{events:?}");
    assert!(
        closed.load(Ordering::Relaxed),
        "close() must run after a panic"
    );
}

/// Its `discover` answers `Attached` only after a delay.
struct SlowDiscover(Duration);

impl CaptureSource for SlowDiscover {
    fn name(&self) -> &str {
        "SlowDiscover"
    }
    fn discover(&mut self) -> Result<Discovery, SourceError> {
        std::thread::sleep(self.0);
        Ok(Discovery::Attached {
            window: "w".to_string(),
        })
    }
    fn poll(&mut self) -> Result<Vec<RawObservation>, SourceError> {
        Ok(Vec::new())
    }
}

#[test]
fn stop_during_discovery_creates_no_session() {
    use mc_engine::live::SessionSlot;
    let opened = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&opened);
    let slot = SessionSlot::Lazy(Box::new(move |_| {
        flag.store(true, Ordering::Relaxed);
        Err(mc_store::StoreError::io(
            std::path::Path::new("x"),
            std::io::Error::other("must not be called"),
        ))
    }));
    let stop = Arc::new(AtomicBool::new(false));
    let (handle, _rx) = spawn(
        once(SlowDiscover(Duration::from_millis(300))),
        cfg(1),
        Arc::new(FakeClock::new(T0)),
        T0,
        slot,
        Arc::clone(&stop),
    );
    std::thread::sleep(Duration::from_millis(100));
    stop.store(true, Ordering::Relaxed);
    let summary = handle.join().unwrap();
    assert_eq!(summary.reason, EndReason::Stop);
    assert!(!opened.load(Ordering::Relaxed));
}

#[test]
fn waiting_for_the_source_does_not_read_the_clock() {
    struct Counting(FakeClock, std::sync::atomic::AtomicUsize);
    impl Clock for Counting {
        fn now(&self) -> Stamp {
            self.1.fetch_add(1, Ordering::Relaxed);
            self.0.now()
        }
    }
    let clock = Arc::new(Counting(
        FakeClock::new(T0),
        std::sync::atomic::AtomicUsize::new(0),
    ));
    let stop = Arc::new(AtomicBool::new(false));
    let (handle, _rx) = spawn(
        once(SlowDiscover(Duration::from_millis(600))),
        cfg(1),
        clock.clone(),
        T0,
        None::<CorpusSession>,
        Arc::clone(&stop),
    );
    std::thread::sleep(Duration::from_millis(400));
    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
    // Only the final flush reads it; the 50 ms waiting slices must not.
    assert!(clock.1.load(Ordering::Relaxed) <= 1);
}

#[test]
fn stop_wins_over_a_source_blocked_for_a_long_time() {
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(T0));
    let stop = Arc::new(AtomicBool::new(false));
    let source = FakeSource::new(vec![FakeStep::Delay(Duration::from_secs(30))]);
    let config = LiveConfig {
        poll_timeout: Duration::from_millis(100),
        ..cfg(1)
    };
    let factory = once(source);
    let (handle, rx) = spawn(factory, config, clock, T0, None, Arc::clone(&stop));
    let mut events = Vec::new();
    while not_responding(&events) == 0 {
        events.push(rx.recv_timeout(Duration::from_secs(5)).expect("a report"));
    }
    let asked = Instant::now();
    stop.store(true, Ordering::Relaxed);
    let summary = handle.join().expect("no panic");
    assert!(
        asked.elapsed() < Duration::from_secs(3),
        "{:?}",
        asked.elapsed()
    );
    assert_eq!(summary.reason, EndReason::Stop);
}

#[test]
fn no_session_is_created_until_the_meeting_is_found() {
    use mc_engine::live::SessionSlot;
    let root = tempfile::tempdir().unwrap();
    let corpus = root.path().join("corpus");
    let opened = Arc::new(AtomicBool::new(false));
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(T0));
    let mut steps: Vec<FakeStep> = (0..3)
        .map(|_| FakeStep::Discovery(Discovery::Searching))
        .collect();
    steps.push(FakeStep::Discovery(Discovery::Attached {
        window: "w".to_string(),
    }));
    steps.push(frame("Ana", "hola"));
    let source = FakeSource::new(steps);
    let factory = once(source);
    let (flag, dir) = (Arc::clone(&opened), corpus.clone());
    let slot = SessionSlot::Lazy(Box::new(move |_| {
        flag.store(true, Ordering::Relaxed);
        CorpusSession::create(
            &dir,
            &SessionSpec {
                platform: "fake",
                capture_source: "FakeSource",
                meeting_name: None,
                consent_method: "flag",
                t0_wall_ms: T0,
                debounce_clock: "mono",
            },
        )
    }));
    let (handle, rx) = spawn(
        factory,
        cfg(1),
        clock,
        T0,
        slot,
        Arc::new(AtomicBool::new(false)),
    );
    let mut searching = 0;
    for event in &rx {
        match event {
            LiveEvent::Source(Discovery::Searching) => {
                searching += 1;
                assert!(!opened.load(Ordering::Relaxed), "created while searching");
                assert!(!corpus.exists());
            }
            LiveEvent::Session { .. } => break,
            _ => {}
        }
    }
    assert!(searching >= 1);
    assert!(opened.load(Ordering::Relaxed));
    let _ = rx.iter().count();
    let summary = handle.join().unwrap();
    let dir = summary.session_dir.expect("session created");
    assert!(manifest(&dir).ended_cleanly());
    assert!(!v2_lines(&dir).is_empty());
}
