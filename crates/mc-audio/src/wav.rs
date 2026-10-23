//! Minimal mono s16le WAV files with a header that survives a crash.
//!
//! The header is canonical (44 bytes) and its sizes are refreshed whenever
//! [`WavWriter::flush_header`] runs, so a recording cut short by a crash or a
//! power loss loses at most the audio since the last refresh, and
//! [`repair_wav`] fixes whatever the header still claims.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

const HEADER_LEN: u64 = 44;
const MAX_DATA: u64 = u32::MAX as u64 - 36;

fn header(rate: u32, data_len: u32) -> [u8; 44] {
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data_len).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&1u16.to_le_bytes()); // mono
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&(rate * 2).to_le_bytes());
    h[32..34].copy_from_slice(&2u16.to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_len.to_le_bytes());
    h
}

pub struct WavWriter {
    file: BufWriter<File>,
    data_bytes: u64,
}

impl WavWriter {
    pub fn create(path: &Path, rate: u32) -> io::Result<Self> {
        let mut file = BufWriter::new(File::create(path)?);
        file.write_all(&header(rate, 0))?;
        Ok(Self {
            file,
            data_bytes: 0,
        })
    }

    /// Samples written so far.
    pub fn frames(&self) -> u64 {
        self.data_bytes / 2
    }

    fn reserve(&mut self, bytes: u64) -> io::Result<()> {
        if self.data_bytes + bytes > MAX_DATA {
            return Err(io::Error::other("WAV file would exceed 4 GiB"));
        }
        self.data_bytes += bytes;
        Ok(())
    }

    pub fn write_samples(&mut self, samples: &[i16]) -> io::Result<()> {
        self.reserve(samples.len() as u64 * 2)?;
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        self.file.write_all(&bytes)
    }

    pub fn write_silence(&mut self, frames: u64) -> io::Result<()> {
        const CHUNK: u64 = 48_000;
        let zeros = [0u8; CHUNK as usize * 2];
        let mut remaining = frames;
        while remaining > 0 {
            let n = remaining.min(CHUNK);
            self.reserve(n * 2)?;
            self.file.write_all(&zeros[..n as usize * 2])?;
            remaining -= n;
        }
        Ok(())
    }

    /// Flush the audio and rewrite the sizes in the header.
    pub fn flush_header(&mut self) -> io::Result<()> {
        self.file.flush()?;
        let data = self.data_bytes as u32;
        let file = self.file.get_mut();
        file.seek(SeekFrom::Start(4))?;
        file.write_all(&(36 + data).to_le_bytes())?;
        file.seek(SeekFrom::Start(40))?;
        file.write_all(&data.to_le_bytes())?;
        file.seek(SeekFrom::End(0))?;
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<()> {
        self.flush_header()?;
        self.file.get_ref().sync_all()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Repair {
    pub rate: u32,
    pub frames: u64,
    /// The header disagreed with the file and was rewritten.
    pub repaired: bool,
}

fn parse_header(head: &[u8]) -> io::Result<(u32, u32)> {
    let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_string());
    if head.len() < HEADER_LEN as usize
        || &head[0..4] != b"RIFF"
        || &head[8..12] != b"WAVE"
        || &head[12..16] != b"fmt "
        || &head[36..40] != b"data"
    {
        return Err(bad("not a canonical WAV file"));
    }
    let word = |at: usize| u32::from_le_bytes(head[at..at + 4].try_into().expect("4 bytes"));
    let format = u16::from_le_bytes([head[20], head[21]]);
    let channels = u16::from_le_bytes([head[22], head[23]]);
    let bits = u16::from_le_bytes([head[34], head[35]]);
    if format != 1 || channels != 1 || bits != 16 {
        return Err(bad("expected mono 16-bit PCM"));
    }
    Ok((word(24), word(40)))
}

/// Make the header of a WAV cut short by a crash match the bytes on disk.
pub fn repair_wav(path: &Path) -> io::Result<Repair> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let mut head = [0u8; 44];
    let read = file.read(&mut head)?;
    let (rate, declared) = parse_header(&head[..read])?;
    let on_disk = file.metadata()?.len().saturating_sub(HEADER_LEN);
    let data = (on_disk & !1).min(MAX_DATA) as u32;
    let repaired = declared != data;
    if repaired {
        file.seek(SeekFrom::Start(4))?;
        file.write_all(&(36 + data).to_le_bytes())?;
        file.seek(SeekFrom::Start(40))?;
        file.write_all(&data.to_le_bytes())?;
        file.sync_all()?;
    }
    Ok(Repair {
        rate,
        frames: u64::from(data) / 2,
        repaired,
    })
}

/// Read a mono s16 WAV: sample rate and samples. A header that claims more
/// (or nothing) than the file holds is read up to the end of the file.
pub fn read_wav(path: &Path) -> io::Result<(u32, Vec<i16>)> {
    let bytes = std::fs::read(path)?;
    let (rate, declared) = parse_header(&bytes)?;
    let samples = pcm_samples(&bytes, declared).collect();
    Ok((rate, samples))
}

/// Like [`read_wav`] but straight to floats in [-1, 1), without a second copy.
pub fn read_wav_f32(path: &Path) -> io::Result<(u32, Vec<f32>)> {
    let bytes = std::fs::read(path)?;
    let (rate, declared) = parse_header(&bytes)?;
    let samples = pcm_samples(&bytes, declared)
        .map(|s| f32::from(s) / 32768.0)
        .collect();
    Ok((rate, samples))
}

/// The data chunk's samples; a zero or overlong size (a cut file) means "all there is".
fn pcm_samples(bytes: &[u8], declared: u32) -> impl Iterator<Item = i16> + '_ {
    let available = bytes.len() as u64 - HEADER_LEN;
    let len = if declared == 0 || u64::from(declared) > available {
        available
    } else {
        u64::from(declared)
    } & !1;
    bytes[HEADER_LEN as usize..HEADER_LEN as usize + len as usize]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| i16::from_le_bytes(pair))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_header_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        let mut w = WavWriter::create(&path, 16_000).unwrap();
        w.write_samples(&[1, -2, 3]).unwrap();
        w.write_silence(2).unwrap();
        assert_eq!(w.frames(), 5);
        w.finish().unwrap();
        let (rate, samples) = read_wav(&path).unwrap();
        assert_eq!((rate, samples), (16_000, vec![1, -2, 3, 0, 0]));
        assert_eq!(
            repair_wav(&path).unwrap(),
            Repair {
                rate: 16_000,
                frames: 5,
                repaired: false
            }
        );
    }

    #[test]
    fn cut_short_file_is_repaired_and_odd_tail_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.wav");
        let mut w = WavWriter::create(&path, 8_000).unwrap();
        w.write_samples(&[10, 20]).unwrap();
        w.flush_header().unwrap();
        // More audio after the last refresh, then the process dies mid-sample.
        w.write_samples(&[30, 40]).unwrap();
        w.file.flush().unwrap();
        std::mem::forget(w);
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&[0x7f]).unwrap();
        drop(f);
        // The stale header only claims the first two samples.
        assert_eq!(read_wav(&path).unwrap().1, vec![10, 20]);
        let fixed = repair_wav(&path).unwrap();
        assert_eq!((fixed.frames, fixed.repaired), (4, true));
        assert_eq!(read_wav(&path).unwrap().1, vec![10, 20, 30, 40]);
        assert!(!repair_wav(&path).unwrap().repaired);
    }

    #[test]
    fn rejects_other_formats() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.wav");
        std::fs::write(&path, b"not a wav at all, definitely not a wav file....").unwrap();
        assert!(repair_wav(&path).is_err());
        assert!(read_wav(&path).is_err());
    }
}
