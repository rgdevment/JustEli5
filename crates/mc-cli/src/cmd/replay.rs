use mc_engine::replay::{Divergence, verify};
use serde_json::{Value, json};

use crate::cli::{CliError, Ctx, Io, emit};

const MAX_SHOWN: usize = 5;

fn show(out: &mut String, label: &str, divergences: &[Divergence]) {
    for d in divergences.iter().take(MAX_SHOWN) {
        out.push_str(&format!(
            "  {label} #{} {}: expected {:?}, got {:?}\n",
            d.index, d.field, d.expected, d.actual
        ));
    }
    if divergences.len() > MAX_SHOWN {
        out.push_str(&format!(
            "  ... {} more {label} divergences\n",
            divergences.len() - MAX_SHOWN
        ));
    }
}

fn to_json(list: &[Divergence]) -> Vec<Value> {
    list.iter()
        .take(MAX_SHOWN)
        .map(|d| json!({"index": d.index, "field": d.field, "expected": d.expected, "actual": d.actual}))
        .collect()
}

pub fn run(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    let strict = ctx.args.flag("strict");
    let [dir] = ctx.args.positional.as_slice() else {
        return Err(CliError::usage(
            "usage: eli5 replay <corpus-dir> --verify [--strict]",
        ));
    };
    if !ctx.args.flag("verify") {
        return Err(CliError::usage("mc replay: only --verify is implemented"));
    }
    let report = verify(dir, strict)?;
    let mut human = format!(
        "{dir}: {} frames, {} records, transcript {}, {} timing / {} content divergences{}\n",
        report.frames,
        report.records,
        if report.transcript_ok {
            "ok"
        } else {
            "DIFFERS"
        },
        report.timing.len(),
        report.content.len(),
        if report.truncated_tail {
            " (truncated tail)"
        } else {
            ""
        },
    );
    if report.frames == 0 {
        human.push_str("  warning: the corpus has no frames, nothing was verified\n");
    }
    if report.transcript_from_records {
        human.push_str("  warning: no expected_transcript.txt, compared with committed records\n");
    }
    show(&mut human, "timing", &report.timing);
    show(&mut human, "content", &report.content);
    let value = json!({
        "dir": dir,
        "frames": report.frames,
        "records": report.records,
        "transcript_ok": report.transcript_ok,
        "transcript_from_records": report.transcript_from_records,
        "truncated_tail": report.truncated_tail,
        "timing": report.timing.len(),
        "content": report.content.len(),
        "first_timing": to_json(&report.timing),
        "first_content": to_json(&report.content),
        "open_lines": report.open_lines,
        "passed": report.passed,
    });
    emit(io, ctx.json, human, value)?;
    Ok(if report.passed { 0 } else { 1 })
}
