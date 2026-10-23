//! Microphone, output-device loopback and named devices through `cpal`
//! (WASAPI on Windows, CoreAudio on macOS). Compiled only on those systems and
//! verified by hand (see the A1-A6 checklist in the README).
//!
//! * mic: the default input device, or the one whose name contains `device`.
//! * system: the named device, or the default output device. An output device
//!   opened for input is captured as loopback (WASAPI loopback; a CoreAudio
//!   process tap on macOS 14.6+). On older macOS name a virtual input such as
//!   BlackHole instead.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, ErrorKind, SampleFormat, SizedSample, StreamConfig};

use crate::devices::DeviceInfo;
use crate::ring::{SampleConsumer, SampleProducer, sample_ring};
use crate::source::{AudioBackend, AudioSource, Read, StreamInfo};
use crate::timeline::AudioError;

pub struct CpalBackend;

fn map_error(error: &cpal::Error) -> AudioError {
    match error.kind() {
        ErrorKind::PermissionDenied => AudioError::Unavailable(format!(
            "audio access was denied by the system ({error}); allow microphone access for this app{}",
            if cfg!(target_os = "macos") {
                format!(" in {}", crate::sysaudio::mic_settings())
            } else {
                String::new()
            }
        )),
        ErrorKind::DeviceNotAvailable | ErrorKind::HostUnavailable => {
            AudioError::Unavailable(error.to_string())
        }
        _ => AudioError::Device(error.to_string()),
    }
}

fn name_of(device: &cpal::Device) -> String {
    device
        .description()
        .map_or_else(|_| device.to_string(), |d| d.name().to_string())
}

/// Exact (case-insensitive) match first, then a substring.
fn find_named(
    host: &cpal::Host,
    wanted: &str,
    input_only: bool,
) -> Result<cpal::Device, AudioError> {
    let wanted = wanted.to_lowercase();
    let devices: Vec<cpal::Device> = host
        .devices()
        .map_err(|e| map_error(&e))?
        .filter(|d| !input_only || d.supports_input())
        .collect();
    let names: Vec<String> = devices.iter().map(|d| name_of(d).to_lowercase()).collect();
    let position = names
        .iter()
        .position(|n| *n == wanted)
        .or_else(|| names.iter().position(|n| n.contains(&wanted)));
    position
        .map(|i| devices[i].clone())
        .ok_or_else(|| AudioError::Unavailable(format!("no audio device named {wanted:?}")))
}

struct CpalSource {
    info: StreamInfo,
    ring: SampleConsumer,
    // Dropping the stream stops the callbacks.
    _stream: cpal::Stream,
}

fn build<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mut producer: SampleProducer,
    convert: fn(T) -> f32,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + Copy + Send + 'static,
{
    let failed = producer.signal();
    device.build_input_stream::<T, _, _>(
        config,
        move |block: &[T], _| producer.push(block.len(), block.iter().map(|v| convert(*v))),
        move |error| {
            // Glitches the stream survives are counted, not fatal: reopening
            // would cost a second of audio and three of them would end the channel.
            if matches!(
                error.kind(),
                ErrorKind::Xrun | ErrorKind::DeviceChanged | ErrorKind::RealtimeDenied
            ) {
                failed.count_overrun();
                return;
            }
            failed.fail(error.to_string());
        },
        None,
    )
}

fn open_device(device: &cpal::Device) -> Result<Box<dyn AudioSource>, AudioError> {
    let capture = device.supports_input();
    let supported = if capture {
        device.default_input_config()
    } else {
        device.default_output_config()
    }
    .map_err(|e| map_error(&e))?;
    let config = StreamConfig {
        channels: supported.channels(),
        sample_rate: supported.sample_rate(),
        buffer_size: BufferSize::Default,
    };
    let channels = usize::from(config.channels.max(1));
    let (producer, ring) = sample_ring(config.sample_rate, channels);
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build::<f32>(device, config, producer, |v| v),
        SampleFormat::I16 => build::<i16>(device, config, producer, |v| f32::from(v) / 32768.0),
        SampleFormat::I32 => build::<i32>(device, config, producer, |v| v as f32 / 2_147_483_648.0),
        SampleFormat::U16 => build::<u16>(device, config, producer, |v| {
            (f32::from(v) - 32768.0) / 32768.0
        }),
        other => {
            return Err(AudioError::Unavailable(format!(
                "unsupported sample format {other:?}"
            )));
        }
    }
    .map_err(|e| map_error(&e))?;
    stream.play().map_err(|e| map_error(&e))?;
    let info = StreamInfo {
        name: if capture {
            name_of(device)
        } else {
            format!("{} (loopback)", name_of(device))
        },
        device_id: device.id().ok().map(|id| id.to_string()),
        samplerate: supported.sample_rate(),
        channels: supported.channels(),
        latency_s: None,
    };
    Ok(Box::new(CpalSource {
        info,
        ring,
        _stream: stream,
    }))
}

impl AudioSource for CpalSource {
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
impl AudioBackend for CpalBackend {
    fn open_mic(&self, device: Option<&str>) -> Result<Box<dyn AudioSource>, AudioError> {
        let host = cpal::default_host();
        let device = match device {
            Some(name) => find_named(&host, name, true)?,
            None => host
                .default_input_device()
                .ok_or_else(|| AudioError::Unavailable("no default microphone".to_string()))?,
        };
        open_device(&device)
    }

    fn open_system(&self, device: Option<&str>) -> Result<Box<dyn AudioSource>, AudioError> {
        let host = cpal::default_host();
        let device = match device {
            Some(name) => find_named(&host, name, false)?,
            None => host.default_output_device().ok_or_else(|| {
                AudioError::Unavailable("no default output device to capture".to_string())
            })?,
        };
        open_device(&device)
    }

    fn versions(&self) -> serde_json::Value {
        serde_json::json!({"audio_backend": "cpal", "cpal": "0.18"})
    }

    fn devices(&self) -> Vec<DeviceInfo> {
        let host = cpal::default_host();
        let default_in = host.default_input_device().map(|d| name_of(&d));
        let default_out = host.default_output_device().map(|d| name_of(&d));
        let Ok(devices) = host.devices() else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for device in devices {
            let name = name_of(&device);
            let id = device.id().ok().map(|id| id.to_string());
            for (direction, supported, default) in [
                ("input", device.supports_input(), default_in.as_ref()),
                ("output", device.supports_output(), default_out.as_ref()),
            ] {
                if !supported {
                    continue;
                }
                let config = if direction == "input" {
                    device.default_input_config()
                } else {
                    device.default_output_config()
                }
                .ok();
                found.push(DeviceInfo {
                    name: name.clone(),
                    id: id.clone(),
                    direction,
                    default: default == Some(&name),
                    samplerate: config
                        .as_ref()
                        .map(cpal::SupportedStreamConfig::sample_rate),
                    channels: config.as_ref().map(cpal::SupportedStreamConfig::channels),
                });
            }
        }
        found
    }
}
