//! Runs: deterministic analyses derived from a recorded session.
//!
//! A run replays the session's captions through the reconciler with a chosen
//! configuration and writes the result next to the corpus
//! (`<corpus>/runs/<run_id>/`). Everything is derived: the JSONL corpus stays
//! the source of truth and a run can be deleted and rebuilt byte for byte.

mod render;

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use mc_core::{Segment, Stamp};
use mc_reconcile::{Policy, ReconcilerConfig};
use mc_store::jsonl::{CaptionLine, ParsedLine, read_bytes_at};
use mc_store::manifest::Manifest;
use mc_store::{FNV_OFFSET, RunRow, SegmentRow, Store, StoreError, fnv_extend};
use serde_json::{Value, json};

use crate::policy::default_policy;
use crate::replay::{ReplayError, ReplayOp, compare, reconstruct, run_with};
use crate::sidecar::{SpeakerNames, display_name, read_speaker_names};

pub use render::{
    MdMeta, fmt_rel, md_escape, rel_ms, render_md, render_segments_jsonl, render_txt,
};

const RUN_FORMAT: &str = "mc-run/1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushMode {
    /// Flush at the manifest's end; for an unfinished session flush at the last
    /// frame instead, so the transcript never loses its final lines.
    Auto,
    /// Flush only when the manifest says the session ended (what `verify` does).
    ManifestOnly,
}

#[derive(Debug, Clone)]
pub struct RunConfig {
    pub reconciler: ReconcilerConfig,
    pub flush: FlushMode,
    /// The user chose no policy: the session's platform decides (see [`default_policy`]).
    pub policy_by_platform: bool,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            reconciler: ReconcilerConfig::default(),
            flush: FlushMode::Auto,
            policy_by_platform: false,
        }
    }
}

/// Stable text for a policy: it feeds run ids, so it must not follow renames.
pub fn policy_name(policy: Policy) -> &'static str {
    match policy {
        Policy::V1Legacy => "v1_legacy",
        Policy::V2 => "v2",
    }
}

pub fn parse_policy(name: &str) -> Option<Policy> {
    match name {
        "v1_legacy" => Some(Policy::V1Legacy),
        "v2" => Some(Policy::V2),
        _ => None,
    }
}

fn flush_name(flush: FlushMode) -> &'static str {
    match flush {
        FlushMode::Auto => "auto",
        FlushMode::ManifestOnly => "manifest_only",
    }
}

fn config_json(cfg: &RunConfig) -> Value {
    let mut config = json!({
        "debounce_s": cfg.reconciler.debounce_s,
        "merge_similarity": cfg.reconciler.merge_similarity,
        "recent_capacity": cfg.reconciler.recent_capacity,
        "policy": policy_name(cfg.reconciler.policy),
        "flush": flush_name(cfg.flush),
    });
    // V1 reports keep their bytes; only V2 depends on its own revision.
    if cfg.reconciler.policy == Policy::V2 {
        config["v2_rev"] = json!(mc_reconcile::V2_REV);
    }
    config
}

/// What identifies a run's input: the captions up to the last newline.
#[derive(Debug, Clone)]
pub struct RunInput<'a> {
    pub session_id: &'a str,
    pub bytes: usize,
    pub fnv: u64,
    /// FNV of the serialised manifest: its fields (title, clocks...) feed the outputs.
    pub manifest_fnv: u64,
    pub ended_wall_ms: Option<i64>,
    /// FNV of the speaker names the user chose (none: the captions' own names).
    pub names_fnv: Option<u64>,
}

/// `r` + 16 hex digits of FNV-64 over a canonical description of the inputs.
/// No clock enters it, so the same input and config always give the same id.
pub fn run_id(input: &RunInput<'_>, cfg: &RunConfig) -> String {
    let mut text = format!(
        "{RUN_FORMAT}\n{}\n{}\n{:016x}\n{:016x}\n{}\n{:016x}\n{:016x}\n{}\n{}\n{}\n{}\n",
        input.session_id,
        input.bytes,
        input.fnv,
        input.manifest_fnv,
        input
            .ended_wall_ms
            .map_or("-".to_string(), |v| v.to_string()),
        cfg.reconciler.debounce_s.to_bits(),
        cfg.reconciler.merge_similarity.to_bits(),
        cfg.reconciler.recent_capacity,
        policy_name(cfg.reconciler.policy),
        flush_name(cfg.flush),
        env!("CARGO_PKG_VERSION"),
    );
    // Only V2 depends on its own revision; V1 ids stay as they were.
    if cfg.reconciler.policy == Policy::V2 {
        text.push_str(&format!("v2rev={}\n", mc_reconcile::V2_REV));
    }
    // Only when names were given, so every id made without them stays as it was.
    if let Some(fnv) = input.names_fnv {
        text.push_str(&format!("names={fnv:016x}\n"));
    }
    format!("r{:016x}", fnv_extend(FNV_OFFSET, text.as_bytes()))
}

pub struct RunArtifacts {
    pub run_id: String,
    pub txt: String,
    pub md: String,
    pub segments_jsonl: String,
    pub events_jsonl: String,
    pub report: Value,
    pub report_json: String,
    pub segments: Vec<Segment>,
    /// Bytes after the last newline (a line still being written). Not part of
    /// the report: it does not affect the run id.
    pub tail_pending_bytes: usize,
    config: Value,
    input_bytes: usize,
    input_fnv: u64,
    session_id: String,
    ended_cleanly: bool,
    synthetic_flush: bool,
    content_div: usize,
    timing_div: usize,
    duration_ms: Option<i64>,
}

/// The names the user chose, for the text a person reads; segments keep what the platform said.
fn with_names(segments: &[Segment], names: &SpeakerNames) -> Vec<Segment> {
    segments
        .iter()
        .map(|segment| Segment {
            speaker: display_name(names, &segment.speaker).to_string(),
            ..segment.clone()
        })
        .collect()
}

fn names_fnv(names: &SpeakerNames) -> Option<u64> {
    (!names.is_empty()).then(|| {
        let text: String = names
            .iter()
            .map(|(from, to)| format!("{from}\n{to}\n"))
            .collect();
        fnv_extend(FNV_OFFSET, text.as_bytes())
    })
}

fn io_err(path: &Path, source: std::io::Error) -> ReplayError {
    ReplayError::Store(StoreError::io(path, source))
}

fn stamp_of(op: &ReplayOp) -> Option<Stamp> {
    match op {
        ReplayOp::Frame { stamp, .. } => Some(*stamp),
        ReplayOp::Flush { .. } => None,
    }
}

fn speaker_stats(segments: &[Segment]) -> Value {
    struct Stats {
        raw: Vec<String>,
        segments: usize,
        words: usize,
        chars: usize,
        first: Option<f64>,
        last: Option<f64>,
    }
    let mut order: Vec<String> = Vec::new();
    let mut stats: BTreeMap<String, Stats> = BTreeMap::new();
    for segment in segments {
        if !stats.contains_key(&segment.speaker) {
            order.push(segment.speaker.clone());
        }
        let entry = stats.entry(segment.speaker.clone()).or_insert(Stats {
            raw: Vec::new(),
            segments: 0,
            words: 0,
            chars: 0,
            first: None,
            last: None,
        });
        let raw = segment
            .speaker_raw
            .clone()
            .unwrap_or_else(|| segment.speaker.clone());
        if !entry.raw.contains(&raw) {
            entry.raw.push(raw);
        }
        entry.segments += 1;
        entry.words += segment.text.split_whitespace().count();
        entry.chars += segment.text.chars().count();
        if let Some(rel) = rel_ms(segment) {
            entry.first = Some(entry.first.map_or(rel, |f| f.min(rel)));
            entry.last = Some(entry.last.map_or(rel, |l| l.max(rel)));
        }
    }
    Value::Array(
        order
            .iter()
            .map(|name| {
                let s = &stats[name];
                json!({
                    "name": name,
                    "raw_names": s.raw,
                    "segments": s.segments,
                    "words": s.words,
                    "chars": s.chars,
                    "first_rel_ms": s.first,
                    "last_rel_ms": s.last,
                })
            })
            .collect(),
    )
}

/// Builds every output in memory. Pure: nothing is written.
pub fn build(dir: impl AsRef<Path>, cfg: &RunConfig) -> Result<RunArtifacts, ReplayError> {
    let dir = dir.as_ref();
    let manifest = Manifest::load(dir.join("manifest.json"))?;
    let mut cfg = cfg.clone();
    if cfg.policy_by_platform {
        cfg.reconciler.policy = default_policy(&manifest.platform_requested);
    }
    let cfg = &cfg;
    let captions_file = if manifest.captions.file.is_empty() {
        "captions.jsonl"
    } else {
        manifest.captions.file.as_str()
    };
    let path = dir.join(captions_file);
    let bytes = fs::read(&path).map_err(|e| io_err(&path, e))?;
    // An unfinished last line is still being written: it is not part of the run.
    let cut = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let consumed = &bytes[..cut];
    let fnv = fnv_extend(FNV_OFFSET, consumed);
    let parsed = read_bytes_at(consumed, true);
    let ops = reconstruct(&parsed)?;
    let names = read_speaker_names(dir);

    let id = run_id(
        &RunInput {
            session_id: &manifest.session_id,
            bytes: cut,
            fnv,
            manifest_fnv: fnv_extend(FNV_OFFSET, manifest.to_json()?.as_bytes()),
            ended_wall_ms: manifest.ended_wall_ms,
            names_fnv: names_fnv(&names),
        },
        cfg,
    );

    let last_frame = ops.iter().rev().find_map(stamp_of);
    let ended = manifest.ended_wall_ms.map(|wall_ms| Stamp {
        wall_ms,
        // Without a recorded end on the monotonic clock, the last frame is the
        // best estimate; zero would stamp the flushed lines at 00:00.
        mono_ms: manifest
            .ended_mono_ms
            .or(last_frame.map(|s| s.mono_ms))
            .unwrap_or(0.0),
    });
    // A session closed by a failure (panic, fatal source, lost writes) is
    // closed but not clean: its tail may be missing.
    let ended_cleanly = manifest.ended_cleanly()
        && manifest
            .end_reason
            .as_deref()
            .is_none_or(|r| matches!(r, "stop" | "source_ended"));
    let recorded_flush = ops.iter().any(|op| matches!(op, ReplayOp::Flush { .. }));
    let (flush_at, synthetic_flush) = match (ended, cfg.flush) {
        (Some(stamp), _) => (Some(stamp), false),
        (None, FlushMode::Auto) => (last_frame, last_frame.is_some() && !recorded_flush),
        (None, FlushMode::ManifestOnly) => (None, false),
    };
    let replay = run_with(&ops, &manifest, flush_at, cfg.reconciler.clone())?;
    // The recording never had a synthetic flush, so compare without it.
    let unflushed;
    let comparable = cfg.reconciler.policy == Policy::V1Legacy;
    let for_compare = if synthetic_flush && comparable {
        unflushed = run_with(&ops, &manifest, None, cfg.reconciler.clone())?;
        &unflushed
    } else {
        &replay
    };

    let file: Vec<CaptionLine> = parsed
        .lines
        .iter()
        .filter_map(|(_, l)| match l {
            ParsedLine::V2(line) => Some(line.clone()),
            ParsedLine::V1(_) => None,
        })
        .collect();
    let recorded = crate::replay::transcript_of(&file);
    let truncated = bytes.len() > cut || !ended_cleanly;
    // The recording was made by V1; a run with other rules differs from it by
    // design, so there is nothing meaningful to compare.
    let compared = if comparable {
        compare(&file, for_compare, truncated, &recorded, false)
    } else {
        crate::replay::VerifyReport::default()
    };

    let duration_ms = manifest
        .ended_mono_ms
        .or_else(|| file.iter().map(|l| l.mono_ms).reduce(f64::max))
        .filter(|v| v.is_finite())
        .map(|v| v.round() as i64);

    let shown = with_names(&replay.segments, &names);
    let txt = render_txt(&shown);
    let md = render_md(
        &MdMeta {
            manifest: &manifest,
            run_id: &id,
            ended_cleanly,
            synthetic_flush,
            duration_ms,
        },
        &shown,
    );
    let segments_jsonl = render_segments_jsonl(&replay.segments, &names)?;
    let mut events_jsonl = String::new();
    for line in &replay.lines {
        events_jsonl.push_str(&line.to_value().to_string());
        events_jsonl.push('\n');
    }

    let mut decisions: BTreeMap<String, usize> = BTreeMap::new();
    for line in &replay.lines {
        let name = if line.decision.starts_with("merge_") {
            "merge"
        } else {
            line.decision.as_str()
        };
        *decisions.entry(name.to_string()).or_default() += 1;
    }
    let config = config_json(cfg);
    let first: Vec<Value> = compared
        .content
        .iter()
        .chain(&compared.timing)
        .take(5)
        .map(|d| json!({"index": d.index, "field": d.field, "expected": d.expected, "actual": d.actual}))
        .collect();
    let report = json!({
        "format": 1,
        "run_id": id,
        "session_id": manifest.session_id,
        "tool_version": env!("CARGO_PKG_VERSION"),
        "config": config,
        "input": {
            "captions_file": captions_file,
            "bytes": cut,
            "fnv": format!("{fnv:016x}"),
            "lines": parsed.lines.len(),
            "bad_lines": parsed.bad.len(),
        },
        "ended_cleanly": ended_cleanly,
        "synthetic_flush": synthetic_flush,
        "duration_ms": duration_ms,
        "frames": ops.iter().filter(|op| matches!(op, ReplayOp::Frame { .. })).count(),
        "records": replay.lines.len(),
        "segments": replay.segments.len(),
        "decisions": decisions,
        "speakers": speaker_stats(&replay.segments),
        "comparable": comparable,
        "divergences": {
            "content": compared.content.len(),
            "timing": compared.timing.len(),
            "first": first,
        },
        "open_lines": replay.open_lines,
        "empty": replay.segments.is_empty(),
    });
    let report_json = serde_json::to_string_pretty(&report).map_err(StoreError::from)? + "\n";

    Ok(RunArtifacts {
        run_id: id,
        txt,
        md,
        segments_jsonl,
        events_jsonl,
        report,
        report_json,
        segments: replay.segments,
        tail_pending_bytes: bytes.len() - cut,
        config,
        input_bytes: cut,
        input_fnv: fnv,
        session_id: manifest.session_id.clone(),
        ended_cleanly,
        synthetic_flush,
        content_div: compared.content.len(),
        timing_div: compared.timing.len(),
        duration_ms,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunAction {
    Created,
    Unchanged,
}

#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub run_id: String,
    pub path: PathBuf,
    pub action: RunAction,
    pub report: Value,
    pub tail_pending_bytes: usize,
}

fn has_report_for(dir: &Path, run_id: &str) -> bool {
    fs::read_to_string(dir.join("report.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .is_some_and(|v| v["run_id"] == run_id)
}

fn write_file(dir: &Path, name: &str, content: &str) -> Result<(), ReplayError> {
    let path = dir.join(name);
    let mut file = fs::File::create(&path).map_err(|e| io_err(&path, e))?;
    file.write_all(content.as_bytes())
        .map_err(|e| io_err(&path, e))?;
    file.sync_all().map_err(|e| io_err(&path, e))
}

static TMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Writes `<dir>/runs/<run_id>/` atomically: files go to a temporary
/// directory that is renamed into place, so a crash never leaves a half run.
pub fn write(dir: impl AsRef<Path>, artifacts: &RunArtifacts) -> Result<RunOutcome, ReplayError> {
    write_with(dir, artifacts, false)
}

/// `write`; with `force` an existing run of the same id is replaced (outputs
/// of an older build of the tool share the id).
pub fn write_with(
    dir: impl AsRef<Path>,
    artifacts: &RunArtifacts,
    force: bool,
) -> Result<RunOutcome, ReplayError> {
    let runs = dir.as_ref().join("runs");
    fs::create_dir_all(&runs).map_err(|e| io_err(&runs, e))?;
    let final_dir = runs.join(&artifacts.run_id);
    let outcome = |action| RunOutcome {
        run_id: artifacts.run_id.clone(),
        path: final_dir.clone(),
        action,
        report: artifacts.report.clone(),
        tail_pending_bytes: artifacts.tail_pending_bytes,
    };
    // Only clearly abandoned temporary directories are swept: a live writer
    // (another process, or another thread of this one) has a young one.
    if let Ok(entries) = fs::read_dir(&runs) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !(name.starts_with('.') && name.contains(".tmp-")) {
                continue;
            }
            let abandoned = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > std::time::Duration::from_secs(3600));
            if abandoned {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
    if !force && has_report_for(&final_dir, &artifacts.run_id) {
        return Ok(outcome(RunAction::Unchanged));
    }
    let tmp = runs.join(format!(
        ".{}.tmp-{}-{}",
        artifacts.run_id,
        std::process::id(),
        TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    fs::create_dir(&tmp).map_err(|e| io_err(&tmp, e))?;
    let result = (|| {
        write_file(&tmp, "transcript.txt", &artifacts.txt)?;
        write_file(&tmp, "transcript.md", &artifacts.md)?;
        write_file(&tmp, "segments.jsonl", &artifacts.segments_jsonl)?;
        write_file(&tmp, "events.jsonl", &artifacts.events_jsonl)?;
        // The report goes last: its presence marks the run as complete.
        write_file(&tmp, "report.json", &artifacts.report_json)
    })();
    if let Err(err) = result {
        let _ = fs::remove_dir_all(&tmp);
        return Err(err);
    }
    // A concurrent writer may have completed the same run meanwhile.
    if !force && has_report_for(&final_dir, &artifacts.run_id) {
        let _ = fs::remove_dir_all(&tmp);
        return Ok(outcome(RunAction::Unchanged));
    }
    // A plain file is never ours: on Windows rename would silently replace it.
    if fs::symlink_metadata(&final_dir).is_ok_and(|m| m.is_file()) {
        let _ = fs::remove_dir_all(&tmp);
        return Err(io_err(
            &final_dir,
            std::io::Error::new(std::io::ErrorKind::AlreadyExists, "not a run directory"),
        ));
    }
    // An incomplete (or forced-over) directory is replaced.
    let _ = fs::remove_dir_all(&final_dir);
    // Antivirus scanners on Windows briefly hold freshly written files.
    let mut attempt = 0;
    loop {
        match fs::rename(&tmp, &final_dir) {
            Ok(()) => break,
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied && attempt < 5 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(err) => {
                let _ = fs::remove_dir_all(&tmp);
                if !force && has_report_for(&final_dir, &artifacts.run_id) {
                    return Ok(outcome(RunAction::Unchanged));
                }
                return Err(io_err(&final_dir, err));
            }
        }
    }
    Ok(outcome(RunAction::Created))
}

fn segment_rows(segments: &[Segment]) -> Vec<SegmentRow> {
    segments
        .iter()
        .map(|s| SegmentRow {
            speaker: s.speaker.clone(),
            speaker_raw: s.speaker_raw.clone(),
            text: s.text.clone(),
            first_mono_ms: s.first_mono_ms,
            last_mono_ms: s.last_mono_ms,
            committed_mono_ms: s.committed_mono_ms,
            committed_wall_ms: s.committed_wall_ms,
        })
        .collect()
}

/// Imports the session into `store` (when given), builds the run, writes it and
/// indexes it. Re-running with the same input changes nothing.
pub fn execute(
    dir: impl AsRef<Path>,
    cfg: &RunConfig,
    store: Option<&mut Store>,
) -> Result<RunOutcome, ReplayError> {
    execute_with(dir, cfg, store, false)
}

/// `execute`; `force` rewrites an existing run's files (see [`write_with`]).
pub fn execute_with(
    dir: impl AsRef<Path>,
    cfg: &RunConfig,
    store: Option<&mut Store>,
    force: bool,
) -> Result<RunOutcome, ReplayError> {
    let dir = dir.as_ref();
    let mut store = store;
    if let Some(store) = store.as_deref_mut() {
        store.import_session(dir)?;
    }
    let artifacts = build(dir, cfg)?;
    let outcome = write_with(dir, &artifacts, force)?;
    if let Some(store) = store {
        let row = RunRow {
            run_id: artifacts.run_id.clone(),
            session_id: artifacts.session_id.clone(),
            dir: fs::canonicalize(&outcome.path)
                .unwrap_or_else(|_| outcome.path.clone())
                .display()
                .to_string(),
            config_json: artifacts.config.to_string(),
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            input_bytes: artifacts.input_bytes as i64,
            input_fnv: artifacts.input_fnv,
            ended_cleanly: artifacts.ended_cleanly,
            synthetic_flush: artifacts.synthetic_flush,
            content_div: artifacts.content_div as i64,
            timing_div: artifacts.timing_div as i64,
            duration_ms: artifacts.duration_ms,
            report_json: artifacts.report_json.clone(),
        };
        store.index_run(&row, &segment_rows(&artifacts.segments))?;
    }
    Ok(outcome)
}
