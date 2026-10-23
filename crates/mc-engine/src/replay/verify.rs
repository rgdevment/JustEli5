//! Runs a reconstructed corpus through the reconciler and diffs the outcome.

use std::path::Path;

use mc_core::Stamp;
use mc_reconcile::{Reconciler, ReconcilerConfig, StepCtx};
use mc_store::StoreError;
use mc_store::jsonl::{CaptionLine, ParsedLine, ReadReport, read_file};
use mc_store::manifest::Manifest;

use crate::policy::recorded_policy;

use super::ReplayError;
use super::frames::{ReplayOp, reconstruct};

pub struct ReplayRun {
    pub lines: Vec<CaptionLine>,
    pub segments: Vec<mc_core::Segment>,
    pub transcript: String,
    /// Lines still open when the session ended without a flush.
    pub open_lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Divergence {
    /// Position in its stream (observation records or committed records).
    pub index: usize,
    pub field: String,
    pub expected: String,
    pub actual: String,
}

#[derive(Debug, Default)]
pub struct VerifyReport {
    pub frames: usize,
    pub records: usize,
    /// A commit moved (frame, stamps, or a `new_open`/`newline` flip).
    pub timing: Vec<Divergence>,
    /// Anything that changes what was said: transcript, observation records,
    /// committed (speaker, text) order, record counts.
    pub content: Vec<Divergence>,
    pub transcript_ok: bool,
    /// `expected_transcript.txt` was missing: the committed records stood in.
    pub transcript_from_records: bool,
    pub truncated_tail: bool,
    pub open_lines: Vec<String>,
    pub passed: bool,
}

/// How the corpus clocked its debounce: `false` = wall, `true` = monotonic.
pub(crate) fn debounce_is_mono(manifest: &Manifest) -> Result<bool, ReplayError> {
    match manifest
        .captions
        .extra
        .get("debounce_clock")
        .map(|v| v.as_str())
    {
        None => Ok(false),
        Some(Some("wall")) => Ok(false),
        Some(Some("mono")) => Ok(true),
        Some(other) => Err(ReplayError::Invalid(format!(
            "unknown debounce_clock {other:?}"
        ))),
    }
}

/// `now` as Python's manager saw it: the wall stamp, or the monotonic one when
/// the corpus was recorded with a monotonic debounce clock.
fn now_s(stamp: Stamp, mono: bool) -> f64 {
    if mono {
        stamp.mono_ms / 1000.0
    } else {
        stamp.wall_ms as f64 / 1000.0
    }
}

pub fn run(
    ops: &[ReplayOp],
    manifest: &Manifest,
    flush_at: Option<Stamp>,
) -> Result<ReplayRun, ReplayError> {
    let mono = debounce_is_mono(manifest)?;
    run_with_now(ops, manifest, flush_at, &|_, stamp| now_s(stamp, mono))
}

/// `run` with the manager's `now` supplied per frame (tests perturb it to prove
/// the comparison notices a debounce error).
pub fn run_with_now(
    ops: &[ReplayOp],
    manifest: &Manifest,
    flush_at: Option<Stamp>,
    now: &dyn Fn(u64, Stamp) -> f64,
) -> Result<ReplayRun, ReplayError> {
    let cfg = ReconcilerConfig {
        policy: recorded_policy(manifest),
        ..ReconcilerConfig::default()
    };
    run_cfg(ops, manifest, flush_at, cfg, now)
}

/// `run` with a chosen reconciler configuration and the corpus' own debounce clock.
pub fn run_with(
    ops: &[ReplayOp],
    manifest: &Manifest,
    flush_at: Option<Stamp>,
    cfg: ReconcilerConfig,
) -> Result<ReplayRun, ReplayError> {
    let mono = debounce_is_mono(manifest)?;
    run_cfg(ops, manifest, flush_at, cfg, &|_, stamp| now_s(stamp, mono))
}

/// The general form: any reconciler configuration, any `now`.
pub fn run_cfg(
    ops: &[ReplayOp],
    manifest: &Manifest,
    flush_at: Option<Stamp>,
    cfg: ReconcilerConfig,
    now: &dyn Fn(u64, Stamp) -> f64,
) -> Result<ReplayRun, ReplayError> {
    let mono = debounce_is_mono(manifest)?;
    // A monotonic session clock starts at zero; wall is seconds since the epoch.
    let init = if mono {
        0.0
    } else {
        manifest.started_wall_ms as f64 / 1000.0
    };
    let mut reconciler = Reconciler::for_replay(cfg, init);
    let mut lines = Vec::new();
    let mut segments = Vec::new();
    let mut seq = 1u64;
    let mut push = |outcome: mc_reconcile::StepOutcome,
                    lines: &mut Vec<CaptionLine>,
                    segments: &mut Vec<mc_core::Segment>| {
        for record in &outcome.records {
            lines.push(CaptionLine::from_record(seq, record, || Stamp {
                wall_ms: 0,
                mono_ms: 0.0,
            }));
            seq += 1;
        }
        segments.extend(outcome.committed);
    };

    for op in ops {
        match op {
            ReplayOp::Frame {
                frame,
                stamp,
                read_ms,
                nodes,
            } => {
                let ctx = StepCtx {
                    now_s: now(*frame, *stamp),
                    read_ms: *read_ms,
                };
                push(
                    reconciler.step_with(nodes, &ctx, *frame, *stamp),
                    &mut lines,
                    &mut segments,
                );
            }
            ReplayOp::Flush { stamp } => {
                push(reconciler.flush_with(*stamp), &mut lines, &mut segments);
            }
        }
    }
    if let Some(stamp) = flush_at
        && !ops.iter().any(|op| matches!(op, ReplayOp::Flush { .. }))
    {
        push(reconciler.flush_with(stamp), &mut lines, &mut segments);
    }

    let transcript = segments.iter().map(|s| s.line() + "\n").collect::<String>();
    Ok(ReplayRun {
        lines,
        segments,
        transcript,
        open_lines: reconciler.open_lines(),
    })
}

pub(crate) fn transcript_of(lines: &[CaptionLine]) -> String {
    lines
        .iter()
        .filter(|l| l.decision == "committed")
        .map(|l| format!("[{}]: {}\n", l.speaker, l.text))
        .collect()
}

fn is_commit(line: &CaptionLine) -> bool {
    line.decision == "committed" || line.decision == "dup_committed_skip"
}

/// `new_open` vs `newline` differ only by whether the previous line of that
/// speaker was already committed, i.e. by when the commit happened.
fn is_commit_flip(a: &str, b: &str) -> bool {
    matches!((a, b), ("new_open", "newline") | ("newline", "new_open"))
}

struct Diff<'a> {
    timing: &'a mut Vec<Divergence>,
    content: &'a mut Vec<Divergence>,
}

impl Diff<'_> {
    fn add(&mut self, content: bool, index: usize, field: &str, expected: String, actual: String) {
        let divergence = Divergence {
            index,
            field: field.to_string(),
            expected,
            actual,
        };
        if content {
            self.content.push(divergence);
        } else {
            self.timing.push(divergence);
        }
    }

    /// Compares every field of two records. `moves` says whether a mismatch
    /// in stamps, frame or decision may just be a commit that moved in time.
    fn line(&mut self, index: usize, file: &CaptionLine, replay: &CaptionLine, moves: bool) {
        let mut field = |name: &str, same: bool, e: String, a: String, timing_ok: bool| {
            if !same {
                // A `new_open`/`newline` flip on an observation is the visible
                // side of a commit that moved, so it is timing even there.
                let is_timing = timing_ok && (moves || name == "decision");
                self.add(!is_timing, index, name, e, a);
            }
        };
        field(
            "frame",
            file.frame == replay.frame,
            format!("{:?}", file.frame),
            format!("{:?}", replay.frame),
            true,
        );
        field(
            "wall_ms",
            file.wall_ms == replay.wall_ms,
            file.wall_ms.to_string(),
            replay.wall_ms.to_string(),
            true,
        );
        field(
            "mono_ms",
            file.mono_ms == replay.mono_ms,
            file.mono_ms.to_string(),
            replay.mono_ms.to_string(),
            true,
        );
        field(
            "t",
            file.t == replay.t,
            file.t.to_string(),
            replay.t.to_string(),
            true,
        );
        field(
            "read_ms",
            file.read_ms == replay.read_ms,
            format!("{:?}", file.read_ms),
            format!("{:?}", replay.read_ms),
            true,
        );
        field(
            "speaker",
            file.speaker == replay.speaker,
            file.speaker.clone(),
            replay.speaker.clone(),
            false,
        );
        field(
            "speaker_raw",
            file.speaker_raw == replay.speaker_raw,
            format!("{:?}", file.speaker_raw),
            format!("{:?}", replay.speaker_raw),
            false,
        );
        field(
            "text",
            file.text == replay.text,
            file.text.clone(),
            replay.text.clone(),
            false,
        );
        field(
            "decision",
            file.decision == replay.decision,
            file.decision.clone(),
            replay.decision.clone(),
            is_commit_flip(&file.decision, &replay.decision),
        );
        let keys_same = file.extra.keys().eq(replay.extra.keys());
        field(
            "extra_keys",
            keys_same,
            format!("{:?}", file.extra.keys().collect::<Vec<_>>()),
            format!("{:?}", replay.extra.keys().collect::<Vec<_>>()),
            false,
        );
        if keys_same {
            for (key, value) in &file.extra {
                let other = &replay.extra[key];
                let same = match (value.as_f64(), other.as_f64()) {
                    (Some(x), Some(y)) => (x - y).abs() < 1e-9,
                    _ => value == other,
                };
                field(key, same, value.to_string(), other.to_string(), true);
            }
        }
    }
}

/// Splits a record stream into observation records and commit records. Only
/// the commits can move when a debounce decision changes; the observations
/// are the input nodes one to one.
fn split(lines: &[CaptionLine]) -> (Vec<&CaptionLine>, Vec<&CaptionLine>) {
    lines.iter().partition(|l| !is_commit(l))
}

#[allow(clippy::too_many_arguments)]
fn compare_streams(
    file: &[CaptionLine],
    replay: &[CaptionLine],
    truncated_tail: bool,
    expected_transcript: &str,
    replay_transcript: &str,
    strict: bool,
) -> VerifyReport {
    let mut report = VerifyReport {
        records: file.len(),
        truncated_tail,
        transcript_ok: expected_transcript == replay_transcript,
        ..VerifyReport::default()
    };

    // A cut tail loses records from the end of the file, never from the replay;
    // the only replay records allowed to exceed the file belong to the last
    // surviving frame.
    let mut replay = replay;
    if truncated_tail && let Some(last) = file.last() {
        let mut end = replay.len();
        while end > file.len() && replay[end - 1].frame == last.frame {
            end -= 1;
        }
        replay = &replay[..end];
    }

    let (file_obs, file_commits) = split(file);
    let (replay_obs, replay_commits) = split(replay);
    let mut diff = Diff {
        timing: &mut report.timing,
        content: &mut report.content,
    };

    for (name, a, b) in [
        ("observation_count", file_obs.len(), replay_obs.len()),
        ("commit_count", file_commits.len(), replay_commits.len()),
    ] {
        if a != b {
            diff.add(true, a.min(b), name, a.to_string(), b.to_string());
        }
    }
    for (index, (f, r)) in file_obs.iter().zip(&replay_obs).enumerate() {
        diff.line(index, f, r, false);
    }
    for (index, (f, r)) in file_commits.iter().zip(&replay_commits).enumerate() {
        // Which text was committed, and in which order, is content; when it
        // happened is timing.
        if f.decision == r.decision && f.speaker == r.speaker && f.text == r.text {
            diff.line(index, f, r, true);
        } else {
            diff.add(
                true,
                index,
                "commit",
                format!("{} [{}]: {}", f.decision, f.speaker, f.text),
                format!("{} [{}]: {}", r.decision, r.speaker, r.text),
            );
        }
    }
    // The observation stream is a decision-for-decision replay, so a moved
    // commit (timing) must not have changed which observations flipped.
    // seq is only comparable when nothing moved.
    if diff.timing.is_empty() && diff.content.is_empty() {
        for (index, (f, r)) in file.iter().zip(replay).enumerate() {
            if f.seq != r.seq {
                diff.add(true, index, "seq", f.seq.to_string(), r.seq.to_string());
            }
        }
    }
    if !report.transcript_ok {
        report.content.push(Divergence {
            index: 0,
            field: "transcript".into(),
            expected: expected_transcript.to_string(),
            actual: replay_transcript.to_string(),
        });
    }
    report.passed = report.content.is_empty() && (!strict || report.timing.is_empty());
    report
}

/// Compares a recording with its replay (see [`verify`] for the corpus form).
pub fn compare(
    file: &[CaptionLine],
    replay: &ReplayRun,
    truncated_tail: bool,
    expected_transcript: &str,
    strict: bool,
) -> VerifyReport {
    let mut report = compare_streams(
        file,
        &replay.lines,
        truncated_tail,
        expected_transcript,
        &replay.transcript,
        strict,
    );
    report.open_lines = replay.open_lines.clone();
    report
}

fn v2_lines(report: &ReadReport) -> Vec<CaptionLine> {
    report
        .lines
        .iter()
        .filter_map(|(_, l)| match l {
            ParsedLine::V2(line) => Some(line.clone()),
            ParsedLine::V1(_) => None,
        })
        .collect()
}

/// Replays `dir` (a corpus directory) and compares it with the recording.
/// `strict` additionally fails on timing divergences.
pub fn verify(dir: impl AsRef<Path>, strict: bool) -> Result<VerifyReport, ReplayError> {
    let dir = dir.as_ref();
    let manifest = Manifest::load(dir.join("manifest.json"))?;
    let captions_file = if manifest.captions.file.is_empty() {
        "captions.jsonl"
    } else {
        manifest.captions.file.as_str()
    };
    let report = read_file(dir.join(captions_file))?;
    if !report.bad.is_empty() {
        return Err(ReplayError::Invalid(format!(
            "{} unreadable line(s), first at line {}: {}",
            report.bad.len(),
            report.bad[0].line_no,
            report.bad[0].reason
        )));
    }
    let ops = reconstruct(&report)?;
    let flush_at = manifest.ended_wall_ms.map(|wall_ms| Stamp {
        wall_ms,
        mono_ms: manifest.ended_mono_ms.unwrap_or(0.0),
    });
    let replay = run(&ops, &manifest, flush_at)?;
    let file = v2_lines(&report);

    let (expected_transcript, from_records) =
        match std::fs::read_to_string(dir.join("expected_transcript.txt")) {
            Ok(text) => (text, false),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => (transcript_of(&file), true),
            Err(err) => {
                return Err(StoreError::io(dir.join("expected_transcript.txt"), err).into());
            }
        };

    let mut result = compare(
        &file,
        &replay,
        report.truncated_tail,
        &expected_transcript,
        strict,
    );
    result.frames = ops
        .iter()
        .filter(|op| matches!(op, ReplayOp::Frame { .. }))
        .count();
    result.transcript_from_records = from_records;
    Ok(result)
}
