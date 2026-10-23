//! Scoring a transcript against a hand-corrected reference.
//!
//! Three questions, answered per variant (captions only, ASR only, fused):
//! how many words are wrong (WER), are the meeting's own terms right
//! (recall/precision over the glossary), and is the right person credited
//! (speaker accuracy). Pure functions: the same inputs give the same numbers.

use mc_core::speakers::normalize_speaker;
use mc_fusion::FusedSegment;
use mc_fusion::glossary::Glossary;
use mc_fusion::text::{fold_words, tokenize};
use serde::{Deserialize, Serialize};

/// One utterance of the reference, as a person verified it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RefUtterance {
    #[serde(default)]
    pub id: Option<String>,
    pub speaker: String,
    /// How Teams labels this person when it cannot name them (guests).
    #[serde(default, alias = "teams_speaker")]
    pub label: Option<String>,
    pub start_ms: f64,
    pub end_ms: f64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Reference {
    pub utterances: Vec<RefUtterance>,
    #[serde(default)]
    pub glossary: Vec<String>,
}

impl Reference {
    /// `{"utterances": [...], "glossary": [...]}`; a generated `truth.json` is one.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let reference: Self = serde_json::from_str(text).map_err(|e| e.to_string())?;
        for (n, u) in reference.utterances.iter().enumerate() {
            if !(u.start_ms.is_finite() && u.end_ms.is_finite() && u.end_ms >= u.start_ms) {
                return Err(format!("utterance {}: bad times", n + 1));
            }
        }
        Ok(reference)
    }
}

/// What a variant says, in the same shape whatever produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct Hyp {
    pub start_ms: f64,
    pub end_ms: f64,
    /// Empty when the source cannot say who spoke.
    pub speaker: String,
    pub text: String,
}

impl From<&FusedSegment> for Hyp {
    fn from(s: &FusedSegment) -> Self {
        Self {
            start_ms: s.start_ms,
            end_ms: s.end_ms,
            speaker: s.speaker.clone(),
            text: s.text.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Metrics {
    pub ref_utterances: usize,
    pub hyp_segments: usize,
    pub matched_utterances: usize,
    /// Segments that overlap no reference utterance (invented or leaked speech).
    pub unmatched_segments: usize,
    pub ref_words: usize,
    pub substitutions: usize,
    pub deletions: usize,
    pub insertions: usize,
    /// Word errors counted with accents (the sum, for `wer_with_accents`).
    pub errors_with_accents: usize,
    /// (substitutions + deletions + insertions) / reference words, comparing
    /// words without case, accents or punctuation.
    pub wer: Option<f64>,
    /// The same keeping accents ("esta" and "está" are different words).
    pub wer_with_accents: Option<f64>,
    pub term_ref: usize,
    pub term_hyp: usize,
    pub term_hit: usize,
    pub term_recall: Option<f64>,
    pub term_precision: Option<f64>,
    pub speaker_checked: usize,
    pub speaker_correct: usize,
    pub speaker_accuracy: Option<f64>,
    /// Median |reference start - segment start| over matched utterances.
    pub start_error_ms: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct EvalOptions {
    /// A segment belongs to a reference utterance when they overlap at least
    /// this share of the shorter one.
    pub min_overlap: f64,
}

impl Default for EvalOptions {
    fn default() -> Self {
        Self { min_overlap: 0.3 }
    }
}

/// Word edits turning `reference` into `hypothesis`: (substitutions, deletions, insertions).
pub fn edits(reference: &[String], hypothesis: &[String]) -> (usize, usize, usize) {
    let (n, m) = (reference.len(), hypothesis.len());
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let sub = d[i - 1][j - 1] + usize::from(reference[i - 1] != hypothesis[j - 1]);
            d[i][j] = sub.min(d[i - 1][j] + 1).min(d[i][j - 1] + 1);
        }
    }
    // Walk back to count each kind; prefer a substitution/match, then a deletion.
    let (mut i, mut j) = (n, m);
    let (mut s, mut del, mut ins) = (0, 0, 0);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 {
            let cost = usize::from(reference[i - 1] != hypothesis[j - 1]);
            if d[i][j] == d[i - 1][j - 1] + cost {
                s += cost;
                i -= 1;
                j -= 1;
                continue;
            }
        }
        if i > 0 && d[i][j] == d[i - 1][j] + 1 {
            del += 1;
            i -= 1;
        } else {
            ins += 1;
            j -= 1;
        }
    }
    (s, del, ins)
}

fn words(text: &str) -> Vec<String> {
    fold_words(text)
        .split(' ')
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Like [`words`] but keeping accents: only case and punctuation are ignored.
fn words_exact(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// A point in time (start == end) still covers a moment.
fn span(start: f64, end: f64) -> (f64, f64) {
    if end > start {
        (start, end)
    } else {
        (start, start + 1.0)
    }
}

fn overlap(a: (f64, f64), b: (f64, f64)) -> f64 {
    a.1.min(b.1) - a.0.max(b.0)
}

fn ratio(numerator: usize, denominator: usize) -> Option<f64> {
    (denominator > 0).then(|| numerator as f64 / denominator as f64)
}

/// Term counts of a token list, one slot per glossary term.
fn term_counts(glossary: &Glossary, size: usize, text: &str) -> Vec<usize> {
    let mut counts = vec![0; size];
    let keys: Vec<String> = tokenize(text).into_iter().map(|w| w.key).collect();
    for found in glossary.find(&keys) {
        counts[found.term] += 1;
    }
    counts
}

fn same_speaker(hyp: &str, reference: &RefUtterance) -> bool {
    let norm = |s: &str| normalize_speaker(Some(s)).0.to_lowercase();
    let label = reference.label.as_deref().unwrap_or(&reference.speaker);
    norm(hyp) == norm(label) || norm(hyp) == norm(&reference.speaker)
}

pub fn evaluate(reference: &Reference, hyps: &[Hyp], opts: &EvalOptions) -> Metrics {
    let glossary = Glossary::new(&reference.glossary);
    let term_slots = reference.glossary.len().max(1);

    // Each segment goes to the reference utterance it overlaps most.
    let mut owner: Vec<Option<usize>> = vec![None; hyps.len()];
    for (h, hyp) in hyps.iter().enumerate() {
        let mut best: Option<(f64, usize)> = None;
        for (r, u) in reference.utterances.iter().enumerate() {
            let (h_span, r_span) = (span(hyp.start_ms, hyp.end_ms), span(u.start_ms, u.end_ms));
            let shared = overlap(h_span, r_span);
            let shorter = (h_span.1 - h_span.0).min(r_span.1 - r_span.0).max(1.0);
            if shared > 0.0
                && shared >= opts.min_overlap * shorter
                && best.is_none_or(|(b, _)| shared > b)
            {
                best = Some((shared, r));
            }
        }
        owner[h] = best.map(|b| b.1);
    }

    let mut m = Metrics {
        ref_utterances: reference.utterances.len(),
        hyp_segments: hyps.len(),
        ..Metrics::default()
    };
    let mut start_errors = Vec::new();
    for (r, u) in reference.utterances.iter().enumerate() {
        let mut mine: Vec<&Hyp> = hyps
            .iter()
            .enumerate()
            .filter(|(h, _)| owner[*h] == Some(r))
            .map(|(_, h)| h)
            .collect();
        mine.sort_by(|a, b| a.start_ms.total_cmp(&b.start_ms));
        let ref_words = words(&u.text);
        let hyp_text = mine
            .iter()
            .map(|h| h.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let (s, d, i) = edits(&ref_words, &words(&hyp_text));
        m.ref_words += ref_words.len();
        (m.substitutions, m.deletions, m.insertions) =
            (m.substitutions + s, m.deletions + d, m.insertions + i);
        let (xs, xd, xi) = edits(&words_exact(&u.text), &words_exact(&hyp_text));
        m.errors_with_accents += xs + xd + xi;

        let ref_terms = term_counts(&glossary, term_slots, &u.text);
        let hyp_terms = term_counts(&glossary, term_slots, &hyp_text);
        m.term_ref += ref_terms.iter().sum::<usize>();
        m.term_hyp += hyp_terms.iter().sum::<usize>();
        m.term_hit += ref_terms
            .iter()
            .zip(&hyp_terms)
            .map(|(a, b)| *a.min(b))
            .sum::<usize>();

        if let Some(first) = mine.first() {
            m.matched_utterances += 1;
            start_errors.push((first.start_ms - u.start_ms).abs());
            // The segment that covers most of the utterance names the speaker.
            let lead = mine
                .iter()
                .max_by(|a, b| {
                    let r = span(u.start_ms, u.end_ms);
                    overlap(span(a.start_ms, a.end_ms), r)
                        .total_cmp(&overlap(span(b.start_ms, b.end_ms), r))
                })
                .copied()
                .unwrap_or(first);
            // A variant that cannot name speakers (raw ASR) is not scored on it.
            if !lead.speaker.is_empty() {
                m.speaker_checked += 1;
                m.speaker_correct += usize::from(same_speaker(&lead.speaker, u));
            }
        }
    }
    // Speech that matches nothing in the reference counts against precision.
    for (h, hyp) in hyps.iter().enumerate().filter(|(h, _)| owner[*h].is_none()) {
        let _ = h;
        m.unmatched_segments += 1;
        m.insertions += words(&hyp.text).len();
        m.errors_with_accents += words_exact(&hyp.text).len();
        m.term_hyp += term_counts(&glossary, term_slots, &hyp.text)
            .iter()
            .sum::<usize>();
    }

    m.wer = ratio(m.substitutions + m.deletions + m.insertions, m.ref_words);
    m.wer_with_accents = ratio(m.errors_with_accents, m.ref_words);
    m.term_recall = ratio(m.term_hit, m.term_ref);
    m.term_precision = ratio(m.term_hit, m.term_hyp);
    m.speaker_accuracy = ratio(m.speaker_correct, m.speaker_checked);
    if !start_errors.is_empty() {
        start_errors.sort_by(f64::total_cmp);
        let n = start_errors.len();
        m.start_error_ms = Some(if n % 2 == 1 {
            start_errors[n / 2]
        } else {
            (start_errors[n / 2 - 1] + start_errors[n / 2]) / 2.0
        });
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    fn utt(speaker: &str, start: f64, end: f64, text: &str) -> RefUtterance {
        RefUtterance {
            id: None,
            speaker: speaker.into(),
            label: None,
            start_ms: start,
            end_ms: end,
            text: text.into(),
        }
    }

    fn hyp(speaker: &str, start: f64, end: f64, text: &str) -> Hyp {
        Hyp {
            start_ms: start,
            end_ms: end,
            speaker: speaker.into(),
            text: text.into(),
        }
    }

    #[test]
    fn edit_counts_by_kind() {
        assert_eq!(edits(&w("a b c d"), &w("a b c d")), (0, 0, 0));
        assert_eq!(edits(&w("a b c d"), &w("a x c d")), (1, 0, 0));
        assert_eq!(edits(&w("a b c d"), &w("a b d")), (0, 1, 0));
        assert_eq!(edits(&w("a b d"), &w("a b c d")), (0, 0, 1));
        assert_eq!(edits(&[], &w("x y")), (0, 0, 2));
        assert_eq!(edits(&w("x y"), &[]), (0, 2, 0));
    }

    #[test]
    fn a_perfect_transcript_scores_perfectly() {
        let reference = Reference {
            utterances: vec![
                utt("Ana", 0.0, 2000.0, "Hice el deploy ayer"),
                utt("Luis", 3000.0, 5000.0, "Perfecto"),
            ],
            glossary: vec!["deploy".into()],
        };
        let hyps = vec![
            hyp("Ana", 100.0, 2100.0, "Hice el deploy ayer."),
            hyp("Luis", 3200.0, 5000.0, "perfecto"),
        ];
        let m = evaluate(&reference, &hyps, &EvalOptions::default());
        assert_eq!(
            (m.wer, m.term_recall, m.term_precision, m.speaker_accuracy),
            (Some(0.0), Some(1.0), Some(1.0), Some(1.0))
        );
        assert_eq!(m.matched_utterances, 2);
        assert_eq!(m.start_error_ms, Some(150.0));
    }

    #[test]
    fn errors_are_counted_where_they_are() {
        let reference = Reference {
            utterances: vec![
                utt("Ana", 0.0, 2000.0, "Hice el deploy ayer"),
                utt("Luis", 3000.0, 5000.0, "Todo bien"),
            ],
            glossary: vec!["deploy".into(), "push".into()],
        };
        let hyps = vec![
            hyp("Ana", 0.0, 2000.0, "Hice el de ploy ayer"), // term wrong, one extra word
            hyp("Marta", 3000.0, 5000.0, "Todo bien"),       // right words, wrong person
            hyp("Ana", 8000.0, 9000.0, "Gracias por ver el push"), // invented
        ];
        let m = evaluate(&reference, &hyps, &EvalOptions::default());
        assert_eq!((m.substitutions, m.deletions, m.insertions), (1, 0, 1 + 5));
        assert_eq!(m.ref_words, 6);
        assert_eq!(m.term_recall, Some(0.0));
        assert_eq!(
            m.term_precision,
            Some(0.0),
            "the invented push is a false positive"
        );
        assert_eq!(m.unmatched_segments, 1);
        assert_eq!(m.speaker_accuracy, Some(0.5));
    }

    #[test]
    fn a_segment_covering_two_utterances_goes_to_the_one_it_overlaps_most() {
        let reference = Reference {
            utterances: vec![
                utt("Ana", 0.0, 2000.0, "uno dos tres"),
                utt("Luis", 2200.0, 3000.0, "cuatro"),
            ],
            glossary: vec![],
        };
        let hyps = vec![hyp("Ana", 0.0, 2900.0, "uno dos tres cuatro")];
        let m = evaluate(&reference, &hyps, &EvalOptions::default());
        assert_eq!(m.matched_utterances, 1);
        assert_eq!(m.deletions, 1);
        assert_eq!(
            m.insertions, 1,
            "the fourth word is extra in the first utterance"
        );
    }

    #[test]
    fn anonymous_labels_are_one_speaker_and_guests_use_their_label() {
        let mut guest = utt("Invitado 1", 0.0, 1000.0, "hola");
        guest.label = Some("Usuario desconocido".into());
        let reference = Reference {
            utterances: vec![guest],
            glossary: vec![],
        };
        let hyps = vec![hyp("Invitado sin nombre", 0.0, 1000.0, "hola")];
        let m = evaluate(&reference, &hyps, &EvalOptions::default());
        assert_eq!(m.speaker_accuracy, Some(1.0));
    }

    #[test]
    fn a_variant_without_speakers_is_not_scored_on_them() {
        let reference = Reference {
            utterances: vec![utt("Ana", 0.0, 1000.0, "hola")],
            glossary: vec![],
        };
        let m = evaluate(
            &reference,
            &[hyp("", 0.0, 1000.0, "hola")],
            &EvalOptions::default(),
        );
        assert_eq!(
            (m.speaker_checked, m.speaker_accuracy, m.wer),
            (0, None, Some(0.0))
        );
    }

    #[test]
    fn accents_count_only_in_the_strict_wer() {
        let reference = Reference {
            utterances: vec![utt("Ana", 0.0, 1000.0, "ya está listo")],
            glossary: vec![],
        };
        let m = evaluate(
            &reference,
            &[hyp("Ana", 0.0, 1000.0, "ya esta listo")],
            &EvalOptions::default(),
        );
        assert_eq!((m.wer, m.wer_with_accents), (Some(0.0), Some(1.0 / 3.0)));
    }

    #[test]
    fn point_segments_still_match() {
        let reference = Reference {
            utterances: vec![utt("Ana", 500.0, 500.0, "hola")],
            glossary: vec![],
        };
        let m = evaluate(
            &reference,
            &[hyp("Ana", 500.0, 500.0, "hola")],
            &EvalOptions::default(),
        );
        assert_eq!(
            (m.matched_utterances, m.wer, m.unmatched_segments),
            (1, Some(0.0), 0)
        );
        let inside = evaluate(
            &Reference {
                utterances: vec![utt("Ana", 0.0, 1000.0, "hola")],
                glossary: vec![],
            },
            &[hyp("Ana", 400.0, 400.0, "hola")],
            &EvalOptions { min_overlap: 0.3 },
        );
        assert_eq!(inside.matched_utterances, 1);
    }

    #[test]
    fn empty_inputs_give_no_numbers_instead_of_nan() {
        let reference = Reference {
            utterances: vec![],
            glossary: vec![],
        };
        let m = evaluate(&reference, &[], &EvalOptions::default());
        assert_eq!(
            (
                m.wer,
                m.term_recall,
                m.term_precision,
                m.speaker_accuracy,
                m.start_error_ms
            ),
            (None, None, None, None, None)
        );
    }

    #[test]
    fn references_load_from_json_and_reject_bad_times() {
        let ok = r#"{"utterances":[{"speaker":"Ana","teams_speaker":"Ana","start_ms":0,"end_ms":10,"text":"hola","extra":1}],"glossary":["push"]}"#;
        let r = Reference::from_json(ok).unwrap();
        assert_eq!(r.glossary, ["push"]);
        assert_eq!(r.utterances[0].label.as_deref(), Some("Ana"));
        let bad = r#"{"utterances":[{"speaker":"Ana","start_ms":10,"end_ms":5,"text":"x"}]}"#;
        assert!(Reference::from_json(bad).is_err());
        assert!(Reference::from_json("nope").is_err());
    }
}
