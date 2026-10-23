//! Merging Teams live captions with a local speech-recognition transcript.
//!
//! Pure functions over already-reconciled caption segments and ASR segments:
//! no files, no clocks, nothing to mock.

pub mod align;
pub mod fuse;
pub mod glossary;
pub mod lag;
pub mod render;
pub mod text;

pub use fuse::{
    Edit, FusedSegment, FusionConfig, FusionCounts, FusionReport, FusionResult, Source, fuse,
};
pub use lag::{LagModel, LagSummary};
