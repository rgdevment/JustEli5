//! Anonymised dumps from `mc doctor --dump` (fixtures/uia/dumps): the Rust
//! parsers must read each one exactly as the real Python captures do (the
//! expectation is `<name>.expected.json`, recorded from
//! the original Python capture), and the committed file must already be
//! what the anonymiser would write.

use std::path::{Path, PathBuf};

use mc_captions::ui::anon::anonymise;
use mc_captions::ui::fake::{Fixture, World};
use mc_captions::ui::{FakeBackend, NodeMode, Platform, Selectors, UiaSource};
use mc_captions::{CaptureSource, Discovery};
use serde_json::Value;

fn dumps_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/uia/dumps")
}

fn dump_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dumps_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            name.ends_with(".json") && !name.ends_with(".expected.json")
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

#[test]
fn every_dump_is_read_like_the_python_capture() {
    let files = dump_files();
    assert!(files.len() >= 4, "found {} dumps", files.len());
    let mut worlds_checked = 0;
    for file in files {
        let stem = file.file_stem().unwrap().to_string_lossy().to_string();
        let fixture: Fixture = serde_json::from_str(&std::fs::read_to_string(&file).unwrap())
            .unwrap_or_else(|e| panic!("{stem}: {e}"));
        let expected: Value = serde_json::from_str(
            &std::fs::read_to_string(dumps_dir().join(format!("{stem}.expected.json")))
                .unwrap_or_else(|e| panic!("{stem}: missing expectation: {e}")),
        )
        .unwrap();
        assert!(
            fixture.anonymized,
            "{stem}: a committed dump must be anonymised"
        );
        let platform = if fixture.platform == "zoom" {
            Platform::Zoom
        } else {
            Platform::Teams
        };
        let world = World {
            windows: fixture.windows.clone(),
        };
        let want = &expected["worlds"][0];
        let mut source = UiaSource::new(
            FakeBackend::new(world),
            platform,
            Selectors::default(),
            NodeMode::All,
        );
        let attached = match source.discover().unwrap() {
            Discovery::Attached { window } => Some(window),
            _ => None,
        };
        assert_eq!(
            attached.as_deref(),
            want["discover"]["attached"].as_str(),
            "{stem}: attached window"
        );
        assert_eq!(
            source.meeting_name().as_deref(),
            want["meeting_name"].as_str(),
            "{stem}: meeting name"
        );
        let all: Vec<(Option<String>, String)> = source
            .poll()
            .unwrap_or_default()
            .into_iter()
            .map(|o| (o.speaker, o.text.unwrap_or_default()))
            .collect();
        assert_eq!(all, pairs(&want["captions_all"]), "{stem}: captions (all)");
        worlds_checked += 1;
    }
    assert!(worlds_checked >= 4);
}

#[test]
fn every_committed_dump_is_already_what_the_anonymiser_writes() {
    let selectors = Selectors::default();
    for file in dump_files() {
        let stem = file.file_stem().unwrap().to_string_lossy().to_string();
        let fixture: Fixture =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(
            anonymise(&fixture, &selectors),
            fixture,
            "{stem}: not a fixed point of anonymise()"
        );
    }
}
