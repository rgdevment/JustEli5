//! Configuration: flags > environment > config file > defaults.
//!
//! The file is JSON (`config.json`). Unknown keys are an error, so a typo is
//! never silently ignored. `Config::sources` records which layer set each field
//! for `mc doctor`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mc_engine::policy::default_policy;
use mc_engine::runs::{FlushMode, RunConfig, parse_policy, policy_name};
use mc_llm::LlmConfig;
use mc_reconcile::{Policy, ReconcilerConfig};
use mc_store::{SeriesRule, user_key};
use serde::Deserialize;

use crate::args::Parsed;
use crate::cli::{CliError, Env, Os};

const APP_DIR: &str = "justeli5";
/// The folder names the app had before, newest first; `legacy` moves the first one found.
pub const LEGACY_APP_DIRS: [&str; 2] = ["onmuted", "meet-copilot"];
const PLACEHOLDERS: [&str; 5] = [
    "session_id",
    "meeting_name",
    "date",
    "transcript_md",
    "commitments_json",
];

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    version: Option<u32>,
    data_dir: Option<String>,
    output_dir: Option<String>,
    run: Option<FileRun>,
    capture: Option<FileCapture>,
    tisty: Option<FileTisty>,
    fusion: Option<FileFusion>,
    llm: Option<FileLlm>,
    series: Option<FileSeries>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSeries {
    rules: Option<Vec<FileSeriesRule>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSeriesRule {
    pattern: String,
    key: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileLlm {
    provider: Option<String>,
    model: Option<String>,
    command: Option<String>,
    args: Option<Vec<String>>,
    base_url: Option<String>,
    api_key_env: Option<String>,
    timeout_s: Option<u64>,
    json_mode: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileFusion {
    me: Option<String>,
    me_role: Option<String>,
    glossary_file: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileCapture {
    source: Option<String>,
    poll_ms: Option<u64>,
    poll_timeout_ms: Option<u64>,
    selectors: Option<mc_captions::ui::Selectors>,
    audio_source: Option<String>,
    mic_device: Option<String>,
    system_device: Option<String>,
    analyst: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct CaptureSettings {
    /// `fake:<corpus-dir>`, `teams` or `zoom`; none until the user picks one.
    pub source: Option<String>,
    pub poll_ms: u64,
    /// A source call slower than this is reported as not responding.
    pub poll_timeout_ms: u64,
    /// Teams/Zoom matching rules; defaults are the Python implementation's.
    pub selectors: mc_captions::ui::Selectors,
    /// `wav:<mic.wav>[,<system.wav>]` plays files as the audio devices (tests,
    /// demos); none uses the machine's devices.
    pub audio_source: Option<String>,
    pub mic_device: Option<String>,
    pub system_device: Option<String>,
    /// Run the live analyst while recording (`--analyst`); off unless asked.
    pub analyst: bool,
}

pub const POLL_MS_RANGE: std::ops::RangeInclusive<u64> = 20..=5000;
const DEFAULT_POLL_MS: u64 = 100;
pub const POLL_TIMEOUT_MS_RANGE: std::ops::RangeInclusive<u64> = 200..=60_000;
const DEFAULT_POLL_TIMEOUT_MS: u64 = 5000;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRun {
    debounce_s: Option<f64>,
    merge_similarity: Option<f64>,
    recent_capacity: Option<usize>,
    policy: Option<String>,
    flush: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileTisty {
    command: Option<String>,
    args: Option<Vec<String>>,
    prompt_template: Option<String>,
    folder: Option<String>,
    list: Option<String>,
    tags: Option<Vec<String>>,
    ask: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct RunSettings {
    pub debounce_s: f64,
    pub merge_similarity: f64,
    pub recent_capacity: usize,
    pub policy: Policy,
    pub flush: FlushMode,
}

#[derive(Debug, Clone)]
pub struct TistySettings {
    pub command: String,
    pub args: Vec<String>,
    pub prompt_template: Option<String>,
    /// Folder (existing in Tisty) that receives the minutes.
    pub folder: String,
    /// The folder came from the configuration file, so there is nothing to ask.
    pub configured: bool,
    /// List for the commitments; `None` leaves them in Tisty's inbox.
    pub list: Option<String>,
    pub tags: Vec<String>,
    /// Confirm before publishing in interactive use.
    pub ask: bool,
}

/// What `mc fuse` and `mc eval` need to know about the person and the vocabulary.
#[derive(Debug, Clone, Default)]
pub struct FusionSettings {
    /// The name Teams shows for the person recording; inferred from the audio when absent.
    pub me: Option<String>,
    /// The user's role, shown beside their name in the minutes' participants.
    pub me_role: Option<String>,
    /// One term per line (the meeting's own words: tools, products, jargon).
    pub glossary_file: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// The file that was (or would be) read; `None` when no location is known.
    pub path: Option<PathBuf>,
    pub file_found: bool,
    pub data_dir: PathBuf,
    pub output_dir: PathBuf,
    pub run: RunSettings,
    pub capture: CaptureSettings,
    pub tisty: TistySettings,
    pub fusion: FusionSettings,
    pub llm: LlmConfig,
    /// Meeting names that belong to a series whatever their wording (`series.rules`).
    pub series_rules: Vec<SeriesRule>,
    /// Which layer set each field: `flag`, `env`, `file` or `default`.
    pub sources: BTreeMap<&'static str, &'static str>,
}

impl Config {
    pub fn corpus_dir(&self) -> PathBuf {
        self.data_dir.join("corpus")
    }

    pub fn index_path(&self) -> PathBuf {
        self.data_dir.join("index.sqlite")
    }

    pub fn run_config(&self) -> RunConfig {
        RunConfig {
            reconciler: ReconcilerConfig {
                debounce_s: self.run.debounce_s,
                merge_similarity: self.run.merge_similarity,
                recent_capacity: self.run.recent_capacity,
                policy: self.run.policy,
            },
            flush: self.run.flush,
            policy_by_platform: self.policy_is_default(),
        }
    }

    fn policy_is_default(&self) -> bool {
        self.sources.get("run.policy") == Some(&"default")
    }

    /// The policy for a session of `platform`: the user's explicit choice, else the platform's.
    pub fn policy_for(&self, platform: &str) -> Policy {
        if self.policy_is_default() {
            default_policy(platform)
        } else {
            self.run.policy
        }
    }

    /// `need_data_dir` is false for commands that never touch the store.
    pub fn load(env: &Env, args: &Parsed, need_data_dir: bool) -> Result<Self, CliError> {
        let path = args
            .value("config")
            .map(PathBuf::from)
            .or_else(|| env.var("MC_CONFIG").map(PathBuf::from))
            .or_else(|| default_config_path(env));
        let mut file = FileConfig::default();
        let mut file_found = false;
        if let Some(path) = &path {
            match std::fs::read_to_string(path) {
                Ok(text) => {
                    file = serde_json::from_str(&text)
                        .map_err(|e| CliError::config(format!("{}: {e}", path.display())))?;
                    file_found = true;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // An explicitly requested file must exist.
                    if args.value("config").is_some() || env.var("MC_CONFIG").is_some() {
                        return Err(CliError::config(format!(
                            "{}: file not found",
                            path.display()
                        )));
                    }
                }
                Err(e) => {
                    return Err(CliError::config(format!("{}: {e}", path.display())));
                }
            }
        }
        if let Some(version) = file.version.filter(|v| *v != 1) {
            return Err(CliError::config(format!(
                "unsupported config version {version} (this build reads version 1)"
            )));
        }
        let config_dir = path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let mut sources = BTreeMap::new();
        let run = file.run.unwrap_or_default();
        let tisty = file.tisty.unwrap_or_default();
        let capture_file = file.capture.unwrap_or_default();
        let number_env = |name: &str| {
            env.var(name)
                .map(|v| {
                    v.parse::<u64>().map_err(|_| {
                        CliError::config(format!("{name}: {v:?} is not a whole number"))
                    })
                })
                .transpose()
        };
        let poll_env = number_env("MC_POLL_MS")?;
        let poll_timeout_env = number_env("MC_POLL_TIMEOUT_MS")?;
        let capture = CaptureSettings {
            source: pick(
                &mut sources,
                "capture.source",
                None,
                env.var("MC_CAPTURE_SOURCE").map(str::to_string),
                capture_file.source,
                None,
            ),
            poll_ms: pick(
                &mut sources,
                "capture.poll_ms",
                None,
                poll_env,
                capture_file.poll_ms,
                Some(DEFAULT_POLL_MS),
            )
            .unwrap_or(DEFAULT_POLL_MS),
            poll_timeout_ms: pick(
                &mut sources,
                "capture.poll_timeout_ms",
                None,
                poll_timeout_env,
                capture_file.poll_timeout_ms,
                Some(DEFAULT_POLL_TIMEOUT_MS),
            )
            .unwrap_or(DEFAULT_POLL_TIMEOUT_MS),
            selectors: capture_file.selectors.unwrap_or_default(),
            audio_source: capture_file.audio_source,
            mic_device: capture_file.mic_device,
            system_device: capture_file.system_device,
            analyst: capture_file.analyst.unwrap_or(false),
        };
        capture.selectors.validate().map_err(CliError::config)?;

        let data_dir = pick(
            &mut sources,
            "data_dir",
            args.value("data-dir")
                .map(|v| absolute(v, &env.cwd, "--data-dir"))
                .transpose()?,
            env.var("MC_DATA_DIR")
                .map(|v| absolute(v, &env.cwd, "MC_DATA_DIR"))
                .transpose()?,
            file.data_dir
                .as_deref()
                .map(|v| absolute(v, &config_dir, "data_dir"))
                .transpose()?,
            default_data_dir(env),
        )
        .map_or_else(
            || {
                if need_data_dir {
                    Err(CliError::config(
                        "cannot locate a data directory; set MC_DATA_DIR or --data-dir",
                    ))
                } else {
                    Ok(PathBuf::new())
                }
            },
            Ok,
        )?;
        let output_dir = pick(
            &mut sources,
            "output_dir",
            None,
            env.var("MC_OUTPUT_DIR")
                .map(|v| absolute(v, &env.cwd, "MC_OUTPUT_DIR"))
                .transpose()?,
            file.output_dir
                .as_deref()
                .map(|v| absolute(v, &config_dir, "output_dir"))
                .transpose()?,
            documents_dir(env).map(|d| d.join(APP_DIR)),
        )
        .unwrap_or_else(|| data_dir.join("exports"));

        let defaults = ReconcilerConfig::default();
        let debounce_env = env
            .var("MC_DEBOUNCE_S")
            .map(|v| {
                v.parse::<f64>()
                    .map_err(|_| CliError::config(format!("MC_DEBOUNCE_S: {v:?} is not a number")))
            })
            .transpose()?;
        let flush_env = env.var("MC_FLUSH").map(parse_flush).transpose()?;
        let flush_file = run.flush.as_deref().map(parse_flush).transpose()?;
        let policy_env = env.var("MC_POLICY").map(parse_policy_name).transpose()?;
        let policy_file = run.policy.as_deref().map(parse_policy_name).transpose()?;
        let run_settings = RunSettings {
            debounce_s: pick(
                &mut sources,
                "run.debounce_s",
                None,
                debounce_env,
                run.debounce_s,
                Some(defaults.debounce_s),
            )
            .unwrap_or(defaults.debounce_s),
            merge_similarity: pick(
                &mut sources,
                "run.merge_similarity",
                None,
                None,
                run.merge_similarity,
                Some(defaults.merge_similarity),
            )
            .unwrap_or(defaults.merge_similarity),
            recent_capacity: pick(
                &mut sources,
                "run.recent_capacity",
                None,
                None,
                run.recent_capacity,
                Some(defaults.recent_capacity),
            )
            .unwrap_or(defaults.recent_capacity),
            policy: pick(
                &mut sources,
                "run.policy",
                None,
                policy_env,
                policy_file,
                Some(defaults.policy),
            )
            .unwrap_or(defaults.policy),
            flush: pick(
                &mut sources,
                "run.flush",
                None,
                flush_env,
                flush_file,
                Some(FlushMode::Auto),
            )
            .unwrap_or(FlushMode::Auto),
        };
        validate_run(&run_settings)?;

        let tisty_settings = TistySettings {
            command: tisty.command.unwrap_or_else(|| "tisty".to_string()),
            args: tisty.args.unwrap_or_else(|| vec!["mcp".to_string()]),
            prompt_template: tisty.prompt_template,
            configured: tisty.folder.is_some(),
            folder: tisty
                .folder
                .map(|f| f.trim().to_string())
                .unwrap_or_else(|| "Reuniones".to_string()),
            list: tisty
                .list
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty()),
            tags: tisty.tags.unwrap_or_default(),
            ask: tisty.ask.unwrap_or(true),
        };
        if tisty_settings.folder.is_empty() {
            return Err(CliError::config("tisty.folder must not be empty"));
        }
        if let Some(template) = &tisty_settings.prompt_template {
            check_template(template)?;
        }
        let fusion_file = file.fusion.unwrap_or_default();
        let fusion_settings = FusionSettings {
            me: fusion_file.me.filter(|m| !m.trim().is_empty()),
            me_role: fusion_file.me_role.filter(|r| !r.trim().is_empty()),
            glossary_file: fusion_file
                .glossary_file
                .as_deref()
                .map(|v| absolute(v, &config_dir, "fusion.glossary_file"))
                .transpose()?,
        };
        let llm = llm_settings(env, args, file.llm.unwrap_or_default(), &mut sources);
        let series_rules = series_rules(file.series.unwrap_or_default())?;
        let mut config = Self {
            path,
            file_found,
            data_dir,
            output_dir,
            run: run_settings,
            capture,
            tisty: tisty_settings,
            fusion: fusion_settings,
            llm,
            series_rules,
            sources,
        };
        config.apply_run_flags(args)?;
        Ok(config)
    }

    /// `--debounce` and `--flush` of `mc run`.
    pub fn apply_run_flags(&mut self, args: &Parsed) -> Result<(), CliError> {
        if let Some(value) = args.value("debounce") {
            self.run.debounce_s = value
                .parse()
                .map_err(|_| CliError::usage(format!("--debounce: {value:?} is not a number")))?;
            self.sources.insert("run.debounce_s", "flag");
            validate_run(&self.run)
                .map_err(|e| CliError::usage(format!("--debounce: {}", e.message())))?;
        }
        if let Some(value) = args.value("policy") {
            self.run.policy = parse_policy(value).ok_or_else(|| {
                CliError::usage(format!(
                    "--policy: unknown policy {value:?} (known: v1_legacy, v2)"
                ))
            })?;
            self.sources.insert("run.policy", "flag");
        }
        if let Some(value) = args.value("flush") {
            self.run.flush = parse_flush(value).map_err(|e| CliError::usage(e.message()))?;
            self.sources.insert("run.flush", "flag");
        }
        validate_run(&self.run)
    }

    /// The LLM settings, for a command that is about to build a client.
    pub fn checked_llm(&self) -> Result<&LlmConfig, CliError> {
        self.llm
            .validate()
            .map_err(|e| CliError::config(format!("llm: {e}")))?;
        Ok(&self.llm)
    }

    pub fn policy_label(&self) -> &'static str {
        policy_name(self.run.policy)
    }
}

/// First present layer wins; records which one.
fn pick<T>(
    sources: &mut BTreeMap<&'static str, &'static str>,
    key: &'static str,
    flag: Option<T>,
    env: Option<T>,
    file: Option<T>,
    default: Option<T>,
) -> Option<T> {
    for (layer, value) in [
        ("flag", flag),
        ("env", env),
        ("file", file),
        ("default", default),
    ] {
        if value.is_some() {
            sources.insert(key, layer);
            return value;
        }
    }
    None
}

/// `--llm-provider`/`--llm-model` > `MC_LLM_PROVIDER`/`MC_LLM_MODEL` > file > default.
/// Not validated here: only a command that builds a client needs it to be sound.
fn llm_settings(
    env: &Env,
    args: &Parsed,
    file: FileLlm,
    sources: &mut BTreeMap<&'static str, &'static str>,
) -> LlmConfig {
    let defaults = LlmConfig::default();
    let owned = |v: Option<&str>| v.map(str::to_string);
    let provider = pick(
        sources,
        "llm.provider",
        owned(args.value("llm-provider")),
        owned(env.var("MC_LLM_PROVIDER")),
        file.provider,
        Some(defaults.provider.clone()),
    )
    .unwrap_or(defaults.provider);
    let model = pick(
        sources,
        "llm.model",
        owned(args.value("llm-model")),
        owned(env.var("MC_LLM_MODEL")),
        file.model,
        None,
    );
    LlmConfig {
        provider,
        model: model.filter(|m| !m.trim().is_empty()),
        command: file.command.filter(|c| !c.trim().is_empty()),
        args: file.args.unwrap_or_default(),
        base_url: file.base_url.filter(|u| !u.trim().is_empty()),
        api_key_env: file.api_key_env.unwrap_or_default(),
        timeout_s: file.timeout_s.unwrap_or(defaults.timeout_s),
        json_mode: file.json_mode.unwrap_or(defaults.json_mode),
    }
}

fn parse_flush(value: &str) -> Result<FlushMode, CliError> {
    match value {
        "auto" => Ok(FlushMode::Auto),
        "manifest-only" | "manifest_only" => Ok(FlushMode::ManifestOnly),
        other => Err(CliError::config(format!(
            "flush must be \"auto\" or \"manifest-only\", not {other:?}"
        ))),
    }
}

fn parse_policy_name(value: &str) -> Result<Policy, CliError> {
    parse_policy(value)
        .ok_or_else(|| CliError::config(format!("unknown policy {value:?} (known: v1_legacy, v2)")))
}

fn validate_run(run: &RunSettings) -> Result<(), CliError> {
    if !(run.debounce_s.is_finite() && run.debounce_s > 0.0) {
        return Err(CliError::config("run.debounce_s must be a number > 0"));
    }
    if !(run.merge_similarity > 0.0 && run.merge_similarity <= 1.0) {
        return Err(CliError::config("run.merge_similarity must be in (0, 1]"));
    }
    if run.recent_capacity == 0 {
        return Err(CliError::config("run.recent_capacity must be >= 1"));
    }
    Ok(())
}

/// A rule needs both a pattern to look for and a key to file the meeting under.
fn series_rules(file: FileSeries) -> Result<Vec<SeriesRule>, CliError> {
    file.rules
        .unwrap_or_default()
        .into_iter()
        .map(|rule| match (rule.pattern.trim(), user_key(&rule.key)) {
            ("", _) | (_, None) => Err(CliError::config(
                "series.rules: every rule needs a non-empty pattern and key",
            )),
            (pattern, Some(key)) => Ok(SeriesRule {
                pattern: pattern.to_string(),
                key,
            }),
        })
        .collect()
}

/// Every `{name}` of the template must be a known placeholder.
fn check_template(template: &str) -> Result<(), CliError> {
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else { break };
        let name = &after[..end];
        if !PLACEHOLDERS.contains(&name) {
            return Err(CliError::config(format!(
                "tisty.prompt_template: unknown placeholder {{{name}}} (known: {})",
                PLACEHOLDERS.join(", ")
            )));
        }
        rest = &after[end + 1..];
    }
    Ok(())
}

/// Relative values resolve against `base`; `~` is never expanded by us.
fn absolute(raw: &str, base: &Path, what: &str) -> Result<PathBuf, CliError> {
    if raw.starts_with('~') {
        return Err(CliError::config(format!(
            "{what}: {raw:?} starts with `~`, which is not expanded; use an absolute path"
        )));
    }
    let path = PathBuf::from(raw);
    Ok(if path.is_absolute() {
        path
    } else {
        base.join(path)
    })
}

fn home(env: &Env) -> Option<PathBuf> {
    let key = if env.os == Os::Windows {
        "USERPROFILE"
    } else {
        "HOME"
    };
    env.var(key).map(PathBuf::from)
}

/// An XDG directory, absolute by Unix rules whatever the host (XDG ignores relative ones).
fn xdg_dir(env: &Env, key: &str) -> Option<PathBuf> {
    env.var(key)
        .filter(|v| v.starts_with('/'))
        .map(PathBuf::from)
}

/// Where the per-user configuration folders live.
pub fn config_base(env: &Env) -> Option<PathBuf> {
    match env.os {
        Os::Windows => env.var("APPDATA").map(PathBuf::from),
        Os::Mac => home(env).map(|h| h.join("Library").join("Application Support")),
        Os::Linux => {
            xdg_dir(env, "XDG_CONFIG_HOME").or_else(|| home(env).map(|h| h.join(".config")))
        }
    }
}

/// Where the per-user data folders live.
pub fn data_base(env: &Env) -> Option<PathBuf> {
    match env.os {
        Os::Windows => env.var("LOCALAPPDATA").map(PathBuf::from),
        Os::Mac => home(env).map(|h| h.join("Library").join("Application Support")),
        Os::Linux => xdg_dir(env, "XDG_DATA_HOME")
            .or_else(|| home(env).map(|h| h.join(".local").join("share"))),
    }
}

pub fn documents_dir(env: &Env) -> Option<PathBuf> {
    home(env).map(|h| h.join("Documents"))
}

pub fn default_config_path(env: &Env) -> Option<PathBuf> {
    Some(config_base(env)?.join(APP_DIR).join("config.json"))
}

pub fn default_data_dir(env: &Env) -> Option<PathBuf> {
    Some(data_base(env)?.join(APP_DIR))
}

/// The app's folder name under each base folder.
pub fn app_dir_name() -> &'static str {
    APP_DIR
}

/// Used by `doctor` to print a path the way the user wrote it.
pub fn display(path: &Path) -> String {
    path.display().to_string()
}
