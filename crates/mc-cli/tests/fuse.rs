//! `mc fuse` and `mc run --fuse` over a generated fusion corpus.

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

fn json_out(out: &common::Out) -> Value {
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    serde_json::from_str(&out.stdout).unwrap()
}

#[test]
fn fusing_writes_the_files_indexes_them_and_is_idempotent() {
    let sb = Sandbox::new();
    let dir = session(&sb, "fusion_spanglish", "aaaa1111", 1);
    let glossary = format!("{dir}/glossary.txt");
    let first = json_out(&sb.mc(&[
        "--json",
        "fuse",
        &dir,
        "--glossary",
        &glossary,
        "--me",
        "Ana Pérez",
    ]));
    assert_eq!(first["reused"], false);
    let out = PathBuf::from(first["path"].as_str().unwrap());
    for file in ["fused.jsonl", "fused.txt", "fused.md", "report.json"] {
        assert!(out.join(file).is_file(), "{file}");
    }
    let text = fs::read_to_string(out.join("fused.txt")).unwrap();
    assert!(text.lines().count() >= 10, "{text}");
    assert!(text.lines().all(|l| l.starts_with('[')), "{text}");
    assert!(
        fs::read_to_string(out.join("fused.md"))
            .unwrap()
            .starts_with("# Reunión con inglés mezclado")
    );
    let report: Value =
        serde_json::from_str(&fs::read_to_string(out.join("report.json")).unwrap()).unwrap();
    assert_eq!(report["asr_id"], "aaaa1111");
    assert!(report["counts"]["edits_applied"].as_u64().unwrap() >= 5);
    assert_eq!(report["me_found"], "Ana Pérez");

    let again = json_out(&sb.mc(&[
        "--json",
        "fuse",
        &dir,
        "--glossary",
        &glossary,
        "--me",
        "Ana Pérez",
    ]));
    assert_eq!(again["reused"], true);
    assert_eq!(again["fuse_id"], first["fuse_id"]);

    // The index knows the ASR run and the fusion, with every segment.
    let store = mc_store::Store::open(sb.data().join("index.sqlite")).unwrap();
    let sid =
        serde_json::from_str::<Value>(&fs::read_to_string(format!("{dir}/manifest.json")).unwrap())
            .unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string();
    assert_eq!(store.asr_runs(&sid).unwrap(), ["aaaa1111"]);
    let fuse_id = first["fuse_id"].as_str().unwrap();
    assert_eq!(store.fusions(&sid).unwrap(), [fuse_id]);
    assert_eq!(
        store.fused_segments(fuse_id).unwrap().len(),
        text.lines().count()
    );

    let forced = json_out(&sb.mc(&[
        "--json",
        "fuse",
        &dir,
        "--glossary",
        &glossary,
        "--me",
        "Ana Pérez",
        "--force",
    ]));
    assert_eq!(forced["reused"], false);
    assert_eq!(forced["fuse_id"], first["fuse_id"]);
}

#[test]
fn the_id_follows_the_glossary_the_user_and_the_asr_run() {
    let sb = Sandbox::new();
    let dir = session(&sb, "fusion_basic", "aaaa1111", 1);
    let glossary = format!("{dir}/glossary.txt");
    let id = |args: &[&str]| {
        let mut full = vec!["--json", "fuse", dir.as_str()];
        full.extend(args);
        json_out(&sb.mc(&full))["fuse_id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let plain = id(&[]);
    let with_glossary = id(&["--glossary", &glossary]);
    let with_me = id(&["--glossary", &glossary, "--me", "Ana Pérez"]);
    assert_ne!(plain, with_glossary);
    assert_ne!(with_glossary, with_me);
    session(&sb, "fusion_basic", "bbbb2222", 2);
    assert_ne!(
        with_me,
        id(&["--glossary", &glossary, "--me", "Ana Pérez"]),
        "the newest ASR run is used"
    );
    assert_eq!(
        with_me,
        id(&[
            "--glossary",
            &glossary,
            "--me",
            "Ana Pérez",
            "--asr",
            "aaaa"
        ])
    );
}

#[test]
fn run_with_fuse_does_both_and_the_options_need_the_flag() {
    let sb = Sandbox::new();
    let dir = session(&sb, "fusion_english", "aaaa1111", 1);
    let out = sb.mc(&["--json", "run", &dir, "--fuse", "--policy", "v2"]);
    let value = json_out(&out);
    assert!(value["run_id"].as_str().unwrap().starts_with('r'));
    assert!(
        value["fusion"]["fuse_id"]
            .as_str()
            .unwrap()
            .starts_with('f')
    );
    let human = sb.mc(&["run", &dir, "--fuse"]);
    assert_eq!(human.code, 0, "{}", human.stderr);
    assert!(
        human.stdout.contains("merged with the ASR"),
        "{}",
        human.stdout
    );

    for option in ["--asr", "--me", "--glossary"] {
        let bad = sb.mc(&["run", &dir, option, "x"]);
        assert_eq!(bad.code, 2, "{option}: {}", bad.stderr);
        assert!(bad.stderr.contains("needs --fuse"), "{}", bad.stderr);
    }
}

#[test]
fn missing_inputs_say_what_to_do() {
    let sb = Sandbox::new();
    let bare = sb.path("bare");
    copy_tree(&fusion_fixture("fusion_basic"), &bare);
    let out = sb.mc(&["fuse", path_str(&bare)]);
    assert_eq!(out.code, 3);
    assert!(out.stderr.contains("mc transcribe"), "{}", out.stderr);
    for (args, code) in [(vec!["fuse"], 2), (vec!["fuse", "/nonexistent"], 3)] {
        assert_eq!(sb.mc(&args).code, code, "{args:?}");
    }
    let dir = session(&sb, "fusion_basic", "aaaa1111", 1);
    assert_eq!(sb.mc(&["fuse", &dir, "--asr", "zzzz"]).code, 3);
    assert_eq!(
        sb.mc(&["fuse", &dir, "--glossary", "/nonexistent.txt"])
            .code,
        3
    );
}

#[test]
fn status_lists_the_asr_runs_and_fusions_of_a_session() {
    let sb = Sandbox::new();
    let dir = session(&sb, "fusion_basic", "aaaa1111", 1);
    let fused = json_out(&sb.mc(&["--json", "fuse", &dir]));
    let sid =
        serde_json::from_str::<Value>(&fs::read_to_string(format!("{dir}/manifest.json")).unwrap())
            .unwrap()["session_id"]
            .as_str()
            .unwrap()
            .to_string();
    let human = sb.mc(&["status", &sid]);
    assert_eq!(human.code, 0, "{}", human.stderr);
    assert!(human.stdout.contains("asr aaaa1111"), "{}", human.stdout);
    assert!(
        human
            .stdout
            .contains(&format!("fusion {}", fused["fuse_id"].as_str().unwrap())),
        "{}",
        human.stdout
    );
    let value = json_out(&sb.mc(&["--json", "status", &sid]));
    assert_eq!(value["asr_runs"][0]["asr_id"], "aaaa1111");
    assert_eq!(value["fusions"][0]["fuse_id"], fused["fuse_id"]);
    assert_eq!(value["fusions"][0]["asr_id"], "aaaa1111");
}

#[test]
fn simultaneous_fuses_of_the_same_session_all_succeed_and_leave_one_clean_result() {
    let sb = Sandbox::new();
    let dir = session(&sb, "fusion_basic", "aaaa1111", 1);
    let glossary = format!("{dir}/glossary.txt");
    let ids: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    let out = sb.mc(&["--json", "fuse", &dir, "--glossary", &glossary, "--force"]);
                    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
                    serde_json::from_str::<Value>(&out.stdout).unwrap()["fuse_id"]
                        .as_str()
                        .unwrap()
                        .to_string()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(ids.iter().all(|id| id == &ids[0]), "{ids:?}");
    let fused = Path::new(&dir).join("fused");
    let entries: Vec<String> = fs::read_dir(&fused)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        entries,
        [ids[0].clone()],
        "no temporary folders left: {entries:?}"
    );
    for file in ["fused.jsonl", "fused.txt", "fused.md", "report.json"] {
        assert!(fused.join(&ids[0]).join(file).is_file());
    }
}

#[test]
fn the_id_follows_the_words_of_the_asr_run_not_just_its_name() {
    let sb = Sandbox::new();
    let dir = session(&sb, "fusion_basic", "aaaa1111", 1);
    let first = json_out(&sb.mc(&["--json", "fuse", &dir]))["fuse_id"]
        .as_str()
        .unwrap()
        .to_string();
    // Same asr id, different text (as after `mc transcribe --force`).
    let file = Path::new(&dir).join("asr/aaaa1111/asr.jsonl");
    let text = fs::read_to_string(&file)
        .unwrap()
        .replacen("deploy", "despliegue", 1);
    fs::write(&file, text).unwrap();
    let second = json_out(&sb.mc(&["--json", "fuse", &dir]))["fuse_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(first, second);
}

#[test]
fn recorded_paths_are_absolute() {
    let sb = Sandbox::new();
    let dir = session(&sb, "fusion_basic", "aaaa1111", 1);
    let value = json_out(&sb.mc(&["--json", "fuse", &dir]));
    assert!(Path::new(value["path"].as_str().unwrap()).is_absolute());
}
