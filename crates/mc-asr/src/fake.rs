//! A scripted engine for tests and demonstrations.

use std::collections::VecDeque;

use serde_json::{Value, json};

use crate::transcriber::{AsrError, RawSegment, Recognized, TranscribeCtx, Transcriber};

/// Every `transcribe` call returns the next scripted text as one segment that
/// spans the whole audio; an exhausted script returns nothing (silence).
pub struct FakeAsr {
    texts: VecDeque<String>,
    numbered: bool,
    calls: usize,
    seen_prompts: Vec<Option<String>>,
}

impl FakeAsr {
    pub fn new(texts: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            texts: texts.into_iter().map(Into::into).collect(),
            numbered: false,
            calls: 0,
            seen_prompts: Vec::new(),
        }
    }

    /// Answers "utterance 1", "utterance 2", ... for as long as it is asked.
    pub fn numbered() -> Self {
        Self {
            numbered: true,
            ..Self::new(Vec::<String>::new())
        }
    }

    /// `["text", ...]` or `{"texts": ["text", ...]}`.
    pub fn from_json(text: &str) -> Result<Self, AsrError> {
        let value: Value =
            serde_json::from_str(text).map_err(|e| AsrError::Input(format!("fake script: {e}")))?;
        let list = value.get("texts").unwrap_or(&value);
        let texts = list
            .as_array()
            .ok_or_else(|| AsrError::Input("fake script: expected a list of texts".into()))?
            .iter()
            .map(|t| {
                t.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| AsrError::Input("fake script: texts must be strings".into()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::new(texts))
    }

    pub fn calls(&self) -> usize {
        self.calls
    }

    /// The prompt each call received.
    pub fn prompts(&self) -> &[Option<String>] {
        &self.seen_prompts
    }
}

impl Transcriber for FakeAsr {
    fn engine(&self) -> &str {
        "fake"
    }

    fn model(&self) -> &str {
        "fake"
    }

    fn params(&self) -> Value {
        json!({"numbered": self.numbered, "script": self.texts.iter().collect::<Vec<_>>()})
    }

    fn transcribe(
        &mut self,
        audio: &[f32],
        ctx: &TranscribeCtx<'_>,
    ) -> Result<Recognized, AsrError> {
        self.calls += 1;
        self.seen_prompts.push(ctx.prompt.map(str::to_string));
        let text = if self.numbered {
            Some(format!("utterance {}", self.calls))
        } else {
            self.texts.pop_front()
        };
        let ms = audio.len() as f64 * 1000.0 / f64::from(crate::ASR_RATE);
        Ok(text
            .filter(|t| !t.is_empty())
            .map(|t| vec![RawSegment::text(0.0, ms, t)])
            .unwrap_or_default()
            .into())
    }
}
