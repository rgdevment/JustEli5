//! Windows UI Automation capture (compile-checked from Linux, run on Windows).
#![allow(unsafe_code)]

use std::marker::PhantomData;

pub mod backend;

pub use backend::{WinOptions, WinUiaBackend};

use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

/// Proof that this thread joined the COM multithreaded apartment.
///
/// Dropping it leaves the apartment (`CoUninitialize`), so every successful
/// [`init_com`] is balanced exactly once. It is `!Send`: COM initialisation is
/// per thread and must be undone on the thread that did it.
pub struct ComGuard {
    _not_send: PhantomData<*const ()>,
}

/// Joins the COM multithreaded apartment on the calling thread.
///
/// `S_OK` and `S_FALSE` (already initialised as MTA) both succeed and both
/// need balancing, which the guard does. `RPC_E_CHANGED_MODE` (the thread is
/// already in a single-threaded apartment) is returned as an error: COM is
/// still usable on that thread but there is nothing to balance.
pub fn init_com() -> windows::core::Result<ComGuard> {
    // SAFETY: plain COM initialisation on the current thread; success is
    // balanced by `ComGuard::drop`.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    Ok(ComGuard {
        _not_send: PhantomData,
    })
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        // SAFETY: a `ComGuard` only exists after a successful `CoInitializeEx`
        // on this thread (it is `!Send`), so this balances exactly that call.
        unsafe { CoUninitialize() }
    }
}

/// A meeting-captions source over Windows UI Automation. Build it on the thread
/// that will poll it (the capture thread): it joins that thread's COM apartment.
pub fn source(
    platform: crate::ui::Platform,
    selectors: crate::ui::Selectors,
    mode: crate::ui::NodeMode,
    options: WinOptions,
) -> Result<crate::ui::UiaSource<WinUiaBackend>, crate::ui::UiError> {
    Ok(crate::ui::UiaSource::new(
        WinUiaBackend::new(options)?,
        platform,
        selectors,
        mode,
    ))
}
