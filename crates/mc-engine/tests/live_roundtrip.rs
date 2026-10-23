//! A recorded corpus, replayed through the live driver with a scripted clock,
//! must write the same `captions.jsonl` back.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use mc_core::{ScriptedClock, Stamp};
use mc_engine::corpus::{CorpusSession, SessionSpec};
use mc_engine::live::{EndReason, LiveConfig, LiveEvent, fake_from_corpus, once, spawn};
use mc_engine::replay::verify;
use mc_engine::runs::{RunConfig, build};
use mc_store::jsonl::{CaptionLine, ParsedLine, read_file};
use mc_store::manifest::Manifest;

fn corpora() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/corpus");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.join("manifest.json").is_file())
        .collect();
    dirs.sort();
    dirs
}

fn lines_of(path: &Path) -> Vec<CaptionLine> {
    read_file(path)
        .unwrap()
        .lines
        .into_iter()
        .filter_map(|(_, l)| match l {
            ParsedLine::V2(line) => Some(line),
            ParsedLine::V1(_) => None,
        })
        .collect()
}

#[test]
fn every_corpus_round_trips_through_the_live_driver() {
    let mut checked = 0;
    for dir in corpora() {
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        if name == "synthetic_skew_divergent" {
            continue; // time.time() ran ahead of the stamp: timing differs by design
        }
        let original_manifest = Manifest::load(dir.join("manifest.json")).unwrap();
        let clean = original_manifest.ended_cleanly();
        let mut fake = fake_from_corpus(&dir).unwrap();
        let frames = fake.steps.len();
        if fake.stamps.len() == frames {
            // No recorded flush: the driver still stamps its final flush.
            let last = fake.stamps.last().copied().unwrap_or(Stamp {
                wall_ms: fake.t0_wall_ms,
                mono_ms: 0.0,
            });
            fake.stamps.push(last);
        }
        let clock = Arc::new(ScriptedClock::new(fake.stamps.clone()));
        let out = tempfile::tempdir().unwrap();
        let session = CorpusSession::create(
            out.path(),
            &SessionSpec {
                platform: "teams",
                capture_source: "FakeSource",
                meeting_name: fake.meeting_name.clone(),
                consent_method: "flag",
                t0_wall_ms: fake.t0_wall_ms,
                debounce_clock: fake.debounce.manifest_name(),
            },
        )
        .unwrap();
        let session_dir = session.dir().to_path_buf();
        let source = fake.source();
        let cfg = LiveConfig {
            poll: Duration::from_millis(1),
            debounce: fake.debounce,
            ..LiveConfig::default()
        };
        let (handle, rx) = spawn(
            once(source),
            cfg,
            clock.clone(),
            fake.t0_wall_ms,
            Some(session),
            Arc::new(AtomicBool::new(false)),
        );
        let events: Vec<LiveEvent> = rx.iter().collect();
        let summary = handle.join().expect("capture thread panicked");
        assert_eq!(summary.reason, EndReason::SourceEnded, "{name}");
        assert_eq!(clock.remaining(), 0, "{name}: stamps left over");
        assert!(
            matches!(events.last(), Some(LiveEvent::Ended { .. })),
            "{name}"
        );

        let original = lines_of(&dir.join("captions.jsonl"));
        let produced = lines_of(&session_dir.join("captions.jsonl"));
        if clean {
            assert_eq!(produced, original, "{name}: captions.jsonl differs");
            let expected = std::fs::read_to_string(dir.join("expected_transcript.txt")).unwrap();
            let live: String = events
                .iter()
                .filter_map(|e| match e {
                    LiveEvent::Segment { segment, .. } => Some(segment.line() + "\n"),
                    _ => None,
                })
                .collect();
            assert_eq!(live, expected, "{name}: transcript differs");
            // The id the analyst cites is the idx a deterministic replay gives the segment.
            let live: Vec<(u64, mc_core::Segment)> = events
                .iter()
                .filter_map(|e| match e {
                    LiveEvent::Segment { idx, segment } => Some((*idx, segment.clone())),
                    _ => None,
                })
                .collect();
            let run = build(&session_dir, &RunConfig::default()).unwrap();
            assert_eq!(live.len(), run.segments.len(), "{name}");
            for (position, (idx, segment)) in live.iter().enumerate() {
                assert_eq!(*idx as usize, position, "{name}: idx is the commit order");
                assert_eq!(segment, &run.segments[position], "{name}: segment {idx}");
            }
            let jsonl = run.segments_jsonl.lines().map(|l| {
                serde_json::from_str::<serde_json::Value>(l).unwrap()["idx"]
                    .as_u64()
                    .unwrap()
            });
            assert!(
                jsonl.eq(live.iter().map(|(idx, _)| *idx)),
                "{name}: segments.jsonl idx"
            );
        } else {
            // The recording was cut by a crash; the live run flushes at the end.
            assert_eq!(
                &produced[..original.len()],
                &original[..],
                "{name}: recorded prefix differs"
            );
        }
        let manifest = Manifest::load(session_dir.join("manifest.json")).unwrap();
        assert!(manifest.ended_cleanly(), "{name}");
        assert_eq!(manifest.end_reason.as_deref(), Some("source_ended"));
        if clean {
            let report = verify(&session_dir, true).unwrap();
            assert!(report.passed, "{name}: replay of the live output diverges");
        }
        checked += 1;
    }
    assert_eq!(checked, 16);
}
