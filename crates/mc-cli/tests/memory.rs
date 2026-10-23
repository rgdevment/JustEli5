//! Memory between meetings: series, commitments carried over a chain of meetings, and the
//! glossary that learns. Three fake meetings of one series over the fusion_basic corpus.

mod common;

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use common::{Sandbox, copy_tree, path_str};
use mc_asr::codec::{ASR_SCHEMA, AsrMeta};
use mc_cli::cli::{Io, run};
use serde_json::{Value, json};

const DAY_MS: i64 = 86_400_000;
const DRAFT: &str = "## Resumen\nok";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fusion/fusion_basic")
}

/// A copy of fusion_basic as the meeting `name` of day `day`, fused and ready to analyse.
fn meeting(sb: &Sandbox, id: &str, name: &str, day: i64) -> String {
    let dir = sb.path(&format!("sessions/{id}"));
    copy_tree(&fixture(), &dir);
    let run = dir.join("asr/aaaa1111");
    fs::create_dir_all(&run).unwrap();
    fs::copy(dir.join("asr_fixture.jsonl"), run.join("asr.jsonl")).unwrap();
    let meta = AsrMeta {
        v: ASR_SCHEMA,
        asr_id: "aaaa1111".into(),
        engine: "fixture".into(),
        model: "fixture".into(),
        model_sha256: None,
        params: json!(null),
        language: None,
        prompt: None,
        segmenter: json!(null),
        channels: vec![],
        segments: 0,
        created_wall_ms: 1,
    };
    fs::write(run.join("meta.json"), serde_json::to_string(&meta).unwrap()).unwrap();
    let path = dir.join("manifest.json");
    let mut manifest: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    manifest["session_id"] = json!(id);
    manifest["meeting_name"] = json!(name);
    manifest["started_wall_ms"] = json!(1_700_000_000_000 + day * DAY_MS);
    manifest["started_utc_iso"] = json!(format!("2026-10-{day:02}T10:00:00.000+00:00"));
    fs::write(&path, manifest.to_string()).unwrap();
    let dir = path_str(&dir).to_string();
    let fused = sb.mc(&["fuse", &dir]);
    assert_eq!(fused.code, 0, "{}", fused.stderr);
    dir
}

/// A config file whose fake model answers with `answers` in order; `extra` adds other keys.
fn config(sb: &Sandbox, name: &str, answers: &[&str], extra: Value) -> String {
    let mut value = json!({"llm": {"provider": "fake", "args": answers}});
    for (key, part) in extra.as_object().into_iter().flatten() {
        value[key] = part.clone();
    }
    let path = sb.path(name);
    fs::write(&path, value.to_string()).unwrap();
    path_str(&path).to_string()
}

fn mc(sb: &Sandbox, config: &str, args: &[&str]) -> common::Out {
    let mut full = vec!["--json", "--config", config];
    full.extend(args);
    sb.mc(&full)
}

fn ok(out: common::Out) -> Value {
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    out.json()
}

fn read_text(folder: &Value, file: &str) -> String {
    fs::read_to_string(PathBuf::from(folder.as_str().unwrap()).join(file)).unwrap()
}

fn read_json(folder: &Value, file: &str) -> Value {
    serde_json::from_str(&read_text(folder, file)).unwrap()
}

fn commitment(who: &str, what: &str, evidence: &[u64]) -> Value {
    json!({"who": who, "what": what, "confidence": 0.9, "evidence": evidence})
}

fn source(session: &str, ledger: &Value, what: &str) -> String {
    let found = ledger["commitments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["what"] == what)
        .unwrap_or_else(|| panic!("{what} in {ledger}"));
    format!("{session}#{}", found["id"].as_str().unwrap())
}

/// Analyses the meeting with `answer`, then writes its minutes.
fn analyze_and_minute(
    sb: &Sandbox,
    dir: &str,
    tag: &str,
    answer: &Value,
    supported: &[&str],
) -> (Value, Value) {
    let analyze_config = config(
        sb,
        &format!("{tag}-a.json"),
        &[&answer.to_string()],
        json!({}),
    );
    let analysis = ok(mc(sb, &analyze_config, &["analyze", dir]));
    let verdicts: Vec<Value> = supported
        .iter()
        .map(|id| json!({"id": id, "verdict": "supported", "reason": "r"}))
        .collect();
    let verdicts = json!({"verdicts": verdicts}).to_string();
    // Whatever is not listed counts as unverified: commitments stay as inferred, statuses stay open.
    let minutes_config = config(sb, &format!("{tag}-m.json"), &[&verdicts, DRAFT], json!({}));
    let minutes = ok(mc(sb, &minutes_config, &["minutes", dir]));
    (analysis, minutes)
}

#[test]
fn commitments_travel_down_a_chain_of_three_meetings_and_what_is_done_stops_travelling() {
    let sb = Sandbox::new();
    let (s1, s2, s3) = (
        meeting(&sb, "s1", "Daily de equipo 21/10", 21),
        meeting(&sb, "s2", "Daily de equipo 22/10", 22),
        meeting(&sb, "s3", "Daily de equipo 23/10", 23),
    );

    // Meeting 1 has nothing to inherit.
    let (a1, m1) = analyze_and_minute(
        &sb,
        &s1,
        "one",
        &json!({"add": {"commitments": [
            commitment("Ana", "cerrar el bug", &[3]),
            commitment("Luis", "revisar el pull request", &[2]),
        ]}}),
        &[],
    );
    assert_eq!(m1["carried"]["total"], 0);
    assert_eq!(read_json(&a1["path"], "meta.json")["carried"], 0);
    let ledger1 = read_json(&a1["path"], "ledger.json");
    let (bug, review) = (
        source("s1", &ledger1, "cerrar el bug"),
        source("s1", &ledger1, "revisar el pull request"),
    );
    let md1 = read_text(&m1["path"], "minutes.md");
    assert!(!md1.contains("Pendientes de la reunión anterior"), "{md1}");

    // Meeting 2: the bug is done (cited); the review is mentioned without evidence, so unknown.
    let (a2, m2) = analyze_and_minute(
        &sb,
        &s2,
        "two",
        &json!({
            "add": {"commitments": [commitment("Ana", "preparar la demo", &[2])]},
            "carried": [
                {"source": bug, "status": "done", "evidence": [1]},
                {"source": review, "status": "done"},
            ],
        }),
        &[&bug],
    );
    let meta2 = read_json(&a2["path"], "meta.json");
    assert_eq!(
        (meta2["carried"].as_u64(), meta2["carried_from"].as_str()),
        (Some(2), Some("s1"))
    );
    assert_eq!(a2["counts"]["carried"], 1);
    assert_eq!(
        m2["carried"],
        json!({"series": "daily de equipo", "from": "s1", "total": 2, "done": 1,
               "still_open": 0, "dropped": 0, "unknown": 1})
    );
    let md2 = read_text(&m2["path"], "minutes.md");
    assert!(
        md2.contains("## Pendientes de la reunión anterior\n"),
        "{md2}"
    );
    assert!(
        md2.contains("| Ana | cerrar el bug | hecho [#1] |"),
        "{md2}"
    );
    assert!(
        md2.contains("| Luis | revisar el pull request | sin resolver |"),
        "{md2}"
    );
    let sources2 = read_text(&m2["path"], "carried.json");
    assert!(
        sources2.contains(&bug) && sources2.contains(&review),
        "{sources2}"
    );
    let carried2 = read_json(&m2["path"], "carried.json");
    assert_eq!(carried2[0]["status"], "done");
    assert_eq!(carried2[1]["status"], Value::Null);
    let ledger2 = read_json(&a2["path"], "ledger.json");
    let demo = source("s2", &ledger2, "preparar la demo");

    // Meeting 3 inherits the review (unsaid) and the demo (new), never the finished bug.
    let (a3, m3) = analyze_and_minute(
        &sb,
        &s3,
        "three",
        &json!({
            "add": {"commitments": [commitment("Luis", "actualizar el pipeline", &[1])]},
            "carried": [
                {"source": bug, "status": "done", "evidence": [1]},
                {"source": review, "status": "still_open", "evidence": [2]},
            ],
        }),
        &[&review],
    );
    assert_eq!(read_json(&a3["path"], "meta.json")["carried"], 2);
    let events = read_text(&a3["path"], "ledger_events.jsonl");
    assert!(
        events.contains("not a carried commitment"),
        "the finished bug was not offered: {events}"
    );
    assert_eq!(a3["counts"]["carried"], 1);
    assert_eq!(
        m3["carried"],
        json!({"series": "daily de equipo", "from": "s2", "total": 2, "done": 0,
               "still_open": 1, "dropped": 0, "unknown": 1})
    );
    let md3 = read_text(&m3["path"], "minutes.md");
    assert!(
        md3.contains("| Luis | revisar el pull request | sigue abierto [#2] |"),
        "{md3}"
    );
    assert!(
        md3.contains("| Ana | preparar la demo | sin resolver |"),
        "{md3}"
    );
    let sources3 = read_text(&m3["path"], "carried.json");
    assert!(sources3.contains(&demo), "{sources3}");
    assert!(!md3.contains("cerrar el bug"), "{md3}");

    // The chain as `mc series` shows it.
    let list = ok(sb.mc(&["--json", "series", "list"]));
    assert_eq!(list["series"][0]["key"], "daily de equipo");
    assert_eq!(list["series"][0]["sessions"], 3);
    let show = ok(sb.mc(&["--json", "series", "show", "Daily de equipo"]));
    let ids: Vec<&str> = show["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["session_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["s1", "s2", "s3"]);
    assert_eq!(show["open_from"], "s3");
    let open: Vec<&str> = show["open_commitments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["what"].as_str().unwrap())
        .collect();
    assert_eq!(
        open,
        [
            "revisar el pull request",
            "preparar la demo",
            "actualizar el pipeline"
        ]
    );
}

#[test]
fn a_meeting_without_a_previous_one_with_minutes_inherits_nothing_and_a_series_of_one_is_listed() {
    let sb = Sandbox::new();
    let s1 = meeting(&sb, "s1", "Retro 3", 21);
    let _ = meeting(&sb, "s2", "Retro 4", 22);
    // Meeting 2 exists but meeting 1 has no minutes yet.
    let answer = json!({"add": {"commitments": [commitment("Ana", "cerrar el bug", &[3])]}});
    let cfg = config(&sb, "a.json", &[&answer.to_string()], json!({}));
    let analysis = ok(mc(&sb, &cfg, &["analyze", &s1]));
    assert_eq!(read_json(&analysis["path"], "meta.json")["carried"], 0);
    let show = ok(sb.mc(&["--json", "series", "show", "retro"]));
    assert_eq!(show["open_from"], Value::Null);
    assert_eq!(show["open_commitments"], json!([]));
    let missing = sb.mc(&["--json", "series", "show", "nada"]);
    assert_eq!(missing.code, 3, "{}", missing.stderr);
}

#[test]
fn a_manual_series_and_a_config_rule_group_meetings_that_the_name_would_not() {
    let sb = Sandbox::new();
    let _ = meeting(&sb, "s1", "Stand-up del equipo 3", 21);
    let _ = meeting(&sb, "s2", "Daily de equipo 22/10", 22);
    let other = meeting(&sb, "s3", "Charla de pasillo", 23);

    let plain = ok(sb.mc(&["--json", "series", "list"]));
    assert_eq!(plain["series"].as_array().unwrap().len(), 3);

    let cfg = config(
        &sb,
        "rules.json",
        &[],
        json!({"series": {"rules": [{"pattern": "STAND-UP", "key": "Daily de Equipo"}]}}),
    );
    let ruled = ok(mc(&sb, &cfg, &["series", "list"]));
    let counts: Vec<(&str, u64)> = ruled["series"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["key"].as_str().unwrap(), s["sessions"].as_u64().unwrap()))
        .collect();
    assert_eq!(counts, [("charla de pasillo", 1), ("daily de equipo", 2)]);
    let show = ok(mc(&sb, &cfg, &["series", "show", "daily de equipo"]));
    assert_eq!(show["sessions"][0]["origin"], "rule");
    assert_eq!(show["sessions"][1]["origin"], "auto");

    // The user's choice beats the name and the rules, and survives later refreshes.
    let set = ok(mc(
        &sb,
        &cfg,
        &["series", "set", &other, "DAILY  de equipo"],
    ));
    assert_eq!(set["series"], "daily de equipo");
    let after = ok(mc(&sb, &cfg, &["series", "show", "daily de equipo"]));
    assert_eq!(after["sessions"].as_array().unwrap().len(), 3);
    assert_eq!(after["sessions"][2]["origin"], "manual");
    let again = ok(sb.mc(&["--json", "series", "list"]));
    let keys: Vec<&str> = again["series"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["key"].as_str().unwrap())
        .collect();
    assert!(keys.contains(&"daily de equipo"), "{keys:?}");
    let by_prefix = ok(sb.mc(&["--json", "series", "set", "s1", "stand-up"]));
    assert_eq!(by_prefix["session_id"], "s1");
}

#[test]
fn bad_series_rules_and_usage_are_reported() {
    let sb = Sandbox::new();
    let empty_key = config(
        &sb,
        "r.json",
        &[],
        json!({"series": {"rules": [{"pattern": "x", "key": " "}]}}),
    );
    let out = mc(&sb, &empty_key, &["series", "list"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(out.stderr.contains("series.rules"), "{}", out.stderr);
    assert_eq!(sb.mc(&["series"]).code, 2);
    assert_eq!(sb.mc(&["series", "set", "only-one"]).code, 2);
    assert_eq!(sb.mc(&["series", "set", "nope", "key"]).code, 3);
}

fn glossary_file(sb: &Sandbox) -> String {
    let path = sb.path("team_glossary.json");
    fs::write(
        &path,
        r#"{"Docker": {"aliases": ["dokér"]}, "YAML": {"aliases": []}}"#,
    )
    .unwrap();
    path_str(&path).to_string()
}

#[test]
fn the_user_glossary_merges_with_the_configured_one_and_only_terms_reach_the_recogniser() {
    let sb = Sandbox::new();
    let file = glossary_file(&sb);
    let cfg = config(
        &sb,
        "g.json",
        &[],
        json!({"fusion": {"glossary_file": file}}),
    );
    ok(mc(
        &sb,
        &cfg,
        &["glossary", "add", "docker", "--alias", "doker"],
    ));
    ok(mc(
        &sb,
        &cfg,
        &["glossary", "add", "Jarvis", "--alias", "yarvis"],
    ));
    let list = ok(mc(&sb, &cfg, &["glossary", "list"]));
    assert_eq!(
        list["glossary"],
        json!([
            {"term": "Docker", "aliases": ["dokér", "doker"], "meaning": null, "user": true},
            {"term": "YAML", "aliases": [], "meaning": null, "user": false},
            {"term": "Jarvis", "aliases": ["yarvis"], "meaning": null, "user": true},
        ])
    );
    let stored = ok(mc(&sb, &cfg, &["memory", "list", "--query", "jarvis"]));
    assert_eq!(stored["entries"][0]["forms"][1]["text"], "yarvis");
    assert!(
        !sb.data().join("glossary.json").exists(),
        "the memory keeps it"
    );

    // A term of the configured file is not the user's to remove.
    let refused = mc(&sb, &cfg, &["glossary", "remove", "YAML"]);
    assert_eq!(refused.code, 3, "{}", refused.stderr);
    assert!(
        refused.stderr.contains("configured glossary"),
        "{}",
        refused.stderr
    );
    ok(mc(&sb, &cfg, &["glossary", "remove", "JARVIS"]));
    ok(mc(&sb, &cfg, &["glossary", "add", "Jarvis"]));

    // The Whisper prompt has the terms and never a misspelling.
    let session = sb.path("sessions/t");
    copy_tree(&common::fixture("synthetic_basic"), &session);
    let samples: Vec<i16> = (0..16_000)
        .map(|n| ((n as f32 * 0.25).sin() * 9000.0) as i16)
        .collect();
    let mut w = mc_audio::wav::WavWriter::create(&session.join("mic.wav"), 16_000).unwrap();
    w.write_samples(&[vec![0; 8000], samples, vec![0; 24_000]].concat())
        .unwrap();
    w.finish().unwrap();
    let script = sb.path("script.json");
    fs::write(&script, r#"["hola"]"#).unwrap();
    let engine = format!("fake:{}", path_str(&script));
    let dir = path_str(&session).to_string();
    let first = ok(mc(&sb, &cfg, &["transcribe", &dir, "--engine", &engine]));
    assert_eq!(
        first["meta"]["prompt"],
        "Vocabulario: Docker, Jarvis, YAML."
    );
    // A new term changes the prompt and with it the run id; the same glossary reuses it.
    assert_eq!(
        ok(mc(&sb, &cfg, &["transcribe", &dir, "--engine", &engine]))["reused"],
        true
    );
    ok(mc(&sb, &cfg, &["glossary", "add", "Kubernetes"]));
    let second = ok(mc(&sb, &cfg, &["transcribe", &dir, "--engine", &engine]));
    assert_ne!(second["asr_id"], first["asr_id"]);
    // A misspelling alone leaves the recogniser run as it was.
    ok(mc(
        &sb,
        &cfg,
        &["glossary", "add", "Kubernetes", "--alias", "cubernetes"],
    ));
    let third = ok(mc(&sb, &cfg, &["transcribe", &dir, "--engine", &engine]));
    assert_eq!(third["asr_id"], second["asr_id"]);
}

#[test]
fn the_analysis_follows_the_glossary_and_shows_the_model_the_misspellings() {
    let sb = Sandbox::new();
    let s1 = meeting(&sb, "s1", "Revisión", 21);
    let answer = json!({}).to_string();
    let cfg = config(&sb, "a.json", &[&answer, &answer, &answer], json!({}));
    let first = ok(mc(&sb, &cfg, &["analyze", &s1]));
    assert_eq!(ok(mc(&sb, &cfg, &["analyze", &s1]))["reused"], true);
    ok(mc(
        &sb,
        &cfg,
        &["glossary", "add", "Jarvis", "--alias", "yarvis"],
    ));
    let after_term = ok(mc(&sb, &cfg, &["analyze", &s1]));
    assert_ne!(after_term["analysis_id"], first["analysis_id"]);
    // The aliases are part of what the analyst reads, so they are part of its id too.
    ok(mc(
        &sb,
        &cfg,
        &["glossary", "add", "Jarvis", "--alias", "charvis"],
    ));
    let after_alias = ok(mc(&sb, &cfg, &["analyze", &s1]));
    assert_ne!(after_alias["analysis_id"], after_term["analysis_id"]);
}

fn review_session(sb: &Sandbox) -> String {
    let dir = meeting(sb, "s1", "Revisión del pipeline", 21);
    let answer = json!({"add": {
        "term_fixes": [{"heard": "paylain", "correct": "Pipeline", "context": "deploy", "evidence": [2]}],
        "glossary_candidates": [{"term": "Jarvis", "evidence": [1]}, {"term": "Docker", "evidence": [0]}],
    }});
    let cfg = config(sb, "a.json", &[&answer.to_string()], json!({}));
    ok(mc(sb, &cfg, &["analyze", &dir]));
    // A correction the user accepted in the TUI while recording.
    fs::write(
        Path::new(&dir).join("term_fixes.jsonl"),
        "{\"mono_ms\":1.0,\"wall_ms\":1,\"heard\":\"escrún\",\"correct\":\"Scrum\"}\n",
    )
    .unwrap();
    dir
}

#[test]
fn review_lists_the_suggestions_with_their_evidence_and_changes_nothing() {
    let sb = Sandbox::new();
    let dir = review_session(&sb);
    let docker = glossary_file(&sb);
    let cfg = config(
        &sb,
        "g.json",
        &[],
        json!({"fusion": {"glossary_file": docker}}),
    );
    let listed = ok(mc(&sb, &cfg, &["glossary", "review", &dir]));
    let found = listed["candidates"].as_array().unwrap();
    // Docker is already in the configured glossary: not suggested again.
    let terms: Vec<(&str, &str)> = found
        .iter()
        .map(|c| (c["source"].as_str().unwrap(), c["term"].as_str().unwrap()))
        .collect();
    assert_eq!(
        terms,
        [
            ("tui", "Scrum"),
            ("term_fix", "Pipeline"),
            ("glossary_candidate", "Jarvis")
        ]
    );
    assert_eq!(found[1]["heard"], "paylain");
    let quoted = found[1]["evidence"][0].as_str().unwrap();
    assert!(quoted.starts_with("[#2] "), "{quoted}");
    assert!(!sb.data().join("glossary.json").exists());
}

#[test]
fn accept_all_adds_everything_and_remembers_it() {
    let sb = Sandbox::new();
    let dir = review_session(&sb);
    let cfg = config(&sb, "g.json", &[], json!({}));
    let done = ok(mc(&sb, &cfg, &["glossary", "review", &dir, "--accept-all"]));
    assert_eq!(done["accepted"].as_array().unwrap().len(), 4);
    let list = ok(mc(&sb, &cfg, &["glossary", "list"]));
    let entries = list["glossary"].as_array().unwrap();
    let entry = |term: &str| {
        entries
            .iter()
            .find(|e| e["term"] == term)
            .unwrap_or_else(|| panic!("{term}"))
    };
    assert_eq!(entry("Scrum")["aliases"], json!(["escrún"]));
    assert_eq!(entry("Pipeline")["aliases"], json!(["paylain"]));
    assert_eq!(entry("Jarvis")["aliases"], json!([]));
    assert!(entries.iter().any(|e| e["term"] == "Docker"));
    let again = ok(mc(&sb, &cfg, &["glossary", "review", &dir]));
    assert_eq!(again["candidates"], json!([]));
}

/// Runs `mc` with answers typed at the terminal.
fn typed(sb: &Sandbox, args: &[&str], answers: &str) -> (u8, String, String) {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(
        &args,
        &sb.env,
        &mut Io {
            out: &mut out,
            err: &mut err,
            input: Some(Box::new(Cursor::new(answers.as_bytes().to_vec()))),
        },
    );
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn review_asks_one_by_one_and_a_rejection_is_not_asked_again() {
    let sb = Sandbox::new();
    let dir = review_session(&sb);
    // Pipeline: yes. Jarvis: no. Docker: Enter (later). The TUI correction needs no answer.
    let (code, out, err) = typed(&sb, &["glossary", "review", &dir], "s\nn\n\n");
    assert_eq!(code, 0, "{err}");
    assert!(
        err.contains("heard \"paylain\" -> \"Pipeline\"") && err.contains("[#2] "),
        "{err}"
    );
    assert!(
        out.contains("4 suggestions: 2 added, 1 rejected, 1 left for later"),
        "{out}"
    );
    let list = ok(sb.mc(&["--json", "glossary", "list"]));
    let terms: Vec<&str> = list["glossary"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["term"].as_str().unwrap())
        .collect();
    assert_eq!(terms, ["Pipeline", "Scrum"]);
    // Only the postponed one comes back.
    let again = ok(sb.mc(&["--json", "glossary", "review", &dir]));
    let left: Vec<&str> = again["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["term"].as_str().unwrap())
        .collect();
    assert_eq!(left, ["Docker"]);
    // Without a terminal and without --accept-all there is nobody to ask.
    let blind = sb.mc(&["glossary", "review", &dir]);
    assert_eq!(blind.code, 2, "{}", blind.stderr);
    // Quitting leaves the rest undecided.
    let (_, quit, _) = typed(&sb, &["glossary", "review", &dir], "q\n");
    assert!(
        quit.contains("1 suggestions: 0 added, 0 rejected, 1 left for later"),
        "{quit}"
    );
}

#[test]
fn glossary_usage_errors() {
    let sb = Sandbox::new();
    assert_eq!(sb.mc(&["glossary"]).code, 2);
    assert_eq!(sb.mc(&["glossary", "add"]).code, 2);
    assert_eq!(sb.mc(&["glossary", "list", "--alias", "x"]).code, 2);
    assert_eq!(sb.mc(&["glossary", "list", "--accept-all"]).code, 2);
    assert_eq!(sb.mc(&["glossary", "add", "   "]).code, 2);
    assert_eq!(sb.mc(&["glossary", "review", "/nonexistent"]).code, 3);
    let broken = sb.data().join("glossary.json");
    fs::create_dir_all(sb.data()).unwrap();
    fs::write(&broken, "{broken").unwrap();
    let out = sb.mc(&["glossary", "list"]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("glossary.json"), "{}", out.stderr);
}

#[test]
fn the_live_analyst_starts_with_the_open_commitments_of_the_previous_meeting() {
    let sb = Sandbox::new();
    // The previous meeting of the series, indexed, with minutes that left one commitment open.
    let prev = sb.path("earlier/daily-1");
    copy_tree(&common::fixture("synthetic_basic"), &prev);
    let manifest_path = prev.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
    manifest["session_id"] = json!("prev1");
    manifest["meeting_name"] = json!("Daily de equipo 1");
    manifest["started_wall_ms"] = json!(1_600_000_000_000_i64);
    fs::write(&manifest_path, manifest.to_string()).unwrap();
    let folder = prev.join("minutes/m1");
    fs::create_dir_all(&folder).unwrap();
    fs::write(
        folder.join("commitments.json"),
        json!([{"id": "c1", "who": "Ana", "what": "cerrar el bug", "due": null,
                "evidence": [1], "inferred": false, "source": "prev1#c1"}])
        .to_string(),
    )
    .unwrap();
    assert_eq!(sb.mc(&["import", path_str(&prev)]).code, 0);

    let source = sb.corpus_at("synthetic_basic", "fake/synthetic_basic");
    let answer = r#"{"carried":[{"source":"prev1#c1","status":"done","evidence":[1]}]}"#;
    let cfg = config(&sb, "live.json", &[answer], json!({}));
    let out = sb.mc(&[
        "--json",
        "--config",
        &cfg,
        "capture",
        "--source",
        &format!("fake:{}", path_str(&source)),
        "--poll-ms",
        "20",
        "--consent",
        "--analyst",
        "--name",
        "Daily de equipo 2",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let session = fs::read_dir(sb.data().join("corpus"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .next()
        .unwrap();
    let ledger: Value = serde_json::from_str(
        &fs::read_to_string(session.join("analysis/live/ledger.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(ledger["carried"][0]["source"], "prev1#c1");
    assert_eq!(ledger["carried"][0]["status"], "done");
}

#[test]
fn open_items_survive_a_meeting_whose_minutes_know_nothing_of_them_and_gaps_are_reported() {
    let sb = Sandbox::new();
    let (s1, s2) = (
        meeting(&sb, "s1", "Daily de equipo 21/10", 21),
        meeting(&sb, "s2", "Daily de equipo 22/10", 22),
    );
    let _ = meeting(&sb, "s3", "Daily de equipo 23/10", 23);
    let first = json!({"add": {"commitments": [commitment("Ana", "cerrar el bug", &[3])]}});
    analyze_and_minute(&sb, &s1, "one", &first, &[]);
    let (_, m2) = analyze_and_minute(&sb, &s2, "two", &json!({}), &[]);
    // Minutes written before carry-over existed have no carried.json.
    fs::remove_file(PathBuf::from(m2["path"].as_str().unwrap()).join("carried.json")).unwrap();
    let show = ok(sb.mc(&["--json", "series", "show", "daily de equipo"]));
    assert_eq!(show["open_from"], "s2");
    assert_eq!(show["open_commitments"][0]["what"], "cerrar el bug");
    let warnings = show["warnings"].to_string();
    assert!(warnings.contains("s3 has no minutes"), "{warnings}");
}

#[test]
fn an_unusable_index_only_switches_the_memory_off_with_a_warning() {
    let sb = Sandbox::new();
    let s1 = meeting(&sb, "s1", "Daily de equipo 21/10", 21);
    fs::write(sb.data().join("index.sqlite"), "this is not a database").unwrap();
    let cfg = config(&sb, "a.json", &["{}"], json!({}));
    let analysis = ok(mc(&sb, &cfg, &["analyze", &s1]));
    assert!(
        analysis["warnings"][0]
            .as_str()
            .unwrap()
            .contains("memory between meetings is off"),
        "{analysis}"
    );
}

#[test]
fn user_keys_are_normalised_like_automatic_ones() {
    let sb = Sandbox::new();
    meeting(&sb, "s1", "Retro", 21);
    let set = ok(sb.mc(&["--json", "series", "set", "s1", "Daily de equipo 21/10"]));
    assert_eq!(set["series"], "daily de equipo");
    assert_eq!(
        ok(sb.mc(&["--json", "series", "show", "Daily de equipo 99/9"]))["key"],
        "daily de equipo"
    );
}

#[test]
fn the_minutes_teach_the_memory_and_the_series_tells_a_homonym_apart() {
    let sb = Sandbox::new();
    let dir = meeting(&sb, "s1", "Rediseño del panel 21/10", 21);
    let analyze = config(&sb, "l-a.json", &[r#"{"add": {}}"#], json!({}));
    ok(mc(&sb, &analyze, &["analyze", &dir]));
    let draft = "## TL;DR\nok\n\n## Participantes\n- Luis: Product Owner\nMencionados sin asistir: el Flaco (negocio)\n\n## Glosario\n- Order: equipo de órdenes\n- RUT: Rol Único Tributario";
    let minutes = config(&sb, "l-m.json", &[draft], json!({}));
    let made = ok(mc(&sb, &minutes, &["minutes", &dir]));
    assert_eq!(
        made["memory"]["added"].as_array().unwrap().len(),
        5,
        "{made}"
    );

    let people = ok(mc(&sb, &minutes, &["memory", "list", "--kind", "person"]));
    let luis = people["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "Luis")
        .unwrap();
    assert_eq!(luis["state"], "proposed");
    assert_eq!(luis["roles"][0]["role"], "Product Owner");

    // A second meaning of "Order" in another series: the first stays tied to its own.
    ok(mc(
        &sb,
        &minutes,
        &[
            "memory",
            "add",
            "term",
            "Order",
            "--meaning",
            "orden de compra",
        ],
    ));
    let first = ok(mc(
        &sb,
        &minutes,
        &[
            "memory",
            "resolve",
            "term",
            "order",
            "--series",
            "rediseño del panel",
        ],
    ));
    assert_eq!(first["resolution"], "known", "{first}");
    let shown = ok(mc(
        &sb,
        &minutes,
        &["memory", "show", &first["id"].to_string()],
    ));
    assert_eq!(shown["entry"]["meaning"], "equipo de órdenes");
}
