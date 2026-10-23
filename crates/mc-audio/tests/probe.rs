//! The doctor's audio probe against scripted and file-backed sources.

use std::time::{Duration, Instant};

use mc_audio::probe::{format_probe, probe};
use mc_audio::source::{sine_block, speech_like_block};
use mc_audio::wav::WavWriter;
use mc_audio::{AudioError, FakeAudio, FakeBackend, Step, WavFileBackend};

fn wav(dir: &std::path::Path, name: &str, samples: &[i16]) -> std::path::PathBuf {
    let path = dir.join(name);
    let mut w = WavWriter::create(&path, 16_000).unwrap();
    w.write_samples(samples).unwrap();
    w.finish().unwrap();
    path
}

fn noise(seed: u32, n: usize, gain: f32) -> Vec<i16> {
    speech_like_block(seed, n, gain * 32767.0)
        .into_iter()
        .map(|v| v as i16)
        .collect()
}

#[test]
fn reports_levels_and_flags_a_microphone_that_hears_the_speakers() {
    let dir = tempfile::tempdir().unwrap();
    let system = noise(7, 35_200, 0.8);
    // The mic is the same sound, a little later and quieter, plus nothing else.
    let mut mic = vec![0i16; 160];
    mic.extend(system.iter().map(|v| v / 3));
    mic.truncate(system.len());
    let backend = WavFileBackend {
        mic: Some(wav(dir.path(), "mic.wav", &mic)),
        system: Some(wav(dir.path(), "sys.wav", &system)),
    };
    let started = Instant::now();
    let result = probe(&backend, None, None, 2.2);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(result.mic.status, "ok");
    assert_eq!(result.system.status, "ok");
    assert!(result.system.peak_db.unwrap() > -10.0);
    assert!(result.mic.peak_db.unwrap() < result.system.peak_db.unwrap());
    assert!(result.echo.unwrap() > 0.8, "{:?}", result.echo);
    assert!(result.notes.iter().any(|n| n.starts_with("echo:")));
    assert!(!result.unusable());
    let text = format_probe(&result, &[]);
    assert!(
        text.contains("audio mic") && text.contains("headphones"),
        "{text}"
    );
}

#[test]
fn independent_signals_show_no_echo_and_silence_is_called_out() {
    let dir = tempfile::tempdir().unwrap();
    let backend = WavFileBackend {
        mic: Some(wav(dir.path(), "mic.wav", &noise(1, 35_200, 0.5))),
        system: Some(wav(dir.path(), "sys.wav", &noise(2, 35_200, 0.5))),
    };
    let result = probe(&backend, None, None, 2.2);
    assert!(result.echo.unwrap() < 0.6, "{:?}", result.echo);
    assert!(result.notes.is_empty(), "{:?}", result.notes);

    let silent = WavFileBackend {
        mic: Some(wav(dir.path(), "m2.wav", &vec![0; 35_200])),
        system: Some(wav(dir.path(), "s2.wav", &noise(3, 35_200, 0.5))),
    };
    let result = probe(&silent, None, None, 2.2);
    assert!(result.notes.iter().any(|n| n.starts_with("mic: no signal")));
    assert_eq!(result.echo, None);
}

#[test]
fn a_short_probe_gives_no_echo_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let system = noise(7, 24_000, 0.8);
    let backend = WavFileBackend {
        mic: Some(wav(dir.path(), "mic.wav", &system)),
        system: Some(wav(dir.path(), "sys.wav", &system)),
    };
    let result = probe(&backend, None, None, 1.2);
    assert_eq!(result.echo, None);
    assert!(
        result.notes.iter().any(|n| n.contains("too short")),
        "{:?}",
        result.notes
    );
}

#[test]
fn a_missing_channel_is_reported_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let backend = WavFileBackend {
        mic: Some(wav(dir.path(), "mic.wav", &noise(1, 16_000, 0.5))),
        system: None,
    };
    let result = probe(&backend, None, None, 0.6);
    assert_eq!(result.mic.status, "ok");
    assert_eq!(result.system.status, "unavailable");
    assert!(!result.unusable());
    let nothing = FakeBackend::new(
        vec![Err(AudioError::Device("boom".into()))],
        vec![Err(AudioError::Unavailable("none".into()))],
    );
    let result = probe(&nothing, None, None, 0.5);
    assert_eq!(result.mic.status, "failed");
    assert_eq!(result.system.status, "unavailable");
    assert!(result.unusable());
}

#[test]
fn a_48k_stereo_source_is_brought_to_16k_before_measuring() {
    let info = mc_audio::StreamInfo {
        name: "stereo".into(),
        device_id: None,
        samplerate: 48_000,
        channels: 2,
        latency_s: None,
    };
    let steps = (0..20)
        .map(|n| Step::Block {
            data: sine_block(48_000, 2, 1000.0, 0.5, n * 4800, 4800),
            advance_ms: 0.0,
        })
        .collect();
    let backend = FakeBackend::new(
        vec![Ok(FakeAudio::new(info, steps, None))],
        vec![Err(AudioError::Unavailable("none".into()))],
    );
    let result = probe(&backend, None, None, 0.6);
    let peak = result.mic.peak_db.unwrap();
    assert!((-7.0..=-5.0).contains(&peak), "{peak}");
    assert_eq!(result.mic.device_samplerate, Some(48_000));
}
