//! `mc capture`: consent, the source and option rules, the output contract,
//! config and environment, and a captured session that imports and runs.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Sandbox, copy_tree, fixture, path_str};
use serde_json::Value;

/// A fake source copied into the sandbox (the fixture itself is never touched).
fn fake(sb: &Sandbox, name: &str) -> String {
    let dir = sb.corpus_at(name, &format!("fake/{name}"));
    format!("fake:{}", path_str(&dir))
}

/// A fake corpus of `frames` frames of one speaker saying the same thing: the
/// source runs long enough to stop it part way through.
fn long_fake(sb: &Sandbox, frames: u64) -> String {
    let dir = sb.path("fake/long");
    copy_tree(&fixture("synthetic_basic"), &dir);
    let mut captions = String::new();
    for frame in 1..=frames {
        let decision = if frame == 1 { "new_open" } else { "dup_exact" };
        let mono = 400.0 + (frame as f64) * 20.0;
        captions.push_str(&format!(
            "{{\"v\": 2, \"seq\": {frame}, \"frame\": {frame}, \"t\": {t}, \"wall_ms\": {wall}, \"mono_ms\": {mono:.1}, \"read_ms\": 5.0, \"speaker\": \"Ana\", \"text\": \"Hola a todos\", \"decision\": \"{decision}\"}}\n",
            t = (1_700_000_000_000 + mono as i64) as f64 / 1000.0,
            wall = 1_700_000_000_000i64 + mono as i64,
        ));
    }
    fs::write(dir.join("captions.jsonl"), captions).unwrap();
    format!("fake:{}", path_str(&dir))
}

/// Session folders under `MC_DATA_DIR/corpus`.
fn sessions(sb: &Sandbox) -> Vec<PathBuf> {
    let root = sb.data().join("corpus");
    let Ok(entries) = fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    dirs.sort();
    dirs
}

fn manifest(dir: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap()
}

fn ndjson(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
        .collect()
}

#[test]
fn without_consent_and_without_a_terminal_it_is_refused_and_nothing_is_created() {
    let sb = Sandbox::new();
    let source = fake(&sb, "synthetic_basic");
    let out = sb.mc(&["capture", "--source", &source, "--poll-ms", "20"]);
    assert_eq!(out.code, 2, "stderr: {}", out.stderr);
    assert!(out.stderr.contains("consent"), "{}", out.stderr);
    assert!(sessions(&sb).is_empty());
    assert!(!sb.data().join("corpus").exists());
}

#[test]
fn no_record_shows_segments_and_creates_no_folder() {
    let sb = Sandbox::new();
    let source = fake(&sb, "synthetic_basic");
    let out = sb.mc(&[
        "capture",
        "--source",
        &source,
        "--poll-ms",
        "20",
        "--no-record",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("live view"), "{}", out.stdout);
    assert!(out.stdout.contains("]: "), "no segments in: {}", out.stdout);
    assert!(sessions(&sb).is_empty());
}

#[test]
fn consent_records_a_session_that_replays_strictly() {
    let sb = Sandbox::new();
    let source = fake(&sb, "synthetic_basic");
    let out = sb.mc(&[
        "capture",
        "--source",
        &source,
        "--poll-ms",
        "20",
        "--consent",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("waiting for the meeting (FakeSource)"),
        "{}",
        out.stdout
    );

    let found = sessions(&sb);
    assert_eq!(found.len(), 1, "{found:?}");
    let dir = &found[0];
    let m = manifest(dir);
    assert!(!m["ended_wall_ms"].is_null(), "closed");
    assert_eq!(m["end_reason"], "source_ended");
    assert_eq!(m["platform_requested"], "fake");
    assert_eq!(m["consent"]["method"], "flag");
    assert_eq!(m["captions"]["debounce_clock"], "mono");

    let replay = sb.mc(&["replay", path_str(dir), "--verify", "--strict"]);
    assert_eq!(
        replay.code, 0,
        "replay: {}\n{}",
        replay.stdout, replay.stderr
    );
}

#[test]
fn json_output_is_only_ndjson_with_event_and_mono_ms_ending_with_ended() {
    let sb = Sandbox::new();
    let source = fake(&sb, "synthetic_basic");
    let out = sb.mc(&[
        "--json",
        "capture",
        "--source",
        &source,
        "--poll-ms",
        "20",
        "--consent",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let events = ndjson(&out.stdout);
    assert!(events.len() >= 2, "{}", out.stdout);
    for event in &events {
        assert!(event["event"].is_string(), "{event}");
        assert!(event["mono_ms"].is_number(), "{event}");
    }
    assert_eq!(events[0]["event"], "started");
    let last = events.last().unwrap();
    assert_eq!(last["event"], "ended");
    assert_eq!(last["reason"], "source_ended");
    assert!(events.iter().any(|e| e["event"] == "segment"));
}

#[test]
fn unknown_source_is_a_usage_error() {
    let sb = Sandbox::new();
    let out = sb.mc(&["capture", "--source", "pigeon", "--consent"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    let out = sb.mc(&["capture", "--source", "fake:", "--consent"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(sessions(&sb).is_empty());
}

/// Without a native backend the live sources are a usage error naming the fake source; elsewhere they
/// attach to real windows (and would wait for a meeting forever), so they are not exercised here.
#[cfg(not(any(windows, target_os = "macos")))]
#[test]
fn teams_and_zoom_need_windows_ui_automation_elsewhere() {
    let sb = Sandbox::new();
    for source in ["teams", "zoom"] {
        let out = sb.mc(&["capture", "--source", source, "--consent"]);
        assert_eq!(out.code, 2, "{source}: {}", out.stderr);
        assert!(
            out.stderr.contains("needs Windows UI Automation"),
            "{}",
            out.stderr
        );
        assert!(out.stderr.contains("fake-uia"), "{}", out.stderr);
    }
    assert!(sessions(&sb).is_empty());
}

#[test]
fn a_missing_source_is_a_usage_error() {
    let sb = Sandbox::new();
    let out = sb.mc(&["capture", "--consent"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(out.stderr.contains("--source"), "{}", out.stderr);
}

#[test]
fn poll_ms_outside_the_range_is_a_usage_error() {
    let sb = Sandbox::new();
    let source = fake(&sb, "synthetic_basic");
    for poll in ["5", "5001", "fast"] {
        let out = sb.mc(&[
            "capture",
            "--source",
            &source,
            "--consent",
            "--poll-ms",
            poll,
        ]);
        assert_eq!(out.code, 2, "--poll-ms {poll}: {}", out.stderr);
    }
    assert!(sessions(&sb).is_empty());
}

#[test]
fn consent_and_no_record_are_exclusive() {
    let sb = Sandbox::new();
    let source = fake(&sb, "synthetic_basic");
    let out = sb.mc(&["capture", "--source", &source, "--consent", "--no-record"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(sessions(&sb).is_empty());
}

#[test]
fn until_zero_ends_early_with_reason_stop() {
    let sb = Sandbox::new();
    let source = long_fake(&sb, 400);
    let out = sb.mc(&[
        "--json",
        "capture",
        "--source",
        &source,
        "--poll-ms",
        "20",
        "--consent",
        "--until",
        "0",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);

    let events = ndjson(&out.stdout);
    let last = events.last().unwrap();
    assert_eq!(last["event"], "ended");
    assert_eq!(last["reason"], "stop");
    assert!(
        last["frames"].as_u64().unwrap() < 400,
        "the fake was not cut short: {last}"
    );

    // The session exists only if the meeting was found before the stop landed.
    let found = sessions(&sb);
    assert!(found.len() <= 1, "{found:?}");
    for dir in &found {
        assert_eq!(manifest(dir)["end_reason"], "stop");
        assert!(!manifest(dir)["ended_wall_ms"].is_null());
    }
}

#[test]
fn a_config_file_supplies_the_source() {
    let sb = Sandbox::new();
    let source = fake(&sb, "synthetic_basic");
    let config = sb.path("config.json");
    fs::write(
        &config,
        serde_json::json!({"capture": {"source": source, "poll_ms": 20}}).to_string(),
    )
    .unwrap();
    let out = sb.mc(&["--config", path_str(&config), "capture", "--consent"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert_eq!(sessions(&sb).len(), 1);
}

#[test]
fn the_source_can_come_from_the_environment() {
    let sb = Sandbox::new();
    let source = fake(&sb, "synthetic_basic");
    let env = sb.env_with(&[("MC_CAPTURE_SOURCE", source.as_str()), ("MC_POLL_MS", "20")]);
    let out = common::mc(&env, &["capture", "--consent"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert_eq!(sessions(&sb).len(), 1);
}

#[test]
fn an_invalid_poll_setting_in_the_environment_is_a_config_error() {
    let sb = Sandbox::new();
    let source = fake(&sb, "synthetic_basic");
    let env = sb.env_with(&[
        ("MC_CAPTURE_SOURCE", source.as_str()),
        ("MC_POLL_MS", "abc"),
    ]);
    let out = common::mc(&env, &["capture", "--consent"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(sessions(&sb).is_empty());

    let env = sb.env_with(&[("MC_CAPTURE_SOURCE", source.as_str()), ("MC_POLL_MS", "1")]);
    let out = common::mc(&env, &["capture", "--consent"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
}

#[test]
fn an_unclean_session_is_reported_before_recording() {
    let sb = Sandbox::new();
    let source = fake(&sb, "synthetic_basic");
    // A killed session left in the corpus folder.
    let stale = sb.data().join("corpus").join("20231114T221320Z_teams");
    copy_tree(&fixture("synthetic_unclean_end"), &stale);
    common::age_files(&stale);

    let out = sb.mc(&[
        "capture",
        "--source",
        &source,
        "--poll-ms",
        "20",
        "--consent",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("did not end cleanly"),
        "stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("20231114T221320Z_teams"),
        "{}",
        out.stderr
    );
    // The stale session was reported, not closed or rewritten.
    assert!(manifest(&stale)["ended_wall_ms"].is_null());
}

#[test]
fn an_unclean_session_comes_before_the_start_in_json() {
    let sb = Sandbox::new();
    let source = fake(&sb, "synthetic_basic");
    let stale = sb.data().join("corpus").join("20231114T221320Z_teams");
    copy_tree(&fixture("synthetic_unclean_end"), &stale);
    common::age_files(&stale);

    let out = sb.mc(&[
        "--json",
        "capture",
        "--source",
        &source,
        "--poll-ms",
        "20",
        "--consent",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let events = ndjson(&out.stdout);
    let names: Vec<&str> = events
        .iter()
        .map(|e| e["event"].as_str().unwrap())
        .collect();
    assert_eq!(names.first(), Some(&"unclean_session"), "{names:?}");
    let started = names.iter().position(|n| *n == "started").unwrap();
    assert!(names[..started].iter().all(|n| *n == "unclean_session"));
}

#[test]
fn a_captured_session_imports_and_runs() {
    let sb = Sandbox::new();
    let source = fake(&sb, "synthetic_basic");
    let out = sb.mc(&[
        "capture",
        "--source",
        &source,
        "--poll-ms",
        "20",
        "--consent",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let found = sessions(&sb);
    assert_eq!(found.len(), 1);
    let dir = path_str(&found[0]);

    let import = sb.mc(&["import", dir]);
    assert_eq!(
        import.code, 0,
        "import: {}\n{}",
        import.stdout, import.stderr
    );
    let run = sb.mc(&["run", dir]);
    assert_eq!(run.code, 0, "run: {}\n{}", run.stdout, run.stderr);
}

#[test]
fn the_source_timeout_is_validated_from_flag_and_environment() {
    let sb = Sandbox::new();
    let source = fake(&sb, "synthetic_basic");
    for timeout in ["10", "60001", "slow"] {
        let out = sb.mc(&[
            "capture",
            "--source",
            &source,
            "--consent",
            "--poll-timeout-ms",
            timeout,
        ]);
        assert_eq!(out.code, 2, "--poll-timeout-ms {timeout}: {}", out.stderr);
    }
    let env = sb.env_with(&[
        ("MC_CAPTURE_SOURCE", source.as_str()),
        ("MC_POLL_TIMEOUT_MS", "abc"),
    ]);
    let out = common::mc(&env, &["capture", "--consent"]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(sessions(&sb).is_empty());

    let env = sb.env_with(&[
        ("MC_CAPTURE_SOURCE", source.as_str()),
        ("MC_POLL_TIMEOUT_MS", "2000"),
        ("MC_POLL_MS", "20"),
    ]);
    let out = common::mc(&env, &["capture", "--consent"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
}
