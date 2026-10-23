//! Scoring the three variants (captions, ASR, fused) on every generated meeting.
//! `cargo test -p mc-eval --test corpora -- --nocapture` prints the table.

use std::path::{Path, PathBuf};

use mc_asr::read_asr_jsonl;
use mc_engine::runs::{RunConfig, build};
use mc_eval::{EvalOptions, Hyp, Metrics, Reference, evaluate};
use mc_fusion::{FusionConfig, fuse};
use mc_reconcile::{Policy, ReconcilerConfig};

const NAMES: [&str; 14] = [
    "fusion_basic",
    "fusion_lag_drift",
    "fusion_overlap",
    "fusion_missing_captions",
    "fusion_spanglish",
    "fusion_hallucination",
    "fusion_anonymous",
    "fusion_asr_missing",
    "fusion_revised",
    "fusion_monologue",
    "fusion_interjections",
    "fusion_english",
    "fusion_leak",
    "fusion_resegmented",
];

fn dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/fusion")
        .join(name)
}

struct Scores {
    captions: Metrics,
    asr: Metrics,
    fused: Metrics,
}

fn score(name: &str) -> Scores {
    let path = dir(name);
    let reference =
        Reference::from_json(&std::fs::read_to_string(path.join("truth.json")).unwrap()).unwrap();
    let cfg = RunConfig {
        reconciler: ReconcilerConfig {
            policy: Policy::V2,
            ..ReconcilerConfig::default()
        },
        ..RunConfig::default()
    };
    let captions = build(&path, &cfg).unwrap().segments;
    let asr = read_asr_jsonl(&path.join("asr_fixture.jsonl")).unwrap();
    let config = FusionConfig {
        me: Some(
            reference
                .utterances
                .iter()
                .find(|u| u.id.as_deref() == Some("u1"))
                .map(|_| "Ana Pérez".to_string())
                .unwrap(),
        ),
        glossary: reference.glossary.clone(),
        ..FusionConfig::default()
    };
    let result = fuse(&captions, &asr, &config);
    let opts = EvalOptions::default();
    let cap_hyps: Vec<Hyp> = captions
        .iter()
        .filter_map(|c| {
            let first = c.first_mono_ms?;
            let lag = result.lag.at(first);
            Some(Hyp {
                start_ms: first - lag,
                end_ms: c.last_mono_ms.unwrap_or(first) - lag,
                speaker: c.speaker.clone(),
                text: c.text.clone(),
            })
        })
        .collect();
    let asr_hyps: Vec<Hyp> = asr
        .iter()
        .map(|a| Hyp {
            start_ms: a.start_ms,
            end_ms: a.end_ms,
            speaker: String::new(),
            text: a.text.clone(),
        })
        .collect();
    let fused_hyps: Vec<Hyp> = result.segments.iter().map(Hyp::from).collect();
    Scores {
        captions: evaluate(&reference, &cap_hyps, &opts),
        asr: evaluate(&reference, &asr_hyps, &opts),
        fused: evaluate(&reference, &fused_hyps, &opts),
    }
}

fn pct(v: Option<f64>) -> String {
    v.map_or("   -  ".into(), |v| format!("{:5.1}%", v * 100.0))
}

#[test]
fn every_meeting_is_scored_and_the_table_is_sensible() {
    let mut totals = [(0usize, 0usize); 3]; // (errors, words) per variant
    println!(
        "{:<26} {:>7} {:>7} {:>7}   term R: cap/asr/fused   speaker: cap/fused",
        "meeting", "cap", "asr", "fused"
    );
    for name in NAMES {
        let s = score(name);
        println!(
            "{name:<26} {} {} {}   {} {} {}   {} {}",
            pct(s.captions.wer),
            pct(s.asr.wer),
            pct(s.fused.wer),
            pct(s.captions.term_recall),
            pct(s.asr.term_recall),
            pct(s.fused.term_recall),
            pct(s.captions.speaker_accuracy),
            pct(s.fused.speaker_accuracy),
        );
        for (slot, m) in totals.iter_mut().zip([&s.captions, &s.asr, &s.fused]) {
            slot.0 += m.substitutions + m.deletions + m.insertions;
            slot.1 += m.ref_words;
        }
        // Fusing never makes the words clearly worse than the captions alone,
        // except where the captions are the only witness to nothing at all.
        assert!(
            s.fused.wer.unwrap() <= s.captions.wer.unwrap() + 0.03,
            "{name}: fused {:?} vs captions {:?}",
            s.fused.wer,
            s.captions.wer
        );
        // Speakers come from Teams, so what the captions name the fusion names too;
        // speech Teams never captioned gets a guessed name, which can miss.
        let fused_speakers = s.fused.speaker_accuracy.unwrap();
        // (Short interjections between two remote speakers are the weak spot: the
        // channel says "remote", only the neighbours say who.)
        assert!(fused_speakers >= 0.7, "{name}: speakers {fused_speakers}");
        if let Some(c) = s.captions.speaker_accuracy {
            assert!(c >= 0.9, "{name}: the captions' own speakers {c}");
        }
    }
    let rate = |t: (usize, usize)| t.0 as f64 / t.1 as f64;
    let (captions, asr, fused) = (rate(totals[0]), rate(totals[1]), rate(totals[2]));
    println!("overall WER: captions {captions:.3}, asr {asr:.3}, fused {fused:.3}");
    assert!(
        fused < captions * 0.5,
        "fusion must at least halve the caption errors: {fused} vs {captions}"
    );
    assert!(
        fused < asr,
        "and beat the ASR alone, which has no punctuation-free accents: {fused} vs {asr}"
    );
}
