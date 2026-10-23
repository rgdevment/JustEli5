//! The Whisper engine. Loading failures run everywhere; real recognition needs
//! a model: `MC_TEST_MODEL=/path/ggml-base.bin cargo test -p mc-asr --features whisper`.
#![cfg(feature = "whisper")]

use std::path::PathBuf;

use mc_asr::whisper::{WhisperAsr, WhisperOptions};
use mc_asr::{AsrError, TranscribeCtx, Transcriber};

#[test]
fn a_missing_model_is_reported_with_the_fix() {
    let dir = tempfile::tempdir().unwrap();
    let error = WhisperAsr::load(
        &dir.path().join("nope.bin"),
        "whisper-base",
        None,
        WhisperOptions::default(),
    )
    .err()
    .expect("no such file");
    match error {
        AsrError::Input(message) => {
            assert!(message.contains("mc models pull whisper-base"), "{message}")
        }
        other => panic!("unexpected {other}"),
    }
}

#[test]
fn a_file_that_is_not_a_model_fails_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("junk.bin");
    std::fs::write(&path, b"this is not a ggml model at all").unwrap();
    let result = WhisperAsr::load(&path, "junk", None, WhisperOptions::default());
    assert!(matches!(result, Err(AsrError::Engine(_))));
}

fn model() -> Option<PathBuf> {
    let path = std::env::var_os("MC_TEST_MODEL").map(PathBuf::from)?;
    path.is_file().then_some(path)
}

#[test]
fn silence_and_noise_produce_no_invented_text() {
    let Some(path) = model() else {
        eprintln!("skipped: set MC_TEST_MODEL to a ggml model file to run this test");
        return;
    };
    let mut asr = WhisperAsr::load(&path, "test-model", None, WhisperOptions::default()).unwrap();
    let ctx = TranscribeCtx {
        channel: "mic",
        prompt: Some("Vocabulario: sprint, deploy."),
        language: None,
    };
    let silence = vec![0.0f32; 16_000 * 3];
    let found = asr.transcribe(&silence, &ctx).unwrap();
    assert!(found.segments.is_empty(), "silence produced {found:?}");
    // Very short input is padded rather than rejected.
    assert!(
        asr.transcribe(&vec![0.0; 800], &ctx)
            .unwrap()
            .segments
            .is_empty()
    );
    assert_eq!(asr.engine(), "whisper.cpp");
    assert_eq!(asr.model(), "test-model");
    assert!(asr.params()["beam_size"].is_number());
}
