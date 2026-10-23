//! `mc detect`: which meeting platforms have a window open now and whether their
//! captions are on. The machine-readable summary of `mc doctor --uia`.

use mc_captions::ui::Platform;
use mc_captions::ui::report::{PickReport, uia_report};
use serde_json::{Value, json};

use crate::cli::{CliError, Ctx, Io, emit};

use super::doctor::ui_backend;

/// Enough for a full Teams window; detection never dumps the tree.
const MAX_NODES: usize = 20_000;

fn platform_json(pick: &PickReport) -> Value {
    let app = Platform::parse(pick.platform).map_or(pick.platform, Platform::app);
    json!({
        "platform": pick.platform,
        "app": app,
        "captions_on": pick.container_path.is_some(),
        "captions": pick.captions,
        "error": pick.error,
    })
}

fn platform_line(pick: &PickReport) -> String {
    let captions = match (&pick.error, pick.container_path.is_some()) {
        (Some(error), _) => format!("could not read the window: {error}"),
        (None, true) => format!("captions on ({} visible)", pick.captions),
        (None, false) => "captions not found".to_string(),
    };
    format!("{:<10} {captions}\n", pick.platform)
}

pub fn run(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    if !ctx.args.positional.is_empty() {
        return Err(CliError::usage("usage: eli5 detect [--json]"));
    }
    let mut backend = ui_backend(ctx)?;
    let started = std::time::Instant::now();
    let mut timer = move || started.elapsed().as_secs_f64() * 1000.0;
    let report = uia_report(
        backend.as_mut(),
        &ctx.cfg.capture.selectors,
        MAX_NODES,
        &mut timer,
    )
    .map_err(|e| CliError::runtime("io", e.to_string()))?;
    let mut human: String = report.picks.iter().map(platform_line).collect();
    if report.picks.is_empty() {
        human.push_str("no meeting window found\n");
    }
    let value = json!({
        "platforms": report.picks.iter().map(platform_json).collect::<Vec<_>>(),
        "advice": report.advice,
    });
    emit(io, ctx.json, human, value)?;
    Ok(u8::from(report.picks.is_empty()))
}
