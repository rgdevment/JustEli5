//! The macOS privacy queries and requests behind `eli5 doctor`.
#![allow(unsafe_code)]

use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaType, AVMediaTypeAudio};
use objc2_core_graphics::{CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess};

use crate::permission::Access;

/// Long enough for a person to read the prompt and answer it.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(120);

fn audio() -> Option<&'static AVMediaType> {
    // SAFETY: a constant NSString exported by AVFoundation, set before any code runs.
    unsafe { AVMediaTypeAudio }
}

pub fn microphone() -> Access {
    let Some(audio) = audio() else {
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

/// macOS asks only once, so this waits for the answer only when it never asked.
pub fn request_microphone() {
    let Some(audio) = audio().filter(|_| microphone() == Access::NotAsked) else {
        return;
    };
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |_granted: Bool| {
        let _ = tx.send(());
    });
    // SAFETY: audio is an accepted media type; the system copies the block it keeps.
    unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(audio, &block) };
    let _ = rx.recv_timeout(ANSWER_TIMEOUT);
}

pub fn screen_recording() -> bool {
    CGPreflightScreenCaptureAccess()
}

/// Returns at once: the answer only counts for the next process.
pub fn request_screen_recording() {
    let _ = CGRequestScreenCaptureAccess();
}
