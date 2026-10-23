//! Runs (`mc_engine::runs`): what `build` renders for every corpus, that the
//! outputs and the run id are deterministic, the atomic `write`, the Markdown
//! and report details, and the index round trip through `execute`. Corpora are
//! read in place by `build`; anything written lives in a temp directory.

use std::fs;
use std::path::{Path, PathBuf};

use mc_engine::runs::{
    FlushMode, RunAction, RunArtifacts, RunConfig, build, execute, fmt_rel, md_escape, rel_ms,
    write,
};
use mc_store::Store;
use mc_store::jsonl::{ParsedLine, read_bytes};
use mc_store::manifest::Manifest;
use serde_json::{Value, json};
use tempfile::TempDir;

const CORPORA: usize = 17;

const RUN_FILES: [&str; 5] = [
    "transcript.txt",
    "transcript.md",
    "segments.jsonl",
    "events.jsonl",
    "report.json",
];

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/corpus")
}

fn corpus_dirs() -> Vec<PathBuf> {
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

/// Copies a flat corpus directory to `root/<name>` and returns the copy.
fn copy_corpus(name: &str, root: &Path) -> PathBuf {
    let src = corpus_root().join(name);
    let dst = root.join(name);
    fs::create_dir_all(&dst).unwrap();
    for entry in fs::read_dir(&src).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            fs::copy(entry.path(), dst.join(entry.file_name())).unwrap();
        }
    }
    dst
}

fn expected_transcript(dir: &Path) -> String {
    fs::read_to_string(dir.join("expected_transcript.txt")).unwrap()
}

/// `[speaker]: text` lines split back into `(speaker, text)`.
fn parse_transcript(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(|line| {
            let rest = line
                .strip_prefix('[')
                .unwrap_or_else(|| panic!("no '[' in {line:?}"));
            let (speaker, text) = rest
                .split_once("]: ")
                .unwrap_or_else(|| panic!("no ']: ' in {line:?}"));
            (speaker.to_string(), text.to_string())
        })
        .collect()
}

/// The turn headers of a Markdown transcript as `(time, name)`.
fn md_headers(md: &str) -> Vec<(String, String)> {
    md.lines()
        .filter_map(|line| line.strip_prefix("**["))
        .map(|rest| {
            let (time, name) = rest
                .split_once("] ")
                .unwrap_or_else(|| panic!("bad header {rest:?}"));
            let name = name
                .strip_suffix("**")
                .unwrap_or_else(|| panic!("header without closing ** {rest:?}"));
            (time.to_string(), name.to_string())
        })
        .collect()
}

fn default_cfg() -> RunConfig {
    RunConfig::default()
}

fn manifest_only() -> RunConfig {
    RunConfig {
        flush: FlushMode::ManifestOnly,
        ..RunConfig::default()
    }
}

fn append_bytes(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

fn file_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

// ------------------------------------------------- 1. every corpus transcript

/// `build(...).txt` equals `expected_transcript.txt` for every corpus. The
/// unclean end has no recorded end, so it is built as `verify` does (manifest
/// flush only); under `Auto` it would add a line the recorder never wrote.
#[test]
fn every_corpus_builds_the_recorded_transcript() {
    let dirs = corpus_dirs();
    assert_eq!(dirs.len(), CORPORA, "corpus count changed");
    for dir in &dirs {
        let name = name_of(dir);
        let cfg = if name == "synthetic_unclean_end" {
            manifest_only()
        } else {
            default_cfg()
        };
        let artifacts = build(dir, &cfg).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            artifacts.txt,
            expected_transcript(dir),
            "{name}: transcript differs"
        );
    }
}

#[test]
fn skew_divergent_run_reports_timing_only() {
    let artifacts = build(
        corpus_root().join("synthetic_skew_divergent"),
        &default_cfg(),
    )
    .unwrap();
    let divergences = &artifacts.report["divergences"];
    assert_eq!(divergences["content"], 0, "{divergences}");
    assert!(divergences["timing"].as_u64().unwrap() > 0, "{divergences}");
}

// ------------------------------------------------------- 2. determinism

#[test]
fn building_twice_gives_byte_equal_artifacts() {
    for dir in corpus_dirs() {
        let name = name_of(&dir);
        let first = build(&dir, &default_cfg()).unwrap();
        let second = build(&dir, &default_cfg()).unwrap();
        assert_eq!(first.run_id, second.run_id, "{name}: run_id");
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
fn writing_twice_creates_then_leaves_the_run_unchanged() {
    let tmp = TempDir::new().unwrap();
    let dir = copy_corpus("synthetic_basic", tmp.path());
    let artifacts = build(&dir, &default_cfg()).unwrap();

    let first = write(&dir, &artifacts).unwrap();
    assert_eq!(first.action, RunAction::Created);
    assert_eq!(first.run_id, artifacts.run_id);
    assert_eq!(first.path, dir.join("runs").join(&artifacts.run_id));

    let contents: [&String; 5] = [
        &artifacts.txt,
        &artifacts.md,
        &artifacts.segments_jsonl,
        &artifacts.events_jsonl,
        &artifacts.report_json,
    ];
    let mut snapshot = Vec::new();
    for (name, content) in RUN_FILES.iter().zip(contents) {
        let bytes = fs::read(first.path.join(name)).unwrap();
        assert_eq!(bytes, content.as_bytes(), "{name}");
        snapshot.push(bytes);
    }

    let second = write(&dir, &artifacts).unwrap();
    assert_eq!(second.action, RunAction::Unchanged);
    assert_eq!(second.path, first.path);
    for (name, before) in RUN_FILES.iter().zip(&snapshot) {
        assert_eq!(&fs::read(first.path.join(name)).unwrap(), before, "{name}");
    }
    assert_eq!(
        file_names(&dir.join("runs")),
        vec![artifacts.run_id.clone()]
    );
}

// ------------------------------------------------------- 3. run id

#[test]
fn run_id_follows_config_and_inputs_but_not_an_unfinished_tail() {
    let tmp = TempDir::new().unwrap();
    let dir = copy_corpus("synthetic_basic", tmp.path());
    let captions = dir.join("captions.jsonl");
    let base_cfg = default_cfg();
    let base = build(&dir, &base_cfg).unwrap();
    assert!(base.run_id.starts_with('r'), "{}", base.run_id);
    assert_eq!(base.run_id.len(), 17, "{}", base.run_id);

    let mut debounce = default_cfg();
    debounce.reconciler.debounce_s = base_cfg.reconciler.debounce_s + 0.5;
    assert_ne!(
        build(&dir, &debounce).unwrap().run_id,
        base.run_id,
        "debounce_s"
    );

    let mut similarity = default_cfg();
    similarity.reconciler.merge_similarity = base_cfg.reconciler.merge_similarity + 0.01;
    assert_ne!(
        build(&dir, &similarity).unwrap().run_id,
        base.run_id,
        "merge_similarity"
    );

    let flush = manifest_only();
    assert_ne!(
        build(&dir, &flush).unwrap().run_id,
        base.run_id,
        "flush mode"
    );

    // A complete new line changes the input, so the id changes.
    let original = fs::read(&captions).unwrap();
    let last = original
        .split(|b| *b == b'\n')
        .rfind(|line| !line.is_empty())
        .unwrap();
    let mut value: Value = serde_json::from_slice(last).unwrap();
    value["seq"] = json!(value["seq"].as_u64().unwrap() + 1);
    append_bytes(&captions, format!("{value}\n").as_bytes());
    let appended = build(&dir, &base_cfg).unwrap();
    assert_ne!(appended.run_id, base.run_id, "appended captions line");

    // An unfinished tail is not part of the run: the id and the text stay.
    let tail: &[u8] = br#"{"v": 2, "seq": 99, "fra"#;
    append_bytes(&captions, tail);
    let with_tail = build(&dir, &base_cfg).unwrap();
    assert_eq!(
        with_tail.run_id, appended.run_id,
        "unfinished tail changed the id"
    );
    assert_eq!(with_tail.txt, appended.txt);
    assert_eq!(with_tail.tail_pending_bytes, tail.len());
    assert_eq!(with_tail.report_json, appended.report_json);
}

// ------------------------------------------------------- 4. unclean end

#[test]
fn unclean_end_is_flushed_synthetically_or_left_open() {
    let tmp = TempDir::new().unwrap();
    let dir = copy_corpus("synthetic_unclean_end", tmp.path());

    let auto = build(
        &dir,
        &RunConfig {
            flush: FlushMode::Auto,
            ..RunConfig::default()
        },
    )
    .unwrap();
    assert_eq!(auto.report["synthetic_flush"], true, "{}", auto.report);
    assert_eq!(auto.report["ended_cleanly"], false);
    assert!(auto.md.contains('⚠'), "auto md has no warning");
    assert!(
        auto.txt.contains("tercera frase"),
        "auto did not flush the open line"
    );

    let open = build(&dir, &manifest_only()).unwrap();
    assert_eq!(open.report["synthetic_flush"], false, "{}", open.report);
    assert_eq!(open.report["ended_cleanly"], false);
    assert_eq!(
        open.report["open_lines"],
        json!(["[Ana]: tercera frase todavia abierta"]),
    );
    assert!(open.md.contains('⚠'), "manifest-only md has no warning");
    assert!(!open.txt.contains("tercera frase"));
}

// ------------------------------------------------------- 5. no captions

#[test]
fn a_session_without_captions_still_writes_all_five_files() {
    let tmp = TempDir::new().unwrap();
    let dir = copy_corpus("synthetic_no_captions", tmp.path());
    let artifacts = build(&dir, &default_cfg()).unwrap();
    let outcome = write(&dir, &artifacts).unwrap();
    assert_eq!(outcome.action, RunAction::Created);

    for name in RUN_FILES {
        assert!(outcome.path.join(name).is_file(), "{name} missing");
    }
    let txt = fs::read_to_string(outcome.path.join("transcript.txt")).unwrap();
    assert!(txt.is_empty(), "txt: {txt:?}");
    let segments = fs::read_to_string(outcome.path.join("segments.jsonl")).unwrap();
    assert!(segments.is_empty(), "segments: {segments:?}");
    let md = fs::read_to_string(outcome.path.join("transcript.md")).unwrap();
    assert!(md.contains("Sin transcripción"), "md: {md}");
    let report: Value =
        serde_json::from_str(&fs::read_to_string(outcome.path.join("report.json")).unwrap())
            .unwrap();
    assert_eq!(report["empty"], true);
}

// ------------------------------------------------------- 6. time format

#[test]
fn fmt_rel_formats_minutes_hours_and_unknown_values() {
    assert_eq!(fmt_rel(Some(0.0)), "00:00");
    assert_eq!(fmt_rel(Some(59_999.0)), "00:59");
    assert_eq!(fmt_rel(Some(60_000.0)), "01:00");
    assert_eq!(fmt_rel(Some(3_600_000.0)), "1:00:00");
    assert_eq!(fmt_rel(None), "--:--");
    assert_eq!(fmt_rel(Some(-5.0)), "00:00");
    assert_eq!(fmt_rel(Some(f64::NAN)), "--:--");
}

// ------------------------------------------------------- 7. markdown

#[test]
fn md_escape_neutralises_block_and_inline_syntax() {
    assert_eq!(md_escape("# a *b* _c_ [d]"), r"\# a \*b\* \_c\_ \[d\]");
    assert_eq!(md_escape("1. x"), r"1\. x");
    assert_eq!(md_escape("- y"), r"\- y");
}

/// One header per run of consecutive segments by the same speaker, with the
/// time of the run's first segment.
fn check_turns(name: &str, headers: usize) {
    let dir = corpus_root().join(name);
    let artifacts = build(&dir, &default_cfg()).unwrap();
    let transcript = parse_transcript(&expected_transcript(&dir));
    assert_eq!(artifacts.segments.len(), transcript.len(), "{name}");

    let mut expected = Vec::new();
    for (index, (speaker, _)) in transcript.iter().enumerate() {
        if index == 0 || transcript[index - 1].0 != *speaker {
            let time = fmt_rel(rel_ms(&artifacts.segments[index]));
            expected.push((time, md_escape(speaker)));
        }
    }
    assert_eq!(expected.len(), headers, "{name}: test expectation");
    assert_eq!(md_headers(&artifacts.md), expected, "{name}");
}

#[test]
fn same_speaker_twice_shares_one_turn_header() {
    check_turns("synthetic_same_speaker_twice", 4);
}

#[test]
fn guests_turns_group_by_speaker_run() {
    check_turns("synthetic_guests", 7);
}

#[test]
fn unicode_turns_group_by_speaker_run() {
    check_turns("synthetic_unicode", 3);
}

// ------------------------------------------------------- 8. jsonl outputs

#[test]
fn every_segment_and_event_line_parses() {
    for dir in corpus_dirs() {
        let name = name_of(&dir);
        let artifacts: RunArtifacts = build(&dir, &default_cfg()).unwrap();

        let segments: Vec<Value> = artifacts
            .segments_jsonl
            .lines()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{name}: {e}")))
            .collect();
        assert_eq!(
            segments.len() as u64,
            artifacts.report["segments"].as_u64().unwrap(),
            "{name}: segment count"
        );
        for (index, segment) in segments.iter().enumerate() {
            assert_eq!(segment["idx"].as_u64(), Some(index as u64), "{name}");
        }

        let events = read_bytes(artifacts.events_jsonl.as_bytes());
        assert!(events.bad.is_empty(), "{name}: {:?}", events.bad);
        assert!(
            events
                .lines
                .iter()
                .all(|(_, line)| matches!(line, ParsedLine::V2(_))),
            "{name}: event line is not v2"
        );
        assert_eq!(
            events.lines.len() as u64,
            artifacts.report["records"].as_u64().unwrap(),
            "{name}: record count"
        );
    }
}

// ------------------------------------------------------- 9. atomic write

/// A directory handle that can change its timestamps.
fn open_dir(path: &Path) -> fs::File {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        fs::OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .unwrap()
    }
    #[cfg(not(windows))]
    fs::File::open(path).unwrap()
}

#[test]
fn write_removes_a_leftover_temporary_directory() {
    let tmp = TempDir::new().unwrap();
    let dir = copy_corpus("synthetic_basic", tmp.path());
    let artifacts = build(&dir, &default_cfg()).unwrap();

    let leftover = dir
        .join("runs")
        .join(format!(".{}.tmp-1", artifacts.run_id));
    fs::create_dir_all(&leftover).unwrap();
    fs::write(leftover.join("transcript.txt"), "partial").unwrap();
    // A foreign directory is only abandoned once it is old; a young one may
    // belong to a live concurrent writer.
    let young = dir
        .join("runs")
        .join(format!(".{}.tmp-2", artifacts.run_id));
    fs::create_dir_all(&young).unwrap();
    let two_hours_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
    open_dir(&leftover).set_modified(two_hours_ago).unwrap();

    let outcome = write(&dir, &artifacts).unwrap();
    assert_eq!(outcome.action, RunAction::Created);
    assert!(!leftover.exists(), "abandoned temporary directory survived");
    assert!(
        young.exists(),
        "a young foreign temporary directory was removed"
    );
    fs::remove_dir_all(&young).unwrap();
    assert_eq!(
        file_names(&dir.join("runs")),
        vec![artifacts.run_id.clone()]
    );
}

#[test]
fn a_failed_write_returns_an_error_and_leaves_no_run_directory() {
    let tmp = TempDir::new().unwrap();
    let dir = copy_corpus("synthetic_basic", tmp.path());
    let artifacts = build(&dir, &default_cfg()).unwrap();

    // `runs` is a regular file, so the directory cannot be created.
    fs::write(dir.join("runs"), "not a directory").unwrap();
    assert!(write(&dir, &artifacts).is_err());
    assert!(dir.join("runs").is_file());
    assert!(!dir.join("runs").join(&artifacts.run_id).exists());
}

#[test]
fn a_file_squatting_the_run_path_is_an_error_and_is_left_alone() {
    let tmp = TempDir::new().unwrap();
    let dir = copy_corpus("synthetic_basic", tmp.path());
    let artifacts = build(&dir, &default_cfg()).unwrap();

    let runs = dir.join("runs");
    fs::create_dir_all(&runs).unwrap();
    let squatter = runs.join(&artifacts.run_id);
    fs::write(&squatter, "squatter").unwrap();

    assert!(write(&dir, &artifacts).is_err());
    assert_eq!(fs::read_to_string(&squatter).unwrap(), "squatter");
    assert_eq!(file_names(&runs), vec![artifacts.run_id.clone()]);
}

// ------------------------------------------------------- 10. speakers

/// Per-speaker counts computed from `expected_transcript.txt` alone, in order
/// of first appearance: `(name, segments, words, chars)`.
#[test]
fn guests_speaker_counts_match_the_transcript() {
    let dir = corpus_root().join("synthetic_guests");
    let artifacts = build(&dir, &default_cfg()).unwrap();

    let mut expected: Vec<(String, usize, usize, usize)> = Vec::new();
    for (speaker, text) in parse_transcript(&expected_transcript(&dir)) {
        let index = match expected.iter().position(|(name, ..)| *name == speaker) {
            Some(index) => index,
            None => {
                expected.push((speaker, 0, 0, 0));
                expected.len() - 1
            }
        };
        let entry = &mut expected[index];
        entry.1 += 1;
        entry.2 += text.split_whitespace().count();
        entry.3 += text.chars().count();
    }

    let actual: Vec<(String, usize, usize, usize)> = artifacts.report["speakers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["name"].as_str().unwrap().to_string(),
                s["segments"].as_u64().unwrap() as usize,
                s["words"].as_u64().unwrap() as usize,
                s["chars"].as_u64().unwrap() as usize,
            )
        })
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(artifacts.report["segments"], json!(8));
}

// ------------------------------------------------------- 11. the index

#[test]
fn execute_twice_creates_then_leaves_one_indexed_run() {
    let tmp = TempDir::new().unwrap();
    let dir = copy_corpus("synthetic_guests", tmp.path());
    let sid = Manifest::load(dir.join("manifest.json"))
        .unwrap()
        .session_id;
    let mut store = Store::open(tmp.path().join("index.sqlite")).unwrap();

    let first = execute(&dir, &default_cfg(), Some(&mut store)).unwrap();
    assert_eq!(first.action, RunAction::Created);
    let second = execute(&dir, &default_cfg(), Some(&mut store)).unwrap();
    assert_eq!(second.action, RunAction::Unchanged);
    assert_eq!(second.run_id, first.run_id);

    assert_eq!(store.runs(&sid).unwrap(), vec![first.run_id.clone()]);
    let rows = store.run_segments(&first.run_id).unwrap();
    let texts: Vec<String> = rows.into_iter().map(|row| row.text).collect();
    let expected: Vec<String> = parse_transcript(&expected_transcript(&dir))
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    assert_eq!(texts, expected);
}

#[test]
fn run_id_changes_when_the_manifest_changes() {
    let tmp = TempDir::new().unwrap();
    let dir = copy_corpus("synthetic_basic", tmp.path());
    let before = build(&dir, &default_cfg()).unwrap();

    let path = dir.join("manifest.json");
    let mut manifest = mc_store::manifest::Manifest::load(&path).unwrap();
    manifest.meeting_name = Some("Otro título".to_string());
    manifest.save_atomic(&path).unwrap();

    let after = build(&dir, &default_cfg()).unwrap();
    assert_ne!(before.run_id, after.run_id);
    assert!(after.md.contains("Otro título"));
}

#[test]
fn auto_flush_does_not_count_as_a_content_divergence() {
    let tmp = TempDir::new().unwrap();
    let dir = copy_corpus("synthetic_unclean_end", tmp.path());
    let artifacts = build(&dir, &default_cfg()).unwrap();
    assert_eq!(artifacts.report["synthetic_flush"], json!(true));
    assert_eq!(artifacts.report["divergences"]["content"], json!(0));
}

#[test]
fn force_rewrites_an_existing_run() {
    let tmp = TempDir::new().unwrap();
    let dir = copy_corpus("synthetic_basic", tmp.path());
    let artifacts = build(&dir, &default_cfg()).unwrap();
    assert_eq!(write(&dir, &artifacts).unwrap().action, RunAction::Created);
    let transcript = dir
        .join("runs")
        .join(&artifacts.run_id)
        .join("transcript.txt");
    fs::write(&transcript, "stale").unwrap();

    assert_eq!(
        write(&dir, &artifacts).unwrap().action,
        RunAction::Unchanged
    );
    assert_eq!(fs::read_to_string(&transcript).unwrap(), "stale");
    let forced = mc_engine::runs::write_with(&dir, &artifacts, true).unwrap();
    assert_eq!(forced.action, RunAction::Created);
    assert_eq!(fs::read_to_string(&transcript).unwrap(), artifacts.txt);
}

#[test]
fn chosen_speaker_names_are_display_only_and_change_the_run_id() {
    let tmp = TempDir::new().unwrap();
    let dir = copy_corpus("synthetic_basic", tmp.path());
    let plain = build(&dir, &default_cfg()).unwrap();
    let captions_before = fs::read(dir.join("captions.jsonl")).unwrap();

    fs::write(dir.join("speaker_names.json"), r#"{"Luis": "Luis G."}"#).unwrap();
    let named = build(&dir, &default_cfg()).unwrap();

    assert_ne!(named.run_id, plain.run_id);
    assert!(named.txt.contains("[Luis G.]: Yo tengo un bloqueo"));
    assert!(!named.txt.contains("[Luis]:"));
    assert!(named.md.contains("Luis G."));
    // What the platform reported stays: in the segments, the report and the captions.
    assert_eq!(named.segments, plain.segments);
    assert!(named.segments_jsonl.contains("\"speaker\":\"Luis\""));
    assert!(
        named
            .segments_jsonl
            .contains("\"display_speaker\":\"Luis G.\"")
    );
    assert!(!plain.segments_jsonl.contains("display_speaker"));
    assert_eq!(named.report["speakers"], plain.report["speakers"]);
    assert_eq!(
        fs::read(dir.join("captions.jsonl")).unwrap(),
        captions_before
    );

    // No names: the run is the one it always was.
    fs::write(dir.join("speaker_names.json"), "{}").unwrap();
    assert_eq!(build(&dir, &default_cfg()).unwrap().run_id, plain.run_id);
}

#[test]
fn swapped_speaker_names_are_each_applied_once() {
    let tmp = TempDir::new().unwrap();
    let dir = copy_corpus("synthetic_basic", tmp.path());
    fs::write(
        dir.join("speaker_names.json"),
        r#"{"Luis": "Marta", "Marta": "Luis"}"#,
    )
    .unwrap();
    let swapped = build(&dir, &default_cfg()).unwrap();
    assert!(swapped.txt.contains("[Marta]: Yo tengo un bloqueo"));
    assert!(swapped.txt.contains("[Luis]: Yo hice push ayer"));
}
