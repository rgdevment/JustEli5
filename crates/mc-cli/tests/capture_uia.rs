//! `mc capture --source fake-uia:<fixture.json>`: accessibility fixtures run
//! through the real `UiaSource` and parsers, record a session that replays
//! strictly, and report their segments as NDJSON. Also the source's refusals.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use common::{Sandbox, path_str};
use serde_json::Value;

/// `fake-uia:` spec for a fixture in `fixtures/uia`.
fn uia(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/uia")
        .join(format!("{name}.json"));
    format!("fake-uia:{}", path_str(&path))
}

/// `fake-uia:` spec for a synthetic browser/Slack tree in `mc-captions/tests/web_fixtures`.
fn web(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../mc-captions/tests/web_fixtures")
        .join(format!("{name}.json"));
    format!("fake-uia:{}", path_str(&path))
}

/// Session folders under `MC_DATA_DIR/corpus`.
fn sessions(sb: &Sandbox) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(sb.data().join("corpus")) else {
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

fn segment_speakers(events: &[Value]) -> BTreeSet<String> {
    events
        .iter()
        .filter(|e| e["event"] == "segment")
        .filter_map(|e| e["speaker"].as_str().map(str::to_string))
        .collect()
}

/// Records `fixture` for two seconds, then replays the session strictly.
/// Returns the NDJSON events and the session folder.
fn record(sb: &Sandbox, fixture: &str) -> (Vec<Value>, PathBuf) {
    record_spec(sb, fixture, &uia(fixture))
}

fn record_spec(sb: &Sandbox, fixture: &str, source: &str) -> (Vec<Value>, PathBuf) {
    let out = sb.mc(&[
        "--json",
        "capture",
        "--source",
        source,
        "--consent",
        "--until",
        "2",
        "--poll-ms",
        "100",
    ]);
    assert_eq!(out.code, 0, "{fixture}: stderr: {}", out.stderr);
    let events = ndjson(&out.stdout);
    assert_eq!(events[0]["event"], "started", "{fixture}");
    assert_eq!(events.last().unwrap()["event"], "ended", "{fixture}");

    let found = sessions(sb);
    assert_eq!(found.len(), 1, "{fixture}: {found:?}");
    let dir = found[0].clone();
    let replay = sb.mc(&["replay", path_str(&dir), "--verify", "--strict"]);
    assert_eq!(
        replay.code, 0,
        "{fixture}: replay stdout: {}\nstderr: {}",
        replay.stdout, replay.stderr
    );
    (events, dir)
}

#[test]
fn teams_three_speakers_is_recorded_and_replays_strictly() {
    let sb = Sandbox::new();
    let (events, dir) = record(&sb, "teams_three_speakers");

    let attached = events
        .iter()
        .any(|e| e["event"] == "source" && e["state"] == "attached");
    assert!(attached, "the fixture's window is attached");

    let speakers = segment_speakers(&events);
    for expected in ["Ana García", "Luis Pérez", "María Soto"] {
        assert!(
            speakers.contains(expected),
            "missing {expected}: {speakers:?}"
        );
    }

    let m = manifest(&dir);
    assert_eq!(m["platform_requested"], "teams");
    // `--until` stops the capture through the same path as `q`: reason "stop".
    assert_eq!(m["end_reason"], "stop");
}

#[test]
fn zoom_captions_are_recorded_and_replay_strictly() {
    let sb = Sandbox::new();
    let (events, dir) = record(&sb, "zoom_class_match");

    let speakers = segment_speakers(&events);
    assert!(speakers.contains("Ana García"), "{speakers:?}");
    assert!(
        speakers.iter().any(|s| s.starts_with("Speaker 2")),
        "room speakers keep their label: {speakers:?}"
    );
    assert_eq!(manifest(&dir)["platform_requested"], "zoom");
}

#[test]
fn web_platform_fixtures_are_recorded_and_replay_strictly() {
    let cases = [
        (
            "meet_two_speakers_growing",
            "meet",
            ["Ana García", "Luis Pérez"],
        ),
        (
            "teams_web_and_desktop",
            "teams-web",
            ["Ana García", "Luis Pérez"],
        ),
        (
            "slack_huddle_desktop",
            "slack",
            ["Ana García", "Luis Pérez"],
        ),
    ];
    for (name, platform, speakers) in cases {
        let sb = Sandbox::new();
        let (events, dir) = record_spec(&sb, name, &web(name));
        let found = segment_speakers(&events);
        for speaker in speakers {
            assert!(found.contains(speaker), "{name}: {found:?}");
        }
        assert_eq!(manifest(&dir)["platform_requested"], platform, "{name}");
        // The same open lines are not repeated on every poll.
        let open: Vec<&Value> = events.iter().filter(|e| e["event"] == "open").collect();
        assert!(
            open.windows(2)
                .all(|pair| pair[0]["lines"] != pair[1]["lines"]),
            "{name}: repeated open events"
        );
    }
}

#[test]
fn a_missing_fixture_file_is_a_runtime_error() {
    let sb = Sandbox::new();
    let missing = sb.path("nope/missing.json");
    let spec = format!("fake-uia:{}", path_str(&missing));
    let out = sb.mc(&["capture", "--source", &spec, "--consent"]);
    assert_eq!(out.code, 3, "stderr: {}", out.stderr);
    assert!(out.stderr.contains("missing.json"), "{}", out.stderr);
    assert!(sessions(&sb).is_empty());
}

#[test]
fn malformed_fixture_json_is_a_usage_error() {
    let sb = Sandbox::new();
    let bad = sb.path("bad.json");
    fs::write(&bad, "{ \"format\": \"mc-uia-tree\", ").unwrap();
    let spec = format!("fake-uia:{}", path_str(&bad));
    let out = sb.mc(&["capture", "--source", &spec, "--consent"]);
    assert_eq!(out.code, 2, "stderr: {}", out.stderr);
    assert!(sessions(&sb).is_empty());
}

#[test]
fn an_unknown_fixture_platform_is_a_usage_error() {
    let sb = Sandbox::new();
    let odd = sb.path("webex.json");
    fs::write(
        &odd,
        r#"{"format": "mc-uia-tree", "version": 1, "platform": "webex", "windows": []}"#,
    )
    .unwrap();
    let spec = format!("fake-uia:{}", path_str(&odd));
    let out = sb.mc(&["capture", "--source", &spec, "--consent"]);
    assert_eq!(out.code, 2, "stderr: {}", out.stderr);
    assert!(out.stderr.contains("unknown platform"), "{}", out.stderr);
    assert!(sessions(&sb).is_empty());
}

#[test]
fn an_empty_fake_uia_spec_is_a_usage_error() {
    let sb = Sandbox::new();
    let out = sb.mc(&["capture", "--source", "fake-uia:", "--consent"]);
    assert_eq!(out.code, 2, "stderr: {}", out.stderr);
    assert!(out.stderr.contains("fixture file"), "{}", out.stderr);
    assert!(sessions(&sb).is_empty());
}

/// Texts of the segments `mc run` wrote for `dir`, with the extra `mc run` arguments.
fn run_segments(sb: &Sandbox, dir: &Path, extra: &[&str]) -> Vec<Value> {
    let mut args = vec!["--json", "run", path_str(dir)];
    args.extend_from_slice(extra);
    let out = sb.mc(&args);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let segments = PathBuf::from(out.json()["path"].as_str().unwrap()).join("segments.jsonl");
    fs::read_to_string(segments)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn a_browser_session_with_two_blocks_of_one_speaker_is_one_segment_per_utterance() {
    let sb = Sandbox::new();
    let (events, dir) = record_spec(
        &sb,
        "meet_one_speaker_two_blocks",
        &web("meet_one_speaker_two_blocks"),
    );
    // The policy is recorded, and `record_spec` already replayed it strictly.
    assert_eq!(manifest(&dir)["captions"]["policy"], "v2");

    let live: Vec<&Value> = events.iter().filter(|e| e["event"] == "segment").collect();
    let texts: Vec<&str> = live.iter().map(|e| e["text"].as_str().unwrap()).collect();
    assert_eq!(
        texts,
        [
            "Hello everyone.",
            "Let us start with the budget for next year"
        ]
    );

    let run = run_segments(&sb, &dir, &[]);
    assert_eq!(run.len(), live.len());
    for (live, run) in live.iter().zip(&run) {
        assert_eq!(live["idx"], run["idx"]);
        assert_eq!(live["text"], run["text"]);
    }
}

#[test]
fn an_explicit_v1_policy_still_wins_over_the_platform_default() {
    let sb = Sandbox::new();
    let (_, dir) = record_spec(
        &sb,
        "meet_one_speaker_two_blocks",
        &web("meet_one_speaker_two_blocks"),
    );
    let v1 = run_segments(&sb, &dir, &["--policy", "v1_legacy"]);
    assert!(
        v1.len() > 2,
        "V1 commits the growing prefixes: {}",
        v1.len()
    );
}
