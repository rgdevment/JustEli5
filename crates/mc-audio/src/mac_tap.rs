//! Core Audio process tap (macOS 14.4+): the audio of the meeting app's
//! processes, or of every app but this one, through a private aggregate device
//! whose IOProc only pushes into a lock-free ring. Compiled on macOS only,
//! verified by hand (see the A-mac checklist in the README).
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_core_audio::{
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart,
    AudioDeviceStop, AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap, AudioObjectID,
    CATapDescription, CATapMuteBehavior, kAudioAggregateDeviceIsPrivateKey,
    kAudioAggregateDeviceNameKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey,
    kAudioHardwarePropertyTranslatePIDToProcessObject, kAudioObjectSystemObject,
    kAudioObjectUnknown, kAudioProcessPropertyIsRunningOutput, kAudioSubTapDriftCompensationKey,
    kAudioSubTapUIDKey, kAudioTapPropertyFormat,
};
use objc2_core_audio_types::{
    AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp, kAudioFormatFlagIsFloat,
};
use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFRetained, CFString, CFType};
use objc2_foundation::{NSArray, NSNumber, NSString};

use crate::mac_ffi::{get_property, push_list};
use crate::procmatch::{Proc, snapshot, tree_of};
use crate::ring::{SampleConsumer, SampleProducer, sample_ring};
use crate::source::{AudioSource, Read, StreamInfo};
use crate::sysaudio::{OpenFailure, core_audio_failure, format_changed, needs_rebuild};
use crate::timeline::AudioError;

/// How often `read` checks that the tap still delivers the format it was opened with.
const FORMAT_CHECK: Duration = Duration::from_secs(1);
/// How often the app's process tree is looked at again for new helper processes.
const TREE_CHECK: Duration = Duration::from_secs(2);
/// Frames asked of the old ring when a tap is replaced: more than it can hold.
const DRAIN_FRAMES: usize = 1 << 22;

/// What the tap listens to.
pub enum Target {
    /// This app's process tree (labelled for the manifest).
    App { label: String, root: u32 },
    /// Every app but this one.
    Everything,
}

/// What the IOProc needs; owned by the session until the IOProc is destroyed.
struct Context {
    producer: SampleProducer,
}

/// Whatever of tap, aggregate device and IOProc exists so far; dropping it
/// tears down what was built, in reverse order.
#[derive(Default)]
struct Session {
    tap: Option<AudioObjectID>,
    aggregate: Option<AudioObjectID>,
    io_proc: AudioDeviceIOProcID,
    started: bool,
    context: Option<*mut Context>,
}

impl Drop for Session {
    fn drop(&mut self) {
        // Teardown errors have nowhere to go and the objects are gone either way.
        if let (Some(aggregate), true) = (self.aggregate, self.started) {
            // SAFETY: the device and IOProc ids were created by this session.
            unsafe { AudioDeviceStop(aggregate, self.io_proc) };
        }
        if let Some(aggregate) = self.aggregate {
            if self.io_proc.is_some() {
                // SAFETY: as above; stopping first means no callback is running.
                unsafe { AudioDeviceDestroyIOProcID(aggregate, self.io_proc) };
            }
            // SAFETY: created by this session and not used after this.
            unsafe { AudioHardwareDestroyAggregateDevice(aggregate) };
        }
        if let Some(tap) = self.tap {
            // SAFETY: created by this session and not used after this.
            unsafe { AudioHardwareDestroyProcessTap(tap) };
        }
        if let Some(context) = self.context {
            // SAFETY: the pointer came from `Box::into_raw` and no callback can run any more.
            drop(unsafe { Box::from_raw(context) });
        }
    }
}

/// A running tap: its session, the ring it fills and what it was built for.
struct Built {
    _session: Session,
    ring: SampleConsumer,
    tap: AudioObjectID,
    /// `(rate, channels)` the tap delivers.
    format: (u32, u32),
    /// Process objects the tap covers; empty for the all-apps tap.
    objects: Vec<AudioObjectID>,
}

pub struct TapSource {
    info: StreamInfo,
    built: Built,
    /// The app being followed, when the tap is not the all-apps one.
    app: Option<(String, u32)>,
    carry: Vec<f32>,
    next_format_check: Instant,
    next_tree_check: Instant,
}

/// The audio process object of `pid`, if that process talks to Core Audio.
pub fn process_object(pid: u32) -> Option<AudioObjectID> {
    let pid = i32::try_from(pid).ok()?;
    get_property::<AudioObjectID>(
        kAudioObjectSystemObject as AudioObjectID,
        kAudioHardwarePropertyTranslatePIDToProcessObject,
        Some(&pid),
    )
    .ok()
    .filter(|id| *id != kAudioObjectUnknown)
}

/// Whether the process is playing sound right now.
pub fn is_running_output(pid: u32) -> bool {
    process_object(pid).is_some_and(|object| {
        get_property::<u32>(object, kAudioProcessPropertyIsRunningOutput, None)
            .is_ok_and(|v| v != 0)
    })
}

/// Core Audio process objects of `root` and its children.
fn objects_of(procs: &[Proc], root: u32) -> Vec<AudioObjectID> {
    let mut objects: Vec<AudioObjectID> = tree_of(procs, root)
        .into_iter()
        .filter_map(process_object)
        .collect();
    objects.sort_unstable();
    objects.dedup();
    objects
}

fn describe(objects: &[AudioObjectID]) -> Retained<CATapDescription> {
    let numbers: Vec<Retained<NSNumber>> = if objects.is_empty() {
        // Our own process renders nothing worth excluding; an empty list taps every app.
        process_object(std::process::id())
            .map(NSNumber::new_u32)
            .into_iter()
            .collect()
    } else {
        objects.iter().copied().map(NSNumber::new_u32).collect()
    };
    let list = NSArray::from_retained_slice(&numbers);
    // SAFETY: plain Objective-C initialisers and setters on a fresh description.
    unsafe {
        let description = if objects.is_empty() {
            CATapDescription::initStereoGlobalTapButExcludeProcesses(
                CATapDescription::alloc(),
                &list,
            )
        } else {
            CATapDescription::initStereoMixdownOfProcesses(CATapDescription::alloc(), &list)
        };
        description.setName(&NSString::from_str("mc system audio"));
        description.setPrivate(true);
        description.setMuteBehavior(CATapMuteBehavior::Unmuted);
        description
    }
}

fn cf(text: &std::ffi::CStr) -> CFRetained<CFString> {
    CFString::from_str(&text.to_string_lossy())
}

/// Dictionary of a private aggregate device holding only the tap.
fn aggregate_properties(tap_uuid: &str, name: &str) -> CFRetained<CFDictionary> {
    let tap = CFDictionary::<CFString, CFType>::from_slices(
        &[
            &cf(kAudioSubTapUIDKey),
            &cf(kAudioSubTapDriftCompensationKey),
        ],
        &[
            CFString::from_str(tap_uuid).as_ref(),
            CFBoolean::new(true).as_ref(),
        ],
    );
    let tap_dict: &CFDictionary = tap.as_ref();
    let taps = CFArray::<CFDictionary>::from_objects(&[tap_dict]);
    let aggregate = CFDictionary::<CFString, CFType>::from_slices(
        &[
            &cf(kAudioAggregateDeviceNameKey),
            &cf(kAudioAggregateDeviceUIDKey),
            &cf(kAudioAggregateDeviceTapListKey),
            &cf(kAudioAggregateDeviceTapAutoStartKey),
            &cf(kAudioAggregateDeviceIsPrivateKey),
        ],
        &[
            CFString::from_str(name).as_ref(),
            CFString::from_str(name).as_ref(),
            AsRef::<CFType>::as_ref(&*taps),
            CFBoolean::new(true).as_ref(),
            CFBoolean::new(true).as_ref(),
        ],
    );
    // SAFETY: a dictionary of CF types is a valid untyped `CFDictionary`.
    unsafe { CFRetained::cast_unchecked::<CFDictionary>(aggregate) }
}

/// Real-time callback: only copies into the ring; a format mismatch is flagged for the reader.
unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    _output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> i32 {
    // SAFETY: `client` is the `Context` registered below; only this callback touches it, and it lives until the IOProc is destroyed.
    let context = unsafe { &mut *client.cast::<Context>() };
    // SAFETY: the HAL passes a valid input list for the duration of this call.
    let _ = unsafe { push_list(&mut context.producer, input.as_ref()) };
    0
}

fn create_tap(session: &mut Session, objects: &[AudioObjectID]) -> Result<String, OpenFailure> {
    let description = describe(objects);
    let mut tap: AudioObjectID = kAudioObjectUnknown;
    // SAFETY: the description is valid and `tap` is a valid out pointer.
    let status = unsafe { AudioHardwareCreateProcessTap(Some(&description), &raw mut tap) };
    if status != 0 {
        return Err(core_audio_failure(status, "tap creation"));
    }
    session.tap = Some(tap);
    // SAFETY: reading the identifier of a valid description.
    Ok(unsafe { description.UUID().UUIDString() }.to_string())
}

fn create_aggregate(session: &mut Session, tap_uuid: &str) -> Result<AudioObjectID, OpenFailure> {
    // The device is destroyed asynchronously, so a reopen must not reuse its UID.
    let name = format!("mc system audio {tap_uuid}");
    let properties = aggregate_properties(tap_uuid, &name);
    let mut aggregate: AudioObjectID = kAudioObjectUnknown;
    // SAFETY: valid dictionary and out pointer.
    let status =
        unsafe { AudioHardwareCreateAggregateDevice(&properties, NonNull::from(&mut aggregate)) };
    if status != 0 {
        return Err(core_audio_failure(status, "aggregate device creation"));
    }
    session.aggregate = Some(aggregate);
    Ok(aggregate)
}

fn start_io(
    session: &mut Session,
    aggregate: AudioObjectID,
    producer: SampleProducer,
) -> Result<(), OpenFailure> {
    let pointer = Box::into_raw(Box::new(Context { producer }));
    session.context = Some(pointer);
    // SAFETY: `io_proc` matches the callback type and `pointer` outlives it (freed in `Session::drop`).
    let status = unsafe {
        AudioDeviceCreateIOProcID(
            aggregate,
            Some(io_proc),
            pointer.cast(),
            NonNull::from(&mut session.io_proc),
        )
    };
    if status != 0 {
        return Err(core_audio_failure(status, "IOProc creation"));
    }
    // SAFETY: the device and IOProc were just created.
    let status = unsafe { AudioDeviceStart(aggregate, session.io_proc) };
    if status != 0 {
        return Err(core_audio_failure(status, "start"));
    }
    session.started = true;
    Ok(())
}

/// `(rate, channels)` of the tap's own stream, which must be float32.
fn tap_format(tap: AudioObjectID) -> Result<(u32, u32), OpenFailure> {
    let format = get_property::<AudioStreamBasicDescription>(tap, kAudioTapPropertyFormat, None)
        .map_err(|status| core_audio_failure(status, "tap format query"))?;
    if format.mFormatFlags & kAudioFormatFlagIsFloat == 0 || format.mBitsPerChannel != 32 {
        return Err(OpenFailure::other(
            "the tap delivers a format other than float32".to_string(),
        ));
    }
    Ok((format.mSampleRate as u32, format.mChannelsPerFrame.max(1)))
}

/// Builds a tap over `objects` (empty: every app but this one) and starts it.
fn build(objects: Vec<AudioObjectID>) -> Result<Built, OpenFailure> {
    let mut session = Session::default();
    let tap_uuid = create_tap(&mut session, &objects)?;
    let tap = session.tap.unwrap_or(kAudioObjectUnknown);
    let aggregate = create_aggregate(&mut session, &tap_uuid)?;
    let format = tap_format(tap)?;
    let (producer, ring) = sample_ring(format.0, format.1 as usize);
    start_io(&mut session, aggregate, producer)?;
    Ok(Built {
        _session: session,
        ring,
        tap,
        format,
        objects,
    })
}

impl TapSource {
    /// `procs` is the snapshot the target was chosen from.
    pub fn open(target: &Target, procs: &[Proc]) -> Result<Self, OpenFailure> {
        let (objects, app, name, device_id) = match target {
            Target::App { label, root } => {
                let objects = objects_of(procs, *root);
                if objects.is_empty() {
                    // Never a tap over an empty process list: it would capture nothing.
                    (
                        Vec::new(),
                        None,
                        format!("all apps (process tap; {label} is not on Core Audio yet)"),
                        "tap:all".to_string(),
                    )
                } else {
                    (
                        objects,
                        Some((label.clone(), *root)),
                        format!("{label} (process tap)"),
                        format!("pid:{root}"),
                    )
                }
            }
            Target::Everything => (
                Vec::new(),
                None,
                "all apps (process tap)".to_string(),
                "tap:all".to_string(),
            ),
        };
        let built = build(objects)?;
        Ok(Self {
            info: StreamInfo {
                name,
                device_id: Some(device_id),
                samplerate: built.format.0,
                channels: built.format.1 as u16,
                latency_s: None,
            },
            built,
            app,
            carry: Vec::new(),
            next_format_check: Instant::now() + FORMAT_CHECK,
            next_tree_check: Instant::now() + TREE_CHECK,
        })
    }

    /// The tap's own stream must still be what was opened; the recorder reopens on the error.
    fn check_format(&mut self) -> Result<(), AudioError> {
        if Instant::now() < self.next_format_check {
            return Ok(());
        }
        self.next_format_check = Instant::now() + FORMAT_CHECK;
        let now = tap_format(self.built.tap).map_err(|f| AudioError::Device(f.message))?;
        let opened = (self.info.samplerate, u32::from(self.info.channels));
        if !format_changed(opened, now) {
            return Ok(());
        }
        Err(AudioError::Device(format!(
            "the tap format changed from {opened:?} to {now:?}"
        )))
    }

    /// Helper processes started after the tap was built (Chrome's audio service,
    /// a WebView) are not in it: build a new tap over the whole tree and swap.
    fn follow_tree(&mut self) {
        let Some((_, root)) = &self.app else { return };
        if Instant::now() < self.next_tree_check {
            return;
        }
        self.next_tree_check = Instant::now() + TREE_CHECK;
        let objects = objects_of(&snapshot(), *root);
        if !needs_rebuild(&self.built.objects, &objects) {
            return;
        }
        // A failed rebuild keeps the old tap and is tried again at the next check.
        let Ok(mut fresh) = build(objects) else {
            return;
        };
        let same_format = fresh.format == self.built.format;
        std::mem::swap(&mut self.built, &mut fresh);
        // A different format is caught by `check_format`, which makes the recorder reopen.
        if same_format {
            self.carry.extend(fresh.ring.try_read(DRAIN_FRAMES));
        }
    }
}

impl AudioSource for TapSource {
    fn info(&self) -> &StreamInfo {
        &self.info
    }

    fn dropped(&self) -> u64 {
        self.built.ring.overruns()
    }

    fn read(&mut self, n_frames: usize) -> Result<Read, AudioError> {
        self.check_format()?;
        self.follow_tree();
        if !self.carry.is_empty() {
            let channels = usize::from(self.info.channels.max(1));
            let take = (n_frames * channels).min(self.carry.len()) / channels * channels;
            return Ok(Read::Block(self.carry.drain(..take).collect()));
        }
        self.built.ring.read(n_frames)
    }
}
