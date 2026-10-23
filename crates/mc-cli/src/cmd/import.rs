use std::path::PathBuf;

use mc_store::{ImportAction, RebuildReason};
use serde_json::json;

use crate::cli::{CliError, Ctx, Io, emit, open_store};

fn action_name(action: ImportAction) -> &'static str {
    match action {
        ImportAction::Created => "created",
        ImportAction::Appended => "appended",
        ImportAction::Unchanged => "unchanged",
        ImportAction::Rebuilt(RebuildReason::Shrunk) => "rebuilt:shrunk",
        ImportAction::Rebuilt(RebuildReason::Replaced) => "rebuilt:replaced",
    }
}

pub fn run(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    let (target, explicit) = match ctx.args.positional.as_slice() {
        [] => (ctx.cfg.corpus_dir(), false),
        [one] => (PathBuf::from(one), true),
        _ => return Err(CliError::usage("usage: eli5 import [<dir|root>]")),
    };
    if !explicit && !target.is_dir() {
        // A fresh install has recorded nothing yet.
        return emit(
            io,
            ctx.json,
            "no sessions found\n".to_string(),
            json!({"sessions": [], "failed": 0}),
        )
        .map(|()| 0);
    }
    if !target.is_dir() {
        return Err(CliError::not_found(
            format!("{}: not a directory", target.display()),
            "pass a session directory or a root that contains them",
        ));
    }
    let mut store = open_store(&ctx.cfg)?;
    let results = if target.join("manifest.json").is_file() {
        let result = store.import_session(&target);
        vec![(target.clone(), result)]
    } else {
        store.import_all(&target)?
    };

    let (mut human, mut items, mut failed) = (String::new(), Vec::new(), 0usize);
    for (dir, result) in &results {
        match result {
            Ok(report) => {
                human.push_str(&format!(
                    "{}  {}  +{} lines, {} bad, {} pending bytes{}\n",
                    report.session_id,
                    action_name(report.action),
                    report.lines_added,
                    report.bad.len(),
                    report.tail_pending_bytes,
                    if report.ended_cleanly {
                        ""
                    } else {
                        "  (not ended cleanly)"
                    },
                ));
                items.push(json!({
                    "dir": dir.display().to_string(),
                    "session_id": report.session_id,
                    "action": action_name(report.action),
                    "lines_added": report.lines_added,
                    "bad_lines": report.bad.len(),
                    "tail_pending_bytes": report.tail_pending_bytes,
                    "ended_cleanly": report.ended_cleanly,
                }));
            }
            Err(err) => {
                failed += 1;
                human.push_str(&format!("{}  FAILED: {err}\n", dir.display()));
                items.push(json!({"dir": dir.display().to_string(), "error": err.to_string()}));
            }
        }
    }
    if results.is_empty() {
        human.push_str("no sessions found\n");
    }
    emit(
        io,
        ctx.json,
        human,
        json!({"sessions": items, "failed": failed}),
    )?;
    Ok(u8::from(failed > 0))
}
