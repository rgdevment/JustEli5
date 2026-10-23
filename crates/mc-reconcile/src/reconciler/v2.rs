//! `V2`: the conservative fixes to the legacy reconciler.
//!
//! What it changes, each with the defect it answers:
//! * several open lines per speaker, matched one-to-one per frame (D1);
//! * a line committed by a pause that then keeps growing, or is corrected, is
//!   amended in place instead of becoming a second line (D2, D3). To make
//!   "in place" possible without retractions in the output, a committed line
//!   is held back for [`AMEND_WINDOW_S`] before it is released to the
//!   transcript, so the run's segments are always final;
//! * the history of committed lines is unbounded (D4) and short repeats are
//!   only dropped when they come back quickly (D5);
//! * Unicode letters count as content (D6);
//! * grow and merge compare whole words (D7);
//! * an all-duplicates commit does not count as a change (D8);
//! * a wall clock that goes backwards cannot strand a line (D9).
//!
//! V2 never emits `newline`; lines are committed by debounce or flush only.

use std::collections::HashMap;

use mc_core::pytext::{clean_text, normalize_text, py_round3};
use mc_core::speakers::normalize_speaker;
use mc_core::{Decision, RawObservation, Segment};

use super::{Reconciler, StepOutcome, difflib};

/// How long a committed line can still be amended (and is held back).
pub const AMEND_WINDOW_S: f64 = 6.0;
/// A short line committed again within this gap is a repeat of the same one.
pub const REAPPEAR_GAP_S: f64 = 3.0;
/// Lines of at least this many words are treated as the same line when they
/// come back within [`LONG_REPEAT_S`] of being last seen.
pub const LONG_WORDS: usize = 4;
/// A long line is not committed twice while it keeps being seen at least this
/// often; after that long unseen, saying it again is a new utterance.
pub const LONG_REPEAT_S: f64 = 120.0;
/// Bumped whenever the rules change; part of the run id.
pub const V2_REV: u32 = 2;

#[derive(Debug, Clone)]
struct OpenLine {
    text: String,
    norm: String,
    ts: f64,
    span: Option<(f64, f64)>,
    raw: Option<String>,
    /// The held segment this line reopened.
    amends: Option<usize>,
}

#[derive(Debug, Clone)]
struct Held {
    speaker: String,
    raw: Option<String>,
    text: String,
    norm: String,
    last_seen: f64,
    span: Option<(f64, f64)>,
    stamp: (Option<f64>, Option<i64>),
    reopened: bool,
}

#[derive(Debug, Default)]
pub(super) struct State {
    open: Vec<(String, OpenLine)>,
    held: Vec<Held>,
    released: usize,
    /// `(speaker, norm)` of committed lines, with when they were last seen.
    done: HashMap<(String, String), f64>,
    /// `now` of the last frame, used by flush.
    last_now: f64,
}

/// What an observation matched this frame.
#[derive(Debug, Clone, Copy)]
enum Target {
    Open(usize),
    Held(usize),
}

#[derive(Debug, Clone, Copy)]
enum Kind {
    Exact,
    Norm,
    Grow,
    /// The line shrank (its start or end scrolled off): nothing to change.
    Shrink,
    Merge {
        prefix_ratio: f64,
        ratio: f64,
    },
}

struct Item {
    label: String,
    raw: Option<String>,
    text: String,
    norm: String,
    words: Vec<String>,
    matched: Option<(Target, Kind)>,
    ignored: Option<Decision>,
    /// The text an ignored observation is recorded with.
    shown: Option<String>,
}

fn words_of(norm: &str) -> Vec<String> {
    norm.split_whitespace().map(str::to_string).collect()
}

fn has_content(text: &str) -> bool {
    text.chars().any(char::is_alphanumeric)
}

/// `old` is a whole-word prefix of `new`, or sits inside it on word boundaries.
fn words_grow(old: &[String], new: &[String]) -> bool {
    !old.is_empty()
        && old.len() < new.len()
        && (new.starts_with(old) || new.windows(old.len()).any(|w| w == old))
}

fn word_prefix_ratio(a: &[String], b: &[String]) -> Option<f64> {
    let shorter = a.len().min(b.len());
    if shorter < 2 {
        return None;
    }
    let common = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    Some(common as f64 / shorter as f64)
}

impl Reconciler {
    pub(super) fn v2_open_lines(&self) -> Vec<String> {
        // Committed lines still held back are not in the transcript yet.
        let mut lines: Vec<String> = self.v2.held[self.v2.released..]
            .iter()
            .filter(|h| !h.reopened)
            .map(|h| format!("[{}]: {}", h.speaker, h.text))
            .collect();
        let mut open: Vec<&(String, OpenLine)> = self.v2.open.iter().collect();
        open.sort_by(|a, b| a.1.ts.total_cmp(&b.1.ts));
        lines.extend(
            open.into_iter()
                .map(|(sp, line)| format!("[{sp}]: {}", line.text)),
        );
        lines
    }

    pub(super) fn v2_frame(&mut self, observations: &[RawObservation], now: f64) -> StepOutcome {
        let mut changed = false;
        // D9: a clock that went back must not strand lines in the future.
        for (_, line) in &mut self.v2.open {
            if line.ts > now {
                line.ts = now;
            }
        }
        // Released lines are final: only the ones still held can be affected.
        for held in &mut self.v2.held[self.v2.released..] {
            held.last_seen = held.last_seen.min(now);
        }
        for seen in self.v2.done.values_mut() {
            *seen = seen.min(now);
        }
        self.v2.done.retain(|_, seen| now - *seen < LONG_REPEAT_S);
        self.v2.last_now = now;
        let mono = self.frame_mono();

        let mut items: Vec<Item> = Vec::new();
        for obs in observations {
            self.reads += 1;
            let (label, _) = normalize_speaker(obs.speaker.as_deref());
            let raw_text = obs.text.as_deref().unwrap_or("");
            let mut item = Item {
                label,
                raw: obs.speaker.clone(),
                text: String::new(),
                norm: String::new(),
                words: Vec::new(),
                matched: None,
                ignored: None,
                shown: None,
            };
            if raw_text.is_empty() {
                item.ignored = Some(Decision::EmptyText);
                item.shown = obs.text.clone();
                self.discarded += 1;
            } else {
                let clean = clean_text(raw_text);
                if !has_content(&clean) {
                    item.ignored = Some(Decision::NoAlnum);
                    item.shown = Some(clean);
                    self.discarded += 1;
                } else {
                    self.last_caption_time = now;
                    item.norm = normalize_text(&clean);
                    item.words = words_of(&item.norm);
                    item.text = clean;
                }
            }
            items.push(item);
        }

        self.v2_match(&mut items, now);

        for item in &items {
            self.speaker_raw = item.raw.clone();
            if let Some(decision) = &item.ignored {
                let text = item.shown.clone();
                self.record(&item.label, text, decision.clone(), None);
                continue;
            }
            if self.v2_apply(item, now, mono) {
                changed = true;
            }
        }

        if self.v2_commit_stale(now, false) {
            changed = true;
        }
        self.v2_release(now, false);
        self.finish(changed)
    }

    /// Passes in order — exact, grow, merge — each over the observations still
    /// unmatched; a line can be claimed by one observation per frame.
    fn v2_match(&self, items: &mut [Item], now: f64) {
        let mut claimed_open: Vec<bool> = vec![false; self.v2.open.len()];
        let mut claimed_held: Vec<bool> = vec![false; self.v2.held.len()];
        let amendable = |h: &Held| !h.reopened && now - h.last_seen <= AMEND_WINDOW_S;

        for pass in 0..3 {
            for item in items.iter_mut() {
                if item.matched.is_some() || item.ignored.is_some() {
                    continue;
                }
                let mut best: Option<(f64, Target, Kind)> = None;
                let mut consider = |score: f64, target: Target, kind: Kind| {
                    if best.as_ref().is_none_or(|(s, _, _)| score > *s) {
                        best = Some((score, target, kind));
                    }
                };
                for (index, (speaker, line)) in self.v2.open.iter().enumerate() {
                    if claimed_open[index] || speaker != &item.label {
                        continue;
                    }
                    let old = words_of(&line.norm);
                    match pass {
                        0 if line.text == item.text => {
                            consider(2.0, Target::Open(index), Kind::Exact)
                        }
                        0 if line.norm == item.norm => {
                            consider(1.9, Target::Open(index), Kind::Norm)
                        }
                        // A prefix or suffix of the open line: it scrolled, not grew.
                        0 if item.words.len() >= 2
                            && item.words.len() < old.len()
                            && (old.starts_with(&item.words) || old.ends_with(&item.words)) =>
                        {
                            consider(1.8, Target::Open(index), Kind::Shrink);
                        }
                        1 if words_grow(&old, &item.words) => {
                            // Prefer the line the new text extends the most.
                            let cover = old.len() as f64 / item.words.len() as f64;
                            consider(0.9 + 0.1 * cover, Target::Open(index), Kind::Grow);
                        }
                        2 => {
                            let ratio = difflib::ratio(&line.norm, &item.norm);
                            let prefix = word_prefix_ratio(&old, &item.words);
                            if prefix.is_some_and(|p| p >= 0.5)
                                || ratio >= self.cfg.merge_similarity
                            {
                                let prefix_ratio = prefix.unwrap_or(0.0);
                                consider(
                                    prefix_ratio.max(ratio),
                                    Target::Open(index),
                                    Kind::Merge {
                                        prefix_ratio,
                                        ratio,
                                    },
                                );
                            }
                        }
                        _ => {}
                    }
                }
                for (index, held) in self.v2.held.iter().enumerate().skip(self.v2.released) {
                    if claimed_held[index] || held.speaker != item.label || !amendable(held) {
                        continue;
                    }
                    let old = words_of(&held.norm);
                    // A short line coming back after a pause is a new
                    // utterance; a long one is the same line either way.
                    let same_line =
                        old.len() >= LONG_WORDS || now - held.last_seen < REAPPEAR_GAP_S;
                    match pass {
                        0 if same_line && held.norm == item.norm => {
                            consider(1.5, Target::Held(index), Kind::Exact);
                        }
                        // The line shrank (its start scrolled off): still the
                        // same committed line, nothing to add.
                        0 if same_line
                            && item.words.len() >= 2
                            && item.words.len() < old.len()
                            && (old.starts_with(&item.words) || old.ends_with(&item.words)) =>
                        {
                            consider(1.4, Target::Held(index), Kind::Norm);
                        }
                        // Extending a committed line is the same utterance only
                        // while it is the same line, or when it continues it.
                        1 if words_grow(&old, &item.words)
                            && (same_line || item.words.starts_with(&old)) =>
                        {
                            let cover = old.len() as f64 / item.words.len() as f64;
                            consider(0.8 + 0.1 * cover, Target::Held(index), Kind::Grow);
                        }
                        2 => {
                            let ratio = difflib::ratio(&held.norm, &item.norm);
                            let prefix = word_prefix_ratio(&old, &item.words);
                            // Never amend a committed line into a shorter one.
                            // The same text after a pause is a new utterance.
                            let not_shorter =
                                item.words.len() >= old.len() && held.norm != item.norm;
                            // A shared start alone is not a correction of a line
                            // that is already committed: it needs real similarity.
                            if not_shorter && ratio >= self.cfg.merge_similarity {
                                let prefix_ratio = prefix.unwrap_or(0.0);
                                consider(
                                    ratio - 0.1,
                                    Target::Held(index),
                                    Kind::Merge {
                                        prefix_ratio,
                                        ratio,
                                    },
                                );
                            }
                        }
                        _ => {}
                    }
                }
                if let Some((_, target, kind)) = best {
                    match target {
                        Target::Open(i) => claimed_open[i] = true,
                        Target::Held(i) => claimed_held[i] = true,
                    }
                    item.matched = Some((target, kind));
                }
            }
        }
    }

    /// Applies one matched (or new) observation. Returns whether state changed.
    fn v2_apply(&mut self, item: &Item, now: f64, mono: Option<f64>) -> bool {
        let text = Some(item.text.clone());
        match item.matched {
            None => {
                self.v2.open.push((
                    item.label.clone(),
                    OpenLine {
                        text: item.text.clone(),
                        norm: item.norm.clone(),
                        ts: now,
                        span: mono.map(|m| (m, m)),
                        raw: item.raw.clone(),
                        amends: None,
                    },
                ));
                self.last_activity_time = now;
                self.record(&item.label, text, Decision::NewOpen, None);
                true
            }
            Some((Target::Open(index), kind)) => {
                let line = &mut self.v2.open[index].1;
                match kind {
                    Kind::Exact => {
                        self.record(&item.label, text, Decision::DupExact, None);
                        false
                    }
                    Kind::Shrink => {
                        self.record(&item.label, text, Decision::DupNorm, None);
                        false
                    }
                    Kind::Norm => {
                        if item.text.chars().count() > line.text.chars().count() {
                            line.text = item.text.clone();
                            if let (Some(span), Some(m)) = (line.span.as_mut(), mono) {
                                span.1 = m;
                            }
                            self.record(&item.label, text, Decision::DupNormLonger, None);
                            true
                        } else {
                            self.record(&item.label, text, Decision::DupNorm, None);
                            false
                        }
                    }
                    Kind::Grow | Kind::Merge { .. } => {
                        line.text = item.text.clone();
                        line.norm = item.norm.clone();
                        line.ts = now;
                        line.raw = item.raw.clone();
                        if let (Some(span), Some(m)) = (line.span.as_mut(), mono) {
                            span.1 = m;
                        }
                        self.last_activity_time = now;
                        let decision = match kind {
                            Kind::Merge {
                                prefix_ratio,
                                ratio,
                            } => Decision::Merge {
                                prefix_ratio,
                                ratio,
                            },
                            _ => Decision::Grow,
                        };
                        self.record(&item.label, text, decision, None);
                        true
                    }
                }
            }
            Some((Target::Held(index), kind)) => match kind {
                Kind::Exact | Kind::Norm | Kind::Shrink => {
                    self.v2.held[index].last_seen = now;
                    if let Some(seen) = self.v2.done.get_mut(&(
                        self.v2.held[index].speaker.clone(),
                        self.v2.held[index].norm.clone(),
                    )) {
                        *seen = now;
                    }
                    self.record(&item.label, text, Decision::SeenCommitted, None);
                    false
                }
                Kind::Grow | Kind::Merge { .. } => {
                    let held = &mut self.v2.held[index];
                    held.reopened = true;
                    let span = held.span.map(|(first, _)| (first, mono.unwrap_or(first)));
                    self.v2.open.push((
                        item.label.clone(),
                        OpenLine {
                            text: item.text.clone(),
                            norm: item.norm.clone(),
                            ts: now,
                            span,
                            raw: item.raw.clone(),
                            amends: Some(index),
                        },
                    ));
                    self.last_activity_time = now;
                    self.record(&item.label, text, Decision::Amend, None);
                    true
                }
            },
        }
    }

    /// Commits the open lines that stayed unchanged for the debounce (all of
    /// them when `all`), oldest first. Returns whether anything was committed
    /// or amended (a pure duplicate skip is not a change: D8).
    fn v2_commit_stale(&mut self, now: f64, all: bool) -> bool {
        let debounce = self.cfg.debounce_s;
        let mut due: Vec<(usize, f64)> = self
            .v2
            .open
            .iter()
            .enumerate()
            .filter(|(_, (_, line))| all || now - line.ts >= debounce)
            .map(|(index, (_, line))| (index, line.ts))
            .collect();
        if due.is_empty() {
            return false;
        }
        due.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        let mut changed = false;
        let mut taken: Vec<usize> = Vec::new();
        for (index, _) in due {
            let (speaker, line) = self.v2.open[index].clone();
            taken.push(index);
            if self.v2_commit_line(&speaker, &line, now) {
                changed = true;
            }
        }
        taken.sort_unstable_by(|a, b| b.cmp(a));
        for index in taken {
            self.v2.open.remove(index);
        }
        changed
    }

    fn v2_commit_line(&mut self, speaker: &str, line: &OpenLine, now: f64) -> bool {
        let span = line.span.map(|(a, b)| (py_round3(a), py_round3(b)));
        self.speaker_raw = line.raw.clone();
        if let Some(held_index) = line.amends {
            let before = self.records.len();
            self.record(speaker, Some(line.text.clone()), Decision::Amended, span);
            let stamp = self.records.get(before).and_then(|r| r.stamp);
            let held = &mut self.v2.held[held_index];
            held.text = line.text.clone();
            held.norm = line.norm.clone();
            held.raw = line.raw.clone();
            held.span = line.span;
            held.last_seen = now;
            held.reopened = false;
            held.stamp = (stamp.map(|s| s.mono_ms), stamp.map(|s| s.wall_ms));
            self.v2
                .done
                .insert((speaker.to_string(), line.norm.clone()), now);
            return true;
        }
        let key = (speaker.to_string(), line.norm.clone());
        let words = words_of(&line.norm).len();
        let window = if words >= LONG_WORDS {
            LONG_REPEAT_S
        } else {
            REAPPEAR_GAP_S
        };
        if let Some(seen) = self.v2.done.get_mut(&key)
            && now - *seen < window
        {
            *seen = now;
            self.record(
                speaker,
                Some(line.text.clone()),
                Decision::DupCommittedSkip,
                span,
            );
            return false;
        }
        let before = self.records.len();
        self.record(speaker, Some(line.text.clone()), Decision::Committed, span);
        let stamp = self.records.get(before).and_then(|r| r.stamp);
        self.v2.done.insert(key, now);
        self.v2.held.push(Held {
            speaker: speaker.to_string(),
            raw: line.raw.clone(),
            text: line.text.clone(),
            norm: line.norm.clone(),
            last_seen: now,
            span: line.span,
            stamp: (stamp.map(|s| s.mono_ms), stamp.map(|s| s.wall_ms)),
            reopened: false,
        });
        true
    }

    /// Moves held lines whose amend window passed (all, on flush) to the
    /// transcript, oldest first, so a reopened line keeps its place.
    fn v2_release(&mut self, now: f64, all: bool) {
        while let Some(held) = self.v2.held.get(self.v2.released) {
            if !all && (held.reopened || now - held.last_seen <= AMEND_WINDOW_S) {
                break;
            }
            let span = held.span.map(|(a, b)| (py_round3(a), py_round3(b)));
            self.committed.push(Segment {
                speaker: held.speaker.clone(),
                speaker_raw: held.raw.clone(),
                text: held.text.clone(),
                first_mono_ms: span.map(|s| s.0),
                last_mono_ms: span.map(|s| s.1),
                committed_mono_ms: held.stamp.0,
                committed_wall_ms: held.stamp.1,
            });
            self.v2.released += 1;
        }
    }

    pub(super) fn v2_flush(&mut self) -> StepOutcome {
        let now = self.v2.last_now;
        self.v2_commit_stale(now, true);
        self.v2.open.clear();
        self.v2_release(f64::MAX, true);
        self.finish(false)
    }
}
