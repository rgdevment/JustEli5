//! The Rust parsers against the Python implementation: every fixture world is
//! read cold by both, and the discovery result, meeting name and captions must
//! match what was recorded from the real classes of the original Python app.

use std::path::{Path, PathBuf};

use mc_captions::ui::fake::{Fixture, World};
use mc_captions::ui::{FakeBackend, NodeMode, Platform, Selectors, UiaSource};
use mc_captions::{CaptureSource, Discovery};
use serde_json::Value;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/uia")
}

fn scenarios() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(fixtures_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            name.ends_with(".json")
                && !name.ends_with(".expected.json")
                && name != "zoom_items.json"
        })
        .collect();
    files.sort();
    files
}

fn pairs(value: &Value) -> Vec<(Option<String>, String)> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|pair| {
            (
                pair[0].as_str().map(str::to_string),
                pair[1].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// `(attached window, meeting name, captions)` of one world.
type Reading = (
    Option<String>,
    Option<String>,
    Vec<(Option<String>, String)>,
);

fn read_world(world: World, platform: Platform, mode: NodeMode) -> Reading {
    let mut source = UiaSource::new(
        FakeBackend::new(world),
        platform,
        Selectors::default(),
        mode,
    );
    let attached = match source.discover().unwrap() {
        Discovery::Attached { window } => Some(window),
        _ => None,
    };
    let name = source.meeting_name();
    let captions = source
        .poll()
        .unwrap_or_default()
        .into_iter()
        .map(|o| (o.speaker, o.text.unwrap_or_default()))
        .collect();
    (attached, name, captions)
}

#[test]
fn every_fixture_world_matches_the_python_recordings() {
    let files = scenarios();
    assert!(files.len() >= 25, "found {} fixtures", files.len());
    let mut worlds_checked = 0;
    for file in files {
        let stem = file.file_stem().unwrap().to_string_lossy().to_string();
        let fixture: Fixture = serde_json::from_str(&std::fs::read_to_string(&file).unwrap())
            .unwrap_or_else(|e| panic!("{stem}: {e}"));
        let expected: Value = serde_json::from_str(
            &std::fs::read_to_string(fixtures_dir().join(format!("{stem}.expected.json"))).unwrap(),
        )
        .unwrap();
        let platform = if fixture.platform == "zoom" {
            Platform::Zoom
        } else {
            Platform::Teams
        };
        let mut worlds = vec![World {
            windows: fixture.windows.clone(),
        }];
        worlds.extend(fixture.script.clone());
        let expected_worlds = expected["worlds"].as_array().unwrap();
        assert_eq!(worlds.len(), expected_worlds.len(), "{stem}: world count");

        for (index, (world, want)) in worlds.into_iter().zip(expected_worlds).enumerate() {
            let label = format!("{stem} world {index}");
            let (attached, name, all) = read_world(world.clone(), platform, NodeMode::All);
            match &want["discover"] {
                Value::String(s) => {
                    assert_eq!(s, "searching", "{label}");
                    assert_eq!(attached, None, "{label}: discover");
                }
                other => assert_eq!(
                    attached.as_deref(),
                    other["attached"].as_str(),
                    "{label}: attached window"
                ),
            }
            assert_eq!(
                name.as_deref(),
                want["meeting_name"].as_str(),
                "{label}: meeting name"
            );
            assert_eq!(all, pairs(&want["captions_all"]), "{label}: captions (all)");
            let (_, _, last) = read_world(world, platform, NodeMode::Last);
            assert_eq!(
                last,
                pairs(&want["captions_last"]),
                "{label}: captions (last)"
            );
            worlds_checked += 1;
        }
    }
    assert!(worlds_checked >= 40, "only {worlds_checked} worlds checked");
}

#[test]
fn zoom_item_vectors_match_python() {
    use mc_captions::ui::node::{Role, UiNode};
    use mc_captions::ui::zoom::parse_item;
    let text = std::fs::read_to_string(fixtures_dir().join("zoom_items.json")).unwrap();
    let file: Value = serde_json::from_str(&text).unwrap();
    let vectors = file["vectors"].as_array().unwrap();
    assert!(vectors.len() >= 200);
    for (index, vector) in vectors.iter().enumerate() {
        let edits: Vec<UiNode> = vector["edits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| UiNode::new(Role::Edit, e.as_str().unwrap()))
            .collect();
        let pane = UiNode::new(Role::Pane, "").with_children(edits);
        let mut item = UiNode::new(Role::ListItem, vector["name"].as_str().unwrap());
        if !vector["edits"].as_array().unwrap().is_empty() {
            item.children.push(pane);
        }
        let got = parse_item(&item);
        let want = &vector["expected"];
        match want {
            Value::Null => assert_eq!(got, None, "vector {index}: {vector}"),
            pair => assert_eq!(
                got,
                Some((
                    pair[0].as_str().map(str::to_string),
                    pair[1].as_str().unwrap().to_string()
                )),
                "vector {index}: {vector}"
            ),
        }
    }
}
