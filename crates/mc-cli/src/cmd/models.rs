use std::time::{Duration, Instant};

use mc_models::{ModelError, ModelStore, PullOptions, PullOutcome};
use serde_json::json;

use crate::cli::{CliError, Ctx, Io, emit};

const USAGE: &str = "usage: eli5 models list | pull <name> [--url URL] [--sha256 HASH] [--force] | verify <name> | path <name> | remove <name>";

fn map_error(error: ModelError) -> CliError {
    match error {
        ModelError::Unknown(_) | ModelError::BadName(_) | ModelError::BadUrl(_) => {
            CliError::usage(error.to_string())
        }
        ModelError::NotInstalled(_) => {
            CliError::not_found(error.to_string(), "run `mc models list`")
        }
        ModelError::Mismatch { .. } => CliError::runtime("checksum", error.to_string()),
        ModelError::Download(_) => CliError::runtime("network", error.to_string()),
        ModelError::Io { .. } => CliError::runtime("io", error.to_string()),
    }
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

pub fn run(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    let store = ModelStore::new(ctx.cfg.data_dir.join("models"));
    let positional = ctx.args.positional.as_slice();
    let (action, name) = match positional {
        [action] => (action.as_str(), None),
        [action, name] => (action.as_str(), Some(name.as_str())),
        _ => return Err(CliError::usage(USAGE)),
    };
    let need_name = || name.ok_or_else(|| CliError::usage(USAGE));
    if action != "pull" {
        for option in ["url", "sha256"] {
            if ctx.args.value(option).is_some() {
                return Err(CliError::usage(format!("--{option} only applies to pull")));
            }
        }
        if ctx.args.flag("force") {
            return Err(CliError::usage("--force only applies to pull"));
        }
    }
    match action {
        "list" => {
            if name.is_some() {
                return Err(CliError::usage(USAGE));
            }
            let models = store.list();
            let mut human = format!("models in {}\n", store.dir().display());
            for m in &models {
                human.push_str(&format!(
                    "  {:<30} {}  {}\n",
                    m.name,
                    if m.installed {
                        format!(
                            "installed {:>10}{}",
                            mib(m.size.unwrap_or(0)),
                            if m.pinned {
                                ""
                            } else {
                                "  (checksum recorded, not official)"
                            }
                        )
                    } else {
                        "not installed".to_string()
                    },
                    m.description
                ));
            }
            emit(
                io,
                ctx.json,
                human,
                json!({"dir": store.dir().display().to_string(), "models": models}),
            )?;
            Ok(0)
        }
        "pull" => {
            let name = need_name()?;
            let opts = PullOptions {
                url: ctx.args.value("url").map(str::to_string),
                sha256: ctx.args.value("sha256").map(str::to_string),
                force: ctx.args.flag("force"),
            };
            let show = !ctx.json;
            let mut last = Instant::now() - Duration::from_secs(10);
            let outcome = store
                .pull(name, &opts, &mut |done, total| {
                    if show && last.elapsed() >= Duration::from_secs(2) {
                        last = Instant::now();
                        let _ = match total {
                            Some(t) => writeln!(io.err, "  {name}: {} of {}", mib(done), mib(t)),
                            None => writeln!(io.err, "  {name}: {}", mib(done)),
                        };
                    }
                })
                .map_err(map_error)?;
            let (human, value) = match outcome {
                PullOutcome::AlreadyInstalled { path } => (
                    format!("{name} is already installed ({})\n", path.display()),
                    json!({"name": name, "action": "already_installed", "path": path.display().to_string()}),
                ),
                PullOutcome::Downloaded {
                    path,
                    sha256,
                    size,
                    unpinned,
                } => (
                    format!(
                        "{name} installed ({}) in {}\n  sha256 {sha256}{}\n",
                        mib(size),
                        path.display(),
                        if unpinned {
                            "\n  note: no official checksum is known for this model; this one was recorded and will be checked from now on"
                        } else {
                            ""
                        }
                    ),
                    json!({"name": name, "action": "downloaded", "path": path.display().to_string(),
                           "sha256": sha256, "size": size, "unpinned": unpinned}),
                ),
            };
            emit(io, ctx.json, human, value)?;
            Ok(0)
        }
        "verify" => {
            let report = store.verify(need_name()?).map_err(map_error)?;
            let human = format!(
                "{}  {}  {}\n",
                if report.ok { "ok" } else { "FAIL" },
                report.name,
                match (&report.expected, report.ok) {
                    (None, _) => "no checksum recorded".to_string(),
                    (Some(_), true) => report.actual.clone(),
                    (Some(e), false) => format!("expected {e}, got {}", report.actual),
                }
            );
            let ok = report.ok;
            emit(io, ctx.json, human, json!(report))?;
            Ok(u8::from(!ok))
        }
        "path" => {
            let path = store.path(need_name()?).map_err(map_error)?;
            emit(
                io,
                ctx.json,
                format!("{}\n", path.display()),
                json!({"path": path.display().to_string()}),
            )?;
            Ok(0)
        }
        "remove" => {
            let name = need_name()?;
            let removed = store.remove(name).map_err(map_error)?;
            emit(
                io,
                ctx.json,
                if removed {
                    format!("{name} removed\n")
                } else {
                    format!("{name} was not installed\n")
                },
                json!({"name": name, "removed": removed}),
            )?;
            Ok(0)
        }
        _ => Err(CliError::usage(USAGE)),
    }
}
