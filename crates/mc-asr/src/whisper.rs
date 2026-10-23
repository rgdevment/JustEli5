//! Whisper through `whisper.cpp` (`whisper-rs`), everything on this machine.
//!
//! Behind the `whisper` cargo feature because it compiles whisper.cpp (cmake
//! and a C++ compiler). The pieces that decide what to keep (`prompt`,
//! `language`, `filters`) are plain Rust and tested without a model; this file
//! is the thin part that needs one, and has a smoke test gated on
//! `MC_TEST_MODEL`.

use std::path::Path;

use serde_json::{Value, json};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
    get_lang_str,
};

use crate::filters::{FilterConfig, apply};
use crate::language::{Decision, LanguagePolicy};
use crate::transcriber::{AsrError, RawSegment, Recognized, TranscribeCtx, Transcriber};

/// Shorter input is padded with silence (whisper.cpp rejects under 100 ms and
/// does better with a little context).
const MIN_SAMPLES: usize = 16_800;

#[derive(Debug, Clone)]
pub struct WhisperOptions {
    pub threads: usize,
    /// 1 is greedy decoding (live); more is beam search (final pass).
    pub beam_size: usize,
    pub languages: LanguagePolicy,
    pub filter: FilterConfig,
}

impl Default for WhisperOptions {
    fn default() -> Self {
        let cores = std::thread::available_parallelism().map_or(4, usize::from);
        Self {
            threads: (cores / 2).clamp(1, 8),
            beam_size: 1,
            languages: LanguagePolicy::default(),
            filter: FilterConfig::default(),
        }
    }
}

pub struct WhisperAsr {
    /// Reused for every utterance: `full` resets its results and `no_context`
    /// keeps utterances apart, so only the buffers are kept.
    state: WhisperState,
    name: String,
    sha256: Option<String>,
    opts: WhisperOptions,
}

fn engine_error(what: &str, error: impl std::fmt::Display) -> AsrError {
    AsrError::Engine(format!("whisper: {what}: {error}"))
}

impl WhisperAsr {
    pub fn load(
        model_path: &Path,
        name: &str,
        sha256: Option<String>,
        opts: WhisperOptions,
    ) -> Result<Self, AsrError> {
        if !model_path.is_file() {
            return Err(AsrError::Input(format!(
                "model file {} not found (run `mc models pull {name}`)",
                model_path.display()
            )));
        }
        // whisper.cpp prints a lot to stderr; this keeps `--json` output tidy.
        whisper_rs::install_logging_hooks();
        let path = model_path
            .to_str()
            .ok_or_else(|| AsrError::Input("model path is not valid UTF-8".to_string()))?;
        let ctx = WhisperContext::new_with_params(path, WhisperContextParameters::default())
            .map_err(|e| engine_error("cannot load the model", e))?;
        let state = ctx
            .create_state()
            .map_err(|e| engine_error("cannot create a decoding state", e))?;
        Ok(Self {
            state,
            name: name.to_string(),
            sha256,
            opts,
        })
    }

    /// One decoding pass. Returns the segments and the language used.
    fn pass(
        opts: &WhisperOptions,
        state: &mut WhisperState,
        audio: &[f32],
        language: Option<&str>,
        prompt: Option<&str>,
    ) -> Result<(Vec<RawSegment>, String), AsrError> {
        let strategy = if opts.beam_size > 1 {
            SamplingStrategy::BeamSearch {
                beam_size: opts.beam_size as i32,
                patience: -1.0,
            }
        } else {
            SamplingStrategy::Greedy { best_of: 1 }
        };
        let mut params = FullParams::new(strategy);
        params.set_n_threads(opts.threads as i32);
        // No language means "detect it", for every utterance.
        params.set_language(language);
        params.set_translate(false);
        // Each utterance stands alone: carrying text across them invites loops.
        params.set_no_context(true);
        params.set_suppress_blank(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_no_speech_thold(0.6);
        if let Some(prompt) = prompt {
            params.set_initial_prompt(prompt);
        }
        state
            .full(params, audio)
            .map_err(|e| engine_error("decoding failed", e))?;
        let detected = language.map(str::to_string).unwrap_or_else(|| {
            get_lang_str(state.full_lang_id_from_state())
                .unwrap_or("")
                .to_string()
        });
        let mut segments = Vec::new();
        for segment in state.as_iter() {
            let text = segment
                .to_str_lossy()
                .map_err(|e| engine_error("unreadable segment", e))?
                .trim()
                .to_string();
            let (mut sum, mut count) = (0.0f64, 0u32);
            for index in 0..segment.n_tokens() {
                let Some(token) = segment.get_token(index) else {
                    continue;
                };
                // Special tokens ([_BEG_], <|...|>) are not words.
                let is_special = token
                    .to_str_lossy()
                    .map_or(true, |t| t.starts_with("[_") || t.starts_with("<|"));
                if !is_special {
                    sum += f64::from(token.token_data().plog);
                    count += 1;
                }
            }
            segments.push(RawSegment {
                // Centiseconds to milliseconds.
                start_ms: segment.start_timestamp() as f64 * 10.0,
                end_ms: segment.end_timestamp() as f64 * 10.0,
                text,
                lang: Some(detected.clone()),
                avg_logprob: (count > 0).then(|| sum / f64::from(count)),
                no_speech_prob: Some(f64::from(segment.no_speech_probability())),
                words: Vec::new(),
            });
        }
        Ok((segments, detected))
    }
}

impl Transcriber for WhisperAsr {
    fn engine(&self) -> &str {
        "whisper.cpp"
    }

    fn model(&self) -> &str {
        &self.name
    }

    fn params(&self) -> Value {
        // Threads change speed, not text, so they are not part of the identity.
        json!({
            "beam_size": self.opts.beam_size,
            "languages": self.opts.languages.allowed,
            "fallback_language": self.opts.languages.fallback,
            "filter": {
                "no_speech_max": self.opts.filter.no_speech_max,
                "min_logprob": self.opts.filter.min_logprob,
                "loop_unique_ratio": self.opts.filter.loop_unique_ratio,
                "loop_min_words": self.opts.filter.loop_min_words,
            },
        })
    }

    fn model_sha256(&self) -> Option<String> {
        self.sha256.clone()
    }

    fn transcribe(
        &mut self,
        audio: &[f32],
        ctx: &TranscribeCtx<'_>,
    ) -> Result<Recognized, AsrError> {
        let mut padded;
        let audio = if audio.len() < MIN_SAMPLES {
            padded = audio.to_vec();
            padded.resize(MIN_SAMPLES, 0.0);
            &padded[..]
        } else {
            audio
        };
        let (mut segments, detected) =
            Self::pass(&self.opts, &mut self.state, audio, ctx.language, ctx.prompt)?;
        if ctx.language.is_none()
            && let Decision::Retry(language) = self.opts.languages.decide(&detected)
        {
            (segments, _) = Self::pass(
                &self.opts,
                &mut self.state,
                audio,
                Some(&language),
                ctx.prompt,
            )?;
        }
        let (segments, dropped) = apply(segments, &self.opts.filter);
        Ok(Recognized { segments, dropped })
    }
}
