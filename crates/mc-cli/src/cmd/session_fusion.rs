//! What `mc fuse` and `mc eval` share: the captions of a session as the V2
//! reconciler reads them, one of its ASR runs, and the person/vocabulary
//! settings.

use std::path::{Path, PathBuf};

use mc_asr::AsrSegment;
use mc_asr::codec::{AsrMeta, read_asr_jsonl, read_meta};
use mc_core::Segment;
use mc_engine::runs::build;
use mc_fusion::render::{render_jsonl, render_md, render_txt};
use mc_fusion::{FusionConfig, FusionResult, Source, fuse};
use mc_reconcile::Policy;
use mc_store::manifest::Manifest;
use mc_store::{AsrRunRow, FNV_OFFSET, FusedRow, FusionRow, fnv_extend};
use serde_json::{Value, json};

use crate::cli::{CliError, Ctx, open_store};

use super::vocabulary::{memory_aliases, merged_glossary};

pub struct Inputs {
    /// The V2 captions run these segments come from.
    pub captions_run_id: String,
    pub captions: Vec<Segment>,
    pub asr: Vec<AsrSegment>,
    pub asr_id: String,
    pub asr_dir: PathBuf,
    pub meta: AsrMeta,
}

fn asr_runs(dir: &Path) -> Vec<(AsrMeta, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir.join("asr")) else {
        return Vec::new();
    };
    let mut runs: Vec<(AsrMeta, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            read_meta(&path.join("meta.json")).ok().map(|m| (m, path))
        })
        .collect();
    runs.sort_by(|a, b| {
        a.0.created_wall_ms
            .cmp(&b.0.created_wall_ms)
            .then(a.0.asr_id.cmp(&b.0.asr_id))
    });
    runs
}

/// `selector`: an asr id or a unique prefix of one. None: the newest run.
fn pick_run(dir: &Path, selector: Option<&str>) -> Result<(AsrMeta, PathBuf), CliError> {
    let mut runs = asr_runs(dir);
    if runs.is_empty() {
        return Err(CliError::not_found(
            format!("{}: no ASR run", dir.display()),
            "run `mc transcribe <dir>` first",
        ));
    }
    match selector {
        None => Ok(runs.pop().expect("not empty")),
        Some(id) => {
            let mut hits: Vec<_> = runs
                .into_iter()
                .filter(|(m, _)| m.asr_id.starts_with(id))
                .collect();
            match hits.len() {
                0 => Err(CliError::not_found(
                    format!("no ASR run starts with {id:?}"),
                    "see the folders under <dir>/asr",
                )),
                1 => Ok(hits.remove(0)),
                _ => Err(CliError::usage(format!(
                    "{id:?} matches more than one ASR run"
                ))),
            }
        }
    }
}

pub fn load(ctx: &Ctx<'_>, dir: &str) -> Result<Inputs, CliError> {
    let path = Path::new(dir);
    if !path.join("manifest.json").is_file() {
        return Err(CliError::not_found(
            format!("{dir}: no manifest.json"),
            "pass a session directory",
        ));
    }
    let (meta, asr_dir) = pick_run(path, ctx.args.value("asr"))?;
    let asr = read_asr_jsonl(&asr_dir.join("asr.jsonl"))
        .map_err(|e| CliError::runtime("io", e.to_string()))?;
    // Fusion works on V2 segments: V1 splits a phrase every time two speakers alternate.
    let mut run = ctx.cfg.run_config();
    run.reconciler.policy = Policy::V2;
    run.policy_by_platform = false;
    let artifacts = build(path, &run)?;
    Ok(Inputs {
        captions_run_id: artifacts.run_id,
        captions: artifacts.segments,
        asr,
        asr_id: meta.asr_id.clone(),
        asr_dir,
        meta,
    })
}

pub fn glossary_terms(ctx: &Ctx<'_>) -> Result<Vec<String>, CliError> {
    Ok(merged_glossary(ctx)?.terms())
}

pub fn fusion_config(ctx: &Ctx<'_>) -> Result<FusionConfig, CliError> {
    Ok(FusionConfig {
        me: ctx
            .args
            .value("me")
            .map(str::to_string)
            .or_else(|| ctx.cfg.fusion.me.clone()),
        glossary: glossary_terms(ctx)?,
        aliases: memory_aliases(&ctx.cfg)?,
        ..FusionConfig::default()
    })
}

/// Part of every fusion id: change it whenever the fusion algorithm changes, so
/// old results are not mistaken for new ones.
const FUSION_VERSION: &str = "mc-fusion/4";

static TMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Unique per process and call, so concurrent fuses never share a scratch name.
fn tmp_tag() -> String {
    let n = TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}.{n}", std::process::id())
}

pub struct Fused {
    pub fuse_id: String,
    pub dir: PathBuf,
    /// The files were already there (same captions, ASR and settings).
    pub reused: bool,
    pub asr_id: String,
    pub result: FusionResult,
    pub report: Value,
}

fn source_name(source: Source) -> &'static str {
    match source {
        Source::Merged => "merged",
        Source::Captions => "captions",
        Source::Asr => "asr",
    }
}

/// Fuses a session's captions with one of its ASR runs, writes
/// `<dir>/fused/<fuse_id>/{fused.jsonl,fused.txt,fused.md,report.json}` and
/// indexes it. The id follows the inputs, so doing it again changes nothing.
pub fn fuse_session(ctx: &Ctx<'_>, dir: &str, force: bool) -> Result<Fused, CliError> {
    let inputs = load(ctx, dir)?;
    let config = fusion_config(ctx)?;
    let result = fuse(&inputs.captions, &inputs.asr, &config);

    // The ASR text itself is part of the identity: `--force` on `mc transcribe`
    // keeps the id (same audio and settings) but may change the words.
    let asr_bytes = std::fs::read(inputs.asr_dir.join("asr.jsonl"))
        .map_err(|e| CliError::runtime("io", e.to_string()))?;
    let asr_fnv = fnv_extend(FNV_OFFSET, &asr_bytes);
    let identity = format!(
        "{FUSION_VERSION}|{}|{}|{asr_fnv:016x}|{config:?}",
        inputs.captions_run_id, inputs.asr_id
    );
    let fuse_id = format!("f{:016x}", fnv_extend(FNV_OFFSET, identity.as_bytes()));
    let out = Path::new(dir).join("fused").join(&fuse_id);
    let manifest = Manifest::load(Path::new(dir).join("manifest.json"))?;
    let report = json!({
        "fuse_id": fuse_id,
        "version": FUSION_VERSION,
        "run_id": inputs.captions_run_id,
        "asr_id": inputs.asr_id,
        "me": config.me,
        "glossary": config.glossary,
        "lag": result.report.lag,
        "speaker_channels": result.report.speaker_channels,
        "me_found": result.report.me,
        "me_source": result.report.me_source,
        "counts": result.report.counts,
        "segments": result.segments.len(),
    });
    let mut reused = !force && out.join("report.json").is_file();
    if !reused {
        // A folder of our own, so two fuses of the same id never share one.
        let tmp = out.with_file_name(format!(".{fuse_id}.{}.tmp", tmp_tag()));
        let mut write = || -> Result<(), CliError> {
            let io = |path: &Path, e: std::io::Error| {
                CliError::runtime("io", format!("{}: {e}", path.display()))
            };
            if let Some(parent) = tmp.parent() {
                std::fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
            }
            std::fs::create_dir(&tmp).map_err(|e| io(&tmp, e))?;
            let title = manifest
                .meeting_name
                .clone()
                .unwrap_or_else(|| manifest.session_id.clone());
            let files = [
                ("fused.jsonl", render_jsonl(&result.segments)),
                ("fused.txt", render_txt(&result.segments)),
                ("fused.md", render_md(&result.segments, &title)),
                (
                    "report.json",
                    serde_json::to_string_pretty(&report).unwrap_or_default() + "\n",
                ),
            ];
            for (name, content) in files {
                std::fs::write(tmp.join(name), content).map_err(|e| io(&tmp.join(name), e))?;
            }
            // Someone else may have finished the same fusion while we wrote.
            if !force && out.join("report.json").is_file() {
                reused = true;
                return Ok(());
            }
            let mut last = None;
            for attempt in 0..8 {
                // Set the old result aside by rename: deleting it in place could empty a
                // folder another writer just put there.
                let aside = out.with_file_name(format!(".{fuse_id}.{}.{attempt}.old", tmp_tag()));
                let displaced = std::fs::rename(&out, &aside).is_ok();
                let placed = std::fs::rename(&tmp, &out);
                if displaced {
                    let _ = std::fs::remove_dir_all(&aside);
                }
                match placed {
                    Ok(()) => return Ok(()),
                    // Lost a race to an identical result: theirs stands.
                    Err(_) if out.join("report.json").is_file() => {
                        reused = true;
                        return Ok(());
                    }
                    Err(e) => last = Some(e),
                }
                std::thread::sleep(std::time::Duration::from_millis(20 << attempt.min(4)));
            }
            Err(io(&out, last.expect("at least one attempt")))
        };
        let outcome = write();
        let _ = std::fs::remove_dir_all(&tmp);
        outcome?;
    }
    let out = crate::cli::canonical(out);
    let asr_dir = crate::cli::canonical(inputs.asr_dir.clone());

    // The index follows the files.
    let mut store = open_store(&ctx.cfg)?;
    store.import_session(dir)?;
    store.index_asr_run(&AsrRunRow {
        asr_id: inputs.asr_id.clone(),
        session_id: manifest.session_id.clone(),
        dir: asr_dir.display().to_string(),
        engine: inputs.meta.engine.clone(),
        model: inputs.meta.model.clone(),
        segments: inputs.asr.len() as i64,
        meta_json: serde_json::to_string(&inputs.meta).unwrap_or_default(),
    })?;
    let counts = &result.report.counts;
    let rows: Vec<FusedRow> = result
        .segments
        .iter()
        .map(|s| FusedRow {
            start_ms: s.start_ms,
            end_ms: s.end_ms,
            speaker: s.speaker.clone(),
            speaker_inferred: s.speaker_inferred,
            ch: s.ch.clone(),
            source: source_name(s.source).to_string(),
            text: s.text.clone(),
            caption_text: s.caption_text.clone(),
            asr_text: s.asr_text.clone(),
        })
        .collect();
    store.index_fusion(
        &FusionRow {
            fuse_id: fuse_id.clone(),
            session_id: manifest.session_id,
            run_id: inputs.captions_run_id,
            asr_id: inputs.asr_id.clone(),
            dir: out.display().to_string(),
            config_json: serde_json::to_string(
                &json!({"me": config.me, "glossary": config.glossary}),
            )
            .unwrap_or_default(),
            merged: counts.merged as i64,
            caption_only: counts.caption_only as i64,
            asr_only: counts.asr_only as i64,
            edits_applied: counts.edits_applied as i64,
            lag_median_ms: result.report.lag.median_ms,
            report_json: report.to_string(),
        },
        &rows,
    )?;
    Ok(Fused {
        fuse_id,
        dir: out,
        reused,
        asr_id: inputs.asr_id,
        result,
        report,
    })
}

/// The lines `mc fuse` and `mc run --fuse` print.
pub fn describe(fused: &Fused) -> String {
    let c = &fused.result.report.counts;
    let lag = fused
        .result
        .report
        .lag
        .median_ms
        .map_or("unknown".to_string(), |v| format!("{v:.0} ms"));
    format!(
        "{}  {}  {}\n  {} segments: {} merged with the ASR, {} captions only, {} ASR only; {} term fixes; caption lag {lag}\n  dropped: {} junk, {} leaked, {} duplicate, {} unsure\n",
        fused.fuse_id,
        if fused.reused { "unchanged" } else { "created" },
        fused.dir.display(),
        fused.result.segments.len(),
        c.merged,
        c.caption_only,
        c.asr_only,
        c.edits_applied,
        c.dropped_junk,
        c.dropped_leak,
        c.dropped_duplicate,
        c.dropped_low_confidence,
    )
}
