use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::cli::{CliError, Ctx, Io, emit, open_store, resolve_one};

/// File-name safe prefix: only `[A-Za-z0-9_-]`, at most `len` characters.
pub(crate) fn short(id: &str, len: usize) -> String {
    let clean: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(len)
        .collect();
    if clean.is_empty() {
        "x".to_string()
    } else {
        clean
    }
}

/// Where a run's files are: the indexed path, else the session's own `runs/`
/// directory (the corpus may have moved and been re-imported since).
fn locate_run_dir(
    store: &mc_store::Store,
    session: &str,
    row: &mc_store::RunRow,
) -> Result<PathBuf, CliError> {
    let mut candidates = vec![PathBuf::from(&row.dir)];
    if let Some(summary) = store
        .list_sessions()?
        .into_iter()
        .find(|s| s.session_id == session)
    {
        candidates.push(Path::new(&summary.dir).join("runs").join(&row.run_id));
    }
    for dir in &candidates {
        let belongs = std::fs::read_to_string(dir.join("report.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .is_some_and(|report| report["run_id"] == row.run_id.as_str());
        if belongs {
            return Ok(dir.clone());
        }
    }
    Err(CliError::not_found(
        format!("the files of run {} are missing", row.run_id),
        "`mc run <dir> --force` recreates them",
    ))
}

/// Writes `content` to `target`. Without `force` the file must not exist
/// (and a failed write removes the partial file); with `force` it is replaced
/// atomically, so a symlink at the target is replaced rather than followed.
pub(crate) fn write_target(target: &Path, content: &str, force: bool) -> Result<(), CliError> {
    if force {
        let tmp = target.with_file_name(format!(
            ".{}.tmp-{}",
            target
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("export"),
            std::process::id()
        ));
        let result = (|| -> std::io::Result<()> {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&tmp, target)
        })();
        if let Err(e) = result {
            let _ = std::fs::remove_file(&tmp);
            return Err(CliError::runtime(
                "io",
                format!("{}: {e}", target.display()),
            ));
        }
        return Ok(());
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                CliError::Runtime {
                    code: "exists",
                    message: format!("{} already exists", target.display()),
                    hint: Some("use --force to overwrite".to_string()),
                }
            } else {
                CliError::runtime("io", format!("{}: {e}", target.display()))
            }
        })?;
    let result = file
        .write_all(content.as_bytes())
        .and_then(|()| file.sync_all());
    if let Err(e) = result {
        drop(file);
        let _ = std::fs::remove_file(target);
        return Err(CliError::runtime(
            "io",
            format!("{}: {e}", target.display()),
        ));
    }
    Ok(())
}

pub fn run(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    if ctx.args.flag("tisty") {
        return super::export_tisty::run(ctx, io);
    }
    if ["tisty-folder", "tisty-list"]
        .iter()
        .any(|o| ctx.args.value(o).is_some())
        || ctx.args.flag("yes")
        || ctx.args.flag("dry-run")
    {
        return Err(CliError::usage(
            "--tisty-folder, --tisty-list, --yes and --dry-run need --tisty",
        ));
    }
    let [prefix] = ctx.args.positional.as_slice() else {
        return Err(CliError::usage(
            "usage: eli5 export <session> --format txt|md|json [--out PATH|DIR|-] [--save] [--force] [--run ID]",
        ));
    };
    let format = ctx
        .args
        .value("format")
        .ok_or_else(|| CliError::usage("--format txt|md|json is required"))?;
    let extension = match format {
        "txt" | "md" | "json" => format,
        other => return Err(CliError::usage(format!("unknown format {other:?}"))),
    };
    if ctx.args.flag("save") && ctx.args.value("out").is_some() {
        return Err(CliError::usage("--save and --out are exclusive"));
    }

    let mut store = open_store(&ctx.cfg)?;
    let session = match crate::cli::session_id_of_dir(prefix) {
        Some(id) => {
            // A folder given as is may not be indexed yet.
            let dir = crate::cli::plain_path(prefix);
            if store.resolve_session(&id)?.is_empty() {
                store.import_session(std::path::Path::new(&dir))?;
            }
            id
        }
        None => resolve_one("session", prefix, store.resolve_session(prefix)?)?,
    };
    let run_id = match ctx.args.value("run") {
        Some(run_prefix) => resolve_one(
            "run",
            run_prefix,
            store.resolve_run_in(&session, run_prefix)?,
        )?,
        None => store.latest_run(&session)?.ok_or_else(|| {
            CliError::not_found(format!("session {session} has no run"), "mc run <dir>")
        })?,
    };
    let row = store
        .run(&run_id)?
        .ok_or_else(|| CliError::not_found(format!("run {run_id} is not indexed"), "mc status"))?;
    if row.session_id != session {
        return Err(CliError::usage(format!(
            "run {run_id} belongs to session {}, not {session}",
            row.session_id
        )));
    }
    let run_dir = locate_run_dir(&store, &session, &row)?;
    let read = |name: &str| -> Result<String, CliError> {
        std::fs::read_to_string(run_dir.join(name)).map_err(|e| {
            CliError::not_found(
                format!("{}: {e}", run_dir.join(name).display()),
                "`mc run <dir> --force` recreates the run files",
            )
        })
    };
    let content = match format {
        "txt" => read("transcript.txt")?,
        "md" => read("transcript.md")?,
        _ => {
            let report: Value = serde_json::from_str(&read("report.json")?)
                .map_err(|e| CliError::runtime("io", format!("report.json: {e}")))?;
            let segments: Vec<Value> = read("segments.jsonl")?
                .lines()
                .map(serde_json::from_str)
                .collect::<Result<_, _>>()
                .map_err(|e| CliError::runtime("io", format!("segments.jsonl: {e}")))?;
            serde_json::to_string_pretty(&json!({"report": report, "segments": segments}))
                .map_err(|e| CliError::runtime("io", e.to_string()))?
                + "\n"
        }
    };

    let name = format!("{}-{}.{extension}", short(&session, 8), short(&run_id, 9));
    let target: Option<PathBuf> = if ctx.args.flag("save") {
        Some(ctx.cfg.output_dir.join(&name))
    } else {
        match ctx.args.value("out") {
            None | Some("-") => None,
            Some("") => return Err(CliError::usage("--out needs a path")),
            Some(out) => {
                let path = PathBuf::from(out);
                let is_dir = path.is_dir() || out.ends_with(['/', '\\']);
                Some(if is_dir { path.join(&name) } else { path })
            }
        }
    };
    let Some(target) = target else {
        if ctx.json {
            // One JSON document, even for stdout output.
            let value = json!({
                "session_id": session, "run_id": run_id, "format": format, "content": content,
            });
            writeln!(io.out, "{value}")?;
        } else {
            io.out.write_all(content.as_bytes())?;
        }
        return Ok(0);
    };
    if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| CliError::runtime("io", format!("{}: {e}", parent.display())))?;
    }
    write_target(&target, &content, ctx.args.flag("force"))?;
    emit(
        io,
        ctx.json,
        format!("{}\n", target.display()),
        json!({"path": target.display().to_string(), "session_id": session, "run_id": run_id, "format": format}),
    )?;
    Ok(0)
}

/// Saves a run's markdown as `<dir>/<session8>-<run9>.md` without ever
/// overwriting. The folder is created only when its parent exists.
pub fn save_md(
    content: &str,
    session_id: &str,
    run_id: &str,
    dir: &Path,
) -> Result<PathBuf, CliError> {
    if !dir.exists() {
        let parent_exists = dir
            .parent()
            .is_some_and(|p| p.as_os_str().is_empty() || p.exists());
        if !parent_exists {
            return Err(CliError::runtime(
                "io",
                format!("{}: the parent folder does not exist", dir.display()),
            ));
        }
        std::fs::create_dir(dir)
            .map_err(|e| CliError::runtime("io", format!("{}: {e}", dir.display())))?;
    }
    let target = dir.join(format!("{}-{}.md", short(session_id, 8), short(run_id, 9)));
    write_target(&target, content, false)?;
    Ok(target)
}
