//! The unsafe edges shared by the macOS backends, each a few lines.
#![allow(unsafe_code)]

use std::mem::MaybeUninit;
use std::ptr::NonNull;

use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList};

use crate::ring::{self, SampleProducer};

/// Reads a fixed-size plain-data property (`u32`, `f64`, a C struct).
pub fn get_property<T: Copy>(
    object: AudioObjectID,
    selector: u32,
    qualifier: Option<&i32>,
) -> Result<T, i32> {
    let address = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let (qualifier_size, qualifier_ptr) = qualifier.map_or((0, std::ptr::null()), |q| {
        (size_of::<i32>() as u32, std::ptr::from_ref(q).cast())
    });
    let mut size = size_of::<T>() as u32;
    let mut out = MaybeUninit::<T>::zeroed();
    // SAFETY: every pointer is valid for the sizes given, and `T` is plain data, so zeros are a valid value.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            qualifier_size,
            qualifier_ptr,
            NonNull::from(&mut size),
            NonNull::from(&mut out).cast(),
        )
    };
    if status != 0 {
        return Err(status);
    }
    // SAFETY: the call succeeded and filled (at most) `size_of::<T>()` bytes of a zeroed value.
    Ok(unsafe { out.assume_init() })
}

/// The samples of one buffer of a float32 buffer list.
fn samples_of(buffer: &AudioBuffer) -> &[f32] {
    if buffer.mData.is_null() {
        return &[];
    }
    let len = buffer.mDataByteSize as usize / size_of::<f32>();
    // SAFETY: the system hands out `mDataByteSize` readable bytes, 16-byte aligned, valid for this call.
    unsafe { std::slice::from_raw_parts(buffer.mData.cast::<f32>(), len) }
}

/// Appends a float32 buffer list from an audio callback to the ring; a layout of
/// another channel count is flagged for the reader and returned.
///
/// # Safety
/// `list` must be a valid `AudioBufferList` whose `mNumberBuffers` buffers are readable.
pub unsafe fn push_list(
    producer: &mut SampleProducer,
    list: &AudioBufferList,
) -> Result<(), usize> {
    // SAFETY: the caller guarantees `mNumberBuffers` entries start at `mBuffers`.
    let buffers =
        unsafe { std::slice::from_raw_parts(list.mBuffers.as_ptr(), list.mNumberBuffers as usize) };
    let samples = buffers
        .iter()
        .map(|b| (b.mNumberChannels as usize, samples_of(b)));
    ring::push_buffers(producer, samples)
}
