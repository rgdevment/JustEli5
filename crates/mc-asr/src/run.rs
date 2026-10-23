//! Transcribing the audio of a session folder into `asr/<asr_id>/`.

use std::io::Read;
use std::path::{Path, PathBuf};

use mc_audio::resample::Resampler;
use mc_audio::wav::read_wav_f32;
use mc_core::pytext::py_round3;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::ASR_RATE;
use crate::codec::{
    ASR_SCHEMA, AsrMeta, AsrSegment, ChannelMeta, DroppedSegment, read_asr_jsonl, read_meta,
    write_asr_jsonl, write_meta,
};
use crate::segmenter::{SegmenterConfig, segment_all};
use crate::transcriber::{AsrError, Dropped, RawSegment, TranscribeCtx, Transcriber};
use crate::vad::EnergyVad;

#[derive(Debug, Clone)]
pub struct TranscribeOptions {
    /// Channels to read, in the order they win ties.
    pub channels: Vec<String>,
    pub segmenter: SegmenterConfig,
    /// `None`: detect the language of every utterance.
    pub language: Option<String>,
    /// Glossary terms to favour.
    pub prompt: Option<String>,
    /// Redo a run that already exists.
    pub force: bool,
}

impl Default for TranscribeOptions {
    fn default() -> Self {
        Self {
            channels: vec!["mic".to_string(), "system".to_string()],
            segmenter: SegmenterConfig::default(),
            language: None,
            prompt: None,
            force: false,
        }
    }
}

#[derive(Debug)]
pub struct AsrRun {
    pub asr_id: String,
    pub dir: PathBuf,
    /// The run was already there and was not redone.
    pub reused: bool,
    pub meta: AsrMeta,
    pub segments: Vec<AsrSegment>,
    /// Requested channels that have no audio file.
    pub missing: Vec<String>,
}

/// Keys sorted, no whitespace: the same value always hashes the same.
fn canonical(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", json!(k), canonical(&map[k])))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical).collect();
            format!("[{}]", parts.join(","))
        }
        other => other.to_string(),
    }
}

fn sha256_file(path: &Path) -> Result<String, AsrError> {
    let mut file = std::fs::File::open(path).map_err(|e| AsrError::io(path, e))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buffer).map_err(|e| AsrError::io(path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn run_id(
    transcriber: &dyn Transcriber,
    params: &Value,
    opts: &TranscribeOptions,
    wavs: &[(String, String)],
) -> String {
    let identity = json!({
        "schema": ASR_SCHEMA,
        "engine": transcriber.engine(),
        "model": transcriber.model(),
        "model_sha256": transcriber.model_sha256(),
        "params": params,
        "language": opts.language,
        "prompt": opts.prompt,
        "segmenter": segmenter_json(&opts.segmenter),
        "audio": wavs.iter().map(|(ch, sha)| json!([ch, sha])).collect::<Vec<_>>(),
    });
    let digest = Sha256::digest(canonical(&identity).as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn segmenter_json(cfg: &SegmenterConfig) -> Value {
    json!({
        "min_speech_ms": cfg.min_speech_ms,
        "max_silence_ms": cfg.max_silence_ms,
        "pre_roll_ms": cfg.pre_roll_ms,
        "post_roll_ms": cfg.post_roll_ms,
        "max_utterance_ms": cfg.max_utterance_ms,
        "vad": "energy",
    })
}

/// 16 kHz mono floats of a WAV, plus its original rate. Resampling goes in
/// one-second pieces so only a piece is ever held as `f64`.
fn load_16k(path: &Path) -> Result<(Vec<f32>, u32), AsrError> {
    let (rate, samples) = read_wav_f32(path).map_err(|e| AsrError::io(path, e))?;
    if rate == ASR_RATE {
        return Ok((samples, rate));
    }
    let mut resampler =
        Resampler::new(rate, ASR_RATE).map_err(|e| AsrError::Input(e.to_string()))?;
    let mut out = Vec::with_capacity(samples.len() * ASR_RATE as usize / rate as usize + 64);
    for piece in samples.chunks(rate as usize) {
        let wide: Vec<f64> = piece.iter().map(|v| f64::from(*v)).collect();
        out.extend(resampler.process(&wide).into_iter().map(|v| v as f32));
    }
    out.extend(resampler.finish().into_iter().map(|v| v as f32));
    Ok((out, rate))
}

/// Finite and within `[0, max]`; anything else becomes `fallback`.
fn bounded(value: f64, max: f64, fallback: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, max)
    } else {
        fallback
    }
}

/// An engine's phrase (times relative to the audio it was given) as a
/// session-clock segment: trimmed, bounded to the audio, non-finite numbers
/// removed. `None` for a blank phrase.
pub(crate) fn segment_from_raw(
    ch: &str,
    base_ms: f64,
    length_ms: f64,
    raw: RawSegment,
) -> Option<AsrSegment> {
    let text = raw.text.trim().to_string();
    if text.is_empty() {
        return None;
    }
    let start = bounded(raw.start_ms, length_ms, 0.0);
    let end = bounded(raw.end_ms, length_ms, length_ms).max(start);
    Some(AsrSegment {
        v: ASR_SCHEMA,
        ch: ch.to_string(),
        start_ms: py_round3(base_ms + start),
        end_ms: py_round3(base_ms + end),
        text,
        lang: raw.lang,
        conf: raw
            .avg_logprob
            .filter(|v| v.is_finite())
            .map(|v| (v * 1e4).round() / 1e4),
        no_speech: raw
            .no_speech_prob
            .filter(|v| v.is_finite())
            .map(|v| (v * 1e4).round() / 1e4),
        words: raw
            .words
            .into_iter()
            .filter(|w| w.s.is_finite() && w.e.is_finite())
            .map(|mut w| {
                w.s = py_round3(base_ms + bounded(w.s, length_ms, 0.0));
                w.e = py_round3(base_ms + bounded(w.e, length_ms, length_ms));
                w.p = w.p.filter(|p| p.is_finite());
                w
            })
            .collect(),
    })
}

/// A discarded phrase with its own times on the session clock.
fn dropped_from_raw(base_ms: f64, length_ms: f64, dropped: Dropped) -> DroppedSegment {
    let raw = dropped.segment;
    let start = bounded(raw.start_ms, length_ms, 0.0);
    let end = bounded(raw.end_ms, length_ms, length_ms).max(start);
    DroppedSegment {
        start_ms: py_round3(base_ms + start),
        end_ms: py_round3(base_ms + end),
        text: raw.text,
        reason: dropped.reason.to_string(),
    }
}

/// Transcribes `<dir>/mic.wav` and `<dir>/system.wav` (or the requested
/// channels) into `<dir>/asr/<asr_id>/{asr.jsonl,meta.json}`. The same audio,
/// engine and settings give the same id, so an existing run is reused.
pub fn transcribe_session(
    dir: &Path,
    transcriber: &mut dyn Transcriber,
    opts: &TranscribeOptions,
) -> Result<AsrRun, AsrError> {
    let mut present = Vec::new();
    let mut missing = Vec::new();
    for ch in &opts.channels {
        let path = dir.join(format!("{ch}.wav"));
        if path.is_file() {
            present.push((ch.clone(), path));
        } else {
            missing.push(ch.clone());
        }
    }
    if present.is_empty() {
        return Err(AsrError::NoAudio(format!(
            "{} has no audio for {} (record with `mc capture --audio`)",
            dir.display(),
            opts.channels.join(", ")
        )));
    }
    let mut hashes = Vec::new();
    for (ch, path) in &present {
        hashes.push((ch.clone(), sha256_file(path)?));
    }
    // Read once: an engine may change its description while it works.
    let params = transcriber.params();
    let asr_id = run_id(transcriber, &params, opts, &hashes);
    let out_dir = dir.join("asr").join(&asr_id);
    let (jsonl, meta_path) = (out_dir.join("asr.jsonl"), out_dir.join("meta.json"));
    if !opts.force
        && meta_path.is_file()
        && let (Ok(meta), Ok(segments)) = (read_meta(&meta_path), read_asr_jsonl(&jsonl))
        && meta.asr_id == asr_id
        && meta.segments == segments.len()
    {
        return Ok(AsrRun {
            asr_id,
            dir: out_dir,
            reused: true,
            meta,
            segments,
            missing,
        });
    }

    let mut segments = Vec::new();
    let mut channels = Vec::new();
    for ((ch, path), (_, sha)) in present.iter().zip(&hashes) {
        let (audio, original_rate) = load_16k(path)?;
        let utterances = segment_all(opts.segmenter.clone(), EnergyVad::default(), &audio);
        let ctx = TranscribeCtx {
            channel: ch,
            prompt: opts.prompt.as_deref(),
            language: opts.language.as_deref(),
        };
        let (mut found, mut speech_ms) = (0usize, 0.0f64);
        let mut dropped = Vec::new();
        for utterance in &utterances {
            let slice = &audio[utterance.start_sample as usize..utterance.end_sample as usize];
            let base = utterance.start_ms();
            let length_ms = slice.len() as f64 * 1000.0 / f64::from(ASR_RATE);
            speech_ms += length_ms;
            let recognized = transcriber.transcribe(slice, &ctx)?;
            for raw in recognized.segments {
                if let Some(segment) = segment_from_raw(ch, base, length_ms, raw) {
                    found += 1;
                    segments.push(segment);
                }
            }
            dropped.extend(
                recognized
                    .dropped
                    .into_iter()
                    .map(|d| dropped_from_raw(base, length_ms, d)),
            );
        }
        channels.push(ChannelMeta {
            ch: ch.clone(),
            file: format!("{ch}.wav"),
            wav_sha256: sha.clone(),
            samplerate: original_rate,
            audio_ms: py_round3(audio.len() as f64 * 1000.0 / f64::from(ASR_RATE)),
            utterances: utterances.len(),
            segments: found,
            speech_ms: py_round3(speech_ms),
            dropped,
        });
    }
    let order = |ch: &str| {
        opts.channels
            .iter()
            .position(|c| c == ch)
            .unwrap_or(usize::MAX)
    };
    segments.sort_by(|a, b| {
        a.start_ms
            .total_cmp(&b.start_ms)
            .then(order(&a.ch).cmp(&order(&b.ch)))
    });

    let meta = AsrMeta {
        v: ASR_SCHEMA,
        asr_id: asr_id.clone(),
        engine: transcriber.engine().to_string(),
        model: transcriber.model().to_string(),
        model_sha256: transcriber.model_sha256(),
        params,
        language: opts.language.clone(),
        prompt: opts.prompt.clone(),
        segmenter: segmenter_json(&opts.segmenter),
        channels,
        segments: segments.len(),
        created_wall_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX)),
    };
    if out_dir.exists() {
        std::fs::remove_dir_all(&out_dir).map_err(|e| AsrError::io(&out_dir, e))?;
    }
    std::fs::create_dir_all(&out_dir).map_err(|e| AsrError::io(&out_dir, e))?;
    write_asr_jsonl(&jsonl, &segments)?;
    // The meta goes last: its presence says the run is complete.
    write_meta(&meta_path, &meta)?;
    Ok(AsrRun {
        asr_id,
        dir: out_dir,
        reused: false,
        meta,
        segments,
        missing,
    })
}
