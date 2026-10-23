//! Line reconciler: turns the volatile caption stream (lines that grow, get
//! corrected in place and are re-emitted every frame) into a stable transcript.
//!
//! This is a faithful port of the original Python `CaptureManager` (`V1Legacy`), kept free of
//! I/O: every operation returns the records that Python would have sent to its
//! recorder and the transcript lines it committed, so a failing sink can never
//! abort a frame half way. Block segmentation (`check_snapshot`) is not ported.
//!
//! The behaviour, quirks included, is pinned by the parity vectors exported
//! from the Python code (`fixtures/vectors/{characterized,fuzz}.json`).

use std::cmp::Ordering;
use std::collections::VecDeque;
use std::sync::Arc;

use indexmap::IndexMap;
use mc_core::config::{LINE_DEBOUNCE_SEC, MERGE_SIMILARITY, RECENT_COMMITTED_CAPACITY};
use mc_core::pytext::{clean_text, common_prefix_len, has_alnum, normalize_text, py_round3};
use mc_core::speakers::normalize_speaker;
use mc_core::{Clock, Decision, FrameRef, RawObservation, Record, Segment, Stamp};

use crate::difflib;

mod v2;

pub use v2::V2_REV;

/// Which reconciliation rules apply. Only the legacy (Python-identical) rules
/// exist so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Policy {
    #[default]
    V1Legacy,
    /// The conservative fixes; see [`v2`].
    V2,
}

#[derive(Debug, Clone)]
pub struct ReconcilerConfig {
    /// Seconds a line must stay unchanged before it is committed.
    pub debounce_s: f64,
    /// Similarity at or above which a divergent frame still counts as a correction.
    pub merge_similarity: f64,
    /// How many recently committed signatures are remembered.
    pub recent_capacity: usize,
    pub policy: Policy,
}

impl Default for ReconcilerConfig {
    fn default() -> Self {
        Self {
            debounce_s: LINE_DEBOUNCE_SEC,
            merge_similarity: MERGE_SIMILARITY,
            recent_capacity: RECENT_COMMITTED_CAPACITY,
            policy: Policy::V1Legacy,
        }
    }
}

/// Context of one `step` (one source read).
#[derive(Debug, Clone, Copy)]
pub struct StepCtx {
    /// The time the manager reads once per update (`time.time()` in Python).
    pub now_s: f64,
    /// How long the source read took, for the records' `read_ms`.
    pub read_ms: Option<f64>,
}

/// What an operation produced.
#[derive(Debug, Clone, Default)]
pub struct StepOutcome {
    pub changed: bool,
    /// Records Python would have logged, in order (empty without a recorder).
    pub records: Vec<Record>,
    /// Transcript lines committed by this operation.
    pub committed: Vec<Segment>,
}

#[derive(Debug, Clone)]
struct Track {
    text: String,
    ts: f64,
}

/// Reconciliation state, in the shape the parity vectors snapshot it.
#[derive(Debug, Clone, PartialEq)]
pub struct StateSnapshot {
    pub tracks: Vec<(String, String, f64)>,
    pub span: Vec<(String, (f64, f64))>,
    pub track_raw: Vec<(String, Option<String>)>,
    pub recent_len: usize,
    pub recent_hash: String,
    pub reads: u64,
    pub discarded: u64,
    pub last_caption_time: f64,
    pub last_activity_time: f64,
    pub frame: u64,
    pub speaker_raw: Option<String>,
    pub frame_stamp: Option<Stamp>,
}

/// Orders two `ts` values like Python's `sorted(key=ts)` for every real input
/// (`-0.0` and `0.0` compare equal, so ties keep insertion order under a stable
/// sort) while still being a total order when a NaN sneaks in: Rust's sort
/// panics on comparators that are not. Every NaN (either sign: `0.0 / 0.0` is a
/// negative NaN on x86) is canonicalised and sorts after every number, in
/// insertion order. Python leaves NaN ordering undefined, so that case is
/// outside the parity contract.
fn cmp_ts(a: f64, b: f64) -> Ordering {
    let normalize = |x: f64| {
        if x.is_nan() {
            f64::NAN
        } else if x == 0.0 {
            0.0
        } else {
            x
        }
    };
    normalize(a).total_cmp(&normalize(b))
}

/// FNV-1a 64 over the `(speaker, text)` signatures, each entry encoded as
/// `speaker 0x00 text 0x01` in UTF-8.
pub fn recent_hash<'a>(recent: impl IntoIterator<Item = &'a (String, String)>) -> String {
    let mut value: u64 = 0xCBF2_9CE4_8422_2325;
    for (speaker, text) in recent {
        for byte in speaker
            .bytes()
            .chain(std::iter::once(0u8))
            .chain(text.bytes())
            .chain(std::iter::once(1u8))
        {
            value ^= u64::from(byte);
            value = value.wrapping_mul(0x0000_0100_0000_01B3);
        }
    }
    format!("{value:016x}")
}

/// Where stamps come from: nowhere (no recorder clock), a live clock, or the
/// caller (replay feeds the stamps a corpus already recorded).
enum StampMode {
    Off,
    Clock(Arc<dyn Clock>),
    External,
}

pub struct Reconciler {
    cfg: ReconcilerConfig,
    /// Fixed at construction like Python's `deque(maxlen=...)`.
    recent_capacity: usize,
    stamps: StampMode,
    has_recorder: bool,
    /// Stamp for out-of-frame records in `External` mode (set by `flush_with`).
    external_now: Option<Stamp>,

    tracks: IndexMap<String, Track>,
    track_raw: IndexMap<String, Option<String>>,
    span: IndexMap<String, (f64, f64)>,
    recent: VecDeque<(String, String)>,
    reads: u64,
    discarded: u64,
    last_caption_time: f64,
    last_activity_time: f64,
    frame: u64,
    frame_stamp: Option<Stamp>,
    read_ms: Option<f64>,
    in_frame: bool,
    speaker_raw: Option<String>,

    records: Vec<Record>,
    committed: Vec<Segment>,
    v2: v2::State,
}

impl Reconciler {
    /// `init_time_s` is the time the manager saw at construction (it seeds
    /// `last_caption_time` / `last_activity_time`). Records are only produced
    /// (and, outside a frame, only ask the clock for stamps) when
    /// `has_recorder` is true, exactly like the Python recorder hook.
    pub fn new(
        cfg: ReconcilerConfig,
        init_time_s: f64,
        clock: Option<Arc<dyn Clock>>,
        has_recorder: bool,
    ) -> Self {
        Self {
            recent_capacity: cfg.recent_capacity,
            cfg,
            stamps: match clock {
                Some(clock) => StampMode::Clock(clock),
                None => StampMode::Off,
            },
            has_recorder,
            external_now: None,
            tracks: IndexMap::new(),
            track_raw: IndexMap::new(),
            span: IndexMap::new(),
            recent: VecDeque::new(),
            reads: 0,
            discarded: 0,
            last_caption_time: init_time_s,
            last_activity_time: init_time_s,
            frame: 0,
            frame_stamp: None,
            read_ms: None,
            in_frame: false,
            speaker_raw: None,
            records: Vec::new(),
            committed: Vec::new(),
            v2: v2::State::default(),
        }
    }

    /// A reconciler whose stamps and frame numbers are supplied by the caller
    /// through `step_with` / `flush_with`; it always produces records.
    pub fn for_replay(cfg: ReconcilerConfig, init_time_s: f64) -> Self {
        let mut reconciler = Self::new(cfg, init_time_s, None, true);
        reconciler.stamps = StampMode::External;
        reconciler
    }

    // -- operations ----------------------------------------------------

    /// One `update()`: reconcile the observations the source returned.
    pub fn step(&mut self, observations: &[RawObservation], ctx: &StepCtx) -> StepOutcome {
        assert!(!matches!(self.stamps, StampMode::External), "use step_with");
        if let StampMode::Clock(clock) = &self.stamps {
            self.frame += 1;
            self.frame_stamp = Some(clock.now());
            self.read_ms = ctx.read_ms;
            self.in_frame = true;
        }
        self.run_frame(observations, ctx.now_s)
    }

    /// `step` for replay: the frame number and stamp come from the corpus.
    pub fn step_with(
        &mut self,
        observations: &[RawObservation],
        ctx: &StepCtx,
        frame: u64,
        stamp: Stamp,
    ) -> StepOutcome {
        assert!(matches!(self.stamps, StampMode::External), "use for_replay");
        self.frame = frame;
        self.frame_stamp = Some(stamp);
        self.read_ms = ctx.read_ms;
        self.in_frame = true;
        self.run_frame(observations, ctx.now_s)
    }

    fn run_frame(&mut self, observations: &[RawObservation], now: f64) -> StepOutcome {
        if self.cfg.policy == Policy::V2 {
            let outcome = self.v2_frame(observations, now);
            self.in_frame = false;
            return outcome;
        }
        let mut changed = false;
        for obs in observations {
            self.reads += 1;
            let (label, _) = normalize_speaker(obs.speaker.as_deref());
            self.speaker_raw = obs.speaker.clone();
            let raw_text = obs.text.as_deref().unwrap_or("");
            if raw_text.is_empty() {
                self.discarded += 1;
                self.record(&label, obs.text.clone(), Decision::EmptyText, None);
                continue;
            }
            let clean = clean_text(raw_text);
            if !has_alnum(&clean) {
                self.discarded += 1;
                self.record(&label, Some(clean), Decision::NoAlnum, None);
                continue;
            }
            self.last_caption_time = now;
            if self.ingest_inner(&label, &clean, now) {
                changed = true;
            }
        }
        if self.commit_stale_inner(now) {
            changed = true;
        }
        self.in_frame = false;
        self.finish(changed)
    }

    /// `flush()` / `check_snapshot(force_flush=True)`: commit every open line.
    pub fn flush(&mut self) -> StepOutcome {
        assert!(
            !matches!(self.stamps, StampMode::External),
            "use flush_with"
        );
        self.flush_inner()
    }

    /// `flush` for replay: out-of-frame records carry the recorded stamp.
    pub fn flush_with(&mut self, stamp: Stamp) -> StepOutcome {
        assert!(matches!(self.stamps, StampMode::External), "use for_replay");
        self.external_now = Some(stamp);
        self.flush_inner()
    }

    fn flush_inner(&mut self) -> StepOutcome {
        if self.cfg.policy == Policy::V2 {
            return self.v2_flush();
        }
        let mut open: Vec<(String, String, f64)> = self
            .tracks
            .iter()
            .map(|(sp, tr)| (sp.clone(), tr.text.clone(), tr.ts))
            .collect();
        open.sort_by(|a, b| cmp_ts(a.2, b.2));
        for (speaker, text, _) in open {
            self.commit_line(&speaker, &text);
        }
        self.tracks.clear();
        self.span.clear();
        self.finish(false)
    }

    /// Direct `_ingest` (no frame): used by the parity vectors.
    #[doc(hidden)]
    pub fn ingest(&mut self, speaker: &str, text: &str, now_s: f64) -> (bool, StepOutcome) {
        let changed = self.ingest_inner(speaker, text, now_s);
        (changed, self.finish(changed))
    }

    /// Direct `_commit_stale` (no frame): used by the parity vectors.
    #[doc(hidden)]
    pub fn commit_stale(&mut self, now_s: f64) -> (bool, StepOutcome) {
        let changed = self.commit_stale_inner(now_s);
        (changed, self.finish(changed))
    }

    pub fn snapshot(&self) -> StateSnapshot {
        StateSnapshot {
            tracks: self
                .tracks
                .iter()
                .map(|(sp, tr)| (sp.clone(), tr.text.clone(), tr.ts))
                .collect(),
            span: self.span.iter().map(|(sp, v)| (sp.clone(), *v)).collect(),
            track_raw: self
                .track_raw
                .iter()
                .map(|(sp, raw)| (sp.clone(), raw.clone()))
                .collect(),
            recent_len: self.recent.len(),
            recent_hash: recent_hash(self.recent.iter()),
            reads: self.reads,
            discarded: self.discarded,
            last_caption_time: self.last_caption_time,
            last_activity_time: self.last_activity_time,
            frame: self.frame,
            speaker_raw: self.speaker_raw.clone(),
            frame_stamp: self.frame_stamp,
        }
    }

    /// Open (not yet committed) lines as `[speaker]: text`, oldest change first.
    pub fn open_lines(&self) -> Vec<String> {
        if self.cfg.policy == Policy::V2 {
            return self.v2_open_lines();
        }
        let mut open: Vec<(&String, &Track)> = self.tracks.iter().collect();
        open.sort_by(|a, b| cmp_ts(a.1.ts, b.1.ts));
        open.into_iter()
            .map(|(sp, tr)| format!("[{sp}]: {}", tr.text))
            .collect()
    }

    /// The tunables, which the Python tests change between operations.
    pub fn config_mut(&mut self) -> &mut ReconcilerConfig {
        &mut self.cfg
    }

    pub fn reads(&self) -> u64 {
        self.reads
    }

    pub fn discarded(&self) -> u64 {
        self.discarded
    }

    pub fn last_caption_time(&self) -> f64 {
        self.last_caption_time
    }

    // -- internals -----------------------------------------------------

    fn finish(&mut self, changed: bool) -> StepOutcome {
        StepOutcome {
            changed,
            records: std::mem::take(&mut self.records),
            committed: std::mem::take(&mut self.committed),
        }
    }

    /// Port of `_record`: the stamp comes from the frame when inside one, from
    /// a fresh clock reading otherwise, and only when a recorder exists.
    fn record(
        &mut self,
        speaker: &str,
        text: Option<String>,
        decision: Decision,
        span: Option<(f64, f64)>,
    ) {
        if !self.has_recorder {
            return;
        }
        let speaker_raw = match &self.speaker_raw {
            Some(raw) if !raw.is_empty() && raw != speaker => Some(raw.clone()),
            _ => None,
        };
        let (stamp, frame, read_ms) = match &self.stamps {
            StampMode::Off => (None, FrameRef::NoClock, None),
            _ if self.in_frame => (
                self.frame_stamp,
                FrameRef::InFrame(self.frame),
                self.read_ms,
            ),
            StampMode::Clock(clock) => (Some(clock.now()), FrameRef::OutOfFrame, None),
            StampMode::External => (self.external_now, FrameRef::OutOfFrame, None),
        };
        self.records.push(Record {
            speaker: speaker.to_string(),
            text,
            decision,
            speaker_raw,
            stamp,
            frame,
            read_ms,
            span,
        });
    }

    fn frame_mono(&self) -> Option<f64> {
        if matches!(self.stamps, StampMode::Off) {
            return None;
        }
        self.frame_stamp.map(|stamp| stamp.mono_ms)
    }

    fn touch_span(&mut self, speaker: &str, mono: Option<f64>, new: bool) {
        let Some(mono) = mono else { return };
        match self.span.get_mut(speaker) {
            Some(entry) if !new => entry.1 = mono,
            _ => {
                self.span.insert(speaker.to_string(), (mono, mono));
            }
        }
    }

    fn ingest_inner(&mut self, speaker: &str, text: &str, now: f64) -> bool {
        let prev_raw = self.track_raw.get(speaker).cloned().flatten();
        self.track_raw
            .insert(speaker.to_string(), self.speaker_raw.clone());
        let mono = self.frame_mono();

        let Some(prev) = self.tracks.get(speaker).map(|t| t.text.clone()) else {
            self.tracks.insert(
                speaker.to_string(),
                Track {
                    text: text.to_string(),
                    ts: now,
                },
            );
            self.touch_span(speaker, mono, true);
            self.last_activity_time = now;
            self.record(speaker, Some(text.to_string()), Decision::NewOpen, None);
            return true;
        };

        if text == prev {
            self.record(speaker, Some(text.to_string()), Decision::DupExact, None);
            return false;
        }

        let np = normalize_text(&prev);
        let nt = normalize_text(text);

        if np == nt {
            // Same content, different casing/punctuation: keep the longer form.
            if text.chars().count() > prev.chars().count() {
                if let Some(track) = self.tracks.get_mut(speaker) {
                    track.text = text.to_string();
                }
                self.touch_span(speaker, mono, false);
                self.record(
                    speaker,
                    Some(text.to_string()),
                    Decision::DupNormLonger,
                    None,
                );
                return true;
            }
            self.record(speaker, Some(text.to_string()), Decision::DupNorm, None);
            return false;
        }

        if !np.is_empty() && (nt.starts_with(&np) || nt.contains(&np)) {
            if let Some(track) = self.tracks.get_mut(speaker) {
                track.text = text.to_string();
                track.ts = now;
            }
            self.touch_span(speaker, mono, false);
            self.last_activity_time = now;
            self.record(speaker, Some(text.to_string()), Decision::Grow, None);
            return true;
        }

        // A long shared prefix is a more robust "same line being rewritten"
        // signal than the global ratio alone.
        let shorter = np.chars().count().min(nt.chars().count());
        let prefix_ratio = if shorter > 0 {
            common_prefix_len(&np, &nt) as f64 / shorter as f64
        } else {
            0.0
        };
        let ratio = if !np.is_empty() && !nt.is_empty() {
            difflib::ratio(&np, &nt)
        } else {
            0.0
        };
        if prefix_ratio >= 0.5 || ratio >= self.cfg.merge_similarity {
            // Keep the newest emission: it carries the correction even if shorter.
            if let Some(track) = self.tracks.get_mut(speaker) {
                track.text = text.to_string();
                track.ts = now;
            }
            self.touch_span(speaker, mono, false);
            self.last_activity_time = now;
            self.record(
                speaker,
                Some(text.to_string()),
                Decision::Merge {
                    prefix_ratio,
                    ratio,
                },
                None,
            );
            return true;
        }

        // Divergent: the previous line is done. It belongs to the alias that
        // wrote it, not to the one that just arrived under the same label.
        let incoming_raw = self.track_raw.get(speaker).cloned().flatten();
        self.track_raw.insert(speaker.to_string(), prev_raw);
        self.commit_line(speaker, &prev);
        self.track_raw
            .insert(speaker.to_string(), incoming_raw.clone());
        self.speaker_raw = incoming_raw;
        self.tracks.insert(
            speaker.to_string(),
            Track {
                text: text.to_string(),
                ts: now,
            },
        );
        self.touch_span(speaker, mono, true);
        self.last_activity_time = now;
        self.record(speaker, Some(text.to_string()), Decision::Newline, None);
        true
    }

    fn commit_line(&mut self, speaker: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        self.speaker_raw = self.track_raw.get(speaker).cloned().flatten();
        let sig = (speaker.to_string(), normalize_text(text));
        let span = self
            .span
            .get(speaker)
            .map(|&(first, last)| (py_round3(first), py_round3(last)));
        if self.recent.contains(&sig) {
            self.record(
                speaker,
                Some(text.to_string()),
                Decision::DupCommittedSkip,
                span,
            );
            return;
        }
        // A zero-capacity deque never stores anything.
        if self.recent_capacity > 0 {
            while self.recent.len() >= self.recent_capacity {
                self.recent.pop_front();
            }
            self.recent.push_back(sig);
        }
        let before = self.records.len();
        self.record(speaker, Some(text.to_string()), Decision::Committed, span);
        let stamp = self.records.get(before).and_then(|r| r.stamp);
        self.committed.push(Segment {
            speaker: speaker.to_string(),
            speaker_raw: self.speaker_raw.clone(),
            text: text.to_string(),
            first_mono_ms: span.map(|s| s.0),
            last_mono_ms: span.map(|s| s.1),
            committed_mono_ms: stamp.map(|s| s.mono_ms),
            committed_wall_ms: stamp.map(|s| s.wall_ms),
        });
    }

    fn commit_stale_inner(&mut self, now: f64) -> bool {
        let mut stale: Vec<(String, String, f64)> = self
            .tracks
            .iter()
            .filter(|(_, tr)| now - tr.ts >= self.cfg.debounce_s)
            .map(|(sp, tr)| (sp.clone(), tr.text.clone(), tr.ts))
            .collect();
        if stale.is_empty() {
            return false;
        }
        // Stable sort like Python's `sorted(key=ts)`: ties (including -0.0 vs
        // 0.0) keep the insertion order of the tracks.
        stale.sort_by(|a, b| cmp_ts(a.2, b.2));
        for (speaker, text, _) in stale {
            self.commit_line(&speaker, &text);
            self.tracks.shift_remove(&speaker);
            self.span.shift_remove(&speaker);
        }
        true
    }
}
