//! The live capture driver: polls a source on its own thread, reconciles the
//! frames and records them, reporting progress over a channel.
//!
//! The driver is the only authority on stamps. It reads the clock once per
//! frame and feeds the same stamp to the reconciler and to the debounce
//! (`step_with`), which is the code path replay uses: a recorded live session
//! therefore replays with `--strict` and no divergences.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mc_asr::{AsrSegment, EngineFactory, LiveAsr, LiveAsrEvent};
use mc_audio::{AudioBackend, AudioCorpusRecorder, ChannelConfig};
use mc_captions::{CaptureSource, Discovery, FakeSource, FakeStep, SourceError};
use mc_core::{Clock, RawObservation, Segment, Stamp};
use mc_reconcile::{Reconciler, ReconcilerConfig, StepCtx};
use mc_store::StoreError;
use mc_store::jsonl::{CaptionLine, read_file};
use mc_store::manifest::Manifest;

use crate::corpus::CorpusSession;
use crate::live_analyst::LedgerUpdate;
use crate::replay::{ReplayError, ReplayOp, reconstruct};
use crate::sidecar::Note;
use crate::source_worker::{Gone, Polled, Reply, SourceWorker};

/// Consecutive failed writes after which the capture gives up.
const MAX_WRITE_FAILURES: u32 = 50;
const SLICE: Duration = Duration::from_millis(50);
/// How long stopping waits for the source to close before leaving it behind.
const SOURCE_CLOSE_WAIT: Duration = Duration::from_millis(300);
/// All of `finish` (source, audio, live ASR) stays under the controller's 3 s drop wait.
const FINISH_BUDGET: Duration = Duration::from_millis(2300);
const MIN_STEP_WAIT: Duration = Duration::from_millis(100);
/// Rebuilds of a hung source allowed per minute before giving up.
const MAX_RESTARTS_PER_MINUTE: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebounceClock {
    Mono,
    Wall,
}

impl DebounceClock {
    pub fn manifest_name(self) -> &'static str {
        match self {
            Self::Mono => "mono",
            Self::Wall => "wall",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LiveConfig {
    pub poll: Duration,
    pub reconciler: ReconcilerConfig,
    pub debounce: DebounceClock,
    /// Warn (once) when no caption text arrived for this long.
    pub no_caption_warn: Duration,
    /// How often discovery is retried while the window is not found.
    pub rediscover: Duration,
    /// How often to look at the source's meeting name.
    pub meeting_name_every: u64,
    /// A source call (discover or poll) slower than this is abandoned and the source rebuilt.
    pub poll_timeout: Duration,
    /// Pause before the first rebuild of a hung source (grows with each one).
    pub restart_backoff: Duration,
}

impl Default for LiveConfig {
    fn default() -> Self {
        Self {
            poll: Duration::from_millis(100),
            reconciler: ReconcilerConfig::default(),
            debounce: DebounceClock::Mono,
            no_caption_warn: Duration::from_secs(30),
            rediscover: Duration::from_secs(1),
            meeting_name_every: 20,
            poll_timeout: Duration::from_secs(5),
            restart_backoff: Duration::from_secs(1),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    Stop,
    SourceEnded,
    SourceFatal,
    Panic,
    WriteFailed,
}

impl EndReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::SourceEnded => "source_ended",
            Self::SourceFatal => "source_fatal",
            Self::Panic => "panic",
            Self::WriteFailed => "write_failed",
        }
    }

    /// Whether the capture ended the way the user expected.
    pub fn is_clean(self) -> bool {
        matches!(self, Self::Stop | Self::SourceEnded)
    }
}

/// Audio to record next to the captions.
#[derive(Clone)]
pub struct AudioSpec {
    /// `None`: no backend on this machine (both channels report unavailable).
    pub backend: Option<Arc<dyn AudioBackend>>,
    pub mic_device: Option<String>,
    pub system_device: Option<String>,
    pub channels: ChannelConfig,
    /// How long stopping may wait for the audio threads.
    pub stop_timeout: Duration,
    /// Draft recognition while the meeting runs (needs an audio backend).
    pub live_asr: Option<LiveAsrSpec>,
}

/// Live recognition: the engine to build (once per channel) and its settings.
#[derive(Clone)]
pub struct LiveAsrSpec {
    pub factory: EngineFactory,
    pub cfg: mc_asr::LiveConfig,
}

impl AudioSpec {
    pub fn new(backend: Option<Arc<dyn AudioBackend>>) -> Self {
        Self {
            backend,
            mic_device: None,
            system_device: None,
            channels: ChannelConfig::default(),
            stop_timeout: Duration::from_millis(1500),
            live_asr: None,
        }
    }
}

impl std::fmt::Debug for AudioSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioSpec")
            .field("backend", &self.backend.is_some())
            .field("mic_device", &self.mic_device)
            .field("system_device", &self.system_device)
            .field("live_asr", &self.live_asr.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub enum LiveEvent {
    Started {
        session_dir: Option<PathBuf>,
        session_id: Option<String>,
        source: String,
    },
    /// The session folder now exists (the first time the meeting window was found).
    Session {
        dir: PathBuf,
        id: String,
    },
    Source(Discovery),
    /// A committed segment; `idx` is its place in the session's commit order, the `idx` a
    /// replay of the captions gives it, and the id the analyst cites.
    Segment {
        idx: u64,
        segment: Segment,
    },
    Open(Vec<String>),
    NoCaptions {
        secs: u64,
    },
    WriteError(String),
    SourceError(String),
    /// A line from the audio recorder (a channel failed or was lost).
    Audio(String),
    /// Whether any audio channel is recording (the REC indicator).
    Rec(bool),
    /// A phrase the live recognition just produced (a draft).
    Asr(AsrSegment),
    /// The live analyst finished a window (derived data, apart from the captions).
    Ledger(Box<LedgerUpdate>),
    /// A window the analyst could not do, or a problem saving it; the capture goes on.
    AnalystError(String),
    Note(Note),
    Mark {
        mono_ms: f64,
        wall_ms: i64,
    },
    /// The user corrected a term while the meeting runs: `heard` is `correct` from now on.
    Fixed {
        heard: String,
        correct: String,
    },
    /// The user named a speaker: the captions' `from` is `to`.
    Named {
        from: String,
        to: String,
    },
    Ended {
        reason: EndReason,
        frames: u64,
        records: u64,
    },
}

#[derive(Debug, Clone)]
pub struct LiveSummary {
    pub reason: EndReason,
    pub frames: u64,
    pub records: u64,
    pub session_dir: Option<PathBuf>,
    pub session_id: Option<String>,
    /// Final state of the audio channels, when audio was recorded.
    pub audio: Option<serde_json::Value>,
}

/// Builds the source on its own thread (native handles are not `Send`); called again to rebuild a hung one.
pub type SourceFactory = Box<dyn FnMut() -> Result<Box<dyn CaptureSource>, SourceError> + Send>;

pub(crate) type SharedFactory = Arc<Mutex<SourceFactory>>;

/// A factory for a source that can be built only once (a rebuild is fatal).
pub fn once(source: impl CaptureSource + Send + 'static) -> SourceFactory {
    let mut slot = Some(source);
    Box::new(move || {
        slot.take()
            .map(|source| Box::new(source) as Box<dyn CaptureSource>)
            .ok_or_else(|| SourceError::Fatal("source cannot be rebuilt".to_string()))
    })
}

/// Creates the session (given the stamp of the moment) when the meeting is first found.
pub type SessionOpener = Box<dyn FnOnce(Stamp) -> Result<CorpusSession, StoreError> + Send>;

/// Why waiting for the source ended without a reply.
enum Halt {
    End(EndReason),
    /// The source did not answer in time.
    Stalled,
}

impl From<EndReason> for Halt {
    fn from(reason: EndReason) -> Self {
        Self::End(reason)
    }
}

/// Where the captions go.
pub enum SessionSlot {
    /// A live view: nothing is written.
    Off,
    Ready(Box<CorpusSession>),
    Lazy(SessionOpener),
}

impl From<Option<CorpusSession>> for SessionSlot {
    fn from(session: Option<CorpusSession>) -> Self {
        session.map_or(Self::Off, |s| Self::Ready(Box::new(s)))
    }
}

struct Driver {
    cfg: LiveConfig,
    clock: Arc<dyn Clock>,
    session: Option<CorpusSession>,
    opener: Option<SessionOpener>,
    t0_wall_ms: i64,
    born: Instant,
    factory: SharedFactory,
    restarts: VecDeque<Instant>,
    reconciler: Reconciler,
    tx: Sender<LiveEvent>,
    stop: Arc<AtomicBool>,
    frames: u64,
    /// Segments committed so far: the next one's `idx`.
    committed: u64,
    ticks: u64,
    records: u64,
    write_failures: u32,
    write_errors: u64,
    write_gave_up: bool,
    source: Option<SourceWorker>,
    /// The process that owns the caption window, once attached.
    caption_pid: Option<u32>,
    audio_spec: Option<AudioSpec>,
    audio_stop_timeout: Duration,
    audio_versions: serde_json::Value,
    audio: Option<AudioCorpusRecorder>,
    /// Manifest entry when the recorder could not even start.
    audio_failure: Option<serde_json::Value>,
    live_asr: Option<LiveAsr>,
    live_dropped_reported: bool,
    drift_reported: bool,
    /// The last `Open` sent: a poll that re-reads the same lines sends nothing.
    last_open: Vec<String>,
    rec: bool,
    last_audio_poll: Instant,
}

/// A segment's `idx` counts commits, but a replay of `captions.jsonl` only sees the lines that
/// were written: after a failed write the two can differ. Said once, at the first commit after it.
fn idx_drift(write_errors: u64, committing: bool, reported: &mut bool) -> Option<String> {
    if write_errors == 0 || !committing || *reported {
        return None;
    }
    *reported = true;
    Some(
        "caption lines were lost: segment ids from here on may differ from a replay of captions.jsonl"
            .to_string(),
    )
}

fn init_time(cfg: &LiveConfig, t0_wall_ms: i64) -> f64 {
    match cfg.debounce {
        DebounceClock::Mono => 0.0,
        DebounceClock::Wall => t0_wall_ms as f64 / 1000.0,
    }
}

/// Starts the capture thread. `t0_wall_ms` is when the session clock started.
pub fn spawn(
    factory: SourceFactory,
    cfg: LiveConfig,
    clock: Arc<dyn Clock>,
    t0_wall_ms: i64,
    session: impl Into<SessionSlot>,
    stop: Arc<AtomicBool>,
) -> (JoinHandle<LiveSummary>, Receiver<LiveEvent>) {
    spawn_with_audio(factory, cfg, clock, t0_wall_ms, session, stop, None)
}

/// [`spawn`] plus audio recording into the session folder (ignored without a session).
pub fn spawn_with_audio(
    factory: SourceFactory,
    cfg: LiveConfig,
    clock: Arc<dyn Clock>,
    t0_wall_ms: i64,
    session: impl Into<SessionSlot>,
    stop: Arc<AtomicBool>,
    audio: Option<AudioSpec>,
) -> (JoinHandle<LiveSummary>, Receiver<LiveEvent>) {
    let (session, opener) = match session.into() {
        SessionSlot::Off => (None, None),
        SessionSlot::Ready(session) => (Some(*session), None),
        SessionSlot::Lazy(opener) => (None, Some(opener)),
    };
    let (tx, rx) = channel();
    let handle = std::thread::spawn(move || {
        let reconciler =
            Reconciler::for_replay(cfg.reconciler.clone(), init_time(&cfg, t0_wall_ms));
        let mut driver = Driver {
            cfg,
            clock,
            session,
            opener,
            t0_wall_ms,
            born: Instant::now(),
            factory: Arc::new(Mutex::new(factory)),
            restarts: VecDeque::new(),
            reconciler,
            tx,
            stop,
            frames: 0,
            committed: 0,
            ticks: 0,
            records: 0,
            write_failures: 0,
            write_errors: 0,
            write_gave_up: false,
            source: None,
            audio_spec: audio,
            caption_pid: None,
            audio_stop_timeout: Duration::from_millis(1500),
            audio_versions: serde_json::Value::Null,
            audio: None,
            audio_failure: None,
            live_asr: None,
            live_dropped_reported: false,
            drift_reported: false,
            last_open: Vec::new(),
            rec: false,
            last_audio_poll: Instant::now(),
        };
        let outcome = catch_unwind(AssertUnwindSafe(|| driver.run()));
        let reason = outcome.unwrap_or(EndReason::Panic);
        driver.finish(reason)
    });
    (handle, rx)
}

impl Driver {
    /// The stamp as it will read back from `captions.jsonl` (`mono_ms` rounded
    /// to 3 decimals), so the debounce sees exactly what a replay will see.
    fn stamp_now(&self) -> Stamp {
        let mut stamp = self.clock.now();
        stamp.mono_ms = mc_core::pytext::py_round3(stamp.mono_ms);
        stamp
    }

    fn send(&self, event: LiveEvent) {
        // A receiver that went away must not stop the recording.
        let _ = self.tx.send(event);
    }

    /// A stamp from `Instant` alone: waiting must not consume reads of the injected clock.
    fn elapsed_stamp(&self) -> Stamp {
        let ms = mc_core::pytext::py_round3(self.born.elapsed().as_secs_f64() * 1000.0);
        Stamp {
            wall_ms: self.t0_wall_ms + ms as i64,
            mono_ms: ms,
        }
    }

    /// Keeps the manifest's "last seen" mark fresh while no frames arrive.
    fn keep_alive(&mut self) {
        let stamp = self.elapsed_stamp();
        if let Some(session) = self.session.as_mut() {
            let _ = session.heartbeat(stamp);
        }
    }

    /// Sleeps until `deadline` in short slices; returns `true` if stop was set.
    fn sleep_until(&self, deadline: Instant) -> bool {
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            std::thread::sleep((deadline - now).min(SLICE));
        }
    }

    /// Starts the audio threads next to the captions. Audio is best effort: a
    /// failure here is reported and recorded, never fatal.
    fn start_audio(&mut self) {
        let (Some(spec), Some(session)) = (self.audio_spec.take(), self.session.as_ref()) else {
            return;
        };
        let tx = Mutex::new(self.tx.clone());
        let mut recorder = AudioCorpusRecorder::new(
            session.dir(),
            Arc::clone(&self.clock),
            spec.backend.clone(),
            spec.channels.clone(),
        )
        .devices(spec.mic_device.as_deref(), spec.system_device.as_deref())
        .meeting_pid(self.caption_pid)
        .on_status(Arc::new(move |message: &str| {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send(LiveEvent::Audio(message.to_string()));
            }
        }));
        if let Some(live) = &spec.live_asr {
            let mut cfg = live.cfg.clone();
            cfg.jsonl = Some(session.dir().join("asr_live.jsonl"));
            let asr = LiveAsr::start(&["mic", "system"], Arc::clone(&live.factory), cfg);
            for ch in ["mic", "system"] {
                if let Some(sender) = asr.sender(ch) {
                    recorder = recorder.tap(ch, sender);
                }
            }
            self.live_asr = Some(asr);
        }
        self.audio_stop_timeout = spec.stop_timeout;
        self.audio_versions = spec
            .backend
            .as_ref()
            .map_or(serde_json::Value::Null, |b| b.versions());
        match recorder.start() {
            Ok(()) => self.audio = Some(recorder),
            Err(error) => {
                // No audio, so nothing for the recognition to listen to.
                if let Some(asr) = self.live_asr.take() {
                    let _ = asr.stop(Duration::from_millis(200));
                }
                let message = format!("audio unavailable: {error}");
                self.send(LiveEvent::Audio(message.clone()));
                self.audio_failure = Some(serde_json::json!({
                    "mic": {"enabled": true, "status": "failed", "error": message},
                    "system": {"enabled": true, "status": "failed", "error": message},
                }));
            }
        }
    }

    /// Tells the front end when the REC indicator changes.
    fn poll_audio(&mut self) {
        const EVERY: Duration = Duration::from_millis(250);
        if self.last_audio_poll.elapsed() < EVERY {
            return;
        }
        self.last_audio_poll = Instant::now();
        self.forward_live_asr();
        let Some(audio) = &self.audio else { return };
        let recording = audio.recording();
        if recording != self.rec {
            self.rec = recording;
            self.send(LiveEvent::Rec(recording));
        }
    }

    /// Recognised phrases (and trouble) from the live recognition, to the front end.
    fn forward_live_asr(&mut self) {
        let Some(asr) = &self.live_asr else { return };
        let events = asr.poll();
        self.forward_live_events(events);
    }

    fn forward_live_events(&mut self, events: Vec<LiveAsrEvent>) {
        for event in events {
            match event {
                LiveAsrEvent::Segment(segment) => self.send(LiveEvent::Asr(segment)),
                LiveAsrEvent::Dropped { .. } => {
                    // Said once: the final pass will cover what live missed.
                    if !self.live_dropped_reported {
                        self.live_dropped_reported = true;
                        self.send(LiveEvent::Audio(
                            "live recognition is behind and skipping phrases; the final pass will have them"
                                .to_string(),
                        ));
                    }
                }
                LiveAsrEvent::Error { ch, message } => {
                    self.send(LiveEvent::Audio(format!(
                        "live recognition ({ch}): {message}"
                    )));
                }
            }
        }
    }

    /// Waits for the worker's reply; a call slower than `poll_timeout` is `Stalled`.
    fn wait_reply(&mut self) -> Result<Reply, Halt> {
        let started = Instant::now();
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return Err(EndReason::Stop.into());
            }
            let worker = self.source.as_ref().ok_or(EndReason::SourceFatal)?;
            match worker.recv(SLICE) {
                Ok(Reply::Closed) | Err(Gone::Dead) => return Err(EndReason::Panic.into()),
                Ok(reply) => return Ok(reply),
                Err(Gone::Timeout) => {}
            }
            self.poll_audio();
            self.keep_alive();
            if started.elapsed() >= self.cfg.poll_timeout {
                return Err(Halt::Stalled);
            }
        }
    }

    fn request(
        &mut self,
        ask: impl FnOnce(&SourceWorker) -> Result<(), Gone>,
    ) -> Result<Reply, Halt> {
        let worker = self.source.as_ref().ok_or(EndReason::SourceFatal)?;
        ask(worker).map_err(|_| EndReason::Panic)?;
        self.wait_reply()
    }

    fn discover(&mut self) -> Result<Result<Discovery, SourceError>, Halt> {
        match self.request(SourceWorker::discover)? {
            Reply::Discovered(result, pid) => {
                self.caption_pid = pid;
                Ok(result)
            }
            _ => Err(EndReason::SourceFatal.into()),
        }
    }

    fn poll_source(&mut self, want_meeting: bool) -> Result<Polled, Halt> {
        match self.request(|worker| worker.poll(want_meeting))? {
            Reply::Polled(polled) => Ok(polled),
            _ => Err(EndReason::SourceFatal.into()),
        }
    }

    /// Builds the source on a fresh worker; returns its name.
    fn start_source(&mut self) -> Result<String, EndReason> {
        self.source = Some(SourceWorker::spawn(Arc::clone(&self.factory)));
        match self.wait_reply() {
            Ok(Reply::Built(Ok(name))) => Ok(name),
            Ok(Reply::Built(Err(error))) => {
                self.send(LiveEvent::SourceError(error.to_string()));
                Err(EndReason::SourceFatal)
            }
            Ok(_) => Err(EndReason::SourceFatal),
            Err(halt) => self.recover(halt),
        }
    }

    /// A stalled source is abandoned (it closes itself when its call returns) and rebuilt, a few times a minute at most.
    fn recover(&mut self, halt: Halt) -> Result<String, EndReason> {
        let Halt::Stalled = halt else {
            return Err(match halt {
                Halt::End(reason) => reason,
                Halt::Stalled => EndReason::SourceFatal,
            });
        };
        self.source = None;
        let now = Instant::now();
        self.restarts
            .retain(|at| now.duration_since(*at) < Duration::from_secs(60));
        if self.restarts.len() >= MAX_RESTARTS_PER_MINUTE {
            self.send(LiveEvent::SourceError(
                "source keeps hanging; giving up".to_string(),
            ));
            return Err(EndReason::SourceFatal);
        }
        self.restarts.push_back(now);
        self.send(LiveEvent::SourceError(format!(
            "source not responding for {} ms; restarting it",
            self.cfg.poll_timeout.as_millis()
        )));
        let pause = self.cfg.restart_backoff * u32::try_from(self.restarts.len()).unwrap_or(1);
        if self.sleep_until(now + pause) {
            return Err(EndReason::Stop);
        }
        self.start_source()
    }

    /// Creates the session on the first call (the meeting was found), then starts the audio.
    fn begin_recording(&mut self) -> Result<(), EndReason> {
        if let Some(open) = self.opener.take() {
            match open(self.elapsed_stamp()) {
                Ok(session) => {
                    self.send(LiveEvent::Session {
                        dir: session.dir().to_path_buf(),
                        id: session.session_id().to_string(),
                    });
                    self.session = Some(session);
                }
                Err(error) => {
                    self.send(LiveEvent::WriteError(error.to_string()));
                    return Err(EndReason::WriteFailed);
                }
            }
        }
        // Only once there is something to capture: a failed source must not leave a mic recording.
        self.start_audio();
        Ok(())
    }

    fn run(&mut self) -> EndReason {
        let name = match self.start_source() {
            Ok(name) => name,
            Err(reason) => return reason,
        };
        self.send(LiveEvent::Started {
            session_dir: self.session.as_ref().map(|s| s.dir().to_path_buf()),
            session_id: self.session.as_ref().map(|s| s.session_id().to_string()),
            source: name,
        });

        let mut attached = false;
        let mut announced_searching = false;
        let mut next = Instant::now();
        let mut last_text = Instant::now();
        let mut warned = false;
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return EndReason::Stop;
            }
            self.poll_audio();
            if !attached {
                let found = match self.discover() {
                    Ok(found) => found,
                    Err(halt) => {
                        if let Err(reason) = self.recover(halt) {
                            return reason;
                        }
                        continue;
                    }
                };
                match found {
                    Ok(Discovery::Attached { window }) => {
                        // Stop may have arrived while the answer was in flight.
                        if self.stop.load(Ordering::Relaxed) {
                            return EndReason::Stop;
                        }
                        if let Err(reason) = self.begin_recording() {
                            return reason;
                        }
                        attached = true;
                        announced_searching = false;
                        // The "no captions" clock starts when the window is found.
                        last_text = Instant::now();
                        warned = false;
                        self.send(LiveEvent::Source(Discovery::Attached { window }));
                        next = Instant::now();
                    }
                    Ok(Discovery::Ended) => return EndReason::SourceEnded,
                    Ok(Discovery::Searching) => {
                        if !announced_searching {
                            announced_searching = true;
                            self.send(LiveEvent::Source(Discovery::Searching));
                        }
                        if self.sleep_until(Instant::now() + self.cfg.rediscover) {
                            return EndReason::Stop;
                        }
                        self.keep_alive();
                        continue;
                    }
                    Err(SourceError::Fatal(m)) => {
                        self.send(LiveEvent::SourceError(m));
                        return EndReason::SourceFatal;
                    }
                    Err(error) => {
                        self.send(LiveEvent::SourceError(error.to_string()));
                        if self.sleep_until(Instant::now() + self.cfg.rediscover) {
                            return EndReason::Stop;
                        }
                        continue;
                    }
                }
            }
            if self.sleep_until(next) {
                return EndReason::Stop;
            }
            // Looked at on the first poll, then every N polls.
            let want_meeting =
                self.ticks == 0 || (self.ticks + 1).is_multiple_of(self.cfg.meeting_name_every);
            let polled = match self.poll_source(want_meeting) {
                Ok(polled) => polled,
                Err(halt) => {
                    if let Err(reason) = self.recover(halt) {
                        return reason;
                    }
                    attached = false;
                    announced_searching = false;
                    continue;
                }
            };
            self.ticks += 1;
            let (read_ms, meeting) = (polled.read_ms, polled.meeting);
            match polled.result {
                Ok(observations) => {
                    if observations
                        .iter()
                        .any(|o| o.text.as_deref().is_some_and(|t| !t.trim().is_empty()))
                    {
                        last_text = Instant::now();
                        warned = false;
                    } else if !warned && last_text.elapsed() >= self.cfg.no_caption_warn {
                        warned = true;
                        self.send(LiveEvent::NoCaptions {
                            secs: self.cfg.no_caption_warn.as_secs(),
                        });
                    }
                    if self.step(&observations, read_ms) {
                        return EndReason::WriteFailed;
                    }
                }
                Err(SourceError::Transient(message)) => {
                    self.send(LiveEvent::SourceError(message));
                }
                Err(SourceError::Lost(_)) => {
                    // The window went away: look for it again (and a source
                    // with nothing left, like a fake, ends at the next discover).
                    attached = false;
                    announced_searching = false;
                }
                Err(SourceError::Fatal(message)) => {
                    self.send(LiveEvent::SourceError(message));
                    return EndReason::SourceFatal;
                }
            }
            if let (Some(name), Some(session)) = (meeting, self.session.as_mut())
                && let Err(error) = session.set_meeting_name(&name)
            {
                self.send(LiveEvent::WriteError(error.to_string()));
            }
            // Fixed rate without catching up in a burst after an overrun.
            next += self.cfg.poll;
            let now = Instant::now();
            if next < now {
                next = now + self.cfg.poll;
            }
        }
    }

    /// One frame. Returns `true` when writing keeps failing and capture must stop.
    fn step(&mut self, observations: &[RawObservation], read_ms: f64) -> bool {
        self.frames += 1;
        let stamp = self.stamp_now();
        let now_s = match self.cfg.debounce {
            DebounceClock::Mono => stamp.mono_ms / 1000.0,
            DebounceClock::Wall => stamp.wall_ms as f64 / 1000.0,
        };
        let ctx = StepCtx {
            now_s,
            read_ms: read_ms.is_finite().then_some(read_ms),
        };
        let outcome = self
            .reconciler
            .step_with(observations, &ctx, self.frames, stamp);
        self.emit(outcome, stamp);
        if let Some(session) = self.session.as_mut()
            && let Err(error) = session.heartbeat(stamp)
        {
            self.send(LiveEvent::WriteError(error.to_string()));
        }
        self.write_gave_up
    }

    fn emit(&mut self, outcome: mc_reconcile::StepOutcome, stamp: Stamp) {
        for record in &outcome.records {
            self.records += 1;
            let Some(session) = self.session.as_mut() else {
                continue;
            };
            let line = CaptionLine::from_record(session.next_seq(), record, || stamp);
            match session.write_line(&line) {
                Ok(()) => self.write_failures = 0,
                Err(error) => {
                    self.write_errors += 1;
                    if self.write_failures == 0 {
                        let _ = self.tx.send(LiveEvent::WriteError(error.to_string()));
                    }
                    self.write_failures += 1;
                    if self.write_failures >= MAX_WRITE_FAILURES {
                        self.write_gave_up = true;
                    }
                }
            }
        }
        if let Some(message) = idx_drift(
            self.write_errors,
            !outcome.committed.is_empty(),
            &mut self.drift_reported,
        ) {
            self.send(LiveEvent::WriteError(message));
        }
        for segment in outcome.committed {
            let idx = self.committed;
            self.committed += 1;
            self.send(LiveEvent::Segment { idx, segment });
        }
        if outcome.changed {
            let lines = self.reconciler.open_lines();
            if lines != self.last_open {
                self.last_open.clone_from(&lines);
                self.send(LiveEvent::Open(lines));
            }
        }
    }

    /// Always runs, panic or not: flush, close the source, close the manifest.
    fn finish(mut self, reason: EndReason) -> LiveSummary {
        let budget_end = Instant::now() + FINISH_BUDGET;
        let left = move |cap: Duration| {
            budget_end
                .saturating_duration_since(Instant::now())
                .min(cap)
                .max(MIN_STEP_WAIT)
        };
        let stamp = self.stamp_now();
        // A panic while flushing must not skip closing the manifest.
        let flushed = catch_unwind(AssertUnwindSafe(|| {
            let outcome = self.reconciler.flush_with(stamp);
            self.emit(outcome, stamp);
        }));
        let mut reason = if flushed.is_err() {
            EndReason::Panic
        } else {
            reason
        };
        // Lost lines are never reported as a clean end.
        if self.write_errors > 0 && reason.is_clean() {
            reason = EndReason::WriteFailed;
        }
        if let Some(worker) = self.source.take() {
            worker.close(left(SOURCE_CLOSE_WAIT));
        }
        // Captions close first; each wait below is cut to what is left of the budget.
        let audio_info = match self.audio.take() {
            Some(mut recorder) => {
                let mut info = recorder.stop(left(self.audio_stop_timeout));
                if let Some(asr) = self.live_asr.take() {
                    let (summary, rest) = asr.stop(left(Duration::from_millis(1000)));
                    self.forward_live_events(rest);
                    info["live_asr"] = serde_json::json!({
                        "segments": summary.segments,
                        "dropped_utterances": summary.dropped_utterances,
                        "dropped_ms": summary.dropped_ms,
                        "stuck": summary.stuck,
                        "file": "asr_live.jsonl",
                    });
                }
                if self.rec {
                    self.rec = false;
                    self.send(LiveEvent::Rec(false));
                }
                Some(info)
            }
            None => self.audio_failure.take(),
        };
        let (mut dir, mut id, mut close_error) = (None, None, None);
        let stuck = audio_info.as_ref().is_some_and(|info| {
            ["mic", "system"]
                .iter()
                .any(|ch| info[*ch]["status"] == "stuck")
        });
        if let Some(session) = self.session.as_mut() {
            if stuck {
                // An abandoned writer never refreshed its header again.
                crate::corpus::repair_session_audio(session.dir());
            }
            if let Some(info) = &audio_info
                && let Err(error) =
                    session.set_audio_info(info.clone(), self.audio_versions.clone())
            {
                close_error = Some(error.to_string());
            }
            dir = Some(session.dir().to_path_buf());
            id = Some(session.session_id().to_string());
            if let Err(error) = session.close(reason.as_str(), stamp) {
                close_error = Some(error.to_string());
            }
        }
        if let Some(message) = close_error {
            self.send(LiveEvent::WriteError(message));
            if reason.is_clean() {
                reason = EndReason::WriteFailed;
            }
        }
        self.send(LiveEvent::Ended {
            reason,
            frames: self.frames,
            records: self.records,
        });
        LiveSummary {
            reason,
            frames: self.frames,
            records: self.records,
            session_dir: dir,
            session_id: id,
            audio: audio_info,
        }
    }
}

/// A recorded corpus turned into something the live driver can replay.
pub struct FakeCorpus {
    pub steps: Vec<FakeStep>,
    /// One stamp per frame the driver will poll (gap frames repeat the previous
    /// stamp), then the flush stamp when the session ended.
    pub stamps: Vec<Stamp>,
    pub meeting_name: Option<String>,
    pub t0_wall_ms: i64,
    pub debounce: DebounceClock,
}

impl FakeCorpus {
    pub fn source(&self) -> FakeSource {
        let source = FakeSource::new(self.steps.clone());
        match &self.meeting_name {
            Some(name) => source.with_meeting_name(name.clone()),
            None => source,
        }
    }
}

/// Reads a corpus and builds the frames, stamps and metadata to replay it live.
pub fn fake_from_corpus(dir: impl AsRef<std::path::Path>) -> Result<FakeCorpus, ReplayError> {
    let dir = dir.as_ref();
    let manifest = Manifest::load(dir.join("manifest.json"))?;
    let name = if manifest.captions.file.is_empty() {
        "captions.jsonl"
    } else {
        manifest.captions.file.as_str()
    };
    let report = read_file(dir.join(name))?;
    let ops = reconstruct(&report)?;
    let (mut steps, mut stamps) = (Vec::new(), Vec::new());
    let mut previous: Option<Stamp> = None;
    let mut expected_frame = 1u64;
    let mut flush = None;
    for op in &ops {
        match op {
            ReplayOp::Frame {
                frame,
                stamp,
                read_ms,
                nodes,
            } => {
                // Frames that left no record are replayed as silent frames on
                // the previous stamp, so frame numbers line up and nothing
                // commits early.
                while expected_frame < *frame {
                    steps.push(FakeStep::Frame {
                        nodes: Vec::new(),
                        read_ms: None,
                    });
                    stamps.push(previous.unwrap_or(*stamp));
                    expected_frame += 1;
                }
                steps.push(FakeStep::Frame {
                    nodes: nodes.clone(),
                    read_ms: *read_ms,
                });
                stamps.push(*stamp);
                previous = Some(*stamp);
                expected_frame = frame + 1;
            }
            ReplayOp::Flush { stamp } => flush = Some(*stamp),
        }
    }
    if let Some(stamp) = flush.or_else(|| {
        manifest.ended_wall_ms.map(|wall_ms| Stamp {
            wall_ms,
            mono_ms: manifest.ended_mono_ms.unwrap_or(0.0),
        })
    }) {
        stamps.push(stamp);
    }
    let debounce = if crate::replay::debounce_is_mono(&manifest)? {
        DebounceClock::Mono
    } else {
        DebounceClock::Wall
    };
    Ok(FakeCorpus {
        steps,
        stamps,
        meeting_name: manifest.meeting_name.clone(),
        t0_wall_ms: manifest.started_wall_ms,
        debounce,
    })
}

#[cfg(test)]
mod drift_tests {
    use super::idx_drift;

    #[test]
    fn lost_lines_are_reported_once_at_the_next_commit() {
        let mut reported = false;
        assert!(idx_drift(0, true, &mut reported).is_none(), "nothing lost");
        assert!(
            idx_drift(3, false, &mut reported).is_none(),
            "no commit yet"
        );
        assert!(
            idx_drift(3, true, &mut reported)
                .unwrap()
                .contains("replay")
        );
        assert!(idx_drift(5, true, &mut reported).is_none(), "once");
    }
}
