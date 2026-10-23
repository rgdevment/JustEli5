//! The live analyst on its own thread. Segments go in through a bounded queue that never
//! blocks the sender; ledger updates come back as events. The capture therefore never waits
//! for the model: a slow or failing model only makes the ledger late.
//!
//! Everything it writes goes to `<session>/analysis/live/`, apart from the captions.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, channel, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mc_analyst::{
    Analyst, AnalystConfig, Background, CarriedCommitment, ChangedItem, Ledger, LedgerEvent,
    PROMPT_VERSION, Step, TranscriptSegment, render_events, replace_folder,
};
use mc_core::Segment;
use mc_llm::Llm;
use serde::Serialize;
use serde_json::json;

use crate::live::LiveEvent;
use crate::runs::rel_ms;
use crate::sidecar::{display_name, read_speaker_names, read_term_fixes};

/// Segments that may wait for the analyst; beyond this the newest are skipped (and said so).
pub const QUEUE: usize = 512;
/// Pending segments are analysed after this much quiet: a pause is often a change of topic.
const IDLE_FLUSH: Duration = Duration::from_secs(45);
const LIVE_FOLDER: &str = "analysis/live";

/// Finds the open commitments of the previous meeting once the session folder exists.
pub type CarryLoader = Box<dyn FnOnce(&Path) -> Vec<CarriedCommitment> + Send>;

/// Reads again what can change while the meeting runs (the glossary the user corrects), before
/// every window.
pub type Refresh = Box<dyn FnMut(&mut Background) + Send>;

pub struct AnalystSpec {
    pub llm: Box<dyn Llm>,
    pub cfg: AnalystConfig,
    pub background: Background,
    /// Runs on the analyst thread before the first window, so a slow lookup never holds the capture.
    pub carry: Option<CarryLoader>,
    pub refresh: Option<Refresh>,
    pub provider: String,
    pub model: Option<String>,
}

/// What a finished window left in the ledger.
#[derive(Debug, Clone)]
pub struct LedgerUpdate {
    pub ledger: Ledger,
    pub changed: Vec<ChangedItem>,
    pub event: LedgerEvent,
}

pub struct LiveAnalyst {
    queue: Option<SyncSender<TranscriptSegment>>,
    events: Receiver<LiveEvent>,
    skipped: Arc<AtomicU64>,
    join: Option<JoinHandle<()>>,
}

impl LiveAnalyst {
    pub fn start(spec: AnalystSpec, session_dir: PathBuf) -> Self {
        Self::start_with_queue(spec, session_dir, QUEUE)
    }

    pub fn start_with_queue(spec: AnalystSpec, session_dir: PathBuf, capacity: usize) -> Self {
        let (queue, segments) = sync_channel(capacity);
        let (out, events) = channel();
        let skipped = Arc::new(AtomicU64::new(0));
        let worker = Worker::new(spec, session_dir, out, Arc::clone(&skipped));
        let join = std::thread::spawn(move || worker.run(&segments));
        Self {
            queue: Some(queue),
            events,
            skipped,
            join: Some(join),
        }
    }

    /// Hands a segment over without waiting; `false` when the queue is full.
    pub fn push(&self, segment: TranscriptSegment) -> bool {
        let Some(queue) = &self.queue else {
            return false;
        };
        let sent = queue.try_send(segment).is_ok();
        if !sent {
            self.skipped.fetch_add(1, Ordering::Relaxed);
        }
        sent
    }

    /// Closes the queue: the analyst does what is left and ends.
    pub fn close(&mut self) {
        self.queue = None;
    }

    /// Whether the analyst thread has ended.
    pub fn is_done(&self) -> bool {
        self.join.as_ref().is_none_or(JoinHandle::is_finished)
    }

    pub fn drain(&self) -> Vec<LiveEvent> {
        self.events.try_iter().collect()
    }

    /// Closes the queue so the rest is analysed, and waits at most `wait` for it. A model that
    /// is still busy is left running; what it finishes later is still saved.
    pub fn finish(mut self, wait: Duration) -> Vec<LiveEvent> {
        self.queue = None;
        let deadline = Instant::now() + wait;
        if let Some(join) = self.join.take() {
            while !join.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if join.is_finished() {
                let _ = join.join();
            }
        }
        self.drain()
    }
}

struct Worker {
    analyst: Analyst,
    base: Background,
    carry: Option<CarryLoader>,
    refresh: Option<Refresh>,
    dir: PathBuf,
    provider: String,
    model: Option<String>,
    out: Sender<LiveEvent>,
    skipped: Arc<AtomicU64>,
    skipped_total: u64,
    analysed: usize,
    last_id: Option<u64>,
}

fn pretty<T: Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).expect("a value serialises") + "\n"
}

impl Worker {
    fn new(
        spec: AnalystSpec,
        dir: PathBuf,
        out: Sender<LiveEvent>,
        skipped: Arc<AtomicU64>,
    ) -> Self {
        Self {
            analyst: Analyst::new(spec.llm, spec.cfg, spec.background.clone()),
            base: spec.background,
            carry: spec.carry,
            refresh: spec.refresh,
            dir,
            provider: spec.provider,
            model: spec.model,
            out,
            skipped,
            skipped_total: 0,
            analysed: 0,
            last_id: None,
        }
    }

    fn run(mut self, segments: &Receiver<TranscriptSegment>) {
        loop {
            match segments.recv_timeout(IDLE_FLUSH) {
                Ok(first) => {
                    let mut batch = vec![first];
                    batch.extend(segments.try_iter());
                    self.accept(batch);
                    self.work(false);
                }
                Err(RecvTimeoutError::Timeout) => self.work(true),
                Err(RecvTimeoutError::Disconnected) => {
                    self.work(true);
                    if !self.analyst.events().is_empty() {
                        self.persist(true);
                    }
                    return;
                }
            }
        }
    }

    /// Speakers go to the model under the names the user gave them.
    fn accept(&mut self, batch: Vec<TranscriptSegment>) {
        let names = read_speaker_names(&self.dir);
        for mut segment in batch {
            segment.speaker = display_name(&names, &segment.speaker).to_string();
            self.analyst.push(segment);
        }
    }

    /// The glossary and context, plus the corrections the user accepted so far.
    fn background(&self) -> Background {
        let fixes = read_term_fixes(&self.dir);
        let mut background = self.base.clone();
        if fixes.is_empty() {
            return background;
        }
        for fix in &fixes {
            if !background.glossary.contains(&fix.correct) {
                background.glossary.push(fix.correct.clone());
            }
        }
        background.term_fixes = fixes.into_iter().map(|f| (f.heard, f.correct)).collect();
        background
    }

    fn work(&mut self, flush: bool) {
        if let Some(load) = self.carry.take() {
            self.base.carried = load(&self.dir);
        }
        if let Some(refresh) = self.refresh.as_mut() {
            refresh(&mut self.base);
        }
        self.report_skipped();
        self.analyst.set_background(self.background());
        while let Some(step) = self.analyst.step(flush) {
            self.publish(&step);
            self.report_skipped();
        }
    }

    fn report_skipped(&mut self) {
        let skipped = self.skipped.swap(0, Ordering::Relaxed);
        if skipped > 0 {
            self.skipped_total += skipped;
            self.say(LiveEvent::AnalystError(format!(
                "the analyst is behind: {skipped} segments were not analysed"
            )));
        }
    }

    fn say(&self, event: LiveEvent) {
        let _ = self.out.send(event);
    }

    fn publish(&mut self, step: &Step) {
        let event = &step.event;
        self.analysed += event.segments;
        self.last_id = Some(event.last_id);
        if let Some(error) = &event.error {
            self.say(LiveEvent::AnalystError(format!(
                "segments #{}-#{} not analysed: {error}",
                event.first_id, event.last_id
            )));
        } else {
            self.say(LiveEvent::Ledger(Box::new(LedgerUpdate {
                ledger: self.analyst.ledger().clone(),
                changed: step.changed.clone(),
                event: event.clone(),
            })));
        }
        self.persist(false);
    }

    fn persist(&self, finished: bool) {
        if let Err(error) = self.write(finished) {
            self.say(LiveEvent::AnalystError(format!(
                "saving the live ledger: {error}"
            )));
        }
    }

    fn write(&self, finished: bool) -> Result<(), String> {
        let events = self.analyst.events();
        let failed = events.iter().filter(|e| e.error.is_some()).count();
        let meta = json!({
            "source": "live",
            "prompt_version": PROMPT_VERSION,
            "provider": self.provider,
            "model": self.model,
            "windows": events.len(),
            "failed_windows": failed,
            "segments_analysed": self.analysed,
            "segments_skipped": self.skipped_total,
            "last_id": self.last_id,
            "finished": finished,
            "complete": finished && failed == 0 && self.skipped_total == 0,
        });
        replace_folder(
            &folder(&self.dir),
            &[
                ("ledger_events.jsonl", render_events(events)),
                ("ledger.json", pretty(self.analyst.ledger())),
                ("meta.json", pretty(&meta)),
            ],
        )
        .map_err(|e| e.to_string())
    }
}

/// The segment as the analyst sees it, with the times `load_run` gives the same segment.
pub fn transcript_segment(idx: u64, segment: &Segment) -> TranscriptSegment {
    let start = rel_ms(segment).unwrap_or(0.0);
    let span = match (segment.first_mono_ms, segment.last_mono_ms) {
        (Some(first), Some(last)) => (last - first).max(0.0),
        _ => 0.0,
    };
    TranscriptSegment {
        id: idx,
        speaker: segment.speaker.clone(),
        text: segment.text.clone(),
        start_ms: start as i64,
        end_ms: (start + span) as i64,
    }
}

/// `<session>/analysis/live`.
pub fn folder(session_dir: &Path) -> PathBuf {
    session_dir.join(LIVE_FOLDER)
}
