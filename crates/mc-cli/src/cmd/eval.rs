use mc_eval::{EvalOptions, Hyp, Metrics, Reference, evaluate};
use mc_fusion::fuse;
use serde_json::{Value, json};

use crate::cli::{CliError, Ctx, Io, emit};

use super::session_fusion::{fusion_config, load};

const VARIANTS: [&str; 3] = ["captions", "asr", "fused"];
const USAGE: &str = "usage: eli5 eval <dir> --reference FILE [--asr ID] [--variants captions,asr,fused] [--me NAME] [--glossary FILE]";

fn pct(value: Option<f64>) -> String {
    value.map_or("-".to_string(), |v| format!("{:.1}%", v * 100.0))
}

pub fn run(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    let [dir] = ctx.args.positional.as_slice() else {
        return Err(CliError::usage(USAGE));
    };
    let reference_path = ctx
        .args
        .value("reference")
        .ok_or_else(|| CliError::usage(USAGE))?;
    let wanted: Vec<&str> = match ctx.args.value("variants") {
        None => VARIANTS.to_vec(),
        Some(list) => {
            let items: Vec<&str> = list.split(',').map(str::trim).collect();
            if items.is_empty() || items.iter().any(|v| !VARIANTS.contains(v)) {
                return Err(CliError::usage(
                    "--variants takes captions, asr and/or fused",
                ));
            }
            items
        }
    };
    let text = std::fs::read_to_string(reference_path).map_err(|e| {
        CliError::not_found(
            format!("{reference_path}: {e}"),
            "pass the reference transcript",
        )
    })?;
    let mut reference = Reference::from_json(&text)
        .map_err(|e| CliError::usage(format!("{reference_path}: {e}")))?;

    let inputs = load(ctx, dir)?;
    // Fusion works with the user's own glossary; the reference decides what is
    // scored, so the two are kept apart (a term only the reference knows is
    // not fixed, only counted).
    let config = fusion_config(ctx)?;
    if reference.glossary.is_empty() {
        reference.glossary.clone_from(&config.glossary);
    }
    let fused = fuse(&inputs.captions, &inputs.asr, &config);
    let lag = fused.report.lag.median_ms.unwrap_or(config.default_lag_ms);

    let opts = EvalOptions::default();
    let mut results: Vec<(&str, Metrics)> = Vec::new();
    for variant in wanted {
        let hyps: Vec<Hyp> = match variant {
            "fused" => fused.segments.iter().map(Hyp::from).collect(),
            "captions" => inputs
                .captions
                .iter()
                .filter_map(|c| {
                    let first = c.first_mono_ms.filter(|t| t.is_finite())?;
                    let last = c.last_mono_ms.filter(|t| t.is_finite()).unwrap_or(first);
                    // Captions appear late: put them back where the speech was,
                    // by the lag around that moment (it drifts).
                    let behind = fused.lag.at(first);
                    Some(Hyp {
                        start_ms: first - behind,
                        end_ms: last.max(first) - behind,
                        speaker: c.speaker.clone(),
                        text: c.text.clone(),
                    })
                })
                .collect(),
            _ => inputs
                .asr
                .iter()
                .map(|a| Hyp {
                    start_ms: a.start_ms,
                    end_ms: a.end_ms,
                    speaker: String::new(),
                    text: a.text.clone(),
                })
                .collect(),
        };
        results.push((variant, evaluate(&reference, &hyps, &opts)));
    }

    let mut human = format!(
        "reference {reference_path}: {} utterances, {} words; {} scored terms, {} used for fusion; ASR run {}; median caption lag {:.0} ms\n",
        reference.utterances.len(),
        results.first().map_or(0, |r| r.1.ref_words),
        reference.glossary.len(),
        config.glossary.len(),
        inputs.asr_id,
        lag
    );
    human.push_str(&format!(
        "{:<9} {:>7} {:>7} {:>5} {:>5} {:>5} {:>8} {:>8} {:>8} {:>7} {:>8}\n",
        "variant",
        "WER",
        "WER+acc",
        "sub",
        "del",
        "ins",
        "term R",
        "term P",
        "speaker",
        "start",
        "unmatched"
    ));
    for (name, m) in &results {
        human.push_str(&format!(
            "{:<9} {:>7} {:>7} {:>5} {:>5} {:>5} {:>8} {:>8} {:>8} {:>7} {:>8}\n",
            name,
            pct(m.wer),
            pct(m.wer_with_accents),
            m.substitutions,
            m.deletions,
            m.insertions,
            pct(m.term_recall),
            pct(m.term_precision),
            pct(m.speaker_accuracy),
            m.start_error_ms
                .map_or("-".to_string(), |v| format!("{v:.0}ms")),
            m.unmatched_segments,
        ));
    }
    let variants: serde_json::Map<String, Value> = results
        .iter()
        .map(|(name, m)| ((*name).to_string(), json!(m)))
        .collect();
    let value = json!({
        "reference": reference_path,
        "asr_id": inputs.asr_id,
        "lag_ms": lag,
        "variants": variants,
    });
    // Nothing to score (an empty reference, or no segment fell on any utterance).
    let scored = results
        .iter()
        .all(|(_, m)| m.wer.is_some() && m.matched_utterances > 0);
    emit(io, ctx.json, human, value)?;
    Ok(u8::from(!scored))
}
