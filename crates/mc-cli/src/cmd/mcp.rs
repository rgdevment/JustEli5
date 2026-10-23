use mc_mcp_server::SeriesIndex;

use crate::cli::{CliError, Ctx, Io};

/// Serves MCP on stdin/stdout until the client closes it; diagnostics go to stderr only.
pub fn run(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    if !ctx.args.positional.is_empty() {
        return Err(CliError::usage("usage: eli5 mcp"));
    }
    let stdin = std::io::stdin();
    let series = SeriesIndex {
        path: ctx.cfg.index_path(),
        rules: ctx.cfg.series_rules.clone(),
    };
    mc_mcp_server::serve(&ctx.cfg.corpus_dir(), series, stdin.lock(), io.out)?;
    Ok(0)
}
