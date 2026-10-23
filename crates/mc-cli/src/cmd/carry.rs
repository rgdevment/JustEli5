//! Memory between meetings: which series a session belongs to and which commitments of the
//! earlier meetings of that series are still open.

use std::path::Path;

use mc_analyst::CarriedCommitment;
use mc_asr::Glossary;
use mc_engine::series::{chain, index_corpus};
use mc_store::manifest::Manifest;
use mc_store::{Store, series_for_name};

use super::vocabulary::merged_glossary;
use crate::cli::{CliError, Ctx, open_store};
use crate::config::Config;

/// What a meeting inherits from the earlier ones of its series.
#[derive(Debug, Default)]
pub struct Carry {
    pub series: Option<String>,
    /// The latest session before this one that has minutes.
    pub previous: Option<String>,
    pub carried: Vec<CarriedCommitment>,
    /// Why the chain may be incomplete (a meeting without minutes, no index...).
    pub warnings: Vec<String>,
}

/// What a command reads once about the meeting it works on, however many steps use it.
pub struct Memory {
    pub carry: Carry,
    pub glossary: Glossary,
}

pub fn load(ctx: &Ctx<'_>, dir: &Path) -> Result<Memory, CliError> {
    Ok(Memory {
        carry: carry_for(&ctx.cfg, dir)?,
        glossary: merged_glossary(ctx)?,
    })
}

/// Indexes the sessions the capture saved that the index does not know yet (the ones it knows
/// are not read again; `mc import` refreshes them), then files every session under its series.
/// `except` is the session being worked on. Sessions kept elsewhere are found once `mc import`
/// or `mc run` has indexed them.
pub fn indexed_store(cfg: &Config, except: Option<&Path>) -> Result<Store, CliError> {
    let mut store = open_store(cfg)?;
    let current = except.and_then(|p| std::fs::canonicalize(p).ok());
    index_corpus(&mut store, &cfg.corpus_dir(), &cfg.series_rules, |path| {
        current.is_some() && std::fs::canonicalize(path).ok() == current
    })?;
    Ok(store)
}

/// The open commitments of the earlier meetings of the series of the session in `dir` (the
/// series of a meeting that is not indexed yet comes from its name). When the index cannot be
/// used (locked, newer than this build), the meeting simply has no series, with a warning.
pub fn carry_for(cfg: &Config, dir: &Path) -> Result<Carry, CliError> {
    let manifest = Manifest::load(dir.join("manifest.json"))?;
    let found = (|| -> Result<Carry, CliError> {
        let store = indexed_store(cfg, Some(dir))?;
        let series = match store.series_of(&manifest.session_id)? {
            Some((key, _)) => Some(key),
            None => manifest
                .meeting_name
                .as_deref()
                .and_then(|name| series_for_name(name, &cfg.series_rules))
                .map(|(key, _)| key),
        };
        let Some(key) = series else {
            return Ok(Carry::default());
        };
        let members = store.series_sessions(&key)?;
        let chain = chain(
            &members,
            Some((&manifest.session_id, manifest.started_wall_ms)),
        );
        Ok(Carry {
            series: Some(key),
            previous: chain.last,
            carried: chain.open,
            warnings: chain.warnings,
        })
    })();
    Ok(found.unwrap_or_else(|e| Carry {
        warnings: vec![format!(
            "memory between meetings is off for this one: {}",
            e.message()
        )],
        ..Carry::default()
    }))
}

/// A stable text of the carried commitments, for ids.
pub fn identity(carried: &[CarriedCommitment]) -> String {
    serde_json::to_string(carried).expect("commitments serialise")
}
