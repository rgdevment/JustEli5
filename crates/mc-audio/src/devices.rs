//! Device listing and the signal checks behind `mc doctor --audio`.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeviceInfo {
    pub name: String,
    pub id: Option<String>,
    /// `"input"` or `"output"` (an output can be captured as loopback).
    pub direction: &'static str,
    pub default: bool,
    pub samplerate: Option<u32>,
    pub channels: Option<u16>,
}

/// Level of a block: (peak, rms), both in [0, 1].
pub fn levels(samples: &[f32]) -> (f64, f64) {
    if samples.is_empty() {
        return (0.0, 0.0);
    }
    let mut peak = 0.0f64;
    let mut sum = 0.0f64;
    for s in samples {
        let v = f64::from(*s);
        if v.is_finite() {
            peak = peak.max(v.abs());
            sum += v * v;
        }
    }
    (peak.min(1.0), (sum / samples.len() as f64).sqrt().min(1.0))
}

/// Envelope frame: 20 ms follows syllables, which survive a room's reverb.
const ENVELOPE_FRAME_MS: u32 = 20;
/// Mean energy below this is a noise floor, which Pearson would scale up to a fake match.
const MIN_MEAN_ENERGY: f64 = 1e-9;

/// Best loudness-envelope correlation of both signals over lags up to `max_lag_ms`, in [0, 1].
pub fn echo_score(mic: &[f32], system: &[f32], rate: u32, max_lag_ms: u32) -> f64 {
    let frame = (rate * ENVELOPE_FRAME_MS / 1000).max(1) as usize;
    if mean_energy(mic) < MIN_MEAN_ENERGY || mean_energy(system) < MIN_MEAN_ENERGY {
        return 0.0;
    }
    let (mic, system) = (envelope(mic, frame), envelope(system, frame));
    let n = mic.len().min(system.len());
    if n < 4 {
        return 0.0;
    }
    let max_lag = max_lag_ms.div_ceil(ENVELOPE_FRAME_MS).max(1) as isize;
    let mut best = 0.0f64;
    for lag in -max_lag..=max_lag {
        // lag > 0: the mic hears it `lag` frames after the system stream has it.
        let len = n.saturating_sub(lag.unsigned_abs());
        if len < n / 2 || len < 4 {
            continue;
        }
        let (m0, s0) = if lag >= 0 {
            (lag as usize, 0)
        } else {
            (0, (-lag) as usize)
        };
        best = best.max(pearson(&mic[m0..m0 + len], &system[s0..s0 + len]));
    }
    best.clamp(0.0, 1.0)
}

fn mean_energy(samples: &[f32]) -> f64 {
    let sum: f64 = samples
        .iter()
        .map(|v| f64::from(*v))
        .filter(|v| v.is_finite())
        .map(|v| v * v)
        .sum();
    sum / samples.len().max(1) as f64
}

/// RMS of each whole `frame` of samples (non-finite samples count as silence).
fn envelope(samples: &[f32], frame: usize) -> Vec<f64> {
    samples
        .chunks_exact(frame)
        .map(|c| {
            let sum: f64 = c
                .iter()
                .map(|v| f64::from(*v))
                .filter(|v| v.is_finite())
                .map(|v| v * v)
                .sum();
            (sum / frame as f64).sqrt()
        })
        .collect()
}

/// Pearson correlation; 0 when either side is flat (silence).
fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        let (dx, dy) = (x - ma, y - mb);
        ab += dx * dy;
        aa += dx * dx;
        bb += dy * dy;
    }
    let norm = (aa * bb).sqrt();
    if norm < 1e-12 { 0.0 } else { ab / norm }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{sine_block, speech_like_block};

    #[test]
    fn levels_of_a_known_signal() {
        let block = sine_block(16_000, 1, 440.0, 0.5, 0, 16_000);
        let (peak, rms) = levels(&block);
        assert!((peak - 0.5).abs() < 0.01);
        assert!((rms - 0.5 / 2f64.sqrt()).abs() < 0.01);
        assert_eq!(levels(&[]), (0.0, 0.0));
        assert_eq!(levels(&[f32::NAN, 0.25]).0, 0.25);
    }

    fn noise(seed: u32, n: usize) -> Vec<f32> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (x >> 8) as f32 / (1 << 24) as f32 - 0.5
            })
            .collect()
    }

    /// Speaker, room and mic: delayed, quiet, 300 ms reverb tail, noise floor.
    fn through_a_room(system: &[f32], delay: usize, gain: f32) -> Vec<f32> {
        let tail = 4_800;
        let ir: Vec<f32> = noise(5, tail)
            .iter()
            .enumerate()
            .map(|(i, v)| v * (-6.9 * i as f32 / tail as f32).exp())
            .collect();
        let floor = noise(11, system.len());
        (0..system.len())
            .map(|i| {
                let direct = if i >= delay { system[i - delay] } else { 0.0 };
                let reverb: f32 = (1..tail)
                    .step_by(16)
                    .filter(|k| i >= delay + k)
                    .map(|k| system[i - delay - k] * ir[k])
                    .sum();
                gain * (direct + 0.5 * reverb) + 0.001 * floor[i]
            })
            .collect()
    }

    #[test]
    fn a_speaker_heard_through_a_room_scores_high() {
        let system = speech_like_block(1, 48_000, 1.0);
        let mic = through_a_room(&system, 4_000, 0.05);
        assert!(echo_score(&mic, &system, 16_000, 1_000) > 0.85);
    }

    #[test]
    fn an_echo_is_found_whichever_stream_started_first() {
        let system = speech_like_block(3, 48_000, 1.0);
        let mic = through_a_room(&system, 300, 0.5);
        let late_system: Vec<f32> = std::iter::repeat_n(0.0, 3_200)
            .chain(system.iter().copied())
            .take(system.len())
            .collect();
        assert!(echo_score(&mic, &late_system, 16_000, 1_000) > 0.85);
        assert!(echo_score(&late_system, &mic, 16_000, 1_000) > 0.85);
    }

    #[test]
    fn unrelated_speech_or_silence_scores_low() {
        let (a, b) = (
            speech_like_block(1, 48_000, 1.0),
            speech_like_block(99, 48_000, 1.0),
        );
        assert!(echo_score(&through_a_room(&b, 4_000, 0.05), &a, 16_000, 1_000) < 0.6);
        assert_eq!(echo_score(&a, &vec![0.0; 48_000], 16_000, 1_000), 0.0);
        let floor: Vec<f32> = noise(4, 48_000).iter().map(|v| v * 1e-5).collect();
        assert_eq!(echo_score(&a, &floor, 16_000, 1_000), 0.0);
        assert_eq!(echo_score(&a[..10], &b[..10], 16_000, 1_000), 0.0);
    }
}
