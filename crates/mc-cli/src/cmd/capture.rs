//! `mc capture`: record a meeting's captions into a corpus session.
//!
//! Only the main thread writes to stdout; the capture thread reports over a
//! channel. std cannot catch Ctrl-C, so a clean stop is `q` + Enter on a
//! terminal, a `STOP` file (session or corpus folder), `--until` seconds, or the
//! source ending. Killing the process is also safe: captions are written
//! unbuffered and the next run flags the session as unclean.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use mc_analyst::AnalystConfig;
use mc_audio::{AudioBackend, WavFileBackend};
use mc_captions::ui::{FakeBackend, Fixture, NodeMode, Platform, Selectors, UiaSource};
use mc_captions::{CaptureSource, Discovery};
use mc_engine::controller::{Capture, CaptureSpec, RecordSpec, start};
use mc_engine::corpus::{repair_session_audio, scan_unclean};
use mc_engine::live::{AudioSpec, LiveAsrSpec};
use mc_engine::live::{
    DebounceClock, LiveConfig, LiveEvent, LiveSummary, SourceFactory, fake_from_corpus, once,
};
use mc_engine::live_analyst::AnalystSpec;
use mc_engine::runs::{fmt_rel, rel_ms};
use serde_json::{Value, json};

use crate::cli::{CliError, Ctx, Io};
use crate::config::{Config, POLL_MS_RANGE, POLL_TIMEOUT_MS_RANGE};

use super::analysis::{background, ledger_counts, make_llm};
use super::carry::carry_for;
use super::memory::{open_memory, teach_person, teach_term};
use super::vocabulary::{glossary_file, glossary_from, memory_homonyms, prompt_terms};

/// How long the end of a plain capture waits for the analyst's last window.
const ANALYST_TAIL_WAIT: Duration = Duration::from_secs(30);

/// What happens when a meeting ends. Phase 1 only suggests next steps; the
/// show / save-as-md / send-to-Tisty prompt arrives with the TUI and Phase 2.
pub trait EndOfMeeting {
    fn on_end(
        &mut self,
        summary: &LiveSummary,
        io: &mut Io<'_>,
        json: bool,
    ) -> Result<(), CliError>;
}

pub struct StubEnd;

impl EndOfMeeting for StubEnd {
    fn on_end(
        &mut self,
        summary: &LiveSummary,
        io: &mut Io<'_>,
        json: bool,
    ) -> Result<(), CliError> {
        if json {
            return Ok(());
        }
        if let Some(dir) = &summary.session_dir {
            writeln!(io.out, "session saved in {}", dir.display())?;
            writeln!(
                io.out,
                "next: `mc import {0}` then `mc run {0}` and `mc export <session> --format md --save`",
                dir.display()
            )?;
        }
        Ok(())
    }
}

enum Source {
    /// Replays a recorded corpus frame by frame.
    Fake(std::path::PathBuf),
    /// Replays an accessibility-tree fixture through the real parsers.
    FakeUia(std::path::PathBuf),
    /// The real meeting window through the OS accessibility API.
    #[cfg(any(windows, target_os = "macos"))]
    Native(Platform),
}

#[cfg(any(windows, target_os = "macos"))]
#[allow(clippy::unnecessary_wraps)]
fn native(platform: Platform) -> Result<Source, CliError> {
    Ok(Source::Native(platform))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn native(_platform: Platform) -> Result<Source, CliError> {
    Err(CliError::usage(
        "--source teams|zoom|meet|teams-web|slack needs Windows UI Automation or the macOS Accessibility API; on this OS use fake-uia:<fixture.json>",
    ))
}

fn parse_source(spec: &str) -> Result<Source, CliError> {
    if let Some(dir) = spec.strip_prefix("fake:") {
        if dir.is_empty() {
            return Err(CliError::usage("--source fake: needs a corpus directory"));
        }
        return Ok(Source::Fake(dir.into()));
    }
    if let Some(path) = spec.strip_prefix("fake-uia:") {
        if path.is_empty() {
            return Err(CliError::usage("--source fake-uia: needs a fixture file"));
        }
        return Ok(Source::FakeUia(path.into()));
    }
    match Platform::parse(spec) {
        Some(platform) => native(platform),
        None => Err(CliError::usage(format!(
            "unknown source {spec:?} (use teams, zoom, meet, teams-web, slack, fake:<corpus-dir> or fake-uia:<fixture.json>)"
        ))),
    }
}

#[cfg(windows)]
fn native_source(
    platform: Platform,
    selectors: Selectors,
) -> Result<Box<dyn CaptureSource>, mc_captions::SourceError> {
    mc_captions::win_uia::source(
        platform,
        selectors,
        NodeMode::All,
        mc_captions::win_uia::WinOptions::default(),
    )
    .map(|s| Box::new(s) as Box<dyn CaptureSource>)
    .map_err(|e| mc_captions::SourceError::Fatal(e.to_string()))
}

#[cfg(target_os = "macos")]
fn native_source(
    platform: Platform,
    selectors: Selectors,
) -> Result<Box<dyn CaptureSource>, mc_captions::SourceError> {
    mc_captions::mac_ax::source(
        platform,
        selectors,
        NodeMode::All,
        mc_captions::mac_ax::MacOptions::default(),
    )
    .map(|s| Box::new(s) as Box<dyn CaptureSource>)
    .map_err(|e| mc_captions::SourceError::Fatal(e.to_string()))
}

/// The source factory plus the platform and source names for the manifest.
fn build_source(
    source: &Source,
    selectors: &Selectors,
) -> Result<(SourceFactory, String, String), CliError> {
    match source {
        Source::Fake(dir) => {
            let fake = fake_from_corpus(dir)?;
            let source = fake.source();
            Ok((once(source), "fake".to_string(), "FakeSource".to_string()))
        }
        #[cfg(any(windows, target_os = "macos"))]
        Source::Native(platform) => {
            // Built inside the factory, on the worker thread: a rebuild after a hang gets a fresh backend.
            let selectors = selectors.clone();
            let platform = *platform;
            Ok((
                Box::new(move || native_source(platform, selectors.clone())),
                platform.name().to_string(),
                platform.label().to_string(),
            ))
        }
        Source::FakeUia(path) => {
            let text = std::fs::read_to_string(path).map_err(|e| {
                CliError::not_found(format!("{}: {e}", path.display()), "pass a fixture file")
            })?;
            let fixture: Fixture = serde_json::from_str(&text)
                .map_err(|e| CliError::usage(format!("{}: {e}", path.display())))?;
            let platform = Platform::parse(&fixture.platform).ok_or_else(|| {
                CliError::usage(format!(
                    "{}: unknown platform {:?} (teams, zoom, meet, teams-web or slack)",
                    path.display(),
                    fixture.platform
                ))
            })?;
            let backend = FakeBackend::from_fixture(fixture).auto_advance(true);
            let uia = UiaSource::new(backend, platform, selectors.clone(), NodeMode::All);
            let label = uia.name().to_string();
            Ok((once(uia), platform.name().to_string(), label))
        }
    }
}

fn ask_consent(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<&'static str, CliError> {
    if ctx.args.flag("consent") {
        return Ok("flag");
    }
    if ctx.env.stdin_tty
        && let Some(input) = io.input.as_mut()
    {
        write!(io.err, "Record the captions of this meeting? [s/N] ")?;
        io.err.flush()?;
        let mut answer = String::new();
        input.read_line(&mut answer)?;
        if matches!(
            answer.trim().to_lowercase().as_str(),
            "s" | "si" | "sí" | "y" | "yes"
        ) {
            return Ok("prompt");
        }
        return Err(CliError::usage("recording declined"));
    }
    Err(CliError::usage(
        "recording needs consent for this meeting: pass --consent (or --no-record for a live view only)",
    ))
}

/// Consent to also record the audio of this meeting. `None`: declined at the
/// prompt (captions go on without audio).
fn ask_audio_consent(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<bool, CliError> {
    if ctx.args.flag("consent-audio") {
        return Ok(true);
    }
    if ctx.env.stdin_tty
        && let Some(input) = io.input.as_mut()
    {
        write!(io.err, "Also record the AUDIO of this meeting? [s/N] ")?;
        io.err.flush()?;
        let mut answer = String::new();
        input.read_line(&mut answer)?;
        let yes = matches!(
            answer.trim().to_lowercase().as_str(),
            "s" | "si" | "sí" | "y" | "yes"
        );
        if !yes {
            writeln!(io.err, "note: audio declined; recording captions only")?;
        }
        return Ok(yes);
    }
    Err(CliError::usage(
        "recording audio needs consent for this meeting: pass --consent-audio",
    ))
}

/// The audio backend from `--audio-source` / `capture.audio_source`: `wav:`
/// plays files, none or `default` uses the machine's devices (absent on Linux).
pub fn audio_backend(ctx: &Ctx<'_>) -> Result<Option<Arc<dyn AudioBackend>>, CliError> {
    let source = ctx
        .args
        .value("audio-source")
        .map(str::to_string)
        .or_else(|| ctx.cfg.capture.audio_source.clone());
    match source.as_deref() {
        None | Some("default") => Ok(mc_audio::default_backend()),
        Some(spec) => {
            let files = spec.strip_prefix("wav:").ok_or_else(|| {
                CliError::usage("--audio-source takes default or wav:<mic.wav>[,<system.wav>]")
            })?;
            let backend = WavFileBackend::parse(files)
                .map_err(|e| CliError::usage(format!("--audio-source: {e}")))?;
            Ok(Some(Arc::new(backend)))
        }
    }
}

/// The audio part of the capture spec from `--audio` and its options.
fn audio_spec(ctx: &Ctx<'_>) -> Result<AudioSpec, CliError> {
    let backend = audio_backend(ctx)?;
    let mut audio = AudioSpec::new(backend);
    audio.mic_device = ctx
        .args
        .value("mic-device")
        .map(str::to_string)
        .or_else(|| ctx.cfg.capture.mic_device.clone());
    audio.system_device = ctx
        .args
        .value("system-device")
        .map(str::to_string)
        .or_else(|| ctx.cfg.capture.system_device.clone());
    if ctx.args.flag("live-asr") {
        audio.live_asr = Some(LiveAsrSpec {
            factory: super::transcribe::live_factory(ctx)?,
            cfg: mc_asr::LiveConfig {
                prompt: mc_asr::glossary_prompt(&prompt_terms(ctx)?, mc_asr::MAX_PROMPT_CHARS),
                ..mc_asr::LiveConfig::default()
            },
        });
    }
    Ok(audio)
}

fn event_json(event: &LiveEvent, mono_ms: f64) -> Value {
    let mut value = match event {
        LiveEvent::Started {
            session_dir,
            session_id,
            source,
        } => {
            json!({"event": "started", "session_dir": session_dir.as_ref().map(|d| d.display().to_string()),
                    "session_id": session_id, "source": source})
        }
        LiveEvent::Session { dir, id } => {
            json!({"event": "session", "session_dir": dir.display().to_string(), "session_id": id})
        }
        LiveEvent::Source(d) => match d {
            Discovery::Searching => json!({"event": "source", "state": "searching"}),
            Discovery::Attached { window } => {
                json!({"event": "source", "state": "attached", "window": window})
            }
            Discovery::Ended => json!({"event": "source", "state": "ended"}),
        },
        LiveEvent::Segment { idx, segment: s } => json!({
            "event": "segment", "idx": idx, "speaker": s.speaker, "speaker_raw": s.speaker_raw, "text": s.text,
            "first_mono_ms": s.first_mono_ms, "last_mono_ms": s.last_mono_ms,
            "committed_mono_ms": s.committed_mono_ms, "committed_wall_ms": s.committed_wall_ms,
        }),
        LiveEvent::Open(lines) => json!({"event": "open", "lines": lines}),
        LiveEvent::NoCaptions { secs } => json!({"event": "no_captions", "secs": secs}),
        LiveEvent::WriteError(m) => json!({"event": "write_error", "message": m}),
        LiveEvent::SourceError(m) => json!({"event": "source_error", "message": m}),
        LiveEvent::Audio(m) => json!({"event": "audio", "message": m}),
        LiveEvent::Rec(on) => json!({"event": "rec", "recording": on}),
        LiveEvent::Asr(s) => json!({
            "event": "asr", "ch": s.ch, "start_ms": s.start_ms, "end_ms": s.end_ms,
            "text": s.text, "lang": s.lang, "conf": s.conf,
        }),
        LiveEvent::Ledger(update) => json!({
            "event": "ledger",
            "first_id": update.event.first_id, "last_id": update.event.last_id,
            "segments": update.event.segments,
            "accepted": update.event.accepted, "rejected": update.event.rejected,
            "counts": ledger_counts(&update.ledger),
            "changed": update.changed,
        }),
        LiveEvent::AnalystError(m) => json!({"event": "analyst_error", "message": m}),
        LiveEvent::Note(n) => json!({
            "event": "note", "text": n.text, "mono_ms": n.mono_ms, "wall_ms": n.wall_ms,
        }),
        LiveEvent::Mark { mono_ms, wall_ms } => {
            json!({"event": "mark", "mono_ms": mono_ms, "wall_ms": wall_ms})
        }
        LiveEvent::Fixed { heard, correct } => {
            json!({"event": "fixed", "heard": heard, "correct": correct})
        }
        LiveEvent::Named { from, to } => json!({"event": "named", "from": from, "to": to}),
        LiveEvent::Ended {
            reason,
            frames,
            records,
        } => {
            json!({"event": "ended", "reason": reason.as_str(), "frames": frames, "records": records})
        }
    };
    // Notes and marks carry the moment they were made.
    if value.get("mono_ms").is_none() {
        value["mono_ms"] = json!(mono_ms);
    }
    value
}

fn print_human(event: &LiveEvent, recording: bool, io: &mut Io<'_>) -> std::io::Result<()> {
    match event {
        LiveEvent::Started {
            session_dir,
            source,
            ..
        } => match session_dir {
            Some(dir) => writeln!(io.out, "recording ({source}) to {}", dir.display()),
            None if recording => writeln!(io.out, "waiting for the meeting ({source})"),
            None => writeln!(io.out, "live view ({source}), nothing is recorded"),
        },
        LiveEvent::Session { dir, .. } => writeln!(io.out, "recording to {}", dir.display()),
        LiveEvent::Source(state) => writeln!(io.out, "source: {state:?}"),
        LiveEvent::Segment { segment: s, .. } => {
            writeln!(io.out, "[{}] {}", fmt_rel(rel_ms(s)), s.line())
        }
        LiveEvent::Open(_) => Ok(()),
        LiveEvent::NoCaptions { secs } => {
            writeln!(
                io.err,
                "warning: no captions for {secs}s; are live captions on?"
            )
        }
        LiveEvent::WriteError(m) => writeln!(io.err, "warning: write error: {m}"),
        LiveEvent::SourceError(m) => writeln!(io.err, "warning: {m}"),
        LiveEvent::Audio(m) => writeln!(io.err, "warning: {m}"),
        LiveEvent::Rec(on) => writeln!(io.out, "audio: {}", if *on { "REC" } else { "stopped" }),
        LiveEvent::Ledger(update) => writeln!(
            io.out,
            "ledger: segments #{}-#{} analysed; {} items changed, {} in total",
            update.event.first_id,
            update.event.last_id,
            update.changed.len(),
            update.ledger.item_count()
        ),
        LiveEvent::AnalystError(m) => writeln!(io.err, "warning: analyst: {m}"),
        LiveEvent::Note(n) => writeln!(io.out, "note: {}", n.text),
        LiveEvent::Fixed { heard, correct } => writeln!(io.out, "fixed: {heard} -> {correct}"),
        LiveEvent::Named { from, to } => writeln!(io.out, "named: {from} is {to}"),
        LiveEvent::Mark { mono_ms, .. } => {
            writeln!(io.out, "mark at {}", fmt_rel(Some(*mono_ms)))
        }
        LiveEvent::Asr(s) => writeln!(
            io.out,
            "[{} {}] ~ {}",
            fmt_rel(Some(s.start_ms)),
            s.ch,
            s.text
        ),
        LiveEvent::Ended {
            reason,
            frames,
            records,
        } => writeln!(
            io.out,
            "ended ({}): {frames} frames, {records} records",
            reason.as_str()
        ),
    }
}

/// Everything `mc capture` and `mc tui` share: validated options, consent,
/// and the capture spec. Prompts (consent) happen here, before any terminal
/// mode change.
pub struct Prepared {
    pub spec: CaptureSpec,
    pub corpus_root: std::path::PathBuf,
    pub recording: bool,
    /// The live analyst will run.
    pub analyst: bool,
}

/// The live analyst from `--analyst` or `capture.analyst`. A configured one is only
/// skipped when nothing is recorded; asking for it explicitly then is an error.
fn analyst_spec(ctx: &Ctx<'_>, recording: bool) -> Result<Option<AnalystSpec>, CliError> {
    let asked = ctx.args.flag("analyst");
    if asked && !recording {
        return Err(CliError::usage(
            "--analyst has no effect with --no-record: the ledger lives in the session folder",
        ));
    }
    let enabled = asked || (ctx.cfg.capture.analyst && recording);
    if !enabled {
        for option in ["llm-provider", "llm-model", "glossary", "context"] {
            if ctx.args.value(option).is_some() {
                return Err(CliError::usage(format!("--{option} needs --analyst")));
            }
        }
    }
    if !enabled {
        return Ok(None);
    }
    // Checked now, before any prompt, so a bad model setting fails before the meeting.
    let llm = make_llm(ctx)?;
    let cfg = ctx.cfg.clone();
    let (memory_cfg, file) = (ctx.cfg.clone(), glossary_file(ctx));
    Ok(Some(AnalystSpec {
        llm,
        cfg: AnalystConfig::default(),
        background: background(ctx)?,
        // A lookup that fails only means the meeting starts without carried commitments.
        carry: Some(Box::new(move |dir| {
            carry_for(&cfg, dir).map(|c| c.carried).unwrap_or_default()
        })),
        // What the user corrects in the memory during the meeting reaches the next window;
        // a memory that cannot be read leaves the last glossary in place.
        refresh: Some(Box::new(move |background| {
            if let Ok(glossary) = glossary_from(&memory_cfg, file.as_deref()) {
                background.glossary = glossary.terms();
                background.aliases = glossary.aliases();
            }
            if let Ok(homonyms) = memory_homonyms(&memory_cfg) {
                background.homonyms = homonyms;
            }
        })),
        provider: ctx.cfg.llm.provider.clone(),
        model: ctx.cfg.llm.model.clone(),
    }))
}

/// A millisecond option: the flag, else the configured value, checked against `range`.
fn ms_option(
    ctx: &Ctx<'_>,
    flag: &str,
    configured: u64,
    range: &std::ops::RangeInclusive<u64>,
    what: &str,
) -> Result<u64, CliError> {
    let value = match ctx.args.value(flag) {
        Some(v) => v
            .parse::<u64>()
            .map_err(|_| CliError::usage(format!("--{flag}: {v:?} is not a whole number")))?,
        None => configured,
    };
    if !range.contains(&value) {
        return Err(CliError::usage(format!(
            "{what} must be between {} and {} ms (--{flag} or capture.{})",
            range.start(),
            range.end(),
            flag.replace('-', "_"),
        )));
    }
    Ok(value)
}

pub fn prepare(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<Prepared, CliError> {
    if !ctx.args.positional.is_empty() {
        return Err(CliError::usage(
            "usage: eli5 capture --source fake:<corpus-dir> [options]",
        ));
    }
    let source_spec = ctx
        .args
        .value("source")
        .map(str::to_string)
        .or_else(|| ctx.cfg.capture.source.clone())
        .ok_or_else(|| CliError::usage("--source is required (for example fake:<corpus-dir>)"))?;
    let source = parse_source(&source_spec)?;
    let poll_ms = ms_option(
        ctx,
        "poll-ms",
        ctx.cfg.capture.poll_ms,
        &POLL_MS_RANGE,
        "poll interval",
    )?;
    let poll_timeout_ms = ms_option(
        ctx,
        "poll-timeout-ms",
        ctx.cfg.capture.poll_timeout_ms,
        &POLL_TIMEOUT_MS_RANGE,
        "source timeout",
    )?;
    let until = ctx
        .args
        .value("until")
        .map(|v| {
            v.parse::<u64>()
                .map_err(|_| CliError::usage(format!("--until: {v:?} is not a whole number")))
        })
        .transpose()?;
    let recording = !ctx.args.flag("no-record");
    if !recording && ctx.args.value("name").is_some() {
        return Err(CliError::usage("--name has no effect with --no-record"));
    }
    if ctx.args.flag("consent") && !recording {
        return Err(CliError::usage("--consent and --no-record are exclusive"));
    }
    let want_audio = ctx.args.flag("audio");
    if !want_audio {
        for option in ["consent-audio", "live-asr"] {
            if ctx.args.flag(option) {
                return Err(CliError::usage(format!("--{option} needs --audio")));
            }
        }
        for option in [
            "audio-source",
            "mic-device",
            "system-device",
            "asr-engine",
            "asr-model",
        ] {
            if ctx.args.value(option).is_some() {
                return Err(CliError::usage(format!("--{option} needs --audio")));
            }
        }
    } else if !recording {
        return Err(CliError::usage("--audio has no effect with --no-record"));
    }
    if !ctx.args.flag("live-asr") {
        for option in ["asr-engine", "asr-model"] {
            if ctx.args.value(option).is_some() {
                return Err(CliError::usage(format!("--{option} needs --live-asr")));
            }
        }
    }

    let analyst = analyst_spec(ctx, recording)?;
    let corpus_root = ctx.cfg.corpus_dir();
    let (factory, platform, capture_source) = build_source(&source, &ctx.cfg.capture.selectors)?;
    let audio = if want_audio {
        // Fail on a bad --audio-source before any prompt.
        Some(audio_spec(ctx)?)
    } else {
        None
    };
    let consent = if recording {
        Some(ask_consent(ctx, io)?)
    } else {
        None
    };
    let audio = match audio {
        Some(spec) if ask_audio_consent(ctx, io)? => Some(spec),
        _ => None,
    };

    for unclean in scan_unclean(&corpus_root) {
        // A killed recording leaves WAV headers that claim no audio.
        let repaired = repair_session_audio(&unclean.dir);
        if ctx.json {
            writeln!(
                io.out,
                "{}",
                json!({"event": "unclean_session", "mono_ms": 0.0,
                       "session_dir": unclean.dir.display().to_string(),
                       "session_id": unclean.session_id})
            )?;
        } else {
            writeln!(
                io.err,
                "note: {} did not end cleanly; `mc run` will flush it",
                unclean.dir.display()
            )?;
            if !repaired.is_empty() {
                writeln!(io.err, "      repaired audio: {}", repaired.join(", "))?;
            }
        }
    }

    let spec = CaptureSpec {
        source: factory,
        cfg: LiveConfig {
            poll: Duration::from_millis(poll_ms),
            poll_timeout: Duration::from_millis(poll_timeout_ms),
            // The policy goes into the manifest so `replay --verify` is exact;
            // the other analysis settings apply later, in `mc run`.
            reconciler: mc_reconcile::ReconcilerConfig {
                policy: ctx.cfg.policy_for(&platform),
                ..mc_reconcile::ReconcilerConfig::default()
            },
            debounce: DebounceClock::Mono,
            ..LiveConfig::default()
        },
        record: consent.map(|method| RecordSpec {
            platform,
            capture_source,
            // Only a name the user chose is pinned; the source's name fills in.
            meeting_name: ctx.args.value("name").map(str::to_string),
            consent_method: method,
            audio,
        }),
        stop_file: true,
        until: until.map(Duration::from_secs),
        analyst,
    };
    Ok(Prepared {
        analyst: spec.analyst.is_some(),
        spec,
        corpus_root,
        recording,
    })
}

/// What the user types while capturing on a terminal (each plus Enter): `q`, `n <text>`, `m`,
/// `t <heard> => <correct>` to correct a term and `s <caption name> => <name>` to name a speaker.
enum Command {
    Note(String),
    Mark,
    Fix { heard: String, correct: String },
    Name { from: String, to: String },
}

fn parse_command(line: &str) -> Option<Command> {
    let line = line.trim();
    if line.eq_ignore_ascii_case("m") {
        return Some(Command::Mark);
    }
    let mut chars = line.chars();
    let letter = chars.next()?.to_ascii_lowercase();
    let rest = chars.as_str();
    // The letter alone, then what it carries.
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let pair = || {
        let (a, b) = rest.split_once("=>")?;
        let (a, b) = (a.trim().to_string(), b.trim().to_string());
        (!a.is_empty() && !b.is_empty()).then_some((a, b))
    };
    match letter {
        'n' => Some(Command::Note(rest.trim().to_string())),
        't' => pair().map(|(heard, correct)| Command::Fix { heard, correct }),
        's' => pair().map(|(from, to)| Command::Name { from, to }),
        _ => None,
    }
}

/// Reads the terminal until `q` or the end of input, which stops the capture.
fn read_commands(
    mut input: Box<dyn std::io::BufRead + Send>,
    stop: Arc<AtomicBool>,
    tx: Sender<Command>,
) {
    let mut line = String::new();
    loop {
        line.clear();
        match input.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) if line.trim().eq_ignore_ascii_case("q") => break,
            Ok(_) => {
                if let Some(command) = parse_command(&line) {
                    let _ = tx.send(command);
                }
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
}

/// Where typed commands come from: the terminal, or a pipe when `--stdin-commands` asks for it.
fn command_input(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Option<Box<dyn std::io::BufRead + Send>> {
    if ctx.env.stdin_tty {
        return io.input.take();
    }
    ctx.args.flag("stdin-commands").then(|| {
        io.input
            .take()
            .unwrap_or_else(|| Box::new(std::io::BufReader::new(std::io::stdin())))
    })
}

/// Writes one event to stdout (NDJSON or text) and flushes it.
fn show(
    capture: &Capture,
    event: &LiveEvent,
    recording: bool,
    json: bool,
    io: &mut Io<'_>,
) -> std::io::Result<()> {
    if json {
        writeln!(io.out, "{}", event_json(event, capture.mono_ms()))
    } else {
        print_human(event, recording, io)
    }
    .and_then(|()| io.out.flush())
}

/// Carries out what was typed and returns the event to show (or why it could not be done).
/// Carries out what was typed and returns the event to show (or why it could not be done). A
/// correction goes to the session, for this meeting's analyst, and to the memory, for the next.
fn apply(capture: &Capture, cfg: &Config, command: Command) -> LiveEvent {
    let done = match command {
        Command::Note(text) => capture.add_note(&text),
        Command::Mark => capture.mark(),
        Command::Fix { heard, correct } => capture.fix_term(&heard, &correct).and_then(|()| {
            let mut memory = open_memory(cfg).map_err(|e| e.message().to_string())?;
            teach_term(&mut memory, &correct, Some(&heard), None)
                .map_err(|e| e.message().to_string())?;
            Ok(LiveEvent::Fixed { heard, correct })
        }),
        Command::Name { from, to } => capture.rename_speaker(&from, &to).and_then(|_| {
            teach_person(cfg, &to, &from).map_err(|e| e.message().to_string())?;
            Ok(LiveEvent::Named { from, to })
        }),
    };
    done.unwrap_or_else(LiveEvent::SourceError)
}

pub fn run(ctx: Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    let prepared = prepare(&ctx, io)?;
    let analyst = prepared.analyst;
    let capture = start(prepared.spec, &prepared.corpus_root)?;

    // `q` + Enter (or EOF) on a terminal stops cleanly; `n <text>` and `m` annotate.
    let (typed, commands): (Sender<Command>, Receiver<Command>) = channel();
    if let Some(input) = command_input(&ctx, io) {
        let stop = capture.stop_flag();
        std::thread::spawn(move || read_commands(input, stop, typed));
    }

    let mut output_ok = true;
    // `ended` stays the last thing said, after the analyst's tail.
    let mut ended = None;
    'outer: loop {
        // Read before polling: anything sent after this is picked up next round.
        let finished = capture.is_finished();
        let mut events = capture.poll(Duration::from_millis(100));
        let disconnected = events.is_empty() && finished;
        events.extend(
            commands
                .try_iter()
                .map(|command| apply(&capture, &ctx.cfg, command)),
        );
        for event in events {
            if matches!(event, LiveEvent::Ended { .. }) {
                ended = Some(event);
                break 'outer;
            }
            if output_ok && show(&capture, &event, prepared.recording, ctx.json, io).is_err() {
                // The reader went away (`| head`): stop recording cleanly
                // instead of abandoning the session.
                output_ok = false;
                capture.request_stop();
            }
        }
        if disconnected {
            break;
        }
    }

    if analyst {
        if output_ok && !ctx.json {
            writeln!(io.err, "finishing the analysis of the last segments...")?;
        }
        // The window says what the wait is for: up to ANALYST_TAIL_WAIT with nothing else to show.
        if output_ok && ctx.json {
            writeln!(io.out, "{}", json!({"event": "closing", "step": "analyst"}))?;
            io.out.flush()?;
        }
        for event in capture.close_analyst(ANALYST_TAIL_WAIT) {
            output_ok =
                output_ok && show(&capture, &event, prepared.recording, ctx.json, io).is_ok();
        }
    }
    if let Some(event) = ended {
        output_ok = output_ok && show(&capture, &event, prepared.recording, ctx.json, io).is_ok();
    }
    let summary = capture.finish().map_err(|_| {
        CliError::runtime(
            "panic",
            "the capture thread panicked; the session folder keeps what was recorded",
        )
    })?;
    if output_ok {
        StubEnd.on_end(&summary, io, ctx.json)?;
    }
    Ok(if summary.reason.is_clean() { 0 } else { 3 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_commands_carry_their_text_and_a_correction_needs_both_sides() {
        assert!(matches!(parse_command("m"), Some(Command::Mark)));
        assert!(
            matches!(parse_command("n revisar costos"), Some(Command::Note(t)) if t == "revisar costos")
        );
        assert!(matches!(
            parse_command("t beri dex => Verídex"),
            Some(Command::Fix { heard, correct }) if heard == "beri dex" && correct == "Verídex"
        ));
        assert!(matches!(
            parse_command("S MEDINA Raul=>Raul Medina"),
            Some(Command::Name { from, to }) if from == "MEDINA Raul" && to == "Raul Medina"
        ));
        assert!(parse_command("t solo").is_none());
        assert!(parse_command("t => x").is_none());
        assert!(parse_command("nota sin espacio").is_none());
    }
}
