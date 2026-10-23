//! `mc analyze` and `mc minutes` over the fusion_basic corpus with a scripted fake model.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Sandbox, assert_json_error, copy_tree, path_str};
use mc_asr::codec::{ASR_SCHEMA, AsrMeta};
use serde_json::{Value, json};

struct Session {
    sb: Sandbox,
    dir: String,
}

fn session() -> Session {
    let sb = Sandbox::new();
    let dir = sb.path("sessions/basic");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fusion/fusion_basic"),
        &dir,
    );
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
    Session {
        dir: path_str(&dir).to_string(),
        sb,
    }
}

impl Session {
    fn fuse(&self) -> String {
        let out = self.sb.mc(&["--json", "fuse", &self.dir]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        out.json()["fuse_id"].as_str().unwrap().to_string()
    }

    /// A config file whose fake model answers with `answers`, in order.
    fn config(&self, name: &str, answers: &[&str]) -> String {
        let path = self.sb.path(name);
        let value = json!({"llm": {"provider": "fake", "args": answers}});
        fs::write(&path, value.to_string()).unwrap();
        path_str(&path).to_string()
    }

    fn mc(&self, config: &str, args: &[&str]) -> common::Out {
        let mut full = vec!["--json", "--config", config];
        full.extend(args);
        self.sb.mc(&full)
    }

    fn segments(&self, fuse_id: &str) -> usize {
        fs::read_to_string(format!("{}/fused/{fuse_id}/fused.jsonl", self.dir))
            .unwrap()
            .lines()
            .count()
    }
}

fn analysis_answer() -> String {
    json!({"add": {
        "topics": [{"title": "Bloqueos del sprint", "evidence": [0, 1]}],
        "decisions": [{"text": "Cerrar el pipeline el viernes", "evidence": [2]}],
        "commitments": [
            {"who": "Ana", "what": "cerrar el bug", "due": "el martes", "confidence": 0.9, "evidence": [3]},
            {"who": "Luis", "what": "cosa inventada", "evidence": [9999]},
        ],
        "risks": [{"text": "El bug falla en producción", "evidence": [3]}],
    }})
    .to_string()
}

fn verdicts(ids: &[(&str, &str)]) -> String {
    let list: Vec<Value> = ids
        .iter()
        .map(|(id, v)| json!({"id": id, "verdict": v, "reason": "r"}))
        .collect();
    json!({"verdicts": list}).to_string()
}

fn read(path: &Value, file: &str) -> String {
    fs::read_to_string(PathBuf::from(path.as_str().unwrap()).join(file)).unwrap()
}

#[test]
fn analyzing_writes_a_ledger_with_only_evidenced_items_and_reuses_it() {
    let s = session();
    s.fuse();
    let config = s.config("llm.json", &[&analysis_answer()]);
    let first = s.mc(&config, &["analyze", &s.dir]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    let first = first.json();
    assert_eq!(first["reused"], false);
    assert_eq!(first["source"]["kind"], "fusion");
    assert_eq!(first["windows"], 1);
    assert_eq!(first["accepted"], 4);
    assert_eq!(first["rejected"], 1);
    assert_eq!(first["counts"]["commitments"], 1);
    assert!(first["analysis_id"].as_str().unwrap().starts_with('a'));

    let ledger: Value = serde_json::from_str(&read(&first["path"], "ledger.json")).unwrap();
    assert_eq!(ledger["commitments"][0]["who"], "Ana");
    assert_eq!(ledger["commitments"][0]["evidence"], json!([3]));
    assert!(
        ledger["commitments"][0]["id"]
            .as_str()
            .unwrap()
            .starts_with('c')
    );
    let events = read(&first["path"], "ledger_events.jsonl");
    let event: Value = serde_json::from_str(events.lines().next().unwrap()).unwrap();
    assert_eq!(event["rejections"][0]["reason"], "unknown segment id 9999");
    assert_eq!(event["model"], "fake");

    // A model with no answers proves the reuse never calls it.
    let silent = s.config("silent.json", &[]);
    let again = s.mc(&silent, &["analyze", &s.dir]).json();
    assert_eq!(again["reused"], true);
    assert_eq!(again["analysis_id"], first["analysis_id"]);
    assert_eq!(again["counts"], first["counts"]);

    let forced = s.mc(&silent, &["analyze", &s.dir, "--force"]);
    assert_eq!(forced.code, 3, "a forced run needs the model");
    assert!(
        forced.err_json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("every window failed")
    );
    let forced = s.mc(&config, &["analyze", &s.dir, "--force"]).json();
    assert_eq!(forced["reused"], false);
    assert_eq!(forced["analysis_id"], first["analysis_id"]);
}

#[test]
fn the_analysis_id_follows_the_transcript_and_the_model() {
    let s = session();
    let fuse_id = s.fuse();
    let config = s.config("llm.json", &[&analysis_answer(), &analysis_answer()]);
    let by_fusion = s.mc(&config, &["analyze", &s.dir]).json();
    let other_model = s
        .mc(&config, &["analyze", &s.dir, "--llm-model", "haiku"])
        .json();
    assert_ne!(by_fusion["analysis_id"], other_model["analysis_id"]);

    // A run's segments are another transcript; a fusion can be picked by prefix.
    assert_eq!(s.sb.mc(&["run", &s.dir]).code, 0);
    let config = s.config("llm3.json", &[&analysis_answer()]);
    let by_run = s.mc(&config, &["analyze", &s.dir, "--run", "r"]).json();
    assert_eq!(by_run["source"]["kind"], "run");
    assert_ne!(by_run["analysis_id"], by_fusion["analysis_id"]);
    let picked = s
        .mc(&config, &["analyze", &s.dir, "--fuse", &fuse_id[..4]])
        .json();
    assert_eq!(picked["analysis_id"], by_fusion["analysis_id"]);
}

#[test]
fn minutes_remove_unsupported_items_and_write_commitments_for_tisty() {
    let s = session();
    let fuse_id = s.fuse();
    let n = s.segments(&fuse_id);
    assert!(n > 4, "{n} fused segments");
    // The analysis ids are deterministic, so the verdicts can name them.
    let probe = s.config("probe.json", &[&analysis_answer()]);
    let analysis = s.mc(&probe, &["analyze", &s.dir]).json();
    let ledger: Value = serde_json::from_str(&read(&analysis["path"], "ledger.json")).unwrap();
    let decision = ledger["decisions"][0]["id"].as_str().unwrap();
    let commitment = ledger["commitments"][0]["id"].as_str().unwrap();

    let config = s.config(
        "minutes.json",
        &[
            &verdicts(&[(decision, "unsupported"), (commitment, "inferred")]),
            "## Resumen\nHubo bloqueos [#0] y un bug [#424242].\n\n## Temas\n- Sprint [#1]",
        ],
    );
    let out = s.mc(&config, &["minutes", &s.dir]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let out = out.json();
    assert_eq!(out["reused"], false);
    assert_eq!(out["analysis_id"], analysis["analysis_id"]);
    assert_eq!(out["removed"], json!([decision]));
    let md = read(&out["path"], "minutes.md");
    assert!(
        md.starts_with("---\ndate: 2023-11-14\ntitle: \"Daily de equipo\""),
        "{md}"
    );
    assert!(md.contains("participants: [\"Luis\""), "{md}");
    assert!(md.contains("Hubo bloqueos [#0] y un bug."), "{md}");
    assert!(!md.contains("pipeline el viernes"), "{md}");
    assert!(
        md.contains("| Ana | cerrar el bug (inferido) [#3] | el martes |"),
        "{md}"
    );
    let commitments: Vec<Value> =
        serde_json::from_str(&read(&out["path"], "commitments.json")).unwrap();
    assert_eq!(commitments.len(), 1);
    assert_eq!(commitments[0]["inferred"], true);
    let session_id = md
        .lines()
        .find_map(|l| l.strip_prefix("session: "))
        .unwrap()
        .trim_matches('"');
    assert_eq!(
        commitments[0]["source"],
        format!("{session_id}#{commitment}")
    );
    let verify: Value = serde_json::from_str(&read(&out["path"], "verify.json")).unwrap();
    assert_eq!(verify["removed"], json!([decision]));

    // Reused without calling the model; --force needs it.
    let silent = s.config("silent.json", &[]);
    let again = s.mc(&silent, &["minutes", &s.dir]).json();
    assert_eq!(again["reused"], true);
    assert_eq!(again["minutes_id"], out["minutes_id"]);
    assert_eq!(s.mc(&silent, &["minutes", &s.dir, "--force"]).code, 3);

    let prefix = &analysis["analysis_id"].as_str().unwrap()[..5];
    let by_id = s.mc(&silent, &["minutes", &s.dir, "--analysis", prefix]);
    assert_eq!(by_id.json()["minutes_id"], out["minutes_id"]);
}

#[test]
fn minutes_make_the_analysis_when_there_is_none() {
    let s = session();
    s.fuse();
    let config = s.config(
        "all.json",
        &[&analysis_answer(), &verdicts(&[]), "## Resumen\nok"],
    );
    let out = s.mc(&config, &["minutes", &s.dir]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let out = out.json();
    // Items the verifier did not mention stay, marked as inferred.
    assert_eq!(out["inferred_commitments"], 1);
    assert!(
        PathBuf::from(&s.dir)
            .join("analysis")
            .join(out["analysis_id"].as_str().unwrap())
            .is_dir()
    );
}

#[test]
fn a_failing_model_is_a_runtime_error_and_bad_usage_is_a_usage_error() {
    let s = session();
    assert_json_error(
        &s.mc(&s.config("none.json", &[]), &["analyze", &s.dir]),
        3,
        "not_found",
    );
    s.fuse();
    assert_json_error(
        &s.mc(
            &s.config("bad.json", &["no json", "tampoco"]),
            &["analyze", &s.dir],
        ),
        3,
        "llm",
    );
    let config = s.config("ok.json", &[]);
    assert_json_error(&s.mc(&config, &["analyze"]), 2, "usage");
    assert_json_error(
        &s.mc(&config, &["analyze", &s.dir, "--run", "r", "--fuse", "f"]),
        2,
        "usage",
    );
    assert_json_error(
        &s.mc(&config, &["minutes", &s.dir, "--analysis", "zzz"]),
        3,
        "not_found",
    );
    assert_json_error(
        &s.mc(&config, &["analyze", &s.dir, "--llm-provider", "nope"]),
        2,
        "config",
    );
}

#[test]
fn help_lists_both_commands_and_human_output_is_short() {
    let s = session();
    let help = s.sb.mc(&["--help"]);
    assert!(help.stdout.contains("analyze <session>") && help.stdout.contains("minutes <session>"));
    s.fuse();
    let config = s.config("h.json", &[&analysis_answer()]);
    let out = s.sb.mc(&["--config", &config, "analyze", &s.dir]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stdout.lines().count() <= 3, "{}", out.stdout);
    assert!(out.stdout.contains("1 commitments"), "{}", out.stdout);
}

fn fused_file(s: &Session, fuse_id: &str) -> PathBuf {
    PathBuf::from(&s.dir).join(format!("fused/{fuse_id}/fused.jsonl"))
}

#[test]
fn a_regenerated_transcript_with_the_same_id_does_not_reuse_the_old_ledger() {
    let s = session();
    let fuse_id = s.fuse();
    let config = s.config("llm.json", &[&analysis_answer()]);
    let first = s.mc(&config, &["analyze", &s.dir]).json();
    let silent = s.config("silent.json", &[]);
    assert_eq!(s.mc(&silent, &["analyze", &s.dir]).json()["reused"], true);

    let file = fused_file(&s, &fuse_id);
    let text = fs::read_to_string(&file).unwrap();
    fs::write(&file, text.replacen("bloqueo", "bloqueito", 1)).unwrap();
    assert_eq!(
        s.mc(&silent, &["analyze", &s.dir]).code,
        3,
        "must call the model"
    );
    let second = s.mc(&config, &["analyze", &s.dir]).json();
    assert_eq!(second["reused"], false);
    assert_ne!(second["analysis_id"], first["analysis_id"]);
}

#[test]
fn minutes_reject_an_analysis_of_another_transcript_and_follow_its_source() {
    let s = session();
    s.fuse();
    assert_eq!(s.sb.mc(&["run", &s.dir]).code, 0);
    // A run has more segments than one window: the extra windows get empty patches.
    let answers = [
        analysis_answer(),
        "{}".into(),
        "{}".into(),
        "{}".into(),
        "{}".into(),
    ];
    let answers: Vec<&str> = answers.iter().map(String::as_str).collect();
    let config = s.config("llm.json", &answers);
    let by_run = s.mc(&config, &["analyze", &s.dir, "--run", "r"]).json();
    let id = by_run["analysis_id"].as_str().unwrap();
    let meta: Value = serde_json::from_str(&read(&by_run["path"], "meta.json")).unwrap();
    assert_eq!(meta["complete"], true);
    assert!(meta["source"].as_str().unwrap().starts_with("run:"));

    let mismatch = s.mc(
        &config,
        &["minutes", &s.dir, "--fuse", "f", "--analysis", id],
    );
    assert_json_error(&mismatch, 2, "usage");
    assert!(
        mismatch.err_json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("another transcript")
    );

    // Without --run/--fuse the transcript is the one the analysis names, not the newest fusion.
    let minutes = s.mc(
        &s.config("m.json", &[&verdicts(&[]), "## Resumen\nok"]),
        &["minutes", &s.dir, "--analysis", id],
    );
    assert_eq!(minutes.code, 0, "{}", minutes.stderr);
    assert_eq!(minutes.json()["source"]["kind"], "run");
}

#[test]
fn an_incomplete_ledger_is_reported_and_redone_and_a_failed_force_keeps_the_good_result() {
    let s = session();
    let fuse_id = s.fuse();
    // 70 distinct segments (repeated lines are collapsed before the analysis): two offline windows.
    let file = fused_file(&s, &fuse_id);
    let lines: Vec<String> = fs::read_to_string(&file)
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    let many: String = (0..70)
        .map(|i| {
            let mut line: Value = serde_json::from_str(&lines[i % lines.len()]).unwrap();
            let text = format!("{} (parte {i})", line["text"].as_str().unwrap());
            line["text"] = json!(text);
            format!("{line}\n")
        })
        .collect();
    fs::write(&file, many).unwrap();

    let broken = s.config("broken.json", &[&analysis_answer(), "no json", "tampoco"]);
    let partial = s.mc(&broken, &["analyze", &s.dir]).json();
    assert_eq!(
        (
            partial["windows"].clone(),
            partial["failed_windows"].clone()
        ),
        (json!(2), json!(1))
    );
    assert_eq!(partial["complete"], false);
    let human =
        s.sb.mc(&["--config", &broken, "analyze", &s.dir, "--force"]);
    assert!(
        human.stdout.contains("warning: incomplete"),
        "{}",
        human.stdout
    );

    // Not reused: with no answers it must call the model, and fails.
    let silent = s.config("silent.json", &[]);
    assert_eq!(s.mc(&silent, &["analyze", &s.dir]).code, 3);
    let partial_path = PathBuf::from(partial["path"].as_str().unwrap());
    assert!(
        partial_path.join("ledger.json").is_file(),
        "the partial result stays"
    );

    let good = s.config("good.json", &[&analysis_answer(), "{}"]);
    let complete = s.mc(&good, &["analyze", &s.dir]).json();
    assert_eq!(
        (complete["complete"].clone(), complete["reused"].clone()),
        (json!(true), json!(false))
    );
    assert_eq!(s.mc(&silent, &["analyze", &s.dir]).json()["reused"], true);

    // A forced run that fails leaves the good result and no temporary folders.
    assert_eq!(s.mc(&silent, &["analyze", &s.dir, "--force"]).code, 3);
    assert!(partial_path.join("ledger.json").is_file());
    let leftovers: Vec<_> = fs::read_dir(partial_path.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}
