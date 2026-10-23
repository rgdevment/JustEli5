//! Entry point shared by the binary and the tests: `run(args, env, io) -> exit code`.
//!
//! Exit codes: 0 ok, 1 negative result (verify failed, compare differs, a
//! doctor check failed, an import with failed sessions), 2 usage or config
//! error, 3 runtime error (I/O, store, corpus, not found).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::args::{Parsed, Spec, parse};
use crate::cmd;
use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Windows,
    Mac,
    Linux,
}

impl Os {
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Mac
        } else {
            Self::Linux
        }
    }
}

/// Everything the library reads from the process, injectable for tests.
#[derive(Debug, Clone)]
pub struct Env {
    pub vars: BTreeMap<String, String>,
    pub os: Os,
    pub cwd: PathBuf,
    /// Whether stdin is an interactive terminal (consent prompt, `q` to stop).
    pub stdin_tty: bool,
    /// Whether stdout is an interactive terminal (the TUI needs both).
    pub stdout_tty: bool,
}

impl Env {
    pub fn from_process() -> Self {
        Self {
            vars: std::env::vars().collect(),
            os: Os::current(),
            cwd: std::env::current_dir().unwrap_or_default(),
            stdin_tty: std::io::IsTerminal::is_terminal(&std::io::stdin()),
            stdout_tty: std::io::IsTerminal::is_terminal(&std::io::stdout()),
        }
    }

    /// Empty values count as unset.
    pub fn var(&self, name: &str) -> Option<&str> {
        let found = self.vars.get(name).or_else(|| {
            // Windows variable names are case-insensitive (`Path` vs `PATH`).
            (self.os == Os::Windows)
                .then(|| {
                    self.vars
                        .iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case(name))
                        .map(|(_, value)| value)
                })
                .flatten()
        });
        found.map(String::as_str).filter(|v| !v.is_empty())
    }
}

pub struct Io<'a> {
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
    /// Where interactive answers come from; `None` when there is no terminal.
    pub input: Option<Box<dyn std::io::BufRead + Send>>,
}

#[derive(Debug)]
pub enum CliError {
    /// The reader of our output went away (`mc export | head`): not an error.
    BrokenPipe,
    Usage(String),
    Config(String),
    Runtime {
        code: &'static str,
        message: String,
        hint: Option<String>,
    },
}

impl CliError {
    pub fn usage(message: impl Into<String>) -> Self {
        Self::Usage(message.into())
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::Config(message.into())
    }

    pub fn runtime(code: &'static str, message: impl Into<String>) -> Self {
        Self::Runtime {
            code,
            message: message.into(),
            hint: None,
        }
    }

    pub fn not_found(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::Runtime {
            code: "not_found",
            message: message.into(),
            hint: Some(hint.into()),
        }
    }

    pub fn exit_code(&self) -> u8 {
        match self {
            Self::BrokenPipe => 0,
            Self::Usage(_) | Self::Config(_) => 2,
            Self::Runtime { .. } => 3,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::BrokenPipe => "broken pipe",
            Self::Usage(m) | Self::Config(m) => m,
            Self::Runtime { message, .. } => message,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::BrokenPipe => "broken_pipe",
            Self::Usage(_) => "usage",
            Self::Config(_) => "config",
            Self::Runtime { code, .. } => code,
        }
    }

    fn hint(&self) -> Option<&str> {
        match self {
            Self::Runtime { hint, .. } => hint.as_deref(),
            _ => None,
        }
    }
}

impl From<mc_store::StoreError> for CliError {
    fn from(err: mc_store::StoreError) -> Self {
        Self::runtime("store", err.to_string())
    }
}

impl From<mc_engine::replay::ReplayError> for CliError {
    fn from(err: mc_engine::replay::ReplayError) -> Self {
        Self::runtime("io", err.to_string())
    }
}

impl From<std::io::Error> for CliError {
    fn from(err: std::io::Error) -> Self {
        if err.kind() == std::io::ErrorKind::BrokenPipe {
            return Self::BrokenPipe;
        }
        Self::runtime("io", err.to_string())
    }
}

/// What a command receives.
pub struct Ctx<'a> {
    pub cfg: Config,
    pub args: Parsed,
    pub json: bool,
    pub env: &'a Env,
}

pub const USAGE: &str = "\
eli5: meeting capture

usage: eli5 <command> [options]

commands:
  replay <dir> --verify [--strict]    replay a recorded session and compare with the recording
  import [<dir|root>]                 index sessions into the local database
  run <dir> [--force] [--flush auto|manifest-only] [--debounce S] [--policy v1_legacy|v2]
          [--fuse [--asr ID] [--me N] [--glossary F]]
                                      derive transcript, segments and report from a session
                                      (--fuse: also merge in the session's ASR run)
  fuse <dir> [--asr ID] [--me NAME] [--glossary FILE] [--force]
                                      merge Teams captions with the local ASR into fused/<id>/
  transcribe <dir> [--engine whisper|fake[:F]] [--model NAME] [--channels mic,system]
             [--language auto|es|en] [--languages es,en] [--prompt TEXT] [--glossary FILE]
             [--beam N] [--threads N] [--force]
                                      recognise the recorded audio locally into asr/<id>/asr.jsonl
  analyze <session> [--run ID | --fuse ID] [--llm-provider P] [--llm-model M] [--glossary F] [--context F] [--force]
                                      have an LLM build the ledger of topics, decisions and commitments
                                      (every item cites segment ids) into analysis/<id>/
  minutes <session> [--run ID | --fuse ID] [--analysis ID] [--notes F] [--llm-provider P]
          [--llm-model M] [--glossary F] [--context F] [--force]
                                      write the minutes in Spanish, checked against their evidence, into
                                      minutes/<id>/ (the newest fusion is read, else the newest run);
                                      without --notes it uses what was noted while capturing (notes.jsonl)
  series list | show <key> | set <session> <key>
                                      meetings that recur under one name: the series, its sessions and
                                      the commitments still open after the last one with minutes
  glossary list | add <term> [--alias A] | remove <term>
  glossary review <session> [--analysis ID] [--accept-all]
                                      your own vocabulary (the terms of the memory, merged with the
                                      configured glossary); review asks about the terms and corrections
                                      a meeting suggests (--accept-all adds them all, --json lists them)
  memory list [--kind term|person] [--query Q] [--state S] | show <id> | add term|person <name>
         [--meaning M] [--forms A,B] [--role R] | edit <id> [--name N] [--meaning M] [--role R]
  memory form add|remove|split <id> <text> | form move <from> <to> <text> | merge <keep> <gone>
  memory confirm <id>... | remove <id> | resolve term|person <text> [--series S] [--present IDS]
  memory export [--out FILE] | import <file> | adopt-people <people.json>
                                      the terms and people known between meetings
                                      (<data dir>/memory.sqlite): each with the forms it is heard or
                                      written in, a homonym told apart by the meetings it came up in
  eval <dir> --reference FILE [--asr ID] [--variants captions,asr,fused] [--me N] [--glossary F]
                                      score captions, ASR and the fused transcript against a reference
  llm models [--llm-provider P] [--base-url U]
                                      the model names the AI provider accepts (copilot and a command
                                      do not say: there the name is written as they expect it)
  models list | pull <name> [--url U] [--sha256 H] [--force] | verify <name> | path <name> | remove <name>
                                      speech-recognition models (downloaded only on request)
  prune --audio [<session>] [--older-than DAYS] [--include-unfused] [--include-unended] [--apply]
                                      delete recorded audio (dry run unless --apply); keeps transcripts
  status [<session>]                  list sessions, or the runs of one session
  mcp                                 MCP server over stdio (JSON-RPC): lets Claude Code or Copilot CLI read and
                                      act on the meetings (`claude mcp add justeli5 -- eli5 mcp`)
  export <session> --format txt|md|json [--out PATH|DIR|-] [--save] [--force] [--run ID]
  export <session> --tisty [--tisty-folder F] [--tisty-list L] [--yes] [--dry-run]
                                      publish the newest minutes (made first if missing) and one task
                                      per commitment to Tisty; shows the plan and asks unless --yes/--json
  doctor [--tisty] [--uia] [--dump FILE [--window N] [--raw]] [--source fake-uia:F]
         [--audio [--audio-seconds S] [--audio-source wav:M[,S]] [--mic-device D] [--system-device D]]
         [--llm-provider P] [--llm-model M]
                                      check configuration and environment; inspect/dump UI trees;
                                      listen to the audio channels (levels, echo)
  detect                              which meeting platforms have a window open now and whether
                                      their captions are on (exit 1 when none is found)
  compare <runA> <runB>               compare two runs
  tui [capture options]               same as capture, in a full-screen terminal UI
  capture --source fake:<dir> [--consent] [--no-record] [--name N] [--poll-ms N] [--poll-timeout-ms N] [--until S]
          [--audio [--consent-audio] [--audio-source wav:<mic.wav>[,<system.wav>]]
           [--mic-device D] [--system-device D]
           [--live-asr [--asr-engine whisper|fake[:F]] [--asr-model NAME]]]
          [--analyst [--llm-provider P] [--llm-model M] [--glossary F] [--context F]]
          [--stdin-commands]
                                      record a meeting's captions (and audio with --audio); the session
                                      folder appears when the meeting is found (NDJSON `session` event).
                                      Stop with `q`, --until, or a STOP file in the session folder (or in
                                      the corpus folder while no meeting was found).
                                      --analyst: an LLM keeps a live ledger of topics, decisions and
                                      commitments in <session>/analysis/live/ (never blocks the capture)
                                      --stdin-commands: read `n <text>`, `m` and `q` from a piped stdin
                                      too (for a GUI); the end of input stops the capture

global options: --config PATH  --data-dir PATH  --json  -h/--help  --version
exit codes: 0 ok, 1 negative result, 2 usage/config error, 3 runtime error
";

fn command_spec(name: &str) -> Option<Spec> {
    Some(match name {
        "replay" => Spec {
            values: &[],
            flags: &["verify", "strict"],
        },
        "import" => Spec {
            values: &[],
            flags: &[],
        },
        "fuse" => Spec {
            values: &["asr", "me", "glossary"],
            flags: &["force"],
        },
        "analyze" => Spec {
            values: &[
                "run",
                "fuse",
                "llm-provider",
                "llm-model",
                "glossary",
                "context",
            ],
            flags: &["force"],
        },
        "minutes" => Spec {
            values: &[
                "run",
                "fuse",
                "analysis",
                "notes",
                "llm-provider",
                "llm-model",
                "glossary",
                "context",
            ],
            flags: &["force"],
        },
        "run" => Spec {
            values: &["flush", "debounce", "policy", "asr", "me", "glossary"],
            flags: &["force", "fuse"],
        },
        "eval" => Spec {
            values: &["reference", "asr", "variants", "me", "glossary"],
            flags: &[],
        },
        "series" => Spec {
            values: &[],
            flags: &[],
        },
        "llm" => Spec {
            values: &["llm-provider", "llm-model", "base-url"],
            flags: &[],
        },
        "glossary" => Spec {
            values: &["alias", "meaning", "analysis"],
            flags: &["accept-all"],
        },
        "memory" => Spec {
            values: &[
                "kind", "query", "state", "meaning", "forms", "role", "name", "series", "present",
                "out",
            ],
            flags: &[],
        },
        "models" => Spec {
            values: &["url", "sha256"],
            flags: &["force"],
        },
        "transcribe" => Spec {
            values: &[
                "engine",
                "model",
                "channels",
                "language",
                "languages",
                "prompt",
                "glossary",
                "beam",
                "threads",
            ],
            flags: &["force"],
        },
        "prune" => Spec {
            values: &["older-than"],
            flags: &["audio", "apply", "include-unfused", "include-unended"],
        },
        "mcp" => Spec {
            values: &[],
            flags: &[],
        },
        "status" => Spec {
            values: &[],
            flags: &[],
        },
        "export" => Spec {
            values: &["format", "out", "run", "tisty-folder", "tisty-list"],
            flags: &["save", "force", "tisty", "yes", "dry-run"],
        },
        "capture" => Spec {
            values: &[
                "source",
                "name",
                "poll-ms",
                "poll-timeout-ms",
                "until",
                "audio-source",
                "mic-device",
                "system-device",
                "asr-engine",
                "asr-model",
                "llm-provider",
                "llm-model",
                "glossary",
                "context",
            ],
            flags: &[
                "consent",
                "no-record",
                "tui",
                "audio",
                "consent-audio",
                "live-asr",
                "analyst",
                "stdin-commands",
            ],
        },
        "tui" => Spec {
            values: &[
                "source",
                "name",
                "poll-ms",
                "poll-timeout-ms",
                "until",
                "audio-source",
                "mic-device",
                "system-device",
                "asr-engine",
                "asr-model",
                "llm-provider",
                "llm-model",
                "glossary",
                "context",
            ],
            flags: &[
                "consent",
                "no-record",
                "audio",
                "consent-audio",
                "live-asr",
                "analyst",
            ],
        },
        "doctor" => Spec {
            values: &[
                "dump",
                "window",
                "source",
                "max-depth",
                "max-nodes",
                "platform",
                "audio-source",
                "mic-device",
                "system-device",
                "audio-seconds",
                "llm-provider",
                "llm-model",
            ],
            flags: &["uia", "raw", "force", "audio", "ax-prompt", "tisty"],
        },
        "detect" => Spec {
            values: &["source", "max-nodes"],
            flags: &["ax-prompt"],
        },
        "compare" => Spec {
            values: &[],
            flags: &[],
        },
        _ => return None,
    })
}

/// The first token that is not an option (nor the value of a global one).
fn split_command(args: &[String]) -> Option<(usize, &String)> {
    let mut skip_next = false;
    for (index, arg) in args.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--config" || arg == "--data-dir" {
            skip_next = true;
        } else if !arg.starts_with('-') {
            return Some((index, arg));
        }
    }
    None
}

fn print_error(io: &mut Io<'_>, err: &CliError, json_output: bool, command: &str) {
    if matches!(err, CliError::BrokenPipe) {
        return;
    }
    if json_output {
        let mut value = json!({"error": {"code": err.code(), "message": err.message()}});
        if let Some(hint) = err.hint() {
            value["error"]["hint"] = json!(hint);
        }
        let _ = writeln!(io.err, "{value}");
    } else {
        let _ = writeln!(io.err, "eli5 {command}: {}", err.message());
        if let Some(hint) = err.hint() {
            let _ = writeln!(io.err, "hint: {hint}");
        }
    }
}

/// Arguments before a `--`: later ones are operands, never options.
fn options_only(args: &[String]) -> &[String] {
    let end = args.iter().position(|a| a == "--").unwrap_or(args.len());
    &args[..end]
}

pub fn run(args: &[String], env: &Env, io: &mut Io<'_>) -> u8 {
    let json_output = options_only(args).iter().any(|a| a == "--json");
    let command = split_command(args).map(|(_, c)| c.clone());
    let label = command.clone().unwrap_or_default();
    match dispatch(args, command.as_deref(), env, io) {
        Ok(code) => code,
        Err(err) => {
            print_error(io, &err, json_output, &label);
            err.exit_code()
        }
    }
}

fn dispatch(
    args: &[String],
    command: Option<&str>,
    env: &Env,
    io: &mut Io<'_>,
) -> Result<u8, CliError> {
    if options_only(args).iter().any(|a| a == "--version") {
        writeln!(io.out, "eli5 {}", env!("CARGO_PKG_VERSION"))?;
        return Ok(0);
    }
    let wants_help = options_only(args)
        .iter()
        .any(|a| a == "-h" || a == "--help");
    let Some(command) = command else {
        if wants_help || args.is_empty() {
            write!(io.out, "eli5 {}\n\n{USAGE}", env!("CARGO_PKG_VERSION"))?;
            return Ok(0);
        }
        return Err(CliError::usage("missing command (try `mc --help`)"));
    };
    if command == "help" || wants_help {
        write!(io.out, "{USAGE}")?;
        return Ok(0);
    }
    let Some(spec) = command_spec(command) else {
        return Err(CliError::usage(format!("unknown command {command:?}")));
    };
    // The command name is the first positional; drop it before parsing.
    let position = split_command(args).map_or(0, |(i, _)| i);
    let mut rest: Vec<String> = args.to_vec();
    rest.remove(position);
    let parsed = parse(&rest, &spec)?;
    // The raw scan above only serves errors raised before parsing.
    let json_output = parsed.flag("json");
    // `replay` needs neither a data dir nor a store.
    let cfg = match Config::load(env, &parsed, command != "replay") {
        Ok(cfg) => cfg,
        // Doctor exists to diagnose a broken configuration: report, don't abort.
        Err(err) if command == "doctor" => {
            return cmd::doctor::broken_config(&err, io, json_output);
        }
        Err(err) => return Err(err),
    };
    let ctx = Ctx {
        cfg,
        args: parsed,
        json: json_output,
        env,
    };
    match command {
        "replay" => cmd::replay::run(&ctx, io),
        "import" => cmd::import::run(&ctx, io),
        "run" => cmd::run::run(&ctx, io),
        "fuse" => cmd::fuse::run(&ctx, io),
        "analyze" => cmd::analyze::run(&ctx, io),
        "minutes" => cmd::minutes::run(&ctx, io),
        "transcribe" => cmd::transcribe::run(&ctx, io),
        "models" => cmd::models::run(&ctx, io),
        "series" => cmd::series::run(&ctx, io),
        "glossary" => cmd::glossary::run(&ctx, io),
        "llm" => cmd::llm::run(&ctx, io),
        "memory" => cmd::memory::run(&ctx, io),
        "eval" => cmd::eval::run(&ctx, io),
        "prune" => cmd::prune::run(&ctx, io),
        "status" => cmd::status::run(&ctx, io),
        "mcp" => cmd::mcp::run(&ctx, io),
        "export" => cmd::export::run(&ctx, io),
        "doctor" => cmd::doctor::run(&ctx, io),
        "detect" => cmd::detect::run(&ctx, io),
        "capture" if ctx.args.flag("tui") => cmd::tui::run(ctx, io),
        "capture" => cmd::capture::run(ctx, io),
        "tui" => cmd::tui::run(ctx, io),
        "compare" => cmd::compare::run(&ctx, io),
        _ => unreachable!("command_spec accepted {command}"),
    }
}

/// Prints one JSON document, or the human text.
pub fn emit(
    io: &mut Io<'_>,
    json_output: bool,
    human: String,
    value: Value,
) -> Result<(), CliError> {
    if json_output {
        writeln!(io.out, "{value}")?;
    } else {
        write!(io.out, "{human}")?;
    }
    Ok(())
}

/// A path without the `\\?\` prefix Windows adds to canonical paths: other programs, and
/// a session lookup by folder, do not take it back.
pub fn plain_path(path: &str) -> String {
    match path.strip_prefix(r"\\?\UNC\") {
        Some(share) => format!(r"\\{share}"),
        None => path.strip_prefix(r"\\?\").unwrap_or(path).to_string(),
    }
}

/// `fs::canonicalize`, kept as given when it fails, without the `\\?\` prefix.
pub fn canonical(path: PathBuf) -> PathBuf {
    let full = std::fs::canonicalize(&path).unwrap_or(path);
    PathBuf::from(plain_path(&full.to_string_lossy()))
}

/// The session id of a selector that is a session folder (with or without the `\\?\`
/// prefix); `None` for an id or a prefix.
pub fn session_id_of_dir(selector: &str) -> Option<String> {
    let dir = PathBuf::from(plain_path(selector));
    mc_store::manifest::Manifest::load(dir.join("manifest.json"))
        .ok()
        .map(|m| m.session_id)
}

/// Opens (creating the data directory) the SQLite index.
pub fn open_store(cfg: &Config) -> Result<mc_store::Store, CliError> {
    std::fs::create_dir_all(&cfg.data_dir)
        .map_err(|e| CliError::runtime("io", format!("{}: {e}", cfg.data_dir.display())))?;
    Ok(mc_store::Store::open(cfg.index_path())?)
}

/// Resolves an id or unique prefix; an ambiguous one is a usage error.
pub fn resolve_one(kind: &str, input: &str, candidates: Vec<String>) -> Result<String, CliError> {
    if input.is_empty() {
        return Err(CliError::usage(format!("empty {kind} id")));
    }
    match candidates.as_slice() {
        [] => Err(CliError::not_found(
            format!("no {kind} matches {input:?}"),
            "mc status lists what is indexed",
        )),
        [one] => Ok(one.clone()),
        many => Err(CliError::usage(format!(
            "{kind} prefix {input:?} is ambiguous: {}",
            many.join(", ")
        ))),
    }
}
