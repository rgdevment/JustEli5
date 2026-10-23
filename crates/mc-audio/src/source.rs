//! Where audio comes from: the [`AudioSource`] trait and a scripted fake.
//!
//! Real sources (microphone, WASAPI loopback, a macOS device) are built on the
//! recording thread by an [`Opener`], so they never need to be `Send`.

use std::collections::VecDeque;
use std::f64::consts::PI;
use std::sync::{Arc, Mutex};

use mc_core::clock::FakeClock;

use crate::silence::SilenceAlert;
use crate::timeline::AudioError;

#[derive(Debug, Clone, PartialEq)]
pub struct StreamInfo {
    pub name: String,
    pub device_id: Option<String>,
    pub samplerate: u32,
    pub channels: u16,
    /// Device latency in seconds, recorded for offline correction.
    pub latency_s: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Read {
    /// Interleaved float samples (a whole number of frames).
    Block(Vec<f32>),
    /// Nothing arrived within the source's own short wait.
    Idle,
}

pub trait AudioSource {
    fn info(&self) -> &StreamInfo;

    /// Up to `n_frames` frames. Must not block for long (an `Idle` return is
    /// how a source gives the recorder a chance to notice a stop request).
    fn read(&mut self, n_frames: usize) -> Result<Read, AudioError>;

    /// Blocks or buffers the source had to drop (overruns, discontinuities).
    fn dropped(&self) -> u64 {
        0
    }
}

/// Builds a fresh source; called again to reopen a lost device.
pub type Opener = Box<dyn FnMut() -> Result<Box<dyn AudioSource>, AudioError> + Send>;

/// Opens the mic and system channels. Shared across the channel threads.
pub trait AudioBackend: Send + Sync {
    fn open_mic(&self, device: Option<&str>) -> Result<Box<dyn AudioSource>, AudioError>;
    fn open_system(&self, device: Option<&str>) -> Result<Box<dyn AudioSource>, AudioError>;
    /// Library / driver versions for the manifest.
    fn versions(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    /// Devices the machine offers (for `mc doctor --audio`).
    fn devices(&self) -> Vec<crate::devices::DeviceInfo> {
        Vec::new()
    }

    /// The system channel aimed at `meeting_pid`, the process that owns the caption
    /// window (when known); a backend with no per-process capture ignores it.
    fn open_system_for(
        &self,
        device: Option<&str>,
        _meeting_pid: Option<u32>,
    ) -> Result<Box<dyn AudioSource>, AudioError> {
        self.open_system(device)
    }

    /// What the backend does for the system channel, for `mc doctor --audio`.
    fn system_notes(&self, _device: Option<&str>) -> Vec<String> {
        Vec::new()
    }

    /// A run of exact zeros on `channel` the backend knows how to explain
    /// (a denied permission the OS answers with silence).
    fn silence_alert(&self, _channel: &str, _system_device: Option<&str>) -> Option<SilenceAlert> {
        None
    }
}

/// One scripted step of a [`FakeAudio`].
#[derive(Debug, Clone)]
pub enum Step {
    /// Deliver `data` after moving the fake clock forward `advance_ms`.
    Block { data: Vec<f32>, advance_ms: f64 },
    /// Fail the read once.
    Fail(String),
}

/// Scripted source for tests. When the script runs out it reports `Idle`.
pub struct FakeAudio {
    info: StreamInfo,
    steps: VecDeque<Step>,
    clock: Option<Arc<FakeClock>>,
}

impl FakeAudio {
    pub fn new(info: StreamInfo, steps: Vec<Step>, clock: Option<Arc<FakeClock>>) -> Self {
        Self {
            info,
            steps: steps.into(),
            clock,
        }
    }

    pub fn mono(samplerate: u32, steps: Vec<Step>, clock: Option<Arc<FakeClock>>) -> Self {
        Self::new(
            StreamInfo {
                name: "fake".to_string(),
                device_id: Some("fake-0".to_string()),
                samplerate,
                channels: 1,
                latency_s: Some(0.01),
            },
            steps,
            clock,
        )
    }
}

impl AudioSource for FakeAudio {
    fn info(&self) -> &StreamInfo {
        &self.info
    }

    fn read(&mut self, _n_frames: usize) -> Result<Read, AudioError> {
        match self.steps.pop_front() {
            None => {
                std::thread::sleep(std::time::Duration::from_millis(1));
                Ok(Read::Idle)
            }
            Some(Step::Fail(message)) => Err(AudioError::Device(message)),
            Some(Step::Block { data, advance_ms }) => {
                if let Some(clock) = &self.clock {
                    clock.advance_ms(advance_ms);
                }
                Ok(Read::Block(data))
            }
        }
    }
}

/// `frames` interleaved frames of a sine, starting at frame `start`.
pub fn sine_block(
    samplerate: u32,
    channels: usize,
    freq: f64,
    amplitude: f64,
    start: usize,
    frames: usize,
) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames * channels);
    for n in start..start + frames {
        let v = (amplitude * (2.0 * PI * freq * n as f64 / f64::from(samplerate)).sin()) as f32;
        out.extend(std::iter::repeat_n(v, channels));
    }
    out
}

/// Noise gated in 100 ms bursts at 16 kHz: the loudness envelope of speech.
pub fn speech_like_block(seed: u32, n: usize, gain: f32) -> Vec<f32> {
    let mut x = seed;
    let mut next = move || {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (x >> 8) as f32 / (1 << 24) as f32 - 0.5
    };
    let mut gate = 1.0;
    (0..n)
        .map(|i| {
            if i % 1_600 == 0 {
                gate = if next() > 0.0 { 1.0 } else { 0.02 };
            }
            next() * gate * gain
        })
        .collect()
}

/// Backend whose `open_*` calls pop pre-built results, one per call.
#[derive(Default)]
pub struct FakeBackend {
    mic: Mutex<VecDeque<Result<FakeAudio, AudioError>>>,
    system: Mutex<VecDeque<Result<FakeAudio, AudioError>>>,
}

impl FakeBackend {
    pub fn new(
        mic: Vec<Result<FakeAudio, AudioError>>,
        system: Vec<Result<FakeAudio, AudioError>>,
    ) -> Self {
        Self {
            mic: Mutex::new(mic.into()),
            system: Mutex::new(system.into()),
        }
    }

    fn pop(
        queue: &Mutex<VecDeque<Result<FakeAudio, AudioError>>>,
    ) -> Result<Box<dyn AudioSource>, AudioError> {
        let next = queue.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
        match next {
            Some(Ok(source)) => Ok(Box::new(source)),
            Some(Err(e)) => Err(e),
            None => Err(AudioError::Unavailable("no more fake devices".to_string())),
        }
    }
}

impl AudioBackend for FakeBackend {
    fn open_mic(&self, _device: Option<&str>) -> Result<Box<dyn AudioSource>, AudioError> {
        Self::pop(&self.mic)
    }

    fn open_system(&self, _device: Option<&str>) -> Result<Box<dyn AudioSource>, AudioError> {
        Self::pop(&self.system)
    }
}
