//! The live analyst: a bounded queue the capture never waits on, ledger events, the files it
//! leaves in the session, and the notes, marks, fixes and names the user adds meanwhile.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use mc_analyst::{AnalystConfig, Background, CarriedCommitment, TranscriptSegment};
use mc_engine::controller::{Capture, CaptureSpec, RecordSpec, start};
use mc_engine::live::{LiveConfig, LiveEvent, fake_from_corpus, once};
use mc_engine::live_analyst::{AnalystSpec, LiveAnalyst, folder};
use mc_llm::{FakeLlm, Llm, LlmError, LlmRequest};
use serde_json::Value;

fn corpus(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/corpus")
        .join(name)
}

fn spec(llm: impl Llm + 'static, window_segments: usize) -> AnalystSpec {
    AnalystSpec {
        llm: Box::new(llm),
        cfg: AnalystConfig {
            window_segments,
            ..AnalystConfig::default()
        },
        background: Background::default(),
        carry: None,
        refresh: None,
        provider: "fake".to_string(),
        model: None,
    }
}

fn segment(id: u64, speaker: &str) -> TranscriptSegment {
    TranscriptSegment {
        id,
        speaker: speaker.to_string(),
        text: format!("texto {id}"),
        start_ms: id as i64 * 1000,
        end_ms: id as i64 * 1000 + 500,
    }
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Answers `{}` after a pause: a model slower than the meeting.
struct SlowLlm(Duration);

impl Llm for SlowLlm {
    fn name(&self) -> String {
        "slow".to_string()
    }

    fn complete(&self, _: &LlmRequest) -> Result<String, LlmError> {
        std::thread::sleep(self.0);
        Ok("{}".to_string())
    }
}

/// Says when a call starts, then blocks until a token arrives (or ten seconds pass).
struct GateLlm(
    Mutex<Receiver<()>>,
    Mutex<Option<std::sync::mpsc::Sender<()>>>,
);

impl Llm for GateLlm {
    fn name(&self) -> String {
        "gate".to_string()
    }

    fn complete(&self, _: &LlmRequest) -> Result<String, LlmError> {
        if let Some(entered) = self.1.lock().unwrap().take() {
            let _ = entered.send(());
        }
        let _ = self.0.lock().unwrap().recv_timeout(Duration::from_secs(10));
        Ok("{}".to_string())
    }
}

#[test]
fn a_full_queue_skips_segments_without_ever_blocking_the_sender() {
    let dir = tempfile::tempdir().unwrap();
    let (open, gate) = channel();
    let (entered_tx, entered) = channel();
    let analyst = LiveAnalyst::start_with_queue(
        spec(GateLlm(Mutex::new(gate), Mutex::new(Some(entered_tx))), 1),
        dir.path().to_path_buf(),
        2,
    );
    // The worker must be stuck inside the model before the queue is filled.
    assert!(analyst.push(segment(0, "Ana")));
    entered
        .recv_timeout(Duration::from_secs(5))
        .expect("model called");
    let started = Instant::now();
    let accepted = (1..20)
        .filter(|id| analyst.push(segment(*id, "Ana")))
        .count();
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "pushing waited for the blocked model"
    );
    assert_eq!(
        accepted, 2,
        "only the queue's capacity fits while the model is busy"
    );
    for _ in 0..20 {
        let _ = open.send(());
    }
    let events = analyst.finish(Duration::from_secs(10));
    let behind: Vec<&LiveEvent> = events
        .iter()
        .filter(|e| matches!(e, LiveEvent::AnalystError(m) if m.contains("behind")))
        .collect();
    assert!(!behind.is_empty(), "the skipped segments are reported");
    let meta = read_json(&folder(dir.path()).join("meta.json"));
    assert_eq!(
        meta["segments_skipped"].as_u64().unwrap(),
        19 - accepted as u64
    );
    assert_eq!(meta["complete"], false);
}

#[test]
fn windows_become_ledger_events_and_files_in_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let llm = FakeLlm::new([
        r#"{"add":{"decisions":[{"text":"Usar Rust","evidence":[0,1]}]}}"#,
        r#"{"add":{"commitments":[{"who":"Ana","what":"enviar el informe","due":"el viernes","confidence":0.9,"evidence":[3]}]}}"#,
    ]);
    let analyst = LiveAnalyst::start(spec(llm.clone(), 2), dir.path().to_path_buf());
    for id in 0..4 {
        assert!(analyst.push(segment(id, "Ana")));
    }
    let events = analyst.finish(Duration::from_secs(10));
    let updates: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            LiveEvent::Ledger(update) => Some(update),
            _ => None,
        })
        .collect();
    assert_eq!(updates.len(), 2, "{events:?}");
    assert_eq!(updates[0].changed[0].kind, "decision");
    assert_eq!(updates[1].changed[0].text, "Ana: enviar el informe");
    assert_eq!(updates[1].ledger.item_count(), 2);
    assert_eq!(
        (updates[1].event.first_id, updates[1].event.last_id),
        (2, 3)
    );

    let live = folder(dir.path());
    let ledger = read_json(&live.join("ledger.json"));
    assert_eq!(
        ledger["decisions"][0]["evidence"],
        serde_json::json!([0, 1])
    );
    let log = std::fs::read_to_string(live.join("ledger_events.jsonl")).unwrap();
    assert_eq!(log.lines().count(), 2);
    let meta = read_json(&live.join("meta.json"));
    assert_eq!(meta["source"], "live");
    assert_eq!(meta["finished"], true);
    assert_eq!(meta["complete"], true);
    assert_eq!(meta["segments_analysed"], 4);
    assert_eq!(meta["last_id"], 3);
}

#[test]
fn a_failing_model_is_reported_and_the_analyst_keeps_going() {
    let dir = tempfile::tempdir().unwrap();
    let llm = FakeLlm::scripted([
        Err(LlmError::Timeout),
        Err(LlmError::Timeout),
        Ok(r#"{"add":{"topics":[{"title":"Daily","evidence":[1]}]}}"#.to_string()),
    ]);
    let analyst = LiveAnalyst::start(spec(llm, 1), dir.path().to_path_buf());
    analyst.push(segment(0, "Ana"));
    analyst.push(segment(1, "Ana"));
    let events = analyst.finish(Duration::from_secs(10));
    assert!(events.iter().any(
        |e| matches!(e, LiveEvent::AnalystError(m) if m.contains("#0-#0") && m.contains("in time"))
    ));
    assert!(events.iter().any(|e| matches!(e, LiveEvent::Ledger(_))));
    let meta = read_json(&folder(dir.path()).join("meta.json"));
    assert_eq!(meta["failed_windows"], 1);
    assert_eq!(meta["complete"], false);
}

#[test]
fn a_session_folder_that_cannot_be_written_is_an_event_not_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    // A file where the `analysis` folder should be.
    std::fs::write(dir.path().join("analysis"), "x").unwrap();
    let llm = FakeLlm::new([r#"{}"#]);
    let analyst = LiveAnalyst::start(spec(llm, 1), dir.path().to_path_buf());
    analyst.push(segment(0, "Ana"));
    let events = analyst.finish(Duration::from_secs(10));
    assert!(events.iter().any(
        |e| matches!(e, LiveEvent::AnalystError(m) if m.starts_with("saving the live ledger"))
    ));
}

#[test]
fn accepted_fixes_and_speaker_names_reach_the_model() {
    let dir = tempfile::tempdir().unwrap();
    mc_engine::sidecar::append_term_fix(
        dir.path(),
        mc_core::Stamp {
            wall_ms: 1,
            mono_ms: 1.0,
        },
        &mc_engine::sidecar::TermFix {
            heard: "meets".into(),
            correct: "Meet".into(),
        },
    )
    .unwrap();
    mc_engine::sidecar::set_speaker_name(dir.path(), "Unknown", "Mario").unwrap();
    let llm = FakeLlm::new(["{}"]);
    let analyst = LiveAnalyst::start(spec(llm.clone(), 1), dir.path().to_path_buf());
    analyst.push(segment(0, "Unknown"));
    analyst.finish(Duration::from_secs(10));
    let user = &llm.requests()[0].user;
    assert!(
        user.contains("BEGIN UNTRUSTED FIXES") && user.contains("\"meets\" -> \"Meet\""),
        "{user}"
    );
    assert!(user.contains("[#0] Mario: texto 0"), "{user}");
}

#[test]
fn what_the_user_corrects_during_the_meeting_reaches_the_next_window() {
    let dir = tempfile::tempdir().unwrap();
    let llm = FakeLlm::new(["{}", "{}"]);
    let mut with_refresh = spec(llm.clone(), 1);
    let mut taught = 0;
    with_refresh.refresh = Some(Box::new(move |background: &mut Background| {
        taught += 1;
        background.glossary = vec![format!("Verídex{taught}")];
    }));
    let analyst = LiveAnalyst::start(with_refresh, dir.path().to_path_buf());
    analyst.push(segment(0, "Ana"));
    std::thread::sleep(Duration::from_millis(300));
    analyst.push(segment(1, "Ana"));
    analyst.finish(Duration::from_secs(10));
    let requests = llm.requests();
    assert!(requests.len() >= 2);
    for (n, request) in requests.iter().enumerate() {
        let glossary = format!("Verídex{}", n + 1);
        assert!(request.user.contains(&glossary), "{}", request.user);
    }
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

fn start_capture(analyst: Option<AnalystSpec>, root: &Path) -> Capture {
    let source = fake_from_corpus(corpus("synthetic_basic"))
        .unwrap()
        .source();
    let spec = CaptureSpec {
        source: once(source),
        cfg: LiveConfig {
            poll: Duration::from_millis(1),
            ..LiveConfig::default()
        },
        record: Some(record()),
        stop_file: true,
        until: None,
        analyst,
    };
    start(spec, root).unwrap()
}

/// Polls until `Ended`, with the moment each event came in.
fn poll_to_end(capture: &Capture) -> Vec<(Instant, LiveEvent)> {
    let mut all = Vec::new();
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(30) {
        let events = capture.poll(Duration::from_millis(5));
        let ended = events.iter().any(|e| matches!(e, LiveEvent::Ended { .. }));
        all.extend(events.into_iter().map(|e| (Instant::now(), e)));
        if ended {
            return all;
        }
    }
    panic!("the capture never ended");
}

#[test]
fn a_slow_model_never_delays_the_capture() {
    let root = tempfile::tempdir().unwrap();
    let pause = Duration::from_millis(600);
    let capture = start_capture(Some(spec(SlowLlm(pause), 1)), root.path());
    let started = Instant::now();
    let events = poll_to_end(&capture);
    // Four segments need four windows, 2.4 s of model time; the capture ends long before.
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "the capture waited for the model: {:?}",
        started.elapsed()
    );
    let segments = events
        .iter()
        .filter(|(_, e)| matches!(e, LiveEvent::Segment { .. }))
        .count();
    let updates_before_end = events
        .iter()
        .filter(|(_, e)| matches!(e, LiveEvent::Ledger(_)))
        .count();
    assert_eq!(segments, 4);
    assert!(updates_before_end < 4);
    let rest = capture.close_analyst(Duration::from_secs(20));
    let late = rest
        .iter()
        .filter(|e| matches!(e, LiveEvent::Ledger(_)))
        .count();
    assert_eq!(updates_before_end + late, 4, "every window is still done");
    capture.finish().unwrap();
}

#[test]
fn the_analyst_cites_the_idx_of_the_live_segments_and_leaves_its_files() {
    let root = tempfile::tempdir().unwrap();
    let llm = FakeLlm::new([
        r#"{"add":{"topics":[{"title":"Daily","evidence":[0]}],"decisions":[{"text":"Seguir","evidence":[1,3]}]}}"#,
    ]);
    // One window of all four segments: the ids it may cite are the segment events' idx.
    let capture = start_capture(Some(spec(llm.clone(), 4)), root.path());
    let events = poll_to_end(&capture);
    let idx: Vec<u64> = events
        .iter()
        .filter_map(|(_, e)| match e {
            LiveEvent::Segment { idx, .. } => Some(*idx),
            _ => None,
        })
        .collect();
    assert_eq!(idx, [0, 1, 2, 3]);
    let dir = capture.session_dir().unwrap().to_path_buf();
    let rest = capture.close_analyst(Duration::from_secs(20));
    let seen = events.iter().map(|(_, e)| e).chain(&rest);
    assert!(seen.into_iter().any(|e| matches!(e, LiveEvent::Ledger(_))));
    let user = &llm.requests()[0].user;
    assert!(user.contains("[#0] Ana Pérez: Buenos días"), "{user}");
    assert!(user.contains("[#3] Ana Pérez: Perfecto"), "{user}");
    let ledger = read_json(&folder(&dir).join("ledger.json"));
    assert_eq!(
        ledger["decisions"][0]["evidence"],
        serde_json::json!([1, 3])
    );
    // The corpus is untouched by the analysis.
    assert!(dir.join("captions.jsonl").is_file());
    capture.finish().unwrap();
}

#[test]
fn notes_marks_fixes_and_names_are_written_next_to_the_captions() {
    let root = tempfile::tempdir().unwrap();
    let capture = start_capture(None, root.path());
    assert!(
        capture.add_note("antes de la reunión").is_err(),
        "no session folder yet"
    );
    poll_to_end(&capture);
    let dir = capture.session_dir().unwrap().to_path_buf();

    let LiveEvent::Note(note) = capture.add_note("  revisar el PR ").unwrap() else {
        panic!("a note event");
    };
    assert_eq!(note.text, "revisar el PR");
    let LiveEvent::Mark { wall_ms, .. } = capture.mark().unwrap() else {
        panic!("a mark event");
    };
    capture.fix_term("meets", "Meet").unwrap();
    let names = capture.rename_speaker("Luis", "Luis G.").unwrap();
    assert_eq!(names["Luis"], "Luis G.");
    assert!(capture.add_note("   ").is_err());
    assert!(capture.fix_term("solo", " ").is_err());

    let notes = read_json_lines(&dir.join("notes.jsonl"));
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["text"], "revisar el PR");
    assert!(notes[0]["mono_ms"].is_f64() && notes[0]["wall_ms"].is_i64());
    let marks = read_json_lines(&dir.join("marks.jsonl"));
    assert_eq!(marks[0]["wall_ms"].as_i64().unwrap(), wall_ms);
    let fixes = read_json_lines(&dir.join("term_fixes.jsonl"));
    assert_eq!(
        (fixes[0]["heard"].as_str(), fixes[0]["correct"].as_str()),
        (Some("meets"), Some("Meet"))
    );
    assert_eq!(
        read_json(&dir.join("speaker_names.json"))["Luis"],
        "Luis G."
    );
    capture.finish().unwrap();
}

fn read_json_lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn stopping_the_analyst_lets_it_finish_without_blocking_and_leaves_a_complete_ledger() {
    let root = tempfile::tempdir().unwrap();
    let llm = FakeLlm::new([r#"{"add":{"topics":[{"title":"Daily","evidence":[0]}]}}"#]);
    let capture = start_capture(Some(spec(llm, 20)), root.path());
    let events = poll_to_end(&capture);
    assert!(capture.analyst_pending(), "it waits for more segments");
    capture.stop_analyst();
    let started = Instant::now();
    let mut late = Vec::new();
    while capture.analyst_pending() && started.elapsed() < Duration::from_secs(10) {
        late.extend(capture.poll(Duration::from_millis(10)));
    }
    assert!(!capture.analyst_pending());
    late.extend(capture.poll(Duration::from_millis(10)));
    let seen = events.iter().map(|(_, e)| e).chain(&late);
    assert!(seen.into_iter().any(|e| matches!(e, LiveEvent::Ledger(_))));
    let meta = read_json(&folder(capture.session_dir().unwrap()).join("meta.json"));
    assert_eq!(meta["finished"], true);
    assert_eq!(meta["complete"], true);
    capture.finish().unwrap();
}

#[test]
fn commitments_carried_from_the_previous_meeting_are_loaded_in_the_session_folder_and_resolved() {
    let dir = tempfile::tempdir().unwrap();
    let llm = FakeLlm::new([
        r#"{"carried":[{"source":"prev#c1","status":"done","evidence":[0]},{"source":"prev#c1","status":"done","evidence":[7]}]}"#,
    ]);
    let mut live = spec(llm.clone(), 2);
    let expected = dir.path().to_path_buf();
    live.carry = Some(Box::new(move |session: &Path| {
        // The loader sees the session folder of the capture.
        assert_eq!(session, expected);
        vec![CarriedCommitment {
            source: "prev#c1".into(),
            who: "Ana".into(),
            what: "cerrar el bug".into(),
            due: None,
        }]
    }));
    let analyst = LiveAnalyst::start(live, dir.path().to_path_buf());
    for id in 0..2 {
        assert!(analyst.push(segment(id, "Ana")));
    }
    analyst.finish(Duration::from_secs(10));
    let request = &llm.requests()[0];
    assert!(request.user.contains("BEGIN UNTRUSTED CARRIED"));
    assert!(request.user.contains("prev#c1 | @Ana: cerrar el bug"));
    let ledger = read_json(&folder(dir.path()).join("ledger.json"));
    // The second answer cited a segment the model never saw: only the first stands.
    assert_eq!(ledger["carried"][0]["status"], "done");
    assert_eq!(ledger["carried"][0]["evidence"], serde_json::json!([0]));
}
