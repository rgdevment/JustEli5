use std::path::Path;

use mc_asr::{AsrError, FakeAsr, TranscribeOptions, Transcriber, transcribe_session};
use serde_json::json;

use crate::cli::{CliError, Ctx, Io, emit};

use super::vocabulary::prompt_terms;

#[cfg(feature = "whisper")]
pub const DEFAULT_MODEL: &str = "whisper-large-v3-turbo-q5_0";
/// Live recognition favours speed: a small model keeps up with the meeting.
#[cfg(feature = "whisper")]
pub const LIVE_MODEL: &str = "whisper-base";

#[cfg(feature = "whisper")]
fn number(ctx: &Ctx<'_>, name: &str, max: usize) -> Result<Option<usize>, CliError> {
    ctx.args
        .value(name)
        .map(|v| {
            v.parse::<usize>()
                .ok()
                .filter(|n| (1..=max).contains(n))
                .ok_or_else(|| {
                    CliError::usage(format!("--{name}: {v:?} is not between 1 and {max}"))
                })
        })
        .transpose()
}

/// What it takes to build an engine, checked up front and cheap to repeat (the
/// live recognition builds one per channel on its own thread).
#[derive(Clone)]
pub enum Recipe {
    Fake(Option<Vec<String>>),
    #[cfg(feature = "whisper")]
    Whisper {
        path: std::path::PathBuf,
        name: String,
        sha: Option<String>,
        opts: mc_asr::whisper::WhisperOptions,
    },
}

impl Recipe {
    pub fn build(&self) -> Result<Box<dyn Transcriber + Send>, AsrError> {
        match self {
            Self::Fake(None) => Ok(Box::new(FakeAsr::numbered())),
            Self::Fake(Some(script)) => Ok(Box::new(FakeAsr::new(script.clone()))),
            #[cfg(feature = "whisper")]
            Self::Whisper {
                path,
                name,
                sha,
                opts,
            } => Ok(Box::new(mc_asr::whisper::WhisperAsr::load(
                path,
                name,
                sha.clone(),
                opts.clone(),
            )?)),
        }
    }
}

#[cfg(feature = "whisper")]
fn whisper_recipe(
    ctx: &Ctx<'_>,
    model_flag: &str,
    default_model: &str,
) -> Result<Recipe, CliError> {
    use mc_asr::LanguagePolicy;
    use mc_asr::whisper::WhisperOptions;
    use mc_models::{ModelError, ModelStore};

    let name = ctx.args.value(model_flag).unwrap_or(default_model);
    let store = ModelStore::new(ctx.cfg.data_dir.join("models"));
    let path = store.path(name).map_err(|e| match e {
        ModelError::NotInstalled(_) | ModelError::Unknown(_) => CliError::not_found(
            e.to_string(),
            format!("download it first: mc models pull {name}"),
        ),
        other => CliError::runtime("io", other.to_string()),
    })?;
    let sha = store.verify(name).ok().map(|r| r.actual);
    let mut opts = WhisperOptions::default();
    if let Some(list) = ctx.args.value("languages") {
        opts.languages = LanguagePolicy::parse(list)
            .ok_or_else(|| CliError::usage("--languages takes two-letter codes, like es,en"))?;
    }
    if let Some(beam) = number(ctx, "beam", 8)? {
        opts.beam_size = beam;
    }
    if let Some(threads) = number(ctx, "threads", 64)? {
        opts.threads = threads;
    }
    Ok(Recipe::Whisper {
        path,
        name: name.to_string(),
        sha,
        opts,
    })
}

#[cfg(not(feature = "whisper"))]
fn whisper_recipe(
    _ctx: &Ctx<'_>,
    _model_flag: &str,
    _default_model: &str,
) -> Result<Recipe, CliError> {
    Err(CliError::usage(
        "this build has no Whisper engine; rebuild with `cargo build --release --features whisper` (or use the fake engine: fake[:<script.json>])",
    ))
}

/// `engine_flag`: `whisper`, `fake` or `fake:<script.json>`.
pub fn recipe(
    ctx: &Ctx<'_>,
    engine_flag: &str,
    model_flag: &str,
    default_model: &str,
) -> Result<Recipe, CliError> {
    match ctx.args.value(engine_flag).unwrap_or("whisper") {
        "fake" => Ok(Recipe::Fake(None)),
        spec if spec.starts_with("fake:") => {
            let path = &spec["fake:".len()..];
            let text = std::fs::read_to_string(path)
                .map_err(|e| CliError::not_found(format!("{path}: {e}"), "pass a script file"))?;
            // Validates the script now; each engine gets its own copy.
            FakeAsr::from_json(&text).map_err(|e| CliError::usage(e.to_string()))?;
            let value: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| CliError::usage(e.to_string()))?;
            let list = value.get("texts").unwrap_or(&value);
            let script = list
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|t| t.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            Ok(Recipe::Fake(Some(script)))
        }
        "whisper" => whisper_recipe(ctx, model_flag, default_model),
        other => Err(CliError::usage(format!(
            "unknown engine {other:?} (whisper, fake, fake:<script.json>)"
        ))),
    }
}

/// The engine factory the live recognition needs, validated before the capture starts.
pub fn live_factory(ctx: &Ctx<'_>) -> Result<mc_asr::EngineFactory, CliError> {
    #[cfg(feature = "whisper")]
    let default_model = LIVE_MODEL;
    #[cfg(not(feature = "whisper"))]
    let default_model = "";
    #[cfg_attr(not(feature = "whisper"), allow(unused_mut))]
    let mut recipe = recipe(ctx, "asr-engine", "asr-model", default_model)?;
    // Two engines run at once (mic and system): share the cores between them
    // unless the user chose a number.
    #[cfg(feature = "whisper")]
    if let Recipe::Whisper { opts, .. } = &mut recipe
        && ctx.args.value("threads").is_none()
    {
        opts.threads = (opts.threads / 2).max(1);
    }
    Ok(std::sync::Arc::new(move |_channel: &str| recipe.build()))
}

fn engine(ctx: &Ctx<'_>) -> Result<Box<dyn Transcriber>, CliError> {
    #[cfg(feature = "whisper")]
    let default_model = DEFAULT_MODEL;
    #[cfg(not(feature = "whisper"))]
    let default_model = "";
    let engine = recipe(ctx, "engine", "model", default_model)?
        .build()
        .map_err(|e| match e {
            AsrError::Input(m) => CliError::not_found(m, "run `mc models list`"),
            other => map_error(other),
        })?;
    Ok(engine)
}

fn map_error(error: AsrError) -> CliError {
    match error {
        AsrError::NoAudio(message) => {
            CliError::not_found(message, "pass a session recorded with --audio")
        }
        AsrError::Input(message) => CliError::runtime("input", message),
        AsrError::Engine(message) => CliError::runtime("asr", message),
        AsrError::Io { .. } => CliError::runtime("io", error.to_string()),
    }
}

pub fn run(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    let [dir] = ctx.args.positional.as_slice() else {
        return Err(CliError::usage(
            "usage: eli5 transcribe <dir> [--engine whisper|fake[:F]] [--model NAME] [--channels mic,system] [--language auto|es|en] [--prompt TEXT|--glossary FILE] [--force]",
        ));
    };
    if !Path::new(dir).join("manifest.json").is_file() {
        return Err(CliError::not_found(
            format!("{dir}: no manifest.json"),
            "pass a session directory",
        ));
    }
    let mut opts = TranscribeOptions {
        force: ctx.args.flag("force"),
        prompt: ctx.args.value("prompt").map(str::to_string),
        ..TranscribeOptions::default()
    };
    if opts.prompt.as_deref().is_some_and(|p| p.contains('\0')) {
        return Err(CliError::usage("--prompt cannot contain a NUL character"));
    }
    if ctx.args.value("glossary").is_some() && opts.prompt.is_some() {
        return Err(CliError::usage("--prompt and --glossary are exclusive"));
    }
    if opts.prompt.is_none() {
        // Terms only: the misspellings would teach the recogniser to write them.
        let terms = prompt_terms(ctx)?;
        opts.prompt = mc_asr::glossary_prompt(&terms, mc_asr::MAX_PROMPT_CHARS);
    }
    if let Some(list) = ctx.args.value("channels") {
        let channels: Vec<String> = list.split(',').map(|c| c.trim().to_string()).collect();
        if channels.is_empty()
            || channels
                .iter()
                .any(|c| !matches!(c.as_str(), "mic" | "system"))
        {
            return Err(CliError::usage(
                "--channels takes mic, system or mic,system",
            ));
        }
        let mut unique: Vec<String> = Vec::new();
        for channel in channels {
            if !unique.contains(&channel) {
                unique.push(channel);
            }
        }
        opts.channels = unique;
    }
    opts.language = match ctx.args.value("language") {
        None | Some("auto") => None,
        Some(code) if code.len() == 2 && code.chars().all(|c| c.is_ascii_lowercase()) => {
            Some(code.to_string())
        }
        Some(other) => {
            return Err(CliError::usage(format!(
                "--language: {other:?} is not auto or a two-letter code"
            )));
        }
    };
    let mut transcriber = engine(ctx)?;
    let run = transcribe_session(Path::new(dir), transcriber.as_mut(), &opts).map_err(map_error)?;

    let per_channel: Vec<String> = run
        .meta
        .channels
        .iter()
        .map(|c| format!("{} {}", c.ch, c.segments))
        .collect();
    let mut human = format!(
        "{}  {}  {}\n  {} segments ({})\n",
        run.asr_id,
        if run.reused { "reused" } else { "created" },
        run.dir.display(),
        run.segments.len(),
        per_channel.join(", ")
    );
    if !run.missing.is_empty() {
        human.push_str(&format!(
            "  note: no audio for {}\n",
            run.missing.join(", ")
        ));
    }
    let value = json!({
        "asr_id": run.asr_id,
        "path": run.dir.display().to_string(),
        "reused": run.reused,
        "missing": run.missing,
        "meta": run.meta,
    });
    emit(io, ctx.json, human, value)?;
    Ok(0)
}
