use std::path::Path;

use mc_store::manifest::Manifest;
use serde_json::{Value, json};

use crate::cli::{CliError, Ctx, Io, emit, open_store, resolve_one};

const USAGE: &str = "usage: eli5 prune --audio [<session>] [--older-than DAYS] [--include-unfused] [--include-unended] [--apply]";
const DAY_MS: i64 = 86_400_000;
/// Only the heavy recordings go; `audio_sync.jsonl`, transcripts and runs stay.
const AUDIO_FILES: [&str; 2] = ["mic.wav", "system.wav"];

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// A fusion counts only when it is a real, non-empty file in a real directory.
fn has_fusion(dir: &Path) -> bool {
    let fused = dir.join("fused");
    if std::fs::symlink_metadata(&fused).is_ok_and(|m| !m.file_type().is_dir()) {
        return false;
    }
    std::fs::read_dir(&fused).is_ok_and(|entries| {
        entries.flatten().any(|e| {
            std::fs::symlink_metadata(e.path().join("fused.jsonl"))
                .is_ok_and(|m| m.file_type().is_file() && m.len() > 0)
        })
    })
}

/// A recorder may still be writing: a recent write means the session is live.
fn recently_written(dir: &Path, files: &[(String, u64)]) -> bool {
    files.iter().any(|(name, _)| {
        std::fs::metadata(dir.join(name))
            .and_then(|m| m.modified())
            .is_ok_and(|t| {
                t.elapsed()
                    .map_or(true, |age| age < std::time::Duration::from_secs(600))
            })
    })
}

/// Regular files only: a symlink named `mic.wav` is never followed or removed.
fn audio_files(dir: &Path) -> Vec<(String, u64)> {
    AUDIO_FILES
        .iter()
        .filter_map(|name| {
            let meta = std::fs::symlink_metadata(dir.join(name)).ok()?;
            meta.file_type()
                .is_file()
                .then(|| (name.to_string(), meta.len()))
        })
        .collect()
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

pub fn run(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    if !ctx.args.flag("audio") {
        return Err(CliError::usage(format!(
            "nothing to prune: pass --audio\n{USAGE}"
        )));
    }
    let prefix = match ctx.args.positional.as_slice() {
        [] => None,
        [one] => Some(one.as_str()),
        _ => return Err(CliError::usage(USAGE)),
    };
    let older_than = ctx
        .args
        .value("older-than")
        .map(|v| {
            v.parse::<u32>().map_err(|_| {
                CliError::usage(format!("--older-than: {v:?} is not a number of days"))
            })
        })
        .transpose()?;
    if prefix.is_none() && older_than.is_none() {
        return Err(CliError::usage(format!(
            "pass --older-than DAYS or one session\n{USAGE}"
        )));
    }
    let apply = ctx.args.flag("apply");
    let include_unfused = ctx.args.flag("include-unfused");
    let include_unended = ctx.args.flag("include-unended");
    let cutoff = now_ms() - i64::from(older_than.unwrap_or(0)) * DAY_MS;

    let store = open_store(&ctx.cfg)?;
    let wanted = match prefix {
        Some(p) => Some(resolve_one("session", p, store.resolve_session(p)?)?),
        None => None,
    };

    let mut human = String::new();
    let mut items = Vec::new();
    let (mut total_files, mut total_bytes, mut failures) = (0usize, 0u64, 0usize);
    for s in store.list_sessions()? {
        if wanted.as_ref().is_some_and(|w| *w != s.session_id) {
            continue;
        }
        let dir = Path::new(&s.dir);
        let files = audio_files(dir);
        let manifest = Manifest::load(dir.join("manifest.json")).ok();
        let skip: Option<&str> = if files.is_empty() {
            if dir.is_dir() {
                continue;
            }
            Some("directory missing (stale index)")
        } else if manifest
            .as_ref()
            .is_none_or(|m| m.session_id != s.session_id)
        {
            Some("manifest missing or from another session")
        } else {
            let m = manifest.as_ref().expect("checked above");
            let age_ref = match m.ended_wall_ms {
                Some(t) => Some(t),
                None if include_unended => Some(m.started_wall_ms),
                None => None,
            };
            if m.ended_wall_ms.is_none() && !include_unended {
                Some("session not ended (--include-unended to override)")
            } else if age_ref.is_none_or(|t| t <= 0) {
                Some("undated")
            } else if age_ref.is_some_and(|t| t > cutoff) {
                Some("too recent")
            } else if recently_written(dir, &files) {
                Some("audio written in the last 10 minutes")
            } else if !include_unfused && !has_fusion(dir) {
                Some("no fused transcript yet (--include-unfused to override)")
            } else {
                None
            }
        };
        let bytes: u64 = files.iter().map(|(_, n)| n).sum();
        let mut removed = Vec::new();
        let mut failed = Vec::new();
        let mut removed_bytes = 0u64;
        let mut manifest_error = None;
        if skip.is_none() {
            for (name, size) in &files {
                if !apply {
                    continue;
                }
                match std::fs::remove_file(dir.join(name)) {
                    Ok(()) => {
                        removed.push(name.clone());
                        removed_bytes += size;
                    }
                    Err(e) => failed.push(json!({"file": name, "error": e.to_string()})),
                }
            }
            if apply && !removed.is_empty() {
                manifest_error = mark_manifest(dir, &removed, removed_bytes).err();
            }
            total_files += removed.len();
            total_bytes += if apply { removed_bytes } else { bytes };
            if !apply {
                total_files += files.len();
            }
            failures += failed.len();
        }
        human.push_str(&format!(
            "{}  {}  {}\n",
            s.session_id,
            mib(bytes),
            match skip {
                Some(reason) => format!("kept: {reason}"),
                None if !apply => "would remove audio".to_string(),
                None if failed.is_empty() => "audio removed".to_string(),
                None => format!("partially removed ({} failed)", failed.len()),
            }
        ));
        if let Some(e) = &manifest_error {
            human.push_str(&format!("  warning: manifest not updated: {e}\n"));
        }
        items.push(json!({
            "session_id": s.session_id,
            "dir": s.dir,
            "bytes": bytes,
            "files": files.iter().map(|(n, _)| n).collect::<Vec<_>>(),
            "would_remove": if apply || skip.is_some() { vec![] } else { files.iter().map(|(n, _)| n.clone()).collect() },
            "removed": removed,
            "failed": failed,
            "manifest_error": manifest_error,
            "kept_because": skip,
        }));
    }
    if items.is_empty() {
        human.push_str("no recorded audio found\n");
    }
    human.push_str(&format!(
        "{} {} file(s), {}{}\n",
        if apply { "removed" } else { "would remove" },
        total_files,
        mib(total_bytes),
        if apply {
            ""
        } else {
            "  (dry run; add --apply)"
        }
    ));
    emit(
        io,
        ctx.json,
        human,
        json!({"applied": apply, "files": total_files, "bytes": total_bytes, "failures": failures, "sessions": items}),
    )?;
    Ok(u8::from(failures > 0))
}

/// Leaves a trace in the manifest so the missing audio is explained later;
/// repeated prunes add to it.
fn mark_manifest(dir: &Path, removed: &[String], bytes: u64) -> Result<(), String> {
    let path = dir.join("manifest.json");
    let mut manifest = Manifest::load(&path).map_err(|e| e.to_string())?;
    if !manifest.audio.is_object() {
        manifest.audio = json!({});
    }
    if let Value::Object(audio) = &mut manifest.audio {
        let previous = audio.get("pruned").cloned().unwrap_or(Value::Null);
        let mut files: Vec<String> = previous["files"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        for name in removed {
            if !files.contains(name) {
                files.push(name.clone());
            }
        }
        let total = previous["bytes"].as_u64().unwrap_or(0) + bytes;
        audio.insert(
            "pruned".into(),
            json!({"at_ms": now_ms(), "files": files, "bytes": total}),
        );
    }
    manifest.save_atomic(&path).map_err(|e| e.to_string())
}
