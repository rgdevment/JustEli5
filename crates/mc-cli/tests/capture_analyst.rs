//! `mc capture --analyst` with the fake model: events, files, failures, and what the user
//! adds to a session (notes, speaker names) reaching `mc run`, `mc analyze` and `mc minutes`.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Sandbox, path_str};
use serde_json::{Value, json};

fn fake_source(sb: &Sandbox) -> String {
    let dir = sb.corpus_at("synthetic_basic", "fake/synthetic_basic");
    format!("fake:{}", path_str(&dir))
}

/// A config whose fake model answers with `answers`, in order.
fn llm_config(sb: &Sandbox, extra: Value, answers: &[&str]) -> String {
    let path = sb.path("config.json");
    let mut value = json!({"llm": {"provider": "fake", "args": answers}});
    if let (Some(base), Some(more)) = (value.as_object_mut(), extra.as_object()) {
        base.extend(more.clone());
    }
    fs::write(&path, value.to_string()).unwrap();
    path_str(&path).to_string()
}

fn events(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
        .collect()
}

fn session(sb: &Sandbox) -> PathBuf {
    let root = sb.data().join("corpus");
    let mut dirs: Vec<PathBuf> = fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    dirs.sort();
    assert_eq!(dirs.len(), 1, "{dirs:?}");
    dirs.remove(0)
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

const ANSWER: &str = r#"{"add":{
    "topics":[{"title":"Daily","evidence":[0]}],
    "decisions":[{"text":"Seguir con el deploy","evidence":[1,3]}],
    "commitments":[{"who":"Luis","what":"arreglar staging","due":"hoy","confidence":0.8,"evidence":[1]}]}}"#;

#[test]
fn the_analyst_reports_the_ledger_and_leaves_it_apart_from_the_captions() {
    let sb = Sandbox::new();
    let source = fake_source(&sb);
    let config = llm_config(&sb, json!({}), &[ANSWER]);
    let out = sb.mc(&[
        "--json",
        "--config",
        &config,
        "capture",
        "--source",
        &source,
        "--poll-ms",
        "20",
        "--consent",
        "--analyst",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let all = events(&out.stdout);
    let names: Vec<&str> = all.iter().map(|e| e["event"].as_str().unwrap()).collect();
    assert_eq!(names.last(), Some(&"ended"));
    let ledger: Vec<&Value> = all.iter().filter(|e| e["event"] == "ledger").collect();
    assert_eq!(ledger.len(), 1, "{names:?}");
    assert_eq!(ledger[0]["counts"]["decisions"], 1);
    assert_eq!(ledger[0]["accepted"], 3);
    let kinds: Vec<&str> = ledger[0]["changed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["topic", "decision", "commitment"]);
    assert_eq!(ledger[0]["changed"][2]["text"], "Luis: arreglar staging");

    let dir = session(&sb);
    let live = dir.join("analysis/live");
    let saved = read_json(&live.join("ledger.json"));
    assert_eq!(saved["decisions"][0]["evidence"], json!([1, 3]));
    assert_eq!(
        fs::read_to_string(live.join("ledger_events.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let meta = read_json(&live.join("meta.json"));
    assert_eq!(
        (meta["source"].as_str(), meta["provider"].as_str()),
        (Some("live"), Some("fake"))
    );
    assert_eq!(meta["finished"], true);
    assert_eq!(meta["complete"], true);

    // The captions are the truth and replay exactly as without the analyst.
    let replay = sb.mc(&["replay", path_str(&dir), "--verify", "--strict"]);
    assert_eq!(replay.code, 0, "{}\n{}", replay.stdout, replay.stderr);
}

#[test]
fn the_idx_of_a_live_segment_is_the_idx_of_a_deterministic_run() {
    let sb = Sandbox::new();
    let source = fake_source(&sb);
    let config = llm_config(&sb, json!({}), &[ANSWER]);
    let out = sb.mc(&[
        "--json",
        "--config",
        &config,
        "capture",
        "--source",
        &source,
        "--poll-ms",
        "20",
        "--consent",
        "--analyst",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let live: Vec<Value> = events(&out.stdout)
        .into_iter()
        .filter(|e| e["event"] == "segment")
        .collect();
    assert_eq!(live.len(), 4);

    let dir = session(&sb);
    let run = sb.mc(&["--json", "run", path_str(&dir)]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let segments = PathBuf::from(run.json()["path"].as_str().unwrap()).join("segments.jsonl");
    let replayed: Vec<Value> = fs::read_to_string(segments)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(replayed.len(), live.len());
    for (live, replayed) in live.iter().zip(&replayed) {
        assert_eq!(live["idx"], replayed["idx"]);
        assert_eq!(live["text"], replayed["text"]);
        assert_eq!(live["speaker"], replayed["speaker"]);
    }
}

#[test]
fn a_model_that_fails_is_an_event_and_the_capture_still_ends_well() {
    let sb = Sandbox::new();
    let source = fake_source(&sb);
    let config = llm_config(&sb, json!({}), &[]);
    let out = sb.mc(&[
        "--json",
        "--config",
        &config,
        "capture",
        "--source",
        &source,
        "--poll-ms",
        "20",
        "--consent",
        "--analyst",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let all = events(&out.stdout);
    let failure = all.iter().find(|e| e["event"] == "analyst_error").unwrap();
    assert!(failure["message"].as_str().unwrap().contains("#0-#3"));
    assert_eq!(all.iter().filter(|e| e["event"] == "segment").count(), 4);
    let meta = read_json(&session(&sb).join("analysis/live/meta.json"));
    assert_eq!(meta["failed_windows"], 1);
    assert_eq!(meta["complete"], false);
}

#[test]
fn the_config_key_turns_the_analyst_on_and_it_is_off_by_default() {
    let sb = Sandbox::new();
    let source = fake_source(&sb);
    let off = sb.mc(&[
        "--json",
        "capture",
        "--source",
        &source,
        "--poll-ms",
        "20",
        "--consent",
    ]);
    assert_eq!(off.code, 0, "{}", off.stderr);
    assert!(!session(&sb).join("analysis").exists());
    assert!(events(&off.stdout).iter().all(|e| e["event"] != "ledger"));

    let sb = Sandbox::new();
    let source = fake_source(&sb);
    let config = llm_config(&sb, json!({"capture": {"analyst": true}}), &[ANSWER]);
    let on = sb.mc(&[
        "--json",
        "--config",
        &config,
        "capture",
        "--source",
        &source,
        "--poll-ms",
        "20",
        "--consent",
    ]);
    assert_eq!(on.code, 0, "{}", on.stderr);
    assert!(session(&sb).join("analysis/live/ledger.json").is_file());
}

#[test]
fn analyst_options_are_checked_before_anything_is_recorded() {
    let sb = Sandbox::new();
    let source = fake_source(&sb);
    let config = llm_config(&sb, json!({}), &[]);
    let base = [
        "--json",
        "--config",
        &config,
        "capture",
        "--source",
        &source,
        "--consent",
    ];
    let with = |extra: &[&str]| {
        let mut args = base.to_vec();
        args.extend(extra);
        sb.mc(&args)
    };
    assert_eq!(with(&["--llm-model", "haiku"]).code, 2, "needs --analyst");
    let no_record = sb.mc(&[
        "--json",
        "--config",
        &config,
        "capture",
        "--source",
        &source,
        "--no-record",
        "--analyst",
    ]);
    assert_eq!(no_record.code, 2);
    let broken = llm_config(&sb, json!({}), &[]);
    let unknown = sb.mc(&[
        "--json",
        "--config",
        &broken,
        "capture",
        "--source",
        &source,
        "--consent",
        "--analyst",
        "--llm-provider",
        "nope",
    ]);
    assert_eq!(unknown.code, 2, "{}", unknown.stderr);
    assert!(!sb.data().join("corpus").exists());
}

#[test]
fn chosen_speaker_names_show_in_run_and_in_the_analysis_but_never_in_the_captions() {
    let sb = Sandbox::new();
    let source = fake_source(&sb);
    let out = sb.mc(&[
        "capture",
        "--source",
        &source,
        "--poll-ms",
        "20",
        "--consent",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let dir = session(&sb);
    let captions = fs::read(dir.join("captions.jsonl")).unwrap();

    let first = sb.mc(&["--json", "run", path_str(&dir)]);
    let before = first.json();
    let transcript = |run: &Value| {
        fs::read_to_string(PathBuf::from(run["path"].as_str().unwrap()).join("transcript.txt"))
            .unwrap()
    };
    assert!(transcript(&before).contains("[Luis]: "));

    let config = llm_config(&sb, json!({}), &[ANSWER, ANSWER]);
    let analysis = |extra: &[&str]| {
        let mut args = vec![
            "--json",
            "--config",
            config.as_str(),
            "analyze",
            path_str(&dir),
        ];
        args.extend(extra);
        let out = sb.mc(&args);
        assert_eq!(out.code, 0, "{}", out.stderr);
        out.json()
    };
    let plain = analysis(&[]);

    fs::write(dir.join("speaker_names.json"), r#"{"Luis": "Luis G."}"#).unwrap();
    let after = sb.mc(&["--json", "run", path_str(&dir)]).json();
    assert_ne!(after["run_id"], before["run_id"]);
    assert!(transcript(&after).contains("[Luis G.]: "));
    assert!(!transcript(&after).contains("[Luis]: "));
    assert_eq!(fs::read(dir.join("captions.jsonl")).unwrap(), captions);

    // The old run still says Luis; the analysis reads it with the names the user chose.
    let renamed = analysis(&["--run", before["run_id"].as_str().unwrap()]);
    assert_ne!(renamed["analysis_id"], plain["analysis_id"]);
}

#[test]
fn minutes_read_the_notes_taken_while_capturing_unless_a_file_is_given() {
    let sb = Sandbox::new();
    let source = fake_source(&sb);
    assert_eq!(
        sb.mc(&[
            "capture",
            "--source",
            &source,
            "--poll-ms",
            "20",
            "--consent"
        ])
        .code,
        0
    );
    let dir = session(&sb);
    assert_eq!(sb.mc(&["run", path_str(&dir)]).code, 0);
    let config = llm_config(
        &sb,
        json!({}),
        &[
            ANSWER,
            r#"{"verdicts":[]}"#,
            "## Resumen\nHubo un bloqueo [#1].",
            r#"{"verdicts":[]}"#,
            "## Resumen\nHubo un bloqueo [#1].",
            r#"{"verdicts":[]}"#,
            "## Resumen\nHubo un bloqueo [#1].",
        ],
    );
    let minutes = |extra: &[&str]| {
        let mut args = vec![
            "--json",
            "--config",
            config.as_str(),
            "minutes",
            path_str(&dir),
        ];
        args.extend(extra);
        let out = sb.mc(&args);
        assert_eq!(out.code, 0, "{}", out.stderr);
        out.json()["minutes_id"].as_str().unwrap().to_string()
    };
    let without = minutes(&[]);

    fs::write(
        dir.join("notes.jsonl"),
        r#"{"mono_ms": 5000.0, "wall_ms": 1700000005000, "text": "pedir el informe a Ana"}"#
            .to_string()
            + "\n",
    )
    .unwrap();
    let with_notes = minutes(&[]);
    assert_ne!(
        with_notes, without,
        "the notes are part of the minutes' identity"
    );

    let file = sb.path("my-notes.md");
    fs::write(&file, "otras notas").unwrap();
    let explicit = minutes(&["--notes", path_str(&file)]);
    assert_ne!(
        explicit, with_notes,
        "an explicit file wins over notes.jsonl"
    );
}

#[test]
fn with_the_config_key_on_the_analyst_flags_need_no_analyst_flag() {
    let sb = Sandbox::new();
    let source = fake_source(&sb);
    let config = llm_config(&sb, json!({"capture": {"analyst": true}}), &[ANSWER]);
    let out = sb.mc(&[
        "--json",
        "--config",
        &config,
        "capture",
        "--source",
        &source,
        "--poll-ms",
        "20",
        "--consent",
        "--llm-model",
        "haiku",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let meta = read_json(&session(&sb).join("analysis/live/meta.json"));
    assert_eq!(meta["model"], "haiku");
}
