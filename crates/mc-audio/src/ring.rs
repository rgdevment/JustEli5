//! The hand-off between a real-time audio callback and the recording thread:
//! a lock-free single-producer single-consumer ring (`rtrb`) of interleaved
//! f32 samples, allocated up front.
//!
//! The callback side never locks, waits or allocates; the reader polls at its
//! own cadence. A block is dropped (and counted) only when the ring is truly
//! full, always in whole frames so the channels stay aligned.

use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::source::Read;
use crate::timeline::AudioError;

/// Unread audio kept at most; a stalled recorder drops the newest instead of
/// growing without bound.
pub const MAX_BUFFER_SECONDS: usize = 10;
const READ_WAIT: Duration = Duration::from_millis(20);
const POLL_STEP: Duration = Duration::from_millis(2);

const OK: u8 = 0;
const FORMAT_CHANGED: u8 = 1;
const FAILED: u8 = 2;

/// Overrun counter and failure flags shared by both ends. Everything the
/// callback side touches is an atomic.
#[derive(Default)]
struct Shared {
    overruns: AtomicU64,
    state: AtomicU8,
    /// Channel count a changed format arrived with.
    got_channels: AtomicU32,
    message: Mutex<Option<String>>,
}

/// A clonable handle for the non-real-time callbacks (stream error reports).
#[derive(Clone)]
pub struct Signal(Arc<Shared>);

impl Signal {
    /// A glitch the stream survives (xrun, device switch).
    pub fn count_overrun(&self) {
        self.0.overruns.fetch_add(1, Ordering::Relaxed);
    }

    /// A failure the reader reports on its next call. Not for real-time code: it locks.
    pub fn fail(&self, message: String) {
        *self
            .0
            .message
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(message);
        self.0.state.store(FAILED, Ordering::Release);
    }
}

pub struct SampleProducer {
    ring: rtrb::Producer<f32>,
    channels: usize,
    shared: Arc<Shared>,
}

pub struct SampleConsumer {
    ring: rtrb::Consumer<f32>,
    channels: usize,
    shared: Arc<Shared>,
}

/// A ring for `samplerate` Hz audio of `channels` interleaved channels.
pub fn sample_ring(samplerate: u32, channels: usize) -> (SampleProducer, SampleConsumer) {
    let channels = channels.max(1);
    ring_of(
        MAX_BUFFER_SECONDS * samplerate as usize * channels,
        channels,
    )
}

fn ring_of(capacity: usize, channels: usize) -> (SampleProducer, SampleConsumer) {
    let (producer, consumer) = rtrb::RingBuffer::new(capacity);
    let shared = Arc::new(Shared::default());
    (
        SampleProducer {
            ring: producer,
            channels,
            shared: Arc::clone(&shared),
        },
        SampleConsumer {
            ring: consumer,
            channels,
            shared,
        },
    )
}

impl SampleProducer {
    pub fn signal(&self) -> Signal {
        Signal(Arc::clone(&self.shared))
    }

    /// Callback side: appends `count` samples (a whole number of frames) taken
    /// from `samples`. What does not fit is dropped and counted.
    pub fn push(&mut self, count: usize, samples: impl Iterator<Item = f32>) {
        let room = self.ring.slots() / self.channels * self.channels;
        let take = count.min(room);
        if take < count {
            self.shared.overruns.fetch_add(1, Ordering::Relaxed);
        }
        if take == 0 {
            return;
        }
        if let Ok(chunk) = self.ring.write_chunk_uninit(take) {
            chunk.fill_from_iter(samples);
        }
    }

    /// Callback side: the stream's format no longer matches what was opened.
    pub fn flag_format_change(&self, got_channels: usize) {
        self.shared
            .got_channels
            .store(got_channels as u32, Ordering::Relaxed);
        self.shared.state.store(FORMAT_CHANGED, Ordering::Release);
    }
}

impl SampleConsumer {
    pub fn overruns(&self) -> u64 {
        self.shared.overruns.load(Ordering::Relaxed)
    }

    fn take_failure(&self) -> Option<String> {
        match self.shared.state.swap(OK, Ordering::Acquire) {
            FORMAT_CHANGED => Some(format!(
                "the audio format changed ({} channels instead of {})",
                self.shared.got_channels.load(Ordering::Relaxed),
                self.channels
            )),
            FAILED => Some(
                self.shared
                    .message
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
                    .unwrap_or_else(|| "the audio stream failed".to_string()),
            ),
            _ => None,
        }
    }

    /// Whole frames waiting right now, at most `n_frames` of them.
    pub fn try_read(&mut self, n_frames: usize) -> Vec<f32> {
        let take = self.ring.slots().min(n_frames * self.channels) / self.channels * self.channels;
        if take == 0 {
            return Vec::new();
        }
        self.ring
            .read_chunk(take)
            .map(|chunk| chunk.into_iter().collect())
            .unwrap_or_default()
    }

    /// Up to `n_frames` whole frames, polling briefly for them.
    pub fn read(&mut self, n_frames: usize) -> Result<Read, AudioError> {
        if let Some(error) = self.take_failure() {
            return Err(AudioError::Device(error));
        }
        let want = n_frames * self.channels;
        let deadline = Instant::now() + READ_WAIT;
        while self.ring.slots() < want && Instant::now() < deadline {
            std::thread::sleep(POLL_STEP);
        }
        let block = self.try_read(n_frames);
        Ok(if block.is_empty() {
            Read::Idle
        } else {
            Read::Block(block)
        })
    }
}

/// One buffer of a Core Audio buffer list: its channel count and its samples.
pub type Buffer<'a> = (usize, &'a [f32]);

/// Pushes a buffer list that is either one interleaved buffer or one mono buffer
/// per channel (planar). A different channel count than the producer's is
/// flagged for the reader and returned as the count that arrived.
pub fn push_buffers<'a, I>(producer: &mut SampleProducer, buffers: I) -> Result<(), usize>
where
    I: ExactSizeIterator<Item = Buffer<'a>> + Clone,
{
    let channels = producer.channels;
    let count = buffers.len();
    let Some((first_channels, first)) = buffers.clone().next() else {
        return Ok(());
    };
    let planar = count > 1 && buffers.clone().all(|(c, _)| c == 1);
    let layout_channels = if count == 1 { first_channels } else { count };
    if layout_channels != channels || (count > 1 && !planar) {
        producer.flag_format_change(layout_channels);
        return Err(layout_channels);
    }
    if count == 1 {
        let whole = first.len() / channels * channels;
        producer.push(whole, first[..whole].iter().copied());
    } else {
        let frames = buffers.clone().map(|(_, p)| p.len()).min().unwrap_or(0);
        producer.push(
            frames * channels,
            (0..frames).flat_map(|i| buffers.clone().map(move |(_, p)| p[i])),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(read: Result<Read, AudioError>) -> Vec<f32> {
        match read.unwrap() {
            Read::Block(b) => b,
            Read::Idle => panic!("expected a block"),
        }
    }

    fn push(producer: &mut SampleProducer, samples: &[f32]) {
        producer.push(samples.len(), samples.iter().copied());
    }

    #[test]
    fn reads_whole_frames_in_order() {
        let (mut p, mut c) = sample_ring(100, 2);
        push(&mut p, &[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(block(c.read(1)), vec![1.0, 2.0]);
        assert_eq!(block(c.read(4)), vec![3.0, 4.0]);
        assert_eq!(c.read(4).unwrap(), Read::Idle);
    }

    #[test]
    fn a_full_ring_drops_whole_frames_and_counts() {
        let (mut p, mut c) = ring_of(5, 2);
        push(&mut p, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(c.overruns(), 1);
        assert_eq!(block(c.read(10)), vec![1.0, 2.0, 3.0, 4.0]);
        push(&mut p, &[7.0, 8.0]);
        assert_eq!(block(c.read(10)), vec![7.0, 8.0]);
        assert_eq!(c.overruns(), 1);
    }

    #[test]
    fn a_failure_is_reported_once() {
        let (p, mut c) = sample_ring(100, 1);
        p.signal().fail("boom".to_string());
        assert_eq!(c.read(1).unwrap_err().to_string(), "boom");
        assert_eq!(c.read(1).unwrap(), Read::Idle);
    }

    #[test]
    fn an_interleaved_buffer_is_pushed_as_is() {
        let (mut p, mut c) = sample_ring(100, 2);
        let data = [1.0, 10.0, 2.0, 20.0];
        push_buffers(&mut p, [(2, &data[..])].into_iter()).unwrap();
        assert_eq!(block(c.read(2)), data.to_vec());
    }

    #[test]
    fn planar_buffers_interleave_by_frame() {
        let (mut p, mut c) = sample_ring(100, 2);
        let (l, r) = ([1.0, 2.0, 3.0], [10.0, 20.0, 30.0]);
        push_buffers(&mut p, [(1, &l[..]), (1, &r[..])].into_iter()).unwrap();
        assert_eq!(block(c.read(3)), vec![1.0, 10.0, 2.0, 20.0, 3.0, 30.0]);
    }

    #[test]
    fn a_changed_channel_count_is_flagged_for_the_reader() {
        let (mut p, mut c) = sample_ring(100, 2);
        let data = [1.0, 2.0, 3.0];
        assert_eq!(push_buffers(&mut p, [(3, &data[..])].into_iter()), Err(3));
        let error = c.read(1).unwrap_err().to_string();
        assert!(error.contains("3 channels instead of 2"), "{error}");
        let planes = [(1, &data[..]), (1, &data[..]), (1, &data[..])];
        assert!(push_buffers(&mut p, planes.into_iter()).is_err());
        assert!(push_buffers(&mut p, std::iter::empty()).is_ok());
    }

    /// A callback at real-time cadence against a reader that stalls now and then.
    #[test]
    fn a_slow_reader_loses_nothing_below_capacity() {
        const RATE: u32 = 48_000;
        const BLOCK: usize = 480;
        const BLOCKS: usize = 150;
        let (mut p, mut c) = sample_ring(RATE, 2);
        let producer = std::thread::spawn(move || {
            let mut next = 0u32;
            for _ in 0..BLOCKS {
                let start = next;
                p.push(
                    BLOCK * 2,
                    (start..start + (BLOCK * 2) as u32).map(|v| v as f32),
                );
                next += (BLOCK * 2) as u32;
                std::thread::sleep(Duration::from_millis(10));
            }
            p
        });
        let mut got: Vec<f32> = Vec::new();
        let mut round = 0u32;
        while got.len() < BLOCKS * BLOCK * 2 {
            round += 1;
            // Every few reads the recorder stalls (a disk flush, a busy core).
            if round.is_multiple_of(7) {
                std::thread::sleep(Duration::from_millis(120));
            }
            if let Read::Block(b) = c.read(4_800).unwrap() {
                got.extend(b);
            }
            assert!(round < 10_000, "the reader never caught up");
        }
        let _p = producer.join().unwrap();
        assert_eq!(c.overruns(), 0);
        assert!(got.iter().enumerate().all(|(i, v)| *v == i as f32));
    }
}
