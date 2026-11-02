//! Capture sources: the `CaptureSource` trait, fakes and native UI adapters.

pub mod source;
pub mod ui;

pub use source::{CaptureSource, Discovery, FakeSource, FakeStep, SourceError};

#[cfg(windows)]
pub mod win_uia;

#[cfg(target_os = "macos")]
pub mod mac_ax;

/// Whether this process may read other apps' windows; `None` where the OS does not
/// ask (Windows UI Automation needs no permission). With `prompt`, macOS shows its dialog.
pub fn accessibility_trusted(prompt: bool) -> Option<bool> {
    #[cfg(target_os = "macos")]
    return Some(mac_ax::is_trusted(prompt));
    #[cfg(not(target_os = "macos"))]
    {
        let _ = prompt;
        None
    }
}
