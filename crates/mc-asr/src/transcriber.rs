use serde_json::Value;

use crate::codec::AsrWord;

#[derive(Debug, thiserror::Error)]
pub enum AsrError {
    #[error("{0}")]
    Engine(String),
    #[error("{0}")]
    Input(String),
    /// The session has none of the audio asked for.
    #[error("{0}")]
    NoAudio(String),
    #[error("{path}: {message}")]
    Io { path: String, message: String },
}

impl AsrError {
    pub fn io(path: impl AsRef<std::path::Path>, error: impl std::fmt::Display) -> Self {
        Self::Io {
            path: path.as_ref().display().to_string(),
            message: error.to_string(),
        }
    }
}

/// One recognised phrase with times relative to the start of the audio given.
#[derive(Debug, Clone, PartialEq)]
pub struct RawSegment {
    pub start_ms: f64,
    pub end_ms: f64,
    pub text: String,
    pub lang: Option<String>,
    pub avg_logprob: Option<f64>,
    pub no_speech_prob: Option<f64>,
    pub words: Vec<AsrWord>,
}

impl RawSegment {
    pub fn text(start_ms: f64, end_ms: f64, text: impl Into<String>) -> Self {
        Self {
            start_ms,
            end_ms,
            text: text.into(),
            lang: None,
            avg_logprob: None,
            no_speech_prob: None,
            words: Vec::new(),
        }
    }
}

/// A phrase the filters discarded as noise, with its own times and the reason.
#[derive(Debug, Clone, PartialEq)]
pub struct Dropped {
    pub segment: RawSegment,
    pub reason: &'static str,
}

/// What one `transcribe` call found: the phrases to keep and the ones dropped.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Recognized {
    pub segments: Vec<RawSegment>,
    pub dropped: Vec<Dropped>,
}

impl From<Vec<RawSegment>> for Recognized {
    fn from(segments: Vec<RawSegment>) -> Self {
        Self {
            segments,
            dropped: Vec::new(),
        }
    }
}

pub struct TranscribeCtx<'a> {
    pub channel: &'a str,
    /// Terms the engine should favour (the meeting glossary).
    pub prompt: Option<&'a str>,
    /// `None`: detect the language of every utterance (Spanish / English mixes).
    pub language: Option<&'a str>,
}

pub trait Transcriber {
    fn engine(&self) -> &str;
    fn model(&self) -> &str;
    /// Settings that change the output; part of the run's identity.
    fn params(&self) -> Value {
        Value::Null
    }
    /// Checksum of the model file, when there is one.
    fn model_sha256(&self) -> Option<String> {
        None
    }
    /// Recognise 16 kHz mono floats in [-1, 1].
    fn transcribe(
        &mut self,
        audio: &[f32],
        ctx: &TranscribeCtx<'_>,
    ) -> Result<Recognized, AsrError>;
}
