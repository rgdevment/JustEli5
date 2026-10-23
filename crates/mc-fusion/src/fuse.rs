//! Teams captions + local ASR -> one transcript.
//!
//! Teams knows who spoke and writes good Spanish; the ASR knows what was said
//! in English and when. The captions are the base of every phrase: the ASR
//! only replaces words where it is clearly right (a glossary term Teams
//! mangled) and fills what Teams never showed.

use std::collections::BTreeMap;

use mc_asr::filters::{FilterConfig, Verdict, judge};
use mc_asr::{AsrSegment, RawSegment};
use mc_core::Segment;
use mc_core::pytext::py_round3;
use mc_reconcile::difflib::ratio;
use serde::Serialize;

use crate::align::{Op, diff, lcs_len};
use crate::glossary::Glossary;
use crate::lag::{LagModel, LagSummary};
use crate::text::{Word, fold_words, split_punct, tokenize};

#[derive(Debug, Clone)]
pub struct FusionConfig {
    /// Display name of the person recording (Teams shows their own speech under it).
    /// Inferred from the audio when absent.
    pub me: Option<String>,
    pub glossary: Vec<String>,
    /// (form, term): a form the user taught, which Teams' text takes as the term.
    pub aliases: Vec<(String, String)>,
    /// Lag assumed when no caption could be paired with speech.
    pub default_lag_ms: f64,
    /// Longest lag considered when pairing captions with speech.
    pub max_lag_ms: f64,
    /// Samples in the local median of the lag.
    pub lag_neighbours: usize,
    /// Lags this far from the typical one are mismatches.
    pub lag_outlier_ms: f64,
    /// Word overlap needed to pair a caption with speech for the lag.
    pub min_pair_score: f64,
    /// Word overlap needed to give an ASR segment to a caption.
    pub min_assign_score: f64,
    /// ASR counts as reliable above this mean log-probability ...
    pub conf_good: f64,
    /// ... and below this no-speech probability.
    pub no_speech_ok: f64,
    /// Speech Teams never captioned is added from the ASR when it is this sure.
    pub allow_asr_only: bool,
    pub asr_only_min_conf: f64,
    pub asr_only_max_no_speech: f64,
    /// A mic segment this close to a system one is the meeting leaking into the mic.
    pub leak_score: f64,
    /// Speaker for ASR-only speech when nobody can be inferred.
    pub unnamed_speaker: String,
    pub filter: FilterConfig,
}

impl Default for FusionConfig {
    fn default() -> Self {
        Self {
            me: None,
            glossary: Vec::new(),
            aliases: Vec::new(),
            default_lag_ms: 1200.0,
            max_lag_ms: 6000.0,
            lag_neighbours: 7,
            lag_outlier_ms: 1800.0,
            min_pair_score: 0.5,
            min_assign_score: 0.2,
            conf_good: -0.5,
            no_speech_ok: 0.3,
            allow_asr_only: true,
            asr_only_min_conf: -0.7,
            asr_only_max_no_speech: 0.35,
            leak_score: 0.6,
            unnamed_speaker: "Participante".to_string(),
            filter: FilterConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Captions corrected or confirmed by the ASR.
    Merged,
    /// Captions with no ASR to compare.
    Captions,
    /// Speech Teams never showed, from the ASR alone.
    Asr,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Edit {
    pub from: String,
    pub to: String,
    pub reason: &'static str,
    /// False for suggestions left out of the text.
    pub applied: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FusedSegment {
    pub start_ms: f64,
    pub end_ms: f64,
    pub speaker: String,
    /// The speaker was guessed (ASR-only speech), not read from Teams.
    pub speaker_inferred: bool,
    pub ch: String,
    pub text: String,
    pub source: Source,
    pub caption_text: Option<String>,
    pub asr_text: Option<String>,
    pub edits: Vec<Edit>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct FusionCounts {
    pub captions: usize,
    pub asr_segments: usize,
    pub merged: usize,
    pub caption_only: usize,
    pub asr_only: usize,
    pub dropped_junk: usize,
    pub dropped_leak: usize,
    pub dropped_duplicate: usize,
    pub dropped_low_confidence: usize,
    pub edits_applied: usize,
    pub edits_suggested: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FusionReport {
    pub lag: LagSummary,
    pub me: Option<String>,
    /// `configured` (Teams shows that name) or `inferred` (from which captions line up with the mic).
    pub me_source: &'static str,
    pub speaker_channels: BTreeMap<String, String>,
    pub counts: FusionCounts,
}

#[derive(Debug, Clone)]
pub struct FusionResult {
    pub segments: Vec<FusedSegment>,
    pub report: FusionReport,
    /// The caption lag over time, for callers that need to put captions back
    /// where the speech was.
    pub lag: LagModel,
}

struct Cap<'a> {
    seg: &'a Segment,
    words: Vec<Word>,
    keys: Vec<String>,
    first: Option<f64>,
    last: Option<f64>,
}

struct Asr<'a> {
    seg: &'a AsrSegment,
    keys: Vec<String>,
    junk: bool,
    good: bool,
}

fn keys_of(words: &[Word]) -> Vec<String> {
    words.iter().map(|w| w.key.clone()).collect()
}

/// Share of the shorter text that appears, in order, in the other.
fn containment(a: &[String], b: &[String]) -> f64 {
    let shorter = a.len().min(b.len());
    if shorter == 0 {
        return 0.0;
    }
    lcs_len(a, b) as f64 / shorter as f64
}

fn join(words: &[Word]) -> String {
    words
        .iter()
        .map(|w| w.surface.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn as_raw(a: &AsrSegment) -> RawSegment {
    RawSegment {
        start_ms: a.start_ms,
        end_ms: a.end_ms,
        text: a.text.clone(),
        lang: a.lang.clone(),
        avg_logprob: a.conf,
        no_speech_prob: a.no_speech,
        words: Vec::new(),
    }
}

struct TextFusion {
    text: String,
    edits: Vec<Edit>,
}

/// Words two texts must share, in order, before they are taken for one speech.
const MIN_SHARED_WORDS: usize = 3;

/// Longest caption x ASR table fused word by word; bigger pairs are left to the captions.
const MAX_DIFF_CELLS: usize = 4_000_000;

/// Caption words [`lo`, `hi`) that best spell `term`, judged by sound-alike
/// letters (so "de ploy" ~ "deploy", "estaging" ~ "staging", "a estaging" is
/// only "estaging"). Shorter wins ties; too different gives nothing.
fn best_window(
    cap: &[Word],
    lo: usize,
    hi: usize,
    term_words: usize,
    term_key: &str,
) -> Option<(usize, usize)> {
    let mut best: Option<(f64, usize, usize)> = None;
    for len in 1..=(hi - lo).min(term_words + 2) {
        for start in lo..=hi - len {
            let joined: String = cap[start..start + len]
                .iter()
                .map(|w| w.key.as_str())
                .collect();
            let score = ratio(&joined, term_key);
            if best.is_none_or(|(b, _, _)| score > b + 1e-9) {
                best = Some((score, start, start + len));
            }
        }
    }
    best.filter(|(score, _, _)| *score >= 0.5)
        .map(|(_, start, end)| (start, end))
}

/// The caption words this ASR range lines up with, from the edit script.
fn caption_region(ops: &[Op], b_from: usize, b_to: usize) -> Option<(usize, usize)> {
    let (mut lo, mut hi) = (usize::MAX, 0);
    for op in ops {
        let (a, b) = match op {
            Op::Equal { a, b } | Op::Replace { a, b } => (a.clone(), b.clone()),
            Op::Delete { .. } | Op::Insert { .. } => continue,
        };
        if b.end <= b_from || b.start >= b_to {
            continue;
        }
        if let Op::Equal { .. } = op {
            // Only the words of the equal block inside the range.
            let from = a.start + b_from.saturating_sub(b.start);
            let to = a.start + (b_to.min(b.end) - b.start);
            lo = lo.min(from);
            hi = hi.max(to);
        } else {
            lo = lo.min(a.start);
            hi = hi.max(a.end);
        }
    }
    (lo < hi).then_some((lo, hi))
}

/// `core` with the leading/trailing punctuation of the words it replaces and
/// the capital of the first one.
fn dress_term(term: &str, replaced: &[Word]) -> String {
    let (lead, first_core, _) = split_punct(&replaced[0].surface);
    let (_, _, trail) = split_punct(&replaced[replaced.len() - 1].surface);
    let capital = first_core.chars().next().is_some_and(char::is_uppercase)
        && term.chars().next().is_some_and(char::is_lowercase);
    let mut chars = term.chars();
    let head: String = match (capital, chars.next()) {
        (true, Some(c)) => c.to_uppercase().collect(),
        (false, Some(c)) => c.to_string(),
        (_, None) => String::new(),
    };
    format!("{lead}{head}{}{trail}", chars.as_str())
}

/// The caption words written as `term` when the term's capitals carry meaning (SAGA, CIAM,
/// TypeScript) and the caption lost them; a plain capitalised word ("Daily") keeps its casing.
fn recased(term: &str, replaced: &[Word]) -> Option<String> {
    let meaningful = term.chars().skip(1).any(char::is_uppercase);
    let core: Vec<&str> = replaced.iter().map(|w| split_punct(&w.surface).1).collect();
    (meaningful && core.join(" ") != term).then(|| dress_term(term, replaced))
}

/// `can_extend`: no other caption claimed this ASR text, so words the caption
/// lacks at either end can only be its own.
fn fuse_text(
    cap: &[Word],
    asr_text: &str,
    asr_good: bool,
    can_extend: bool,
    glossary: &Glossary,
) -> TextFusion {
    let asr_words = tokenize(asr_text);
    let (cap_keys, asr_keys) = (keys_of(cap), keys_of(&asr_words));
    if cap.len().saturating_mul(asr_words.len()) > MAX_DIFF_CELLS {
        return TextFusion {
            text: join(cap),
            edits: Vec::new(),
        };
    }
    let ops = diff(&cap_keys, &asr_keys);
    let found = glossary.find(&asr_keys);
    let mut edits = Vec::new();

    // Term fixes: caption range -> the term as the user wrote it.
    let mut fixes: Vec<(usize, usize, String)> = Vec::new();
    let (mut head, mut tail): (Option<String>, Option<String>) = (None, None);
    for f in &found {
        let (b_from, b_to) = (f.start, f.start + f.len);
        let term = glossary.canonical(f.term);
        let term_key: String = term.split_whitespace().map(crate::text::fold).collect();
        match caption_region(&ops, b_from, b_to) {
            Some((lo, hi)) => {
                let joined: String = cap[lo..hi].iter().map(|w| w.key.as_str()).collect();
                // Right only if written as the same words ("back log" is not "backlog").
                if joined == term_key && hi - lo == f.len {
                    let replaced = &cap[lo..hi];
                    if let Some(to) = recased(term, replaced) {
                        edits.push(Edit {
                            from: join(replaced),
                            to: to.clone(),
                            reason: "glossary_case",
                            applied: true,
                        });
                        fixes.push((lo, hi, to));
                    }
                    continue;
                }
                let overlaps =
                    |s: usize, e: usize| fixes.iter().any(|(fs, fe, _)| s < *fe && *fs < e);
                match best_window(cap, lo, hi, f.len, &term_key) {
                    Some((s, e)) if !overlaps(s, e) => {
                        let replaced = &cap[s..e];
                        if replaced.len() == f.len
                            && replaced.iter().map(|w| w.key.as_str()).collect::<String>()
                                == term_key
                        {
                            continue;
                        }
                        let to = dress_term(term, replaced);
                        edits.push(Edit {
                            from: join(replaced),
                            to: to.clone(),
                            reason: "glossary",
                            applied: true,
                        });
                        fixes.push((s, e, to));
                    }
                    _ => edits.push(Edit {
                        from: join(&cap[lo..hi]),
                        to: term.to_string(),
                        reason: "asr_differs",
                        applied: false,
                    }),
                }
            }
            None => {
                // Teams has nothing here; only an edge of the line can be filled.
                let first = ops
                    .first()
                    .is_some_and(|op| matches!(op, Op::Insert { b } if b.contains(&b_from)));
                let last = ops
                    .last()
                    .is_some_and(|op| matches!(op, Op::Insert { b } if b.contains(&b_from)));
                if can_extend && asr_good && (first || last) {
                    edits.push(Edit {
                        from: String::new(),
                        to: term.to_string(),
                        reason: "glossary_insert",
                        applied: true,
                    });
                    if first {
                        head = Some(term.to_string());
                    } else {
                        tail = Some(term.to_string());
                    }
                } else {
                    edits.push(Edit {
                        from: String::new(),
                        to: term.to_string(),
                        reason: "asr_extra",
                        applied: false,
                    });
                }
            }
        }
    }

    // A form the user taught for a term, as Teams wrote it, becomes the term.
    for f in glossary.find_aliases(&cap_keys) {
        let (s, e) = (f.start, f.start + f.len);
        if fixes.iter().any(|(fs, fe, _)| s < *fe && *fs < e) {
            continue;
        }
        let replaced = &cap[s..e];
        let to = dress_term(glossary.alias_term(f.term), replaced);
        edits.push(Edit {
            from: join(replaced),
            to: to.clone(),
            reason: "glossary_alias",
            applied: true,
        });
        fixes.push((s, e, to));
    }

    // Words Teams stopped (or started) too late to show, when only this line owns them.
    let last_op = ops.len().saturating_sub(1);
    for (n, op) in ops.iter().enumerate() {
        let Op::Insert { b } = op else { continue };
        let at_edge = n == 0 || n == last_op;
        if !(at_edge && can_extend && asr_good && b.len() >= 2) {
            continue;
        }
        let words: Vec<String> = asr_words[b.clone()]
            .iter()
            .map(|w| w.surface.clone())
            .collect();
        // Terms inside were already handled above; do not say them twice.
        if found
            .iter()
            .any(|f| f.start < b.end && b.start < f.start + f.len)
        {
            continue;
        }
        edits.push(Edit {
            from: String::new(),
            to: words.join(" "),
            reason: if n == 0 { "head" } else { "tail" },
            applied: true,
        });
        let joined = words.join(" ");
        if n == 0 {
            head = Some(joined);
        } else {
            tail = Some(joined);
        }
    }

    let mut out: Vec<String> = Vec::new();
    out.extend(head);
    let mut i = 0;
    while i < cap.len() {
        match fixes.iter().find(|(s, _, _)| *s == i) {
            Some((_, e, to)) => {
                out.push(to.clone());
                i = *e;
            }
            None => {
                out.push(cap[i].surface.clone());
                i += 1;
            }
        }
    }
    out.extend(tail);
    TextFusion {
        text: out.join(" "),
        edits,
    }
}

pub fn fuse(captions: &[Segment], asr: &[AsrSegment], cfg: &FusionConfig) -> FusionResult {
    let glossary = Glossary::with_aliases(&cfg.glossary, &cfg.aliases);
    let mut counts = FusionCounts {
        captions: captions.len(),
        asr_segments: asr.len(),
        ..FusionCounts::default()
    };

    let caps: Vec<Cap> = captions
        .iter()
        .map(|seg| {
            let words = tokenize(&seg.text);
            Cap {
                keys: keys_of(&words),
                words,
                first: seg
                    .first_mono_ms
                    .or(seg.last_mono_ms)
                    .or(seg.committed_mono_ms)
                    .filter(|t| t.is_finite()),
                last: seg
                    .last_mono_ms
                    .or(seg.first_mono_ms)
                    .or(seg.committed_mono_ms)
                    .filter(|t| t.is_finite()),
                seg,
            }
        })
        .collect();
    let asrs: Vec<Asr> = asr
        .iter()
        .map(|seg| {
            let raw = as_raw(seg);
            let junk = !(seg.start_ms.is_finite() && seg.end_ms.is_finite())
                || matches!(judge(&raw, &cfg.filter), Verdict::Drop(_));
            Asr {
                keys: keys_of(&tokenize(&seg.text)),
                junk,
                good: seg.conf.is_none_or(|c| c >= cfg.conf_good)
                    && seg.no_speech.is_none_or(|n| n <= cfg.no_speech_ok),
                seg,
            }
        })
        .collect();
    counts.dropped_junk = asrs.iter().filter(|a| a.junk).count();

    // 1. Pair captions with speech by what they say, to learn the lag.
    let mut samples = Vec::new();
    let mut best_for_cap: Vec<Option<usize>> = vec![None; caps.len()];
    for (ci, c) in caps.iter().enumerate() {
        let Some(first) = c.first else { continue };
        let mut best: Option<(f64, f64, usize)> = None;
        for (ai, a) in asrs.iter().enumerate().filter(|(_, a)| !a.junk) {
            let delta = first - a.seg.start_ms;
            if !(-500.0..=cfg.max_lag_ms).contains(&delta) {
                continue;
            }
            // A couple of shared words ("el", "de") prove nothing about timing.
            if lcs_len(&c.keys, &a.keys) < MIN_SHARED_WORDS {
                continue;
            }
            let score = containment(&c.keys, &a.keys);
            if score < cfg.min_pair_score {
                continue;
            }
            let dist = (delta - cfg.default_lag_ms).abs();
            let better = best.is_none_or(|(s, d, _)| {
                score > s + 1e-9 || ((score - s).abs() <= 1e-9 && dist < d)
            });
            if better {
                best = Some((score, dist, ai));
            }
        }
        if let Some((_, _, ai)) = best {
            samples.push((first, first - asrs[ai].seg.start_ms));
            best_for_cap[ci] = Some(ai);
        }
    }
    let lag = LagModel::new(
        samples,
        cfg.default_lag_ms,
        cfg.lag_neighbours,
        cfg.lag_outlier_ms,
    );

    // 2. Which speaker is on which channel.
    let mut tally: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for (ci, best) in best_for_cap.iter().enumerate() {
        if let Some(ai) = best {
            let entry = tally.entry(caps[ci].seg.speaker.as_str()).or_default();
            if asrs[*ai].seg.ch == "mic" {
                entry.0 += 1;
            } else {
                entry.1 += 1;
            }
        }
    }
    let same_name = |a: &str, b: &str| fold_words(a) == fold_words(b);
    // The configured name only counts if Teams shows someone by it ("Ana Perez"
    // vs "Ana Pérez" is the same person); otherwise the audio decides.
    let configured = cfg
        .me
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .and_then(|name| {
            caps.iter()
                .map(|c| c.seg.speaker.as_str())
                .find(|speaker| same_name(name, speaker))
        })
        .map(str::to_string);
    let me_source = if configured.is_some() {
        "configured"
    } else {
        "inferred"
    };
    let me: Option<String> = configured.or_else(|| {
        tally
            .iter()
            .filter(|(_, (mic, sys))| *mic >= 2 && *mic as f64 >= 0.6 * (*mic + *sys) as f64)
            .max_by_key(|(name, (mic, _))| (*mic, std::cmp::Reverse(**name)))
            .map(|(name, _)| (*name).to_string())
    });
    let channel_of = |speaker: &str| -> &'static str {
        if me.as_deref().is_some_and(|m| same_name(m, speaker)) {
            "mic"
        } else {
            "system"
        }
    };
    let speaker_channels: BTreeMap<String, String> = caps
        .iter()
        .map(|c| {
            (
                c.seg.speaker.clone(),
                channel_of(&c.seg.speaker).to_string(),
            )
        })
        .collect();

    // 3. Give each ASR segment to the caption it overlaps most.
    let mut windows: Vec<Option<(f64, f64)>> = Vec::with_capacity(caps.len());
    for c in &caps {
        windows.push(match (c.first, c.last) {
            (Some(first), Some(last)) => {
                let l = lag.at(first);
                Some((first - l - 500.0, last.max(first) - l + 1200.0))
            }
            _ => None,
        });
    }
    // (text score, overlap, caption, asr): the text decides who owns a segment,
    // the overlap only breaks ties.
    let mut pairs: Vec<(f64, f64, usize, usize)> = Vec::new();
    for (ci, c) in caps.iter().enumerate() {
        let Some((from, to)) = windows[ci] else {
            continue;
        };
        let ch = channel_of(&c.seg.speaker);
        for (ai, a) in asrs
            .iter()
            .enumerate()
            .filter(|(_, a)| !a.junk && a.seg.ch == ch)
        {
            let overlap = to.min(a.seg.end_ms) - from.max(a.seg.start_ms);
            let duration = (a.seg.end_ms - a.seg.start_ms).max(1.0);
            if !overlap.is_finite() || overlap < (0.3 * duration).min(300.0) {
                continue;
            }
            let shorter = c.keys.len().min(a.keys.len());
            if shorter == 0 {
                continue;
            }
            let shared = lcs_len(&c.keys, &a.keys);
            let score = shared as f64 / shorter as f64;
            if shared < MIN_SHARED_WORDS.min(shorter) || score < cfg.min_assign_score {
                continue;
            }
            pairs.push((score, overlap, ci, ai));
        }
    }
    pairs.sort_by(|x, y| {
        y.0.total_cmp(&x.0)
            .then(y.1.total_cmp(&x.1))
            .then(x.2.cmp(&y.2))
            .then(x.3.cmp(&y.3))
    });
    // An ASR segment that two captions could both claim spans more than one
    // phrase: it may correct either, but must not extend any of them.
    let mut claimants: Vec<Vec<usize>> = vec![Vec::new(); asrs.len()];
    for &(_, _, ci, ai) in &pairs {
        if !claimants[ai].contains(&ci) {
            claimants[ai].push(ci);
        }
    }
    let contested: Vec<bool> = claimants.iter().map(|c| c.len() > 1).collect();
    let mut owner: Vec<Option<usize>> = vec![None; asrs.len()];
    let mut assigned: Vec<Vec<usize>> = vec![Vec::new(); caps.len()];
    let mut borrowed: Vec<Vec<usize>> = vec![Vec::new(); caps.len()];
    for (_, _, ci, ai) in pairs {
        if owner[ai].is_none() {
            owner[ai] = Some(ci);
            assigned[ci].push(ai);
        } else if contested[ai] && !borrowed[ci].contains(&ai) {
            // The other claimant keeps the segment; this one may still use it to
            // fix terms.
            borrowed[ci].push(ai);
        }
    }

    // 4. Captions, corrected by the speech they own.
    let mut fused: Vec<FusedSegment> = Vec::new();
    let mut last_end = 0.0f64;
    for (ci, c) in caps.iter().enumerate() {
        let ch = channel_of(&c.seg.speaker);
        let mut mine = assigned[ci].clone();
        mine.sort_by(|x, y| asrs[*x].seg.start_ms.total_cmp(&asrs[*y].seg.start_ms));
        let mut used = mine.clone();
        used.extend(
            borrowed[ci]
                .iter()
                .copied()
                .filter(|a| owner[*a] != Some(ci)),
        );
        used.sort_by(|x, y| asrs[*x].seg.start_ms.total_cmp(&asrs[*y].seg.start_ms));
        let caption_text = c.seg.text.clone();
        let first = c.first.unwrap_or(last_end);
        let l = lag.at(first);
        let (mut start, mut end) = (first - l, c.last.unwrap_or(first) - l);
        let (text, source, asr_text, edits) = if used.is_empty() {
            counts.caption_only += 1;
            (caption_text.clone(), Source::Captions, None, Vec::new())
        } else {
            counts.merged += 1;
            if !mine.is_empty() {
                start = mine
                    .iter()
                    .map(|i| asrs[*i].seg.start_ms)
                    .fold(f64::INFINITY, f64::min);
                end = mine
                    .iter()
                    .map(|i| asrs[*i].seg.end_ms)
                    .fold(f64::NEG_INFINITY, f64::max);
            }
            let joined = used
                .iter()
                .map(|i| asrs[*i].seg.text.trim())
                .collect::<Vec<_>>()
                .join(" ");
            let good = used.iter().all(|i| asrs[*i].good);
            let can_extend = used.iter().all(|i| !contested[*i]);
            let fusion = fuse_text(&c.words, &joined, good, can_extend, &glossary);
            (fusion.text, Source::Merged, Some(joined), fusion.edits)
        };
        counts.edits_applied += edits.iter().filter(|e| e.applied).count();
        counts.edits_suggested += edits.iter().filter(|e| !e.applied).count();
        last_end = last_end.max(end);
        fused.push(FusedSegment {
            start_ms: py_round3(start),
            end_ms: py_round3(end.max(start)),
            speaker: c.seg.speaker.clone(),
            speaker_inferred: false,
            ch: ch.to_string(),
            text,
            source,
            caption_text: Some(caption_text),
            asr_text,
            edits,
        });
    }

    // 5. Speech nobody captioned.
    let system_asr: Vec<&Asr> = asrs
        .iter()
        .filter(|a| !a.junk && a.seg.ch == "system")
        .collect();
    let system_speakers: Vec<(f64, f64, &str)> = fused
        .iter()
        .filter(|f| f.ch == "system")
        .map(|f| (f.start_ms, f.end_ms, f.speaker.as_str()))
        .collect();
    let mut extra: Vec<FusedSegment> = Vec::new();
    for (ai, a) in asrs.iter().enumerate() {
        if a.junk || owner[ai].is_some() {
            continue;
        }
        let (start, end) = (a.seg.start_ms, a.seg.end_ms);
        // The meeting leaking into the mic: the same words on the system channel.
        // (A short "sí" from the user is not worth dropping on a coincidence.)
        if a.seg.ch == "mic"
            && a.keys.len() >= MIN_SHARED_WORDS
            && system_asr.iter().any(|s| {
                s.seg.start_ms < end + 400.0
                    && start < s.seg.end_ms + 400.0
                    && containment(&a.keys, &s.keys) >= cfg.leak_score
            })
        {
            counts.dropped_leak += 1;
            continue;
        }
        // Already covered by a caption of that channel: not new speech.
        if fused.iter().any(|f| {
            f.ch == a.seg.ch
                && f.start_ms < end
                && start < f.end_ms
                && containment(&a.keys, &keys_of(&tokenize(&f.text))) >= 0.4
        }) {
            counts.dropped_duplicate += 1;
            continue;
        }
        let sure = a.seg.conf.is_none_or(|c| c >= cfg.asr_only_min_conf)
            && a.seg
                .no_speech
                .is_none_or(|n| n <= cfg.asr_only_max_no_speech);
        if !cfg.allow_asr_only || !sure {
            counts.dropped_low_confidence += 1;
            continue;
        }
        let (speaker, inferred) = if a.seg.ch == "mic" {
            (me.clone().unwrap_or_else(|| "Yo".to_string()), me.is_none())
        } else {
            nearest_speaker(&system_speakers, start, end, &cfg.unnamed_speaker)
        };
        counts.asr_only += 1;
        extra.push(FusedSegment {
            start_ms: py_round3(start),
            end_ms: py_round3(end),
            speaker,
            speaker_inferred: inferred,
            ch: a.seg.ch.clone(),
            text: a.seg.text.trim().to_string(),
            source: Source::Asr,
            caption_text: None,
            asr_text: Some(a.seg.text.trim().to_string()),
            edits: Vec::new(),
        });
    }
    fused.extend(extra);
    fused.sort_by(|a, b| a.start_ms.total_cmp(&b.start_ms).then(a.ch.cmp(&b.ch)));

    let report = FusionReport {
        lag: lag.summary(),
        me,
        me_source,
        speaker_channels,
        counts,
    };
    FusionResult {
        segments: fused,
        report,
        lag,
    }
}

/// The captioned system speaker closest in time, if the one before and the one
/// after agree (or only one is near).
fn nearest_speaker(
    spans: &[(f64, f64, &str)],
    start: f64,
    end: f64,
    unnamed: &str,
) -> (String, bool) {
    const NEAR_MS: f64 = 20_000.0;
    let before = spans
        .iter()
        .filter(|s| s.1 <= start + 1.0 && start - s.1 <= NEAR_MS)
        .max_by(|a, b| a.1.total_cmp(&b.1));
    let after = spans
        .iter()
        .filter(|s| s.0 >= end - 1.0 && s.0 - end <= NEAR_MS)
        .min_by(|a, b| a.0.total_cmp(&b.0));
    match (before, after) {
        (Some(b), Some(a)) if b.2 == a.2 => (b.2.to_string(), true),
        (Some(b), None) => (b.2.to_string(), true),
        (None, Some(a)) => (a.2.to_string(), true),
        _ => (unnamed.to_string(), true),
    }
}
