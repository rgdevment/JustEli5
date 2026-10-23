use serde_json::json;

use crate::cli::{CliError, Ctx, Io, emit};

use std::path::Path;

use super::analysis::{analyze_transcript, resolve_transcript};
use super::carry::load as load_memory;

pub fn run(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    let [dir] = ctx.args.positional.as_slice() else {
        return Err(CliError::usage(
            "usage: eli5 analyze <session> [--run ID | --fuse ID] [--llm-provider P] [--llm-model M] [--glossary F] [--context F] [--force]",
        ));
    };
    let transcript = resolve_transcript(ctx, dir)?;
    let memory = load_memory(ctx, Path::new(dir))?;
    let analyzed = analyze_transcript(ctx, dir, &transcript, ctx.args.flag("force"), &memory)?;
    let accepted: usize = analyzed.events.iter().map(|e| e.accepted).sum();
    let rejected: usize = analyzed.events.iter().map(|e| e.rejected).sum();
    let counts = analyzed.summary();
    let value = json!({
        "analysis_id": analyzed.id,
        "path": analyzed.dir.display().to_string(),
        "reused": analyzed.reused,
        "source": {"kind": transcript.kind.name(), "id": transcript.id},
        "windows": analyzed.events.len(),
        "failed_windows": analyzed.failed(),
        "complete": analyzed.complete,
        "accepted": accepted,
        "rejected": rejected,
        "counts": counts,
        "warnings": memory.carry.warnings,
    });
    let human = format!(
        "{}  {}  {}\n  from {}; {} windows ({} failed); {accepted} accepted, {rejected} rejected\n  {} topics, {} decisions, {} commitments, {} open questions, {} risks, {} term fixes, {} glossary candidates\n",
        analyzed.id,
        if analyzed.reused {
            "unchanged"
        } else {
            "created"
        },
        analyzed.dir.display(),
        transcript.label(),
        analyzed.events.len(),
        analyzed.failed(),
        counts["topics"],
        counts["decisions"],
        counts["commitments"],
        counts["open_questions"],
        counts["risks"],
        counts["term_fixes"],
        counts["glossary_candidates"],
    );
    let mut human = human;
    if !analyzed.complete {
        human.push_str("  warning: incomplete, some windows failed; the next run redoes it (see ledger_events.jsonl)\n");
    }
    for warning in &memory.carry.warnings {
        human.push_str(&format!(
            "  warning: {warning}
"
        ));
    }
    emit(io, ctx.json, human, value)?;
    Ok(0)
}
