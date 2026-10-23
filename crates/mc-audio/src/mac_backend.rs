//! macOS: cpal for the microphone; for the meeting's audio a Core Audio process
//! tap (14.4+), else ScreenCaptureKit (13.0+), else a virtual device by name.
//! Compiled on macOS only; the decisions it takes live in `sysaudio`.

use std::sync::Arc;

use objc2_foundation::NSProcessInfo;

use crate::cpal_backend::CpalBackend;
use crate::devices::DeviceInfo;
use crate::mac_sck::SckSource;
use crate::mac_tap::{TapSource, Target, is_running_output};
use crate::procmatch::{self, Choice, MACOS_APPS, Proc, snapshot};
use crate::silence::SilenceAlert;
use crate::source::{AudioBackend, AudioSource};
use crate::sysaudio::{
    Mechanism, OpenFailure, OsVersion, SystemSpec, first_that_opens, mechanisms_for,
    parse_system_spec, silence_alert_for, system_note,
};
use crate::timeline::AudioError;

pub struct MacBackend {
    version: OsVersion,
}

fn os_version() -> OsVersion {
    let v = NSProcessInfo::processInfo().operatingSystemVersion();
    let part = |n: isize| u32::try_from(n).unwrap_or(0);
    OsVersion::new(
        part(v.majorVersion),
        part(v.minorVersion),
        part(v.patchVersion),
    )
}

type Opened = Result<Box<dyn AudioSource>, OpenFailure>;

fn boxed<S: AudioSource + 'static>(source: Result<S, OpenFailure>) -> Opened {
    source.map(|s| Box::new(s) as Box<dyn AudioSource>)
}

/// The caption window's app; else the one meeting app playing audio; else
/// everything. Only the pids of the meeting apps' trees are asked if they play.
fn choose_target(procs: &[Proc], caption_pid: Option<u32>) -> Choice {
    procmatch::choose(procs, MACOS_APPS, caption_pid, is_running_output)
}

fn tap_target(choice: Choice) -> Target {
    match choice {
        Choice::Process { label, pid, .. } => Target::App { label, root: pid },
        Choice::Everything { .. } => Target::Everything,
    }
}

/// Whether any other process is playing sound right now.
fn something_is_playing() -> bool {
    let own = std::process::id();
    snapshot()
        .iter()
        .any(|p| p.pid != own && is_running_output(p.pid))
}

impl MacBackend {
    pub fn new() -> Self {
        Self {
            version: os_version(),
        }
    }

    fn open_mechanism(&self, mechanism: Mechanism, meeting_pid: Option<u32>) -> Opened {
        match mechanism {
            Mechanism::ProcessTap => {
                // One process snapshot serves the choice and the tap.
                let procs = snapshot();
                let target = tap_target(choose_target(&procs, meeting_pid));
                boxed(TapSource::open(&target, &procs))
            }
            Mechanism::ScreenCaptureKit => boxed(SckSource::open()),
            Mechanism::NamedDevice => CpalBackend
                .open_system(None)
                .map_err(|e| OpenFailure::other(e.to_string())),
        }
    }

    fn open_process(&self, spec: &str) -> Result<Box<dyn AudioSource>, AudioError> {
        if mechanisms_for(self.version)[0] != Mechanism::ProcessTap {
            return Err(AudioError::Unavailable(format!(
                "process taps need macOS 14.4 or later (this is {})",
                self.version
            )));
        }
        let procs = snapshot();
        let (label, root) = procmatch::resolve(&procs, spec, MACOS_APPS)?;
        Ok(boxed(TapSource::open(
            &Target::App { label, root },
            &procs,
        ))?)
    }
}

impl AudioBackend for MacBackend {
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
        match parse_system_spec(device) {
            SystemSpec::Auto => first_that_opens(&mechanisms_for(self.version), |m| {
                self.open_mechanism(m, meeting_pid)
            }),
            SystemSpec::Process(spec) => self.open_process(&spec),
            SystemSpec::GlobalTap => Ok(boxed(TapSource::open(&Target::Everything, &[]))?),
            SystemSpec::ScreenCaptureKit => Ok(boxed(SckSource::open())?),
            SystemSpec::Device(name) => CpalBackend.open_system(Some(&name)),
        }
    }

    fn versions(&self) -> serde_json::Value {
        serde_json::json!({
            "audio_backend": "coreaudio-tap+screencapturekit+cpal",
            "macos": self.version.to_string(),
            "cpal": "0.18",
        })
    }

    fn devices(&self) -> Vec<DeviceInfo> {
        CpalBackend.devices()
    }

    fn system_notes(&self, device: Option<&str>) -> Vec<String> {
        let spec = parse_system_spec(device);
        let mut notes = vec![system_note(self.version, &spec)];
        if spec == SystemSpec::Auto && mechanisms_for(self.version)[0] == Mechanism::ProcessTap {
            notes.push(choose_target(&snapshot(), None).describe());
        }
        notes
    }

    fn silence_alert(&self, channel: &str, system_device: Option<&str>) -> Option<SilenceAlert> {
        silence_alert_for(
            channel,
            self.version,
            &parse_system_spec(system_device),
            Arc::new(something_is_playing),
        )
    }
}
