//! Telling "nothing is playing" from "the system hands us zeros".
//!
//! macOS does not fail a denied microphone or system-audio permission: the
//! stream opens and delivers exact zeros. A real signal always carries some
//! noise, so a long run of exact zeros on the mic is a strong sign, and on the
//! system channel a hint worth showing.

/// Asks the backend whether something is playing on the system right now.
pub type PlayingCheck = std::sync::Arc<dyn Fn() -> bool + Send + Sync>;

/// What a backend asks the channel to watch for.
#[derive(Clone)]
pub struct SilenceAlert {
    /// Seconds of exact zeros before the warning fires.
    pub after_s: f64,
    pub message: String,
    /// When set, zeros only warn if this says the system is playing: zeros while
    /// something plays mean access is denied, zeros in a quiet room mean nothing.
    pub playing: Option<PlayingCheck>,
}

impl std::fmt::Debug for SilenceAlert {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SilenceAlert")
            .field("after_s", &self.after_s)
            .field("message", &self.message)
            .finish_non_exhaustive()
    }
}

/// What a block did to the watch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Watch {
    Quiet,
    /// The stream has been zeros since its start for the limit.
    Silent,
    /// Real audio arrived after a `Silent`.
    Resumed,
}

/// Watches the start of a stream: a denied permission is zeros from the first
/// sample, while a pause after real audio is just a pause.
pub struct ZeroWatch {
    after_s: f64,
    run_s: f64,
    heard: bool,
    fired: bool,
}

impl ZeroWatch {
    pub fn new(after_s: f64) -> Self {
        Self {
            after_s,
            run_s: 0.0,
            heard: false,
            fired: false,
        }
    }

    /// Feeds one block (`samples_per_second` counts every channel).
    pub fn feed(&mut self, block: &[f32], samples_per_second: f64) -> Watch {
        if block.iter().any(|v| *v != 0.0) {
            self.heard = true;
            return if std::mem::take(&mut self.fired) {
                Watch::Resumed
            } else {
                Watch::Quiet
            };
        }
        if self.heard {
            return Watch::Quiet;
        }
        self.run_s += block.len() as f64 / samples_per_second.max(1.0);
        if self.fired || self.run_s < self.after_s {
            return Watch::Quiet;
        }
        self.fired = true;
        Watch::Silent
    }

    /// The silence was not worth a warning: count the limit again from now.
    pub fn rearm(&mut self) {
        self.run_s = 0.0;
        self.fired = false;
    }
}

/// True when the whole signal is exact zeros (and there is some).
pub fn all_zero(samples: &[f32]) -> bool {
    !samples.is_empty() && samples.iter().all(|v| *v == 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fires_once_after_the_limit_of_leading_zeros() {
        let mut watch = ZeroWatch::new(2.0);
        let second = vec![0.0; 100];
        assert_eq!(watch.feed(&second, 100.0), Watch::Quiet);
        assert_eq!(watch.feed(&second, 100.0), Watch::Silent);
        assert_eq!(watch.feed(&second, 100.0), Watch::Quiet);
    }

    #[test]
    fn real_audio_after_the_warning_reports_it_resumed_once() {
        let mut watch = ZeroWatch::new(1.0);
        assert_eq!(watch.feed(&[0.0; 100], 100.0), Watch::Silent);
        assert_eq!(watch.feed(&[0.1; 10], 100.0), Watch::Resumed);
        assert_eq!(watch.feed(&[0.1; 10], 100.0), Watch::Quiet);
        assert_eq!(watch.feed(&[0.0; 500], 100.0), Watch::Quiet);
    }

    #[test]
    fn a_pause_after_real_audio_is_not_a_denial() {
        let mut watch = ZeroWatch::new(1.0);
        let mut noisy = vec![0.0; 100];
        noisy[10] = 1e-7;
        assert_eq!(watch.feed(&noisy, 100.0), Watch::Quiet);
        assert_eq!(watch.feed(&[0.0; 500], 100.0), Watch::Quiet);
    }

    #[test]
    fn zeros_add_up_across_blocks_and_rearm_restarts_the_count() {
        let mut watch = ZeroWatch::new(1.0);
        assert_eq!(watch.feed(&[0.0; 60], 100.0), Watch::Quiet);
        assert_eq!(watch.feed(&[0.0; 60], 100.0), Watch::Silent);
        watch.rearm();
        assert_eq!(watch.feed(&[0.0; 60], 100.0), Watch::Quiet);
        assert_eq!(watch.feed(&[0.0; 60], 100.0), Watch::Silent);
    }

    #[test]
    fn a_quiet_noise_floor_is_not_silence() {
        let mut watch = ZeroWatch::new(1.0);
        let floor: Vec<f32> = (0..1_000).map(|i| ((i % 3) as f32 - 1.0) * 1e-6).collect();
        assert_eq!(watch.feed(&floor, 100.0), Watch::Quiet);
    }

    #[test]
    fn all_zero_needs_samples() {
        assert!(all_zero(&[0.0, 0.0]));
        assert!(!all_zero(&[]));
        assert!(!all_zero(&[0.0, 0.5]));
    }
}
