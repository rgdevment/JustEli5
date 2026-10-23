//! Fusion over the generated meetings (`fixtures/fusion/*`): the captions go
//! through the V2 reconciler, the ASR comes from `asr_fixture.jsonl`, and the
//! result is scored against `truth.json`. Nothing under `fixtures/` is written.

use std::path::{Path, PathBuf};

use mc_asr::{AsrSegment, read_asr_jsonl};
use mc_core::Segment;
use mc_engine::runs::{RunConfig, build};
use mc_fusion::fuse::fuse;
use mc_fusion::text::fold_words;
use mc_fusion::{FusedSegment, FusionConfig, FusionResult, Source};
use mc_reconcile::{Policy, ReconcilerConfig};
use serde_json::Value;

fn dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/fusion")
        .join(name)
}

struct Meeting {
    truth: Value,
    captions: Vec<Segment>,
    asr: Vec<AsrSegment>,
    cfg: FusionConfig,
}

fn load(name: &str, with_me: bool) -> Meeting {
    let path = dir(name);
    let truth: Value =
        serde_json::from_str(&std::fs::read_to_string(path.join("truth.json")).unwrap()).unwrap();
    let cfg = RunConfig {
        reconciler: ReconcilerConfig {
            policy: Policy::V2,
            ..ReconcilerConfig::default()
        },
        ..RunConfig::default()
    };
    let captions = build(&path, &cfg).unwrap().segments;
    let asr = read_asr_jsonl(&path.join("asr_fixture.jsonl")).unwrap();
    let glossary = std::fs::read_to_string(path.join("glossary.txt"))
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    Meeting {
        cfg: FusionConfig {
            me: with_me.then(|| truth["me"].as_str().unwrap().to_string()),
            glossary,
            ..FusionConfig::default()
        },
        truth,
        captions,
        asr,
    }
}

impl Meeting {
    fn fuse(&self) -> FusionResult {
        fuse(&self.captions, &self.asr, &self.cfg)
    }

    fn utterances(&self) -> Vec<&Value> {
        self.truth["utterances"]
            .as_array()
            .unwrap()
            .iter()
            .collect()
    }
}

/// Word error rate of `hyp` against `reference` (words folded), as edits / words.
fn wer(reference: &str, hyp: &str) -> (usize, usize) {
    let (r, h): (Vec<String>, Vec<String>) = (
        fold_words(reference)
            .split(' ')
            .map(str::to_string)
            .collect(),
        fold_words(hyp).split(' ').map(str::to_string).collect(),
    );
    let mut d = vec![vec![0usize; h.len() + 1]; r.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=r.len() {
        for j in 1..=h.len() {
            let sub = d[i - 1][j - 1] + usize::from(r[i - 1] != h[j - 1]);
            d[i][j] = sub.min(d[i - 1][j] + 1).min(d[i][j - 1] + 1);
        }
    }
    (d[r.len()][h.len()], r.len())
}

/// The fused segment that best covers a truth utterance (by time overlap and speaker channel).
fn find<'a>(fused: &'a [FusedSegment], u: &Value) -> Option<&'a FusedSegment> {
    let (start, end) = (
        u["start_ms"].as_f64().unwrap(),
        u["end_ms"].as_f64().unwrap(),
    );
    let ch = u["ch"].as_str().unwrap();
    fused
        .iter()
        .filter(|f| f.ch == ch)
        .map(|f| (f, f.end_ms.min(end) - f.start_ms.max(start)))
        .filter(|(f, overlap)| *overlap > 0.4 * (end - start).min(f.end_ms - f.start_ms).max(1.0))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(f, _)| f)
}

/// Total word errors of the transcript against the truth: (fused, captions only).
fn score(m: &Meeting) -> (usize, usize, usize) {
    let result = m.fuse();
    let (mut fused_errors, mut caption_errors, mut words) = (0, 0, 0);
    for u in m.utterances() {
        let text = u["text"].as_str().unwrap();
        let fused = find(&result.segments, u).map_or("", |f| f.text.as_str());
        let caption = u["teams"].as_str().unwrap_or("");
        let (fe, n) = wer(text, fused);
        fused_errors += fe;
        caption_errors += wer(text, caption).0;
        words += n;
    }
    (fused_errors, caption_errors, words)
}

#[test]
fn fusing_beats_the_captions_alone_on_the_meetings_with_english_terms() {
    // The resegmented meeting folds two spoken interjections into the phrase
    // before them (the ASR cut them together), which costs those two words.
    for (name, tenths) in [
        ("fusion_basic", 6),
        ("fusion_spanglish", 6),
        ("fusion_revised", 6),
        ("fusion_resegmented", 7),
    ] {
        let m = load(name, true);
        let (fused, captions, words) = score(&m);
        assert!(
            fused * 10 <= captions * tenths,
            "{name}: fused {fused} vs captions {captions} errors in {words} words"
        );
    }
}

#[test]
fn the_lag_is_estimated_and_follows_the_drift() {
    let basic = load("fusion_basic", true).fuse();
    let median = basic.report.lag.median_ms.unwrap();
    assert!((median - 1200.0).abs() <= 400.0, "{median}");
    assert!(basic.report.lag.samples >= 8);

    let drift = load("fusion_lag_drift", true).fuse();
    let (min, max) = (
        drift.report.lag.min_ms.unwrap(),
        drift.report.lag.max_ms.unwrap(),
    );
    assert!(min < 1300.0 && max > 2000.0, "{min}..{max}");
}

#[test]
fn speakers_always_come_from_teams_and_channels_match_the_truth() {
    for name in [
        "fusion_basic",
        "fusion_overlap",
        "fusion_spanglish",
        "fusion_anonymous",
    ] {
        let m = load(name, true);
        let result = m.fuse();
        let (mut checked, mut total) = (0, 0);
        for u in m.utterances() {
            total += 1;
            let Some(f) = find(&result.segments, u) else {
                continue;
            };
            checked += 1;
            // The reconciler already turns every anonymous label into one.
            let shown = match u["teams_speaker"].as_str().unwrap() {
                "Usuario desconocido" | "Unknown User" => "Invitado sin nombre",
                other => other,
            };
            assert_eq!(f.speaker, shown, "{name} {}", u["id"]);
            // The channel is decided from the speaker, not borrowed from the audio.
            assert_eq!(
                result.report.speaker_channels[shown],
                u["ch"].as_str().unwrap(),
                "{name} {shown}"
            );
        }
        assert!(
            checked * 10 >= total * 8,
            "{name}: only {checked} of {total} utterances found"
        );
    }
}

#[test]
fn the_user_is_found_from_the_audio_when_not_configured() {
    let m = load("fusion_basic", false);
    let result = m.fuse();
    assert_eq!(result.report.me.as_deref(), m.truth["me"].as_str());
    assert_eq!(result.report.speaker_channels["Luis"], "system");
}

#[test]
fn glossary_terms_are_restored_and_each_fix_is_recorded() {
    let m = load("fusion_spanglish", true);
    let result = m.fuse();
    let fixes: Vec<_> = result
        .segments
        .iter()
        .flat_map(|s| s.edits.iter())
        .filter(|e| e.applied && e.reason == "glossary")
        .collect();
    assert!(fixes.len() >= 8, "{}", fixes.len());
    let glossary = &m.cfg.glossary;
    assert!(
        // Every fix is a whole glossary term, written as the user listed it
        // (a capital at the start of a sentence aside).
        fixes
            .iter()
            .all(|e| glossary.iter().any(|t| t.eq_ignore_ascii_case(&e.to))),
        "{fixes:?}"
    );
    assert!(fixes.iter().all(|e| e.from != e.to));
    assert!(result.report.counts.edits_applied >= fixes.len());
    // The corrected text really contains the term, not the mangling.
    for fix in &fixes {
        assert!(
            result.segments.iter().any(|s| s.text.contains(&fix.to)),
            "{fix:?}"
        );
    }
}

#[test]
fn an_asr_that_misses_phrases_never_loses_the_captions() {
    let m = load("fusion_asr_missing", true);
    let result = m.fuse();
    for u in m.utterances().into_iter().filter(|u| u["asr"].is_null()) {
        let f = find(&result.segments, u).unwrap_or_else(|| panic!("lost {}", u["id"]));
        assert_eq!(f.source, Source::Captions);
        assert!(f.asr_text.is_none());
    }
}

#[test]
fn the_asr_fills_what_teams_never_showed() {
    // An outage with the ASR running.
    let m = load("fusion_missing_captions", true);
    let result = m.fuse();
    let outage = m.truth["outages_ms"][0].as_array().unwrap();
    let (low, high) = (outage[0].as_f64().unwrap(), outage[1].as_f64().unwrap());
    let mut filled = 0;
    for u in m.utterances().into_iter().filter(|u| u["teams"].is_null()) {
        let f = find(&result.segments, u).unwrap_or_else(|| panic!("hole at {}", u["id"]));
        assert_eq!(f.source, Source::Asr, "{}", u["id"]);
        assert!(f.start_ms >= low - 3_000.0 && f.start_ms <= high + 1.0);
        let (errors, words) = wer(u["text"].as_str().unwrap(), &f.text);
        assert!(
            errors * 3 <= words,
            "{}: {} vs {}",
            u["id"],
            f.text,
            u["text"]
        );
        filled += 1;
    }
    assert!(filled >= 2);
    assert_eq!(result.report.counts.asr_only, filled);
    // Lines Teams only half-showed get their end back.
    for u in m
        .utterances()
        .into_iter()
        .filter(|u| u["teams_cut"] == true)
    {
        let f = find(&result.segments, u).unwrap();
        let (fused_errors, _) = wer(u["text"].as_str().unwrap(), &f.text);
        let (caption_errors, _) = wer(u["text"].as_str().unwrap(), u["teams"].as_str().unwrap());
        assert!(fused_errors <= caption_errors, "{}", u["id"]);
    }
}

#[test]
fn hallucinated_phrases_in_silence_never_reach_the_transcript() {
    let m = load("fusion_hallucination", true);
    let result = m.fuse();
    let truth_count = m.utterances().len();
    assert_eq!(result.segments.len(), truth_count, "{:#?}", result.segments);
    for s in &result.segments {
        assert_ne!(s.source, Source::Asr, "{s:?}");
        for stock in ["Amara", "Suscr", "Thanks for watching", "Gracias por ver"] {
            assert!(!s.text.contains(stock), "{}", s.text);
        }
    }
    assert!(result.report.counts.dropped_junk + result.report.counts.dropped_low_confidence >= 5);
}

#[test]
fn the_meeting_leaking_into_the_mic_is_not_attributed_to_the_user() {
    let m = load("fusion_leak", true);
    let result = m.fuse();
    let leaked: Vec<&str> = m.truth["leaked_utterances"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(!leaked.is_empty());
    let mine = result.segments.iter().filter(|s| s.ch == "mic").count();
    let truth_mine = m.utterances().iter().filter(|u| u["ch"] == "mic").count();
    assert_eq!(
        mine,
        truth_mine,
        "{:#?}",
        result
            .segments
            .iter()
            .filter(|s| s.ch == "mic")
            .collect::<Vec<_>>()
    );
    assert!(result.report.counts.dropped_leak >= leaked.len() / 2);
}

#[test]
fn interjections_teams_never_captions_come_from_the_asr_with_a_guessed_speaker() {
    let m = load("fusion_interjections", true);
    let result = m.fuse();
    let short: Vec<&Value> = m
        .utterances()
        .into_iter()
        .filter(|u| u["teams"].is_null())
        .collect();
    assert!(short.len() >= 5);
    let found = short
        .iter()
        .filter(|u| find(&result.segments, u).is_some_and(|f| f.source == Source::Asr))
        .count();
    assert!(found * 10 >= short.len() * 7, "{found} of {}", short.len());
    for s in result
        .segments
        .iter()
        .filter(|s| s.source == Source::Asr && s.ch == "system")
    {
        assert!(s.speaker_inferred);
    }
}

#[test]
fn guests_under_one_label_keep_the_label_teams_gave_them() {
    let m = load("fusion_anonymous", true);
    let result = m.fuse();
    assert!(
        result
            .segments
            .iter()
            .any(|s| s.speaker == "Invitado sin nombre")
    );
}

#[test]
fn a_monologue_longer_than_the_engine_window_still_fuses() {
    let m = load("fusion_monologue", true);
    let result = m.fuse();
    let (fused, captions, _) = score(&m);
    assert!(fused <= captions, "{fused} > {captions}");
    assert!(result.segments.len() >= 3);
}

#[test]
fn english_meetings_are_left_alone() {
    let m = load("fusion_english", true);
    let result = m.fuse();
    let (fused, _captions, words) = score(&m);
    assert!(fused * 10 <= words, "{fused} errors in {words} words");
    assert_eq!(
        result.report.counts.edits_applied,
        0,
        "{:#?}",
        result
            .segments
            .iter()
            .flat_map(|s| &s.edits)
            .collect::<Vec<_>>()
    );
}

#[test]
fn fusion_is_deterministic() {
    let m = load("fusion_basic", true);
    let a = mc_fusion::render::render_jsonl(&m.fuse().segments);
    let b = mc_fusion::render::render_jsonl(&m.fuse().segments);
    assert_eq!(a, b);
    assert!(a.lines().count() >= 10);
}
