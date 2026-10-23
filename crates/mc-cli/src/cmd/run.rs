use serde_json::json;

use mc_engine::runs::{RunAction, execute_with};

use crate::cli::{CliError, Ctx, Io, emit, open_store};

use super::session_fusion::{describe, fuse_session};

pub fn run(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    let [dir] = ctx.args.positional.as_slice() else {
        return Err(CliError::usage(
            "usage: eli5 run <dir> [--force] [--flush auto|manifest-only] [--debounce S]",
        ));
    };
    if !std::path::Path::new(dir).join("manifest.json").is_file() {
        return Err(CliError::not_found(
            format!("{dir}: no manifest.json"),
            "pass a session directory",
        ));
    }
    if !ctx.args.flag("fuse") {
        for option in ["asr", "me", "glossary"] {
            if ctx.args.value(option).is_some() {
                return Err(CliError::usage(format!("--{option} needs --fuse")));
            }
        }
    }
    let mut store = open_store(&ctx.cfg)?;
    let outcome = execute_with(
        dir,
        &ctx.cfg.run_config(),
        Some(&mut store),
        ctx.args.flag("force"),
    )?;
    let action = match outcome.action {
        RunAction::Created => "created",
        RunAction::Unchanged => "unchanged",
    };
    let div = &outcome.report["divergences"];
    let mut human = format!(
        "{}  {action}  {}\n  {} segments, {} content / {} timing divergences\n",
        outcome.run_id,
        outcome.path.display(),
        outcome.report["segments"],
        div["content"],
        div["timing"],
    );
    if outcome.report["synthetic_flush"] == json!(true) {
        human.push_str("  note: the session did not end cleanly; the last lines were flushed\n");
    }
    if outcome.tail_pending_bytes > 0 {
        human.push_str(&format!(
            "  warning: {} bytes of an unfinished last line were not included\n",
            outcome.tail_pending_bytes
        ));
    }
    let fused = if ctx.args.flag("fuse") {
        let fused = fuse_session(ctx, dir, ctx.args.flag("force"))?;
        human.push_str(&describe(&fused));
        Some(fused)
    } else {
        None
    };
    let mut value = json!({
        "run_id": outcome.run_id,
        "path": outcome.path.display().to_string(),
        "action": action,
        "tail_pending_bytes": outcome.tail_pending_bytes,
        "report": outcome.report,
    });
    if let Some(fused) = fused {
        value["fusion"] = json!({
            "fuse_id": fused.fuse_id,
            "path": fused.dir.display().to_string(),
            "reused": fused.reused,
            "asr_id": fused.asr_id,
            "report": fused.report,
        });
    }
    emit(io, ctx.json, human, value)?;
    Ok(0)
}
