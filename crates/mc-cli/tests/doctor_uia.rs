//! `mc doctor --uia`, `mc doctor --dump`, the `--source` refusals, and the
//! `capture.selectors` config block, end to end through `cli::run`.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Out, Sandbox, mc, path_str};
use mc_captions::ui::Fixture;
use serde_json::Value;

/// `fake-uia:` spec for a fixture in `fixtures/uia`.
fn uia(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/uia")
        .join(format!("{name}.json"));
    format!("fake-uia:{}", path_str(&path))
}

fn ndjson(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
        .collect()
}

/// Every `segment` event's text, in order.
fn segment_texts(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .filter(|e| e["event"] == "segment")
        .filter_map(|e| e["text"].as_str().map(str::to_string))
        .collect()
}

fn read_fixture(path: &Path) -> Fixture {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

// --- doctor --uia ---------------------------------------------------------------------

#[test]
fn doctor_uia_prints_the_selector_version_and_every_window_row() {
    let sb = Sandbox::new();
    let out = sb.mc(&["doctor", "--uia", "--source", &uia("teams_popout_and_main")]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("selectors v5"), "{}", out.stdout);
    assert!(out.stdout.contains("[0] TeamsWebView"), "{}", out.stdout);
    assert!(out.stdout.contains("[1] TeamsWebView"), "{}", out.stdout);
    assert!(
        out.stdout
            .contains("Subtítulos | Reunión X | Microsoft Teams"),
        "{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("teams: window [1] (CaptionPopout)"),
        "{}",
        out.stdout
    );
}

#[test]
fn doctor_uia_reports_zoom_meetings_too() {
    let sb = Sandbox::new();
    let out = sb.mc(&["doctor", "--uia", "--source", &uia("zoom_class_match")]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("zoom: window [1] (Meeting)"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("2 captions"), "{}", out.stdout);
}

#[test]
fn doctor_uia_json_carries_the_report() {
    let sb = Sandbox::new();
    let out = sb.mc(&[
        "--json",
        "doctor",
        "--uia",
        "--source",
        &uia("teams_popout_and_main"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let value = out.json();
    assert_eq!(value["ok"], true);
    let report = &value["uia"]["report"];
    assert_eq!(report["selectors_version"], 5);
    assert_eq!(report["windows"].as_array().unwrap().len(), 2);
    let teams = report["picks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["platform"] == "teams")
        .expect("a teams pick");
    assert_eq!(teams["tier"], "CaptionPopout");
    assert_eq!(teams["captions"], 1);
}

// Native backends exist on Windows and macOS; tests must never touch them.
#[cfg(not(any(windows, target_os = "macos")))]
#[test]
fn doctor_uia_without_a_source_is_a_usage_error_on_this_os() {
    let sb = Sandbox::new();
    let out = sb.mc(&["doctor", "--uia"]);
    assert_eq!(out.code, 2, "stderr: {}", out.stderr);
    assert!(out.stderr.contains("fake-uia"), "{}", out.stderr);
    assert!(out.stdout.is_empty(), "{}", out.stdout);
}

#[test]
fn doctor_uia_rejects_a_source_that_is_not_fake_uia() {
    let sb = Sandbox::new();
    let out = sb.mc(&["doctor", "--uia", "--source", "fake:/nope"]);
    assert_eq!(out.code, 2, "stderr: {}", out.stderr);
    assert!(out.stderr.contains("fake-uia:"), "{}", out.stderr);
}

#[test]
fn doctor_uia_number_options_must_be_positive_numbers() {
    let sb = Sandbox::new();
    let source = uia("teams_three_speakers");
    let target = path_str(&sb.path("n.json")).to_string();
    for args in [
        vec!["doctor", "--uia", "--source", &source, "--max-nodes", "0"],
        vec![
            "doctor",
            "--uia",
            "--source",
            &source,
            "--max-nodes",
            "many",
        ],
        vec![
            "doctor",
            "--dump",
            &target,
            "--source",
            &source,
            "--max-depth",
            "0",
        ],
        vec![
            "doctor",
            "--dump",
            &target,
            "--source",
            &source,
            "--max-depth",
            "deep",
        ],
    ] {
        let out = sb.mc(&args);
        assert_eq!(out.code, 2, "{args:?}: stderr: {}", out.stderr);
    }
    assert!(!sb.path("n.json").exists());
}

/// SUSPECTED GAP: `--max-depth`, `--window`, `--raw` and `--platform` are only
/// read on the `--dump` branch, so `doctor --uia --max-depth 0` exits 0 and
/// ignores the value without a word.
#[test]
fn dump_only_options_with_uia_alone_are_refused() {
    let sb = Sandbox::new();
    let out = sb.mc(&[
        "doctor",
        "--uia",
        "--source",
        &uia("teams_three_speakers"),
        "--max-depth",
        "0",
    ]);
    assert_eq!(out.code, 2, "stderr: {}", out.stderr);
}

#[test]
fn doctor_uia_with_a_missing_fixture_is_a_runtime_error() {
    let sb = Sandbox::new();
    let missing = sb.path("nope/missing.json");
    let spec = format!("fake-uia:{}", path_str(&missing));
    let out = sb.mc(&["doctor", "--uia", "--source", &spec]);
    assert_eq!(out.code, 3, "stderr: {}", out.stderr);
    assert!(out.stderr.contains("missing.json"), "{}", out.stderr);
}

// --- doctor --dump --------------------------------------------------------------------------

#[test]
fn dump_writes_a_fixture_file_and_no_temporary_file() {
    let sb = Sandbox::new();
    let target = sb.path("d.json");
    let out = sb.mc(&[
        "doctor",
        "--dump",
        path_str(&target),
        "--source",
        &uia("teams_popout_and_main"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("anonymised: review it before sharing"),
        "{}",
        out.stdout
    );
    assert!(!out.stdout.contains("RAW"), "{}", out.stdout);

    let fixture = read_fixture(&target);
    assert!(fixture.anonymized);
    assert_eq!(fixture.windows.len(), 2);
    assert_eq!(fixture.platform, "teams");
    assert!(
        !sb.path("d.json.tmp").exists(),
        "the temporary file was renamed away"
    );
    let text = fs::read_to_string(&target).unwrap();
    assert!(
        !text.contains("Ana García") && !text.contains("Luis Pérez"),
        "anonymised dump leaks names"
    );
}

#[test]
fn raw_dump_says_raw_and_keeps_the_text() {
    let sb = Sandbox::new();
    let target = sb.path("raw.json");
    let out = sb.mc(&[
        "doctor",
        "--dump",
        path_str(&target),
        "--raw",
        "--window",
        "0",
        "--source",
        &uia("teams_three_speakers"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("RAW: contains the meeting's text"),
        "{}",
        out.stdout
    );
    let fixture = read_fixture(&target);
    assert!(!fixture.anonymized);
    let text = fs::read_to_string(&target).unwrap();
    assert!(
        text.contains("Ana García") && text.contains("Buenos días."),
        "raw dump lost its text"
    );
}

#[test]
fn dump_platform_must_be_teams_or_zoom() {
    let sb = Sandbox::new();
    let target = sb.path("bogus.json");
    let out = sb.mc(&[
        "doctor",
        "--dump",
        path_str(&target),
        "--platform",
        "bogus",
        "--source",
        &uia("teams_three_speakers"),
    ]);
    assert_eq!(out.code, 2, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("teams, zoom, meet, teams-web or slack"),
        "{}",
        out.stderr
    );
    assert!(!target.exists(), "nothing is written on a usage error");
    assert!(!sb.path("bogus.json.tmp").exists());
}

#[test]
fn doctor_reports_and_dumps_a_meet_browser_window() {
    let sb = Sandbox::new();
    let spec = format!(
        "fake-uia:{}",
        path_str(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../mc-captions/tests/web_fixtures/meet_two_speakers_growing.json")
        )
    );
    let out = sb.mc(&["--json", "doctor", "--uia", "--source", &spec]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let value = out.json();
    let picks = value["uia"]["report"]["picks"].as_array().unwrap();
    assert_eq!(picks.len(), 1);
    assert_eq!(picks[0]["platform"], "meet");
    assert_eq!(picks[0]["captions"], 2);

    let target = sb.path("meet.json");
    let out = sb.mc(&[
        "doctor",
        "--dump",
        path_str(&target),
        "--platform",
        "meet",
        "--source",
        &spec,
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let fixture = read_fixture(&target);
    assert_eq!(fixture.platform, "meet");
    assert_eq!(fixture.app, "Google Meet");
    assert!(fixture.anonymized);
}

#[test]
fn dump_of_a_zoom_meeting_names_the_app_zoom() {
    let sb = Sandbox::new();
    let target = sb.path("zoom.json");
    let out = sb.mc(&[
        "doctor",
        "--dump",
        path_str(&target),
        "--platform",
        "zoom",
        "--source",
        &uia("zoom_class_match"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let fixture = read_fixture(&target);
    assert_eq!(fixture.platform, "zoom");
    assert_eq!(fixture.app, "Zoom");
}

/// SUSPECTED GAP: `--window 9` on a two-window source writes a fixture with no
/// windows and exits 0; an index that names no window should be a usage error.
#[test]
fn dump_window_index_out_of_range_is_a_usage_error() {
    let sb = Sandbox::new();
    let target = sb.path("oob.json");
    let out = sb.mc(&[
        "doctor",
        "--dump",
        path_str(&target),
        "--window",
        "9",
        "--source",
        &uia("teams_popout_and_main"),
    ]);
    assert_eq!(out.code, 2, "stderr: {}", out.stderr);
    assert!(!target.exists());
}

#[test]
fn dump_window_must_be_an_index() {
    let sb = Sandbox::new();
    let target = sb.path("bad-window.json");
    let out = sb.mc(&[
        "doctor",
        "--dump",
        path_str(&target),
        "--window",
        "main",
        "--source",
        &uia("teams_popout_and_main"),
    ]);
    assert_eq!(out.code, 2, "stderr: {}", out.stderr);
    assert!(out.stderr.contains("is not an index"), "{}", out.stderr);
}

#[test]
fn a_dump_reads_back_through_the_same_parsers() {
    let sb = Sandbox::new();
    let target = sb.path("again.json");
    let dumped = sb.mc(&[
        "doctor",
        "--dump",
        path_str(&target),
        "--source",
        &uia("teams_three_speakers"),
    ]);
    assert_eq!(dumped.code, 0, "stderr: {}", dumped.stderr);
    let spec = format!("fake-uia:{}", path_str(&target));
    let out = sb.mc(&["--json", "doctor", "--uia", "--source", &spec]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert_eq!(out.json()["uia"]["report"]["picks"][0]["captions"], 3);
}

// --- --source teams/zoom refusals --------------------------------------------------------

#[cfg(not(any(windows, target_os = "macos")))]
#[test]
fn capture_from_the_live_teams_window_is_a_usage_error_off_windows() {
    let sb = Sandbox::new();
    let out = sb.mc(&["capture", "--source", "teams", "--consent"]);
    assert_eq!(out.code, 2, "stderr: {}", out.stderr);
    assert!(out.stderr.contains("Windows"), "{}", out.stderr);
    assert!(!sb.data().join("corpus").exists(), "no session is started");
}

#[cfg(not(any(windows, target_os = "macos")))]
#[test]
fn capture_from_the_live_zoom_window_is_a_usage_error_off_windows() {
    let sb = Sandbox::new();
    let out = sb.mc(&["--json", "capture", "--source", "zoom", "--consent"]);
    assert_eq!(out.code, 2, "stderr: {}", out.stderr);
    assert!(out.stdout.is_empty());
    assert!(out.stderr.contains("Windows"), "{}", out.stderr);
}

// --- capture.selectors in the config file ---------------------------------------------------

fn write_config(sb: &Sandbox, name: &str, text: &str) -> PathBuf {
    let path = sb.path(name);
    fs::write(&path, text).unwrap();
    path
}

fn capture_with_config(sb: &Sandbox, config: Option<&Path>, fixture: &str) -> Out {
    let source = uia(fixture);
    let mut env = sb.env.clone();
    if let Some(path) = config {
        env.vars
            .insert("MC_CONFIG".to_string(), path_str(path).to_string());
    }
    mc(
        &env,
        &[
            "--json",
            "capture",
            "--source",
            &source,
            "--consent",
            "--until",
            "1",
            "--poll-ms",
            "100",
        ],
    )
}

#[test]
fn the_default_selectors_drop_muted_captions() {
    let sb = Sandbox::new();
    let out = capture_with_config(&sb, None, "teams_mic_noise_group");
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let texts = segment_texts(&ndjson(&out.stdout));
    assert!(texts.contains(&"Hola a todos".to_string()), "{texts:?}");
    assert!(!texts.contains(&"Muted".to_string()), "{texts:?}");
}

#[test]
fn a_config_noise_list_replaces_the_default_and_is_honoured_end_to_end() {
    let sb = Sandbox::new();
    let config = write_config(
        &sb,
        "override.json",
        r#"{"version": 1, "capture": {"selectors": {"teams": {"noise": ["Zzz"]}}}}"#,
    );
    let out = capture_with_config(&sb, Some(&config), "teams_mic_noise_group");
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let texts = segment_texts(&ndjson(&out.stdout));
    // With "Muted" no longer noise, the muted caption is read; the defaults' other
    // noise words ("Micrófono") are no longer filtered either.
    assert!(texts.contains(&"Muted".to_string()), "{texts:?}");
    assert!(
        texts.contains(&"Micrófono silenciado".to_string()),
        "{texts:?}"
    );
}

#[test]
fn a_config_selector_override_is_shown_by_doctor_uia_too() {
    let sb = Sandbox::new();
    let config = write_config(
        &sb,
        "version.json",
        r#"{"version": 1, "capture": {"selectors": {"version": 7}}}"#,
    );
    let mut env = sb.env.clone();
    env.vars
        .insert("MC_CONFIG".to_string(), path_str(&config).to_string());
    let out = mc(
        &env,
        &[
            "--json",
            "doctor",
            "--uia",
            "--source",
            &uia("teams_three_speakers"),
        ],
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert_eq!(out.json()["uia"]["report"]["selectors_version"], 7);
}

#[test]
fn an_empty_keyword_in_the_selectors_fails_the_config_check() {
    let sb = Sandbox::new();
    let config = write_config(
        &sb,
        "empty.json",
        r#"{"version": 1, "capture": {"selectors": {"teams": {"caption_keywords": ["captions", ""]}}}}"#,
    );
    let mut env = sb.env.clone();
    env.vars
        .insert("MC_CONFIG".to_string(), path_str(&config).to_string());
    let out = mc(&env, &["--json", "doctor"]);
    assert_eq!(out.code, 1, "stderr: {}", out.stderr);
    let value = out.json();
    assert_eq!(value["ok"], false);
    assert_eq!(value["checks"][0]["name"], "config");
    assert_eq!(value["checks"][0]["status"], "FAIL");
    assert!(
        value["checks"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("selectors.teams.caption_keywords"),
        "{value}"
    );
}

#[test]
fn an_unknown_key_in_the_selectors_fails_the_config_check_in_human_output() {
    let sb = Sandbox::new();
    let config = write_config(
        &sb,
        "unknown.json",
        r#"{"version": 1, "capture": {"selectors": {"teams": {"nosuch": ["x"]}}}}"#,
    );
    let mut env = sb.env.clone();
    env.vars
        .insert("MC_CONFIG".to_string(), path_str(&config).to_string());
    let out = mc(&env, &["doctor"]);
    assert_eq!(out.code, 1, "stderr: {}", out.stderr);
    assert!(out.stdout.starts_with("FAIL  config"), "{}", out.stdout);
    assert!(out.stdout.contains("nosuch"), "{}", out.stdout);
    assert!(
        out.stdout.contains("other checks skipped"),
        "{}",
        out.stdout
    );
}

#[test]
fn an_empty_required_title_fails_the_config_check() {
    let sb = Sandbox::new();
    let config = write_config(
        &sb,
        "title.json",
        r#"{"version": 1, "capture": {"selectors": {"teams": {"required_title": ""}}}}"#,
    );
    let mut env = sb.env.clone();
    env.vars
        .insert("MC_CONFIG".to_string(), path_str(&config).to_string());
    let out = mc(&env, &["--json", "doctor"]);
    assert_eq!(out.code, 1, "stderr: {}", out.stderr);
    assert!(
        out.json()["checks"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("required_title")
    );
}

#[test]
fn a_raw_dump_without_a_window_is_refused() {
    let sb = Sandbox::new();
    let target = sb.path("raw.json");
    let out = sb.mc(&[
        "doctor",
        "--dump",
        path_str(&target),
        "--raw",
        "--source",
        &uia("teams_three_speakers"),
    ]);
    assert_eq!(out.code, 2, "stderr: {}", out.stderr);
    assert!(!target.exists());
}

#[test]
fn an_existing_dump_is_not_overwritten_without_force() {
    let sb = Sandbox::new();
    let target = sb.path("d.json");
    let args = |force: bool| {
        let mut a = vec![
            "doctor".to_string(),
            "--dump".to_string(),
            path_str(&target).to_string(),
            "--source".to_string(),
            uia("teams_three_speakers"),
        ];
        if force {
            a.push("--force".to_string());
        }
        a
    };
    let first = args(false);
    let first: Vec<&str> = first.iter().map(String::as_str).collect();
    assert_eq!(sb.mc(&first).code, 0);
    assert_eq!(sb.mc(&first).code, 2, "second write must be refused");
    let forced = args(true);
    let forced: Vec<&str> = forced.iter().map(String::as_str).collect();
    assert_eq!(sb.mc(&forced).code, 0);
}

// --- detect ----------------------------------------------------------------------------

#[test]
fn detect_json_lists_the_open_platforms_and_their_captions() {
    let sb = Sandbox::new();
    let out = sb.mc(&[
        "--json",
        "detect",
        "--source",
        &uia("teams_popout_and_main"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let value = out.json();
    let teams = value["platforms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["platform"] == "teams")
        .expect("teams detected");
    assert_eq!(teams["captions_on"], true);
    assert_eq!(teams["captions"], 1);
    assert_eq!(teams["app"], "Microsoft Teams");
}

#[test]
fn doctor_json_carries_the_tisty_target_and_the_whisper_check() {
    let sb = Sandbox::new();
    let value = sb.mc(&["--json", "doctor"]).json();
    assert_eq!(value["tisty_target"]["folder"], "Reuniones");
    assert!(
        value["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "whisper"),
        "{value}"
    );
}
