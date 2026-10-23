//! Weeding out what engines invent on silence and noise.
//!
//! Whisper-style models hallucinate on quiet audio: stock phrases from their
//! training subtitles, endless repetition, a lone "thank you". Better to lose a
//! doubtful phrase than to put words in someone's mouth, so these drop.

use crate::transcriber::{Dropped, RawSegment};

#[derive(Debug, Clone, PartialEq)]
pub struct FilterConfig {
    /// Segments the engine itself thinks are not speech ...
    pub no_speech_max: f64,
    /// ... and also unsure of (mean token log-probability below this).
    pub min_logprob: f64,
    /// A phrase made of this few distinct words (of at least `loop_min_words`) is a loop.
    pub loop_unique_ratio: f64,
    pub loop_min_words: usize,
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self {
            no_speech_max: 0.6,
            min_logprob: -1.0,
            loop_unique_ratio: 0.25,
            loop_min_words: 12,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Keep,
    Drop(&'static str),
}

/// Lower case, letters and digits only, single spaces.
fn normal(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_lowercase().collect::<String>()
            } else {
                " ".to_string()
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Stock lines that show up on silence, whatever the audio.
const PHANTOMS: [&str; 14] = [
    "subtitulos realizados por la comunidad de amara org",
    "subtitulos por la comunidad de amara org",
    "subtítulos realizados por la comunidad de amara org",
    "subtitles by the amara org community",
    "amara org",
    "gracias por ver el video",
    "gracias por ver",
    "gracias por vernos",
    "suscribete",
    "suscríbete",
    "thanks for watching",
    "thank you for watching",
    "please subscribe",
    "like and subscribe",
];

/// Fillers that are real speech sometimes but, alone on a doubtful segment, are
/// the classic hallucination.
const FILLERS: [&str; 8] = [
    "you",
    "thank you",
    "thanks",
    "gracias",
    "bye",
    "adios",
    "adiós",
    "ok",
];

/// Doubtful means the engine gave real weight to "no speech". The engine
/// measures that once per 30 s window, so a short utterance (mostly padding)
/// often sits at 0.3-0.5 even when real: the filler must also be a shaky read.
const DOUBTFUL_NO_SPEECH: f64 = 0.5;

pub fn judge(segment: &RawSegment, cfg: &FilterConfig) -> Verdict {
    let text = normal(&segment.text);
    if text.is_empty() {
        return Verdict::Drop("empty");
    }
    if PHANTOMS.iter().any(|p| normal(p) == text) {
        return Verdict::Drop("stock phrase");
    }
    let no_speech = segment.no_speech_prob.unwrap_or(0.0);
    if no_speech >= cfg.no_speech_max && segment.avg_logprob.is_some_and(|lp| lp < cfg.min_logprob)
    {
        return Verdict::Drop("no speech");
    }
    let shaky = segment
        .avg_logprob
        .is_none_or(|lp| lp < cfg.min_logprob / 2.0);
    if no_speech >= DOUBTFUL_NO_SPEECH && shaky && FILLERS.contains(&text.as_str()) {
        return Verdict::Drop("lone filler on doubtful audio");
    }
    let words: Vec<&str> = text.split(' ').collect();
    if words.len() >= cfg.loop_min_words {
        let unique: std::collections::HashSet<&&str> = words.iter().collect();
        if (unique.len() as f64) / (words.len() as f64) <= cfg.loop_unique_ratio {
            return Verdict::Drop("repetition loop");
        }
    }
    Verdict::Keep
}

/// Keeps what passes; the rest comes back, with its times and reason, for the log.
pub fn apply(segments: Vec<RawSegment>, cfg: &FilterConfig) -> (Vec<RawSegment>, Vec<Dropped>) {
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for segment in segments {
        match judge(&segment, cfg) {
            Verdict::Keep => kept.push(segment),
            Verdict::Drop(reason) => dropped.push(Dropped { segment, reason }),
        }
    }
    (kept, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(text: &str, no_speech: Option<f64>, logprob: Option<f64>) -> RawSegment {
        RawSegment {
            no_speech_prob: no_speech,
            avg_logprob: logprob,
            ..RawSegment::text(0.0, 1000.0, text)
        }
    }

    fn verdict(text: &str, no_speech: Option<f64>, logprob: Option<f64>) -> Verdict {
        judge(&seg(text, no_speech, logprob), &FilterConfig::default())
    }

    #[test]
    fn normal_speech_is_kept() {
        assert_eq!(
            verdict("Vamos a hacer el deploy mañana.", Some(0.02), Some(-0.3)),
            Verdict::Keep
        );
        assert_eq!(
            verdict("Gracias, Ana, por el resumen", None, None),
            Verdict::Keep
        );
        assert_eq!(verdict("Thank you", Some(0.01), Some(-0.2)), Verdict::Keep);
    }

    #[test]
    fn empty_and_punctuation_only_are_dropped() {
        for text in ["", "   ", "...", "¿?"] {
            assert_eq!(
                verdict(text, None, None),
                Verdict::Drop("empty"),
                "{text:?}"
            );
        }
    }

    #[test]
    fn stock_subtitle_lines_are_dropped_with_any_punctuation_and_case() {
        for text in [
            "Subtítulos realizados por la comunidad de Amara.org",
            "¡Suscríbete!",
            "Thanks for watching!",
            "Gracias por ver el video.",
        ] {
            assert_eq!(
                verdict(text, Some(0.0), Some(0.0)),
                Verdict::Drop("stock phrase"),
                "{text:?}"
            );
        }
    }

    #[test]
    fn unsure_non_speech_is_dropped_but_confident_low_probability_speech_stays() {
        assert_eq!(
            verdict("mm hmm sí claro", Some(0.8), Some(-1.4)),
            Verdict::Drop("no speech")
        );
        assert_eq!(
            verdict("mm hmm sí claro", Some(0.8), Some(-0.4)),
            Verdict::Keep
        );
        assert_eq!(
            verdict("mm hmm sí claro", Some(0.1), Some(-1.4)),
            Verdict::Keep
        );
    }

    #[test]
    fn a_lone_filler_goes_only_when_the_audio_was_doubtful() {
        assert!(matches!(
            verdict("Gracias.", Some(0.6), Some(-0.7)),
            Verdict::Drop(_)
        ));
        assert!(matches!(verdict("you", Some(0.55), None), Verdict::Drop(_)));
        assert!(matches!(
            verdict("Adiós.", Some(0.7), Some(-0.9)),
            Verdict::Drop(_)
        ));
        // A short real "Gracias." has a middling no-speech value but a sure read.
        assert_eq!(verdict("Gracias.", Some(0.4), Some(-0.2)), Verdict::Keep);
        assert_eq!(verdict("Gracias.", Some(0.7), Some(-0.2)), Verdict::Keep);
        assert_eq!(verdict("Gracias.", Some(0.05), Some(-0.5)), Verdict::Keep);
    }

    #[test]
    fn repetition_loops_are_dropped_and_short_repeats_are_not() {
        let looped = "no no no no no no no no no no no no no no no no";
        assert_eq!(
            verdict(looped, None, None),
            Verdict::Drop("repetition loop")
        );
        assert_eq!(
            verdict("no, no, no, claro que no", None, None),
            Verdict::Keep
        );
    }

    #[test]
    fn apply_splits_kept_and_dropped_with_reasons() {
        let (kept, dropped) = apply(
            vec![
                seg("hola equipo", None, None),
                seg("Suscríbete", None, None),
                RawSegment::text(2000.0, 3000.0, ""),
            ],
            &FilterConfig::default(),
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(
            dropped.iter().map(|d| d.reason).collect::<Vec<_>>(),
            ["stock phrase", "empty"]
        );
        assert_eq!(dropped[1].segment.start_ms, 2000.0);
    }
}
