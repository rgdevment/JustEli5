//! One module per subcommand.

pub mod analysis;
pub mod analyze;
pub mod capture;
pub mod carry;
pub mod compare;
pub mod detect;
pub mod doctor;
pub mod eval;
pub mod export;
pub mod export_tisty;
pub mod fuse;
pub mod glossary;
pub mod import;
pub mod llm;
pub mod mcp;
pub mod memory;
pub mod minutes;
pub mod models;
pub mod prune;
pub mod replay;
pub mod run;
pub mod series;
pub mod session_fusion;
pub mod status;
pub mod transcribe;
pub mod tui;
pub mod vocabulary;

pub use mc_core::time::fmt_utc;

/// `mm:ss`/`h:mm:ss` from milliseconds.
pub fn fmt_duration(ms: Option<i64>) -> String {
    mc_engine::runs::fmt_rel(ms.map(|v| v as f64))
}
