//! `mc series`: meetings that recur under one name, and what carries over between them.

use std::path::Path;

use mc_store::{SeriesOrigin, Store, name_similarity, series_for_name, user_key};
use serde_json::{Value, json};

use super::fmt_utc;
use mc_engine::series::{chain, has_minutes};

use super::carry::indexed_store;
use super::export_tisty::series_docs;
use crate::cli::{CliError, Ctx, Io, emit, resolve_one};
use crate::publish::tisty::{meeting_series, resolve};

const USAGE: &str =
    "usage: eli5 series list | show <key> | set <session> <key> | match <meeting name> [--tisty]";

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

/// Below this a name is another meeting, not the same one written differently.
const ALIKE: f64 = 0.6;

/// A series a new meeting could belong to: recorded here, kept in Tisty, or both.
struct Known {
    key: String,
    title: String,
    sessions: u64,
    last_wall_ms: Option<i64>,
    in_tisty: bool,
}

fn local_series(store: &Store) -> Result<Vec<Known>, CliError> {
    let mut known = Vec::new();
    for s in store.list_series()? {
        let title = store
            .series_sessions(&s.key)?
            .into_iter()
            .rev()
            .find_map(|m| m.meeting_name)
            .unwrap_or_else(|| s.key.clone());
        known.push(Known {
            key: s.key,
            title,
            sessions: s.sessions,
            last_wall_ms: s.last_wall_ms,
            in_tisty: false,
        });
    }
    Ok(known)
}

/// Tisty's titles win: they are what the person sees and may have renamed.
fn add_tisty_series(ctx: &Ctx<'_>, known: &mut Vec<Known>) -> Result<(), String> {
    let settings = &ctx.cfg.tisty;
    let program = resolve(ctx.env, &settings.command)
        .ok_or_else(|| format!("Tisty was not found (`{}`)", settings.command))?;
    let published = series_docs(&ctx.cfg)
        .into_iter()
        .map(|(key, doc)| (doc, key))
        .collect();
    for found in meeting_series(&program, &settings.args, &settings.folder, &published)? {
        match known.iter_mut().find(|k| k.key == found.key) {
            Some(k) => {
                k.title = found.title;
                k.in_tisty = true;
            }
            None => known.push(Known {
                key: found.key,
                title: found.title,
                sessions: 0,
                last_wall_ms: None,
                in_tisty: true,
            }),
        }
    }
    Ok(())
}

/// The series a meeting name belongs to, or the ones it may have been meant as: a name typed a
/// little differently would otherwise start a new series.
fn find_alike(ctx: &Ctx<'_>, io: &mut Io<'_>, name: &str) -> Result<u8, CliError> {
    let key = series_for_name(name, &ctx.cfg.series_rules).map(|(key, _)| key);
    let store = indexed_store(&ctx.cfg, None)?;
    let mut known = local_series(&store)?;
    let tisty_error = if ctx.args.flag("tisty") {
        add_tisty_series(ctx, &mut known).err()
    } else {
        None
    };
    let exact = known.iter().find(|k| Some(&k.key) == key.as_ref());
    let mut alike: Vec<(f64, &Known)> = if exact.is_some() {
        Vec::new()
    } else {
        known
            .iter()
            .map(|k| {
                (
                    name_similarity(name, &k.title).max(name_similarity(name, &k.key)),
                    k,
                )
            })
            .filter(|(score, _)| *score >= ALIKE)
            .collect()
    };
    alike.sort_by(|a, b| b.0.total_cmp(&a.0));
    alike.truncate(3);
    let mut human = match exact {
        Some(k) => format!("{name}: series {} ({} meetings)\n", k.key, k.sessions),
        None if alike.is_empty() => format!("{name}: a new series\n"),
        None => format!("{name}: a new series, unless it is one of these\n"),
    };
    for (score, k) in &alike {
        human.push_str(&format!("  {:.2}  {}  ({})\n", score, k.title, k.key));
    }
    if let Some(why) = &tisty_error {
        human.push_str(&format!("warning: Tisty was not asked: {why}\n"));
    }
    let item = |score: Option<f64>, k: &Known| {
        json!({"key": k.key, "title": k.title, "sessions": k.sessions,
               "last_wall_ms": k.last_wall_ms, "in_tisty": k.in_tisty, "score": score})
    };
    emit(
        io,
        ctx.json,
        human,
        json!({"name": name, "key": key, "exact": exact.map(|k| item(None, k)),
               "alike": alike.iter().map(|(s, k)| item(Some(*s), k)).collect::<Vec<_>>(),
               "tisty_error": tisty_error}),
    )?;
    Ok(0)
}

pub fn run(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    match ctx.args.positional.as_slice() {
        [action] if action == "list" => list(ctx, io),
        [action, name] if action == "match" => find_alike(ctx, io, name),
        [action, key] if action == "show" => show(ctx, io, key),
        [action, session, key] if action == "set" => set(ctx, io, session, key),
        _ => Err(CliError::usage(USAGE)),
    }
}
