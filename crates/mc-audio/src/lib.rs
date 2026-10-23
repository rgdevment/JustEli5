//! Audio for `mc`: keeping recorded samples on the session clock and writing
//! them next to the captions.
//!
//! The pure pieces (`timeline`, `resample`, `wav`) are checked against the
//! Python recorder's vectors; the threads (`channel`, `recorder`) run against
//! scripted sources, so everything is testable without a sound card.

pub mod channel;
#[cfg(any(windows, target_os = "macos"))]
pub mod cpal_backend;
pub mod devices;
#[cfg(target_os = "macos")]
mod mac_backend;
#[cfg(target_os = "macos")]
mod mac_ffi;
#[cfg(target_os = "macos")]
mod mac_sck;
#[cfg(target_os = "macos")]
mod mac_tap;
pub mod probe;
pub mod procmatch;
pub mod recorder;
pub mod resample;
pub mod ring;
pub mod silence;
pub mod source;
pub mod sync_log;
pub mod sysaudio;
pub mod timeline;
#[cfg(windows)]
pub mod wasapi_process;
pub mod wav;
pub mod wavfile;

pub use channel::{AudioChunk, ChannelConfig, ChannelInfo, ChannelRecorder, ChannelSpec, StatusFn};
pub use recorder::AudioCorpusRecorder;
pub use silence::SilenceAlert;
pub use source::{
    AudioBackend, AudioSource, FakeAudio, FakeBackend, Opener, Read, Step, StreamInfo,
};
pub use timeline::{AudioError, AudioTimeline, TimelineAligner, downmix, quantize, to_mono_int16};
pub use wavfile::{WavFileBackend, WavFileSource};

/// The machine's real devices, where this build supports them.
pub fn default_backend() -> Option<std::sync::Arc<dyn AudioBackend>> {
    #[cfg(windows)]
    return Some(std::sync::Arc::new(system::WindowsBackend));
    #[cfg(target_os = "macos")]
    return Some(std::sync::Arc::new(mac_backend::MacBackend::new()));
    #[cfg(not(any(windows, target_os = "macos")))]
    None
}

#[cfg(windows)]
mod system {
    use crate::cpal_backend::CpalBackend;
    use crate::procmatch::Choice;
    use crate::source::{AudioBackend, AudioSource};
    use crate::timeline::AudioError;
    use crate::wasapi_process::{
        ProcessLoopbackSource, choose_target, meeting_output_for, resolve,
    };

    /// cpal for the microphone and endpoint loopback, WASAPI process loopback
    /// for the meeting app's own audio.
    ///
    /// `system_device` chooses the system channel: empty or `auto` listens to the
    /// process that owns the caption window, else the meeting app that is playing
    /// audio, else the default output device (never an idle app). A meeting that
    /// plays on another output (a headset's chat output) is captured from that
    /// device, because its process loopback is silent;
    /// `process:<name|pid|meeting>` insists on a process; anything else is an
    /// output (or virtual input) device by name.
    pub struct WindowsBackend;

    impl AudioBackend for WindowsBackend {
        fn open_mic(&self, device: Option<&str>) -> Result<Box<dyn AudioSource>, AudioError> {
            CpalBackend.open_mic(device)
        }

        fn open_system(&self, device: Option<&str>) -> Result<Box<dyn AudioSource>, AudioError> {
            self.open_system_for(device, None)
        }

        fn open_system_for(
            &self,
            device: Option<&str>,
            meeting_pid: Option<u32>,
        ) -> Result<Box<dyn AudioSource>, AudioError> {
            match device.map(str::trim) {
                None | Some("" | "auto") => {
                    if let Choice::Process { label, pid, .. } = choose_target(meeting_pid) {
                        if let Some(device) = meeting_output_for(pid)
                            && let Ok(source) = CpalBackend.open_system(Some(&device))
                        {
                            return Ok(source);
                        }
                        if let Ok(source) = ProcessLoopbackSource::open(&label, pid) {
                            return Ok(Box::new(source));
                        }
                    }
                    CpalBackend.open_system(None)
                }
                Some(spec) => match spec.strip_prefix("process:") {
                    Some(process) => {
                        let (label, pid) = resolve(process)?;
                        Ok(Box::new(ProcessLoopbackSource::open(&label, pid)?))
                    }
                    None => CpalBackend.open_system(Some(spec)),
                },
            }
        }

        fn versions(&self) -> serde_json::Value {
            serde_json::json!({"audio_backend": "wasapi+cpal", "cpal": "0.18", "wasapi": "0.26"})
        }

        fn devices(&self) -> Vec<crate::devices::DeviceInfo> {
            CpalBackend.devices()
        }

        fn system_notes(&self, device: Option<&str>) -> Vec<String> {
            match device.map(str::trim) {
                None | Some("" | "auto") => {
                    let choice = choose_target(None);
                    let mut notes = vec![choice.describe()];
                    if let Choice::Process { label, pid, .. } = &choice
                        && let Some(device) = meeting_output_for(*pid)
                    {
                        notes.push(format!(
                            "system: {label} plays on {device}, not the default output: that device is captured"
                        ));
                    }
                    notes
                }
                Some(_) => Vec::new(),
            }
        }
    }
}
