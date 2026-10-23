//! Corpus files (JSONL/manifest) and the SQLite index built from them.
//!
//! The JSONL files are the source of truth and stay interchangeable with the
//! Python implementation: `captions.jsonl` v2 and the legacy `raw_uia_*.jsonl`
//! are read, v2 is written, and `manifest.json` round-trips unknown fields.

pub mod db;
pub mod error;
pub mod jsonl;
pub mod manifest;
pub mod series;

pub use db::{
    AsrRunRow, FNV_OFFSET, FusedRow, FusionRow, ImportAction, ImportOutcome, ImportReport,
    RebuildReason, RunRow, SegmentRow, SeriesMember, SeriesSummary, SessionSummary, Store,
    fnv_extend,
};
pub use error::StoreError;
pub use series::{SeriesOrigin, SeriesRule, clean_key, series_for_name, series_key, user_key};
