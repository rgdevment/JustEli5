//! Starts a capture and hands its pieces to a front end (plain CLI or TUI).
//!
//! The front end asks for consent before calling [`start`]; nothing here
//! prompts. Events are drained in batches with the `Open` updates coalesced to
//! the latest one, so a slow renderer never lags behind a busy capture.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mc_core::{Clock, SessionClock, Stamp};
use mc_reconcile::Policy;
use mc_store::StoreError;

use crate::corpus::{CorpusSession, SessionSpec};
use crate::live::{
    AudioSpec, DebounceClock, LiveConfig, LiveEvent, LiveSummary, SessionOpener, SessionSlot,
    SourceFactory, spawn_with_audio,
};
use crate::live_analyst::{AnalystSpec, LiveAnalyst, transcript_segment};
use crate::sidecar::{self, SpeakerNames, TermFix};

/// What to record (absent: a live view that writes nothing).
#[derive(Debug, Clone)]
pub struct RecordSpec {
    pub platform: String,
    pub capture_source: String,
    /// A name the user chose; it wins over the source's.
    pub meeting_name: Option<String>,
    /// `"flag"` or `"prompt"`.
    pub consent_method: &'static str,
    /// Also record audio (the user consented to it for this meeting).
    pub audio: Option<AudioSpec>,
}

pub struct CaptureSpec {
    pub source: SourceFactory,
    pub cfg: LiveConfig,
    pub record: Option<RecordSpec>,
    /// Stop when a `STOP` file appears in the session folder (or, before one exists, the corpus folder).
    pub stop_file: bool,
    pub until: Option<Duration>,
    /// The live analyst; it needs the session, so it starts when the session is created.
    pub analyst: Option<AnalystSpec>,
}

pub struct Capture {
    stop: Arc<AtomicBool>,
    rx: Receiver<LiveEvent>,
    join: Option<JoinHandle<LiveSummary>>,
    stop_files: bool,
    corpus_root: PathBuf,
    deadline: Option<Instant>,
    clock: Arc<SessionClock>,
    /// Set from the `Session` event; the STOP file lives in it.
    session_dir: OnceLock<PathBuf>,
    analyst_spec: Cell<Option<AnalystSpec>>,
    analyst: RefCell<Option<LiveAnalyst>>,
}

/// The capture thread panicked outside its own guard.
#[derive(Debug)]
pub struct Panicked;

/// Why [`Capture::finish_timeout`] gave no summary.
#[derive(Debug, PartialEq, Eq)]
pub enum FinishError {
    Panicked,
    /// The thread did not end in time (a source stuck in a native call). It is
    /// left running; its captions are already on disk.
    Hung,
}

/// How long a dropped capture waits for its thread after asking it to stop.
const DROP_WAIT: Duration = Duration::from_secs(3);
const STOP_FILE: &str = "STOP";
/// How long the end of a plain capture waits for the analyst's last window.
const ANALYST_WAIT: Duration = Duration::from_secs(30);

/// Creates the session (and its audio setup) when the meeting is found.
fn session_opener(
    record: RecordSpec,
    corpus_root: PathBuf,
    t0_wall_ms: i64,
    audio: bool,
    policy: Policy,
) -> SessionOpener {
    Box::new(move |opened_at| {
        let mut session = CorpusSession::create(
            &corpus_root,
            &SessionSpec {
                platform: &record.platform,
                capture_source: &record.capture_source,
                meeting_name: record.meeting_name.clone(),
                consent_method: record.consent_method,
                t0_wall_ms,
                debounce_clock: DebounceClock::Mono.manifest_name(),
            },
        )?;
        session.set_policy(policy)?;
        if audio && let Err(error) = session.enable_audio() {
            // Do not leave a session folder that looks like a crashed capture.
            let _ = session.close("audio_setup_failed", opened_at);
            return Err(error);
        }
        Ok(session)
    })
}

/// Fails now, not at the meeting, when the corpus folder could not be written (leaves nothing behind).
fn check_writable(corpus_root: &Path) -> Result<(), StoreError> {
    let existing = corpus_root
        .ancestors()
        .find(|path| path.exists())
        .unwrap_or(Path::new("."));
    let probe = existing.join(format!(".mc-write-check-{}", std::process::id()));
    let written = std::fs::write(&probe, b"x").and_then(|()| std::fs::remove_file(&probe));
    written.map_err(|e| StoreError::io(&probe, e))
}

/// Starts the capture; a recording's folder appears at `LiveEvent::Session`, not before.
pub fn start(spec: CaptureSpec, corpus_root: &Path) -> Result<Capture, StoreError> {
    if spec.record.is_some() {
        check_writable(corpus_root)?;
    }
    if spec.stop_file {
        // A STOP left behind by an earlier run must not end this capture.
        let _ = std::fs::remove_file(corpus_root.join(STOP_FILE));
    }
    let clock = Arc::new(SessionClock::new());
    let t0 = clock.t0_wall_ms();
    let audio = spec.record.as_ref().and_then(|r| r.audio.clone());
    let policy = spec.cfg.reconciler.policy;
    let slot = match spec.record {
        Some(record) => SessionSlot::Lazy(session_opener(
            record,
            corpus_root.to_path_buf(),
            t0,
            audio.is_some(),
            policy,
        )),
        None => SessionSlot::Off,
    };
    let stop = Arc::new(AtomicBool::new(false));
    let (join, rx) = spawn_with_audio(
        spec.source,
        spec.cfg,
        clock.clone(),
        t0,
        slot,
        Arc::clone(&stop),
        audio,
    );
    Ok(Capture {
        stop,
        rx,
        join: Some(join),
        stop_files: spec.stop_file,
        corpus_root: corpus_root.to_path_buf(),
        deadline: spec.until.map(|d| Instant::now() + d),
        clock,
        session_dir: OnceLock::new(),
        analyst_spec: Cell::new(spec.analyst),
        analyst: RefCell::new(None),
    })
}

impl Drop for Capture {
    /// A capture dropped without `finish` (the UI failed or panicked) still
    /// stops and closes its session instead of dying mid-write with the process.
    fn drop(&mut self) {
        let Some(join) = self.join.take() else { return };
        self.request_stop();
        let deadline = Instant::now() + DROP_WAIT;
        while !join.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if join.is_finished() {
            let _ = join.join();
        }
    }
}

impl Capture {
    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }

    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// The session folder, once the meeting was found and the event polled.
    pub fn session_dir(&self) -> Option<&Path> {
        self.session_dir.get().map(PathBuf::as_path)
    }

    /// The corpus folder's STOP works from the start; the session's once it exists.
    fn stop_file_paths(&self) -> Vec<PathBuf> {
        if !self.stop_files {
            return Vec::new();
        }
        let mut paths = vec![self.corpus_root.join(STOP_FILE)];
        paths.extend(self.session_dir().map(|dir| dir.join(STOP_FILE)));
        paths
    }

    /// Milliseconds on the session clock.
    pub fn mono_ms(&self) -> f64 {
        self.clock.now().mono_ms
    }

    /// Waits up to `wait` for an event, then drains whatever else is queued.
    /// Consecutive `Open` events collapse to the last one. Also fires the
    /// `--until` deadline and the STOP file. Empty when nothing happened (or the
    /// capture has finished and its channel is closed).
    pub fn poll(&self, wait: Duration) -> Vec<LiveEvent> {
        let mut events = Vec::new();
        match self.rx.recv_timeout(wait) {
            Ok(event) => events.push(event),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
        }
        while let Ok(event) = self.rx.try_recv() {
            events.push(event);
        }
        // Updates of earlier segments come first; what this batch forwards answers in a later poll.
        let mut out = self
            .analyst
            .borrow()
            .as_ref()
            .map_or_else(Vec::new, LiveAnalyst::drain);
        for event in &events {
            match event {
                LiveEvent::Session { dir, .. } => {
                    let _ = self.session_dir.set(dir.clone());
                    self.start_analyst(dir);
                }
                LiveEvent::Segment { idx, segment } => {
                    if let Some(analyst) = self.analyst.borrow().as_ref() {
                        analyst.push(transcript_segment(*idx, segment));
                    }
                }
                _ => {}
            }
        }
        if self.deadline.is_some_and(|d| Instant::now() >= d)
            || self.stop_file_paths().iter().any(|f| f.exists())
        {
            self.request_stop();
        }
        out.extend(coalesce_open(events));
        out
    }

    fn start_analyst(&self, session_dir: &Path) {
        if let Some(spec) = self.analyst_spec.take() {
            let analyst = LiveAnalyst::start(spec, session_dir.to_path_buf());
            *self.analyst.borrow_mut() = Some(analyst);
        }
    }

    /// Asks the analyst to finish its last window without waiting for it; `poll` keeps
    /// delivering what it reports and `analyst_pending` says when it is over.
    pub fn stop_analyst(&self) {
        if let Some(analyst) = self.analyst.borrow_mut().as_mut() {
            analyst.close();
        }
    }

    /// Whether an analyst exists and has not ended.
    pub fn analyst_pending(&self) -> bool {
        self.analyst.borrow().as_ref().is_some_and(|a| !a.is_done())
    }

    /// Lets the analyst do its last window (waiting at most `wait`) and returns what it
    /// reported meanwhile. Nothing when there was no analyst or it was closed already.
    pub fn close_analyst(&self, wait: Duration) -> Vec<LiveEvent> {
        let analyst = self.analyst.borrow_mut().take();
        analyst.map_or_else(Vec::new, |a| a.finish(wait))
    }

    fn stamp(&self) -> Stamp {
        self.clock.now()
    }

    fn session_for_notes(&self) -> Result<&Path, String> {
        self.session_dir()
            .ok_or_else(|| "there is no session folder yet".to_string())
    }

    pub fn add_note(&self, text: &str) -> Result<LiveEvent, String> {
        let dir = self.session_for_notes()?;
        sidecar::append_note(dir, self.stamp(), text).map(LiveEvent::Note)
    }

    pub fn mark(&self) -> Result<LiveEvent, String> {
        let dir = self.session_for_notes()?;
        let at = self.stamp();
        sidecar::append_mark(dir, at)?;
        Ok(LiveEvent::Mark {
            mono_ms: at.mono_ms,
            wall_ms: at.wall_ms,
        })
    }

    /// Accepted by the user; the analyst reads it before its next window.
    pub fn fix_term(&self, heard: &str, correct: &str) -> Result<(), String> {
        let fix = TermFix {
            heard: heard.to_string(),
            correct: correct.to_string(),
        };
        sidecar::append_term_fix(self.session_for_notes()?, self.stamp(), &fix)
    }

    pub fn rename_speaker(&self, from: &str, to: &str) -> Result<SpeakerNames, String> {
        sidecar::set_speaker_name(self.session_for_notes()?, from, to)
    }

    fn remove_stop_file(&self) {
        for file in self.stop_file_paths() {
            let _ = std::fs::remove_file(file);
        }
    }

    /// Whether the capture thread has ended (its events may still be queued).
    pub fn is_finished(&self) -> bool {
        self.join.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Joins the capture thread and removes the STOP file.
    pub fn finish(mut self) -> Result<LiveSummary, Panicked> {
        self.remove_stop_file();
        let summary = match self.join.take() {
            Some(join) => join.join().map_err(|_| Panicked),
            None => Err(Panicked),
        };
        self.close_analyst(ANALYST_WAIT);
        summary
    }

    /// Like [`finish`](Self::finish) but gives up after `limit`, leaving a stuck
    /// thread detached instead of hanging the caller.
    pub fn finish_timeout(mut self, limit: Duration) -> Result<LiveSummary, FinishError> {
        self.remove_stop_file();
        let Some(join) = self.join.take() else {
            return Err(FinishError::Panicked);
        };
        let deadline = Instant::now() + limit;
        while !join.is_finished() {
            if Instant::now() >= deadline {
                return Err(FinishError::Hung);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let summary = join.join().map_err(|_| FinishError::Panicked);
        // Within what is left of `limit`; a window still running finishes (and saves) detached.
        self.close_analyst(deadline.saturating_duration_since(Instant::now()));
        summary
    }
}

/// Keeps only the last `Open` of the whole batch, in the position it arrived.
fn coalesce_open(events: Vec<LiveEvent>) -> Vec<LiveEvent> {
    let last_open = events.iter().rposition(|e| matches!(e, LiveEvent::Open(_)));
    events
        .into_iter()
        .enumerate()
        .filter(|(index, event)| !matches!(event, LiveEvent::Open(_)) || Some(*index) == last_open)
        .map(|(_, event)| event)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_events_collapse_to_the_last_one() {
        let events = vec![
            LiveEvent::Open(vec!["a".into()]),
            LiveEvent::Open(vec!["b".into()]),
            LiveEvent::NoCaptions { secs: 1 },
            LiveEvent::Open(vec!["c".into()]),
        ];
        let out = coalesce_open(events);
        assert_eq!(out.len(), 2);
        assert!(matches!(&out[0], LiveEvent::NoCaptions { .. }));
        assert!(matches!(&out[1], LiveEvent::Open(lines) if lines == &["c".to_string()]));
    }
}
