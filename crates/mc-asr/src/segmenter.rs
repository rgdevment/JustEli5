//! Cuts a stream of 16 kHz samples into utterances with a [`Vad`].
//!
//! Streaming and deterministic: feed samples as they arrive, collect finished
//! utterances (sample ranges on the same timeline as the input), and call
//! [`Segmenter::finish`] at the end.

use crate::vad::{FRAME, Vad};

#[derive(Debug, Clone, PartialEq)]
pub struct SegmenterConfig {
    /// Shorter bursts of speech are dropped (clicks, coughs).
    pub min_speech_ms: u32,
    /// Silence that ends an utterance.
    pub max_silence_ms: u32,
    /// Audio kept before the first speech frame.
    pub pre_roll_ms: u32,
    /// Audio kept after the last speech frame.
    pub post_roll_ms: u32,
    /// Longer utterances are cut (engines work on bounded windows).
    pub max_utterance_ms: u32,
}

impl Default for SegmenterConfig {
    fn default() -> Self {
        Self {
            min_speech_ms: 240,
            max_silence_ms: 600,
            pre_roll_ms: 200,
            post_roll_ms: 200,
            max_utterance_ms: 28_000,
        }
    }
}

/// A span of the input, in samples from its start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Utterance {
    pub start_sample: u64,
    pub end_sample: u64,
}

impl Utterance {
    pub fn start_ms(&self) -> f64 {
        self.start_sample as f64 * 1000.0 / f64::from(crate::ASR_RATE)
    }

    pub fn len(&self) -> usize {
        (self.end_sample - self.start_sample) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.end_sample == self.start_sample
    }
}

struct Open {
    /// Continues speech that was cut at the maximum length: never too short.
    continuation: bool,
    start_frame: u64,
    last_speech_frame: u64,
    speech_frames: u64,
}

pub struct Segmenter<V: Vad> {
    cfg: SegmenterConfig,
    vad: V,
    pending: Vec<f32>,
    frames_seen: u64,
    open: Option<Open>,
    /// Frame right after a forced cut, where the speech may go on.
    cut_at_frame: Option<u64>,
    /// End of the previous utterance (pre-roll never reaches back into it).
    last_end_sample: u64,
}

fn frames(ms: u32) -> u64 {
    // Rounded up so a requirement of N ms is never shortened.
    (u64::from(ms) * u64::from(crate::ASR_RATE)).div_ceil(1000 * FRAME as u64)
}

impl<V: Vad> Segmenter<V> {
    pub fn new(cfg: SegmenterConfig, vad: V) -> Self {
        Self {
            cfg,
            vad,
            pending: Vec::new(),
            frames_seen: 0,
            open: None,
            cut_at_frame: None,
            last_end_sample: 0,
        }
    }

    pub fn push(&mut self, samples: &[f32]) -> Vec<Utterance> {
        let mut done = Vec::new();
        let mut rest = samples;
        // Complete a frame left over from the previous call first.
        if !self.pending.is_empty() {
            let need = FRAME - self.pending.len();
            let take = need.min(rest.len());
            self.pending.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.pending.len() == FRAME {
                let frame = std::mem::take(&mut self.pending);
                self.feed(&frame, &mut done);
            }
        }
        let (frames, remainder) = rest.as_chunks::<FRAME>();
        for frame in frames {
            self.feed(frame, &mut done);
        }
        self.pending.extend_from_slice(remainder);
        done
    }

    fn feed(&mut self, frame: &[f32], done: &mut Vec<Utterance>) {
        let speech = self.vad.is_speech(frame);
        let index = self.frames_seen;
        self.frames_seen += 1;
        self.step(index, speech, done);
    }

    fn step(&mut self, index: u64, speech: bool, done: &mut Vec<Utterance>) {
        match (&mut self.open, speech) {
            (None, true) => {
                self.open = Some(Open {
                    continuation: self.cut_at_frame == Some(index),
                    start_frame: index,
                    last_speech_frame: index,
                    speech_frames: 1,
                });
            }
            (None, false) => {}
            (Some(open), true) => {
                open.last_speech_frame = index;
                open.speech_frames += 1;
            }
            (Some(open), false) => {
                if index - open.last_speech_frame >= frames(self.cfg.max_silence_ms) {
                    self.close(index + 1, done);
                }
            }
        }
        if let Some(open) = &self.open
            && index + 1 - open.start_frame >= frames(self.cfg.max_utterance_ms)
        {
            self.close(index + 1, done);
            self.cut_at_frame = Some(index + 1);
        }
    }

    /// Ends the open utterance; `now_frame` is the first frame not yet processed.
    fn close(&mut self, now_frame: u64, done: &mut Vec<Utterance>) {
        let Some(open) = self.open.take() else { return };
        if !open.continuation && open.speech_frames < frames(self.cfg.min_speech_ms) {
            return;
        }
        let rate = u64::from(crate::ASR_RATE);
        let ms = |v: u32| u64::from(v) * rate / 1000;
        let start = (open.start_frame * FRAME as u64)
            .saturating_sub(ms(self.cfg.pre_roll_ms))
            .max(self.last_end_sample);
        let end = ((open.last_speech_frame + 1) * FRAME as u64 + ms(self.cfg.post_roll_ms))
            .min(now_frame * FRAME as u64);
        if end > start {
            self.last_end_sample = end;
            done.push(Utterance {
                start_sample: start,
                end_sample: end,
            });
        }
    }

    /// First sample (from the start of the input) that a future utterance may
    /// still need; everything before it can be forgotten.
    pub fn earliest_needed_sample(&self) -> u64 {
        let pre_roll = u64::from(self.cfg.pre_roll_ms) * u64::from(crate::ASR_RATE) / 1000;
        let reach = match &self.open {
            Some(open) => open.start_frame * FRAME as u64,
            // A new utterance can start at the next frame, preceded by its roll.
            None => self.frames_seen * FRAME as u64,
        };
        reach.saturating_sub(pre_roll).max(self.last_end_sample)
    }

    /// Ends the input: whatever was being said is an utterance too.
    pub fn finish(&mut self) -> Vec<Utterance> {
        let mut done = Vec::new();
        let now = self.frames_seen;
        self.close(now, &mut done);
        done
    }
}

/// Convenience for whole files.
pub fn segment_all<V: Vad>(cfg: SegmenterConfig, vad: V, samples: &[f32]) -> Vec<Utterance> {
    let mut segmenter = Segmenter::new(cfg, vad);
    let mut all = segmenter.push(samples);
    all.extend(segmenter.finish());
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vad::EnergyVad;

    fn silence(ms: usize) -> Vec<f32> {
        vec![0.0; ms * 16]
    }

    fn speech(ms: usize) -> Vec<f32> {
        (0..ms * 16)
            .map(|n| 0.3 * (n as f32 * 0.25).sin())
            .collect()
    }

    fn cut(parts: &[&[f32]]) -> Vec<Utterance> {
        let audio: Vec<f32> = parts.iter().flat_map(|p| p.iter().copied()).collect();
        segment_all(SegmenterConfig::default(), EnergyVad::default(), &audio)
    }

    #[test]
    fn two_phrases_separated_by_a_pause_are_two_utterances() {
        let (a, b) = (speech(1000), speech(800));
        let found = cut(&[&silence(500), &a, &silence(1500), &b, &silence(500)]);
        assert_eq!(found.len(), 2, "{found:?}");
        // Pre-roll of 200 ms before the first speech at 500 ms.
        assert!(
            (found[0].start_ms() - 300.0).abs() <= 20.0,
            "{:?}",
            found[0]
        );
        assert!(found[0].len() > 16 * 1000 && found[0].len() < 16 * 1500);
        assert!(found[1].start_sample > found[0].end_sample);
    }

    #[test]
    fn a_short_pause_inside_a_phrase_does_not_split_it() {
        let found = cut(&[&speech(700), &silence(300), &speech(700), &silence(1000)]);
        assert_eq!(found.len(), 1, "{found:?}");
    }

    #[test]
    fn clicks_are_dropped_and_silence_gives_nothing() {
        assert!(cut(&[&silence(2000), &speech(60), &silence(2000)]).is_empty());
        assert!(cut(&[&silence(3000)]).is_empty());
        assert!(cut(&[]).is_empty());
    }

    #[test]
    fn speech_running_to_the_end_is_kept() {
        let found = cut(&[&silence(300), &speech(900)]);
        assert_eq!(found.len(), 1);
        assert!(found[0].end_sample <= (300 + 900) * 16);
    }

    #[test]
    fn long_speech_is_cut_at_the_maximum() {
        let cfg = SegmenterConfig {
            max_utterance_ms: 2000,
            ..SegmenterConfig::default()
        };
        let audio = speech(5000);
        let found = segment_all(cfg, EnergyVad::default(), &audio);
        assert!(found.len() >= 2, "{found:?}");
        for u in &found {
            assert!(u.len() <= 16 * 2400);
        }
        for pair in found.windows(2) {
            assert!(pair[1].start_sample >= pair[0].end_sample, "no overlap");
        }
    }

    #[test]
    fn the_tail_after_a_forced_cut_is_kept_even_if_short() {
        let cfg = SegmenterConfig {
            max_utterance_ms: 1000,
            ..SegmenterConfig::default()
        };
        // 1.1 s of speech: the cut at 1 s leaves 100 ms (< the 240 ms minimum).
        let audio: Vec<f32> = [silence(300), speech(1100), silence(1500)].concat();
        let found = segment_all(cfg, EnergyVad::default(), &audio);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[1].start_sample >= found[0].end_sample);
    }

    #[test]
    fn feeding_in_odd_chunks_gives_the_same_result() {
        let audio: Vec<f32> = [silence(400), speech(900), silence(900), speech(500)].concat();
        let whole = segment_all(SegmenterConfig::default(), EnergyVad::default(), &audio);
        let mut seg = Segmenter::new(SegmenterConfig::default(), EnergyVad::default());
        let mut parts = Vec::new();
        for chunk in audio.chunks(777) {
            parts.extend(seg.push(chunk));
        }
        parts.extend(seg.finish());
        assert_eq!(whole, parts);
    }
}
