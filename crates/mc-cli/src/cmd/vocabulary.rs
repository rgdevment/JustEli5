//! The glossary every command works with: the configured file merged with the terms the memory
//! knows, which `eli5 memory`, `eli5 glossary` and every minutes add to.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mc_asr::Glossary;
use mc_core::fold::fold;
use mc_memory::{Kind, Memory, State};

use super::memory::open_memory;
use crate::cli::{CliError, Ctx};
use crate::config::Config;

/// The user's glossary before the memory existed: the memory starts from it.
pub fn user_glossary_path(cfg: &Config) -> PathBuf {
    cfg.data_dir.join("glossary.json")
}

pub fn read_glossary_file(cfg: &Config) -> Result<Glossary, CliError> {
    Glossary::load(&user_glossary_path(cfg)).map_err(CliError::usage)
}

/// The memory's terms, what the user confirmed first; homonyms share a spelling, so their
/// meanings go together.
pub fn load_user_glossary(cfg: &Config) -> Result<Glossary, CliError> {
    memory_glossary(&open_memory(cfg)?)
}

fn memory_glossary(memory: &Memory) -> Result<Glossary, CliError> {
    let mut terms = memory.list(Some(Kind::Term), None, None)?;
    terms.sort_by_key(|t| t.state != State::Confirmed);
    let mut glossary = Glossary::default();
    let mut meanings: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for term in &terms {
        glossary.learn(&term.name, None);
        for form in &term.forms {
            glossary.learn(&term.name, Some(&form.text));
        }
        if let Some(meaning) = &term.meaning {
            let entry = meanings
                .entry(fold(&term.name))
                .or_insert_with(|| (term.name.clone(), Vec::new()));
            entry.1.push(meaning.clone());
        }
    }
    for (name, list) in meanings.values() {
        glossary.explain(name, &list.join(" | o bien: "));
    }
    Ok(glossary)
}

/// The forms two or more entries share, each with `Name: what it is` for every entry.
pub fn memory_homonyms(cfg: &Config) -> Result<Vec<(String, Vec<String>)>, CliError> {
    let terms = open_memory(cfg)?.list(Some(Kind::Term), None, None)?;
    let mut by_form: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for term in &terms {
        let what = match &term.meaning {
            Some(meaning) => format!("{}: {meaning}", term.name),
            None => term.name.clone(),
        };
        for form in &term.forms {
            let entry = by_form
                .entry(fold(&form.text))
                .or_insert_with(|| (form.text.clone(), Vec::new()));
            if !entry.1.contains(&what) {
                entry.1.push(what.clone());
            }
        }
    }
    Ok(by_form.into_values().filter(|(_, m)| m.len() > 1).collect())
}

/// (form, term) for the forms that name one spelling only: the fusion turns them into the term
/// without asking, so a form two different terms share is left out.
pub fn memory_aliases(cfg: &Config) -> Result<Vec<(String, String)>, CliError> {
    let terms = open_memory(cfg)?.list(Some(Kind::Term), None, None)?;
    let mut targets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for term in &terms {
        for form in &term.forms {
            let names = targets.entry(fold(&form.text)).or_default();
            if !names.iter().any(|n| fold(n) == fold(&term.name)) {
                names.push(term.name.clone());
            }
        }
    }
    let mut aliases = Vec::new();
    for term in &terms {
        for form in term
            .forms
            .iter()
            .filter(|f| fold(&f.text) != fold(&term.name))
        {
            if targets.get(&fold(&form.text)).is_some_and(|n| n.len() == 1) {
                aliases.push((form.text.clone(), term.name.clone()));
            }
        }
    }
    Ok(aliases)
}

/// `--glossary FILE`, else `fusion.glossary_file`.
pub fn glossary_file(ctx: &Ctx<'_>) -> Option<PathBuf> {
    ctx.args
        .value("glossary")
        .map(PathBuf::from)
        .or_else(|| ctx.cfg.fusion.glossary_file.clone())
}

/// The configured glossary file, then what the memory adds; the memory's spelling wins for a
/// term both have.
pub fn glossary_from(cfg: &Config, file: Option<&Path>) -> Result<Glossary, CliError> {
    let configured = match file {
        None => Glossary::default(),
        Some(file) => {
            let text = std::fs::read_to_string(file).map_err(|e| {
                CliError::not_found(format!("{}: {e}", file.display()), "pass a glossary file")
            })?;
            Glossary::parse(&text)
                .map_err(|e| CliError::usage(format!("{}: {e}", file.display())))?
        }
    };
    Ok(Glossary::merged(configured, &load_user_glossary(cfg)?))
}

pub fn merged_glossary(ctx: &Ctx<'_>) -> Result<Glossary, CliError> {
    glossary_from(&ctx.cfg, glossary_file(ctx).as_deref())
}

/// The terms for the recogniser prompt, whose budget is small: what the user taught comes first,
/// so a long configured glossary cannot starve it.
pub fn prompt_terms(ctx: &Ctx<'_>) -> Result<Vec<String>, CliError> {
    Ok(merged_glossary(ctx)?.terms_first(&load_user_glossary(&ctx.cfg)?))
}
