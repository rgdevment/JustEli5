//! `mc eval` over a generated fusion corpus with its ASR fixture installed as a run.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Sandbox, copy_tree, path_str};
use mc_asr::codec::{ASR_SCHEMA, AsrMeta};
use serde_json::{Value, json};

fn fusion_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/fusion")
        .join(name)
}

/// The corpus copied into the sandbox, its ASR fixture installed as run `id`.
fn session(sb: &Sandbox, name: &str, id: &str, created: i64) -> String {
    let dir = sb.path(&format!("sessions/{name}"));
    if !dir.exists() {
        copy_tree(&fusion_fixture(name), &dir);
    }
    let run = dir.join("asr").join(id);
    fs::create_dir_all(&run).unwrap();
    fs::copy(dir.join("asr_fixture.jsonl"), run.join("asr.jsonl")).unwrap();
    let meta = AsrMeta {
        v: ASR_SCHEMA,
        asr_id: id.to_string(),
        engine: "fixture".into(),
        model: "fixture".into(),
        model_sha256: None,
        params: json!(null),
        language: None,
        prompt: None,
        segmenter: json!(null),
        channels: vec![],
        segments: 0,
        created_wall_ms: created,
    };
    fs::write(run.join("meta.json"), serde_json::to_string(&meta).unwrap()).unwrap();
    path_str(&dir).to_string()
}

fn metrics(value: &Value, variant: &str, key: &str) -> f64 {
    value["variants"][variant][key]
        .as_f64()
        .unwrap_or_else(|| panic!("{variant}.{key}"))
}

#[test]
fn fusing_scores_better_than_either_source_alone_on_english_terms() {
    let sb = Sandbox::new();
    let dir = session(&sb, "fusion_spanglish", "aaaa1111", 1);
    let truth = format!("{dir}/truth.json");
    let glossary = format!("{dir}/glossary.txt");
    let out = sb.mc(&[
        "--json",
        "eval",
        &dir,
        "--reference",
        &truth,
        "--glossary",
        &glossary,
    ]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    let value: Value = serde_json::from_str(&out.stdout).unwrap();
    assert!(metrics(&value, "fused", "wer") < metrics(&value, "captions", "wer"));
    assert!(metrics(&value, "fused", "term_recall") > metrics(&value, "captions", "term_recall"));
    assert!(metrics(&value, "fused", "speaker_accuracy") >= 0.9);
    // The ASR alone cannot name the speakers.
    assert!(value["variants"]["asr"]["speaker_accuracy"].is_null());
    assert!(metrics(&value, "asr", "term_recall") >= 0.8);
    let lag = value["lag_ms"].as_f64().unwrap();
    assert!((lag - 1200.0).abs() < 500.0, "{lag}");

    let table = sb.mc(&["eval", &dir, "--reference", &truth, "--glossary", &glossary]);
    assert_eq!(table.code, 0, "{}", table.stderr);
    for word in ["variant", "captions", "asr", "fused", "WER", "speaker"] {
        assert!(table.stdout.contains(word), "{}", table.stdout);
    }
}

#[test]
fn variants_and_the_asr_run_can_be_chosen() {
    let sb = Sandbox::new();
    let dir = session(&sb, "fusion_basic", "aaaa1111", 1);
    session(&sb, "fusion_basic", "bbbb2222", 2);
    let truth = format!("{dir}/truth.json");
    let only = sb.mc(&[
        "--json",
        "eval",
        &dir,
        "--reference",
        &truth,
        "--variants",
        "fused",
        "--asr",
        "aaaa",
    ]);
    assert_eq!(only.code, 0, "{}", only.stderr);
    let value: Value = serde_json::from_str(&only.stdout).unwrap();
    assert_eq!(value["asr_id"], "aaaa1111");
    assert_eq!(value["variants"].as_object().unwrap().len(), 1);
    // The newest run is the default.
    let newest: Value = serde_json::from_str(
        &sb.mc(&["--json", "eval", &dir, "--reference", &truth])
            .stdout,
    )
    .unwrap();
    assert_eq!(newest["asr_id"], "bbbb2222");
}

#[test]
fn bad_input_is_reported() {
    let sb = Sandbox::new();
    let dir = session(&sb, "fusion_basic", "aaaa1111", 1);
    let truth = format!("{dir}/truth.json");
    for (args, code) in [
        (vec!["eval"], 2),
        (vec!["eval", &dir], 2),
        (
            vec!["eval", &dir, "--reference", &truth, "--variants", "magic"],
            2,
        ),
        (
            vec!["eval", &dir, "--reference", &truth, "--asr", "zzzz"],
            3,
        ),
        (vec!["eval", &dir, "--reference", "/nonexistent.json"], 3),
        (vec!["eval", "/nonexistent", "--reference", &truth], 3),
    ] {
        let out = sb.mc(&args);
        assert_eq!(out.code, code, "{args:?}: {}", out.stderr);
    }
    let bare = sb.path("bare");
    copy_tree(&fusion_fixture("fusion_basic"), &bare);
    let out = sb.mc(&["eval", path_str(&bare), "--reference", &truth]);
    assert_eq!(out.code, 3);
    assert!(out.stderr.contains("mc transcribe"), "{}", out.stderr);
}

#[test]
fn nothing_to_score_is_a_negative_result_and_repeated_variants_collapse() {
    let sb = Sandbox::new();
    let dir = session(&sb, "fusion_basic", "aaaa1111", 1);
    let empty = sb.path("empty.json");
    fs::write(&empty, r#"{"utterances": []}"#).unwrap();
    let out = sb.mc(&["eval", &dir, "--reference", path_str(&empty)]);
    assert_eq!(out.code, 1, "{}\n{}", out.stdout, out.stderr);

    let truth = format!("{dir}/truth.json");
    let twice = sb.mc(&[
        "--json",
        "eval",
        &dir,
        "--reference",
        &truth,
        "--variants",
        "fused,fused",
    ]);
    assert_eq!(twice.code, 0, "{}", twice.stderr);
    let value: Value = serde_json::from_str(&twice.stdout).unwrap();
    assert_eq!(value["variants"].as_object().unwrap().len(), 1);
}

#[test]
fn a_term_only_the_reference_knows_is_scored_but_not_fixed() {
    let sb = Sandbox::new();
    let dir = session(&sb, "fusion_spanglish", "aaaa1111", 1);
    let truth = format!("{dir}/truth.json");
    // No glossary for the fusion: nothing gets corrected, yet the reference's
    // own glossary still decides what is scored.
    let out = sb.mc(&["--json", "eval", &dir, "--reference", &truth]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let value: Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(
        metrics(&value, "fused", "term_recall"),
        metrics(&value, "captions", "term_recall"),
        "without a glossary the fusion cannot improve the terms"
    );
    assert!(metrics(&value, "asr", "term_recall") > metrics(&value, "fused", "term_recall"));
}
