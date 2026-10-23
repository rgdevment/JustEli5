//! Replays every recorded corpus under `fixtures/corpus` and checks the
//! outcome against what the recorder wrote (see `replay::verify`).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use mc_core::Stamp;
use mc_engine::replay::{
    ReplayError, ReplayOp, ReplayRun, compare, reconstruct, run, run_with_now, verify,
};
use mc_store::jsonl::{CaptionLine, ParsedLine, ReadReport, read_file};
use mc_store::manifest::Manifest;
use serde_json::{Value, json};

const CORPORA: usize = 17;

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/corpus")
}

fn corpora() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(corpus_root())
        .expect("fixtures/corpus exists")
        .map(|entry| entry.expect("readable entry").path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

fn name_of(dir: &Path) -> String {
    dir.file_name().unwrap().to_string_lossy().into_owned()
}

/// A corpus read the way `verify` reads it, kept whole so tests can replay
/// it with different clocks and compare it with the file.
struct Corpus {
    manifest: Manifest,
    report: ReadReport,
    ops: Vec<ReplayOp>,
    flush_at: Option<Stamp>,
    file: Vec<CaptionLine>,
    transcript: String,
}

fn flush_at_of(manifest: &Manifest) -> Option<Stamp> {
    manifest.ended_wall_ms.map(|wall_ms| Stamp {
        wall_ms,
        mono_ms: manifest.ended_mono_ms.unwrap_or(0.0),
    })
}

fn load(dir: &Path) -> Corpus {
    let manifest = Manifest::load(dir.join("manifest.json")).unwrap();
    let report = read_file(dir.join(&manifest.captions.file)).unwrap();
    let ops = reconstruct(&report).unwrap();
    let file = report
        .lines
        .iter()
        .filter_map(|(_, line)| match line {
            ParsedLine::V2(line) => Some(line.clone()),
            ParsedLine::V1(_) => None,
        })
        .collect();
    let transcript = fs::read_to_string(dir.join("expected_transcript.txt")).unwrap();
    Corpus {
        flush_at: flush_at_of(&manifest),
        manifest,
        report,
        ops,
        file,
        transcript,
    }
}

/// Replays with the manager's `now` taken from `now_of(frame, stamp)`.
fn replay_with(c: &Corpus, now_of: &dyn Fn(u64, Stamp) -> f64) -> ReplayRun {
    run_with_now(&c.ops, &c.manifest, c.flush_at, now_of).unwrap()
}

/// The wall clock as the recorder read it.
fn wall_now(_frame: u64, stamp: Stamp) -> f64 {
    stamp.wall_ms as f64 / 1000.0
}

/// A `ReplayRun` that replays the file as-is (for mutation tests).
fn as_replay(file: &[CaptionLine], transcript: &str) -> ReplayRun {
    ReplayRun {
        lines: file.to_vec(),
        segments: Vec::new(),
        transcript: transcript.to_string(),
        open_lines: Vec::new(),
    }
}

/// A corpus directory copied into a temp dir that is removed on drop.
struct TempCorpus(PathBuf);

impl TempCorpus {
    /// Copies `files` from `src` and writes `manifest` (if given) in place of
    /// the source manifest.
    fn new(tag: &str, src: &Path, files: &[&str], manifest: Option<Value>) -> Self {
        let dir = std::env::temp_dir().join(format!("mc-engine-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for file in files {
            fs::copy(src.join(file), dir.join(file)).unwrap();
        }
        if let Some(manifest) = manifest {
            let text = serde_json::to_string_pretty(&manifest).unwrap();
            fs::write(dir.join("manifest.json"), text).unwrap();
        }
        Self(dir)
    }
}

impl Drop for TempCorpus {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn manifest_json(dir: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap()
}

fn with_debounce_clock(mut manifest: Value, clock: &str) -> Value {
    manifest["captions"]["debounce_clock"] = json!(clock);
    manifest
}

#[test]
fn every_corpus_replays_as_recorded() {
    let dirs = corpora();
    assert_eq!(dirs.len(), CORPORA, "corpus count changed");

    for dir in &dirs {
        let name = name_of(dir);
        let expected_path = dir.join("expected_replay.json");

        if let Ok(text) = fs::read_to_string(&expected_path) {
            // A corpus that documents a known divergence: the strict check
            // fails on it, the content check passes.
            let spec: serde_json::Value = serde_json::from_str(&text).expect("valid json");
            assert_eq!(spec["strict"], "fail", "{name}: unexpected expectation");
            let strict = verify(dir, true).unwrap();
            assert!(!strict.passed, "{name}: strict should fail");
            assert!(strict.content.is_empty(), "{name}: content diverged");
            let lenient = verify(dir, false).unwrap();
            assert!(lenient.passed, "{name}: non-strict should pass");
            assert!(lenient.content.is_empty(), "{name}: content diverged");
            continue;
        }

        let report = verify(dir, true).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(report.frames > 0, "{name}: no frames replayed");
        assert!(report.transcript_ok, "{name}: transcript differs");
        assert!(
            report.timing.is_empty(),
            "{name}: timing {:?}",
            report.timing
        );
        assert!(
            report.content.is_empty(),
            "{name}: content {:?}",
            report.content
        );
        assert!(report.passed, "{name}: strict check failed");
    }
}

/// Timing divergences `synthetic_skew_divergent` produces when replayed.
const SKEW_TIMING: usize = 10;

/// The skew corpus differs from its recording only in timing: the commit
/// moves one frame, so non-strict passes and strict fails.
#[test]
fn skew_divergent_differs_only_in_timing() {
    let dir = corpus_root().join("synthetic_skew_divergent");

    let strict = verify(&dir, true).unwrap();
    assert!(!strict.passed);
    assert_eq!(
        strict.timing.len(),
        SKEW_TIMING,
        "pinned timing divergence count"
    );
    assert!(strict.transcript_ok);
    assert!(strict.content.is_empty(), "content {:?}", strict.content);

    let lenient = verify(&dir, false).unwrap();
    assert!(lenient.passed);
}

#[test]
fn no_captions_corpus_counts() {
    let report = verify(corpus_root().join("synthetic_no_captions"), true).unwrap();
    assert_eq!(report.frames, 5);
    assert_eq!(report.records, 5);
    assert!(report.passed);
}

#[test]
fn unclean_end_reports_truncated_tail() {
    let report = verify(corpus_root().join("synthetic_unclean_end"), true).unwrap();
    assert!(report.truncated_tail);
    assert!(report.passed);
    assert_eq!(
        report.open_lines,
        vec!["[Ana]: tercera frase todavia abierta".to_string()]
    );
}

#[test]
fn missing_captions_file_is_an_error() {
    let dir =
        std::env::temp_dir().join(format!("mc-engine-missing-captions-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::copy(
        corpus_root().join("synthetic_basic/manifest.json"),
        dir.join("manifest.json"),
    )
    .unwrap();

    let result = verify(&dir, true);
    fs::remove_dir_all(&dir).unwrap();

    assert!(
        matches!(result, Err(ReplayError::Store(_))),
        "expected a store error, got {result:?}"
    );
}

#[test]
fn missing_transcript_is_compared_with_committed_records() {
    let src = corpus_root().join("synthetic_basic");
    let tmp = TempCorpus::new(
        "no-transcript",
        &src,
        &["manifest.json", "captions.jsonl"],
        None,
    );

    let report = verify(&tmp.0, true).unwrap();
    assert!(report.transcript_from_records);
    assert!(report.transcript_ok);
    assert!(report.passed);
}

/// The boundary at `LINE_DEBOUNCE_SEC` (1.2 s): the replay keeps every
/// recorded stamp, and only the manager's `now` changes.
#[test]
fn debounce_boundary_is_sensitive_to_the_manager_clock() {
    let c = load(&corpus_root().join("synthetic_debounce_boundary"));
    let silent: HashSet<u64> = c
        .ops
        .iter()
        .filter_map(|op| match op {
            ReplayOp::Frame { frame, nodes, .. } if nodes.is_empty() => Some(*frame),
            _ => None,
        })
        .collect();
    assert!(!silent.is_empty(), "corpus has no frames without nodes");

    // Positive control: the recorder's own clock reproduces the file exactly.
    let baseline = replay_with(&c, &wall_now);
    let positive = compare(
        &c.file,
        &baseline,
        c.report.truncated_tail,
        &c.transcript,
        true,
    );
    assert!(positive.passed, "positive control failed: {positive:?}");
    assert!(positive.timing.is_empty() && positive.content.is_empty());

    // Lowered by 1 ms on the frames without nodes: the debounce no longer
    // fires at the boundary, so the commit moves and strict must notice.
    let lowered = |frame: u64, stamp: Stamp| {
        let now = wall_now(frame, stamp);
        if silent.contains(&frame) {
            now - 0.001
        } else {
            now
        }
    };
    let early = compare(
        &c.file,
        &replay_with(&c, &lowered),
        c.report.truncated_tail,
        &c.transcript,
        true,
    );
    assert!(!early.passed, "lowered now passed strict");
    assert!(
        !early.timing.is_empty() || !early.content.is_empty(),
        "lowered now produced no divergence"
    );

    // Raised by 1 ms on the same frames: the commit happens no later than
    // recorded, so strict still passes.
    let raised = |frame: u64, stamp: Stamp| {
        let now = wall_now(frame, stamp);
        if silent.contains(&frame) {
            now + 0.001
        } else {
            now
        }
    };
    let late = compare(
        &c.file,
        &replay_with(&c, &raised),
        c.report.truncated_tail,
        &c.transcript,
        true,
    );
    assert!(late.passed, "raised now failed strict: {late:?}");
}

#[test]
fn mutated_observation_decision_is_content() {
    let c = load(&corpus_root().join("synthetic_basic"));
    let baseline = as_replay(&c.file, &c.transcript);
    let ok = compare(&c.file, &baseline, false, &c.transcript, true);
    assert!(ok.passed, "identity replay must pass: {ok:?}");

    let mut replay = as_replay(&c.file, &c.transcript);
    let index = replay
        .lines
        .iter()
        .position(|l| l.decision == "grow")
        .expect("corpus has a grow record");
    replay.lines[index].decision = "dup_exact".into();

    let report = compare(&c.file, &replay, false, &c.transcript, false);
    assert!(!report.passed);
    assert!(
        report.content.iter().any(|d| d.field == "decision"),
        "content {:?}",
        report.content
    );
}

#[test]
fn dropped_observation_record_is_a_content_count_mismatch() {
    let c = load(&corpus_root().join("synthetic_basic"));
    let mut replay = as_replay(&c.file, &c.transcript);
    let index = replay
        .lines
        .iter()
        .position(|l| l.decision != "committed" && l.decision != "dup_committed_skip")
        .expect("corpus has an observation record");
    replay.lines.remove(index);

    let report = compare(&c.file, &replay, false, &c.transcript, false);
    assert!(!report.passed);
    assert!(
        report
            .content
            .iter()
            .any(|d| d.field == "observation_count"),
        "content {:?}",
        report.content
    );
}

#[test]
fn truncated_tail_tolerates_an_extra_record_of_the_last_frame_only() {
    let c = load(&corpus_root().join("synthetic_unclean_end"));
    assert!(c.report.truncated_tail);
    let last = c.file.last().expect("corpus has records").clone();
    let last_frame = last.frame.expect("last record is in a frame");

    let mut same_frame = as_replay(&c.file, &c.transcript);
    same_frame.lines.push(CaptionLine {
        seq: last.seq + 1,
        ..last.clone()
    });
    let tolerated = compare(&c.file, &same_frame, true, &c.transcript, true);
    assert!(
        tolerated.passed,
        "same-frame extra must pass: {tolerated:?}"
    );

    let mut other_frame = as_replay(&c.file, &c.transcript);
    other_frame.lines.push(CaptionLine {
        seq: last.seq + 1,
        frame: Some(last_frame + 1),
        ..last
    });
    let rejected = compare(&c.file, &other_frame, true, &c.transcript, false);
    assert!(!rejected.passed, "other-frame extra must fail");
    assert!(
        !rejected.content.is_empty(),
        "other-frame extra must be content"
    );
}

#[test]
fn wall_jump_passes_with_explicit_wall_clock() {
    let src = corpus_root().join("synthetic_wall_jump");
    let manifest = with_debounce_clock(manifest_json(&src), "wall");
    let tmp = TempCorpus::new(
        "wall-explicit",
        &src,
        &["captions.jsonl", "expected_transcript.txt"],
        Some(manifest),
    );
    let report = verify(&tmp.0, true).unwrap();
    assert!(report.passed, "{report:?}");
}

/// Documents the actual outcome: on `synthetic_wall_jump` the wall clock steps
/// back at frame 8, but the mono clock replays to the same records and the
/// same (passing) verdict, so the jump never changes a commit.
#[test]
fn wall_jump_under_mono_clock_matches_wall() {
    let src = corpus_root().join("synthetic_wall_jump");
    let manifest = with_debounce_clock(manifest_json(&src), "mono");
    let tmp = TempCorpus::new(
        "wall-jump-mono",
        &src,
        &["captions.jsonl", "expected_transcript.txt"],
        Some(manifest.clone()),
    );
    let mono_report = verify(&tmp.0, true).unwrap();
    let wall_report = verify(&src, true).unwrap();
    assert!(mono_report.passed, "{mono_report:?}");
    assert!(wall_report.passed, "{wall_report:?}");
    assert_eq!(mono_report.timing.len(), wall_report.timing.len());
    assert_eq!(mono_report.content.len(), wall_report.content.len());

    let c = load(&src);
    let mono_manifest: Manifest = serde_json::from_value(manifest).unwrap();
    let mono_run = run(&c.ops, &mono_manifest, c.flush_at).unwrap();
    let wall_run = replay_with(&c, &wall_now);
    assert_eq!(
        mono_run.lines, wall_run.lines,
        "mono and wall replays differ"
    );
}

#[test]
fn debounce_clock_must_be_known() {
    let src = corpus_root().join("synthetic_wall_jump");
    let manifest = with_debounce_clock(manifest_json(&src), "bogus");
    let tmp = TempCorpus::new(
        "bogus-clock",
        &src,
        &["captions.jsonl", "expected_transcript.txt"],
        Some(manifest),
    );
    let result = verify(&tmp.0, true);
    assert!(
        matches!(result, Err(ReplayError::Invalid(_))),
        "expected Invalid, got {result:?}"
    );
}
