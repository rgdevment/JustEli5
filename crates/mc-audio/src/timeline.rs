//! Port of the original Python app's audio timeline.
//!
//! An audio device delivers blocks, not a continuous clock: WASAPI loopback may
//! send nothing while the system is silent, and a stall or a reopened stream
//! drops time. The aligner decides how much silence to write before each block
//! so that sample N of the WAV always means N / samplerate seconds after
//! session start (`mono_ms == 0`).

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioError {
    /// A parameter that must be positive (or non-negative) was not.
    InvalidParameter(&'static str),
    /// The sample buffer does not hold a whole number of frames.
    BadShape { len: usize, channels: usize },
    /// The device or the audio library is not there (not a transient failure).
    Unavailable(String),
    /// A device failure while reading or opening a stream.
    Device(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidParameter(what) => write!(f, "invalid parameter: {what}"),
            Self::BadShape { len, channels } => write!(
                f,
                "{len} samples is not a whole number of frames of {channels} channel(s)"
            ),
            Self::Unavailable(what) | Self::Device(what) => f.write_str(what),
        }
    }
}

impl std::error::Error for AudioError {}

/// Interleaved float samples -> mono floats in [-1, 1].
///
/// Channels are averaged (sequentially, like numpy does for the small channel
/// counts devices expose). NaN becomes silence and +/-inf and out-of-range
/// values are clipped.
pub fn downmix(samples: &[f32], channels: usize) -> Result<Vec<f64>, AudioError> {
    if channels == 0 {
        return Err(AudioError::InvalidParameter("channels must be positive"));
    }
    if !samples.len().is_multiple_of(channels) {
        return Err(AudioError::BadShape {
            len: samples.len(),
            channels,
        });
    }
    Ok(samples
        .chunks_exact(channels)
        .map(|frame| {
            let mean = if channels == 1 {
                f64::from(frame[0])
            } else {
                frame.iter().map(|s| f64::from(*s)).sum::<f64>() / channels as f64
            };
            if mean.is_nan() {
                0.0
            } else {
                mean.clamp(-1.0, 1.0)
            }
        })
        .collect())
}

/// One float sample -> int16. The scale is 32767 and conversion truncates
/// toward zero, exactly as the Python recorder, so WAV files are identical.
pub fn quantize(sample: f64) -> i16 {
    // Within [-32767, 32767] after the clamp; `as` truncates toward zero.
    (sample.clamp(-1.0, 1.0) * 32767.0) as i16
}

/// Interleaved float samples in [-1, 1] -> mono int16 ([`downmix`] + [`quantize`]).
pub fn to_mono_int16(samples: &[f32], channels: usize) -> Result<Vec<i16>, AudioError> {
    Ok(downmix(samples, channels)?
        .into_iter()
        .map(quantize)
        .collect())
}

/// Computes the silence to write before each block of one audio channel.
///
/// `end_mono_ms` is the session-clock time at which the block finished being
/// captured. The first block is always padded up to its expected start so
/// sample 0 corresponds to session t0; later blocks are padded only when the
/// gap exceeds the tolerance, so normal scheduling jitter never inserts
/// silence. A channel that runs ahead of the clock is never trimmed (audio is
/// never discarded); the drift is only reported.
#[derive(Debug, Clone)]
pub struct TimelineAligner {
    samplerate: u32,
    tolerance_frames: i64,
    frames_written: u64,
    padded_frames: u64,
    started: bool,
}

impl TimelineAligner {
    pub fn new(samplerate: u32, gap_tolerance_ms: f64) -> Result<Self, AudioError> {
        if samplerate == 0 {
            return Err(AudioError::InvalidParameter("samplerate must be positive"));
        }
        if !gap_tolerance_ms.is_finite() || gap_tolerance_ms < 0.0 {
            return Err(AudioError::InvalidParameter(
                "gap_tolerance_ms must be finite and not negative",
            ));
        }
        Ok(Self {
            samplerate,
            tolerance_frames: frames_for(gap_tolerance_ms, samplerate),
            frames_written: 0,
            padded_frames: 0,
            started: false,
        })
    }

    /// Default tolerance of the Python recorder (150 ms).
    pub fn with_defaults(samplerate: u32) -> Result<Self, AudioError> {
        Self::new(samplerate, 150.0)
    }

    pub fn samplerate(&self) -> u32 {
        self.samplerate
    }

    pub fn frames_written(&self) -> u64 {
        self.frames_written
    }

    pub fn padded_frames(&self) -> u64 {
        self.padded_frames
    }

    /// Frames of silence to write BEFORE this block; accounts for both the
    /// padding and the block itself. A non-finite timestamp (Python's `round`
    /// raises on it) is rejected without touching the counters, so a bad clock
    /// reading can never become an enormous run of silence.
    pub fn place(&mut self, n_frames: usize, end_mono_ms: f64) -> Result<u64, AudioError> {
        if !end_mono_ms.is_finite() {
            return Err(AudioError::InvalidParameter("end_mono_ms must be finite"));
        }
        let n = n_frames as i64;
        let expected_start = frames_for(end_mono_ms, self.samplerate) - n;
        let written = i64::try_from(self.frames_written).unwrap_or(i64::MAX);
        let gap = expected_start - written;
        let pad = if self.started {
            if gap > self.tolerance_frames { gap } else { 0 }
        } else {
            expected_start.max(0)
        } as u64;
        let total = self
            .frames_written
            .checked_add(pad)
            .and_then(|t| t.checked_add(n_frames as u64))
            .ok_or(AudioError::InvalidParameter("timeline overflow"))?;
        self.started = true;
        self.frames_written = total;
        self.padded_frames += pad;
        Ok(pad)
    }

    /// Account for `n_frames` appended right after the last block, without
    /// padding (the tail a resampler releases when a stream ends).
    pub fn advance(&mut self, n_frames: usize) {
        self.frames_written += n_frames as u64;
        self.started = true;
    }

    /// Audio written so far minus the session clock (positive: audio ahead).
    pub fn drift_ms(&self, end_mono_ms: f64) -> f64 {
        self.frames_written as f64 / f64::from(self.samplerate) * 1000.0 - end_mono_ms
    }
}

/// Python `round(ms * samplerate / 1000)`: round-half-to-even.
pub(crate) fn frames_for(ms: f64, samplerate: u32) -> i64 {
    (ms * f64::from(samplerate) / 1000.0).round_ties_even() as i64
}

/// Sample index <-> session clock for one WAV file whose sample 0 is session t0.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioTimeline {
    samplerate: u32,
}

impl AudioTimeline {
    pub fn new(samplerate: u32) -> Result<Self, AudioError> {
        if samplerate == 0 {
            return Err(AudioError::InvalidParameter("samplerate must be positive"));
        }
        Ok(Self { samplerate })
    }

    pub fn samplerate(&self) -> u32 {
        self.samplerate
    }

    /// Session time (ms) at which `sample` starts.
    pub fn sample_to_mono_ms(&self, sample: u64) -> f64 {
        sample as f64 / f64::from(self.samplerate) * 1000.0
    }

    /// Nearest sample index for a session time; times before t0 map to 0.
    /// A non-finite time has no sample.
    pub fn mono_ms_to_sample(&self, mono_ms: f64) -> Option<u64> {
        mono_ms
            .is_finite()
            .then(|| frames_for(mono_ms.max(0.0), self.samplerate).max(0) as u64)
    }

    /// Duration (ms) of `samples` frames.
    pub fn duration_ms(&self, samples: u64) -> f64 {
        self.sample_to_mono_ms(samples)
    }
}
