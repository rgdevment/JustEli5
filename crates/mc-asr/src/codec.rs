//! `asr.jsonl` (one recognised phrase per line) and `meta.json` of an ASR run.
//!
//! These are derived data: the corpus (captions, audio) stays the source of
//! truth and any run can be deleted and rebuilt.

use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::transcriber::AsrError;

pub const ASR_SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AsrWord {
    /// Start / end in milliseconds on the same scale as the segment.
    pub s: f64,
    pub e: f64,
    pub t: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AsrSegment {
    pub v: u32,
    /// `mic` or `system`.
    pub ch: String,
    /// Milliseconds on the session clock (sample 0 of the WAV is `mono_ms == 0`).
    pub start_ms: f64,
    pub end_ms: f64,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// Mean log-probability of the tokens, when the engine reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conf: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_speech: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<AsrWord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelMeta {
    pub ch: String,
    pub file: String,
    pub wav_sha256: String,
    pub samplerate: u32,
    pub audio_ms: f64,
    pub utterances: usize,
    pub segments: usize,
    pub speech_ms: f64,
    /// What the hallucination filters threw away, so a silent loss can be seen.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropped: Vec<DroppedSegment>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DroppedSegment {
    /// Where the dropped phrase itself sat, in channel time.
    pub start_ms: f64,
    #[serde(default)]
    pub end_ms: f64,
    pub text: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AsrMeta {
    pub v: u32,
    pub asr_id: String,
    pub engine: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_sha256: Option<String>,
    pub params: Value,
    pub language: Option<String>,
    pub prompt: Option<String>,
    pub segmenter: Value,
    pub channels: Vec<ChannelMeta>,
    pub segments: usize,
    pub created_wall_ms: i64,
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), AsrError> {
    let tmp = path.with_extension("tmp");
    let write = || -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    write().map_err(|e| AsrError::io(path, e))
}

pub fn write_asr_jsonl(path: &Path, segments: &[AsrSegment]) -> Result<(), AsrError> {
    let mut text = String::new();
    for segment in segments {
        text.push_str(&serde_json::to_string(segment).map_err(|e| AsrError::io(path, e))?);
        text.push('\n');
    }
    write_atomic(path, text.as_bytes())
}

pub fn read_asr_jsonl(path: &Path) -> Result<Vec<AsrSegment>, AsrError> {
    let text = std::fs::read_to_string(path).map_err(|e| AsrError::io(path, e))?;
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(n, line)| {
            let segment: AsrSegment = serde_json::from_str(line)
                .map_err(|e| AsrError::io(path, format!("line {}: {e}", n + 1)))?;
            if segment.v != ASR_SCHEMA {
                return Err(AsrError::io(
                    path,
                    format!("line {}: unsupported schema {}", n + 1, segment.v),
                ));
            }
            Ok(segment)
        })
        .collect()
}

pub fn write_meta(path: &Path, meta: &AsrMeta) -> Result<(), AsrError> {
    let text = serde_json::to_string_pretty(meta).map_err(|e| AsrError::io(path, e))? + "\n";
    write_atomic(path, text.as_bytes())
}

pub fn read_meta(path: &Path) -> Result<AsrMeta, AsrError> {
    let text = std::fs::read_to_string(path).map_err(|e| AsrError::io(path, e))?;
    serde_json::from_str(&text).map_err(|e| AsrError::io(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ch: &str, text: &str) -> AsrSegment {
        AsrSegment {
            v: ASR_SCHEMA,
            ch: ch.into(),
            start_ms: 1234.5,
            end_ms: 2000.0,
            text: text.into(),
            lang: Some("es".into()),
            conf: Some(-0.25),
            no_speech: None,
            words: vec![AsrWord {
                s: 1234.5,
                e: 1500.0,
                t: "hola".into(),
                p: Some(0.9),
            }],
        }
    }

    #[test]
    fn jsonl_round_trips_including_unicode_and_optional_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("asr.jsonl");
        let mut bare = sample("system", "¿Qué tal? 😀 \"push\"\nnueva línea");
        bare.lang = None;
        bare.conf = None;
        bare.words.clear();
        let all = vec![sample("mic", "hola"), bare];
        write_asr_jsonl(&path, &all).unwrap();
        assert_eq!(read_asr_jsonl(&path).unwrap(), all);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(!text.lines().nth(1).unwrap().contains("words"));
    }

    #[test]
    fn a_bad_line_names_its_number_and_a_newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("asr.jsonl");
        std::fs::write(
            &path,
            "{\"v\":1,\"ch\":\"mic\",\"start_ms\":0,\"end_ms\":1,\"text\":\"a\"}\nnope\n",
        )
        .unwrap();
        let error = read_asr_jsonl(&path).unwrap_err().to_string();
        assert!(error.contains("line 2"), "{error}");
        std::fs::write(
            &path,
            "{\"v\":9,\"ch\":\"mic\",\"start_ms\":0,\"end_ms\":1,\"text\":\"a\"}\n",
        )
        .unwrap();
        assert!(
            read_asr_jsonl(&path)
                .unwrap_err()
                .to_string()
                .contains("schema")
        );
    }
}
