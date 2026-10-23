use serde_json::json;

use crate::cli::{CliError, Ctx, Io, emit};

use super::session_fusion::{describe, fuse_session};

pub fn run(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    let [dir] = ctx.args.positional.as_slice() else {
        return Err(CliError::usage(
            "usage: eli5 fuse <dir> [--asr ID] [--me NAME] [--glossary FILE] [--force]",
        ));
    };
    let fused = fuse_session(ctx, dir, ctx.args.flag("force"))?;
    let value = json!({
        "fuse_id": fused.fuse_id,
        "path": fused.dir.display().to_string(),
        "reused": fused.reused,
        "asr_id": fused.asr_id,
        "report": fused.report,
    });
    emit(io, ctx.json, describe(&fused), value)?;
    Ok(0)
}
