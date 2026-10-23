//! Streaming windowed-sinc resampler (mono, f64).
//!
//! Used to bring any device rate to the rate of the WAV (16 kHz by default).
//! Output sample `n` sits at input position `n * in_rate / out_rate`, with
//! silence before the first sample, so sample 0 stays on the session t0. The
//! filter is symmetric: an output needs `half` input samples after its
//! position, which delays it by roughly 1 ms at 48 kHz -> 16 kHz (well under
//! the aligner's tolerance, and not compensated).

use std::f64::consts::PI;

use crate::timeline::AudioError;

/// Zero crossings of the sinc on each side, measured at the lower rate.
const ZEROS: f64 = 16.0;

pub struct Resampler {
    passthrough: bool,
    step: f64,
    cutoff: f64,
    half: i64,
    buf: Vec<f64>,
    /// Absolute input index of `buf[0]`.
    origin: i64,
    total: i64,
    out_index: u64,
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Result<Self, AudioError> {
        if in_rate == 0 || out_rate == 0 {
            return Err(AudioError::InvalidParameter(
                "sample rates must be positive",
            ));
        }
        let cutoff = (f64::from(out_rate) / f64::from(in_rate)).min(1.0);
        Ok(Self {
            passthrough: in_rate == out_rate,
            step: f64::from(in_rate) / f64::from(out_rate),
            cutoff,
            half: (ZEROS / cutoff).ceil() as i64,
            buf: Vec::new(),
            origin: 0,
            total: 0,
            out_index: 0,
        })
    }

    pub fn process(&mut self, input: &[f64]) -> Vec<f64> {
        if self.passthrough {
            return input.to_vec();
        }
        self.buf.extend_from_slice(input);
        self.total += input.len() as i64;
        self.drain(false)
    }

    /// Outputs that were waiting for input beyond the end of the stream.
    pub fn finish(&mut self) -> Vec<f64> {
        if self.passthrough {
            return Vec::new();
        }
        self.drain(true)
    }

    fn drain(&mut self, flush: bool) -> Vec<f64> {
        let mut out = Vec::new();
        loop {
            let pos = self.out_index as f64 * self.step;
            let centre = pos.floor() as i64;
            let ready = if flush {
                pos < self.total as f64
            } else {
                centre + self.half < self.total
            };
            if !ready {
                break;
            }
            let mut acc = 0.0;
            for k in (centre - self.half + 1).max(0)..=(centre + self.half).min(self.total - 1) {
                let x = pos - k as f64;
                let arg = x * self.cutoff;
                let sinc = if arg == 0.0 {
                    1.0
                } else {
                    (PI * arg).sin() / (PI * arg)
                };
                let window = 0.5 * (1.0 + (PI * x / self.half as f64).cos());
                acc += self.buf[(k - self.origin) as usize] * sinc * window;
            }
            out.push(acc * self.cutoff);
            self.out_index += 1;
        }
        let next = (self.out_index as f64 * self.step).floor() as i64 - self.half + 1;
        if next > self.origin {
            let drop = ((next - self.origin) as usize).min(self.buf.len());
            self.buf.drain(..drop);
            self.origin += drop as i64;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, freq: f64, frames: usize) -> Vec<f64> {
        (0..frames)
            .map(|n| 0.5 * (2.0 * PI * freq * n as f64 / f64::from(rate)).sin())
            .collect()
    }

    fn rms(x: &[f64]) -> f64 {
        (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt()
    }

    fn run(in_rate: u32, out_rate: u32, input: &[f64], chunk: usize) -> Vec<f64> {
        let mut r = Resampler::new(in_rate, out_rate).unwrap();
        let mut out = Vec::new();
        for c in input.chunks(chunk) {
            out.extend(r.process(c));
        }
        out.extend(r.finish());
        out
    }

    #[test]
    fn same_rate_is_untouched() {
        let x = tone(16_000, 440.0, 1000);
        assert_eq!(run(16_000, 16_000, &x, 333), x);
    }

    #[test]
    fn keeps_in_band_tone_and_length() {
        let x = tone(48_000, 1000.0, 48_000);
        let y = run(48_000, 16_000, &x, 4_800);
        assert_eq!(y.len(), 16_000);
        let steady = &y[1_000..15_000];
        assert!(
            (rms(steady) - 0.5 / 2f64.sqrt()).abs() < 0.01,
            "{}",
            rms(steady)
        );
    }

    #[test]
    fn rejects_out_of_band_tone() {
        // 10 kHz is above the 8 kHz Nyquist of the output: it must vanish, not alias.
        let x = tone(48_000, 10_000.0, 48_000);
        let y = run(48_000, 16_000, &x, 4_800);
        assert!(rms(&y[1_000..15_000]) < 0.01, "{}", rms(&y[1_000..15_000]));
    }

    #[test]
    fn chunking_does_not_change_the_result() {
        let x = tone(44_100, 700.0, 20_000);
        let whole = run(44_100, 16_000, &x, x.len());
        let pieces = run(44_100, 16_000, &x, 441);
        assert_eq!(whole.len(), pieces.len());
        for (a, b) in whole.iter().zip(&pieces) {
            assert!((a - b).abs() < 1e-12);
        }
    }

    #[test]
    fn upsampling_works_and_memory_stays_bounded() {
        let x = tone(8_000, 500.0, 8_000);
        let y = run(8_000, 16_000, &x, 800);
        assert_eq!(y.len(), 16_000);
        assert!((rms(&y[500..15_500]) - 0.5 / 2f64.sqrt()).abs() < 0.01);
        let mut r = Resampler::new(48_000, 16_000).unwrap();
        for _ in 0..200 {
            r.process(&tone(48_000, 300.0, 4_800));
        }
        assert!(r.buf.len() < 1_000);
    }
}
