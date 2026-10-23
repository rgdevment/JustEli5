//! `mc capture --audio`: consent, options and the recorded audio.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Sandbox, copy_tree, fixture, path_str};
use mc_audio::wav::{WavWriter, read_wav};
use serde_json::Value;

/// Seconds of capture: a slow runner needs nearly two just to start the audio.
const UNTIL: &str = "4";

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

fn wav(sb: &Sandbox, name: &str, rate: u32, seconds: f64) -> PathBuf {
    let path = sb.path(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut w = WavWriter::create(&path, rate).unwrap();
    w.write_samples(&vec![6000; (f64::from(rate) * seconds) as usize])
        .unwrap();
    w.finish().unwrap();
    path
}

fn only_session(sb: &Sandbox) -> PathBuf {
    let root = sb.data().join("corpus");
    let mut dirs: Vec<PathBuf> = fs::read_dir(root)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(dirs.len(), 1, "{dirs:?}");
    dirs.remove(0)
}

fn manifest(dir: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap()
}

#[test]
fn audio_is_recorded_with_its_own_consent() {
    let sb = Sandbox::new();
    let source = long_fake(&sb, 300);
    let mic = wav(&sb, "audio/mic.wav", 16_000, 1.2);
    let system = wav(&sb, "audio/system.wav", 44_100, 1.2);
    let audio_source = format!("wav:{},{}", path_str(&mic), path_str(&system));
    let out = sb.mc(&[
        "capture",
        "--source",
        &source,
        "--poll-ms",
        "20",
        "--consent",
        "--audio",
        "--consent-audio",
        "--audio-source",
        &audio_source,
        "--until",
        UNTIL,
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("audio: REC"), "{}", out.stdout);
    assert!(out.stdout.contains("audio: stopped"), "{}", out.stdout);

    let dir = only_session(&sb);
    let m = manifest(&dir);
    assert_eq!(m["consent"]["audio"], true);
    assert_eq!(m["audio"]["mic"]["status"], "ok");
    assert_eq!(m["audio"]["system"]["device_samplerate"], 44_100);
    let (rate, samples) = read_wav(&dir.join("system.wav")).unwrap();
    assert_eq!(rate, 16_000);
    // The whole file made it; past its end the channel keeps time with silence until the stop.
    assert!(
        (18_000..=16_000 * 6).contains(&samples.len()),
        "{}",
        samples.len()
    );
    // Still strictly replayable: audio does not disturb the captions.
    let replay = sb.mc(&["replay", path_str(&dir), "--verify", "--strict"]);
    assert_eq!(replay.code, 0, "{}\n{}", replay.stdout, replay.stderr);
}

#[test]
fn json_output_reports_rec_and_audio_events() {
    let sb = Sandbox::new();
    let source = long_fake(&sb, 300);
    let mic = wav(&sb, "audio/mic.wav", 16_000, 1.0);
    let out = sb.mc(&[
        "--json",
        "capture",
        "--source",
        &source,
        "--poll-ms",
        "20",
        "--consent",
        "--audio",
        "--consent-audio",
        "--audio-source",
        &format!("wav:{}", path_str(&mic)),
        "--until",
        UNTIL,
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let events: Vec<Value> = out
        .stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let rec: Vec<bool> = events
        .iter()
        .filter(|e| e["event"] == "rec")
        .map(|e| e["recording"].as_bool().unwrap())
        .collect();
    assert_eq!(rec, [true, false]);
    // The system channel has no file: reported, and the recording went on.
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "audio" && e["message"].as_str().unwrap().contains("system"))
    );
    assert_eq!(events.last().unwrap()["event"], "ended");
}

#[test]
fn audio_needs_its_own_consent_without_a_terminal() {
    let sb = Sandbox::new();
    let source = long_fake(&sb, 10);
    let out = sb.mc(&[
        "capture",
        "--source",
        &source,
        "--consent",
        "--audio",
        "--poll-ms",
        "20",
    ]);
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(out.stderr.contains("--consent-audio"), "{}", out.stderr);
    assert!(!sb.data().join("corpus").exists());
}

#[test]
fn audio_options_are_validated() {
    let sb = Sandbox::new();
    let source = long_fake(&sb, 10);
    for args in [
        vec!["--consent", "--consent-audio"],
        vec!["--consent", "--audio-source", "wav:x.wav"],
        vec!["--no-record", "--audio"],
        vec![
            "--consent",
            "--audio",
            "--consent-audio",
            "--audio-source",
            "alsa:0",
        ],
        vec![
            "--consent",
            "--audio",
            "--consent-audio",
            "--audio-source",
            "wav:",
        ],
    ] {
        let mut full = vec!["capture", "--source", &source, "--poll-ms", "20"];
        full.extend(args.iter());
        let out = sb.mc(&full);
        assert_eq!(out.code, 2, "{args:?}: {}", out.stderr);
    }
    assert!(!sb.data().join("corpus").exists());
}

// Windows and macOS have a real backend: there it would open the microphone.
#[cfg(not(any(windows, target_os = "macos")))]
#[test]
fn without_a_backend_the_captions_still_record_and_audio_is_unavailable() {
    let sb = Sandbox::new();
    let source = long_fake(&sb, 20);
    let out = sb.mc(&[
        "capture",
        "--source",
        &source,
        "--poll-ms",
        "20",
        "--consent",
        "--audio",
        "--consent-audio",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(out.stderr.contains("no audio backend"), "{}", out.stderr);
    let m = manifest(&only_session(&sb));
    assert_eq!(m["audio"]["mic"]["status"], "unavailable");
    assert_eq!(m["end_reason"], "source_ended");
}

fn voice_wav(sb: &Sandbox, name: &str) -> PathBuf {
    let path = sb.path(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut w = WavWriter::create(&path, 16_000).unwrap();
    let silence = vec![0i16; 8_000];
    let tone: Vec<i16> = (0..16_000)
        .map(|n| (0.3 * (n as f32 * 0.25).sin() * 32767.0) as i16)
        .collect();
    for part in [&silence, &tone, &silence, &silence] {
        w.write_samples(part).unwrap();
    }
    w.finish().unwrap();
    path
}

#[test]
fn live_recognition_writes_a_draft_and_reports_each_phrase() {
    let sb = Sandbox::new();
    let source = long_fake(&sb, 400);
    let mic = voice_wav(&sb, "audio/voice.wav");
    let out = sb.mc(&[
        "--json",
        "capture",
        "--source",
        &source,
        "--poll-ms",
        "20",
        "--consent",
        "--audio",
        "--consent-audio",
        "--audio-source",
        &format!("wav:{}", path_str(&mic)),
        "--live-asr",
        "--asr-engine",
        "fake",
        "--until",
        "4",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    let events: Vec<Value> = out
        .stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let phrases: Vec<&Value> = events.iter().filter(|e| e["event"] == "asr").collect();
    assert_eq!(phrases.len(), 1, "{events:#?}");
    assert_eq!(phrases[0]["text"], "utterance 1");
    assert_eq!(phrases[0]["ch"], "mic");
    let start = phrases[0]["start_ms"].as_f64().unwrap();
    assert!((start - 300.0).abs() < 900.0, "{start}");

    let dir = only_session(&sb);
    let draft = fs::read_to_string(dir.join("asr_live.jsonl")).unwrap();
    assert_eq!(draft.lines().count(), 1, "{draft}");
    let m = manifest(&dir);
    assert_eq!(m["audio"]["live_asr"]["segments"], 1);
    assert_eq!(m["audio"]["live_asr"]["stuck"], false);
    // The draft does not disturb the captions.
    let replay = sb.mc(&["replay", path_str(&dir), "--verify", "--strict"]);
    assert_eq!(replay.code, 0, "{}\n{}", replay.stdout, replay.stderr);
}

#[test]
fn live_recognition_options_are_validated_before_anything_starts() {
    let sb = Sandbox::new();
    let source = long_fake(&sb, 10);
    for args in [
        vec!["--consent", "--live-asr"],
        vec![
            "--consent",
            "--audio",
            "--consent-audio",
            "--asr-engine",
            "fake",
        ],
        vec![
            "--consent",
            "--audio",
            "--consent-audio",
            "--live-asr",
            "--asr-engine",
            "nope",
        ],
        vec![
            "--consent",
            "--audio",
            "--consent-audio",
            "--live-asr",
            "--asr-engine",
            "fake:/missing.json",
        ],
    ] {
        let mut full = vec!["capture", "--source", &source, "--poll-ms", "20"];
        full.extend(args.iter());
        let out = sb.mc(&full);
        assert_ne!(out.code, 0, "{args:?}");
    }
    // The Whisper default needs an installed model (or a build with the engine).
    let out = sb.mc(&[
        "capture",
        "--source",
        &source,
        "--consent",
        "--audio",
        "--consent-audio",
        "--live-asr",
    ]);
    assert_ne!(out.code, 0);
    assert!(
        out.stderr.contains("mc models pull") || out.stderr.contains("--features whisper"),
        "{}",
        out.stderr
    );
    assert!(!sb.data().join("corpus").exists(), "nothing was created");
}
