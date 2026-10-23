//! `mc prune --audio`: dry run by default, keeps unfused sessions, leaves transcripts.

mod common;

use std::fs;

use common::{Sandbox, path_str};
use serde_json::Value;

fn with_audio(sb: &Sandbox, fused: bool) -> std::path::PathBuf {
    let dir = sb.path("sessions/basic");
    common::copy_tree(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/fusion/fusion_basic"),
        &dir,
    );
    fs::write(dir.join("mic.wav"), vec![0u8; 4096]).unwrap();
    fs::write(dir.join("system.wav"), vec![0u8; 2048]).unwrap();
    fs::write(dir.join("audio_sync.jsonl"), "{}\n").unwrap();
    if fused {
        let run = dir.join("fused/f1");
        fs::create_dir_all(&run).unwrap();
        fs::write(run.join("fused.jsonl"), "{}\n").unwrap();
    }
    // Old files: recent writes mean a live recorder.
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 3600);
    for name in ["mic.wav", "system.wav"] {
        fs::File::options()
            .write(true)
            .open(dir.join(name))
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    assert_eq!(sb.mc(&["import", path_str(&dir)]).code, 0);
    dir
}

fn edit_manifest(dir: &std::path::Path, f: impl FnOnce(&mut Value)) {
    let path = dir.join("manifest.json");
    let mut m: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    f(&mut m);
    fs::write(path, m.to_string()).unwrap();
}

fn json(out: &common::Out) -> Value {
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    serde_json::from_str(&out.stdout).unwrap()
}

#[test]
fn a_dry_run_changes_nothing_and_apply_removes_only_the_recordings() {
    let sb = Sandbox::new();
    let dir = with_audio(&sb, true);
    let dry = json(&sb.mc(&["--json", "prune", "--audio", "--older-than", "0"]));
    assert_eq!(dry["applied"], false);
    assert_eq!(dry["files"], 2);
    assert_eq!(dry["bytes"], 6144);
    assert!(dir.join("mic.wav").is_file());

    let done = json(&sb.mc(&["--json", "prune", "--audio", "--older-than", "0", "--apply"]));
    assert_eq!(done["files"], 2);
    assert!(!dir.join("mic.wav").exists() && !dir.join("system.wav").exists());
    assert!(dir.join("audio_sync.jsonl").is_file());
    assert!(dir.join("captions.jsonl").is_file());
    let manifest: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["audio"]["pruned"]["bytes"], 6144);

    let again = json(&sb.mc(&["--json", "prune", "--audio", "--older-than", "0", "--apply"]));
    assert_eq!(again["files"], 0);
}

#[test]
fn sessions_without_a_fused_transcript_or_too_recent_are_kept() {
    let sb = Sandbox::new();
    let dir = with_audio(&sb, false);
    let out = json(&sb.mc(&["--json", "prune", "--audio", "--older-than", "0", "--apply"]));
    assert_eq!(out["files"], 0);
    assert!(out["sessions"][0]["kept_because"].is_string());
    assert!(dir.join("mic.wav").is_file());

    let forced = json(&sb.mc(&[
        "--json",
        "prune",
        "--audio",
        "--older-than",
        "0",
        "--include-unfused",
        "--apply",
    ]));
    assert_eq!(forced["files"], 2);
}

#[test]
fn the_options_are_validated() {
    let sb = Sandbox::new();
    assert_eq!(sb.mc(&["prune"]).code, 2);
    assert_eq!(sb.mc(&["prune", "--audio"]).code, 2);
    assert_eq!(sb.mc(&["prune", "--audio", "--older-than", "x"]).code, 2);
}

#[test]
fn recent_unended_undated_and_empty_fusions_are_kept() {
    let prune = |sb: &Sandbox, extra: &[&str]| {
        let mut args = vec!["--json", "prune", "--audio", "--apply"];
        args.extend_from_slice(extra);
        json(&sb.mc(&args))
    };
    // Too recent for the age asked.
    let sb = Sandbox::new();
    let dir = with_audio(&sb, true);
    let out = prune(&sb, &["--older-than", "100000"]);
    assert_eq!(out["sessions"][0]["kept_because"], "too recent");
    assert!(dir.join("mic.wav").is_file());

    // Not ended (live or crashed): kept unless asked.
    let sb = Sandbox::new();
    let dir = with_audio(&sb, true);
    edit_manifest(&dir, |m| m["ended_wall_ms"] = Value::Null);
    let out = prune(&sb, &["--older-than", "0"]);
    assert_eq!(out["files"], 0);
    assert!(dir.join("mic.wav").is_file());
    assert_eq!(
        prune(&sb, &["--older-than", "0", "--include-unended"])["files"],
        2
    );

    // Undated.
    let sb = Sandbox::new();
    let dir = with_audio(&sb, true);
    edit_manifest(&dir, |m| {
        m["started_wall_ms"] = Value::Null;
        m["ended_wall_ms"] = Value::Null;
    });
    assert_eq!(
        prune(&sb, &["--older-than", "0", "--include-unended"])["files"],
        0
    );

    // An empty fused file is not a fusion.
    let sb = Sandbox::new();
    let dir = with_audio(&sb, true);
    fs::write(dir.join("fused/f1/fused.jsonl"), "").unwrap();
    assert_eq!(prune(&sb, &["--older-than", "0"])["files"], 0);

    // Audio written minutes ago belongs to a live recorder.
    let sb = Sandbox::new();
    let dir = with_audio(&sb, true);
    fs::write(dir.join("mic.wav"), vec![0u8; 4096]).unwrap();
    let out = prune(&sb, &["--older-than", "0"]);
    assert_eq!(out["files"], 0);
    assert!(dir.join("mic.wav").is_file());
}

#[test]
fn a_directory_that_holds_another_session_is_left_alone() {
    let sb = Sandbox::new();
    let dir = with_audio(&sb, true);
    edit_manifest(&dir, |m| m["session_id"] = "someone-else".into());
    let out = json(&sb.mc(&["--json", "prune", "--audio", "--older-than", "0", "--apply"]));
    assert_eq!(out["files"], 0);
    assert!(dir.join("mic.wav").is_file());
}

#[test]
fn dry_run_lists_would_remove_and_removed_stays_empty() {
    let sb = Sandbox::new();
    with_audio(&sb, true);
    let out = json(&sb.mc(&["--json", "prune", "--audio", "--older-than", "0"]));
    assert_eq!(out["sessions"][0]["removed"].as_array().unwrap().len(), 0);
    assert_eq!(
        out["sessions"][0]["would_remove"].as_array().unwrap().len(),
        2
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_recording_is_never_followed_or_removed() {
    let sb = Sandbox::new();
    let dir = with_audio(&sb, true);
    let outside = sb.path("outside.wav");
    fs::write(&outside, b"keep me").unwrap();
    fs::remove_file(dir.join("mic.wav")).unwrap();
    std::os::unix::fs::symlink(&outside, dir.join("mic.wav")).unwrap();
    let out = json(&sb.mc(&["--json", "prune", "--audio", "--older-than", "0", "--apply"]));
    assert_eq!(out["files"], 1);
    assert!(outside.is_file());
    assert!(fs::symlink_metadata(dir.join("mic.wav")).is_ok());
}

#[test]
fn repeated_prunes_merge_the_manifest_trace() {
    let sb = Sandbox::new();
    let dir = with_audio(&sb, true);
    fs::remove_file(dir.join("system.wav")).unwrap();
    json(&sb.mc(&["--json", "prune", "--audio", "--older-than", "0", "--apply"]));
    fs::write(dir.join("system.wav"), vec![0u8; 100]).unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 3600);
    fs::File::options()
        .write(true)
        .open(dir.join("system.wav"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    json(&sb.mc(&["--json", "prune", "--audio", "--older-than", "0", "--apply"]));
    let m: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(m["audio"]["pruned"]["bytes"], 4196);
    assert_eq!(m["audio"]["pruned"]["files"].as_array().unwrap().len(), 2);
}
