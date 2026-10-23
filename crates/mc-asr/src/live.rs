//! Recognition while the meeting runs: a draft, not the record.
//!
//! The audio recorder offers every block it writes ([`AudioChunk`]); here each
//! channel is cut into utterances with the VAD and handed to a worker that runs
//! the engine. Everything is bounded: when the engine cannot keep up, new
//! utterances are dropped and counted rather than letting memory or delay grow.
//! The real transcript comes from `mc transcribe` once the meeting is over, so
//! losing a live phrase costs nothing permanent.

use std::collections::HashMap;
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{
    Receiver, RecvTimeoutError, SyncSender, TrySendError, channel, sync_channel,
};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mc_audio::AudioChunk;

use crate::ASR_RATE;
use crate::codec::AsrSegment;
use crate::run::segment_from_raw;
use crate::segmenter::{Segmenter, SegmenterConfig};
use crate::transcriber::{AsrError, TranscribeCtx, Transcriber};
use crate::vad::EnergyVad;

/// Builds the engine for a channel, on that channel's worker thread.
pub type EngineFactory =
    Arc<dyn Fn(&str) -> Result<Box<dyn Transcriber + Send>, AsrError> + Send + Sync>;

#[derive(Debug, Clone)]
pub struct LiveConfig {
    pub segmenter: SegmenterConfig,
    /// Utterances waiting for the engine, per channel; more are dropped.
    pub queue: usize,
    /// Audio blocks waiting for the VAD, per channel.
    pub audio_queue: usize,
    pub language: Option<String>,
    pub prompt: Option<String>,
    /// A hole in the audio longer than this ends the utterance in progress.
    pub gap_reset_ms: u64,
    /// Append each phrase here as it is recognised (crash-safe draft).
    pub jsonl: Option<PathBuf>,
}

impl Default for LiveConfig {
    fn default() -> Self {
        Self {
            segmenter: SegmenterConfig::default(),
            queue: 4,
            audio_queue: 256,
            language: None,
            prompt: None,
            gap_reset_ms: 5_000,
            jsonl: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LiveAsrEvent {
    Segment(AsrSegment),
    /// An utterance was dropped because the engine was behind.
    Dropped {
        ch: String,
        ms: f64,
    },
    Error {
        ch: String,
        message: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveSummary {
    pub segments: u64,
    pub dropped_utterances: u64,
    pub dropped_ms: f64,
    /// A worker was still inside the engine at the deadline and was left behind.
    pub stuck: bool,
}

fn count_drop(
    counters: &Counters,
    events: &std::sync::mpsc::Sender<LiveAsrEvent>,
    ch: &str,
    ms: f64,
) {
    counters.dropped.fetch_add(1, Ordering::Relaxed);
    counters.dropped_ms.fetch_add(ms as u64, Ordering::Relaxed);
    let _ = events.send(LiveAsrEvent::Dropped {
        ch: ch.to_string(),
        ms,
    });
}

struct Work {
    start_sample: u64,
    audio: Vec<f32>,
}

#[derive(Default)]
struct Counters {
    segments: AtomicU64,
    dropped: AtomicU64,
    dropped_ms: AtomicU64,
}

pub struct LiveAsr {
    /// When the workers must give up on queued phrases (set by `stop`).
    deadline: Arc<Mutex<Option<Instant>>>,
    senders: HashMap<String, SyncSender<AudioChunk>>,
    stop: Arc<AtomicBool>,
    events: Receiver<LiveAsrEvent>,
    threads: Vec<JoinHandle<()>>,
    counters: Arc<Counters>,
}

fn millis(samples: u64) -> f64 {
    samples as f64 * 1000.0 / f64::from(ASR_RATE)
}

struct Ingest {
    ch: String,
    cfg: LiveConfig,
    seg: Segmenter<EnergyVad>,
    /// Audio held for the utterances the segmenter may still cut.
    buf: Vec<f32>,
    buf_start: u64,
    /// Absolute sample where the current segmenter started counting.
    origin: u64,
    next: u64,
    work: SyncSender<Work>,
    events: std::sync::mpsc::Sender<LiveAsrEvent>,
    counters: Arc<Counters>,
}

impl Ingest {
    fn new(
        ch: &str,
        cfg: LiveConfig,
        work: SyncSender<Work>,
        events: std::sync::mpsc::Sender<LiveAsrEvent>,
        counters: Arc<Counters>,
    ) -> Self {
        Self {
            ch: ch.to_string(),
            seg: Segmenter::new(cfg.segmenter.clone(), EnergyVad::default()),
            cfg,
            buf: Vec::new(),
            buf_start: 0,
            origin: 0,
            next: 0,
            work,
            events,
            counters,
        }
    }

    fn submit(&mut self, found: Vec<crate::segmenter::Utterance>) {
        for u in found {
            let (start, end) = (self.origin + u.start_sample, self.origin + u.end_sample);
            let from = start.saturating_sub(self.buf_start) as usize;
            let to = (end.saturating_sub(self.buf_start) as usize).min(self.buf.len());
            if from >= to {
                continue;
            }
            let work = Work {
                start_sample: start,
                audio: self.buf[from..to].to_vec(),
            };
            match self.work.try_send(work) {
                Ok(()) => {}
                Err(TrySendError::Full(w) | TrySendError::Disconnected(w)) => {
                    let ms = millis(w.audio.len() as u64);
                    self.counters.dropped.fetch_add(1, Ordering::Relaxed);
                    self.counters
                        .dropped_ms
                        .fetch_add(ms as u64, Ordering::Relaxed);
                    let _ = self.events.send(LiveAsrEvent::Dropped {
                        ch: self.ch.clone(),
                        ms,
                    });
                }
            }
        }
    }

    fn trim(&mut self) {
        let keep_from = self.origin + self.seg.earliest_needed_sample();
        if keep_from > self.buf_start {
            let drop = ((keep_from - self.buf_start) as usize).min(self.buf.len());
            self.buf.drain(..drop);
            self.buf_start += drop as u64;
        }
    }

    fn restart_at(&mut self, sample: u64) {
        self.seg = Segmenter::new(self.cfg.segmenter.clone(), EnergyVad::default());
        self.buf.clear();
        self.buf_start = sample;
        self.origin = sample;
        self.next = sample;
    }

    fn feed(&mut self, chunk: &AudioChunk) {
        if chunk.start_frame > self.next {
            let gap = chunk.start_frame - self.next;
            if millis(gap) > self.cfg.gap_reset_ms as f64 {
                let found = self.seg.finish();
                self.submit(found);
                self.restart_at(chunk.start_frame);
            } else {
                // Short holes are silence the recorder padded; keep time honest.
                let zeros = vec![0.0f32; gap as usize];
                let found = self.seg.push(&zeros);
                self.buf.extend_from_slice(&zeros);
                self.submit(found);
            }
        } else if chunk.start_frame < self.next {
            // Out of order (should not happen): start over from here.
            let found = self.seg.finish();
            self.submit(found);
            self.restart_at(chunk.start_frame);
        }
        let samples: Vec<f32> = chunk
            .samples
            .iter()
            .map(|s| f32::from(*s) / 32768.0)
            .collect();
        let found = self.seg.push(&samples);
        self.buf.extend_from_slice(&samples);
        self.next = chunk.start_frame + chunk.samples.len() as u64;
        self.submit(found);
        self.trim();
    }

    fn finish(&mut self) {
        let found = self.seg.finish();
        self.submit(found);
    }
}

fn append_line(path: &PathBuf, segment: &AsrSegment) {
    let Ok(mut line) = serde_json::to_string(segment) else {
        return;
    };
    line.push('\n');
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

impl LiveAsr {
    /// One ingest thread and one worker thread per channel.
    pub fn start(channels: &[&str], factory: EngineFactory, cfg: LiveConfig) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let deadline: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
        let counters = Arc::new(Counters::default());
        let (event_tx, events) = channel();
        let file_lock = Arc::new(Mutex::new(()));
        let mut senders = HashMap::new();
        let mut threads = Vec::new();
        for ch in channels {
            let (audio_tx, audio_rx) = sync_channel::<AudioChunk>(cfg.audio_queue.max(1));
            let (work_tx, work_rx) = sync_channel::<Work>(cfg.queue.max(1));
            senders.insert((*ch).to_string(), audio_tx);

            let mut ingest = Ingest::new(
                ch,
                cfg.clone(),
                work_tx,
                event_tx.clone(),
                Arc::clone(&counters),
            );
            let ingest_stop = Arc::clone(&stop);
            threads.push(std::thread::spawn(move || {
                loop {
                    match audio_rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(chunk) => ingest.feed(&chunk),
                        Err(RecvTimeoutError::Timeout) => {
                            if ingest_stop.load(Ordering::Relaxed) {
                                break;
                            }
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                // What the recorder wrote before stopping still counts.
                for chunk in audio_rx.try_iter() {
                    ingest.feed(&chunk);
                }
                ingest.finish();
            }));

            let (worker_stop, worker_counters) = (Arc::clone(&stop), Arc::clone(&counters));
            let (worker_events, worker_factory) = (event_tx.clone(), Arc::clone(&factory));
            let (channel_name, worker_cfg) = ((*ch).to_string(), cfg.clone());
            let (lock, worker_deadline) = (Arc::clone(&file_lock), Arc::clone(&deadline));
            threads.push(std::thread::spawn(move || {
                let mut engine = match worker_factory(&channel_name) {
                    Ok(engine) => Some(engine),
                    Err(error) => {
                        let _ = worker_events.send(LiveAsrEvent::Error {
                            ch: channel_name.clone(),
                            message: error.to_string(),
                        });
                        None
                    }
                };
                let out_of_time = || {
                    worker_stop.load(Ordering::Relaxed)
                        && worker_deadline
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .is_some_and(|d| Instant::now() >= d)
                };
                loop {
                    if out_of_time() {
                        break;
                    }
                    let work = match work_rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(work) => work,
                        Err(RecvTimeoutError::Timeout) => continue,
                        // The ingest side is done and everything it queued was handled.
                        Err(RecvTimeoutError::Disconnected) => break,
                    };
                    let length_ms = millis(work.audio.len() as u64);
                    let Some(running) = engine.as_mut() else {
                        count_drop(&worker_counters, &worker_events, &channel_name, length_ms);
                        continue;
                    };
                    let ctx = TranscribeCtx {
                        channel: &channel_name,
                        prompt: worker_cfg.prompt.as_deref(),
                        language: worker_cfg.language.as_deref(),
                    };
                    let outcome = catch_unwind(AssertUnwindSafe(|| running.transcribe(&work.audio, &ctx)));
                    match outcome {
                        Ok(Ok(found)) => {
                            // Phrases the filters dropped are not captions: nothing to report live.
                            for raw in found.segments {
                                if let Some(segment) = segment_from_raw(
                                    &channel_name,
                                    millis(work.start_sample),
                                    length_ms,
                                    raw,
                                ) {
                                    if let Some(path) = &worker_cfg.jsonl {
                                        let _guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
                                        append_line(path, &segment);
                                    }
                                    worker_counters.segments.fetch_add(1, Ordering::Relaxed);
                                    let _ = worker_events.send(LiveAsrEvent::Segment(segment));
                                }
                            }
                        }
                        Ok(Err(error)) => {
                            let _ = worker_events.send(LiveAsrEvent::Error {
                                ch: channel_name.clone(),
                                message: error.to_string(),
                            });
                        }
                        Err(_) => {
                            // An engine that panicked is not trusted again.
                            engine = None;
                            let _ = worker_events.send(LiveAsrEvent::Error {
                                ch: channel_name.clone(),
                                message: "the recognition engine crashed; live recognition stopped for this channel".to_string(),
                            });
                            count_drop(&worker_counters, &worker_events, &channel_name, length_ms);
                        }
                    }
                }
                // Whatever is still queued goes to the final pass.
                for work in work_rx.try_iter() {
                    count_drop(
                        &worker_counters,
                        &worker_events,
                        &channel_name,
                        millis(work.audio.len() as u64),
                    );
                }
            }));
        }
        Self {
            deadline,
            senders,
            stop,
            events,
            threads,
            counters,
        }
    }

    /// Where the recorder offers a channel's blocks (its `tap`).
    pub fn sender(&self, ch: &str) -> Option<SyncSender<AudioChunk>> {
        self.senders.get(ch).cloned()
    }

    /// What has been recognised (or gone wrong) since the last call.
    pub fn poll(&self) -> Vec<LiveAsrEvent> {
        self.events.try_iter().collect()
    }

    /// Stops listening; waits up to `timeout` for the workers. Whatever is still
    /// queued is left for the final pass.
    pub fn stop(mut self, timeout: Duration) -> (LiveSummary, Vec<LiveAsrEvent>) {
        let deadline = Instant::now() + timeout;
        *self.deadline.lock().unwrap_or_else(PoisonError::into_inner) = Some(deadline);
        self.stop.store(true, Ordering::Relaxed);
        self.senders.clear();
        let mut stuck = false;
        for handle in self.threads.drain(..) {
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if handle.is_finished() {
                let _ = handle.join();
            } else {
                stuck = true;
            }
        }
        let rest = self.events.try_iter().collect();
        (
            LiveSummary {
                segments: self.counters.segments.load(Ordering::Relaxed),
                dropped_utterances: self.counters.dropped.load(Ordering::Relaxed),
                dropped_ms: self.counters.dropped_ms.load(Ordering::Relaxed) as f64,
                stuck,
            },
            rest,
        )
    }
}

impl Drop for LiveAsr {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
