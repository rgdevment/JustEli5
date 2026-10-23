//! Mic and system audio recorded side by side into a session folder.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};

use mc_core::clock::Clock;
use serde_json::{Value, json};

use crate::channel::{AudioChunk, ChannelConfig, ChannelRecorder, ChannelSpec, StatusFn};
use crate::source::{AudioBackend, Opener};
use crate::sync_log::SyncLog;
use crate::timeline::AudioError;

pub const CHANNELS: [&str; 2] = ["mic", "system"];

/// Single use: `start` after `stop` does nothing. `on_status` runs on the audio
/// threads, so a UI must route it through its own queue.
pub struct AudioCorpusRecorder {
    out_dir: PathBuf,
    clock: Arc<dyn Clock>,
    backend: Option<Arc<dyn AudioBackend>>,
    mic_device: Option<String>,
    system_device: Option<String>,
    meeting_pid: Option<u32>,
    on_status: Option<StatusFn>,
    cfg: ChannelConfig,
    taps: Vec<(String, SyncSender<AudioChunk>)>,
    sync: Option<Arc<SyncLog>>,
    channels: Vec<ChannelRecorder>,
    started: bool,
}

impl Drop for AudioCorpusRecorder {
    fn drop(&mut self) {
        // Dropped without `stop` (an early return, a panic): end the threads.
        if self.started && self.sync.as_ref().is_some_and(|s| s.is_open()) {
            self.stop(Duration::from_secs(1));
        }
    }
}

impl AudioCorpusRecorder {
    pub fn new(
        out_dir: impl AsRef<Path>,
        clock: Arc<dyn Clock>,
        backend: Option<Arc<dyn AudioBackend>>,
        cfg: ChannelConfig,
    ) -> Self {
        Self {
            out_dir: out_dir.as_ref().to_path_buf(),
            clock,
            backend,
            mic_device: None,
            system_device: None,
            meeting_pid: None,
            on_status: None,
            cfg,
            taps: Vec::new(),
            sync: None,
            channels: Vec::new(),
            started: false,
        }
    }

    pub fn devices(mut self, mic: Option<&str>, system: Option<&str>) -> Self {
        self.mic_device = mic.filter(|d| !d.is_empty()).map(str::to_string);
        self.system_device = system.filter(|d| !d.is_empty()).map(str::to_string);
        self
    }

    /// The process that owns the caption window: the system channel listens to it.
    pub fn meeting_pid(mut self, pid: Option<u32>) -> Self {
        self.meeting_pid = pid;
        self
    }

    pub fn on_status(mut self, f: StatusFn) -> Self {
        self.on_status = Some(f);
        self
    }

    /// Follow the audio written on `ch` (e.g. for live ASR).
    pub fn tap(mut self, ch: &str, sender: SyncSender<AudioChunk>) -> Self {
        self.taps.push((ch.to_string(), sender));
        self
    }

    pub fn start(&mut self) -> Result<(), AudioError> {
        if self.started {
            return Ok(());
        }
        self.started = true;
        std::fs::create_dir_all(&self.out_dir)
            .map_err(|e| AudioError::Device(format!("{}: {e}", self.out_dir.display())))?;
        let sync = Arc::new(SyncLog::open(
            &self.out_dir.join("audio_sync.jsonl"),
            Arc::clone(&self.clock),
        ));
        self.sync = Some(Arc::clone(&sync));
        for ch in CHANNELS {
            let device = if ch == "mic" {
                self.mic_device.clone()
            } else {
                self.system_device.clone()
            };
            let silence = self
                .backend
                .as_ref()
                .and_then(|b| b.silence_alert(ch, self.system_device.as_deref()));
            let backend = self.backend.clone();
            let meeting_pid = self.meeting_pid;
            let opener: Opener = Box::new(move || {
                let Some(backend) = &backend else {
                    return Err(AudioError::Unavailable(
                        "no audio backend available".to_string(),
                    ));
                };
                if ch == "mic" {
                    backend.open_mic(device.as_deref())
                } else {
                    backend.open_system_for(device.as_deref(), meeting_pid)
                }
            });
            let tap = self
                .taps
                .iter()
                .find(|(name, _)| name == ch)
                .map(|(_, s)| s.clone());
            let recorder = ChannelRecorder::spawn(ChannelSpec {
                ch: ch.to_string(),
                opener,
                wav_path: self.out_dir.join(format!("{ch}.wav")),
                sync: Arc::clone(&sync),
                clock: Arc::clone(&self.clock),
                on_status: self.on_status.clone(),
                cfg: self.cfg.clone(),
                silence,
                tap,
            })
            .map_err(|e| AudioError::Device(format!("cannot start audio thread: {e}")))?;
            self.channels.push(recorder);
        }
        Ok(())
    }

    /// True while at least one channel is actively recording (drives the REC LED).
    pub fn recording(&self) -> bool {
        self.channels.iter().any(|c| c.info().status == "recording")
    }

    /// Current state of every channel, without stopping (live views).
    pub fn info(&self) -> Value {
        let mut info = serde_json::Map::new();
        for channel in &self.channels {
            info.insert(
                channel.channel().to_string(),
                serde_json::to_value(channel.info()).unwrap_or(Value::Null),
            );
        }
        for ch in CHANNELS {
            info.entry(ch.to_string())
                .or_insert_with(|| json!({"enabled": false, "status": "disabled"}));
        }
        Value::Object(info)
    }

    /// Stop every channel and return their info for the manifest. `timeout`
    /// bounds the whole call; a channel stuck in a read is reported as `stuck`.
    pub fn stop(&mut self, timeout: Duration) -> Value {
        for channel in &self.channels {
            channel.request_stop();
        }
        let deadline = Instant::now() + timeout;
        let mut info = serde_json::Map::new();
        for channel in &mut self.channels {
            let finished = channel.join_until(deadline);
            let mut entry = serde_json::to_value(channel.info()).unwrap_or(Value::Null);
            if !finished {
                // Abandoned: never report it as recording. Its own "stop"
                // event is lost; the sync log is closed below. A channel that
                // never opened was waiting to retry, not blocked in a read.
                if channel.info().status == "recording" {
                    channel.mark_stuck();
                    entry["status"] = json!("stuck");
                }
                entry["stop_timeout"] = json!(true);
            }
            info.insert(channel.channel().to_string(), entry);
        }
        for ch in CHANNELS {
            info.entry(ch.to_string())
                .or_insert_with(|| json!({"enabled": false, "status": "disabled"}));
        }
        if let Some(sync) = &self.sync {
            sync.close();
        }
        Value::Object(info)
    }
}
