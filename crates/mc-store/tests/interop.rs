//! Interop with the Python writers. The files under `tests/data` were produced
//! by the original Python app: `captions_v2.jsonl` and `raw_uia_v1.jsonl` by
//! its `RawRecorder`, the manifests by its `CorpusSession`. They must read back here without changes.

use std::fs;
use std::path::{Path, PathBuf};

use mc_core::pytext::py_round3;
use mc_core::{Clock, Decision, FakeClock, FrameRef, Record, ScriptedClock, Stamp};
use mc_store::StoreError;
use mc_store::jsonl::{self, CaptionLine, CaptionWriter, LegacyLine, ParsedLine, RESERVED_KEYS};
use mc_store::manifest::{MANIFEST_SCHEMA, Manifest};
use serde_json::{Map, Value, json};
use tempfile::TempDir;

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

/// The non-blank lines of a file, decoded with serde_json (the reference parse).
fn raw_values(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap()
        .split('\n')
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn expect_v2(parsed: &ParsedLine) -> &CaptionLine {
    match parsed {
        ParsedLine::V2(line) => line,
        other => panic!("expected a v2 line, got {other:?}"),
    }
}

fn expect_v1(parsed: &ParsedLine) -> &LegacyLine {
    match parsed {
        ParsedLine::V1(line) => line,
        other => panic!("expected a v1 line, got {other:?}"),
    }
}

fn temp_file(dir: &TempDir, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, bytes).unwrap();
    path
}

/// A valid v2 line as Python writes it, built from JSON.
fn v2_value(seq: u64, text: &str) -> Value {
    json!({
        "v": 2,
        "seq": seq,
        "frame": null,
        "t": 1700000000.5,
        "wall_ms": 1700000000500i64,
        "mono_ms": 500.0,
        "read_ms": null,
        "speaker": "Ana",
        "text": text,
        "decision": "new_open",
    })
}

fn v2_json(seq: u64, text: &str) -> String {
    v2_value(seq, text).to_string()
}

fn caption(seq: u64, text: &str) -> CaptionLine {
    CaptionLine {
        seq,
        frame: None,
        t: 1700000000.5,
        wall_ms: 1_700_000_000_500,
        mono_ms: 500.0,
        read_ms: None,
        speaker: "Ana".into(),
        text: text.into(),
        decision: "new_open".into(),
        speaker_raw: None,
        extra: Map::new(),
    }
}

/// Stands in for the clock in tests that must not ask it.
fn never_asked() -> Stamp {
    panic!("the clock must not be asked when the record has a stamp")
}

fn record(stamp: Option<Stamp>) -> Record {
    Record {
        speaker: "Ana".into(),
        text: Some("hola".into()),
        decision: Decision::NewOpen,
        speaker_raw: None,
        stamp,
        frame: FrameRef::InFrame(7),
        read_ms: Some(12.34567),
        span: None,
    }
}

// ---------------------------------------------------------------- (i) captions v2

#[test]
fn captions_v2_fixture_reads_every_line_as_v2() {
    let path = data("captions_v2.jsonl");
    let report = jsonl::read_file(&path).unwrap();
    let originals = raw_values(&path);

    assert!(report.bad.is_empty(), "bad lines: {:?}", report.bad);
    assert!(!report.truncated_tail);
    assert_eq!(report.lines.len(), 40);
    assert_eq!(originals.len(), 40);

    for ((line_no, parsed), original) in report.lines.iter().zip(&originals) {
        assert_eq!(original["v"], json!(2), "line {line_no}");
        let line = expect_v2(parsed);
        assert_eq!(
            line.seq,
            original["seq"].as_u64().unwrap(),
            "line {line_no}"
        );
        assert_eq!(line.frame, original["frame"].as_u64(), "line {line_no}");
        assert_eq!(line.t, original["t"].as_f64().unwrap(), "line {line_no}");
        assert_eq!(line.wall_ms, original["wall_ms"].as_i64().unwrap());
        assert_eq!(line.mono_ms, original["mono_ms"].as_f64().unwrap());
        assert_eq!(line.read_ms, original["read_ms"].as_f64());
        assert_eq!(line.speaker, original["speaker"].as_str().unwrap());
        assert_eq!(line.text, original["text"].as_str().unwrap());
        assert_eq!(line.decision, original["decision"].as_str().unwrap());
        assert_eq!(
            line.speaker_raw.as_deref(),
            original.get("speaker_raw").and_then(Value::as_str)
        );
        let expected_extra: Map<String, Value> = original
            .as_object()
            .unwrap()
            .iter()
            .filter(|(k, _)| !RESERVED_KEYS.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        assert_eq!(line.extra, expected_extra, "line {line_no}");
    }
}

#[test]
fn captions_v2_fixture_covers_the_combinations() {
    let report = jsonl::read_file(data("captions_v2.jsonl")).unwrap();
    let lines: Vec<&CaptionLine> = report.lines.iter().map(|(_, p)| expect_v2(p)).collect();

    assert!(lines.iter().any(|l| l.speaker_raw.is_some()));
    assert!(lines.iter().any(|l| l.speaker_raw.is_none()));
    assert!(lines.iter().any(|l| l.frame.is_some()));
    assert!(lines.iter().any(|l| l.frame.is_none()));
    assert!(lines.iter().any(|l| l.read_ms.is_some()));
    assert!(lines.iter().any(|l| l.read_ms.is_none()));
    assert!(lines.iter().any(|l| l.extra.contains_key("first_mono_ms")));
    assert!(lines.iter().any(|l| l.extra.contains_key("note")));
    assert!(lines.iter().any(|l| l.speaker.is_empty()));
    assert!(lines.iter().any(|l| l.text.is_empty()));
    assert!(lines.iter().any(|l| l.text.contains('"')));
    assert!(lines.iter().any(|l| l.text.contains('\n')));
    assert!(lines.iter().any(|l| l.text.contains("🎉")));
    assert!(lines.iter().any(|l| l.text.contains("漢字")));
}

// ---------------------------------------------------------------- (ii) round trip

#[test]
fn captions_v2_to_value_matches_each_original_line() {
    let path = data("captions_v2.jsonl");
    let report = jsonl::read_file(&path).unwrap();
    let originals = raw_values(&path);

    for ((line_no, parsed), original) in report.lines.iter().zip(&originals) {
        let line = expect_v2(parsed);
        assert_eq!(line.to_value(), *original, "line {line_no}");
    }
}

#[test]
fn captions_v2_written_again_reads_back_the_same_lines() {
    let path = data("captions_v2.jsonl");
    let report = jsonl::read_file(&path).unwrap();
    let originals = raw_values(&path);

    let dir = TempDir::new().unwrap();
    let out = dir.path().join("captions.jsonl");
    let mut writer = CaptionWriter::create_new(&out).unwrap();
    for (_, parsed) in &report.lines {
        writer.write_line(expect_v2(parsed)).unwrap();
    }
    drop(writer);

    assert_eq!(raw_values(&out), originals);
}

// ---------------------------------------------------------------- (iii) raw_uia v1

#[test]
fn raw_uia_v1_fixture_reads_every_line_as_legacy() {
    let path = data("raw_uia_v1.jsonl");
    let report = jsonl::read_file(&path).unwrap();
    let originals = raw_values(&path);

    assert!(report.bad.is_empty(), "bad lines: {:?}", report.bad);
    assert!(!report.truncated_tail);
    assert_eq!(report.lines.len(), 8);

    for ((line_no, parsed), original) in report.lines.iter().zip(&originals) {
        let line = expect_v1(parsed);
        assert_eq!(line.t, original["t"].as_f64().unwrap(), "line {line_no}");
        assert_eq!(line.speaker, original["speaker"].as_str().unwrap());
        assert_eq!(line.text, original["text"].as_str().unwrap());
        assert_eq!(line.decision, original["decision"].as_str().unwrap());
    }
}

#[test]
fn raw_uia_v1_keeps_excluded_speaker_as_a_string() {
    let report = jsonl::read_file(data("raw_uia_v1.jsonl")).unwrap();
    let excluded: Vec<&LegacyLine> = report
        .lines
        .iter()
        .map(|(_, p)| expect_v1(p))
        .filter(|l| l.decision == "excluded_speaker")
        .collect();

    assert_eq!(excluded.len(), 1);
    assert_eq!(excluded[0].speaker, "Invitado");
    assert_eq!(excluded[0].text, "comillas \" y \\ y salto\n");
}

// ---------------------------------------------------------------- (iv) tolerance

#[test]
fn truncated_last_line_is_flagged_and_earlier_lines_survive() {
    let full_bytes = fs::read(data("captions_v2.jsonl")).unwrap();
    let full = jsonl::read_bytes(&full_bytes);

    // Cut the final line in half: no newline, not valid JSON.
    let body = &full_bytes[..full_bytes.len() - 1];
    let last_start = body.iter().rposition(|b| *b == b'\n').unwrap() + 1;
    let cut = last_start + (body.len() - last_start) / 2;

    let dir = TempDir::new().unwrap();
    let file = temp_file(&dir, "captions.jsonl", &full_bytes[..cut]);
    let report = jsonl::read_file(&file).unwrap();

    assert!(report.truncated_tail);
    assert!(report.bad.is_empty(), "bad lines: {:?}", report.bad);
    assert_eq!(report.lines, full.lines[..39].to_vec());
}

#[test]
fn complete_last_line_without_newline_is_not_truncated() {
    let full_bytes = fs::read(data("captions_v2.jsonl")).unwrap();
    let report = jsonl::read_bytes(&full_bytes[..full_bytes.len() - 1]);

    assert!(!report.truncated_tail);
    assert!(report.bad.is_empty());
    assert_eq!(report.lines.len(), 40);
}

#[test]
fn truncation_inside_a_multibyte_character_is_a_truncated_tail() {
    let first = v2_json(1, "a");
    let second = v2_json(2, "ñ");
    let cut = second.find('ñ').unwrap() + 1; // after the first byte of ñ
    let mut bytes = format!("{first}\n").into_bytes();
    bytes.extend_from_slice(&second.as_bytes()[..cut]);

    let report = jsonl::read_bytes(&bytes);
    assert!(report.truncated_tail);
    assert!(report.bad.is_empty());
    assert_eq!(report.lines.len(), 1);
}

#[test]
fn only_a_truncated_line_gives_no_lines() {
    let report = jsonl::read_bytes(b"{\"v\":2,\"se");
    assert!(report.truncated_tail);
    assert!(report.bad.is_empty());
    assert!(report.lines.is_empty());
}

#[test]
fn corrupt_middle_line_is_bad_and_the_rest_is_read() {
    let bytes = format!("{}\n{{not json\n{}\n", v2_json(1, "a"), v2_json(3, "c"));
    let report = jsonl::read_bytes(bytes.as_bytes());

    assert!(!report.truncated_tail);
    assert_eq!(report.bad.len(), 1);
    assert_eq!(report.bad[0].line_no, 2);
    assert!(!report.bad[0].reason.is_empty());
    let numbers: Vec<usize> = report.lines.iter().map(|(n, _)| *n).collect();
    assert_eq!(numbers, vec![1, 3]);
}

#[test]
fn corrupt_last_line_with_newline_is_bad_not_truncated() {
    let bytes = format!("{}\n{{not json\n", v2_json(1, "a"));
    let report = jsonl::read_bytes(bytes.as_bytes());

    assert!(!report.truncated_tail);
    assert_eq!(report.bad.len(), 1);
    assert_eq!(report.bad[0].line_no, 2);
    assert_eq!(report.lines.len(), 1);
}

#[test]
fn blank_lines_are_ignored_but_still_counted() {
    let bytes = format!("\n{}\n   \n\n{}\n", v2_json(1, "a"), v2_json(2, "b"));
    let report = jsonl::read_bytes(bytes.as_bytes());

    assert!(report.bad.is_empty());
    let numbers: Vec<usize> = report.lines.iter().map(|(n, _)| *n).collect();
    assert_eq!(numbers, vec![2, 5]);
}

#[test]
fn crlf_lines_read_like_lf_lines() {
    let lf = format!("{}\n{}\n", v2_json(1, "a"), v2_json(2, "ñ"));
    let crlf = lf.replace('\n', "\r\n");

    let from_lf = jsonl::read_bytes(lf.as_bytes());
    let from_crlf = jsonl::read_bytes(crlf.as_bytes());

    assert!(from_crlf.bad.is_empty());
    assert_eq!(from_crlf.lines, from_lf.lines);
    assert_eq!(from_crlf.lines.len(), 2);
}

#[test]
fn invalid_utf8_line_is_bad() {
    let mut bytes = v2_json(1, "a").into_bytes();
    bytes.push(b'\n');
    bytes.extend_from_slice(b"{\"v\":2,\"text\":\"\xff\"}\n");
    bytes.extend_from_slice(v2_json(3, "c").as_bytes());
    bytes.push(b'\n');

    let report = jsonl::read_bytes(&bytes);
    assert_eq!(report.bad.len(), 1);
    assert_eq!(report.bad[0].line_no, 2);
    assert!(
        report.bad[0].reason.contains("UTF-8"),
        "{}",
        report.bad[0].reason
    );
    let numbers: Vec<usize> = report.lines.iter().map(|(n, _)| *n).collect();
    assert_eq!(numbers, vec![1, 3]);
}

#[test]
fn unknown_schema_version_is_bad_and_names_the_version() {
    let bytes = b"{\"v\":3,\"seq\":1}\n";
    let report = jsonl::read_bytes(bytes);

    assert_eq!(report.bad.len(), 1);
    let reason = &report.bad[0].reason;
    assert!(reason.contains("version"), "{reason}");
    assert!(reason.contains('3'), "{reason}");
}

#[test]
fn objects_missing_required_keys_are_bad() {
    let mut no_mono = v2_value(1, "a");
    no_mono.as_object_mut().unwrap().remove("mono_ms");
    let no_decision_v1 = "{\"t\":1.0,\"speaker\":\"A\",\"text\":\"x\"}";
    let no_t_v1 = "{\"speaker\":\"A\",\"text\":\"x\",\"decision\":\"grow\"}";

    let bytes = format!("{no_mono}\n");
    let report = jsonl::read_bytes(bytes.as_bytes());
    assert_eq!(report.bad.len(), 1);
    assert!(
        report.bad[0].reason.contains("mono_ms"),
        "{}",
        report.bad[0].reason
    );

    let report = jsonl::read_bytes(format!("{no_decision_v1}\n").as_bytes());
    assert_eq!(report.bad.len(), 1);
    assert!(
        report.bad[0].reason.contains("decision"),
        "{}",
        report.bad[0].reason
    );

    let report = jsonl::read_bytes(format!("{no_t_v1}\n").as_bytes());
    assert_eq!(report.bad.len(), 1);
    assert!(
        report.bad[0].reason.contains("\"t\""),
        "{}",
        report.bad[0].reason
    );
}

#[test]
fn lines_that_are_not_objects_are_bad() {
    let bytes = b"[1,2]\n\"texto\"\n42\nnull\n";
    let report = jsonl::read_bytes(bytes);

    assert!(report.lines.is_empty());
    let numbers: Vec<usize> = report.bad.iter().map(|b| b.line_no).collect();
    assert_eq!(numbers, vec![1, 2, 3, 4]);
    assert!(
        report
            .bad
            .iter()
            .all(|b| b.reason.contains("not a JSON object"))
    );
}

#[test]
fn empty_input_gives_an_empty_report() {
    let report = jsonl::read_bytes(b"");
    assert!(report.lines.is_empty());
    assert!(report.bad.is_empty());
    assert!(!report.truncated_tail);
}

// ---------------------------------------------------------------- (v) reserved keys

#[test]
fn reserved_keys_in_extra_never_overwrite_schema_fields() {
    let mut extra = Map::new();
    extra.insert("v".into(), json!(99));
    extra.insert("seq".into(), json!(0));
    extra.insert("mono_ms".into(), json!(-1));
    extra.insert("ok".into(), json!(5));
    let line = CaptionLine {
        seq: 4,
        frame: None,
        t: 1.5,
        wall_ms: 1500,
        mono_ms: 1.5,
        read_ms: None,
        speaker: "A".into(),
        text: "x".into(),
        decision: "grow".into(),
        speaker_raw: None,
        extra,
    };

    let value = line.to_value();
    assert_eq!(value["v"], json!(2));
    assert_eq!(value["seq"], json!(4));
    assert_eq!(value["mono_ms"], json!(1.5));
    assert_eq!(value["ok"], json!(5));
    assert!(value.get("speaker_raw").is_none());
}

#[test]
fn unreserved_extra_keys_are_kept_when_reading() {
    let value = json!({
        "v": 2, "seq": 1, "frame": 3, "t": 1.0, "wall_ms": 1000, "mono_ms": 1.0,
        "read_ms": 2.5, "speaker": "A", "text": "x", "decision": "committed",
        "first_mono_ms": 0.5, "last_mono_ms": 1.0, "zz": "y",
    });
    let parsed = jsonl::parse_value(&value).unwrap();
    let line = match parsed {
        ParsedLine::V2(line) => line,
        other => panic!("{other:?}"),
    };

    assert_eq!(line.frame, Some(3));
    assert_eq!(line.read_ms, Some(2.5));
    assert_eq!(line.extra.len(), 3);
    assert_eq!(line.extra["first_mono_ms"], json!(0.5));
    assert_eq!(line.extra["zz"], json!("y"));
    assert!(!line.extra.contains_key("text"));
    assert_eq!(line.to_value(), value);
}

// ---------------------------------------------------------------- (vi) from_record

#[test]
fn from_record_uses_the_record_stamp_without_asking_the_clock() {
    let clock = ScriptedClock::new(Vec::<Stamp>::new());
    let stamped = record(Some(Stamp {
        wall_ms: 1_700_000_000_123,
        mono_ms: 1234.56789,
    }));

    let line = CaptionLine::from_record(1, &stamped, || clock.now());

    assert_eq!(clock.calls(), 0);
    assert_eq!(line.wall_ms, 1_700_000_000_123);
    assert_eq!(line.mono_ms, py_round3(1234.56789));
    assert_eq!(line.mono_ms, 1234.568);
}

#[test]
fn from_record_asks_the_clock_only_when_the_stamp_is_missing() {
    let clock = ScriptedClock::new([Stamp {
        wall_ms: 1_700_000_000_500,
        mono_ms: 500.25,
    }]);
    let unstamped = record(None);

    let line = CaptionLine::from_record(2, &unstamped, || clock.now());

    assert_eq!(clock.calls(), 1);
    assert_eq!(clock.remaining(), 0);
    assert_eq!(line.wall_ms, 1_700_000_000_500);
    assert_eq!(line.mono_ms, 500.25);
    assert_eq!(line.t, 1_700_000_000.5);
}

#[test]
fn from_record_t_is_wall_seconds_rounded_to_three_decimals() {
    let rec = record(Some(Stamp {
        wall_ms: 1_700_000_000_123,
        mono_ms: 0.0,
    }));
    let line = CaptionLine::from_record(1, &rec, never_asked);

    assert_eq!(line.t, py_round3(1_700_000_000_123_f64 / 1000.0));
    assert_eq!(line.t, 1_700_000_000.123);
}

#[test]
fn from_record_maps_frame_and_text() {
    let mut rec = record(Some(Stamp {
        wall_ms: 1,
        mono_ms: 0.0,
    }));

    rec.frame = FrameRef::NoClock;
    assert_eq!(CaptionLine::from_record(1, &rec, never_asked).frame, None);
    rec.frame = FrameRef::OutOfFrame;
    assert_eq!(CaptionLine::from_record(1, &rec, never_asked).frame, None);
    rec.frame = FrameRef::InFrame(7);
    assert_eq!(
        CaptionLine::from_record(1, &rec, never_asked).frame,
        Some(7)
    );

    rec.text = None;
    assert_eq!(CaptionLine::from_record(1, &rec, never_asked).text, "");
    rec.text = Some(String::new());
    assert_eq!(CaptionLine::from_record(1, &rec, never_asked).text, "");
}

#[test]
fn from_record_rounds_read_ms() {
    let rec = record(Some(Stamp {
        wall_ms: 1,
        mono_ms: 0.0,
    }));
    let line = CaptionLine::from_record(1, &rec, never_asked);

    assert_eq!(line.read_ms, Some(py_round3(12.34567)));
    assert_eq!(line.read_ms, Some(12.346));
}

#[test]
fn from_record_span_goes_to_first_and_last_mono_ms() {
    let mut rec = record(Some(Stamp {
        wall_ms: 1,
        mono_ms: 0.0,
    }));
    rec.span = Some((1000.0, 2000.5));
    let line = CaptionLine::from_record(1, &rec, never_asked);

    assert_eq!(line.extra["first_mono_ms"], json!(1000.0));
    assert_eq!(line.extra["last_mono_ms"], json!(2000.5));

    rec.span = None;
    let line = CaptionLine::from_record(1, &rec, never_asked);
    assert!(!line.extra.contains_key("first_mono_ms"));
    assert!(!line.extra.contains_key("last_mono_ms"));
}

#[test]
fn from_record_decision_and_speaker_raw_match_python_labels() {
    let mut rec = record(Some(Stamp {
        wall_ms: 1,
        mono_ms: 0.0,
    }));
    rec.decision = Decision::Merge {
        prefix_ratio: 0.666,
        ratio: 0.674,
    };
    rec.speaker_raw = Some("Unknown User".into());

    let line = CaptionLine::from_record(1, &rec, never_asked);

    assert_eq!(line.decision, rec.decision.to_string());
    assert_eq!(line.decision, "merge_p0.67_r0.67");
    assert_eq!(line.speaker_raw.as_deref(), Some("Unknown User"));
}

#[test]
fn write_record_numbers_lines_consecutively() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("captions.jsonl");
    let clock = FakeClock::new(1_700_000_000_000);
    let mut writer = CaptionWriter::create_new(&path).unwrap();

    let mut seqs = Vec::new();
    for _ in 0..3 {
        clock.advance_ms(10.0);
        seqs.push(writer.write_record(&record(None), &clock).unwrap());
    }
    assert_eq!(seqs, vec![1, 2, 3]);
    assert_eq!(writer.next_seq(), 4);
    drop(writer);

    let lines = raw_values(&path);
    assert_eq!(lines.len(), 3);
    for (i, value) in lines.iter().enumerate() {
        let step = i as i64 + 1;
        assert_eq!(value["seq"], json!(step));
        assert_eq!(value["wall_ms"], json!(1_700_000_000_000 + 10 * step));
        assert_eq!(value["mono_ms"], json!(10.0 * step as f64));
    }
}

#[test]
fn write_record_does_not_ask_the_clock_when_the_stamp_is_present() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("captions.jsonl");
    let clock = ScriptedClock::new(Vec::<Stamp>::new());
    let mut writer = CaptionWriter::create_new(&path).unwrap();

    let stamped = record(Some(Stamp {
        wall_ms: 1_700_000_000_000,
        mono_ms: 0.0,
    }));
    assert_eq!(writer.write_record(&stamped, &clock).unwrap(), 1);
    assert_eq!(clock.calls(), 0);
}

// ---------------------------------------------------------------- (vii) writer

#[test]
fn create_new_refuses_an_existing_file_and_leaves_it_alone() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("captions.jsonl");
    fs::write(&path, "keep\n").unwrap();

    let Err(err) = CaptionWriter::create_new(&path) else {
        panic!("create_new must not reuse an existing file");
    };
    assert!(matches!(err, StoreError::Io { .. }), "{err:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "keep\n");
}

#[test]
fn open_append_continues_from_the_given_seq() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("captions.jsonl");
    {
        let mut writer = CaptionWriter::create_new(&path).unwrap();
        writer.write_line(&caption(1, "uno")).unwrap();
        writer.write_line(&caption(2, "dos")).unwrap();
    }

    let clock = FakeClock::new(1_700_000_000_000);
    let mut writer = CaptionWriter::open_append(&path, 3).unwrap();
    assert_eq!(writer.next_seq(), 3);
    assert_eq!(writer.write_record(&record(None), &clock).unwrap(), 3);
    drop(writer);

    let seqs: Vec<u64> = raw_values(&path)
        .iter()
        .map(|v| v["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, vec![1, 2, 3]);
}

#[test]
fn every_line_is_flushed_before_the_writer_is_dropped() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("captions.jsonl");
    let mut writer = CaptionWriter::create_new(&path).unwrap();

    writer.write_line(&caption(1, "uno")).unwrap();
    let after_one = fs::read_to_string(&path).unwrap();
    assert_eq!(after_one.lines().count(), 1);
    assert!(after_one.ends_with('\n'));

    writer.write_line(&caption(2, "dos")).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 2);
}

#[test]
fn write_line_moves_next_seq_past_an_explicit_seq() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("captions.jsonl");
    let mut writer = CaptionWriter::create_new(&path).unwrap();

    writer.write_line(&caption(10, "x")).unwrap();
    assert_eq!(writer.next_seq(), 11);
    writer.write_line(&caption(4, "y")).unwrap();
    assert_eq!(writer.next_seq(), 11);
}

#[test]
fn open_append_after_a_truncated_tail_keeps_the_new_line() {
    let dir = TempDir::new().unwrap();
    let path = temp_file(
        &dir,
        "captions.jsonl",
        format!("{}\n{{\"v\":2,\"seq\":2,\"fr", v2_json(1, "a")).as_bytes(),
    );

    let mut writer = CaptionWriter::open_append(&path, 2).unwrap();
    writer.write_line(&caption(2, "b")).unwrap();
    drop(writer);

    let report = jsonl::read_file(&path).unwrap();
    assert_eq!(report.lines.len(), 2, "bad: {:?}", report.bad);
}

// ---------------------------------------------------------------- (viii) manifest

fn fixture_text(name: &str) -> String {
    fs::read_to_string(data(name)).unwrap()
}

#[test]
fn closed_manifest_loads_the_python_fields() {
    let m = Manifest::load(data("manifest_closed.json")).unwrap();

    assert_eq!(m.schema, MANIFEST_SCHEMA);
    assert_eq!(m.session_id.len(), 32);
    assert_eq!(m.meeting_name.as_deref(), Some("Daily"));
    assert_eq!(m.capture_source.as_deref(), Some("FakeSource"));
    assert_eq!(m.platform_requested, "teams");
    assert_eq!(m.app.version, "2.0");
    assert!(m.ended_cleanly());
    assert_eq!(m.ended_wall_ms, Some(1_700_000_001_500));
    assert_eq!(m.ended_mono_ms, Some(1500.0));
    assert_eq!(m.end_reason.as_deref(), Some("stop"));
    assert_eq!(m.captions.file, "captions.jsonl");
    assert_eq!(m.captions.schema, 2);
    assert_eq!(m.captions.anonymous_label, "Invitado sin nombre");
    assert_eq!(
        m.captions.anonymous_aliases,
        vec!["usuario desconocido", "unknown user"]
    );
    assert!(m.captions.extra.is_empty());
}

#[test]
fn open_manifest_is_not_ended_cleanly() {
    let m = Manifest::load(data("manifest_open.json")).unwrap();

    assert_eq!(m.schema, MANIFEST_SCHEMA);
    assert_eq!(m.meeting_name.as_deref(), Some("Daily"));
    assert_eq!(m.capture_source.as_deref(), Some("FakeSource"));
    assert!(!m.ended_cleanly());
    assert_eq!(m.ended_wall_ms, None);
    assert_eq!(m.ended_utc_iso, None);
    assert_eq!(m.ended_mono_ms, None);
    assert_eq!(m.end_reason, None);
    assert_eq!(m.captions.file, "captions.jsonl");
    assert_eq!(m.captions.schema, 2);
    assert_eq!(m.captions.anonymous_label, "Invitado sin nombre");
    assert_eq!(
        m.captions.anonymous_aliases,
        vec!["usuario desconocido", "unknown user"]
    );
}

#[test]
fn manifests_round_trip_without_losing_any_field() {
    for name in ["manifest_closed.json", "manifest_open.json"] {
        let m = Manifest::load(data(name)).unwrap();
        let back = Manifest::from_json(&m.to_json().unwrap()).unwrap();
        assert_eq!(back, m, "{name}");

        // Every key Python wrote comes back with the same value.
        let original: Value = serde_json::from_str(&fixture_text(name)).unwrap();
        let written: Value = serde_json::from_str(&m.to_json().unwrap()).unwrap();
        assert_eq!(written, original, "{name}");
    }
}

#[test]
fn unknown_manifest_fields_survive_a_rust_round_trip() {
    let mut input: Value = serde_json::from_str(&fixture_text("manifest_closed.json")).unwrap();
    input["x_future"] = json!({"a": [1, 2.5], "b": null});
    input["captions"]["capture_all_nodes"] = json!(true);

    let m = Manifest::from_json(&input.to_string()).unwrap();
    assert!(m.extra.contains_key("x_future"));
    assert_eq!(
        m.captions.extra.get("capture_all_nodes"),
        Some(&json!(true))
    );

    let written: Value = serde_json::from_str(&m.to_json().unwrap()).unwrap();
    assert_eq!(written, input);
}

#[test]
fn unknown_keys_inside_app_os_and_clock_survive() {
    let mut input: Value = serde_json::from_str(&fixture_text("manifest_closed.json")).unwrap();
    input["app"]["x_future"] = json!(1);
    input["os"]["x_future"] = json!(2);
    input["clock"]["x_future"] = json!(3);

    let m = Manifest::from_json(&input.to_string()).unwrap();
    let written: Value = serde_json::from_str(&m.to_json().unwrap()).unwrap();
    assert_eq!(written, input);
}

#[test]
fn manifest_with_another_schema_is_a_format_error() {
    let text = fixture_text("manifest_closed.json")
        .replace("meet-copilot.corpus/1", "meet-copilot.corpus/2");
    let err = Manifest::from_json(&text).unwrap_err();
    assert!(matches!(err, StoreError::Format(_)), "{err:?}");
}

#[test]
fn broken_manifest_json_is_an_error() {
    let err = Manifest::from_json("{\"schema\": \"meet-copilot.corpus/1\"").unwrap_err();
    assert!(matches!(err, StoreError::Json(_)), "{err:?}");

    let missing = Manifest::load(data("does_not_exist.json")).unwrap_err();
    assert!(matches!(missing, StoreError::Io { .. }), "{missing:?}");
}

#[test]
fn save_atomic_writes_the_file_and_leaves_no_temporary() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("manifest.json");
    let m = Manifest::load(data("manifest_closed.json")).unwrap();

    m.save_atomic(&path).unwrap();

    assert!(path.exists());
    assert!(!dir.path().join("manifest.json.tmp").exists());
    assert_eq!(Manifest::load(&path).unwrap(), m);

    // Overwriting with another manifest replaces the content.
    let open = Manifest::load(data("manifest_open.json")).unwrap();
    open.save_atomic(&path).unwrap();
    assert_eq!(Manifest::load(&path).unwrap(), open);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn save_atomic_into_a_missing_directory_is_an_error() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("no_such_dir").join("manifest.json");
    let m = Manifest::load(data("manifest_closed.json")).unwrap();

    assert!(m.save_atomic(&path).is_err());
}

#[test]
fn minimal_manifest_loads_with_defaults() {
    let m =
        Manifest::from_json(r#"{"schema": "meet-copilot.corpus/1", "session_id": "abc"}"#).unwrap();

    assert_eq!(m.session_id, "abc");
    assert!(m.app.version.is_empty());
    assert_eq!(m.app.git_commit, None);
    assert_eq!(m.meeting_name, None);
    assert_eq!(m.capture_source, None);
    assert_eq!(m.started_wall_ms, 0);
    assert_eq!(m.clock.t0_wall_ms, 0);
    assert!(m.captions.file.is_empty());
    assert!(m.captions.anonymous_aliases.is_empty());
    assert!(m.libs.is_null());
    assert!(m.audio.is_null());
    assert!(!m.ended_cleanly());

    let back = Manifest::from_json(&m.to_json().unwrap()).unwrap();
    assert_eq!(back, m);
}

// ---------------------------------------------------------------- (ix) auditoría: escritura

fn file_len(path: &Path) -> u64 {
    fs::metadata(path).unwrap().len()
}

#[test]
fn non_finite_numbers_are_refused_without_writing_or_moving_seq() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("captions.jsonl");
    let mut writer = CaptionWriter::create_new(&path).unwrap();
    let before = file_len(&path);

    let mut nan_mono = caption(5, "a");
    nan_mono.mono_ms = f64::NAN;
    let mut inf_read = caption(5, "b");
    inf_read.read_ms = Some(f64::INFINITY);
    let mut neg_t = caption(5, "c");
    neg_t.t = f64::NEG_INFINITY;

    for line in [&nan_mono, &inf_read, &neg_t] {
        let err = writer.write_line(line).unwrap_err();
        assert!(matches!(err, StoreError::Format(_)), "{err:?}");
        assert_eq!(file_len(&path), before);
        assert_eq!(writer.next_seq(), 1);
    }

    // The writer keeps working, and the first good line still gets seq 1.
    writer.write_line(&caption(1, "ok")).unwrap();
    assert_eq!(writer.next_seq(), 2);
    drop(writer);

    let report = jsonl::read_file(&path).unwrap();
    assert_eq!(report.lines.len(), 1);
    assert!(report.bad.is_empty(), "{:?}", report.bad);
}

#[test]
fn unknown_version_on_an_unterminated_last_line_is_bad_not_a_cut() {
    let bytes = format!(
        "{}\n{}",
        v2_json(1, "a"),
        v2_json(2, "b").replace("\"v\":2", "\"v\":3")
    );
    let report = jsonl::read_bytes(bytes.as_bytes());

    assert_eq!(report.lines.len(), 1);
    assert_eq!(report.bad.len(), 1);
    assert_eq!(report.bad[0].line_no, 2);
    assert!(
        report.bad[0]
            .reason
            .contains("unsupported schema version 3"),
        "{}",
        report.bad[0].reason
    );
    assert!(!report.truncated_tail);
}

#[test]
fn utf8_bom_is_not_part_of_the_first_line() {
    let bytes = format!("\u{feff}{}\n", v2_json(1, "a"));
    let report = jsonl::read_bytes(bytes.as_bytes());

    assert!(report.bad.is_empty(), "{:?}", report.bad);
    assert_eq!(report.lines.len(), 1);
    assert_eq!(report.lines[0].0, 1);
    assert_eq!(expect_v2(&report.lines[0].1).seq, 1);
}

#[test]
fn read_report_next_seq_continues_the_numbering() {
    assert_eq!(jsonl::read_bytes(b"").next_seq(), 1);

    let v1 = jsonl::read_file(data("raw_uia_v1.jsonl")).unwrap();
    assert_eq!(v1.next_seq(), 1);

    let v2_path = data("captions_v2.jsonl");
    let report = jsonl::read_file(&v2_path).unwrap();
    let last_seq = raw_values(&v2_path)
        .iter()
        .map(|v| v["seq"].as_u64().unwrap())
        .max()
        .unwrap();
    assert_eq!(report.next_seq(), last_seq + 1);

    let dir = TempDir::new().unwrap();
    let path = temp_file(&dir, "captions.jsonl", &fs::read(&v2_path).unwrap());
    let mut writer = CaptionWriter::open_append(&path, report.next_seq()).unwrap();
    assert_eq!(writer.next_seq(), last_seq + 1);
    writer
        .write_line(&caption(writer.next_seq(), "continua"))
        .unwrap();
    drop(writer);

    let after = jsonl::read_file(&path).unwrap();
    assert!(after.bad.is_empty(), "{:?}", after.bad);
    assert_eq!(after.lines.len(), report.lines.len() + 1);
    assert_eq!(after.next_seq(), last_seq + 2);
    assert_eq!(expect_v2(&after.lines.last().unwrap().1).seq, last_seq + 1);
}

#[cfg(target_os = "linux")]
#[test]
fn a_failing_device_gives_errors_not_panics_and_seq_keeps_moving() {
    let mut writer = CaptionWriter::open_append("/dev/full", 1).unwrap();

    let first = writer.write_line(&caption(1, "a"));
    assert!(matches!(first, Err(StoreError::Io { .. })), "{first:?}");
    assert_eq!(writer.next_seq(), 2);

    // The next write is attempted again (and fails again), without a panic.
    let second = writer.write_line(&caption(2, "b"));
    assert!(second.is_err());
    assert_eq!(writer.next_seq(), 3);
}

// ---------------------------------------------------------------- (x) auditoría: manifest

#[test]
fn extra_keys_that_collide_with_typed_fields_never_reach_the_file() {
    let mut m = Manifest::load(data("manifest_closed.json")).unwrap();
    let typed_session = m.session_id.clone();
    let typed_version = m.app.version.clone();
    let typed_system = m.os.system.clone();
    let typed_file = m.captions.file.clone();
    m.extra.insert("session_id".into(), json!("EXTRA"));
    m.app.extra.insert("version".into(), json!("EXTRA"));
    m.os.extra.insert("system".into(), json!("EXTRA"));
    m.captions.extra.insert("file".into(), json!("EXTRA"));

    let text = m.to_json().unwrap();
    assert_eq!(text.matches("\"session_id\"").count(), 1);
    assert!(
        !text.contains("EXTRA"),
        "a colliding extra reached the file"
    );

    let back = Manifest::from_json(&text).unwrap();
    assert_eq!(back.session_id, typed_session);
    assert_eq!(back.app.version, typed_version);
    assert_eq!(back.os.system, typed_system);
    assert_eq!(back.captions.file, typed_file);
    assert!(!back.extra.contains_key("session_id"));
    assert!(!back.app.extra.contains_key("version"));
    assert!(!back.os.extra.contains_key("system"));
    assert!(!back.captions.extra.contains_key("file"));
}

#[test]
fn null_values_in_typed_manifest_fields_read_as_defaults() {
    let text = r#"{
        "schema": "meet-copilot.corpus/1",
        "session_id": "abc",
        "app": {"version": null, "git_commit": null},
        "os": {"system": null},
        "clock": {"t0_wall_ms": null},
        "captions": {"schema": null, "anonymous_aliases": null},
        "platform_requested": null
    }"#;
    let m = Manifest::from_json(text).unwrap();

    assert_eq!(m.app.version, "");
    assert_eq!(m.os.system, "");
    assert_eq!(m.clock.t0_wall_ms, 0);
    assert_eq!(m.captions.schema, 0);
    assert!(m.captions.anonymous_aliases.is_empty());
    assert_eq!(m.platform_requested, "");
}

#[test]
fn save_atomic_falls_back_to_a_direct_write_when_the_temporary_name_is_taken() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("manifest.json");
    // A directory named like the temporary file makes the atomic path fail.
    fs::create_dir(dir.path().join("manifest.json.tmp")).unwrap();
    let m = Manifest::load(data("manifest_closed.json")).unwrap();

    m.save_atomic(&path).unwrap();

    assert_eq!(Manifest::load(&path).unwrap(), m);
}

// ---------------------------------------------------------------- (xi) auditoría: v1 nulls

#[test]
fn legacy_line_with_null_speaker_and_text_reads_as_empty_strings() {
    let report = jsonl::read_bytes(
        b"{\"t\":1.0,\"speaker\":null,\"text\":null,\"decision\":\"empty_text\"}\n",
    );

    assert!(report.bad.is_empty(), "{:?}", report.bad);
    assert_eq!(report.lines.len(), 1);
    let line = expect_v1(&report.lines[0].1);
    assert_eq!(line.t, 1.0);
    assert_eq!(line.speaker, "");
    assert_eq!(line.text, "");
    assert_eq!(line.decision, "empty_text");
}

// ---------------------------------------------------------------- (xii) cruce con Python

/// The original Python app's `recorder_cases()`: 300 `RawRecorder.log`
/// calls, each with the exact line Python wrote (`line`).
fn recorder_cases() -> Vec<Value> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/vectors/recorder_cases.json");
    let text = fs::read_to_string(&path).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// The `Decision` whose `Display` is the label Python wrote.
fn decision_from_label(label: &str) -> Decision {
    match label {
        "new_open" => Decision::NewOpen,
        "grow" => Decision::Grow,
        "committed" => Decision::Committed,
        "newline" => Decision::Newline,
        "empty_text" => Decision::EmptyText,
        "dup_committed_skip" => Decision::DupCommittedSkip,
        other => {
            let rest = other
                .strip_prefix("merge_p")
                .unwrap_or_else(|| panic!("unknown decision label {other:?}"));
            let (prefix, ratio) = rest
                .split_once("_r")
                .unwrap_or_else(|| panic!("bad merge label {other:?}"));
            Decision::Merge {
                prefix_ratio: prefix.parse().unwrap(),
                ratio: ratio.parse().unwrap(),
            }
        }
    }
}

/// The `Record` that `RawRecorder.log` was called with. Python writes
/// `speaker_raw` only when it is truthy and differs from the speaker, so the
/// record gets it under the same rule.
fn record_from_case(case: &Value) -> (Record, String) {
    let speaker = case["speaker"].as_str().unwrap_or("").to_string();
    let speaker_raw = case["speaker_raw"]
        .as_str()
        .filter(|raw| !raw.is_empty() && *raw != speaker)
        .map(str::to_string);
    let stamp = &case["stamp"];
    let span = case["extra"].as_object().and_then(|extra| {
        match (extra.get("first_mono_ms"), extra.get("last_mono_ms")) {
            (Some(first), Some(last)) => Some((first.as_f64().unwrap(), last.as_f64().unwrap())),
            _ => None,
        }
    });
    let record = Record {
        speaker: speaker.clone(),
        text: case["text"].as_str().map(str::to_string),
        decision: decision_from_label(case["decision"].as_str().unwrap()),
        speaker_raw,
        stamp: Some(Stamp {
            wall_ms: stamp["wall_ms"].as_i64().unwrap(),
            mono_ms: stamp["mono_ms"].as_f64().unwrap(),
        }),
        frame: match case["frame"].as_u64() {
            Some(n) => FrameRef::InFrame(n),
            None => FrameRef::OutOfFrame,
        },
        read_ms: case["read_ms"].as_f64(),
        span,
    };
    (record, speaker)
}

#[test]
fn rust_writer_matches_python_recorder_line_by_line() {
    let cases = recorder_cases();
    assert_eq!(cases.len(), 300);

    let mut compared = 0;
    let mut with_extra_x = 0;
    for (n, case) in cases.iter().enumerate() {
        let ctx = format!("case {n}: {case}");
        let (record, speaker) = record_from_case(case);
        let python = &case["line"];

        // The decision label survives the round trip through Decision.
        assert_eq!(
            record.decision.to_string(),
            case["decision"].as_str().unwrap(),
            "{ctx}"
        );
        // Python wrote speaker_raw exactly when the rule above keeps it.
        assert_eq!(
            python.get("speaker_raw").and_then(Value::as_str),
            record.speaker_raw.as_deref(),
            "{ctx}"
        );
        assert_eq!(python["speaker"].as_str().unwrap(), speaker, "{ctx}");

        // `extra` keys other than the span cannot be carried by a Record: they
        // are added to the line directly, after from_record built the rest.
        let mut line = CaptionLine::from_record(1, &record, || {
            unreachable!("the recorder always passes a stamp")
        });
        if let Some(extra) = case["extra"].as_object() {
            assert_eq!(
                extra.contains_key("first_mono_ms"),
                extra.contains_key("last_mono_ms"),
                "{ctx}"
            );
            for (key, value) in extra {
                if key != "first_mono_ms" && key != "last_mono_ms" {
                    line.extra.insert(key.clone(), value.clone());
                    with_extra_x += 1;
                }
            }
        }

        let mut expected = python.clone();
        assert_eq!(expected["seq"], json!(1), "{ctx}");
        expected.as_object_mut().unwrap().remove("seq");
        let mut actual = line.to_value();
        actual.as_object_mut().unwrap().remove("seq");
        assert_eq!(actual, expected, "{ctx}");
        compared += 1;
    }

    // Nothing is skipped: all 300 cases are compared (116 of them with an extra
    // `x` key added directly, see above).
    assert_eq!(compared, 300);
    assert!(compared >= 200);
    assert_eq!(with_extra_x, 116);
}
