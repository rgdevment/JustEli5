//! `eli5 memory`: the terms and people known between meetings (`<data dir>/memory.sqlite`), to
//! look up and correct. What the user does here is confirmed: the model never changes it.

use std::path::PathBuf;

use mc_analyst::Learned;
use mc_core::fold::fold_words;
use mc_memory::{
    Adopted, Concept, Context, Kind, Learnt, Lesson, Memory, MemoryError, Met, Resolution, State,
};
use serde_json::json;

use super::vocabulary::read_glossary_file;
use crate::cli::{CliError, Ctx, Io, emit};
use crate::config::Config;

const USAGE: &str =
    "usage: eli5 memory list [--kind term|person] [--query Q] [--state proposed|confirmed]
       eli5 memory show <id>
       eli5 memory add term|person <name> [--meaning M] [--forms A,B] [--role R]
       eli5 memory edit <id> [--name N] [--meaning M] [--role R]
       eli5 memory form add|remove|split <id> <text> | form move <from-id> <to-id> <text>
       eli5 memory merge <keep-id> <gone-id> | confirm <id>... | remove <id>
       eli5 memory resolve term|person <text> [--series S] [--present ID,ID]
       eli5 memory export [--out FILE] | import <file> | adopt-people <people.json>";

impl From<MemoryError> for CliError {
    fn from(err: MemoryError) -> Self {
        match err {
            MemoryError::Missing(id) => CliError::not_found(
                format!("no entry {id} in the memory"),
                "`eli5 memory list` shows the ids",
            ),
            MemoryError::Invalid(why) => CliError::usage(why),
            other => CliError::runtime("memory", other.to_string()),
        }
    }
}

pub fn memory_path(cfg: &Config) -> PathBuf {
    cfg.data_dir.join("memory.sqlite")
}

/// The memory, which takes in the terms of the user's `glossary.json` the first time.
pub fn open_memory(cfg: &Config) -> Result<Memory, CliError> {
    std::fs::create_dir_all(&cfg.data_dir)
        .map_err(|e| CliError::runtime("io", format!("{}: {e}", cfg.data_dir.display())))?;
    let mut memory = Memory::open(memory_path(cfg))?;
    memory.tidy(now_ms())?;
    let glossary = read_glossary_file(cfg)?;
    if glossary.is_empty() {
        return Ok(memory);
    }
    let meanings = glossary.meanings();
    let aliases = glossary.aliases();
    let terms: Vec<Adopted> = glossary
        .terms()
        .into_iter()
        .map(|term| Adopted {
            kind: Kind::Term,
            meaning: meanings
                .iter()
                .find(|(t, _)| *t == term)
                .map(|(_, m)| m.clone()),
            forms: aliases
                .iter()
                .filter(|(t, _)| *t == term)
                .flat_map(|(_, heard)| heard.clone())
                .collect(),
            role: None,
            name: term,
        })
        .collect();
    memory.adopt("glossary.json", &terms, now_ms())?;
    Ok(memory)
}

/// `name` is a person, and the captions write them as `form`: one entry made or confirmed.
pub fn teach_person(cfg: &Config, name: &str, form: &str) -> Result<i64, CliError> {
    let mut memory = open_memory(cfg)?;
    let at = now_ms();
    let id = match memory.candidates(Kind::Person, name)?.as_slice() {
        [one] => *one,
        _ => memory.add(Kind::Person, name, None, State::Confirmed, at)?,
    };
    memory.claim_form(id, form, at)?;
    memory.confirm(id, at)?;
    Ok(id)
}

/// `term` as the user spells it, with a form it is heard in and what it is: one entry made or
/// confirmed. A spelling two entries share needs `eli5 memory`, which names them by id.
pub fn teach_term(
    memory: &mut Memory,
    term: &str,
    heard: Option<&str>,
    meaning: Option<&str>,
) -> Result<i64, CliError> {
    let at = now_ms();
    let id = match memory.candidates(Kind::Term, term)?.as_slice() {
        [] => memory.add(Kind::Term, term, None, State::Confirmed, at)?,
        [one] => *one,
        many => {
            return Err(CliError::usage(format!(
                "{term:?} names {} entries; `eli5 memory list --query {term:?}` shows their ids",
                many.len()
            )));
        }
    };
    if let Some(heard) = heard.filter(|h| !h.trim().is_empty()) {
        memory.claim_form(id, heard, at)?;
    }
    if let Some(meaning) = meaning {
        memory.set_meaning(id, Some(meaning), at)?;
    }
    memory.confirm(id, at)?;
    Ok(id)
}

/// Removes the one entry `term` names; false when none does.
pub fn forget_term(memory: &mut Memory, term: &str) -> Result<bool, CliError> {
    match memory.candidates(Kind::Term, term)?.as_slice() {
        [] => Ok(false),
        [one] => {
            memory.remove(*one)?;
            Ok(true)
        }
        many => Err(CliError::usage(format!(
            "{term:?} names {} entries; remove one with `eli5 memory remove <id>`",
            many.len()
        ))),
    }
}

/// The role the memory knows for each of `names`, in this series: the people it is sure of first,
/// so they settle who a shared name is.
pub fn known_roles(
    cfg: &Config,
    names: &[String],
    series: Option<&str>,
) -> Result<Vec<(String, String)>, CliError> {
    let memory = open_memory(cfg)?;
    let mut context = Context {
        series: series.map(str::to_string),
        present: Vec::new(),
    };
    let mut found: Vec<(usize, i64)> = Vec::new();
    for _ in 0..2 {
        for (at, name) in names.iter().enumerate() {
            if found.iter().any(|(i, _)| *i == at) {
                continue;
            }
            if let Resolution::Known { id } = memory.resolve(Kind::Person, name, &context)? {
                found.push((at, id));
                context.present.push(id);
            }
        }
    }
    found.sort_unstable();
    let mut roles = Vec::new();
    for (at, id) in found {
        if let Some(role) = memory.get(id)?.role() {
            roles.push((names[at].clone(), role.to_string()));
        }
    }
    Ok(roles)
}

/// The known people who did not attend but whose name, short name or nickname was said, as
/// `- Caro: Carolina Soto (Product Owner de Pagos)`: the model then knows who they are.
pub fn named_people(
    cfg: &Config,
    attendees: &[String],
    said: &[&str],
) -> Result<Vec<String>, CliError> {
    let text = format!(" {} ", fold_words(&said.join(" ")));
    let present: Vec<String> = attendees.iter().map(|a| fold_words(a)).collect();
    let people = open_memory(cfg)?.list(Some(Kind::Person), None, None)?;
    let forms_of =
        |p: &Concept| -> Vec<String> { p.forms.iter().map(|f| fold_words(&f.text)).collect() };
    let (here, away): (Vec<&Concept>, Vec<&Concept>) = people
        .iter()
        .partition(|p| forms_of(p).iter().any(|f| present.contains(f)));
    // A first name someone present also answers to says nothing about who else was meant.
    let taken: Vec<String> = here.iter().flat_map(|p| forms_of(p)).collect();
    let mut lines = Vec::new();
    for person in away {
        let Some(heard) = person.forms.iter().find(|f| {
            let form = fold_words(&f.text);
            !taken.contains(&form) && text.contains(&format!(" {form} "))
        }) else {
            continue;
        };
        let role = person.role().map_or(String::new(), |r| format!(" ({r})"));
        lines.push(format!("- {}: {}{role}", heard.text, person.name));
    }
    Ok(lines)
}

/// What a meeting's minutes taught, into the memory: the series and the people present tell
/// homonyms apart.
pub fn learn_minutes(
    cfg: &Config,
    session: &str,
    series: Option<&str>,
    at_ms: i64,
    learned: &Learned,
) -> Result<Learnt, CliError> {
    let lesson = Lesson {
        session: session.to_string(),
        series: series.map(str::to_string),
        at_ms,
        people: learned
            .people
            .iter()
            .map(|p| Met {
                name: p.name.clone(),
                also: p.also.clone(),
                role: p.role.clone(),
                attended: p.attended,
            })
            .collect(),
        terms: learned
            .terms
            .iter()
            .map(|t| (t.term.clone(), t.meaning.clone()))
            .collect(),
    };
    Ok(open_memory(cfg)?.learn(&lesson)?)
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

pub fn run(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    let words: Vec<&str> = ctx.args.positional.iter().map(String::as_str).collect();
    let mut memory = open_memory(&ctx.cfg)?;
    match words.as_slice() {
        ["list"] => list(ctx, io, &memory),
        ["show", id] => show(ctx, io, &memory, id),
        ["add", kind, name] => add(ctx, io, &mut memory, kind, name),
        ["edit", id] => edit(ctx, io, &mut memory, id),
        ["form", "add", id, text] => {
            let id = parse_id(id)?;
            memory.claim_form(id, text, now_ms())?;
            shown(ctx, io, &memory, id)
        }
        ["form", "split", id, text] => {
            let apart = memory.split_form(parse_id(id)?, text, now_ms())?;
            shown(ctx, io, &memory, apart)
        }
        ["form", "remove", id, text] => {
            let id = parse_id(id)?;
            memory.remove_form(id, text)?;
            shown(ctx, io, &memory, id)
        }
        ["form", "move", from, to, text] => {
            let to = parse_id(to)?;
            memory.move_form(parse_id(from)?, to, text)?;
            shown(ctx, io, &memory, to)
        }
        ["merge", keep, gone] => {
            let keep = parse_id(keep)?;
            memory.merge(keep, parse_id(gone)?, now_ms())?;
            shown(ctx, io, &memory, keep)
        }
        ["confirm", ids @ ..] if !ids.is_empty() => confirm(ctx, io, &mut memory, ids),
        ["remove", id] => {
            let id = parse_id(id)?;
            memory.remove(id)?;
            emit(
                io,
                ctx.json,
                format!("{id} removed\n"),
                json!({"removed": id}),
            )?;
            Ok(0)
        }
        ["resolve", kind, text] => resolve(ctx, io, &memory, kind, text),
        ["export"] => export(ctx, io, &memory),
        ["import", file] => import(ctx, io, &mut memory, file),
        ["adopt-people", file] => adopt_people(ctx, io, &mut memory, file),
        _ => Err(CliError::usage(USAGE)),
    }
}

fn parse_id(text: &str) -> Result<i64, CliError> {
    text.parse()
        .map_err(|_| CliError::usage(format!("{text:?} is not an entry id")))
}

fn parse_kind(text: &str) -> Result<Kind, CliError> {
    Kind::parse(text)
        .ok_or_else(|| CliError::usage(format!("{text:?}: the kind is term or person")))
}

fn line(concept: &Concept) -> String {
    let others: Vec<&str> = concept
        .forms
        .iter()
        .map(|f| f.text.as_str())
        .filter(|f| *f != concept.name)
        .collect();
    let mut out = format!("{:>5}  {}", concept.id, concept.name);
    if let Some(role) = concept.role() {
        out.push_str(&format!(" ({role})"));
    }
    if let Some(meaning) = &concept.meaning {
        out.push_str(&format!(" — {meaning}"));
    }
    if !others.is_empty() {
        out.push_str(&format!("  [also: {}]", others.join(", ")));
    }
    if concept.state == State::Proposed {
        out.push_str("  (proposed)");
    }
    out.push('\n');
    out
}

fn list(ctx: &Ctx<'_>, io: &mut Io<'_>, memory: &Memory) -> Result<u8, CliError> {
    let kind = ctx.args.value("kind").map(parse_kind).transpose()?;
    let state = ctx
        .args
        .value("state")
        .map(|s| {
            State::parse(s).ok_or_else(|| {
                CliError::usage(format!("{s:?}: the state is proposed or confirmed"))
            })
        })
        .transpose()?;
    let found = memory.list(kind, ctx.args.value("query"), state)?;
    let mut human: String = found.iter().map(line).collect();
    if human.is_empty() {
        human.push_str("nothing in the memory matches\n");
    }
    emit(io, ctx.json, human, json!({"entries": found}))?;
    Ok(0)
}

fn show(ctx: &Ctx<'_>, io: &mut Io<'_>, memory: &Memory, id: &str) -> Result<u8, CliError> {
    let id = parse_id(id)?;
    let concept = memory.get(id)?;
    let meetings = memory.sightings(id)?;
    let mut human = line(&concept);
    for seen in &meetings {
        human.push_str(&format!(
            "       {}{}{}\n",
            seen.session,
            seen.series
                .as_deref()
                .map_or(String::new(), |s| format!(" · {s}")),
            if seen.attended { "" } else { " · mentioned" }
        ));
    }
    emit(
        io,
        ctx.json,
        human,
        json!({"entry": concept, "meetings": meetings}),
    )?;
    Ok(0)
}

fn shown(ctx: &Ctx<'_>, io: &mut Io<'_>, memory: &Memory, id: i64) -> Result<u8, CliError> {
    let concept = memory.get(id)?;
    emit(io, ctx.json, line(&concept), json!({"entry": concept}))?;
    Ok(0)
}

fn add(
    ctx: &Ctx<'_>,
    io: &mut Io<'_>,
    memory: &mut Memory,
    kind: &str,
    name: &str,
) -> Result<u8, CliError> {
    let kind = parse_kind(kind)?;
    let at = now_ms();
    let id = memory.add(kind, name, ctx.args.value("meaning"), State::Confirmed, at)?;
    for form in ctx.args.value("forms").unwrap_or_default().split(',') {
        if !form.trim().is_empty() {
            memory.claim_form(id, form, at)?;
        }
    }
    if let Some(role) = ctx.args.value("role") {
        memory.set_role(id, role, at, State::Confirmed)?;
    }
    shown(ctx, io, memory, id)
}

fn edit(ctx: &Ctx<'_>, io: &mut Io<'_>, memory: &mut Memory, id: &str) -> Result<u8, CliError> {
    let id = parse_id(id)?;
    let at = now_ms();
    if let Some(name) = ctx.args.value("name") {
        memory.rename(id, name, at)?;
    }
    if let Some(meaning) = ctx.args.value("meaning") {
        memory.set_meaning(id, Some(meaning), at)?;
    }
    if let Some(role) = ctx.args.value("role") {
        memory.set_role(id, role, at, State::Confirmed)?;
    }
    memory.confirm(id, at)?;
    shown(ctx, io, memory, id)
}

fn confirm(
    ctx: &Ctx<'_>,
    io: &mut Io<'_>,
    memory: &mut Memory,
    ids: &[&str],
) -> Result<u8, CliError> {
    let ids = ids
        .iter()
        .map(|id| parse_id(id))
        .collect::<Result<Vec<_>, _>>()?;
    let at = now_ms();
    for &id in &ids {
        memory.confirm(id, at)?;
    }
    emit(
        io,
        ctx.json,
        format!("{} confirmed\n", ids.len()),
        json!({"confirmed": ids}),
    )?;
    Ok(0)
}

fn resolve(
    ctx: &Ctx<'_>,
    io: &mut Io<'_>,
    memory: &Memory,
    kind: &str,
    text: &str,
) -> Result<u8, CliError> {
    let present = ctx
        .args
        .value("present")
        .unwrap_or_default()
        .split(',')
        .filter(|p| !p.trim().is_empty())
        .map(|p| parse_id(p.trim()))
        .collect::<Result<Vec<_>, _>>()?;
    let context = Context {
        series: ctx.args.value("series").map(str::to_string),
        present,
    };
    let resolution = memory.resolve(parse_kind(kind)?, text, &context)?;
    emit(
        io,
        ctx.json,
        format!(
            "{}\n",
            serde_json::to_string(&resolution).unwrap_or_default()
        ),
        json!(resolution),
    )?;
    Ok(0)
}

fn export(ctx: &Ctx<'_>, io: &mut Io<'_>, memory: &Memory) -> Result<u8, CliError> {
    let backup = memory.export()?;
    match ctx.args.value("out") {
        Some(out) => {
            std::fs::write(out, &backup)
                .map_err(|e| CliError::runtime("io", format!("{out}: {e}")))?;
            emit(
                io,
                ctx.json,
                format!("written to {out}\n"),
                json!({"out": out}),
            )?;
        }
        None => writeln!(io.out, "{backup}")?,
    }
    Ok(0)
}

fn import(ctx: &Ctx<'_>, io: &mut Io<'_>, memory: &mut Memory, file: &str) -> Result<u8, CliError> {
    let text = std::fs::read_to_string(file).map_err(|e| {
        CliError::not_found(
            format!("{file}: {e}"),
            "pass a file `eli5 memory export` wrote",
        )
    })?;
    let count = memory.import(&text, now_ms())?;
    emit(
        io,
        ctx.json,
        format!("{count} entries imported\n"),
        json!({"imported": count}),
    )?;
    Ok(0)
}

/// The window's `people.json` (name to who they are), taken in once as confirmed people.
fn adopt_people(
    ctx: &Ctx<'_>,
    io: &mut Io<'_>,
    memory: &mut Memory,
    file: &str,
) -> Result<u8, CliError> {
    let text = std::fs::read_to_string(file).map_err(|e| {
        CliError::not_found(format!("{file}: {e}"), "pass the window's people.json")
    })?;
    let notes: std::collections::BTreeMap<String, String> =
        serde_json::from_str(&text).map_err(|e| CliError::usage(format!("{file}: {e}")))?;
    let people: Vec<Adopted> = notes
        .into_iter()
        .map(|(name, note)| Adopted {
            kind: Kind::Person,
            name,
            meaning: None,
            forms: Vec::new(),
            role: Some(note),
        })
        .collect();
    let taken = memory.adopt("people.json", &people, now_ms())?;
    let count = if taken { people.len() } else { 0 };
    emit(
        io,
        ctx.json,
        format!("{count} people taken in\n"),
        json!({"adopted": count}),
    )?;
    Ok(0)
}
