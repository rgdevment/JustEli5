//! Pure caption reconciler (no I/O), a port of the Python `CaptureManager`.

pub mod difflib;
pub mod reconciler;

pub use reconciler::{
    Policy, Reconciler, ReconcilerConfig, StateSnapshot, StepCtx, StepOutcome, V2_REV, recent_hash,
};
