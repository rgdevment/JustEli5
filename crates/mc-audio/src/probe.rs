//! `mc doctor --audio`: open both channels for a few seconds and report what
//! they hear, without writing anything.

use std::sync::Barrier;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::devices::{echo_score, levels};
use crate::resample::Resampler;
use crate::silence::all_zero;
use crate::source::{AudioBackend, AudioSource, Read, StreamInfo};
use crate::timeline::{AudioError, downmix};

const RATE: u32 = 16_000;
/// Peak below this (dBFS) counts as silence.
const SILENT_DB: f64 = -60.0;
/// Both channels must be at least this loud for an echo verdict.
const ACTIVE_DB: f64 = -50.0;
/// Envelope correlation above this means the mic hears the meeting (echo ~0.85+, unrelated speech ~0.65).
const ECHO_THRESHOLD: f64 = 0.75;
/// Stream start offset plus speaker latency either way; wider lets unrelated speech match by chance.
const ECHO_MAX_LAG_MS: u32 = 600;
/// Shorter audio gives chance matches and cannot cover every lag.
const ECHO_MIN_SECONDS: f64 = 2.0;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ChannelProbe {
    /// `ok`, `unavailable` or `failed`.
    pub status: &'static str,
    pub device: Option<String>,
    pub device_samplerate: Option<u32>,
    pub peak_db: Option<f64>,
    pub rms_db: Option<f64>,
    pub seconds: f64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Probe {
    pub mic: ChannelProbe,
    pub system: ChannelProbe,
    /// Correlation between both channels in [0, 1]; none unless both were heard.
    pub echo: Option<f64>,
    pub notes: Vec<String>,
}

impl Probe {
    /// Neither channel could be opened.
    pub fn unusable(&self) -> bool {
        self.mic.status != "ok" && self.system.status != "ok"
    }
}

fn db(level: f64) -> f64 {
    (20.0 * level.max(1e-5).log10() * 10.0).round() / 10.0
}

type Captured = (StreamInfo, Vec<f32>);

/// Reads `seconds` of audio and returns it as mono 16 kHz.
fn capture(
    open: impl FnOnce() -> Result<Box<dyn AudioSource>, AudioError>,
    seconds: f64,
    start: &Barrier,
) -> Result<Captured, AudioError> {
    let opened = open();
    // Both channels start listening together, whatever each took to open.
    start.wait();
    let mut source = opened?;
    let info = source.info().clone();
    let mut resampler = Resampler::new(info.samplerate, RATE)?;
    let channels = usize::from(info.channels.max(1));
    let block = (info.samplerate / 10).max(1) as usize;
    let mut mono = Vec::new();
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < end {
        match source.read(block)? {
            Read::Block(samples) => {
                let down = downmix(&samples, channels)?;
                mono.extend(resampler.process(&down).into_iter().map(|v| v as f32));
            }
            Read::Idle => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    Ok((info, mono))
}

fn summarize(result: Result<Captured, AudioError>, seconds: f64) -> (ChannelProbe, Vec<f32>) {
    match result {
        Ok((info, mono)) => {
            let (peak, rms) = levels(&mono);
            (
                ChannelProbe {
                    status: "ok",
                    device: Some(info.name),
                    device_samplerate: Some(info.samplerate),
                    peak_db: Some(db(peak)),
                    rms_db: Some(db(rms)),
                    seconds,
                    error: None,
                },
                mono,
            )
        }
        Err(error) => (
            ChannelProbe {
                status: if matches!(error, AudioError::Unavailable(_)) {
                    "unavailable"
                } else {
                    "failed"
                },
                device: None,
                device_samplerate: None,
                peak_db: None,
                rms_db: None,
                seconds,
                error: Some(error.to_string()),
            },
            Vec::new(),
        ),
    }
}

/// Listens on both channels at the same time (so an echo can be seen).
pub fn probe(
    backend: &dyn AudioBackend,
    mic_device: Option<&str>,
    system_device: Option<&str>,
    seconds: f64,
) -> Probe {
    let seconds = seconds.clamp(0.5, 60.0);
    let start = Barrier::new(2);
    let (mic, system) = std::thread::scope(|scope| {
        let mic = scope.spawn(|| capture(|| backend.open_mic(mic_device), seconds, &start));
        let system =
            scope.spawn(|| capture(|| backend.open_system(system_device), seconds, &start));
        (
            mic.join()
                .unwrap_or_else(|_| Err(AudioError::Device("mic probe panicked".into()))),
            system
                .join()
                .unwrap_or_else(|_| Err(AudioError::Device("system probe panicked".into()))),
        )
    });
    let (mic, mic_audio) = summarize(mic, seconds);
    let (system, system_audio) = summarize(system, seconds);

    let mut notes = Vec::new();
    let mut echo = None;
    let loud = |p: &ChannelProbe| p.peak_db.is_some_and(|d| d > SILENT_DB);
    notes.extend(backend.system_notes(system_device));
    // Exact zeros are how macOS answers a denied permission; the backend knows the fix.
    let zero_hint = |ch: &str, audio: &[f32]| {
        all_zero(audio)
            .then(|| backend.silence_alert(ch, system_device))
            .flatten()
            .filter(|alert| alert.playing.as_ref().is_none_or(|playing| playing()))
            .map(|alert| alert.message)
    };
    if mic.status == "ok" && !loud(&mic) {
        notes.push(zero_hint("mic", &mic_audio).map_or_else(
            || "mic: no signal; is it muted or the wrong device?".to_string(),
            |hint| format!("mic: {hint}"),
        ));
    }
    if system.status == "ok" && !loud(&system) {
        notes.push(zero_hint("system", &system_audio).map_or_else(
            || "system: no signal; play something from the meeting app while this runs".to_string(),
            |hint| format!("system: {hint}"),
        ));
    }
    let active = |p: &ChannelProbe| p.peak_db.is_some_and(|d| d > ACTIVE_DB);
    let long_enough = |a: &[f32]| a.len() as f64 >= ECHO_MIN_SECONDS * f64::from(RATE);
    if active(&mic) && active(&system) && !(long_enough(&mic_audio) && long_enough(&system_audio)) {
        notes.push("echo: probe too short to judge (use --audio-seconds 3)".to_string());
    } else if active(&mic) && active(&system) {
        let score = echo_score(&mic_audio, &system_audio, RATE, ECHO_MAX_LAG_MS);
        echo = Some((score * 100.0).round() / 100.0);
        if score >= ECHO_THRESHOLD {
            notes.push(format!(
                "echo: the meeting audio reaches your microphone (score {score:.2}); use headphones"
            ));
        }
    }
    Probe {
        mic,
        system,
        echo,
        notes,
    }
}

pub fn format_probe(probe: &Probe, devices: &[crate::devices::DeviceInfo]) -> String {
    let mut out = String::new();
    if !devices.is_empty() {
        out.push_str("audio devices:\n");
        for d in devices {
            out.push_str(&format!(
                "  {:<6} {}{}{}\n",
                d.direction,
                d.name,
                d.samplerate.map_or(String::new(), |r| format!(
                    "  {r} Hz x{}",
                    d.channels.unwrap_or(0)
                )),
                if d.default { "  (default)" } else { "" }
            ));
        }
    }
    for (name, c) in [("mic", &probe.mic), ("system", &probe.system)] {
        match c.status {
            "ok" => out.push_str(&format!(
                "audio {name:<6} ok  {}  peak {:.1} dBFS  rms {:.1} dBFS\n",
                c.device.as_deref().unwrap_or("?"),
                c.peak_db.unwrap_or(f64::NEG_INFINITY),
                c.rms_db.unwrap_or(f64::NEG_INFINITY),
            )),
            status => out.push_str(&format!(
                "audio {name:<6} {}  {}\n",
                if status == "failed" { "FAIL" } else { "warn" },
                c.error.as_deref().unwrap_or("")
            )),
        }
    }
    for note in &probe.notes {
        out.push_str(&format!("audio note    {note}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::silence::SilenceAlert;
    use crate::source::{FakeAudio, FakeBackend, Step};

    struct Explains(FakeBackend);

    impl AudioBackend for Explains {
        fn open_mic(&self, d: Option<&str>) -> Result<Box<dyn AudioSource>, AudioError> {
            self.0.open_mic(d)
        }

        fn open_system(&self, d: Option<&str>) -> Result<Box<dyn AudioSource>, AudioError> {
            self.0.open_system(d)
        }

        fn system_notes(&self, _d: Option<&str>) -> Vec<String> {
            vec!["system: via a fake".to_string()]
        }

        fn silence_alert(&self, ch: &str, _d: Option<&str>) -> Option<SilenceAlert> {
            Some(SilenceAlert {
                after_s: 1.0,
                message: format!("{ch} zeros are a permission"),
                playing: None,
            })
        }
    }

    fn steady(level: f32) -> FakeAudio {
        let steps = (0..40)
            .map(|_| Step::Block {
                data: vec![level; 1_600],
                advance_ms: 0.0,
            })
            .collect();
        FakeAudio::mono(16_000, steps, None)
    }

    #[test]
    fn exact_zeros_get_the_backends_explanation_and_real_audio_does_not() {
        let backend = Explains(FakeBackend::new(
            vec![Ok(steady(0.0))],
            vec![Ok(steady(0.3))],
        ));
        let probe = probe(&backend, None, None, 0.5);
        assert!(probe.notes.contains(&"system: via a fake".to_string()));
        assert!(
            probe
                .notes
                .contains(&"mic: mic zeros are a permission".to_string())
        );
        assert!(!probe.notes.iter().any(|n| n.starts_with("system: system")));
    }
}
