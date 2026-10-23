//! The V2 policy through the CLI: `mc run --policy`, `run.policy` in the config
//! file, `MC_POLICY`, the policy recorded in the report, and `mc compare`
//! between a V1 and a V2 run.

mod common;

use std::fs;

use common::*;
use serde_json::json;

/// Copies and imports a corpus, returning its directory.
fn imported(s: &Sandbox, name: &str) -> std::path::PathBuf {
    let dir = s.corpus(name);
    assert_eq!(s.mc(&["import", path_str(&dir)]).code, 0);
    dir
}

/// Runs `mc run <dir> --json` with extra arguments and returns its run id.
fn run_id_of(s: &Sandbox, dir: &std::path::Path, extra: &[&str]) -> String {
    let mut args = vec!["run", path_str(dir)];
    args.extend_from_slice(extra);
    args.push("--json");
    let out = s.mc(&args);
    assert_eq!(out.code, 0, "{}", out.stderr);
    out.run_id()
}

#[test]
fn run_with_policy_v2_creates_a_v2_run() {
    let s = Sandbox::new();
    let dir = imported(&s, "synthetic_basic");
    let out = s.mc(&["run", path_str(&dir), "--policy", "v2", "--json"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let value = out.json();
    assert_eq!(value["action"], "created");
    assert_eq!(value["report"]["config"]["policy"], "v2");
    assert_eq!(value["report"]["comparable"], false);
    assert_eq!(value["report"]["divergences"]["content"], 0);

    let run_dir = dir.join("runs").join(out.run_id());
    assert!(run_dir.join("transcript.txt").is_file());
    assert!(run_dir.join("report.json").is_file());
}

#[test]
fn policy_flag_rejects_unknown_names_as_usage_errors() {
    let s = Sandbox::new();
    let dir = imported(&s, "synthetic_basic");
    let dir_s = path_str(&dir);
    assert_json_error(
        &s.mc(&["run", dir_s, "--policy", "bogus", "--json"]),
        2,
        "usage",
    );
    assert_eq!(s.mc(&["run", dir_s, "--policy", "bogus"]).code, 2);
    assert!(
        !dir.join("runs").exists(),
        "a rejected policy must not write a run"
    );
}

#[test]
fn run_config_policy_v2_in_the_config_file_is_honoured() {
    let s = Sandbox::new();
    let dir = imported(&s, "synthetic_basic");
    let config = s.path("config.json");
    fs::write(&config, json!({"run": {"policy": "v2"}}).to_string()).unwrap();

    let out = s.mc(&[
        "--config",
        path_str(&config),
        "run",
        path_str(&dir),
        "--json",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.json()["report"]["config"]["policy"], "v2");
}

#[test]
fn the_flag_beats_the_config_file_and_the_environment() {
    let s = Sandbox::new();
    let dir = imported(&s, "synthetic_basic");
    let config = s.path("config.json");
    fs::write(&config, json!({"run": {"policy": "v2"}}).to_string()).unwrap();
    let env = s.env_with(&[("MC_POLICY", "v2")]);

    let out = mc(
        &env,
        &[
            "--config",
            path_str(&config),
            "run",
            path_str(&dir),
            "--policy",
            "v1_legacy",
            "--json",
        ],
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.json()["report"]["config"]["policy"], "v1_legacy");
}

#[test]
fn mc_policy_env_selects_v2_and_matches_the_flag() {
    let s = Sandbox::new();
    let dir = imported(&s, "synthetic_basic");
    let env = s.env_with(&[("MC_POLICY", "v2")]);

    let out = mc(&env, &["run", path_str(&dir), "--json"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.json()["report"]["config"]["policy"], "v2");
    assert_eq!(
        out.run_id(),
        run_id_of(&s, &dir, &["--policy", "v2"]),
        "the same config gives the same run"
    );
}

#[test]
fn unknown_mc_policy_is_a_config_error() {
    let s = Sandbox::new();
    let dir = imported(&s, "synthetic_basic");
    let env = s.env_with(&[("MC_POLICY", "v9")]);
    let out = mc(&env, &["run", path_str(&dir)]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(out.stderr.contains("v9"), "{}", out.stderr);
}

#[test]
fn compare_v1_with_v2_on_history_differs_in_the_config_policy() {
    let s = Sandbox::new();
    let dir = imported(&s, "synthetic_history");
    let v1 = run_id_of(&s, &dir, &["--policy", "v1_legacy"]);
    let v2 = run_id_of(&s, &dir, &["--policy", "v2"]);
    assert_ne!(v1, v2);

    let out = s.mc(&["compare", &v1, &v2]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(out.stdout.contains("differ"), "{}", out.stdout);
    assert!(
        out.stdout
            .contains("config policy: \"v1_legacy\" vs \"v2\""),
        "{}",
        out.stdout
    );
    // The transcripts are equal (the repeat is 133.5 s after the line was last
    // seen, beyond the 120 s window), so only the policy differs.
    assert!(out.stdout.contains("segments: 96 vs 96"), "{}", out.stdout);
    assert!(
        out.stdout
            .contains("segments: 96 equal, 0 changed, 0 only in A, 0 only in B"),
        "{}",
        out.stdout
    );

    let value = s.mc(&["compare", &v1, &v2, "--json"]).json();
    assert_eq!(value["identical"], false);
}

#[test]
fn compare_v1_with_v2_on_clock_jitter_shows_fewer_v2_segments() {
    let s = Sandbox::new();
    let dir = imported(&s, "synthetic_clock_jitter");
    let v1 = run_id_of(&s, &dir, &["--policy", "v1_legacy"]);
    let v2 = run_id_of(&s, &dir, &["--policy", "v2"]);

    let out = s.mc(&["compare", &v1, &v2]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(out.stdout.contains("segments: 25 vs 19"), "{}", out.stdout);
    assert!(
        out.stdout
            .contains("config policy: \"v1_legacy\" vs \"v2\""),
        "{}",
        out.stdout
    );
}
