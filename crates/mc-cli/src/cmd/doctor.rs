use std::path::PathBuf;

use mc_audio::permission::AudioAccess;
use mc_captions::ui::ax_map::PERMISSION_HELP;
use mc_captions::ui::dump::{DumpOptions, dump as dump_trees};
use mc_captions::ui::report::{format_report, uia_report};
use mc_captions::ui::{FakeBackend, Fixture, Platform, UiBackend};
use serde_json::json;

use crate::cli::{CliError, Ctx, Env, Io, Os, emit};
use crate::config::display;
use crate::publish::tisty::{self, Health, resolve};

struct Check {
    name: &'static str,
    status: &'static str,
    detail: String,
    /// A permission's state for programs (`granted`, `denied`, `not_asked`, `restricted`, `missing`).
    state: Option<&'static str>,
}

fn check(name: &'static str, status: &'static str, detail: impl Into<String>) -> Check {
    Check {
        name,
        status,
        detail: detail.into(),
        state: None,
    }
}

impl Check {
    fn with_state(self, state: &'static str) -> Self {
        Self {
            state: Some(state),
            ..self
        }
    }

    fn json(&self) -> serde_json::Value {
        let mut value = json!({"name": self.name, "status": self.status, "detail": self.detail});
        if let Some(state) = self.state {
            value["state"] = json!(state);
        }
        value
    }
}

/// The permissions `--ask-permission` can ask macOS for.
const ASKABLE: [&str; 3] = ["accessibility", "microphone", "screen_recording"];

/// Presence check of a command on `PATH` (honouring `PATHEXT` on Windows).
pub fn find_on_path(env: &Env, command: &str) -> Option<PathBuf> {
    let path = env.var("PATH")?;
    let pathext =
        (env.os == Os::Windows).then(|| env.var("PATHEXT").unwrap_or(".COM;.EXE;.BAT;.CMD"));
    mc_llm::find_on_path(command, path, pathext)
}

fn writable(dir: &std::path::Path) -> Result<(), String> {
    use std::io::Write;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let probe = dir.join(format!(".mc-doctor-{}", std::process::id()));
    // create_new: never follow or truncate whatever already sits at the path.
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .and_then(|mut file| file.write_all(b"probe"));
    let removed = std::fs::remove_file(&probe);
    written.map_err(|e| e.to_string())?;
    removed.map_err(|e| e.to_string())
}

/// The configuration did not load: report that and stop.
pub fn broken_config(err: &CliError, io: &mut Io<'_>, json_output: bool) -> Result<u8, CliError> {
    let human = format!(
        "FAIL  config      {}\n      (other checks skipped until the configuration loads)\n",
        err.message()
    );
    let value = json!({
        "checks": [{"name": "config", "status": "FAIL", "detail": err.message()}],
        "ok": false,
    });
    emit(io, json_output, human, value)?;
    Ok(1)
}

pub fn run(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    if !ctx.args.positional.is_empty() {
        return Err(CliError::usage("usage: eli5 doctor"));
    }
    let ask = ctx.args.value("ask-permission");
    if let Some(name) = ask
        && !ASKABLE.contains(&name)
    {
        return Err(CliError::usage(format!(
            "--ask-permission takes one of {}, not {name:?}",
            ASKABLE.join(", ")
        )));
    }
    let cfg = &ctx.cfg;
    let mut checks = Vec::new();

    let layers: Vec<String> = cfg
        .sources
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    checks.push(check(
        "config",
        "ok",
        format!(
            "{} ({}); {}",
            cfg.path
                .as_deref()
                .map_or("no location".to_string(), display),
            if cfg.file_found {
                "found"
            } else {
                "not found, using defaults"
            },
            layers.join(" ")
        ),
    ));

    match writable(&cfg.data_dir) {
        Ok(()) => checks.push(check("data_dir", "ok", display(&cfg.data_dir))),
        Err(e) => checks.push(check(
            "data_dir",
            "FAIL",
            format!("{}: {e}", cfg.data_dir.display()),
        )),
    }
    match mc_store::Store::open(cfg.index_path()).and_then(|s| s.schema_version()) {
        Ok(version) => checks.push(check(
            "index",
            "ok",
            format!("{} (schema {version})", cfg.index_path().display()),
        )),
        Err(e) => checks.push(check(
            "index",
            "FAIL",
            format!("{}: {e}", cfg.index_path().display()),
        )),
    }
    match writable(&cfg.output_dir) {
        Ok(()) => checks.push(check("output_dir", "ok", display(&cfg.output_dir))),
        Err(e) => checks.push(check(
            "output_dir",
            "warn",
            format!("{}: {e}", cfg.output_dir.display()),
        )),
    }
    let tisty = resolve(ctx.env, &cfg.tisty.command);
    match &tisty {
        Some(path) => checks.push(check("tisty", "ok", display(path))),
        None => checks.push(check(
            "tisty",
            "warn",
            format!(
                "`{}` not found on PATH nor in the default install; exports fall back to files",
                cfg.tisty.command
            ),
        )),
    }
    if ctx.args.flag("tisty") {
        checks.push(tisty_mcp_check(&tisty, &cfg.tisty.args));
    }

    checks.push(llm_check(ctx));
    checks.push(whisper_check());
    match ask {
        Some("microphone") => mc_audio::request_microphone(),
        Some("screen_recording") => mc_audio::request_screen_recording(),
        _ => {}
    }
    // The run's only Accessibility prompt: `--uia` and `--dump` then read without asking again.
    let prompt = ctx.args.flag("ax-prompt") || ask == Some("accessibility");
    checks.extend(permission_checks(
        mc_captions::accessibility_trusted(prompt),
        mc_audio::audio_access(),
    ));

    let failed = checks.iter().any(|c| c.status == "FAIL");
    let mut human: String = checks
        .iter()
        .map(|c| format!("{:<5} {:<11} {}\n", c.status, c.name, c.detail))
        .collect();
    let (uia_text, uia_value) = uia_section(ctx)?;
    human.push_str(&uia_text);
    let (audio_text, audio_value, audio_unusable) = audio_section(ctx)?;
    human.push_str(&audio_text);
    let failed = failed || audio_unusable;
    let mut value = json!({
        "checks": checks
            .iter()
            .map(Check::json)
            .collect::<Vec<_>>(),
        "ok": !failed,
    });
    value["tisty_target"] = json!({
        "configured": cfg.tisty.configured,
        "folder": cfg.tisty.folder,
        "list": cfg.tisty.list,
    });
    if let Some(uia) = uia_value {
        value["uia"] = uia;
    }
    if let Some(audio) = audio_value {
        value["audio"] = audio;
    }
    value["ok"] = json!(!failed);
    emit(io, ctx.json, human, value)?;
    Ok(u8::from(failed))
}

/// Starts Tisty's MCP server and asks it what it offers; "not admitted" is told apart.
fn tisty_mcp_check(program: &Option<PathBuf>, args: &[String]) -> Check {
    let Some(program) = program else {
        return check("tisty_mcp", "warn", "Tisty was not found, nothing to start");
    };
    match tisty::check(program, args) {
        Health::Ready { tools } => check(
            "tisty_mcp",
            "ok",
            format!("write_doc and propose available ({tools} tools)"),
        ),
        Health::NotAdmitted(detail) => check("tisty_mcp", "warn", detail),
        Health::MissingTools(missing) => check(
            "tisty_mcp",
            "warn",
            format!("this Tisty lacks the tools: {}", missing.join(", ")),
        ),
        Health::Broken(detail) => check("tisty_mcp", "warn", detail),
    }
}

/// A missing permission only warns: only the step that needs it fails.
fn permission_checks(accessibility: Option<bool>, audio: Option<AudioAccess>) -> Vec<Check> {
    let mut checks: Vec<Check> = accessibility
        .into_iter()
        .map(|trusted| {
            if trusted {
                check("accessibility", "ok", "allowed: the captions can be read")
                    .with_state("granted")
            } else {
                check("accessibility", "warn", PERMISSION_HELP).with_state("missing")
            }
        })
        .collect();
    checks.extend(
        audio
            .map(mc_audio::permission::lines)
            .unwrap_or_default()
            .into_iter()
            .map(|line| {
                check(line.name, if line.ok { "ok" } else { "warn" }, line.detail)
                    .with_state(line.state)
            }),
    );
    checks
}

/// Whether this build can recognise speech; without it `transcribe` and `--live-asr` refuse.
fn whisper_check() -> Check {
    if cfg!(feature = "whisper") {
        // What the build targets: whisper.cpp still falls back to the CPU without a usable GPU.
        let device = if cfg!(target_os = "macos") {
            "compiled for Metal"
        } else if cfg!(feature = "cuda") {
            "compiled for CUDA"
        } else if cfg!(feature = "vulkan") {
            "compiled for Vulkan"
        } else {
            "CPU only"
        };
        check(
            "whisper",
            "ok",
            format!("Whisper engine built in ({device})"),
        )
    } else {
        check(
            "whisper",
            "warn",
            "this build has no Whisper engine; rebuild with `--features whisper`",
        )
    }
}

/// The provider and, for a CLI provider, whether its command is on PATH (a warning, never a failure).
fn llm_check(ctx: &Ctx<'_>) -> Check {
    let llm = &ctx.cfg.llm;
    if let Err(e) = llm.validate() {
        return check("llm", "FAIL", e.to_string());
    }
    let model = llm.model.as_deref().unwrap_or("default model");
    if let Some(command) = llm.command_name() {
        let path = ctx.env.var("PATH").unwrap_or_default();
        let pathext = (ctx.env.os == Os::Windows)
            .then(|| ctx.env.var("PATHEXT").unwrap_or(".COM;.EXE;.BAT;.CMD"));
        return match mc_llm::resolve_command(command, &ctx.env.cwd, path, pathext) {
            Some(found) => check(
                "llm",
                "ok",
                format!("{} ({model}): {}", llm.provider, display(&found)),
            ),
            None => check(
                "llm",
                "warn",
                format!(
                    "{}: `{command}` not found; install it or change llm.provider",
                    llm.provider
                ),
            ),
        };
    }
    match llm.key_env() {
        Some(key) if ctx.env.var(&key).is_none() => check(
            "llm",
            "warn",
            format!("{} ({model}): {key} is not set", llm.provider),
        ),
        _ => check("llm", "ok", format!("{} ({model})", llm.provider)),
    }
}

/// The accessibility backend `--uia`/`--dump` read from: a fixture on any OS
/// (`--source fake-uia:<file>`), the real windows on Windows and macOS.
#[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
pub fn ui_backend(ctx: &Ctx<'_>, prompt: bool) -> Result<Box<dyn UiBackend>, CliError> {
    if let Some(spec) = ctx.args.value("source") {
        let Some(path) = spec.strip_prefix("fake-uia:") else {
            return Err(CliError::usage(
                "doctor --source takes fake-uia:<fixture.json>; the live windows are read by default",
            ));
        };
        let text = std::fs::read_to_string(path)
            .map_err(|e| CliError::not_found(format!("{path}: {e}"), "pass a fixture file"))?;
        let fixture: Fixture =
            serde_json::from_str(&text).map_err(|e| CliError::usage(format!("{path}: {e}")))?;
        return Ok(Box::new(FakeBackend::from_fixture(fixture)));
    }
    #[cfg(windows)]
    {
        let backend = mc_captions::win_uia::WinUiaBackend::new(mc_captions::win_uia::WinOptions {
            // Diagnosing may read a huge tree; there is no stop key to honour here.
            connection_ms: 5_000,
            transaction_ms: 20_000,
        })
        .map_err(|e| CliError::runtime("io", e.to_string()))?;
        Ok(Box::new(backend))
    }
    #[cfg(target_os = "macos")]
    {
        let backend = mc_captions::mac_ax::MacAxBackend::new(mc_captions::mac_ax::MacOptions {
            // Diagnosing may read a huge tree; there is no stop key to honour here.
            messaging_ms: 20_000,
            prompt,
            platform: None,
            selectors: ctx.cfg.capture.selectors.clone(),
        })
        .map_err(|e| CliError::runtime("io", e.to_string()))?;
        Ok(Box::new(backend))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    Err(CliError::usage(
        "--uia/--dump read the live windows through Windows UI Automation or the macOS Accessibility API; on this OS pass --source fake-uia:<fixture.json>",
    ))
}

fn number_option(ctx: &Ctx<'_>, name: &str, default: usize) -> Result<usize, CliError> {
    match ctx.args.value(name) {
        None => Ok(default),
        Some(v) => v
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| CliError::usage(format!("--{name}: {v:?} is not a positive number"))),
    }
}

/// `--uia` report and `--dump` file; nothing when neither is asked for.
fn uia_section(ctx: &Ctx<'_>) -> Result<(String, Option<serde_json::Value>), CliError> {
    let dump = ctx.args.value("dump");
    if !ctx.args.flag("uia") && dump.is_none() {
        return Ok((String::new(), None));
    }
    if dump.is_none() {
        for option in ["window", "platform", "max-depth"] {
            if ctx.args.value(option).is_some() {
                return Err(CliError::usage(format!(
                    "--{option} only applies with --dump"
                )));
            }
        }
        if ctx.args.flag("raw") {
            return Err(CliError::usage("--raw only applies with --dump"));
        }
    }
    let selectors = &ctx.cfg.capture.selectors;
    let max_nodes = number_option(ctx, "max-nodes", 20_000)?;
    let mut backend = ui_backend(ctx, false)?;
    let mut text = String::new();
    let mut value = serde_json::Map::new();
    if ctx.args.flag("uia") {
        let started = std::time::Instant::now();
        let mut timer = move || started.elapsed().as_secs_f64() * 1000.0;
        let report = uia_report(backend.as_mut(), selectors, max_nodes, &mut timer)
            .map_err(|e| CliError::runtime("io", e.to_string()))?;
        text.push_str(&format_report(&report));
        value.insert(
            "report".into(),
            serde_json::to_value(&report).map_err(|e| CliError::runtime("io", e.to_string()))?,
        );
    }
    if let Some(path) = dump {
        let window = ctx
            .args
            .value("window")
            .map(|v| {
                v.parse::<usize>()
                    .map_err(|_| CliError::usage(format!("--window: {v:?} is not an index")))
            })
            .transpose()?;
        if let Some(w) = window {
            let count = backend
                .windows()
                .map_err(|e| CliError::runtime("io", e.to_string()))?
                .len();
            if w >= count {
                return Err(CliError::usage(format!(
                    "--window {w}: there are only {count} window(s) (see `mc doctor --uia`)"
                )));
            }
        }
        if ctx.args.flag("raw") && window.is_none() {
            return Err(CliError::usage(
                "--raw needs --window N (the index from `mc doctor --uia`): a raw dump holds everything the window shows",
            ));
        }
        if std::path::Path::new(path).exists() && !ctx.args.flag("force") {
            return Err(CliError::usage(format!(
                "{path} already exists; pass --force to overwrite it"
            )));
        }
        if ctx.args.flag("raw") {
            eprintln!(
                "warning: --raw keeps the real text of the window (names, captions, URLs). Do not share or commit the file."
            );
        }
        let options = DumpOptions {
            platform: ctx.args.value("platform").unwrap_or("teams").to_string(),
            windows: window.map(|w| vec![w]),
            max_depth: number_option(ctx, "max-depth", 60)?,
            max_nodes,
            raw: ctx.args.flag("raw"),
        };
        if Platform::parse(&options.platform).is_none() {
            return Err(CliError::usage(
                "--platform must be teams, zoom, meet, teams-web or slack",
            ));
        }
        let fixture = dump_trees(backend.as_mut(), selectors, &options)
            .map_err(|e| CliError::runtime("io", e.to_string()))?;
        let json = serde_json::to_string_pretty(&fixture)
            .map_err(|e| CliError::runtime("io", e.to_string()))?
            + "\n";
        let tmp = format!("{path}.tmp");
        std::fs::write(&tmp, json)
            .and_then(|()| std::fs::rename(&tmp, path))
            .map_err(|e| CliError::runtime("io", format!("{path}: {e}")))?;
        text.push_str(&format!(
            "dump: {} window(s) written to {path}{}\n",
            fixture.windows.len(),
            if options.raw {
                " (RAW: contains the meeting's text; do not share or commit)"
            } else {
                " (anonymised: review it before sharing)"
            }
        ));
        value.insert(
            "dump".into(),
            serde_json::json!({"path": path, "windows": fixture.windows.len(), "raw": options.raw}),
        );
    }
    Ok((text, Some(serde_json::Value::Object(value))))
}

/// `--audio`: list the devices and listen to both channels for a few seconds.
/// Returns the text, the JSON and whether neither channel could be opened.
fn audio_section(ctx: &Ctx<'_>) -> Result<(String, Option<serde_json::Value>, bool), CliError> {
    if !ctx.args.flag("audio") {
        for option in [
            "audio-source",
            "mic-device",
            "system-device",
            "audio-seconds",
        ] {
            if ctx.args.value(option).is_some() {
                return Err(CliError::usage(format!("--{option} needs --audio")));
            }
        }
        return Ok((String::new(), None, false));
    }
    let seconds = match ctx.args.value("audio-seconds") {
        None => 3.0,
        Some(v) => v
            .parse::<f64>()
            .ok()
            .filter(|s| s.is_finite() && (0.5..=60.0).contains(s))
            .ok_or_else(|| {
                CliError::usage(format!("--audio-seconds: {v:?} is not between 0.5 and 60"))
            })?,
    };
    let Some(backend) = super::capture::audio_backend(ctx)? else {
        let text = "FAIL  audio       no audio backend on this OS (pass --audio-source wav:<mic.wav>[,<system.wav>] to try files)\n".to_string();
        return Ok((text, Some(json!({"available": false})), true));
    };
    let mic = ctx
        .args
        .value("mic-device")
        .or(ctx.cfg.capture.mic_device.as_deref());
    let system = ctx
        .args
        .value("system-device")
        .or(ctx.cfg.capture.system_device.as_deref());
    let devices = backend.devices();
    let probe = mc_audio::probe::probe(backend.as_ref(), mic, system, seconds);
    let text = mc_audio::probe::format_probe(&probe, &devices);
    let value = json!({"available": true, "devices": devices, "probe": probe});
    let unusable = probe.unusable();
    Ok((text, Some(value), unusable))
}

#[cfg(test)]
mod tests {
    use mc_audio::permission::{Access, Screen};

    use super::*;

    fn names(checks: &[Check]) -> Vec<(&str, &str)> {
        checks.iter().map(|c| (c.name, c.status)).collect()
    }

    #[test]
    fn no_permission_lines_where_the_os_keeps_none() {
        assert!(permission_checks(None, None).is_empty());
    }

    #[test]
    fn missing_permissions_warn_and_say_where_to_allow_them() {
        let audio = AudioAccess {
            microphone: Access::Denied,
            screen_recording: Some(Screen {
                granted: false,
                fallback: false,
            }),
        };
        let checks = permission_checks(Some(false), Some(audio));
        assert_eq!(
            names(&checks),
            [
                ("accessibility", "warn"),
                ("microphone", "warn"),
                ("screen_recording", "warn")
            ]
        );
        assert!(
            checks[0]
                .detail
                .contains("Privacy & Security > Accessibility")
        );
        let states: Vec<_> = checks.iter().map(|c| c.json()["state"].clone()).collect();
        assert_eq!(
            states,
            [json!("missing"), json!("denied"), json!("missing")]
        );
    }

    #[test]
    fn granted_permissions_are_ok() {
        let audio = AudioAccess {
            microphone: Access::Granted,
            screen_recording: None,
        };
        let checks = permission_checks(Some(true), Some(audio));
        assert_eq!(
            names(&checks),
            [("accessibility", "ok"), ("microphone", "ok")]
        );
        assert_eq!(checks[1].json()["state"], "granted");
    }

    #[test]
    fn other_checks_carry_no_state() {
        assert!(check("config", "ok", "x").json().get("state").is_none());
    }
}
