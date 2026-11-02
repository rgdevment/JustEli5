//! The macOS privacy queries `eli5 doctor` asks; none of them shows a prompt.
#![allow(unsafe_code)]

use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};
use objc2_core_graphics::CGPreflightScreenCaptureAccess;

use crate::permission::Access;

pub fn microphone() -> Access {
    // SAFETY: a constant NSString exported by AVFoundation, set before any code runs.
    let Some(audio) = (unsafe { AVMediaTypeAudio }) else {
        return Access::NotAsked;
    };
    // SAFETY: audio is one of the two media types the method accepts.
    match unsafe { AVCaptureDevice::authorizationStatusForMediaType(audio) } {
        AVAuthorizationStatus::Authorized => Access::Granted,
        AVAuthorizationStatus::Denied => Access::Denied,
        AVAuthorizationStatus::Restricted => Access::Restricted,
        _ => Access::NotAsked,
    }
}

pub fn screen_recording() -> bool {
    CGPreflightScreenCaptureAccess()
}
