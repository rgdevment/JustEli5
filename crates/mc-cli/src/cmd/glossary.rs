//! `mc glossary`: the user's own vocabulary, and the review of what a meeting suggests adding.

use std::path::Path;

use mc_analyst::{Ledger, TranscriptSegment, flat, render_segment};
use mc_asr::Glossary;
use mc_engine::sidecar::read_term_fixes;
use mc_memory::Memory;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::analysis::{analysis_transcript, newest_analysis_id, pick_id, read_json};
use super::memory::{forget_term, open_memory, teach_term};
use super::vocabulary::{load_user_glossary, merged_glossary};
use crate::cli::{CliError, Ctx, Io, emit};

const USAGE: &str = "usage: eli5 glossary list | add <term> [--alias A] [--meaning M] | remove <term> | review <session> [--analysis ID] [--accept-all] [--json]";
const REVIEW_FILE: &str = "glossary_review.json";
const LIVE_LEDGER: &str = "analysis/live/ledger.json";
const MAX_EVIDENCE_LINES: usize = 3;
const MAX_EVIDENCE_CHARS: usize = 160;

pub fn run(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    let positional = ctx.args.positional.as_slice();
    let alias = ctx.args.value("alias");
    let meaning = ctx.args.value("meaning");
    let analysis = ctx.args.value("analysis");
    let accept_all = ctx.args.flag("accept-all");
    let misplaced =
        |name: &str, only: &str| CliError::usage(format!("{name} only applies to {only}"));
    if (alias.is_some() || meaning.is_some())
        && positional.first().map(String::as_str) != Some("add")
    {
        return Err(misplaced("--alias and --meaning", "add"));
    }
    if (analysis.is_some() || accept_all)
        && positional.first().map(String::as_str) != Some("review")
    {
        return Err(misplaced("--analysis and --accept-all", "review"));
    }
    match positional {
        [action] if action == "list" => list(ctx, io),
        [action, term] if action == "add" => add(ctx, io, term, alias, meaning),
        [action, term] if action == "remove" => remove(ctx, io, term),
        [action, session] if action == "review" => review(ctx, io, session),
        _ => Err(CliError::usage(USAGE)),
    }
}

fn list(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    let merged = merged_glossary(ctx)?;
    let user = load_user_glossary(&ctx.cfg)?;
    let aliases = merged.aliases();
    let meanings = merged.meanings();
    let mut human = String::new();
    let mut items = Vec::new();
    for term in merged.terms() {
        let heard: Vec<String> = aliases
            .iter()
            .filter(|(t, _)| *t == term)
            .flat_map(|(_, list)| list.clone())
            .collect();
        let own = user.contains(&term);
        human.push_str(&format!(
            "{term}{}{}\n",
            if heard.is_empty() {
                String::new()
            } else {
                format!("  (heard as: {})", heard.join(", "))
            },
            if own { "  [yours]" } else { "" }
        ));
        let meaning = meanings.iter().find(|(t, _)| *t == term).map(|(_, m)| m);
        items.push(json!({"term": term, "aliases": heard, "meaning": meaning, "user": own}));
    }
    if items.is_empty() {
        human.push_str("the glossary is empty; `mc glossary add <term>` starts it\n");
    }
    emit(io, ctx.json, human, json!({"glossary": items}))?;
    Ok(0)
}

fn add(
    ctx: &Ctx<'_>,
    io: &mut Io<'_>,
    term: &str,
    alias: Option<&str>,
    meaning: Option<&str>,
) -> Result<u8, CliError> {
    if term.trim().is_empty() {
        return Err(CliError::usage("the term is empty"));
    }
    teach_term(&mut open_memory(&ctx.cfg)?, term, alias, meaning)?;
    emit(
        io,
        ctx.json,
        format!("{term} added to the memory\n"),
        json!({"term": term, "alias": alias, "meaning": meaning}),
    )?;
    Ok(0)
}

fn remove(ctx: &Ctx<'_>, io: &mut Io<'_>, term: &str) -> Result<u8, CliError> {
    if !forget_term(&mut open_memory(&ctx.cfg)?, term)? {
        let hint = if merged_glossary(ctx)?.contains(term) {
            "it comes from the configured glossary file, not from yours"
        } else {
            "`mc glossary list` shows what is there"
        };
        return Err(CliError::not_found(
            format!("{term:?} is not in your glossary"),
            hint,
        ));
    }
    emit(
        io,
        ctx.json,
        format!("{term} removed\n"),
        json!({"removed": term}),
    )?;
    Ok(0)
}

/// What a meeting suggests adding to the glossary.
#[derive(Debug, Clone, Serialize)]
struct Candidate {
    key: String,
    /// `tui` (a correction the user accepted while recording), `term_fix` or `glossary_candidate`.
    source: &'static str,
    term: String,
    heard: Option<String>,
    evidence: Vec<String>,
}

impl Candidate {
    fn title(&self) -> String {
        match &self.heard {
            Some(heard) => format!("heard {heard:?} -> {:?}", self.term),
            None => format!("new term {:?}", self.term),
        }
    }

    fn apply(&self, memory: &mut Memory) -> Result<(), CliError> {
        teach_term(memory, &self.term, self.heard.as_deref(), None).map(|_| ())
    }
}

/// What the user already decided about this session's suggestions.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Decisions {
    #[serde(default)]
    accepted: Vec<String>,
    #[serde(default)]
    rejected: Vec<String>,
}

impl Decisions {
    fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join(REVIEW_FILE))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn save(&self, dir: &Path) -> Result<(), CliError> {
        let path = dir.join(REVIEW_FILE);
        let text = serde_json::to_string_pretty(self).expect("decisions serialise") + "\n";
        std::fs::write(&path, text)
            .map_err(|e| CliError::runtime("io", format!("{}: {e}", path.display())))
    }

    fn decided(&self, key: &str) -> bool {
        self.accepted.iter().chain(&self.rejected).any(|k| k == key)
    }
}

fn evidence_lines(ids: &[u64], segments: &[TranscriptSegment]) -> Vec<String> {
    ids.iter()
        .filter_map(|id| segments.iter().find(|s| s.id == *id))
        .take(MAX_EVIDENCE_LINES)
        .map(|s| {
            flat(&render_segment(s))
                .chars()
                .take(MAX_EVIDENCE_CHARS)
                .collect()
        })
        .collect()
}

/// The ledger the analyst suggestions come from: the chosen analysis, else the newest one,
/// else the live one; with the transcript its citations point into.
fn analyst_ledger(
    ctx: &Ctx<'_>,
    dir: &str,
) -> Result<Option<(Ledger, Vec<TranscriptSegment>)>, CliError> {
    let parent = Path::new(dir).join("analysis");
    let chosen = match ctx.args.value("analysis") {
        Some(selector) => Some(pick_id(&parent, "ledger.json", selector, "analysis")?),
        None => newest_analysis_id(dir),
    };
    if let Some(id) = chosen {
        let ledger = read_json(&parent.join(&id).join("ledger.json"))?;
        // A transcript that cannot be read only costs the quoted lines.
        let segments = analysis_transcript(dir, &id).map_or_else(|_| Vec::new(), |t| t.segments);
        return Ok(Some((ledger, segments)));
    }
    let live = Path::new(dir).join(LIVE_LEDGER);
    Ok(live
        .is_file()
        .then(|| read_json(&live))
        .transpose()?
        .map(|ledger| (ledger, Vec::new())))
}

fn candidates(
    ctx: &Ctx<'_>,
    dir: &str,
    known: &Glossary,
    decisions: &Decisions,
) -> Result<Vec<Candidate>, CliError> {
    let mut out: Vec<Candidate> = Vec::new();
    let mut push = |candidate: Candidate| {
        let useless = match &candidate.heard {
            Some(heard) => known.has_alias(&candidate.term, heard),
            None => known.contains(&candidate.term),
        };
        if !useless
            && !decisions.decided(&candidate.key)
            && !out.iter().any(|c| c.key == candidate.key)
        {
            out.push(candidate);
        }
    };
    for fix in read_term_fixes(Path::new(dir)) {
        push(Candidate {
            key: format!(
                "fix:{}->{}",
                fix.heard.to_lowercase(),
                fix.correct.to_lowercase()
            ),
            source: "tui",
            term: fix.correct,
            heard: Some(fix.heard),
            evidence: Vec::new(),
        });
    }
    if let Some((ledger, segments)) = analyst_ledger(ctx, dir)? {
        for fix in &ledger.term_fixes {
            push(Candidate {
                key: format!(
                    "fix:{}->{}",
                    fix.heard.to_lowercase(),
                    fix.correct.to_lowercase()
                ),
                source: "term_fix",
                term: fix.correct.clone(),
                heard: Some(fix.heard.clone()),
                evidence: evidence_lines(&fix.evidence, &segments),
            });
        }
        for candidate in &ledger.glossary_candidates {
            push(Candidate {
                key: format!("term:{}", candidate.term.to_lowercase()),
                source: "glossary_candidate",
                term: candidate.term.clone(),
                heard: None,
                evidence: evidence_lines(&candidate.evidence, &segments),
            });
        }
    }
    Ok(out)
}

enum Answer {
    Accept,
    Reject,
    Later,
    Quit,
}

fn ask(
    io: &mut Io<'_>,
    candidate: &Candidate,
    position: (usize, usize),
) -> Result<Answer, CliError> {
    writeln!(
        io.err,
        "[{}/{}] {}",
        position.0,
        position.1,
        candidate.title()
    )?;
    for line in &candidate.evidence {
        writeln!(io.err, "    {line}")?;
    }
    write!(
        io.err,
        "  add to your glossary? [s]i / [n]o / Enter later / [q]uit "
    )?;
    io.err.flush()?;
    let mut answer = String::new();
    let read = match io.input.as_mut() {
        Some(input) => input.read_line(&mut answer)?,
        None => 0,
    };
    Ok(match answer.trim().to_lowercase().as_str() {
        _ if read == 0 => Answer::Quit,
        "s" | "si" | "sí" | "y" | "yes" => Answer::Accept,
        "n" | "no" => Answer::Reject,
        "q" => Answer::Quit,
        _ => Answer::Later,
    })
}

fn review(ctx: &Ctx<'_>, io: &mut Io<'_>, dir: &str) -> Result<u8, CliError> {
    if !Path::new(dir).join("manifest.json").is_file() {
        return Err(CliError::not_found(
            format!("{dir}: no manifest.json"),
            "pass a session directory",
        ));
    }
    let accept_all = ctx.args.flag("accept-all");
    let list_only = ctx.json && !accept_all;
    if !accept_all && !list_only && io.input.is_none() {
        return Err(CliError::usage(
            "reviewing needs a terminal: pass --accept-all, or --json to list the suggestions",
        ));
    }
    let mut memory = open_memory(&ctx.cfg)?;
    let mut decisions = Decisions::load(Path::new(dir));
    let found = candidates(ctx, dir, &merged_glossary(ctx)?, &decisions)?;
    if list_only {
        let value = json!({"candidates": found});
        emit(io, true, String::new(), value)?;
        return Ok(0);
    }
    let (mut accepted, mut rejected) = (Vec::new(), Vec::new());
    let total = found.len();
    for (i, candidate) in found.iter().enumerate() {
        // What the user already accepted while recording needs no second question.
        let answer = if accept_all || candidate.source == "tui" {
            Answer::Accept
        } else {
            ask(io, candidate, (i + 1, total))?
        };
        match answer {
            Answer::Accept => {
                candidate.apply(&mut memory)?;
                decisions.accepted.push(candidate.key.clone());
                accepted.push(candidate);
            }
            Answer::Reject => {
                decisions.rejected.push(candidate.key.clone());
                rejected.push(candidate);
            }
            Answer::Later => {}
            Answer::Quit => break,
        }
    }
    if !accepted.is_empty() || !rejected.is_empty() {
        decisions.save(Path::new(dir))?;
    }
    let summary = |list: &[&Candidate]| -> Vec<Value> {
        list.iter()
            .map(|c| json!({"term": c.term, "heard": c.heard, "source": c.source}))
            .collect()
    };
    emit(
        io,
        ctx.json,
        format!(
            "{} suggestions: {} added, {} rejected, {} left for later\n",
            total,
            accepted.len(),
            rejected.len(),
            total - accepted.len() - rejected.len()
        ),
        json!({
            "suggestions": total,
            "accepted": summary(&accepted),
            "rejected": summary(&rejected),
        }),
    )?;
    Ok(0)
}
