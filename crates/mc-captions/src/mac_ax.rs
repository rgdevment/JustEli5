//! macOS Accessibility capture (compile-checked from Windows, run on macOS).
#![allow(unsafe_code)]

pub mod backend;
mod element;

pub use backend::{MacAxBackend, MacOptions};

use objc2_application_services::{AXIsProcessTrusted, AXIsProcessTrustedWithOptions};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, CFType};

/// Whether this process has been granted Accessibility permission. With
/// `prompt`, macOS also shows its (asynchronous) permission dialog.
pub fn is_trusted(prompt: bool) -> bool {
    if !prompt {
        // SAFETY: argument-free query with no preconditions.
        return unsafe { AXIsProcessTrusted() };
    }
    // SAFETY: the key is a valid static CFString exported by the framework.
    let key = unsafe { objc2_application_services::kAXTrustedCheckOptionPrompt };
    let options = CFDictionary::<CFString, CFType>::from_slices(&[key], &[CFBoolean::new(true)]);
    // SAFETY: the dictionary maps a CFString to a CFBoolean, as documented.
    let options = unsafe { CFRetained::cast_unchecked::<CFDictionary>(options) };
    // SAFETY: `options` is a valid dictionary of the documented shape.
    unsafe { AXIsProcessTrustedWithOptions(Some(&options)) }
}

/// A meeting-captions source over the macOS Accessibility API.
pub fn source(
    platform: crate::ui::Platform,
    selectors: crate::ui::Selectors,
    mode: crate::ui::NodeMode,
    options: MacOptions,
) -> Result<crate::ui::UiaSource<MacAxBackend>, crate::ui::UiError> {
    let options = MacOptions {
        platform: Some(platform),
        selectors: selectors.clone(),
        ..options
    };
    Ok(
        crate::ui::UiaSource::new(MacAxBackend::new(options)?, platform, selectors, mode)
            .with_limits(crate::ui::source::Limits::macos()),
    )
}
