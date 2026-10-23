//! A WAV file played back as a live source, paced at real time.
//!
//! It lets `mc capture --audio-source wav:<mic.wav>,<system.wav>` and the tests
//! exercise the whole recording path without a sound card: blocks become
//! available as the wall clock reaches them, exactly like a device.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::source::{AudioBackend, AudioSource, Read, StreamInfo};
use crate::timeline::AudioError;
use crate::wav::read_wav;

pub struct WavFileSource {
    info: StreamInfo,
    samples: Vec<f32>,
    pos: usize,
    started: Instant,
}

impl WavFileSource {
    pub fn open(path: &Path) -> Result<Self, AudioError> {
        let (rate, samples) = read_wav(path)
            .map_err(|e| AudioError::Unavailable(format!("{}: {e}", path.display())))?;
        Ok(Self {
            info: StreamInfo {
                name: path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                device_id: Some(path.display().to_string()),
                samplerate: rate,
                channels: 1,
                latency_s: None,
            },
            samples: samples
                .into_iter()
                .map(|s| f32::from(s) / 32768.0)
                .collect(),
            pos: 0,
            started: Instant::now(),
        })
    }
}

impl AudioSource for WavFileSource {
    fn info(&self) -> &StreamInfo {
        &self.info
    }

    fn read(&mut self, n_frames: usize) -> Result<Read, AudioError> {
        let due = (self.started.elapsed().as_secs_f64() * f64::from(self.info.samplerate)) as usize;
        let reachable = due.min(self.samples.len());
        let want = n_frames.min(self.samples.len() - self.pos);
        if want == 0 || reachable < self.pos + want {
            std::thread::sleep(Duration::from_millis(5));
            return Ok(Read::Idle);
        }
        let block = self.samples[self.pos..self.pos + want].to_vec();
        self.pos += want;
        Ok(Read::Block(block))
    }
}

/// Plays one WAV per channel; a channel without a file is unavailable.
pub struct WavFileBackend {
    pub mic: Option<PathBuf>,
    pub system: Option<PathBuf>,
}

impl WavFileBackend {
    /// Parses `<mic.wav>[,<system.wav>]`; an empty side leaves that channel unavailable.
    pub fn parse(spec: &str) -> Result<Self, AudioError> {
        let mut parts = spec.splitn(3, ',');
        let side = |part: Option<&str>| {
            part.map(str::trim)
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
        };
        let mic = side(parts.next());
        let system = side(parts.next());
        if parts.next().is_some() || (mic.is_none() && system.is_none()) {
            return Err(AudioError::InvalidParameter(
                "expected <mic.wav>[,<system.wav>]",
            ));
        }
        Ok(Self { mic, system })
    }

    fn open(path: &Option<PathBuf>) -> Result<Box<dyn AudioSource>, AudioError> {
        match path {
            Some(p) => Ok(Box::new(WavFileSource::open(p)?)),
            None => Err(AudioError::Unavailable("no file for this channel".into())),
        }
    }
}

impl AudioBackend for WavFileBackend {
    fn open_mic(&self, _device: Option<&str>) -> Result<Box<dyn AudioSource>, AudioError> {
        Self::open(&self.mic)
    }

    fn open_system(&self, _device: Option<&str>) -> Result<Box<dyn AudioSource>, AudioError> {
        Self::open(&self.system)
    }

    fn versions(&self) -> serde_json::Value {
        serde_json::json!({"audio_backend": "wav-files"})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wav::WavWriter;

    #[test]
    fn plays_a_file_at_real_time_and_then_idles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        let mut w = WavWriter::create(&path, 8_000).unwrap();
        w.write_samples(&[16384; 800]).unwrap(); // 100 ms
        w.finish().unwrap();
        // Measured from before open: playback is paced from the moment the file opens.
        let start = Instant::now();
        let mut source = WavFileSource::open(&path).unwrap();
        assert_eq!(source.read(800).unwrap(), Read::Idle, "not due yet");
        let block = loop {
            if let Read::Block(b) = source.read(800).unwrap() {
                break b;
            }
            assert!(start.elapsed() < Duration::from_secs(2));
        };
        assert_eq!(block.len(), 800);
        assert!((block[0] - 0.5).abs() < 1e-6);
        assert!(start.elapsed() >= Duration::from_millis(90));
        assert_eq!(source.read(800).unwrap(), Read::Idle);
    }

    #[test]
    fn parses_specs() {
        let b = WavFileBackend::parse("m.wav,s.wav").unwrap();
        assert_eq!(b.mic.unwrap(), PathBuf::from("m.wav"));
        assert!(WavFileBackend::parse(",s.wav").unwrap().mic.is_none());
        assert!(WavFileBackend::parse("m.wav").unwrap().system.is_none());
        assert!(WavFileBackend::parse("").is_err());
        assert!(WavFileBackend::parse("a,b,c").is_err());
        assert!(WavFileBackend::parse(",").is_err());
    }
}
