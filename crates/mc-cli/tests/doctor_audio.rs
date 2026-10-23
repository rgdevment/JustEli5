//! `mc doctor --audio`: devices, levels and the echo warning.

mod common;

use common::{Sandbox, path_str};
use mc_audio::source::speech_like_block;
use mc_audio::wav::WavWriter;
use serde_json::Value;

fn noise(seed: u32, n: usize) -> Vec<i16> {
    speech_like_block(seed, n, 0.8 * 32767.0)
        .into_iter()
        .map(|v| v as i16)
        .collect()
}

fn wav(sb: &Sandbox, name: &str, samples: &[i16]) -> String {
    let path = sb.path(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut w = WavWriter::create(&path, 16_000).unwrap();
    w.write_samples(samples).unwrap();
    w.finish().unwrap();
    path_str(&path).to_string()
}

#[test]
fn listens_to_both_channels_and_warns_about_echo() {
    let sb = Sandbox::new();
    let system = noise(5, 35_200);
    let mut mic = vec![0i16; 100];
    mic.extend(system.iter().map(|v| v / 2));
    mic.truncate(system.len());
    let source = format!(
        "wav:{},{}",
        wav(&sb, "a/mic.wav", &mic),
        wav(&sb, "a/sys.wav", &system)
    );
    let out = sb.mc(&[
        "doctor",
        "--audio",
        "--audio-seconds",
        "2.2",
        "--audio-source",
        &source,
    ]);
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    assert!(out.stdout.contains("audio mic"), "{}", out.stdout);
    assert!(out.stdout.contains("headphones"), "{}", out.stdout);

    let json = sb.mc(&[
        "--json",
        "doctor",
        "--audio",
        "--audio-seconds",
        "2.2",
        "--audio-source",
        &source,
    ]);
    let value: Value = serde_json::from_str(&json.stdout).unwrap();
    assert_eq!(value["audio"]["probe"]["mic"]["status"], "ok");
    assert!(value["audio"]["probe"]["echo"].as_f64().unwrap() > 0.8);
    assert_eq!(value["ok"], true);
}

#[test]
fn without_any_channel_the_result_is_negative() {
    let sb = Sandbox::new();
    // No backend on Linux CI; on a real machine this would open the devices.
    let out = sb.mc(&[
        "doctor",
        "--audio",
        "--audio-source",
        "wav:/nonexistent/m.wav",
    ]);
    assert_eq!(out.code, 1, "{}\n{}", out.stdout, out.stderr);
    assert!(out.stdout.contains("audio mic"), "{}", out.stdout);
}

#[test]
fn audio_options_need_the_flag_and_are_validated() {
    let sb = Sandbox::new();
    for args in [
        vec!["doctor", "--audio-seconds", "2"],
        vec!["doctor", "--mic-device", "x"],
        vec!["doctor", "--audio", "--audio-seconds", "0.1"],
        vec!["doctor", "--audio", "--audio-source", "alsa:0"],
    ] {
        let out = sb.mc(&args);
        assert_eq!(out.code, 2, "{args:?}: {}", out.stderr);
    }
}
