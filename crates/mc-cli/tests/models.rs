//! `mc models` with file:// sources (nothing here touches the network).

mod common;

use std::fs;

use common::{Sandbox, path_str};
use serde_json::Value;

fn model_file(sb: &Sandbox) -> String {
    let path = sb.path("src/m.bin");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"weights weights weights").unwrap();
    format!("file://{}", path_str(&path))
}

#[test]
fn pull_list_verify_path_and_remove() {
    let sb = Sandbox::new();
    let url = model_file(&sb);

    let list = sb.mc(&["models", "list"]);
    assert_eq!(list.code, 0, "{}", list.stderr);
    assert!(
        list.stdout.contains("whisper-large-v3-turbo-q5_0"),
        "{}",
        list.stdout
    );
    assert!(list.stdout.contains("not installed"), "{}", list.stdout);

    let pull = sb.mc(&["models", "pull", "mi-modelo", "--url", &url]);
    assert_eq!(pull.code, 0, "{}\n{}", pull.stdout, pull.stderr);
    assert!(pull.stdout.contains("installed"), "{}", pull.stdout);
    assert!(
        pull.stdout.contains("no official checksum"),
        "{}",
        pull.stdout
    );

    let again = sb.mc(&["--json", "models", "pull", "mi-modelo", "--url", &url]);
    let value: Value = serde_json::from_str(&again.stdout).unwrap();
    assert_eq!(value["action"], "already_installed");

    let verify = sb.mc(&["models", "verify", "mi-modelo"]);
    assert_eq!(verify.code, 0, "{}", verify.stdout);
    let path = sb.mc(&["--json", "models", "path", "mi-modelo"]);
    let file = serde_json::from_str::<Value>(&path.stdout).unwrap()["path"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(fs::read(&file).unwrap().starts_with(b"weights"));

    fs::write(&file, b"corrupted").unwrap();
    let bad = sb.mc(&["models", "verify", "mi-modelo"]);
    assert_eq!(bad.code, 1, "{}", bad.stdout);
    assert!(bad.stdout.contains("FAIL"), "{}", bad.stdout);

    let json = sb.mc(&["--json", "models", "list"]);
    let value: Value = serde_json::from_str(&json.stdout).unwrap();
    let mine = value["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "mi-modelo")
        .unwrap();
    assert_eq!(mine["installed"], true);

    let removed = sb.mc(&["models", "remove", "mi-modelo"]);
    assert_eq!(removed.code, 0);
    assert!(removed.stdout.contains("removed"));
    assert_ne!(sb.mc(&["models", "path", "mi-modelo"]).code, 0);
}

#[test]
fn checksum_mismatch_is_a_runtime_error_and_options_are_validated() {
    let sb = Sandbox::new();
    let url = model_file(&sb);
    let wrong = "ab".repeat(32);
    let out = sb.mc(&["models", "pull", "x", "--url", &url, "--sha256", &wrong]);
    assert_eq!(out.code, 3, "{}\n{}", out.stdout, out.stderr);
    assert!(out.stderr.contains("checksum"), "{}", out.stderr);

    for args in [
        vec!["models"],
        vec!["models", "frobnicate"],
        vec!["models", "pull"],
        vec!["models", "pull", "no-such-model"],
        vec!["models", "list", "--force"],
        vec!["models", "verify", "x", "--url", "file:///x"],
        vec!["models", "pull", "../evil", "--url", &url],
    ] {
        let out = sb.mc(&args);
        assert_eq!(out.code, 2, "{args:?}: {} {}", out.code, out.stderr);
    }
}
