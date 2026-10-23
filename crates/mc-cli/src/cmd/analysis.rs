//! What `mc analyze` and `mc minutes` share: which transcript to read, the
//! model, and the analysis folder under `<dir>/analysis/<id>/`.

use std::path::{Path, PathBuf};

use mc_analyst::{
    Analysis, AnalystConfig, Background, Ledger, LedgerEvent, PROMPT_VERSION, TranscriptSegment,
    analyze, collapse_repeats, load_fusion, load_run, render_events, transcript_hash,
};
use mc_asr::Glossary;
use mc_engine::sidecar::{display_name, read_speaker_names};
use mc_store::manifest::Manifest;
use mc_store::{FNV_OFFSET, fnv_extend};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cli::{CliError, Ctx};

use super::carry::{Memory, identity as carry_identity};
use super::vocabulary::{memory_homonyms, merged_glossary};

/// `analysis/live` belongs to the running capture, not to a transcript id.
const LIVE_FOLDER: &str = "live";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Fusion,
    Run,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Fusion => "fusion",
            Self::Run => "run",
        }
    }

    fn folder(self) -> &'static str {
        match self {
            Self::Fusion => "fused",
            Self::Run => "runs",
        }
    }

    fn file(self) -> &'static str {
        match self {
            Self::Fusion => "fused.jsonl",
            Self::Run => "segments.jsonl",
        }
    }
}

pub struct Transcript {
    pub kind: Kind,
    pub id: String,
    /// Of the exact words and times: the same id can hold other text after a regeneration.
    pub hash: String,
    pub segments: Vec<TranscriptSegment>,
}

impl Transcript {
    /// `fusion:<id>` or `run:<id>`.
    pub fn label(&self) -> String {
        format!("{}:{}", self.kind.name(), self.id)
    }
}

pub fn fnv_hex(text: &str) -> String {
    format!("{:016x}", fnv_extend(FNV_OFFSET, text.as_bytes()))
}

fn io_error(path: &Path, e: &std::io::Error) -> CliError {
    CliError::runtime("io", format!("{}: {e}", path.display()))
}

/// Folder names under `parent` that hold `file`, oldest first.
pub fn candidates(parent: &Path, file: &str) -> Vec<String> {
    mc_analyst::candidates(parent, file, &[LIVE_FOLDER])
}

/// An id or a unique prefix of one among the folders of `parent`.
pub fn pick_id(parent: &Path, file: &str, selector: &str, what: &str) -> Result<String, CliError> {
    let hits: Vec<String> = candidates(parent, file)
        .into_iter()
        .filter(|n| n.starts_with(selector))
        .collect();
    match hits.as_slice() {
        [] => Err(CliError::not_found(
            format!("no {what} starts with {selector:?}"),
            format!("see the folders under {}", parent.display()),
        )),
        [one] => Ok(one.clone()),
        _ => Err(CliError::usage(format!(
            "{selector:?} matches more than one {what}"
        ))),
    }
}

/// `--fuse ID` or `--run ID`; otherwise the newest fusion, else the newest run.
pub fn resolve_transcript(ctx: &Ctx<'_>, dir: &str) -> Result<Transcript, CliError> {
    let root = Path::new(dir);
    if !root.join("manifest.json").is_file() {
        return Err(CliError::not_found(
            format!("{dir}: no manifest.json"),
            "pass a session directory",
        ));
    }
    let (fuse, run) = (ctx.args.value("fuse"), ctx.args.value("run"));
    let (kind, id) = match (fuse, run) {
        (Some(_), Some(_)) => return Err(CliError::usage("--run and --fuse are exclusive")),
        (Some(selector), None) => (
            Kind::Fusion,
            pick_id(&root.join("fused"), "fused.jsonl", selector, "fusion")?,
        ),
        (None, Some(selector)) => (
            Kind::Run,
            pick_id(&root.join("runs"), "segments.jsonl", selector, "run")?,
        ),
        (None, None) if ctx.args.value("analysis").is_some() => {
            // An analysis names the transcript it was built from.
            let selector = ctx.args.value("analysis").unwrap_or_default();
            let parent = root.join("analysis");
            let folder = parent.join(pick_id(&parent, "ledger.json", selector, "analysis")?);
            parse_label(&read_meta(&folder)?.source)?
        }
        (None, None) => {
            let newest = |kind: Kind| {
                candidates(&root.join(kind.folder()), kind.file())
                    .pop()
                    .map(|id| (kind, id))
            };
            newest(Kind::Fusion)
                .or_else(|| newest(Kind::Run))
                .ok_or_else(|| {
                    CliError::not_found(
                        format!("{dir}: no fusion or run to read"),
                        "run `mc fuse <dir>` or `mc run <dir>` first",
                    )
                })?
        }
    };
    load_transcript(dir, kind, id)
}

fn load_transcript(dir: &str, kind: Kind, id: String) -> Result<Transcript, CliError> {
    let root = Path::new(dir);
    let path = root.join(kind.folder()).join(&id).join(kind.file());
    let mut segments = match kind {
        Kind::Fusion => load_fusion(&path),
        Kind::Run => load_run(&path),
    }
    .map_err(|e| CliError::runtime("io", e.to_string()))?;
    // The names the user gave while capturing, as display only.
    let names = read_speaker_names(root);
    for segment in &mut segments {
        segment.speaker = display_name(&names, &segment.speaker).to_string();
    }
    let segments = collapse_repeats(segments);
    if segments.is_empty() {
        return Err(CliError::runtime(
            "empty",
            format!("{}: the transcript has no segments", path.display()),
        ));
    }
    Ok(Transcript {
        kind,
        id,
        hash: transcript_hash(&segments),
        segments,
    })
}

/// The transcript an existing analysis was built from.
pub fn analysis_transcript(dir: &str, analysis_id: &str) -> Result<Transcript, CliError> {
    let folder = Path::new(dir).join("analysis").join(analysis_id);
    let (kind, id) = parse_label(&read_meta(&folder)?.source)?;
    load_transcript(dir, kind, id)
}

/// The newest analysis folder of a session (the live one aside).
pub fn newest_analysis_id(dir: &str) -> Option<String> {
    candidates(&Path::new(dir).join("analysis"), "ledger.json").pop()
}

fn parse_label(label: &str) -> Result<(Kind, String), CliError> {
    match label.split_once(':') {
        Some(("fusion", id)) => Ok((Kind::Fusion, id.to_string())),
        Some(("run", id)) => Ok((Kind::Run, id.to_string())),
        _ => Err(CliError::runtime(
            "io",
            format!("analysis source {label:?} is not fusion:<id> or run:<id>"),
        )),
    }
}

/// Which transcript, model and prompt an analysis came from, and whether every window worked.
#[derive(Debug, Serialize, Deserialize)]
pub struct AnalysisMeta {
    pub analysis_id: String,
    /// `fusion:<id>` or `run:<id>`.
    pub source: String,
    pub transcript_hash: String,
    pub prompt_version: String,
    pub provider: String,
    pub model: Option<String>,
    pub failed_windows: usize,
    pub complete: bool,
    /// The session whose open commitments the analyst was shown, when there were any.
    #[serde(default)]
    pub carried_from: Option<String>,
    #[serde(default)]
    pub carried: usize,
    /// Calls, tokens and time spent on the model; `null` tokens when it did not say.
    #[serde(default)]
    pub usage: Option<Value>,
}

fn read_meta(folder: &Path) -> Result<AnalysisMeta, CliError> {
    let path = folder.join("meta.json");
    if !path.is_file() {
        return Err(CliError::usage(format!(
            "{}: the analysis does not record its transcript; run `mc analyze --force`",
            folder.display()
        )));
    }
    read_json(&path)
}

pub fn read_optional(ctx: &Ctx<'_>, option: &str) -> Result<String, CliError> {
    let Some(file) = ctx.args.value(option) else {
        return Ok(String::new());
    };
    std::fs::read_to_string(file)
        .map_err(|e| CliError::not_found(format!("{file}: {e}"), format!("pass --{option} FILE")))
}

/// The glossary and project context; what a meeting inherits is added by the one who knows the session.
pub fn background(ctx: &Ctx<'_>) -> Result<Background, CliError> {
    background_with(ctx, &merged_glossary(ctx)?)
}

fn background_with(ctx: &Ctx<'_>, glossary: &Glossary) -> Result<Background, CliError> {
    Ok(Background {
        glossary: glossary.terms(),
        aliases: glossary.aliases(),
        homonyms: memory_homonyms(&ctx.cfg)?,
        project_context: read_optional(ctx, "context")?,
        me: ctx.cfg.fusion.me.clone(),
        ..Background::default()
    })
}

pub fn make_llm(ctx: &Ctx<'_>) -> Result<Box<dyn mc_llm::Llm>, CliError> {
    mc_llm::from_config(ctx.cfg.checked_llm()?).map_err(|e| CliError::Runtime {
        code: "llm",
        message: e.to_string(),
        hint: Some("`mc doctor` checks the llm settings".to_string()),
    })
}

/// The model settings that shape an answer, for ids.
pub fn llm_identity(ctx: &Ctx<'_>) -> String {
    let llm = &ctx.cfg.llm;
    format!("{}|{}", llm.provider, llm.model.as_deref().unwrap_or(""))
}

/// Writes `files` into `out` as one step (see [`mc_analyst::write_folder`]).
pub fn write_folder(out: &Path, files: &[(&str, String)], marker: &str) -> Result<(), CliError> {
    mc_analyst::write_folder(out, files, marker).map_err(|e| CliError::runtime("io", e.to_string()))
}

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, CliError> {
    let text = std::fs::read_to_string(path).map_err(|e| io_error(path, &e))?;
    serde_json::from_str(&text)
        .map_err(|e| CliError::runtime("io", format!("{}: {e}", path.display())))
}

pub fn pretty<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).expect("a value serialises") + "\n"
}

pub struct Analyzed {
    pub id: String,
    pub dir: PathBuf,
    pub reused: bool,
    pub ledger: Ledger,
    pub events: Vec<LedgerEvent>,
    /// Every window got an answer; otherwise the ledger is partial and not reused.
    pub complete: bool,
}

impl Analyzed {
    pub fn failed(&self) -> usize {
        self.events.iter().filter(|e| e.error.is_some()).count()
    }

    pub fn summary(&self) -> Value {
        ledger_counts(&self.ledger)
    }
}

/// How many items of each kind a ledger holds.
pub fn ledger_counts(l: &Ledger) -> Value {
    json!({
        "topics": l.topics.len(),
        "decisions": l.decisions.len(),
        "commitments": l.commitments.len(),
        "open_questions": l.open_questions.len(),
        "risks": l.risks.len(),
        "term_fixes": l.term_fixes.len(),
        "glossary_candidates": l.glossary_candidates.len(),
        "carried": l.carried.len(),
    })
}

fn read_events(path: &Path) -> Vec<LedgerEvent> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// `«Daily de equipo», 2026-10-21.`: the anchor for "mañana" and for the topics.
fn meeting_line(dir: &str) -> String {
    let Ok(manifest) = Manifest::load(Path::new(dir).join("manifest.json")) else {
        return String::new();
    };
    let date: String = manifest.started_utc_iso.chars().take(10).collect();
    match manifest.meeting_name {
        Some(name) => format!("«{name}», {date}."),
        None => format!("Reunión del {date}."),
    }
}

/// Everyone who speaks, in order of appearance.
pub fn speakers(segments: &[TranscriptSegment]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in segments {
        if !s.speaker.trim().is_empty() && !out.contains(&s.speaker) {
            out.push(s.speaker.clone());
        }
    }
    out
}

/// Analyses a transcript into `<dir>/analysis/<id>/{ledger.json,ledger_events.jsonl,meta.json}`.
/// The id follows the transcript's words, the model and the prompt, so a complete result is
/// reused; an incomplete one (a window failed) is redone.
pub fn analyze_transcript(
    ctx: &Ctx<'_>,
    dir: &str,
    transcript: &Transcript,
    force: bool,
    memory: &Memory,
) -> Result<Analyzed, CliError> {
    let mut background = background_with(ctx, &memory.glossary)?;
    background.carried.clone_from(&memory.carry.carried);
    background.meeting = meeting_line(dir);
    background.participants = speakers(&transcript.segments);
    let cfg = AnalystConfig::offline();
    let identity = format!(
        "{PROMPT_VERSION}|{}|{}|{}|{cfg:?}|{}|{}|{}|{}|{}",
        transcript.label(),
        transcript.hash,
        llm_identity(ctx),
        fnv_hex(&memory.glossary.fingerprint()),
        fnv_hex(&background.project_context),
        fnv_hex(&carry_identity(&background.carried)),
        background.me.as_deref().unwrap_or_default(),
        background.meeting,
    );
    let id = format!("a{}", fnv_hex(&identity));
    let out = Path::new(dir).join("analysis").join(&id);
    let events_path = out.join("ledger_events.jsonl");
    let reusable = !force
        && out.join("ledger.json").is_file()
        && read_meta(&out).is_ok_and(|m| m.complete && m.transcript_hash == transcript.hash);
    if reusable {
        return Ok(Analyzed {
            id,
            ledger: read_json(&out.join("ledger.json"))?,
            events: read_events(&events_path),
            dir: out,
            reused: true,
            complete: true,
        });
    }
    let llm = make_llm(ctx)?;
    let Analysis { ledger, events } =
        analyze(llm.as_ref(), &transcript.segments, &background, &cfg);
    let failed: Vec<&LedgerEvent> = events.iter().filter(|e| e.error.is_some()).collect();
    if !events.is_empty() && failed.len() == events.len() {
        // Nothing is written: an earlier result, good or partial, stays as it was.
        let last = failed
            .last()
            .and_then(|e| e.error.clone())
            .unwrap_or_default();
        return Err(CliError::Runtime {
            code: "llm",
            message: format!("every window failed: {last}"),
            hint: Some("`mc doctor` checks the llm settings".to_string()),
        });
    }
    let meta = AnalysisMeta {
        analysis_id: id.clone(),
        source: transcript.label(),
        transcript_hash: transcript.hash.clone(),
        prompt_version: PROMPT_VERSION.to_string(),
        provider: ctx.cfg.llm.provider.clone(),
        model: ctx.cfg.llm.model.clone(),
        failed_windows: failed.len(),
        complete: failed.is_empty(),
        carried_from: memory.carry.previous.clone(),
        carried: background.carried.len(),
        usage: Some(mc_llm::usage_json(&llm.usage())),
    };
    write_folder(
        &out,
        &[
            ("ledger_events.jsonl", render_events(&events)),
            ("ledger.json", pretty(&ledger)),
            ("meta.json", pretty(&meta)),
        ],
        "ledger.json",
    )?;
    let out = crate::cli::canonical(out);
    Ok(Analyzed {
        id,
        dir: out,
        reused: false,
        ledger,
        events,
        complete: meta.complete,
    })
}

/// `--analysis ID` (an existing one, built from this very transcript), or the analysis of
/// this transcript, made if missing.
pub fn resolve_analysis(
    ctx: &Ctx<'_>,
    dir: &str,
    transcript: &Transcript,
    memory: &Memory,
) -> Result<Analyzed, CliError> {
    let Some(selector) = ctx.args.value("analysis") else {
        return analyze_transcript(ctx, dir, transcript, false, memory);
    };
    let parent = Path::new(dir).join("analysis");
    let id = pick_id(&parent, "ledger.json", selector, "analysis")?;
    let out = parent.join(&id);
    let meta = read_meta(&out)?;
    if meta.transcript_hash != transcript.hash {
        return Err(CliError::usage(format!(
            "analysis {id} was built from another transcript ({}), not {}; pass the matching --fuse/--run or analyze again",
            meta.source,
            transcript.label()
        )));
    }
    Ok(Analyzed {
        ledger: read_json(&out.join("ledger.json"))?,
        events: read_events(&out.join("ledger_events.jsonl")),
        dir: out,
        id,
        reused: true,
        complete: meta.complete,
    })
}
