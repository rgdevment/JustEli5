use serde_json::{Value, json};

use super::{fmt_duration, fmt_utc};
use crate::cli::{CliError, Ctx, Io, emit, open_store, resolve_one};

pub fn run(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    let store = open_store(&ctx.cfg)?;
    match ctx.args.positional.as_slice() {
        [] => {
            let sessions = store.list_sessions()?;
            if sessions.is_empty() {
                return emit_empty(ctx, io);
            }
            let mut human = String::new();
            let mut items = Vec::new();
            for s in &sessions {
                let runs = store.runs(&s.session_id)?;
                let latest = store.latest_run(&s.session_id)?;
                human.push_str(&format!(
                    "{}  {}  {}  {} captions, {} committed, {} bad, {} runs{}\n",
                    s.session_id,
                    s.started_wall_ms.map_or("-".to_string(), fmt_utc),
                    s.meeting_name.as_deref().unwrap_or("-"),
                    s.captions,
                    s.committed,
                    s.bad_lines,
                    runs.len(),
                    latest
                        .as_ref()
                        .map_or(String::new(), |l| format!("  latest {l}")),
                ));
                items.push(json!({
                    "session_id": s.session_id,
                    "dir": s.dir,
                    "meeting_name": s.meeting_name,
                    "started_wall_ms": s.started_wall_ms,
                    "ended_wall_ms": s.ended_wall_ms,
                    "captions": s.captions,
                    "committed": s.committed,
                    "bad_lines": s.bad_lines,
                    "runs": runs.len(),
                    "latest_run": latest,
                }));
            }
            emit(io, ctx.json, human, json!({"sessions": items}))?;
        }
        [prefix] => {
            let session = resolve_one("session", prefix, store.resolve_session(prefix)?)?;
            let mut human = format!("{session}\n");
            let mut items = Vec::new();
            for run_id in store.runs(&session)? {
                let Some(row) = store.run(&run_id)? else {
                    continue;
                };
                let config: Value = serde_json::from_str(&row.config_json).unwrap_or(Value::Null);
                human.push_str(&format!(
                    "  {run_id}  flush {}  {} content / {} timing  {}\n",
                    config["flush"].as_str().unwrap_or("?"),
                    row.content_div,
                    row.timing_div,
                    fmt_duration(row.duration_ms),
                ));
                items.push(json!({
                    "run_id": run_id,
                    "dir": row.dir,
                    "config": config,
                    "content_div": row.content_div,
                    "timing_div": row.timing_div,
                    "duration_ms": row.duration_ms,
                    "ended_cleanly": row.ended_cleanly,
                    "synthetic_flush": row.synthetic_flush,
                }));
            }
            if items.is_empty() {
                human.push_str("  no runs; use `mc run <dir>`\n");
            }
            let mut asr = Vec::new();
            for id in store.asr_runs(&session)? {
                let Some(row) = store.asr_run(&session, &id)? else {
                    continue;
                };
                human.push_str(&format!(
                    "  asr {id}  {} {}  {} segments\n",
                    row.engine, row.model, row.segments
                ));
                asr.push(json!({"asr_id": id, "dir": row.dir, "engine": row.engine,
                                "model": row.model, "segments": row.segments}));
            }
            let mut fusions = Vec::new();
            for id in store.fusions(&session)? {
                let Some(row) = store.fusion(&id)? else {
                    continue;
                };
                human.push_str(&format!(
                    "  fusion {id}  asr {}  {} merged, {} captions only, {} ASR only, {} term fixes\n",
                    row.asr_id, row.merged, row.caption_only, row.asr_only, row.edits_applied
                ));
                fusions.push(json!({"fuse_id": id, "dir": row.dir, "run_id": row.run_id,
                                    "asr_id": row.asr_id, "merged": row.merged,
                                    "caption_only": row.caption_only, "asr_only": row.asr_only,
                                    "edits_applied": row.edits_applied,
                                    "lag_median_ms": row.lag_median_ms}));
            }
            emit(
                io,
                ctx.json,
                human,
                json!({"session_id": session, "runs": items, "asr_runs": asr, "fusions": fusions}),
            )?;
        }
        _ => return Err(CliError::usage("usage: eli5 status [<session>]")),
    }
    Ok(0)
}

fn emit_empty(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    emit(
        io,
        ctx.json,
        "no sessions; run `mc import <dir>`\n".to_string(),
        json!({"sessions": []}),
    )?;
    Ok(0)
}
