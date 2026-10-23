//! `mc tui`: the capture in a full-screen terminal UI.

use std::path::{Path, PathBuf};

use mc_engine::controller::start;
use mc_engine::runs::execute;
use mc_tui::{Actions, App, TistyTarget};

use super::analysis::candidates;
use super::capture::{self, prepare};
use super::export::save_md;
use super::export_tisty::{self, Prepared};
use crate::cli::{CliError, Ctx, Io, open_store};

/// What one analysis produced; later saves reuse it instead of re-running.
struct Analysed {
    markdown: String,
    session_id: String,
    run_id: String,
}

/// The folder typed in the UI: no `~` expansion (like the config), relative
/// paths against the working directory the CLI was given.
fn resolve_folder(input: &Path, cwd: &Path) -> Result<PathBuf, String> {
    if input.to_string_lossy().starts_with('~') {
        return Err(format!(
            "{}: `~` is not expanded; type the full path",
            input.display()
        ));
    }
    Ok(if input.is_absolute() {
        input.to_path_buf()
    } else {
        cwd.join(input)
    })
}

/// What the end menu does: the analysis runs once and every save reuses it.
struct Saver<'a> {
    ctx: &'a Ctx<'a>,
    analysed: Option<Analysed>,
    /// The plan shown for confirmation, published as is.
    prepared: Option<Prepared>,
}

impl Saver<'_> {
    fn analyse(&mut self, session_dir: &Path) -> Result<&Analysed, String> {
        if self.analysed.is_none() {
            let mut store = open_store(&self.ctx.cfg).map_err(|e| e.message().to_string())?;
            let outcome = execute(session_dir, &self.ctx.cfg.run_config(), Some(&mut store))
                .map_err(|e| e.to_string())?;
            let markdown = std::fs::read_to_string(outcome.path.join("transcript.md"))
                .map_err(|e| e.to_string())?;
            self.analysed = Some(Analysed {
                markdown,
                session_id: outcome.report["session_id"]
                    .as_str()
                    .unwrap_or("session")
                    .to_string(),
                run_id: outcome.run_id,
            });
        }
        self.analysed
            .as_ref()
            .ok_or_else(|| "analysis missing".to_string())
    }
}

impl Actions for Saver<'_> {
    fn save_md(&mut self, session_dir: &Path, folder: &Path) -> Result<PathBuf, String> {
        let folder = resolve_folder(folder, &self.ctx.env.cwd)?;
        let done = self.analyse(session_dir)?;
        save_md(&done.markdown, &done.session_id, &done.run_id, &folder)
            .map_err(|e| e.message().to_string())
    }

    fn tisty_preview(
        &mut self,
        session_dir: &Path,
        target: &TistyTarget,
    ) -> Result<String, String> {
        // The minutes read a transcript; a session just captured has none until it is run.
        if !has_transcript(session_dir) {
            self.analyse(session_dir)?;
        }
        let target = export_tisty::target(self.ctx, &target.folder, &target.list);
        let prepared = export_tisty::prepare(self.ctx, session_dir, &target, &mut std::io::sink())
            .map_err(|e| e.message().to_string())?;
        let preview = prepared.plan.preview();
        self.prepared = Some(prepared);
        Ok(preview)
    }

    fn tisty_publish(
        &mut self,
        session_dir: &Path,
        _target: &TistyTarget,
    ) -> Result<String, String> {
        let prepared = self.prepared.take().ok_or("there is no plan to publish")?;
        let report = export_tisty::publish(self.ctx, &prepared);
        let retry = format!("mc export {} --tisty", session_dir.display());
        let text = format!(
            "{}{}",
            report.human(),
            export_tisty::summary(&report, &retry)
        );
        if report.failed() { Err(text) } else { Ok(text) }
    }
}

fn has_transcript(session_dir: &Path) -> bool {
    !candidates(&session_dir.join("fused"), "fused.jsonl").is_empty()
        || !candidates(&session_dir.join("runs"), "segments.jsonl").is_empty()
}

pub fn run(ctx: Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    if !(ctx.env.stdin_tty && ctx.env.stdout_tty) || ctx.json {
        writeln!(
            io.err,
            "the TUI needs an interactive terminal; falling back to plain capture"
        )?;
        return capture::run(ctx, io);
    }
    // Consent is asked with a plain prompt, before the screen changes.
    let prepared = prepare(&ctx, io)?;
    let consent = prepared
        .spec
        .record
        .as_ref()
        .map(|r| r.consent_method.to_string());
    let analyst = prepared.analyst;
    let capture = start(prepared.spec, &prepared.corpus_root)?;
    let app = App::new(
        prepared.recording,
        consent,
        ctx.cfg.output_dir.clone(),
        (80, 24),
    )
    .with_analyst(analyst)
    .with_tisty_default(
        &ctx.cfg.tisty.folder,
        ctx.cfg.tisty.list.as_deref().unwrap_or(""),
    )
    .with_tisty_configured(ctx.cfg.tisty.configured);

    let mut saver = Saver {
        ctx: &ctx,
        analysed: None,
        prepared: None,
    };
    let outcome = mc_tui::run(app, capture, &mut saver)?;
    if let Some(summary) = &outcome.summary
        && let Some(dir) = &summary.session_dir
    {
        writeln!(io.out, "session saved in {}", dir.display())?;
    }
    if let Some(failure) = &outcome.failure {
        writeln!(io.err, "mc tui: {failure}")?;
        return Ok(3);
    }
    Ok(match &outcome.summary {
        Some(s) if !s.reason.is_clean() => 3,
        _ => 0,
    })
}
