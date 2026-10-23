//! Windows per-process loopback: only the audio of Teams / Zoom (and their
//! child processes), not the music or notifications of the rest of the system.
//! Needs Windows 10 build 20348 or later; older builds fall back to the endpoint
//! loopback. Compiled on Windows only, verified by hand.

use std::collections::VecDeque;

use wasapi::{
    AudioClient, DeviceEnumerator, Direction, SampleType, SessionState, StreamMode, WaveFormat,
    initialize_mta,
};

use crate::procmatch::{self, Choice, OutputSessions, WINDOWS_APPS, snapshot, tree_of};
use crate::source::{AudioSource, Read, StreamInfo};
use crate::timeline::AudioError;

const RATE: usize = 48_000;
const CHANNELS: usize = 2;
const BYTES_PER_FRAME: usize = CHANNELS * 4;
/// Process loopback delivers nothing while the process is silent, so a wait
/// that times out is normal and just means "no audio yet".
const WAIT_MS: u32 = 20;

/// Every output device with the processes that have a running stream on it.
fn output_sessions() -> Vec<OutputSessions> {
    let _ = initialize_mta();
    let Ok(enumerator) = DeviceEnumerator::new() else {
        return Vec::new();
    };
    let default = enumerator
        .get_default_device(&Direction::Render)
        .and_then(|d| d.get_id())
        .ok();
    let Ok(devices) = enumerator.get_device_collection(&Direction::Render) else {
        return Vec::new();
    };
    let mut outputs = Vec::new();
    for device in (0..devices.get_nbr_devices().unwrap_or(0))
        .filter_map(|index| devices.get_device_at_index(index).ok())
    {
        let Ok(name) = device.get_friendlyname() else {
            continue;
        };
        let mut active = Vec::new();
        if let Ok(sessions) = device
            .get_iaudiosessionmanager()
            .and_then(|m| m.get_audiosessionenumerator())
        {
            for session in
                (0..sessions.get_count().unwrap_or(0)).filter_map(|i| sessions.get_session(i).ok())
            {
                if session
                    .get_state()
                    .is_ok_and(|state| state == SessionState::Active)
                    && let Ok(pid) = session.get_process_id()
                {
                    active.push(pid);
                }
            }
        }
        outputs.push(OutputSessions {
            default: default.is_some() && device.get_id().ok() == default,
            device: name,
            active,
        });
    }
    outputs
}

/// The output device `pid` (or a process below it) plays the meeting on, when that
/// is not the default output.
pub fn meeting_output_for(pid: u32) -> Option<String> {
    procmatch::meeting_output(&output_sessions(), &tree_of(&snapshot(), pid))
}

/// The process the system channel should listen to.
pub fn choose_target(caption_pid: Option<u32>) -> Choice {
    // Processes with a running stream on any output (a call on a headset, music on
    // the speakers), not an idle app.
    let active: Vec<u32> = if caption_pid.is_some() {
        Vec::new()
    } else {
        output_sessions()
            .into_iter()
            .flat_map(|o| o.active)
            .collect()
    };
    procmatch::choose(&snapshot(), WINDOWS_APPS, caption_pid, |pid| {
        active.contains(&pid)
    })
}

/// Parses `process:<name|pid>`; `process:meeting` (or empty) looks for Teams/Zoom.
pub fn resolve(spec: &str) -> Result<(String, u32), AudioError> {
    procmatch::resolve(&snapshot(), spec, WINDOWS_APPS)
}
pub struct ProcessLoopbackSource {
    info: StreamInfo,
    client: AudioClient,
    capture: wasapi::AudioCaptureClient,
    event: wasapi::Handle,
    bytes: VecDeque<u8>,
    /// Buffers the system flagged as having lost data before them.
    discontinuities: u64,
}

impl ProcessLoopbackSource {
    pub fn open(label: &str, pid: u32) -> Result<Self, AudioError> {
        let fail = |what: &str, e: &dyn std::fmt::Display| {
            AudioError::Unavailable(format!("process loopback for {label}: {what}: {e}"))
        };
        // The recording thread owns this COM apartment for its whole life.
        let _ = initialize_mta();
        let format = WaveFormat::new(32, 32, &SampleType::Float, RATE, CHANNELS, None);
        let mut client = AudioClient::new_application_loopback_client(pid, true)
            .map_err(|e| fail("activation", &e))?;
        client
            .initialize_client(
                &format,
                &Direction::Capture,
                &StreamMode::EventsShared {
                    autoconvert: true,
                    // 200 ms: room to ride out a scheduling stall without overflowing.
                    buffer_duration_hns: 2_000_000,
                },
            )
            .map_err(|e| fail("initialisation", &e))?;
        let event = client
            .set_get_eventhandle()
            .map_err(|e| fail("event handle", &e))?;
        let capture = client
            .get_audiocaptureclient()
            .map_err(|e| fail("capture client", &e))?;
        client.start_stream().map_err(|e| fail("start", &e))?;
        Ok(Self {
            info: StreamInfo {
                name: format!("{label} (process loopback)"),
                device_id: Some(format!("pid:{pid}")),
                samplerate: RATE as u32,
                channels: CHANNELS as u16,
                latency_s: None,
            },
            client,
            capture,
            event,
            bytes: VecDeque::new(),
            discontinuities: 0,
        })
    }
}

impl AudioSource for ProcessLoopbackSource {
    fn info(&self) -> &StreamInfo {
        &self.info
    }

    fn read(&mut self, n_frames: usize) -> Result<Read, AudioError> {
        // Only wait when there is not enough audio already queued; otherwise a
        // backlog would drain one wait at a time and never catch up.
        if self.bytes.len() < n_frames * BYTES_PER_FRAME {
            // A timeout only means silence; a real failure shows up below.
            let _ = self.event.wait_for_event(WAIT_MS);
        }
        while self
            .capture
            .get_next_packet_size()
            .map_err(|e| AudioError::Device(format!("packet size: {e}")))?
            .unwrap_or(0)
            > 0
        {
            let before = self.bytes.len();
            let info = self
                .capture
                .read_from_device_to_deque(&mut self.bytes)
                .map_err(|e| AudioError::Device(format!("read: {e}")))?;
            if info.flags.silent {
                // The buffer's contents are undefined when it is flagged silent.
                self.bytes.iter_mut().skip(before).for_each(|b| *b = 0);
            }
            if info.flags.data_discontinuity {
                self.discontinuities += 1;
            }
        }
        let frames = (self.bytes.len() / BYTES_PER_FRAME).min(n_frames);
        if frames == 0 {
            return Ok(Read::Idle);
        }
        let mut samples = Vec::with_capacity(frames * CHANNELS);
        let mut word = [0u8; 4];
        for _ in 0..frames * CHANNELS {
            for byte in &mut word {
                *byte = self.bytes.pop_front().unwrap_or(0);
            }
            samples.push(f32::from_le_bytes(word));
        }
        Ok(Read::Block(samples))
    }

    fn dropped(&self) -> u64 {
        self.discontinuities
    }
}

impl Drop for ProcessLoopbackSource {
    fn drop(&mut self) {
        let _ = self.client.stop_stream();
    }
}
