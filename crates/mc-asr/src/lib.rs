//! Local speech recognition framework: voice activity, utterance segmentation,
//! the `Transcriber` interface and the `asr.jsonl` files the fusion reads.
//!
//! The engines themselves (Whisper) live behind cargo features; everything
//! else here is deterministic and runs against scripted engines in tests.

pub mod codec;
pub mod fake;
pub mod filters;
pub mod glossary;
pub mod language;
pub mod live;
pub mod prompt;
pub mod run;
pub mod segmenter;
pub mod transcriber;
pub mod vad;
#[cfg(feature = "whisper")]
pub mod whisper;

pub use codec::{AsrMeta, AsrSegment, AsrWord, DroppedSegment, read_asr_jsonl, write_asr_jsonl};
pub use fake::FakeAsr;
pub use filters::FilterConfig;
pub use glossary::Glossary;
pub use language::{Decision, LanguagePolicy};
pub use live::{EngineFactory, LiveAsr, LiveAsrEvent, LiveConfig, LiveSummary};
pub use prompt::{MAX_PROMPT_CHARS, glossary_prompt, parse_glossary, parse_glossary_entries};
pub use run::{AsrRun, TranscribeOptions, transcribe_session};
pub use segmenter::{Segmenter, SegmenterConfig, Utterance};
pub use transcriber::{AsrError, Dropped, RawSegment, Recognized, TranscribeCtx, Transcriber};
pub use vad::{EnergyVad, Vad};

/// The only sample rate the engines see.
pub const ASR_RATE: u32 = 16_000;
