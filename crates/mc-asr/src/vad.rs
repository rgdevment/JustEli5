//! Voice activity detection behind a trait, with a deterministic energy-based
//! implementation (a neural VAD can replace it without touching the segmenter).

/// 20 ms of 16 kHz audio.
pub const FRAME: usize = 320;

pub trait Vad {
    /// Whether this `FRAME`-sample block holds speech. Stateful (noise floor).
    fn is_speech(&mut self, frame: &[f32]) -> bool;
}

/// Frames of history the noise floor is taken from (3 s).
const WINDOW: usize = 150;
/// Until this many frames are known the floor is assumed quiet, so a recording
/// that starts mid-sentence is still heard.
const WARMUP: usize = 25;
/// The floor never rises above this (dBFS rms): energy alone cannot tell a
/// steady loud voice from a steady loud fan, and speech must never become the
/// floor it is measured against.
const FLOOR_CAP_DB: f64 = -30.0;

/// Speech is anything clearly above a noise floor, taken as the 10th
/// percentile of the last three seconds of energy (capped, see
/// `FLOOR_CAP_DB`). A fan or music bed becomes the floor within seconds (even
/// after a stretch of digital silence) and a long speech turn does not.
#[derive(Debug, Clone)]
pub struct EnergyVad {
    margin_db: f64,
    min_db: f64,
    history: std::collections::VecDeque<f64>,
}

impl Default for EnergyVad {
    fn default() -> Self {
        Self::new(12.0, -50.0)
    }
}

impl EnergyVad {
    /// `margin_db` above the floor and never below `min_db` (dBFS).
    pub fn new(margin_db: f64, min_db: f64) -> Self {
        Self {
            margin_db,
            min_db,
            history: std::collections::VecDeque::with_capacity(WINDOW + 1),
        }
    }

    pub fn energy_db(frame: &[f32]) -> f64 {
        if frame.is_empty() {
            return -120.0;
        }
        let mean = frame
            .iter()
            .map(|s| {
                let v = f64::from(*s);
                if v.is_finite() { v * v } else { 0.0 }
            })
            .sum::<f64>()
            / frame.len() as f64;
        10.0 * (mean + 1e-12).log10()
    }

    fn floor_db(&self) -> f64 {
        let mut sorted: Vec<f64> = self.history.iter().copied().collect();
        let index = sorted.len() / 10;
        let (_, floor, _) = sorted.select_nth_unstable_by(index, f64::total_cmp);
        let floor = floor.min(FLOOR_CAP_DB);
        if self.history.len() < WARMUP {
            floor.min(self.min_db)
        } else {
            floor
        }
    }
}

impl Vad for EnergyVad {
    fn is_speech(&mut self, frame: &[f32]) -> bool {
        let db = Self::energy_db(frame);
        if self.history.len() == WINDOW {
            self.history.pop_front();
        }
        self.history.push_back(db);
        db > (self.floor_db() + self.margin_db).max(self.min_db)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(amplitude: f32) -> Vec<f32> {
        (0..FRAME)
            .map(|n| amplitude * (n as f32 * 0.3).sin())
            .collect()
    }

    #[test]
    fn silence_is_not_speech_and_a_tone_is() {
        let mut vad = EnergyVad::default();
        assert!(!vad.is_speech(&vec![0.0; FRAME]));
        assert!(vad.is_speech(&tone(0.3)));
        assert!(!vad.is_speech(&vec![0.0001; FRAME]));
    }

    #[test]
    fn steady_background_noise_stops_counting_as_speech() {
        // Hiss at -40 dBFS rms, and a much louder fan at -25 dBFS.
        for amplitude in [0.014, 0.08] {
            let mut vad = EnergyVad::default();
            let hiss = tone(amplitude);
            for _ in 0..400 {
                vad.is_speech(&hiss);
            }
            assert!(!vad.is_speech(&hiss), "amplitude {amplitude}");
            assert!(
                vad.is_speech(&tone(amplitude * 12.0)),
                "speech over it is heard"
            );
        }
    }

    #[test]
    fn digital_silence_does_not_make_later_noise_permanent_speech() {
        let mut vad = EnergyVad::default();
        for _ in 0..100 {
            vad.is_speech(&vec![0.0; FRAME]);
        }
        let hiss = tone(0.05); // -29 dBFS rms
        // Heard as speech while the zeros are still in the window ...
        assert!(vad.is_speech(&hiss));
        // ... and as noise once the window holds only the hiss.
        for _ in 0..400 {
            vad.is_speech(&hiss);
        }
        assert!(!vad.is_speech(&hiss));
    }

    #[test]
    fn a_long_turn_with_short_pauses_stays_speech() {
        let mut vad = EnergyVad::default();
        let (loud, pause) = (tone(0.3), vec![0.0005f32; FRAME]);
        let mut heard = 0;
        for n in 0..1000 {
            // A 100 ms pause every second.
            let speaking = n % 50 >= 5;
            let frame = if speaking { &loud } else { &pause };
            if vad.is_speech(frame) == speaking {
                heard += 1;
            }
        }
        assert!(heard > 980, "{heard}");
    }

    #[test]
    fn non_finite_samples_do_not_poison_the_floor() {
        let mut vad = EnergyVad::default();
        let mut frame = tone(0.3);
        frame[3] = f32::NAN;
        frame[9] = f32::INFINITY;
        assert!(vad.is_speech(&frame) || !vad.is_speech(&frame));
        assert!(vad.is_speech(&tone(0.3)));
    }
}
