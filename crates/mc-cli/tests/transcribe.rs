//! `mc transcribe` with the scripted engine.

mod common;

use std::fs;

use common::{Sandbox, copy_tree, fixture, path_str};
use mc_audio::wav::WavWriter;
use serde_json::Value;

fn speech(ms: usize) -> Vec<i16> {
    (0..ms * 16)
        .map(|n| (0.3 * (n as f32 * 0.25).sin() * 32767.0) as i16)
        .collect()
}

/// A session folder (manifest from a fixture) with a mic.wav holding two phrases.
fn session(sb: &Sandbox) -> String {
    let dir = sb.path("sessions/s1");
    copy_tree(&fixture("synthetic_basic"), &dir);
    let samples: Vec<i16> = [
        vec![0; 8000],
        speech(1000),
        vec![0; 24_000],
        speech(700),
        vec![0; 8000],
    ]
    .concat();
    let mut w = WavWriter::create(&dir.join("mic.wav"), 16_000).unwrap();
    w.write_samples(&samples).unwrap();
    w.finish().unwrap();
    path_str(&dir).to_string()
}

#[test]
fn transcribes_a_session_and_reuses_the_run() {
    let sb = Sandbox::new();
    let dir = session(&sb);
    let script = sb.path("script.json");
    fs::write(&script, r#"["hola equipo", "vamos con el deploy"]"#).unwrap();
    let engine = format!("fake:{}", path_str(&script));
    let out = sb.mc(&["transcribe", &dir, "--engine", &engine]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(out.stdout.contains("created"), "{}", out.stdout);
    assert!(out.stdout.contains("2 segments (mic 2)"), "{}", out.stdout);
    assert!(out.stdout.contains("no audio for system"), "{}", out.stdout);

    let again = sb.mc(&["--json", "transcribe", &dir, "--engine", &engine]);
    assert_eq!(again.code, 0, "{}", again.stderr);
    let value: Value = serde_json::from_str(&again.stdout).unwrap();
    assert_eq!(value["reused"], true);
    let asr_dir = value["path"].as_str().unwrap();
    let text = fs::read_to_string(format!("{asr_dir}/asr.jsonl")).unwrap();
    assert!(text.contains("hola equipo") && text.contains("vamos con el deploy"));

    let forced = sb.mc(&["--json", "transcribe", &dir, "--engine", &engine, "--force"]);
    let value: Value = serde_json::from_str(&forced.stdout).unwrap();
    assert_eq!(value["reused"], false);
}

#[test]
fn a_broken_json_glossary_is_a_usage_error_naming_the_file() {
    let sb = Sandbox::new();
    let dir = session(&sb);
    let glossary = sb.path("glossary.json");
    fs::write(&glossary, r#"["CIAM",]"#).unwrap();
    let file = path_str(&glossary);
    let out = sb.mc(&["transcribe", &dir, "--engine", "fake", "--glossary", file]);
    assert_eq!(
        out.code, 2,
        "{}
{}",
        out.stdout, out.stderr
    );
    assert!(out.stderr.contains("glossary.json"), "{}", out.stderr);
}

#[test]
fn bad_input_is_reported_clearly() {
    let sb = Sandbox::new();
    let dir = session(&sb);
    for (args, code) in [
        (vec!["transcribe"], 2),
        (vec!["transcribe", &dir, "--engine", "nope"], 2),
        (
            vec![
                "transcribe",
                &dir,
                "--engine",
                "fake",
                "--channels",
                "radio",
            ],
            2,
        ),
        (
            vec![
                "transcribe",
                &dir,
                "--engine",
                "fake",
                "--language",
                "spanish",
            ],
            2,
        ),
        (vec!["transcribe", "/nonexistent", "--engine", "fake"], 4),
        (
            vec![
                "transcribe",
                &dir,
                "--engine",
                "fake",
                "--channels",
                "mic,mic",
            ],
            0,
        ),
    ] {
        let out = sb.mc(&args);
        assert!(
            out.code == code || (code == 4 && out.code != 0),
            "{args:?}: {} {}",
            out.code,
            out.stderr
        );
    }
    // Whisper needs a build with the feature and an installed model: either
    // way the command fails and says what to do.
    let out = sb.mc(&["transcribe", &dir, "--engine", "whisper"]);
    assert_ne!(out.code, 0);
    assert!(
        out.stderr.contains("--features whisper") || out.stderr.contains("mc models pull"),
        "{}",
        out.stderr
    );
    // A session without audio says so.
    let empty = sb.path("sessions/s2");
    copy_tree(&fixture("synthetic_basic"), &empty);
    let out = sb.mc(&["transcribe", path_str(&empty), "--engine", "fake"]);
    assert_ne!(out.code, 0);
    assert!(out.stderr.contains("no audio"), "{}", out.stderr);
}
