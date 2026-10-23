//! Capture sources: the `CaptureSource` trait, fakes and native UI adapters.

pub mod source;
pub mod ui;

pub use source::{CaptureSource, Discovery, FakeSource, FakeStep, SourceError};

#[cfg(windows)]
pub mod win_uia;

#[cfg(target_os = "macos")]
pub mod mac_ax;
