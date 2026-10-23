//! ScreenCaptureKit audio (macOS 13.0 to 14.3, where process taps do not
//! exist): the sound of every app but this one, from a stream whose video is
//! cut to 2x2 pixels at 1 fps. Needs the Screen Recording permission. Compiled
//! on macOS only, verified by hand (see the A-mac checklist in the README).
#![allow(unsafe_code)]

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError, mpsc};
use std::time::Duration;

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_core_audio_types::AudioBufferList;
use objc2_core_foundation::CFRetained;
use objc2_core_media::{CMBlockBuffer, CMSampleBuffer, CMTime, CMTimeFlags};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamDelegate, SCStreamOutput, SCStreamOutputType,
};

use crate::mac_ffi::push_list;
use crate::ring::{SampleConsumer, SampleProducer, Signal, sample_ring};
use crate::source::{AudioSource, Read, StreamInfo};
use crate::sysaudio::{OpenFailure, sck_failure};
use crate::timeline::AudioError;

const RATE: u32 = 48_000;
const CHANNELS: usize = 2;
/// The first call may sit behind the system's permission prompt.
const SYSTEM_REPLY_TIMEOUT: Duration = Duration::from_secs(30);
/// `kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment`.
const ASSURE_16_BYTE_ALIGNMENT: u32 = 1;

/// What the callbacks need. The system may still call them after teardown, so
/// everything lives in the object and `stopped` turns them into no-ops.
struct OutputState {
    producer: Mutex<SampleProducer>,
    signal: Signal,
    stopped: AtomicBool,
}

impl OutputState {
    /// Copies one audio sample buffer into the ring.
    fn deliver(&self, buffer: &CMSampleBuffer) {
        if self.stopped.load(Ordering::Acquire) {
            return;
        }
        let mut producer = self.producer.lock().unwrap_or_else(PoisonError::into_inner);
        copy_audio(buffer, &mut producer, &self.signal);
    }
}

fn copy_audio(buffer: &CMSampleBuffer, producer: &mut SampleProducer, signal: &Signal) {
    let mut needed = 0usize;
    // SAFETY: a size query: no list is passed, the size out pointer is valid.
    let status = unsafe {
        buffer.audio_buffer_list_with_retained_block_buffer(
            &raw mut needed,
            std::ptr::null_mut(),
            0,
            None,
            None,
            0,
            std::ptr::null_mut(),
        )
    };
    if status != 0 || needed == 0 {
        // A buffer without audio data (not an error of the stream): count it as a drop.
        signal.count_overrun();
        return;
    }
    // u64 storage: the list holds pointers, so it needs 8-byte alignment.
    let mut storage = vec![0u64; needed.div_ceil(size_of::<u64>())];
    let list = storage.as_mut_ptr().cast::<AudioBufferList>();
    let mut block: *mut CMBlockBuffer = std::ptr::null_mut();
    // SAFETY: `list` has `needed` writable bytes; `block` is a valid out pointer.
    let status = unsafe {
        buffer.audio_buffer_list_with_retained_block_buffer(
            std::ptr::null_mut(),
            list,
            needed,
            None,
            None,
            ASSURE_16_BYTE_ALIGNMENT,
            &raw mut block,
        )
    };
    // SAFETY: on success the call returned a retained block buffer, which keeps the samples alive.
    let _block = NonNull::new(block).map(|p| unsafe { CFRetained::from_raw(p) });
    if status != 0 {
        signal.count_overrun();
        return;
    }
    // SAFETY: the call filled `list` with buffers that `_block` keeps readable.
    // A layout of another channel count is flagged for the reader inside.
    let _ = unsafe { push_list(producer, &*list) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "McAudioOutput"]
    #[ivars = OutputState]
    struct AudioOutput;

    unsafe impl NSObjectProtocol for AudioOutput {}

    unsafe impl SCStreamOutput for AudioOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(
            &self,
            _stream: &SCStream,
            buffer: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind == SCStreamOutputType::Audio {
                self.ivars().deliver(buffer);
            }
        }
    }

    unsafe impl SCStreamDelegate for AudioOutput {
        #[unsafe(method(stream:didStopWithError:))]
        fn did_stop(&self, _stream: &SCStream, error: &NSError) {
            let failure = error_failure(error);
            self.ivars().signal.fail(failure.message);
        }
    }
);

impl AudioOutput {
    fn new(producer: SampleProducer) -> Retained<Self> {
        let signal = producer.signal();
        let this = Self::alloc().set_ivars(OutputState {
            producer: Mutex::new(producer),
            signal,
            stopped: AtomicBool::new(false),
        });
        // SAFETY: `init` of the NSObject superclass.
        unsafe { msg_send![super(this), init] }
    }
}

fn error_failure(error: &NSError) -> OpenFailure {
    sck_failure(
        error.code() as i64,
        &error.localizedDescription().to_string(),
    )
}

/// An Objective-C object handed from the system's callback thread to ours.
struct Handoff<T>(Retained<T>);

// SAFETY: ScreenCaptureKit objects are reference-counted and the framework itself passes them between threads.
unsafe impl<T> Send for Handoff<T> {}

fn timed_out() -> OpenFailure {
    OpenFailure::other(
        "ScreenCaptureKit did not answer (is a permission prompt waiting?)".to_string(),
    )
}

/// The main display, which the system lists only when Screen Recording is allowed.
fn first_display() -> Result<Retained<SCDisplay>, OpenFailure> {
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            // SAFETY: the system passes either a valid object or null for each argument.
            let result = match unsafe { content.as_ref() } {
                // SAFETY: as above, and `displays` is a plain getter.
                Some(content) => unsafe { content.displays() }
                    .firstObject()
                    .map(Handoff)
                    .ok_or_else(|| OpenFailure::other("no display to capture the audio of".into())),
                // SAFETY: as above.
                None => Err(unsafe { error.as_ref() }.map_or_else(timed_out, error_failure)),
            };
            let _ = tx.send(result);
        },
    );
    // SAFETY: the block stays alive for the call; the system copies it if it keeps it.
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&block) };
    let reply = rx
        .recv_timeout(SYSTEM_REPLY_TIMEOUT)
        .map_err(|_| timed_out())?;
    reply.map(|display| display.0)
}

fn start(stream: &SCStream) -> Result<(), OpenFailure> {
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |error: *mut NSError| {
        // SAFETY: the system passes a valid error or null.
        let _ = tx.send(unsafe { error.as_ref() }.map(error_failure));
    });
    // SAFETY: the block stays alive for the call; the system copies it if it keeps it.
    unsafe { stream.startCaptureWithCompletionHandler(Some(&block)) };
    match rx.recv_timeout(SYSTEM_REPLY_TIMEOUT) {
        Ok(None) => Ok(()),
        Ok(Some(failure)) => Err(failure),
        Err(_) => Err(timed_out()),
    }
}

fn configuration() -> Retained<SCStreamConfiguration> {
    // SAFETY: plain setters on a fresh configuration.
    unsafe {
        let config = SCStreamConfiguration::new();
        config.setCapturesAudio(true);
        config.setExcludesCurrentProcessAudio(true);
        config.setSampleRate(RATE as isize);
        config.setChannelCount(CHANNELS as isize);
        // Video is not wanted: the smallest picture at the slowest rate.
        config.setWidth(2);
        config.setHeight(2);
        config.setMinimumFrameInterval(CMTime {
            value: 1,
            timescale: 1,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        });
        config
    }
}

pub struct SckSource {
    info: StreamInfo,
    ring: SampleConsumer,
    stream: Retained<SCStream>,
    output: Retained<AudioOutput>,
    _dispatch: DispatchRetained<DispatchQueue>,
}

impl SckSource {
    pub fn open() -> Result<Self, OpenFailure> {
        let display = first_display()?;
        // SAFETY: Objective-C initialisers with valid arguments.
        let filter = unsafe {
            SCContentFilter::initWithDisplay_excludingWindows(
                SCContentFilter::alloc(),
                &display,
                &NSArray::new(),
            )
        };
        let (producer, ring) = sample_ring(RATE, CHANNELS);
        let output = AudioOutput::new(producer);
        let handler = ProtocolObject::from_ref(&*output);
        // SAFETY: valid filter, configuration and delegate; the output outlives the stream (see `Drop`).
        let stream = unsafe {
            SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &configuration(),
                Some(ProtocolObject::from_ref(&*output)),
            )
        };
        let dispatch = DispatchQueue::new("mc.audio.screencapturekit", DispatchQueueAttr::SERIAL);
        // SAFETY: a serial queue and a live output.
        unsafe {
            stream.addStreamOutput_type_sampleHandlerQueue_error(
                handler,
                SCStreamOutputType::Audio,
                Some(&dispatch),
            )
        }
        .map_err(|error| error_failure(&error))?;
        start(&stream)?;
        Ok(Self {
            info: StreamInfo {
                name: "all apps (ScreenCaptureKit)".to_string(),
                device_id: Some("sck:display".to_string()),
                samplerate: RATE,
                channels: CHANNELS as u16,
                latency_s: None,
            },
            ring,
            stream,
            output,
            _dispatch: dispatch,
        })
    }
}

/// How long teardown waits for the system to confirm the stream stopped.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

impl Drop for SckSource {
    fn drop(&mut self) {
        // Callbacks already queued run as no-ops from here on.
        self.output.ivars().stopped.store(true, Ordering::Release);
        let (tx, rx) = mpsc::channel();
        let block = RcBlock::new(move |_error: *mut NSError| {
            let _ = tx.send(());
        });
        // SAFETY: the block lives through the call; the system copies it if it keeps it.
        unsafe { self.stream.stopCaptureWithCompletionHandler(Some(&block)) };
        // Bounded: a stream that never answers must not hang the recorder.
        let _ = rx.recv_timeout(STOP_TIMEOUT);
        // SAFETY: removing the output we added; nobody to tell if it fails.
        let _ = unsafe {
            self.stream.removeStreamOutput_type_error(
                ProtocolObject::from_ref(&*self.output),
                SCStreamOutputType::Audio,
            )
        };
    }
}

impl AudioSource for SckSource {
    fn info(&self) -> &StreamInfo {
        &self.info
    }

    fn dropped(&self) -> u64 {
        self.ring.overruns()
    }

    fn read(&mut self, n_frames: usize) -> Result<Read, AudioError> {
        self.ring.read(n_frames)
    }
}
