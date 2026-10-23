//! SQLite index (`mc_store::db`): migrations, the incremental import rules
//! (created, appended, cut tail, rebuilt, conflict, missing file) and the
//! round trip of tricky values. The corpus is read in place; anything edited
//! lives in a temp directory.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::thread;

use mc_store::StoreError;
use mc_store::db::{
    AsrRunRow, FusedRow, FusionRow, ImportAction, RebuildReason, RunRow, SegmentRow,
    SessionSummary, Store,
};
use mc_store::jsonl::{self, CaptionLine, CaptionWriter, LegacyLine, ParsedLine};
use mc_store::manifest::Manifest;
use mc_store::series::SeriesOrigin;
use serde_json::{Map, json};
use tempfile::TempDir;

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/corpus")
}

fn synthetic_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(corpus_root())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("synthetic_"))
        })
        .collect();
    dirs.sort();
    dirs
}

/// Copies a corpus session to `dst` (which must not exist) so it can be edited.
fn copy_to(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), dst.join(entry.file_name())).unwrap();
    }
}

/// A session directory with only a manifest (based on `synthetic_basic`) and a
/// session id of its own, for files written by the test.
fn bare_session(root: &Path, session_id: &str) -> PathBuf {
    let dir = root.join(session_id);
    fs::create_dir_all(&dir).unwrap();
    let mut manifest = Manifest::load(corpus_root().join("synthetic_basic/manifest.json")).unwrap();
    manifest.session_id = session_id.into();
    manifest.save_atomic(dir.join("manifest.json")).unwrap();
    dir
}

fn session_id(dir: &Path) -> String {
    Manifest::load(dir.join("manifest.json"))
        .unwrap()
        .session_id
}

/// The lines of a JSONL file in file order; the file must have no bad lines.
fn file_lines(path: &Path) -> Vec<ParsedLine> {
    let report = jsonl::read_file(path).unwrap();
    assert!(
        report.bad.is_empty(),
        "bad lines in {}: {:?}",
        path.display(),
        report.bad
    );
    report.lines.into_iter().map(|(_, line)| line).collect()
}

fn append_bytes(path: &Path, bytes: &[u8]) {
    OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

/// The first `n` complete lines of `bytes`.
fn first_lines(bytes: &[u8], n: usize) -> &[u8] {
    let end = bytes
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .nth(n - 1)
        .map_or(bytes.len(), |(index, _)| index + 1);
    &bytes[..end]
}

fn caption(seq: u64, text: &str) -> CaptionLine {
    let wall_ms = 1_700_000_100_000 + seq as i64 * 1000;
    CaptionLine {
        seq,
        frame: Some(seq),
        t: wall_ms as f64 / 1000.0,
        wall_ms,
        mono_ms: 100_000.0 + seq as f64 * 1000.0,
        read_ms: Some(500.0),
        speaker: "Ana".into(),
        text: text.into(),
        decision: "new_open".into(),
        speaker_raw: None,
        extra: Map::new(),
    }
}

fn decision_of(line: &ParsedLine) -> &str {
    match line {
        ParsedLine::V1(l) => &l.decision,
        ParsedLine::V2(l) => &l.decision,
    }
}

fn expect_v2(line: &ParsedLine) -> &CaptionLine {
    match line {
        ParsedLine::V2(l) => l,
        other => panic!("expected a v2 line, got {other:?}"),
    }
}

// ------------------------------------------------------------ 1. migrations

#[test]
fn fresh_database_reaches_latest_schema_version() {
    let temp = TempDir::new().unwrap();
    let file_store = Store::open(temp.path().join("index.sqlite")).unwrap();
    assert_eq!(file_store.schema_version().unwrap(), 4);

    let memory_store = Store::open_in_memory().unwrap();
    assert_eq!(memory_store.schema_version().unwrap(), 4);
}

#[test]
fn reopening_a_file_database_is_a_no_op() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("index.sqlite");
    let dir = temp.path().join("session");
    copy_to(&corpus_root().join("synthetic_basic"), &dir);

    let before = {
        let mut store = Store::open(&path).unwrap();
        store.import_session(&dir).unwrap();
        store.list_sessions().unwrap()
    };

    let store = Store::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 4);
    assert_eq!(store.list_sessions().unwrap(), before);
}

#[test]
fn database_from_a_newer_build_is_refused() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("index.sqlite");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA user_version=99").unwrap();
    }
    let err = Store::open(&path)
        .err()
        .expect("a schema newer than this build must be refused");
    assert!(
        matches!(
            err,
            StoreError::SchemaTooNew {
                found: 99,
                known: 4
            }
        ),
        "unexpected error: {err}"
    );
}

// ------------------------------------------- 2. every synthetic corpus session

#[test]
fn every_synthetic_session_imports_once_then_unchanged() {
    let dirs = synthetic_dirs();
    assert!(!dirs.is_empty(), "no synthetic corpus sessions found");
    let temp = TempDir::new().unwrap();
    let mut store = Store::open(temp.path().join("index.sqlite")).unwrap();

    let first = store.import_all(corpus_root()).unwrap();
    assert_eq!(first.len(), dirs.len());
    for (dir, result) in &first {
        let report = result
            .as_ref()
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
        assert_eq!(report.action, ImportAction::Created, "{}", dir.display());
        assert!(report.bad.is_empty(), "{}: {:?}", dir.display(), report.bad);
        // synthetic_unclean_end ends in a line cut mid-write (no newline): it
        // stays pending until a newline arrives, which is the intended behaviour.
        if dir.ends_with("synthetic_unclean_end") {
            assert!(report.tail_pending_bytes > 0, "{}", dir.display());
        } else {
            assert_eq!(report.tail_pending_bytes, 0, "{}", dir.display());
        }
    }

    let counts = |store: &Store| -> Vec<(String, u64)> {
        store
            .list_sessions()
            .unwrap()
            .into_iter()
            .map(|s| (s.session_id, s.captions))
            .collect()
    };
    let before = counts(&store);

    let second = store.import_all(corpus_root()).unwrap();
    for (dir, result) in &second {
        let report = result
            .as_ref()
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
        assert_eq!(report.action, ImportAction::Unchanged, "{}", dir.display());
        assert_eq!(report.lines_added, 0, "{}", dir.display());
    }
    assert_eq!(counts(&store), before);

    for dir in &dirs {
        let sid = session_id(dir);
        let expected = file_lines(&dir.join("captions.jsonl"));
        assert_eq!(store.captions(&sid).unwrap(), expected, "{}", dir.display());
    }
}

// ------------------------------------------------------ 3. appended records

#[test]
fn records_appended_with_the_writer_import_as_appended() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("session");
    copy_to(&corpus_root().join("synthetic_basic"), &dir);
    let captions = dir.join("captions.jsonl");
    let sid = session_id(&dir);

    let mut store = Store::open_in_memory().unwrap();
    store.import_session(&dir).unwrap();
    let before = store.captions(&sid).unwrap().len();

    const N: u64 = 7;
    let start = jsonl::read_file(&captions).unwrap().next_seq();
    let mut writer = CaptionWriter::open_append(&captions, start).unwrap();
    for i in 0..N {
        writer
            .write_line(&caption(start + i, &format!("nueva {i}")))
            .unwrap();
    }
    drop(writer);

    let report = store.import_session(&dir).unwrap();
    assert_eq!(report.action, ImportAction::Appended);
    assert_eq!(report.lines_added, N as usize);
    assert!(report.bad.is_empty(), "{:?}", report.bad);
    assert_eq!(report.tail_pending_bytes, 0);

    let stored = store.captions(&sid).unwrap();
    assert_eq!(stored.len(), before + N as usize);
    assert_eq!(stored, file_lines(&captions));
}

// ------------------------------------------------------------ 4. cut tail

#[test]
fn a_cut_tail_waits_for_its_newline_and_is_imported_once() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("session");
    copy_to(&corpus_root().join("synthetic_basic"), &dir);
    let captions = dir.join("captions.jsonl");
    let sid = session_id(&dir);

    let mut store = Store::open_in_memory().unwrap();
    store.import_session(&dir).unwrap();
    let before = store.captions(&sid).unwrap().len();

    let partial = br#"{"v": 2, "seq": 13, "frame": null, "t": 1700000009.0, "wall_ms": 1700000009000, "mono_ms": 9000.0, "read_ms": null, "speaker": "Ana", "text": "a medias"#;
    append_bytes(&captions, partial);
    let report = store.import_session(&dir).unwrap();
    assert_eq!(report.tail_pending_bytes, partial.len());
    assert_eq!(report.lines_added, 0);
    assert_eq!(store.captions(&sid).unwrap().len(), before);

    // The partial line ends inside the "text" string, so the completion closes it.
    append_bytes(&captions, b"\", \"decision\": \"committed\"}\n");
    let report = store.import_session(&dir).unwrap();
    assert_eq!(report.action, ImportAction::Appended);
    assert_eq!(report.lines_added, 1);
    assert_eq!(report.tail_pending_bytes, 0);

    let stored = store.captions(&sid).unwrap();
    assert_eq!(stored.len(), before + 1);
    let completed = CaptionLine {
        seq: 13,
        frame: None,
        t: 1_700_000_009.0,
        wall_ms: 1_700_000_009_000,
        mono_ms: 9000.0,
        read_ms: None,
        speaker: "Ana".into(),
        text: "a medias".into(),
        decision: "committed".into(),
        speaker_raw: None,
        extra: Map::new(),
    };
    assert_eq!(stored.last(), Some(&ParsedLine::V2(completed)));

    let again = store.import_session(&dir).unwrap();
    assert_eq!(again.action, ImportAction::Unchanged);
    assert_eq!(again.lines_added, 0);
    assert_eq!(store.captions(&sid).unwrap(), file_lines(&captions));
}

// ------------------------------------------------- 5. shrunk and replaced

#[test]
fn a_shrunk_file_is_rebuilt_from_scratch() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("session");
    copy_to(&corpus_root().join("synthetic_basic"), &dir);
    let captions = dir.join("captions.jsonl");
    let sid = session_id(&dir);

    let mut store = Store::open_in_memory().unwrap();
    store.import_session(&dir).unwrap();
    let original = fs::read(&captions).unwrap();
    fs::write(&captions, first_lines(&original, 5)).unwrap();

    let report = store.import_session(&dir).unwrap();
    assert_eq!(report.action, ImportAction::Rebuilt(RebuildReason::Shrunk));
    assert_eq!(report.lines_added, 5);
    let stored = store.captions(&sid).unwrap();
    assert_eq!(stored.len(), 5);
    assert_eq!(stored, file_lines(&captions));
}

#[test]
fn a_same_size_rewrite_is_rebuilt_from_scratch() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("session");
    copy_to(&corpus_root().join("synthetic_unclean_end"), &dir);
    let captions = dir.join("captions.jsonl");
    let sid = session_id(&dir);

    let mut store = Store::open_in_memory().unwrap();
    store.import_session(&dir).unwrap();
    let original = fs::read_to_string(&captions).unwrap();
    // Same length, different content in the first line.
    let rewritten = original.replace("primera frase", "PRIMERA frase");
    assert_eq!(rewritten.len(), original.len());
    assert_ne!(rewritten, original);
    fs::write(&captions, &rewritten).unwrap();

    let expected = file_lines(&captions);
    let report = store.import_session(&dir).unwrap();
    assert_eq!(
        report.action,
        ImportAction::Rebuilt(RebuildReason::Replaced)
    );
    assert_eq!(report.lines_added, expected.len());
    let stored = store.captions(&sid).unwrap();
    assert_eq!(stored, expected);
    assert_eq!(expect_v2(&stored[0]).text, "PRIMERA frase de la reunion");
}

// ------------------------------------------------------ 6. ended manifest

#[test]
fn editing_the_manifest_to_ended_updates_the_summary_without_reimporting() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("session");
    copy_to(&corpus_root().join("synthetic_unclean_end"), &dir);
    let manifest_path = dir.join("manifest.json");
    let sid = session_id(&dir);

    let mut store = Store::open_in_memory().unwrap();
    store.import_session(&dir).unwrap();
    let before: SessionSummary = store.list_sessions().unwrap().remove(0);
    assert_eq!(before.ended_wall_ms, None);

    let mut manifest = Manifest::load(&manifest_path).unwrap();
    manifest.ended_wall_ms = Some(1_700_000_012_000);
    manifest.ended_utc_iso = Some("2023-11-14T22:13:32.000+00:00".into());
    manifest.end_reason = Some("stop".into());
    manifest.save_atomic(&manifest_path).unwrap();

    let report = store.import_session(&dir).unwrap();
    assert_eq!(report.action, ImportAction::Unchanged);
    assert!(report.ended_cleanly);

    let after = store.list_sessions().unwrap().remove(0);
    assert_eq!(after.session_id, sid);
    assert_eq!(after.ended_wall_ms, Some(1_700_000_012_000));
    assert_eq!(after.captions, before.captions);
}

// ------------------------------------------------------ 7. legacy v1 file

#[test]
fn legacy_v1_lines_round_trip_as_v1() {
    let temp = TempDir::new().unwrap();
    let dir = bare_session(temp.path(), "1e6a0000000000000000000000000001");
    let sid = session_id(&dir);
    fs::write(
        dir.join("captions.jsonl"),
        concat!(
            r#"{"t":1.0,"speaker":"A","text":"x","decision":"committed"}"#,
            "\n",
            r#"{"t":2.5,"speaker":"B","text":"y","decision":"new_open"}"#,
            "\n",
        ),
    )
    .unwrap();

    let mut store = Store::open_in_memory().unwrap();
    let report = store.import_session(&dir).unwrap();
    assert_eq!(report.action, ImportAction::Created);
    assert!(report.bad.is_empty(), "{:?}", report.bad);

    let expected = vec![
        ParsedLine::V1(LegacyLine {
            t: 1.0,
            speaker: "A".into(),
            text: "x".into(),
            decision: "committed".into(),
        }),
        ParsedLine::V1(LegacyLine {
            t: 2.5,
            speaker: "B".into(),
            text: "y".into(),
            decision: "new_open".into(),
        }),
    ];
    assert_eq!(store.captions(&sid).unwrap(), expected);
    assert_eq!(store.committed_lines(&sid).unwrap(), expected[..1].to_vec());
}

// ----------------------------------------------------- 8. bad line in middle

#[test]
fn a_bad_line_in_the_middle_is_reported_and_the_rest_imported() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("session");
    copy_to(&corpus_root().join("synthetic_basic"), &dir);
    let captions = dir.join("captions.jsonl");
    let sid = session_id(&dir);

    let original = jsonl::read_file(&captions).unwrap();
    assert!(original.lines.iter().any(|(n, _)| *n == 5));
    let expected: Vec<ParsedLine> = original
        .lines
        .iter()
        .filter(|(n, _)| *n != 5)
        .map(|(_, line)| line.clone())
        .collect();

    let text = fs::read_to_string(&captions).unwrap();
    let mut lines: Vec<&str> = text.lines().collect();
    lines[4] = "{this is not json";
    fs::write(&captions, lines.join("\n") + "\n").unwrap();

    let mut store = Store::open_in_memory().unwrap();
    let report = store.import_session(&dir).unwrap();
    assert_eq!(report.bad.len(), 1, "{:?}", report.bad);
    assert_eq!(report.bad[0].line_no, 5);
    assert_eq!(report.lines_added, expected.len());
    assert_eq!(store.captions(&sid).unwrap(), expected);
    assert_eq!(store.list_sessions().unwrap()[0].bad_lines, 1);
}

// ------------------------------------------------ 9. awkward text round trip

#[test]
fn nul_emoji_crlf_and_bom_round_trip_exactly() {
    let temp = TempDir::new().unwrap();
    let dir = bare_session(temp.path(), "a0a0000000000000000000000000000a");
    let sid = session_id(&dir);

    let texts = [
        "nul\u{0}dentro del texto",
        "emoji 🎉 👩🏽‍💻 fin",
        "linea\r\ncon CRLF dentro",
        "comillas \" barra \\ tab\t",
        "BOM \u{feff} en medio",
    ];
    // A UTF-8 BOM at the start of the file, CRLF after every line.
    let mut bytes = b"\xEF\xBB\xBF".to_vec();
    let mut expected = Vec::new();
    for (i, text) in texts.iter().enumerate() {
        let line = caption(i as u64 + 1, text);
        bytes.extend_from_slice(line.to_value().to_string().as_bytes());
        bytes.extend_from_slice(b"\r\n");
        expected.push(ParsedLine::V2(line));
    }
    fs::write(dir.join("captions.jsonl"), &bytes).unwrap();

    let mut store = Store::open_in_memory().unwrap();
    let report = store.import_session(&dir).unwrap();
    assert!(report.bad.is_empty(), "{:?}", report.bad);
    assert_eq!(report.lines_added, texts.len());

    let stored = store.captions(&sid).unwrap();
    assert_eq!(stored, expected);
    for (line, text) in stored.iter().zip(texts) {
        assert_eq!(expect_v2(line).text, text);
    }
    assert_eq!(stored, file_lines(&dir.join("captions.jsonl")));
}

// --------------------------------------------------- 10. extreme wall clock

#[test]
fn extreme_wall_clock_values_round_trip() {
    let temp = TempDir::new().unwrap();
    let dir = bare_session(temp.path(), "e1e0000000000000000000000000000e");
    let sid = session_id(&dir);
    let captions = dir.join("captions.jsonl");

    let walls = [-86_400_000i64, -1, 0, i64::MAX - 1, i64::MAX];
    let mut writer = CaptionWriter::create_new(&captions).unwrap();
    for (i, wall) in walls.iter().enumerate() {
        let mut line = caption(i as u64 + 1, "reloj");
        line.wall_ms = *wall;
        line.t = *wall as f64 / 1000.0;
        writer.write_line(&line).unwrap();
    }
    drop(writer);

    let mut store = Store::open_in_memory().unwrap();
    let report = store.import_session(&dir).unwrap();
    assert!(report.bad.is_empty(), "{:?}", report.bad);

    let stored = store.captions(&sid).unwrap();
    let stored_walls: Vec<i64> = stored.iter().map(|l| expect_v2(l).wall_ms).collect();
    assert_eq!(stored_walls, walls.to_vec());
    assert_eq!(stored, file_lines(&captions));
    assert_eq!(
        store.list_sessions().unwrap()[0].last_wall_ms,
        Some(i64::MAX)
    );
}

#[test]
fn seq_beyond_i64_is_a_bad_line_not_a_panic() {
    let temp = TempDir::new().unwrap();
    let dir = bare_session(temp.path(), "5e90000000000000000000000000005e");
    let sid = session_id(&dir);

    let with_seq = |seq: u64, text: &str| {
        let mut value = caption(1, text).to_value();
        value["seq"] = json!(seq);
        format!("{value}\n")
    };
    let content = [
        with_seq(1, "bien"),
        with_seq(9_223_372_036_854_775_808, "pasa de i64"),
        with_seq(u64::MAX, "tope u64"),
        with_seq(4, "bien tres"),
    ]
    .concat();
    fs::write(dir.join("captions.jsonl"), content).unwrap();

    let mut store = Store::open_in_memory().unwrap();
    let report = store.import_session(&dir).unwrap();
    assert_eq!(report.bad.len(), 2, "{:?}", report.bad);
    assert_eq!(report.bad[0].line_no, 2);
    assert_eq!(report.bad[1].line_no, 3);
    assert!(
        report.bad[0].reason.contains("seq"),
        "{}",
        report.bad[0].reason
    );
    assert_eq!(report.lines_added, 2);

    let stored = store.captions(&sid).unwrap();
    assert_eq!(
        stored,
        vec![
            ParsedLine::V2(caption(1, "bien")),
            // `with_seq` only swaps the seq, so the frame stays the one from caption(1, ..).
            ParsedLine::V2(CaptionLine {
                seq: 4,
                ..caption(1, "bien tres")
            }),
        ]
    );
    assert_eq!(store.list_sessions().unwrap()[0].bad_lines, 2);
}

// ---------------------------------------------- 11. committed and summaries

#[test]
fn committed_lines_and_summaries_agree_with_the_files() {
    let dirs = synthetic_dirs();
    let temp = TempDir::new().unwrap();
    let mut store = Store::open(temp.path().join("index.sqlite")).unwrap();
    for (dir, result) in store.import_all(corpus_root()).unwrap() {
        result.unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    }

    let summaries = store.list_sessions().unwrap();
    assert_eq!(summaries.len(), dirs.len());
    let mut total_committed = 0;
    for dir in &dirs {
        let sid = session_id(dir);
        let manifest = Manifest::load(dir.join("manifest.json")).unwrap();
        let lines = file_lines(&dir.join("captions.jsonl"));
        let committed: Vec<ParsedLine> = lines
            .iter()
            .filter(|line| decision_of(line) == "committed")
            .cloned()
            .collect();
        total_committed += committed.len();

        assert_eq!(
            store.committed_lines(&sid).unwrap(),
            committed,
            "{}",
            dir.display()
        );

        let summary = summaries
            .iter()
            .find(|s| s.session_id == sid)
            .unwrap_or_else(|| panic!("no summary for {}", dir.display()));
        assert_eq!(summary.captions, lines.len() as u64, "{}", dir.display());
        assert_eq!(
            summary.committed,
            committed.len() as u64,
            "{}",
            dir.display()
        );
        assert!(summary.committed <= summary.captions);
        assert_eq!(summary.bad_lines, 0, "{}", dir.display());
        let last = lines
            .iter()
            .filter_map(|line| match line {
                ParsedLine::V2(l) => Some(l.wall_ms),
                ParsedLine::V1(_) => None,
            })
            .max();
        assert_eq!(summary.last_wall_ms, last, "{}", dir.display());
        assert_eq!(summary.started_wall_ms, Some(manifest.started_wall_ms));
        assert_eq!(summary.ended_wall_ms, manifest.ended_wall_ms);
        assert_eq!(summary.meeting_name, manifest.meeting_name);
    }
    assert!(total_committed > 0, "the corpus has committed lines");
}

// ------------------------------------------------ 12. same id, other content

#[test]
fn same_session_id_with_different_content_from_another_dir_is_a_conflict() {
    let temp = TempDir::new().unwrap();
    let a = temp.path().join("a");
    let b = temp.path().join("b");
    copy_to(&corpus_root().join("synthetic_basic"), &a);
    copy_to(&corpus_root().join("synthetic_basic"), &b);
    let sid = session_id(&a);
    assert_eq!(session_id(&b), sid);

    let mut changed = (caption(1000, "otra version").to_value().to_string() + "\n").into_bytes();
    changed.extend(fs::read(b.join("captions.jsonl")).unwrap());
    fs::write(b.join("captions.jsonl"), changed).unwrap();

    let mut store = Store::open(temp.path().join("index.sqlite")).unwrap();
    store.import_session(&a).unwrap();
    let before_lines = store.captions(&sid).unwrap();
    let before_sessions = store.list_sessions().unwrap();

    let err = store
        .import_session(&b)
        .expect_err("a different file under the same session id must conflict");
    assert!(
        matches!(err, StoreError::Conflict(_)),
        "unexpected error: {err}"
    );
    assert_eq!(store.captions(&sid).unwrap(), before_lines);
    assert_eq!(store.list_sessions().unwrap(), before_sessions);
}

// --------------------------------------------- 13. captions file removed

#[test]
fn a_removed_captions_file_is_an_error_and_keeps_the_rows() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("session");
    copy_to(&corpus_root().join("synthetic_basic"), &dir);
    let captions = dir.join("captions.jsonl");
    let sid = session_id(&dir);

    let mut store = Store::open_in_memory().unwrap();
    store.import_session(&dir).unwrap();
    let original = fs::read(&captions).unwrap();
    let before_lines = store.captions(&sid).unwrap();
    let before_sessions = store.list_sessions().unwrap();

    fs::remove_file(&captions).unwrap();
    let err = store
        .import_session(&dir)
        .expect_err("a missing captions file must fail the import");
    assert!(
        matches!(err, StoreError::Io { .. }),
        "unexpected error: {err}"
    );
    assert_eq!(store.captions(&sid).unwrap(), before_lines);
    assert_eq!(store.list_sessions().unwrap(), before_sessions);

    fs::write(&captions, &original).unwrap();
    let report = store.import_session(&dir).unwrap();
    assert_eq!(report.action, ImportAction::Unchanged);
    assert_eq!(store.captions(&sid).unwrap(), before_lines);
}

// -------------------------------------- 14. database failure rolls back import

#[test]
fn a_database_failure_during_import_rolls_back_and_the_next_import_keeps_every_line() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("index.sqlite");
    let dir = temp.path().join("session");
    copy_to(&corpus_root().join("synthetic_basic"), &dir);
    let captions = dir.join("captions.jsonl");
    let sid = session_id(&dir);

    let mut store = Store::open(&path).unwrap();
    store.import_session(&dir).unwrap();
    let before = store.captions(&sid).unwrap();
    let before_sessions = store.list_sessions().unwrap();

    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER boom BEFORE INSERT ON captions BEGIN SELECT RAISE(ABORT,'boom'); END;",
        )
        .unwrap();
    }
    const N: u64 = 3;
    let start = jsonl::read_file(&captions).unwrap().next_seq();
    let mut writer = CaptionWriter::open_append(&captions, start).unwrap();
    for i in 0..N {
        writer
            .write_line(&caption(start + i, &format!("nueva {i}")))
            .unwrap();
    }
    drop(writer);

    let err = store
        .import_session(&dir)
        .expect_err("a database failure during import must abort it");
    assert!(
        matches!(err, StoreError::Sqlite(_)),
        "unexpected error: {err}"
    );
    assert_eq!(store.captions(&sid).unwrap(), before);
    assert_eq!(store.list_sessions().unwrap(), before_sessions);

    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("DROP TRIGGER boom").unwrap();
    }
    let report = store.import_session(&dir).unwrap();
    assert_eq!(report.action, ImportAction::Appended);
    assert_eq!(report.lines_added, N as usize);
    assert!(report.bad.is_empty(), "{:?}", report.bad);

    let stored = store.captions(&sid).unwrap();
    assert_eq!(stored.len(), before.len() + N as usize);
    assert_eq!(stored, file_lines(&captions));
}

// ------------------------------------------------ 15. renamed captions file

#[test]
fn a_renamed_captions_file_with_the_same_bytes_is_rebuilt_not_duplicated() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("session");
    copy_to(&corpus_root().join("synthetic_basic"), &dir);
    let captions = dir.join("captions.jsonl");
    let renamed = dir.join("other.jsonl");
    let sid = session_id(&dir);

    let mut store = Store::open_in_memory().unwrap();
    store.import_session(&dir).unwrap();
    let before = store.captions(&sid).unwrap();

    fs::rename(&captions, &renamed).unwrap();
    let manifest_path = dir.join("manifest.json");
    let mut manifest = Manifest::load(&manifest_path).unwrap();
    manifest.captions.file = "other.jsonl".into();
    manifest.save_atomic(&manifest_path).unwrap();

    let report = store.import_session(&dir).unwrap();
    assert_eq!(
        report.action,
        ImportAction::Rebuilt(RebuildReason::Replaced)
    );
    assert_eq!(report.lines_added, before.len());
    let stored = store.captions(&sid).unwrap();
    assert_eq!(stored.len(), before.len());
    assert_eq!(stored, before);
    assert_eq!(stored, file_lines(&renamed));
    assert_eq!(store.list_sessions().unwrap().len(), 1);
}

// --------------------------------------------------- 16. moved corpus directory

#[test]
fn a_moved_corpus_with_changed_content_rebuilds_without_conflict() {
    let temp = TempDir::new().unwrap();
    let a = temp.path().join("a");
    let b = temp.path().join("b");
    copy_to(&corpus_root().join("synthetic_basic"), &a);
    let sid = session_id(&a);

    let mut store = Store::open(temp.path().join("index.sqlite")).unwrap();
    store.import_session(&a).unwrap();

    // Same length, different bytes: the first line changes.
    let captions_a = a.join("captions.jsonl");
    let original = fs::read_to_string(&captions_a).unwrap();
    let rewritten = original.replacen("Buenos días", "Buenas días", 1);
    assert_eq!(rewritten.len(), original.len());
    assert_ne!(rewritten, original);
    fs::write(&captions_a, &rewritten).unwrap();
    fs::rename(&a, &b).unwrap();

    let report = store
        .import_session(&b)
        .expect("a moved corpus must not conflict with its old location");
    assert_eq!(
        report.action,
        ImportAction::Rebuilt(RebuildReason::Replaced)
    );
    assert!(report.bad.is_empty(), "{:?}", report.bad);
    let captions_b = b.join("captions.jsonl");
    assert_eq!(store.captions(&sid).unwrap(), file_lines(&captions_b));

    let summaries = store.list_sessions().unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(
        summaries[0].dir,
        fs::canonicalize(&b).unwrap().display().to_string()
    );
}

#[test]
fn a_copy_with_other_content_conflicts_while_the_original_still_exists() {
    let temp = TempDir::new().unwrap();
    let a = temp.path().join("a");
    let b = temp.path().join("b");
    copy_to(&corpus_root().join("synthetic_basic"), &a);
    let sid = session_id(&a);

    let mut store = Store::open(temp.path().join("index.sqlite")).unwrap();
    store.import_session(&a).unwrap();
    let before_lines = store.captions(&sid).unwrap();

    copy_to(&a, &b);
    let captions_b = b.join("captions.jsonl");
    let original = fs::read_to_string(&captions_b).unwrap();
    let rewritten = original.replacen("Buenos días", "Buenas días", 1);
    assert_eq!(rewritten.len(), original.len());
    fs::write(&captions_b, &rewritten).unwrap();

    let err = store
        .import_session(&b)
        .expect_err("a different copy under the same id must conflict while the original exists");
    assert!(
        matches!(err, StoreError::Conflict(_)),
        "unexpected error: {err}"
    );
    assert_eq!(store.captions(&sid).unwrap(), before_lines);
}

// ------------------------------------------------------ 17. BOM mid-file

#[test]
fn a_mid_file_bom_is_a_bad_line_in_incremental_and_full_import_alike() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("session");
    copy_to(&corpus_root().join("synthetic_basic"), &dir);
    let captions = dir.join("captions.jsonl");
    let sid = session_id(&dir);

    let mut store = Store::open_in_memory().unwrap();
    store.import_session(&dir).unwrap();
    let original_lines = fs::read(&captions)
        .unwrap()
        .iter()
        .filter(|b| **b == b'\n')
        .count();

    let mut bom_line = b"\xEF\xBB\xBF".to_vec();
    bom_line.extend_from_slice(caption(900, "tras BOM").to_value().to_string().as_bytes());
    bom_line.push(b'\n');
    append_bytes(&captions, &bom_line);
    append_bytes(
        &captions,
        (caption(901, "buena").to_value().to_string() + "\n").as_bytes(),
    );

    let incremental = store.import_session(&dir).unwrap();
    assert_eq!(incremental.action, ImportAction::Appended);
    assert_eq!(incremental.lines_added, 1);
    assert_eq!(incremental.bad.len(), 1, "{:?}", incremental.bad);
    assert_eq!(incremental.bad[0].line_no, original_lines + 1);

    let mut fresh = Store::open_in_memory().unwrap();
    let full = fresh.import_session(&dir).unwrap();
    assert_eq!(full.action, ImportAction::Created);
    assert_eq!(full.bad, incremental.bad);
    assert_eq!(fresh.captions(&sid).unwrap(), store.captions(&sid).unwrap());
}

// ------------------------------------------------ 18. session without a start

#[test]
fn a_session_without_a_start_time_sorts_after_the_dated_ones() {
    // `started_wall_ms` is never NULL through `import_session` (the manifest
    // field is an i64), so the NULL state is written directly with SQL here.
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("index.sqlite");
    let mut store = Store::open(&path).unwrap();

    let make = |name: &str, sid: &str, started: i64| -> PathBuf {
        let dir = temp.path().join(name);
        copy_to(&corpus_root().join("synthetic_basic"), &dir);
        let mut manifest = Manifest::load(dir.join("manifest.json")).unwrap();
        manifest.session_id = sid.into();
        manifest.started_wall_ms = started;
        manifest.save_atomic(dir.join("manifest.json")).unwrap();
        dir
    };
    // Ids chosen so that id order differs from start order.
    let early_id = "c0000000000000000000000000000001";
    let later_id = "a0000000000000000000000000000002";
    let undated_id = "b0000000000000000000000000000003";
    let early = make("early", early_id, 1_600_000_000_000);
    let later = make("later", later_id, 1_700_000_000_000);
    let undated = make("undated", undated_id, 1_800_000_000_000);
    for dir in [&early, &later, &undated] {
        store.import_session(dir).unwrap();
    }

    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        let changed = conn
            .execute(
                "UPDATE sessions SET started_wall_ms = NULL WHERE session_id = ?1",
                [undated_id],
            )
            .unwrap();
        assert_eq!(changed, 1);
    }

    let order: Vec<String> = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .map(|s| s.session_id)
        .collect();
    assert_eq!(order, vec![early_id, later_id, undated_id]);
}

// ------------------------------------ 19. two threads opening a new database

#[test]
fn two_threads_opening_the_same_new_database_both_succeed() {
    let temp = TempDir::new().unwrap();
    for round in 0..20 {
        let path = temp.path().join(format!("index-{round}.sqlite"));
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || -> Result<i64, StoreError> {
                    barrier.wait();
                    let store = Store::open(&path)?;
                    store.schema_version()
                })
            })
            .collect();
        for handle in handles {
            let version = handle
                .join()
                .expect("thread panicked")
                .unwrap_or_else(|e| panic!("round {round}: {e}"));
            assert_eq!(version, 4, "round {round}");
        }
    }
}

// ------------------------------------------------------ 20. indexed runs

fn run_row(session_id: &str, run_id: &str) -> RunRow {
    RunRow {
        run_id: run_id.into(),
        session_id: session_id.into(),
        dir: format!("runs/{run_id}"),
        config_json: r#"{"debounce_s":1.2,"flush":"auto"}"#.into(),
        tool_version: "0.1.0".into(),
        input_bytes: 1234,
        input_fnv: u64::MAX - 7,
        ended_cleanly: true,
        synthetic_flush: false,
        content_div: 0,
        timing_div: 3,
        duration_ms: None,
        report_json: "{}".into(),
    }
}

fn segment_rows() -> Vec<SegmentRow> {
    vec![
        SegmentRow {
            speaker: "Ana".into(),
            speaker_raw: Some("Ana Pérez (raw)".into()),
            text: "hola 😀\ncon salto".into(),
            first_mono_ms: Some(1000.5),
            last_mono_ms: Some(2000.0),
            committed_mono_ms: Some(2500.25),
            committed_wall_ms: Some(1_700_000_002_500),
        },
        SegmentRow {
            speaker: String::new(),
            speaker_raw: None,
            text: "sin hablante".into(),
            first_mono_ms: None,
            last_mono_ms: None,
            committed_mono_ms: None,
            committed_wall_ms: None,
        },
    ]
}

#[test]
fn index_run_is_idempotent_and_round_trips_its_segments() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("session");
    copy_to(&corpus_root().join("synthetic_basic"), &dir);
    let sid = session_id(&dir);
    let mut store = Store::open(temp.path().join("index.sqlite")).unwrap();
    store.import_session(&dir).unwrap();

    let row = run_row(&sid, "r0000000000000001");
    let segments = segment_rows();
    assert!(store.index_run(&row, &segments).unwrap());
    assert!(!store.index_run(&row, &segments).unwrap());
    // A second record with other segments changes nothing.
    assert!(!store.index_run(&row, &[]).unwrap());

    assert_eq!(store.runs(&sid).unwrap(), vec![row.run_id.clone()]);
    assert_eq!(store.run_segments(&row.run_id).unwrap(), segments);
    assert!(store.run_segments("r-unknown").unwrap().is_empty());
}

#[test]
fn runs_list_only_the_runs_of_their_session_oldest_first() {
    let temp = TempDir::new().unwrap();
    let mut store = Store::open_in_memory().unwrap();
    let mut sids = Vec::new();
    for name in ["synthetic_basic", "synthetic_guests"] {
        let dir = temp.path().join(name);
        copy_to(&corpus_root().join(name), &dir);
        sids.push(session_id(&dir));
        store.import_session(&dir).unwrap();
    }
    for run_id in ["r00000000000000a1", "r00000000000000a2"] {
        store
            .index_run(&run_row(&sids[0], run_id), &segment_rows())
            .unwrap();
    }
    store
        .index_run(&run_row(&sids[1], "r00000000000000b1"), &[])
        .unwrap();

    assert_eq!(
        store.runs(&sids[0]).unwrap(),
        vec![
            "r00000000000000a1".to_string(),
            "r00000000000000a2".to_string()
        ]
    );
    assert_eq!(
        store.runs(&sids[1]).unwrap(),
        vec!["r00000000000000b1".to_string()]
    );
    assert!(store.runs("not-imported").unwrap().is_empty());
}

#[test]
fn deleting_the_session_cascades_to_its_runs_and_segments() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("index.sqlite");
    let dir = temp.path().join("session");
    copy_to(&corpus_root().join("synthetic_basic"), &dir);
    let sid = session_id(&dir);
    let row = run_row(&sid, "r0000000000000002");
    {
        let mut store = Store::open(&path).unwrap();
        store.import_session(&dir).unwrap();
        assert!(store.index_run(&row, &segment_rows()).unwrap());
    }

    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        let deleted = conn
            .execute("DELETE FROM sessions WHERE session_id = ?1", [&sid])
            .unwrap();
        assert_eq!(deleted, 1);
        let counts: (i64, i64) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM runs), (SELECT COUNT(*) FROM run_segments)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(counts, (0, 0));
    }

    let store = Store::open(&path).unwrap();
    assert!(store.runs(&sid).unwrap().is_empty());
    assert!(store.run_segments(&row.run_id).unwrap().is_empty());
}

#[test]
fn index_run_for_an_unimported_session_is_an_error() {
    let mut store = Store::open_in_memory().unwrap();
    let row = run_row("0123456789abcdef0123456789abcdef", "r0000000000000003");
    let err = store
        .index_run(&row, &segment_rows())
        .expect_err("indexing a run of an unknown session must fail");
    assert!(
        matches!(err, StoreError::Format(_)),
        "unexpected error: {err}"
    );
    assert!(store.runs(&row.session_id).unwrap().is_empty());
    assert!(store.run_segments(&row.run_id).unwrap().is_empty());
}

// ------------------------------------------------- ASR runs and fusions (v3)

fn asr_row(session_id: &str, asr_id: &str) -> AsrRunRow {
    AsrRunRow {
        asr_id: asr_id.to_string(),
        session_id: session_id.to_string(),
        dir: "/data/s/asr/abc".to_string(),
        engine: "whisper.cpp".to_string(),
        model: "whisper-base".to_string(),
        segments: 12,
        meta_json: "{}".to_string(),
    }
}

fn fusion_row(session_id: &str, fuse_id: &str) -> FusionRow {
    FusionRow {
        fuse_id: fuse_id.to_string(),
        session_id: session_id.to_string(),
        run_id: "r0000000000000001".to_string(),
        asr_id: "abc".to_string(),
        dir: "/data/s/fused/f1".to_string(),
        config_json: "{}".to_string(),
        merged: 2,
        caption_only: 1,
        asr_only: 1,
        edits_applied: 3,
        lag_median_ms: Some(1234.5),
        report_json: "{}".to_string(),
    }
}

fn fused_rows() -> Vec<FusedRow> {
    (0..3)
        .map(|n| FusedRow {
            start_ms: n as f64 * 1000.0,
            end_ms: n as f64 * 1000.0 + 800.0,
            speaker: if n == 1 { "Luis".into() } else { "Ana".into() },
            speaker_inferred: n == 2,
            ch: "system".to_string(),
            source: ["merged", "captions", "asr"][n].to_string(),
            text: format!("frase {n} con \"comillas\" y ñ"),
            caption_text: (n < 2).then(|| format!("caption {n}")),
            asr_text: (n != 1).then(|| format!("asr {n}")),
        })
        .collect()
}

#[test]
fn asr_runs_and_fusions_are_indexed_idempotently_and_round_trip() {
    let temp = TempDir::new().unwrap();
    let dir = temp.path().join("session");
    copy_to(&corpus_root().join("synthetic_basic"), &dir);
    let sid = session_id(&dir);
    let mut store = Store::open(temp.path().join("index.sqlite")).unwrap();
    store.import_session(&dir).unwrap();

    assert!(store.index_asr_run(&asr_row(&sid, "abc")).unwrap());
    assert!(!store.index_asr_run(&asr_row(&sid, "abc")).unwrap());
    assert!(store.index_asr_run(&asr_row(&sid, "def")).unwrap());
    assert_eq!(store.asr_runs(&sid).unwrap(), ["abc", "def"]);

    let row = fusion_row(&sid, "f0000000000000001");
    let segments = fused_rows();
    assert!(store.index_fusion(&row, &segments).unwrap());
    assert!(!store.index_fusion(&row, &segments).unwrap());
    assert_eq!(
        store.fusions(&sid).unwrap(),
        std::slice::from_ref(&row.fuse_id)
    );
    assert_eq!(store.fused_segments(&row.fuse_id).unwrap(), segments);
    assert!(store.fused_segments("f-unknown").unwrap().is_empty());

    // The same id redone with other content replaces what was indexed.
    let redone = FusionRow {
        merged: 9,
        edits_applied: 7,
        ..row.clone()
    };
    assert!(!store.index_fusion(&redone, &segments[..1]).unwrap());
    assert_eq!(store.fused_segments(&row.fuse_id).unwrap(), segments[..1]);
    let back = store.fusion(&row.fuse_id).unwrap().unwrap();
    assert_eq!((back.merged, back.edits_applied), (9, 7));
    assert_eq!(store.fusions(&sid).unwrap().len(), 1);

    let asr = store.asr_run(&sid, "abc").unwrap().unwrap();
    assert_eq!((asr.engine.as_str(), asr.segments), ("whisper.cpp", 12));
    let changed = AsrRunRow {
        segments: 99,
        ..asr_row(&sid, "abc")
    };
    assert!(!store.index_asr_run(&changed).unwrap());
    assert_eq!(store.asr_run(&sid, "abc").unwrap().unwrap().segments, 99);
    assert!(store.asr_run(&sid, "nope").unwrap().is_none());
}

#[test]
fn asr_and_fusion_rows_need_an_imported_session_and_go_with_it() {
    let mut store = Store::open_in_memory().unwrap();
    let ghost = "0123456789abcdef0123456789abcdef";
    assert!(matches!(
        store.index_asr_run(&asr_row(ghost, "abc")),
        Err(StoreError::Format(_))
    ));
    assert!(matches!(
        store.index_fusion(&fusion_row(ghost, "f1"), &[]),
        Err(StoreError::Format(_))
    ));
    assert!(store.asr_runs(ghost).unwrap().is_empty());
    assert!(store.fusions(ghost).unwrap().is_empty());
}

#[test]
fn a_database_at_version_two_upgrades_in_place_keeping_its_data() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("index.sqlite");
    let dir = temp.path().join("session");
    copy_to(&corpus_root().join("synthetic_basic"), &dir);
    let sid = session_id(&dir);
    {
        let mut store = Store::open(&path).unwrap();
        store.import_session(&dir).unwrap();
        store
            .index_run(&run_row(&sid, "r0000000000000009"), &segment_rows())
            .unwrap();
    }
    // Pretend the file was written by the previous build.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP TABLE series; DROP TABLE fused_segments; DROP TABLE fusions; DROP TABLE asr_runs; PRAGMA user_version=2;",
        )
        .unwrap();
    }
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 4);
    assert_eq!(store.runs(&sid).unwrap(), ["r0000000000000009"]);
    assert!(store.index_asr_run(&asr_row(&sid, "abc")).unwrap());
}

#[test]
fn the_same_asr_id_can_belong_to_two_sessions_and_everything_goes_with_its_session() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("index.sqlite");
    let (a, b) = (temp.path().join("a"), temp.path().join("b"));
    copy_to(&corpus_root().join("synthetic_basic"), &a);
    copy_to(&corpus_root().join("synthetic_guests"), &b);
    let (sa, sb) = (session_id(&a), session_id(&b));
    let mut store = Store::open(&path).unwrap();
    store.import_session(&a).unwrap();
    store.import_session(&b).unwrap();
    // Copies of a recording share the id of its audio.
    assert!(store.index_asr_run(&asr_row(&sa, "same")).unwrap());
    assert!(store.index_asr_run(&asr_row(&sb, "same")).unwrap());
    assert_eq!(store.asr_runs(&sa).unwrap(), ["same"]);
    assert_eq!(store.asr_runs(&sb).unwrap(), ["same"]);
    store
        .index_fusion(&fusion_row(&sa, "f1"), &fused_rows())
        .unwrap();

    drop(store);
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        conn.execute("DELETE FROM sessions WHERE session_id = ?1", [&sa])
            .unwrap();
        let left = |table: &str| -> i64 {
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(
            (left("asr_runs"), left("fusions"), left("fused_segments")),
            (1, 0, 0)
        );
    }
}

#[test]
fn a_database_at_version_three_gains_the_series_table_keeping_its_data() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("index.sqlite");
    let dir = temp.path().join("session");
    copy_to(&corpus_root().join("synthetic_basic"), &dir);
    let sid = session_id(&dir);
    {
        let mut store = Store::open(&path).unwrap();
        store.import_session(&dir).unwrap();
    }
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("DROP TABLE series; PRAGMA user_version=3;")
            .unwrap();
    }
    let mut store = Store::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 4);
    assert_eq!(store.resolve_session(&sid[..8]).unwrap().len(), 1);
    assert_eq!(store.series_of(&sid).unwrap(), None);
    assert!(
        store
            .assign_series(&sid, "daily", SeriesOrigin::Auto)
            .unwrap()
    );
    assert_eq!(
        store.series_of(&sid).unwrap(),
        Some(("daily".to_string(), SeriesOrigin::Auto))
    );
}

#[test]
fn a_manual_series_is_never_overridden_and_series_list_in_order() {
    let temp = TempDir::new().unwrap();
    let mut store = Store::open_in_memory().unwrap();
    let (a, b) = (temp.path().join("a"), temp.path().join("b"));
    copy_to(&corpus_root().join("synthetic_basic"), &a);
    copy_to(&corpus_root().join("synthetic_guests"), &b);
    let (sa, sb) = (session_id(&a), session_id(&b));
    store.import_session(&a).unwrap();
    store.import_session(&b).unwrap();
    assert!(store.assign_series(&sa, "x", SeriesOrigin::Auto).unwrap());
    assert!(store.assign_series(&sb, "x", SeriesOrigin::Rule).unwrap());
    assert!(store.assign_series(&sa, "y", SeriesOrigin::Manual).unwrap());
    // A later automatic pass does not take the manual choice back.
    assert!(!store.assign_series(&sa, "x", SeriesOrigin::Auto).unwrap());
    assert!(!store.assign_series(&sa, "x", SeriesOrigin::Rule).unwrap());
    assert_eq!(store.series_of(&sa).unwrap().unwrap().0, "y");

    let list = store.list_series().unwrap();
    let keys: Vec<(&str, u64)> = list.iter().map(|s| (s.key.as_str(), s.sessions)).collect();
    assert_eq!(keys, [("x", 1), ("y", 1)]);
    let members = store.series_sessions("x").unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].session_id, sb);
    assert_eq!(members[0].origin, SeriesOrigin::Rule);
    assert!(matches!(
        store.assign_series("ghost", "x", SeriesOrigin::Manual),
        Err(StoreError::Format(_))
    ));
}
