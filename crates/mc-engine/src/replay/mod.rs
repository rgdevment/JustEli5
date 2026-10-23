//! Replays a recorded corpus through the Rust reconciler and compares the
//! result with what the recorder wrote.

mod frames;
mod verify;

pub(crate) use verify::{debounce_is_mono, transcript_of};

pub use frames::{ReplayOp, reconstruct};
pub use verify::{
    Divergence, ReplayRun, VerifyReport, compare, run, run_cfg, run_with, run_with_now, verify,
};

use mc_store::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("captions file has no frame information (legacy v1 format)")]
    NoFrames,
    #[error("invalid corpus: {0}")]
    Invalid(String),
}
