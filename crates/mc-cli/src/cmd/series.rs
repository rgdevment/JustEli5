//! `mc series`: meetings that recur under one name, and what carries over between them.

use std::path::Path;

use mc_store::{SeriesOrigin, Store, user_key};
use serde_json::{Value, json};

use super::fmt_utc;
use mc_engine::series::{chain, has_minutes};

use super::carry::indexed_store;
use crate::cli::{CliError, Ctx, Io, emit, resolve_one};

const USAGE: &str = "usage: eli5 series list | show <key> | set <session> <key>";

/// A session directory (indexed on the way) or an id or unique prefix of an indexed one.
fn resolve_session(store: &mut Store, input: &str) -> Result<String, CliError> {
    if Path::new(input).join("manifest.json").is_file() {
        store.import_session(input)?;
        return Ok(
            mc_store::manifest::Manifest::load(Path::new(input).join("manifest.json"))?.session_id,
        );
    }
    resolve_one("session", input, store.resolve_session(input)?)
}

fn date(ms: Option<i64>) -> String {
    ms.map_or("-".to_string(), fmt_utc)
}

fn list(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    let store = indexed_store(&ctx.cfg, None)?;
    let series = store.list_series()?;
    let mut human = String::new();
    for s in &series {
        human.push_str(&format!(
            "{}  {} meetings  last {}\n",
            s.key,
            s.sessions,
            date(s.last_wall_ms)
        ));
    }
    if series.is_empty() {
        human.push_str("no series; sessions with a meeting name are filed automatically\n");
    }
    let items: Vec<Value> = series
        .iter()
        .map(|s| json!({"key": s.key, "sessions": s.sessions, "last_wall_ms": s.last_wall_ms}))
        .collect();
    emit(io, ctx.json, human, json!({"series": items}))?;
    Ok(0)
}

fn show(ctx: &Ctx<'_>, io: &mut Io<'_>, key: &str) -> Result<u8, CliError> {
    let key = user_key(key).ok_or_else(|| CliError::usage(USAGE))?;
    let store = indexed_store(&ctx.cfg, None)?;
    let members = store.series_sessions(&key)?;
    if members.is_empty() {
        return Err(CliError::not_found(
            format!("no series {key:?}"),
            "`mc series list` shows them",
        ));
    }
    let mut human = format!("{key}\n");
    let mut sessions = Vec::new();
    for m in &members {
        let minutes = has_minutes(&m.dir);
        human.push_str(&format!(
            "  {}  {}  {}  {}{}\n",
            date(m.started_wall_ms),
            m.session_id,
            m.meeting_name.as_deref().unwrap_or("-"),
            m.origin.as_str(),
            if minutes { "  minutes" } else { "" }
        ));
        sessions.push(json!({
            "session_id": m.session_id,
            "dir": m.dir,
            "meeting_name": m.meeting_name,
            "started_wall_ms": m.started_wall_ms,
            "origin": m.origin.as_str(),
            "minutes": minutes,
        }));
    }
    let walked = chain(&members, None);
    let (from, commitments) = (walked.last, walked.open);
    for warning in &walked.warnings {
        human.push_str(&format!(
            "warning: {warning}
"
        ));
    }
    human.push_str(&format!(
        "open commitments ({}){}\n",
        commitments.len(),
        from.as_ref()
            .map_or(String::new(), |f| format!(" after {f}"))
    ));
    for c in &commitments {
        human.push_str(&format!("  @{}: {}  {}\n", c.who, c.what, c.source));
    }
    emit(
        io,
        ctx.json,
        human,
        json!({"key": key, "sessions": sessions, "open_from": from, "open_commitments": commitments,
               "warnings": walked.warnings}),
    )?;
    Ok(0)
}

fn set(ctx: &Ctx<'_>, io: &mut Io<'_>, session: &str, key: &str) -> Result<u8, CliError> {
    let key = user_key(key).ok_or_else(|| CliError::usage("the series key is empty"))?;
    let mut store = indexed_store(&ctx.cfg, None)?;
    let session = resolve_session(&mut store, session)?;
    store.assign_series(&session, &key, SeriesOrigin::Manual)?;
    emit(
        io,
        ctx.json,
        format!("{session}  series {key}\n"),
        json!({"session_id": session, "series": key, "origin": "manual"}),
    )?;
    Ok(0)
}

pub fn run(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    match ctx.args.positional.as_slice() {
        [action] if action == "list" => list(ctx, io),
        [action, key] if action == "show" => show(ctx, io, key),
        [action, session, key] if action == "set" => set(ctx, io, session, key),
        _ => Err(CliError::usage(USAGE)),
    }
}
