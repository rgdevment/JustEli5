//! `mc export <session> --tisty`: publish the newest minutes and their commitments to Tisty.

use std::io::Write;
use std::path::{Path, PathBuf};

use std::collections::BTreeMap;

use mc_analyst::CommitmentRecord;
use mc_store::series_for_name;
use serde_json::json;

use super::analysis::{candidates, read_json};
use super::minutes;
use crate::args::Parsed;
use crate::cli::{CliError, Ctx, Io, emit, open_store, resolve_one};
use crate::config::Config;
use crate::publish::tisty::{TistyPublisher, resolve};
use crate::publish::{
    Abort, AbortKind, ItemKind, Plan, PublishReport, Publisher, Status, Target, build_plan,
    parse_properties,
};

pub struct Prepared {
    pub plan: Plan,
    pub minutes_dir: PathBuf,
    /// The meeting's series, to remember which Tisty document holds it.
    pub series_key: Option<String>,
}

/// Series key → Tisty document id: the document is found by id even after it is renamed.
const SERIES_DOCS_FILE: &str = "tisty_series.json";

fn series_docs(cfg: &Config) -> BTreeMap<String, String> {
    std::fs::read_to_string(cfg.data_dir.join(SERIES_DOCS_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Keeps the id for next time; failing to write it only costs a lookup by title then.
fn remember_series_doc(cfg: &Config, key: &str, doc: &str) {
    let mut docs = series_docs(cfg);
    if docs.get(key).map(String::as_str) == Some(doc) {
        return;
    }
    docs.insert(key.to_string(), doc.to_string());
    if let Ok(text) = serde_json::to_string_pretty(&docs) {
        let _ = std::fs::write(cfg.data_dir.join(SERIES_DOCS_FILE), text + "\n");
    }
}

/// The session folder for an id or prefix of the index, or a folder given as is.
fn session_dir(ctx: &Ctx<'_>, selector: &str) -> Result<PathBuf, CliError> {
    let direct = PathBuf::from(crate::cli::plain_path(selector));
    if direct.join("manifest.json").is_file() {
        return Ok(direct);
    }
    let store = open_store(&ctx.cfg)?;
    let id = resolve_one("session", selector, store.resolve_session(selector)?)?;
    store
        .list_sessions()?
        .into_iter()
        .find(|s| s.session_id == id)
        .map(|s| PathBuf::from(s.dir))
        .ok_or_else(|| CliError::not_found(format!("session {id} is not indexed"), "mc status"))
}

/// Complete minutes folders are the ones that hold both files; the newest wins.
fn latest_minutes(session_dir: &Path) -> Option<PathBuf> {
    let parent = session_dir.join("minutes");
    let mut folders = candidates(&parent, "minutes.md");
    folders.retain(|name| parent.join(name).join("commitments.json").is_file());
    folders.pop().map(|name| parent.join(name))
}

/// The newest minutes of the session; made first (as `mc minutes` would) when there are none.
fn ensure_minutes(
    ctx: &Ctx<'_>,
    session_dir: &Path,
    err: &mut dyn Write,
) -> Result<PathBuf, CliError> {
    if let Some(found) = latest_minutes(session_dir) {
        return Ok(found);
    }
    let inner = Ctx {
        cfg: ctx.cfg.clone(),
        args: Parsed {
            positional: vec![session_dir.display().to_string()],
            ..Parsed::default()
        },
        json: false,
        env: ctx.env,
    };
    let mut discarded = Vec::new();
    let mut io = Io {
        out: &mut discarded,
        err,
        input: None,
    };
    minutes::run(&inner, &mut io)?;
    latest_minutes(session_dir).ok_or_else(|| {
        CliError::runtime(
            "minutes",
            "`mc minutes` finished but left no minutes folder",
        )
    })
}

pub fn target(ctx: &Ctx<'_>, folder: &str, list: &str) -> Target {
    Target {
        folder: folder.trim().to_string(),
        list: Some(list.trim().to_string()).filter(|l| !l.is_empty()),
        tags: ctx.cfg.tisty.tags.clone(),
        me: ctx.cfg.fusion.me.clone(),
    }
}

pub fn prepare(
    ctx: &Ctx<'_>,
    session_dir: &Path,
    target: &Target,
    err: &mut dyn Write,
) -> Result<Prepared, CliError> {
    if target.folder.is_empty() {
        return Err(CliError::usage("the Tisty folder must not be empty"));
    }
    let minutes_dir = ensure_minutes(ctx, session_dir, err)?;
    let path = minutes_dir.join("minutes.md");
    let markdown = std::fs::read_to_string(&path)
        .map_err(|e| CliError::runtime("io", format!("{}: {e}", path.display())))?;
    let commitments: Vec<CommitmentRecord> = read_json(&minutes_dir.join("commitments.json"))?;
    let mut plan = build_plan(&markdown, &commitments, target);
    let (props, _) = parse_properties(&markdown);
    let series_key = series_for_name(&props.title, &ctx.cfg.series_rules).map(|(key, _)| key);
    plan.minutes.series_doc = series_key
        .as_ref()
        .and_then(|key| series_docs(&ctx.cfg).get(key).cloned());
    // Picture paths in the minutes are relative to their folder; Tisty needs the file itself.
    for (_, path) in &mut plan.minutes.images {
        *path = minutes_dir.join(&*path).display().to_string();
    }
    Ok(Prepared {
        plan,
        minutes_dir,
        series_key,
    })
}

/// Sends the plan to Tisty. Everything that goes wrong is in the report; the files stay on disk.
pub fn publish(ctx: &Ctx<'_>, prepared: &Prepared) -> PublishReport {
    let plan = &prepared.plan;
    let settings = &ctx.cfg.tisty;
    match resolve(ctx.env, &settings.command) {
        Some(program) => {
            let report = TistyPublisher {
                program,
                args: settings.args.clone(),
            }
            .publish(plan);
            if let (Some(key), Some(doc)) = (&prepared.series_key, &report.series_doc) {
                remember_series_doc(&ctx.cfg, key, doc);
            }
            report
        }
        None => PublishReport::default().stopped_by(
            plan,
            Abort {
                kind: AbortKind::Unavailable,
                message: format!(
                    "Tisty was not found (`{}`); install it or set tisty.command in the config",
                    settings.command
                ),
            },
        ),
    }
}

/// The one-line outcome plus, when something failed, how to try again.
pub fn summary(report: &PublishReport, retry: &str) -> String {
    let tasks = |what: fn(&Status) -> bool| report.count(ItemKind::Task, what);
    let mut text = format!(
        "tasks: {} created, {} already existed, {} failed\n",
        tasks(|s| *s == Status::Created),
        tasks(|s| *s == Status::Existed),
        tasks(|s| matches!(s, Status::Failed(_))),
    );
    if report.failed() {
        text.push_str(&format!(
            "nothing was lost: the minutes and commitments are still on disk. To try again: {retry}\n"
        ));
        if let Some(Abort {
            kind: AbortKind::NotAdmitted,
            ..
        }) = &report.aborted
        {
            text.push_str(&format!("{}\n", crate::publish::tisty::NOT_ADMITTED_HINT));
        }
    }
    text
}

fn confirmed(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<bool, CliError> {
    if ctx.args.flag("yes") || ctx.json || !ctx.cfg.tisty.ask {
        return Ok(true);
    }
    if ctx.env.stdin_tty
        && let Some(input) = io.input.as_mut()
    {
        write!(io.err, "Publish this to Tisty? [s/N] ")?;
        io.err.flush()?;
        let mut answer = String::new();
        input.read_line(&mut answer)?;
        return Ok(matches!(
            answer.trim().to_lowercase().as_str(),
            "s" | "si" | "sí" | "y" | "yes"
        ));
    }
    Err(CliError::usage(
        "publishing asks for confirmation: pass --yes (or --dry-run to see the plan only)",
    ))
}

pub fn run(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    let [selector] = ctx.args.positional.as_slice() else {
        return Err(CliError::usage(
            "usage: eli5 export <session> --tisty [--tisty-folder F] [--tisty-list L] [--yes] [--dry-run]",
        ));
    };
    if ["format", "out", "run"]
        .iter()
        .any(|o| ctx.args.value(o).is_some())
        || ctx.args.flag("save")
        || ctx.args.flag("force")
    {
        return Err(CliError::usage(
            "--tisty cannot be combined with --format, --out, --save, --force or --run",
        ));
    }
    let settings = &ctx.cfg.tisty;
    let folder = ctx.args.value("tisty-folder").unwrap_or(&settings.folder);
    let list = ctx
        .args
        .value("tisty-list")
        .or(settings.list.as_deref())
        .unwrap_or("");
    let session_dir = session_dir(ctx, selector)?;
    let target = target(ctx, folder, list);
    let prepared = prepare(ctx, &session_dir, &target, io.err)?;
    let retry = format!("mc export {selector} --tisty");
    let minutes_dir = prepared.minutes_dir.display().to_string();

    if ctx.args.flag("dry-run") {
        let human = format!("{}(dry run: nothing was sent)\n", prepared.plan.preview());
        let value = json!({
            "dry_run": true, "minutes_dir": minutes_dir,
            "plan": prepared.plan.to_json(),
        });
        emit(io, ctx.json, human, value)?;
        return Ok(0);
    }
    if !ctx.json {
        write!(io.out, "{}", prepared.plan.preview())?;
    }
    if !confirmed(ctx, io)? {
        return Err(CliError::usage("publishing declined; nothing was sent"));
    }
    let report = publish(ctx, &prepared);
    let human = format!("{}{}", report.human(), summary(&report, &retry));
    let mut value = report.to_json();
    value["minutes_dir"] = json!(minutes_dir);
    if report.failed() {
        value["retry"] = json!(retry);
    }
    emit(io, ctx.json, human, value)?;
    Ok(if report.failed() { 3 } else { 0 })
}
