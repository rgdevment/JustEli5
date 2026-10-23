//! One recording channel (mic or system): a thread that reads a source,
//! brings it to the WAV rate, keeps it on the session timeline and writes it.
//!
//! Timing note: a block is stamped with the session clock right after `read`
//! returns. The real capture time is earlier by roughly the device latency
//! plus any buffer backlog, so the WAV lags reality slightly. That is not
//! compensated here; the latency is logged (`latency_ms`) for offline
//! correction.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mc_core::clock::Clock;
use mc_core::pytext::py_round3;
use serde::Serialize;
use serde_json::json;

use crate::resample::Resampler;
use crate::silence::{SilenceAlert, Watch, ZeroWatch};
use crate::source::{AudioSource, Opener, Read, StreamInfo};
use crate::sync_log::SyncLog;
use crate::timeline::{AudioError, TimelineAligner, downmix, frames_for, quantize};
use crate::wav::WavWriter;

/// After this much uninterrupted audio the reopen budget is restored.
const HEALTHY_RESET_S: f64 = 30.0;

/// Called from the audio threads with a one-line message.
pub type StatusFn = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct ChannelConfig {
    pub block_ms: u32,
    pub max_reopens: u32,
    pub reopen_wait: Duration,
    pub sync_interval_ms: f64,
    pub gap_tolerance_ms: f64,
    /// Sample rate of the WAV (and of what the ASR will read).
    pub target_rate: u32,
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            block_ms: 100,
            max_reopens: 3,
            reopen_wait: Duration::from_secs(1),
            sync_interval_ms: 10_000.0,
            gap_tolerance_ms: 150.0,
            target_rate: 16_000,
        }
    }
}

/// Audio just written, for consumers that follow the recording (live ASR).
#[derive(Debug, Clone, PartialEq)]
pub struct AudioChunk {
    pub ch: String,
    /// Index in the WAV of the first sample (silence padding is not in `samples`).
    pub start_frame: u64,
    pub samples: Vec<i16>,
}

/// The manifest entry of a channel (same shape as the Python recorder, plus
/// `device_samplerate` and `tap_dropped`).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ChannelInfo {
    pub enabled: bool,
    pub status: String,
    pub device_name: Option<String>,
    pub device_id: Option<String>,
    pub samplerate: u32,
    pub device_samplerate: Option<u32>,
    pub channels_in: Option<u16>,
    pub channels_out: u8,
    pub sample_format: &'static str,
    pub file: String,
    pub latency_ms: Option<f64>,
    pub frames_written: u64,
    pub padded_frames: u64,
    pub max_abs_drift_ms: f64,
    pub tap_dropped: u64,
    /// Something odd that did not stop the channel (e.g. a run of exact zeros).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    pub error: Option<String>,
}

#[derive(Default)]
struct Stop {
    flag: Mutex<bool>,
    cv: Condvar,
}

impl Stop {
    fn set(&self) {
        *self.flag.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.cv.notify_all();
    }

    fn is_set(&self) -> bool {
        *self.flag.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sleep up to `timeout`; true when a stop was requested.
    fn wait(&self, timeout: Duration) -> bool {
        let guard = self.flag.lock().unwrap_or_else(PoisonError::into_inner);
        let (guard, _) = self
            .cv
            .wait_timeout_while(guard, timeout, |stopped| !*stopped)
            .unwrap_or_else(PoisonError::into_inner);
        *guard
    }
}

/// Everything a channel thread owns.
struct Worker {
    ch: String,
    opener: Opener,
    wav_path: PathBuf,
    sync: Arc<SyncLog>,
    clock: Arc<dyn Clock>,
    on_status: Option<StatusFn>,
    cfg: ChannelConfig,
    silence: Option<SilenceAlert>,
    tap: Option<SyncSender<AudioChunk>>,
    stop: Arc<Stop>,
    info: Arc<Mutex<ChannelInfo>>,
}

/// What a channel needs to run.
pub struct ChannelSpec {
    pub ch: String,
    pub opener: Opener,
    pub wav_path: PathBuf,
    pub sync: Arc<SyncLog>,
    pub clock: Arc<dyn Clock>,
    pub on_status: Option<StatusFn>,
    pub cfg: ChannelConfig,
    pub silence: Option<SilenceAlert>,
    pub tap: Option<SyncSender<AudioChunk>>,
}

pub struct ChannelRecorder {
    ch: String,
    stop: Arc<Stop>,
    info: Arc<Mutex<ChannelInfo>>,
    done: Receiver<()>,
    handle: Option<JoinHandle<()>>,
}

impl ChannelRecorder {
    pub fn spawn(spec: ChannelSpec) -> std::io::Result<Self> {
        let ChannelSpec {
            ch,
            opener,
            wav_path,
            sync,
            clock,
            on_status,
            cfg,
            silence,
            tap,
        } = spec;
        let stop = Arc::new(Stop::default());
        let file = wav_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let info = Arc::new(Mutex::new(ChannelInfo {
            enabled: true,
            status: "starting".to_string(),
            device_name: None,
            device_id: None,
            samplerate: cfg.target_rate,
            device_samplerate: None,
            channels_in: None,
            channels_out: 1,
            sample_format: "s16le",
            file,
            latency_ms: None,
            frames_written: 0,
            padded_frames: 0,
            max_abs_drift_ms: 0.0,
            tap_dropped: 0,
            warning: None,
            error: None,
        }));
        let (done_tx, done) = mpsc::channel();
        let worker = Worker {
            ch: ch.clone(),
            opener,
            wav_path,
            sync,
            clock,
            on_status,
            cfg,
            silence,
            tap,
            stop: Arc::clone(&stop),
            info: Arc::clone(&info),
        };
        let handle = std::thread::Builder::new()
            .name(format!("audio-{}", worker.ch))
            .spawn(move || {
                worker.run();
                let _ = done_tx.send(());
            })?;
        Ok(Self {
            ch,
            stop,
            info,
            done,
            handle: Some(handle),
        })
    }

    pub fn channel(&self) -> &str {
        &self.ch
    }

    pub fn request_stop(&self) {
        self.stop.set();
    }

    /// Record that the thread was abandoned in a blocked read, so no view
    /// keeps showing it as recording.
    pub fn mark_stuck(&self) {
        self.info
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .status = "stuck".to_string();
    }

    pub fn info(&self) -> ChannelInfo {
        self.info
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Wait for the thread to end; false when it is still running at the
    /// deadline (a blocked read). A stuck thread is left behind, not killed.
    pub fn join_until(&mut self, deadline: Instant) -> bool {
        let left = deadline.saturating_duration_since(Instant::now());
        match self.done.recv_timeout(left) {
            Err(mpsc::RecvTimeoutError::Timeout) => false,
            _ => {
                if let Some(handle) = self.handle.take() {
                    let _ = handle.join();
                }
                true
            }
        }
    }
}

impl Drop for ChannelRecorder {
    fn drop(&mut self) {
        // A dropped recorder must not leave a thread holding the device.
        self.stop.set();
    }
}

/// Source format -> mono i16 at the WAV rate.
struct Pipeline {
    channels: usize,
    resampler: Option<Resampler>,
}

impl Pipeline {
    fn new(info: &StreamInfo, target_rate: u32) -> Result<Self, AudioError> {
        if info.channels == 0 {
            return Err(AudioError::InvalidParameter("device reports 0 channels"));
        }
        Ok(Self {
            channels: usize::from(info.channels),
            resampler: (info.samplerate != target_rate)
                .then(|| Resampler::new(info.samplerate, target_rate))
                .transpose()?,
        })
    }

    fn convert(&mut self, block: &[f32]) -> Result<Vec<i16>, AudioError> {
        let mono = downmix(block, self.channels)?;
        let mono = match &mut self.resampler {
            Some(r) => r.process(&mono),
            None => mono,
        };
        Ok(mono.into_iter().map(quantize).collect())
    }

    /// What the resampler still holds when the stream ends.
    fn finish(&mut self) -> Vec<i16> {
        self.resampler
            .as_mut()
            .map(|r| r.finish().into_iter().map(quantize).collect())
            .unwrap_or_default()
    }
}

/// Timeline bookkeeping of one channel while it records.
struct Track {
    aligner: TimelineAligner,
    max_drift: f64,
    tap_dropped: u64,
    tap: Option<SyncSender<AudioChunk>>,
}

impl Track {
    /// Offer the samples just accounted in `aligner` to the live consumer.
    fn send(&mut self, ch: &str, samples: Vec<i16>) {
        let Some(tap) = &self.tap else { return };
        let chunk = AudioChunk {
            ch: ch.to_string(),
            start_frame: self.aligner.frames_written() - samples.len() as u64,
            samples,
        };
        // Never block the recording on a slow consumer.
        match tap.try_send(chunk) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => self.tap_dropped += 1,
            Err(TrySendError::Disconnected(_)) => self.tap = None,
        }
    }
}

impl Worker {
    fn set(&self, update: impl FnOnce(&mut ChannelInfo)) {
        update(&mut self.info.lock().unwrap_or_else(PoisonError::into_inner));
    }

    fn status(&self) -> String {
        self.info
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .status
            .clone()
    }

    fn notify(&self, message: &str) {
        if let Some(f) = &self.on_status {
            let _ = catch_unwind(AssertUnwindSafe(|| f(message)));
        }
    }

    fn fail(&self, status: &str, error: &str) {
        self.set(|i| {
            i.status = status.to_string();
            i.error = Some(error.to_string());
        });
        self.sync
            .write(&self.ch, "error", json!({ "error": error }));
        self.notify(&format!("Audio {}: {error}", self.ch));
    }

    /// False when the backend says the zeros are no reason to warn (nothing is playing).
    fn warn_silence(&self) -> bool {
        let Some(alert) = &self.silence else {
            return true;
        };
        if alert.playing.as_ref().is_some_and(|playing| !playing()) {
            return false;
        }
        self.set(|i| i.warning = Some(alert.message.clone()));
        self.sync
            .write(&self.ch, "warning", json!({ "warning": alert.message }));
        self.notify(&format!("Audio {}: {}", self.ch, alert.message));
        true
    }

    fn clear_silence(&self) {
        self.set(|i| i.warning = None);
        self.sync.write(&self.ch, "warning_cleared", json!({}));
        self.notify(&format!("Audio {}: audio now flowing", self.ch));
    }

    fn describe(&self, info: &StreamInfo) {
        self.set(|i| {
            i.device_name = Some(info.name.clone());
            i.device_id = info.device_id.clone();
            i.device_samplerate = Some(info.samplerate);
            i.channels_in = Some(info.channels);
            i.latency_ms = info.latency_s.map(|s| py_round3(s * 1000.0));
        });
    }

    fn run(mut self) {
        let result = catch_unwind(AssertUnwindSafe(|| self.record()));
        if result.is_err() {
            self.fail("failed", "internal error in the audio thread");
        }
        if self.status() == "recording" {
            self.set(|i| i.status = "ok".to_string());
        }
        let frames = self
            .info
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .frames_written;
        self.sync
            .write(&self.ch, "stop", json!({ "frames_written": frames }));
    }

    fn record(&mut self) {
        let mut stream = match (self.opener)() {
            Ok(s) => s,
            Err(AudioError::Unavailable(m)) => {
                self.fail("unavailable", &m);
                return;
            }
            Err(e) => {
                self.fail("failed", &e.to_string());
                return;
            }
        };
        self.describe(stream.info());
        let mut wav = match WavWriter::create(&self.wav_path, self.cfg.target_rate) {
            Ok(w) => w,
            Err(e) => {
                self.fail("failed", &format!("cannot open WAV: {e}"));
                return;
            }
        };
        self.set(|i| i.status = "recording".to_string());
        let started = stream.info().clone();
        self.sync.write(
            &self.ch,
            "start",
            json!({
                "device": started.name,
                "samplerate": self.cfg.target_rate,
                "device_samplerate": started.samplerate,
                "latency_ms": started.latency_s.map(|s| py_round3(s * 1000.0)),
            }),
        );
        self.record_loop(&mut stream, &mut wav);
        if let Err(e) = wav.finish() {
            self.sync.write(
                &self.ch,
                "error",
                json!({ "error": format!("wav finish failed: {e}") }),
            );
        }
    }

    /// Pad to the block's place on the timeline and write it. False when a
    /// stop request interrupted a long silence.
    fn write_block(
        &self,
        track: &mut Track,
        wav: &mut WavWriter,
        samples: Vec<i16>,
        end_mono: f64,
    ) -> Result<bool, String> {
        let pad = track
            .aligner
            .place(samples.len(), end_mono)
            .map_err(|e| e.to_string())?;
        if pad > 0 {
            // In chunks, so a clock jump (resume from sleep) cannot hold up a stop.
            let mut left = pad;
            while left > 0 {
                if self.stop.is_set() {
                    return Ok(false);
                }
                let n = left.min(160_000);
                wav.write_silence(n).map_err(|e| e.to_string())?;
                left -= n;
            }
            self.sync.write(
                &self.ch,
                "gap",
                json!({"pad_frames": pad, "frames_written": track.aligner.frames_written()}),
            );
        }
        wav.write_samples(&samples).map_err(|e| e.to_string())?;
        track.send(&self.ch, samples);
        Ok(true)
    }

    /// Write what the resampler holds right after the last block.
    fn flush_tail(&self, track: &mut Track, wav: &mut WavWriter, pipeline: &mut Pipeline) {
        let tail = pipeline.finish();
        if tail.is_empty() || wav.write_samples(&tail).is_err() {
            return;
        }
        track.aligner.advance(tail.len());
        track.send(&self.ch, tail);
        self.publish(track);
    }

    fn publish(&self, track: &Track) {
        self.set(|i| {
            i.frames_written = track.aligner.frames_written();
            i.padded_frames = track.aligner.padded_frames();
            i.max_abs_drift_ms = py_round3(track.max_drift);
            i.tap_dropped = track.tap_dropped;
        });
    }

    fn record_loop(&mut self, stream: &mut Box<dyn AudioSource>, wav: &mut WavWriter) {
        let aligner = match TimelineAligner::new(self.cfg.target_rate, self.cfg.gap_tolerance_ms) {
            Ok(a) => a,
            Err(e) => {
                self.fail("failed", &e.to_string());
                return;
            }
        };
        let mut track = Track {
            aligner,
            max_drift: 0.0,
            tap_dropped: 0,
            tap: self.tap.take(),
        };
        let mut pipeline = match Pipeline::new(stream.info(), self.cfg.target_rate) {
            Ok(p) => p,
            Err(e) => {
                self.fail("failed", &e.to_string());
                return;
            }
        };
        let mut n_frames = block_frames(stream.info(), self.cfg.block_ms);
        let mut reopens = 0u32;
        let mut healthy_frames = 0u64;
        let mut last_sync_ms = 0.0f64;
        let mut watch = self.silence.as_ref().map(|a| ZeroWatch::new(a.after_s));

        while !self.stop.is_set() {
            let block = match stream.read(n_frames) {
                Ok(Read::Block(b)) if !b.is_empty() => b,
                Ok(_) => {
                    self.stop.wait(Duration::from_millis(10));
                    continue;
                }
                Err(e) => {
                    if self.stop.is_set() {
                        break;
                    }
                    self.sync.write(
                        &self.ch,
                        "error",
                        json!({ "error": format!("read failed: {e}") }),
                    );
                    self.flush_tail(&mut track, wav, &mut pipeline);
                    if !self.reopen(stream, &mut reopens) {
                        return;
                    }
                    healthy_frames = 0;
                    let reopened = stream.info().clone();
                    self.describe(&reopened);
                    n_frames = block_frames(&reopened, self.cfg.block_ms);
                    // The WAV rate is fixed, so a different device format only
                    // needs a new conversion pipeline, not a new timeline.
                    pipeline = match Pipeline::new(&reopened, self.cfg.target_rate) {
                        Ok(p) => p,
                        Err(e) => {
                            self.fail("failed", &e.to_string());
                            return;
                        }
                    };
                    continue;
                }
            };

            let end_mono = self.clock.now().mono_ms;
            if let Some(watch) = &mut watch {
                let info = stream.info();
                let rate = f64::from(info.samplerate) * f64::from(info.channels);
                match watch.feed(&block, rate) {
                    Watch::Silent if !self.warn_silence() => watch.rearm(),
                    Watch::Resumed => self.clear_silence(),
                    _ => {}
                }
            }
            let samples = match pipeline.convert(&block) {
                Ok(s) => s,
                Err(e) => {
                    // One bad block is not worth the channel: log it and go on.
                    self.sync.write(
                        &self.ch,
                        "error",
                        json!({ "error": format!("malformed block dropped: {e}") }),
                    );
                    continue;
                }
            };
            if samples.is_empty() {
                continue;
            }
            let len = samples.len() as u64;
            match self.write_block(&mut track, wav, samples, end_mono) {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => {
                    self.fail("failed", &format!("write failed: {e}"));
                    return;
                }
            }

            healthy_frames += len;
            if reopens > 0
                && healthy_frames as f64 >= HEALTHY_RESET_S * f64::from(self.cfg.target_rate)
            {
                reopens = 0;
            }
            track.max_drift = track.max_drift.max(track.aligner.drift_ms(end_mono).abs());
            if end_mono - last_sync_ms >= self.cfg.sync_interval_ms {
                last_sync_ms = end_mono;
                // Keeps the header honest between syncs, so a crash loses little.
                let _ = wav.flush_header();
                self.sync.write(
                    &self.ch,
                    "sync",
                    json!({
                        "frames_written": track.aligner.frames_written(),
                        "expected_frames": frames_for(end_mono, self.cfg.target_rate),
                        "source_dropped": Some(stream.dropped()).filter(|d| *d > 0),
                    }),
                );
            }
            // After the sync point, so what the info reports is already on disk.
            self.publish(&track);
        }
        self.flush_tail(&mut track, wav, &mut pipeline);
    }

    /// Replace a broken stream. False when the channel is given up.
    fn reopen(&mut self, stream: &mut Box<dyn AudioSource>, reopens: &mut u32) -> bool {
        let info = stream.info().clone();
        drop(std::mem::replace(stream, Box::new(Closed(info))));
        while *reopens < self.cfg.max_reopens && !self.stop.is_set() {
            *reopens += 1;
            if self.stop.wait(self.cfg.reopen_wait) {
                break;
            }
            match (self.opener)() {
                Ok(new_stream) => {
                    self.sync.write(
                        &self.ch,
                        "reopen",
                        json!({ "device": new_stream.info().name }),
                    );
                    *stream = new_stream;
                    return true;
                }
                Err(e) => self
                    .sync
                    .write(&self.ch, "reopen", json!({ "error": e.to_string() })),
            }
        }
        if !self.stop.is_set() {
            self.fail("failed", "audio device lost and could not be reopened");
        }
        false
    }
}

fn block_frames(info: &StreamInfo, block_ms: u32) -> usize {
    ((f64::from(info.samplerate) * f64::from(block_ms) / 1000.0).round_ties_even() as usize).max(1)
}

/// Stand-in while a broken stream is being replaced, so the old device is
/// released before the new one is opened (exclusive-mode devices need that).
struct Closed(StreamInfo);

impl AudioSource for Closed {
    fn info(&self) -> &StreamInfo {
        &self.0
    }

    fn read(&mut self, _n_frames: usize) -> Result<Read, AudioError> {
        Ok(Read::Idle)
    }
}
