//! `Policy::V2` through `mc_engine::runs`: every corpus builds under both
//! policies, V2 runs get their own id and are reported as not comparable with
//! no divergences, they are deterministic, and the transcripts that differ from
//! V1 are pinned with the defect behind each difference. Corpora are copied to a
//! temporary directory; nothing under `fixtures/` is written.

use std::fs;
use std::path::{Path, PathBuf};

use mc_engine::runs::{RunAction, RunArtifacts, RunConfig, build, execute};
use mc_reconcile::{Policy, ReconcilerConfig};
use mc_store::Store;
use serde_json::Value;
use tempfile::TempDir;

const CORPORA: usize = 17;

/// Corpora whose V2 transcript equals the V1 transcript.
const UNCHANGED: [&str; 13] = [
    "synthetic_basic",
    "synthetic_debounce_boundary",
    "synthetic_guests",
    "synthetic_history",
    "synthetic_long_silence",
    "synthetic_mixed_alias_frame",
    "synthetic_monologue",
    "synthetic_no_captions",
    "synthetic_same_speaker_twice",
    "synthetic_single_node",
    "synthetic_skew_divergent",
    "synthetic_unclean_end",
    "synthetic_wall_jump",
];

/// Corpora whose transcript changes, with the segment counts (V1, V2).
///
/// * `clock_jitter` 25 -> 19: V1's extra segments are fragments and garbled
///   re-emissions of the same speech (for example "carrya vamos ebuipo b 100%
///   revisar" beside "carry vamos equipo b 100% revisar"), which V2 does not
///   emit. Under V2 a shorter re-emission of a committed line is a duplicate
///   and no longer replaces it (see
///   `a_truncated_reemission_does_not_shrink_a_committed_line`). The per-line
///   mechanism was not traced further.
/// * `reappear` 3 -> 2: D2, the line that grows after its commit (inside the
///   amend window) is amended into its first segment instead of a new one.
/// * `tie_ts` 6 -> 6: D1 (one-to-one matching, no newline). Same segments, but
///   V2 orders lines that share a timestamp by creation; V1 did not.
/// * `unicode` 6 -> 7: D6, the CJK-only caption is content and is kept.
const CHANGED: [(&str, usize, usize); 4] = [
    ("synthetic_clock_jitter", 25, 19),
    ("synthetic_reappear", 3, 2),
    ("synthetic_tie_ts", 6, 6),
    ("synthetic_unicode", 6, 7),
];

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/corpus")
}

fn corpus_names() -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(corpus_root())
        .expect("fixtures/corpus exists")
        .map(|entry| entry.expect("readable entry").path())
        .filter(|path| path.is_dir())
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Copies every corpus into `root/<name>`.
fn copy_all(root: &Path) -> Vec<(String, PathBuf)> {
    corpus_names()
        .into_iter()
        .map(|name| {
            let src = corpus_root().join(&name);
            let dst = root.join(&name);
            fs::create_dir_all(&dst).unwrap();
            for entry in fs::read_dir(&src).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_file() {
                    fs::copy(entry.path(), dst.join(entry.file_name())).unwrap();
                }
            }
            (name, dst)
        })
        .collect()
}

fn v1() -> RunConfig {
    RunConfig::default()
}

fn v2() -> RunConfig {
    RunConfig {
        reconciler: ReconcilerConfig {
            policy: Policy::V2,
            ..ReconcilerConfig::default()
        },
        ..RunConfig::default()
    }
}

fn texts(artifacts: &RunArtifacts) -> Vec<String> {
    artifacts.segments.iter().map(|s| s.text.clone()).collect()
}

#[test]
fn the_corpus_set_is_the_expected_seventeen() {
    let names = corpus_names();
    assert_eq!(names.len(), CORPORA, "corpus count changed");
    let mut expected: Vec<&str> = UNCHANGED
        .iter()
        .copied()
        .chain(CHANGED.iter().map(|(name, _, _)| *name))
        .collect();
    expected.sort_unstable();
    let actual: Vec<&str> = names.iter().map(String::as_str).collect();
    assert_eq!(actual, expected);
}

#[test]
fn every_corpus_builds_under_v1_and_v2_with_distinct_run_ids() {
    let tmp = TempDir::new().unwrap();
    for (name, dir) in copy_all(tmp.path()) {
        let a = build(&dir, &v1()).unwrap_or_else(|e| panic!("{name} v1: {e}"));
        let b = build(&dir, &v2()).unwrap_or_else(|e| panic!("{name} v2: {e}"));

        assert_ne!(a.run_id, b.run_id, "{name}: V2 must not share the V1 id");
        assert_eq!(a.report["config"]["policy"], "v1_legacy", "{name}");
        assert_eq!(b.report["config"]["policy"], "v2", "{name}");
        assert_eq!(a.report["comparable"], true, "{name}");
        assert_eq!(b.report["comparable"], false, "{name}");
        assert_eq!(b.report["divergences"]["content"], 0, "{name}");
        assert_eq!(b.report["divergences"]["timing"], 0, "{name}");
    }
}

#[test]
fn v2_runs_are_deterministic() {
    let tmp = TempDir::new().unwrap();
    for (name, dir) in copy_all(tmp.path()) {
        let first = build(&dir, &v2()).unwrap();
        let second = build(&dir, &v2()).unwrap();
        assert_eq!(first.run_id, second.run_id, "{name}: run id");
        assert_eq!(first.txt, second.txt, "{name}: txt");
        assert_eq!(first.md, second.md, "{name}: md");
        assert_eq!(
            first.segments_jsonl, second.segments_jsonl,
            "{name}: segments"
        );
        assert_eq!(first.events_jsonl, second.events_jsonl, "{name}: events");
        assert_eq!(first.report_json, second.report_json, "{name}: report");
    }
}

#[test]
fn v1_run_ids_are_stable_and_do_not_depend_on_the_v2_revision() {
    // V2_REV is only folded into V2 ids. It cannot be changed from a test, so
    // this checks what can be: V1 ids are reproducible and differ per corpus.
    let tmp = TempDir::new().unwrap();
    let mut ids = Vec::new();
    for (name, dir) in copy_all(tmp.path()) {
        let first = build(&dir, &v1()).unwrap().run_id;
        let second = build(&dir, &v1()).unwrap().run_id;
        assert_eq!(first, second, "{name}");
        ids.push(first);
    }
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), CORPORA, "run ids collide across corpora");
}

#[test]
fn unchanged_corpora_have_the_same_transcript_under_v2() {
    let tmp = TempDir::new().unwrap();
    let dirs = copy_all(tmp.path());
    for name in UNCHANGED {
        let dir = &dirs.iter().find(|(n, _)| n == name).unwrap().1;
        let a = build(dir, &v1()).unwrap();
        let b = build(dir, &v2()).unwrap();
        assert_eq!(a.txt, b.txt, "{name}: V2 transcript must equal V1");
        assert_eq!(a.segments.len(), b.segments.len(), "{name}");
    }
}

#[test]
fn changed_corpora_have_the_pinned_segment_counts() {
    let tmp = TempDir::new().unwrap();
    let dirs = copy_all(tmp.path());
    for (name, v1_count, v2_count) in CHANGED {
        let dir = &dirs.iter().find(|(n, _)| n == name).unwrap().1;
        let a = build(dir, &v1()).unwrap();
        let b = build(dir, &v2()).unwrap();
        assert_eq!(a.segments.len(), v1_count, "{name}: V1 segments");
        assert_eq!(b.segments.len(), v2_count, "{name}: V2 segments");
        assert_ne!(a.txt, b.txt, "{name}: transcripts must differ");
    }
}

#[test]
fn tie_ts_differs_only_in_the_order_of_its_segments() {
    let tmp = TempDir::new().unwrap();
    let dirs = copy_all(tmp.path());
    let dir = &dirs
        .iter()
        .find(|(n, _)| n == "synthetic_tie_ts")
        .unwrap()
        .1;
    let a = texts(&build(dir, &v1()).unwrap());
    let b = texts(&build(dir, &v2()).unwrap());
    let (mut sa, mut sb) = (a.clone(), b.clone());
    sa.sort();
    sb.sort();
    assert_eq!(sa, sb, "the same lines, only ordered differently");
    assert_ne!(a, b, "V2 orders the tied lines by creation");
    assert_eq!(b[0], "uno dos tres");
    assert_eq!(b[1], "cuatro cinco seis");
}

#[test]
fn unicode_v2_adds_only_the_cjk_line() {
    let tmp = TempDir::new().unwrap();
    let dirs = copy_all(tmp.path());
    let dir = &dirs
        .iter()
        .find(|(n, _)| n == "synthetic_unicode")
        .unwrap()
        .1;
    let a = texts(&build(dir, &v1()).unwrap());
    let b = texts(&build(dir, &v2()).unwrap());
    assert!(!a.iter().any(|t| t.contains("日本語")));
    let extra: Vec<&String> = b.iter().filter(|t| !a.contains(t)).collect();
    assert_eq!(extra, vec!["日本語のテキストだけ"]);
}

/// The long line "lolo sapeka vi mimi ka tomilo" is seen at 6.0-10.5 s and
/// again from 144.0 s: 133.5 s after it was last seen, beyond the 120 s window
/// of `LONG_REPEAT_S`. So the repeat is a new utterance under V2 too, and both
/// transcripts keep it (V1 and V2 are equal here).
#[test]
fn history_v2_keeps_the_repeat_of_a_long_line_beyond_the_window() {
    let tmp = TempDir::new().unwrap();
    let dirs = copy_all(tmp.path());
    let dir = &dirs
        .iter()
        .find(|(n, _)| n == "synthetic_history")
        .unwrap()
        .1;
    let a = texts(&build(dir, &v1()).unwrap());
    let b = texts(&build(dir, &v2()).unwrap());
    let line = "lolo sapeka vi mimi ka tomilo";
    assert_eq!(
        a.iter().filter(|t| *t == line).count(),
        2,
        "V1 committed it twice"
    );
    assert_eq!(
        b.iter().filter(|t| *t == line).count(),
        2,
        "V2 keeps the repeat: it is beyond the window"
    );
    assert_eq!(a, b, "the two transcripts are equal");
}

#[test]
fn reappear_v2_folds_the_grown_line_into_its_first_segment() {
    let tmp = TempDir::new().unwrap();
    let dirs = copy_all(tmp.path());
    let dir = &dirs
        .iter()
        .find(|(n, _)| n == "synthetic_reappear")
        .unwrap()
        .1;
    let b = build(dir, &v2()).unwrap();
    assert_eq!(
        texts(&b),
        vec![
            "esta frase vuelve a aparecer tal cual y ahora crece un poco mas todavia",
            "otra cosa completamente distinta ahora",
        ]
    );
}

#[test]
fn execute_under_v2_writes_the_outputs_and_indexes_the_run() {
    let tmp = TempDir::new().unwrap();
    let dir = copy_all(tmp.path())
        .into_iter()
        .find(|(n, _)| n == "synthetic_guests")
        .unwrap()
        .1;
    let sid = mc_store::manifest::Manifest::load(dir.join("manifest.json"))
        .unwrap()
        .session_id;
    let mut store = Store::open(tmp.path().join("index.sqlite")).unwrap();

    let first = execute(&dir, &v2(), Some(&mut store)).unwrap();
    assert_eq!(first.action, RunAction::Created);
    assert_eq!(first.report["config"]["policy"], "v2");
    for file in [
        "transcript.txt",
        "transcript.md",
        "segments.jsonl",
        "events.jsonl",
        "report.json",
    ] {
        assert!(first.path.join(file).is_file(), "{file} missing");
    }
    let md = fs::read_to_string(first.path.join("transcript.md")).unwrap();
    assert!(md.contains("**["), "markdown turn headers");

    let second = execute(&dir, &v2(), Some(&mut store)).unwrap();
    assert_eq!(second.action, RunAction::Unchanged);
    assert_eq!(second.run_id, first.run_id);
    assert_eq!(store.runs(&sid).unwrap(), vec![first.run_id.clone()]);

    let row = store.run(&first.run_id).unwrap().expect("run is indexed");
    let config: Value = serde_json::from_str(&row.config_json).unwrap();
    assert_eq!(config["policy"], "v2");
    let indexed: Vec<String> = store
        .run_segments(&first.run_id)
        .unwrap()
        .into_iter()
        .map(|row| row.text)
        .collect();
    let built = build(&dir, &v2()).unwrap();
    assert_eq!(indexed, texts(&built));
    assert!(!indexed.is_empty());
}
