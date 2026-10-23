//! Parity with the original Python audio timeline over `fixtures/vectors/audio_timeline.json`.

use mc_audio::{AudioError, AudioTimeline, TimelineAligner, to_mono_int16};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct MonoCase {
    channels: usize,
    samples: Vec<Value>,
    expected: Vec<i16>,
}

#[derive(Deserialize)]
struct AlignerCase {
    samplerate: u32,
    tolerance_ms: f64,
    blocks: Vec<(usize, f64)>,
    pads: Vec<u64>,
    drifts: Vec<f64>,
    frames_written: u64,
    padded_frames: u64,
}

#[derive(Deserialize)]
struct Tie {
    samplerate: u32,
    end_ms: f64,
    n: usize,
    pad: u64,
}

#[derive(Deserialize)]
struct Vectors {
    to_mono: Vec<MonoCase>,
    aligner: Vec<AlignerCase>,
    ties: Vec<Tie>,
}

fn load() -> Vectors {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/vectors/audio_timeline.json"
    );
    let text = std::fs::read_to_string(path).expect("audio_timeline.json is missing");
    serde_json::from_str(&text).expect("audio_timeline.json is not valid")
}

fn sample(value: &Value) -> f32 {
    match value {
        Value::String(s) => match s.as_str() {
            "nan" => f32::NAN,
            "inf" => f32::INFINITY,
            "-inf" => f32::NEG_INFINITY,
            other => panic!("unknown special float {other}"),
        },
        // The vector holds float32 values widened to f64: narrowing is exact.
        number => number.as_f64().expect("number") as f32,
    }
}

#[test]
fn to_mono_matches_python() {
    let vectors = load();
    assert_eq!(vectors.to_mono.len(), 400);
    for (i, case) in vectors.to_mono.iter().enumerate() {
        let samples: Vec<f32> = case.samples.iter().map(sample).collect();
        let got = to_mono_int16(&samples, case.channels).unwrap();
        assert_eq!(got, case.expected, "to_mono case {i}");
    }
}

#[test]
fn aligner_matches_python() {
    let vectors = load();
    assert_eq!(vectors.aligner.len(), 300);
    let mut padded_cases = 0;
    for (i, case) in vectors.aligner.iter().enumerate() {
        let mut aligner = TimelineAligner::new(case.samplerate, case.tolerance_ms).unwrap();
        for (b, (n, end)) in case.blocks.iter().enumerate() {
            let pad = aligner.place(*n, *end).unwrap();
            assert_eq!(pad, case.pads[b], "aligner case {i} block {b}: pad");
            assert_eq!(
                aligner.drift_ms(*end),
                case.drifts[b],
                "aligner case {i} block {b}: drift"
            );
        }
        assert_eq!(aligner.frames_written(), case.frames_written, "case {i}");
        assert_eq!(aligner.padded_frames(), case.padded_frames, "case {i}");
        padded_cases += usize::from(case.padded_frames > 0);
    }
    assert!(padded_cases > 100, "the vectors must exercise padding");
}

#[test]
fn ties_round_half_to_even_like_python() {
    for tie in load().ties {
        let mut aligner = TimelineAligner::new(tie.samplerate, 0.0).unwrap();
        assert_eq!(
            aligner.place(tie.n, tie.end_ms).unwrap(),
            tie.pad,
            "{}",
            tie.end_ms
        );
    }
}

#[test]
fn rejects_bad_parameters_and_shapes() {
    assert!(TimelineAligner::new(0, 150.0).is_err());
    assert!(TimelineAligner::new(16_000, -1.0).is_err());
    assert!(TimelineAligner::new(16_000, f64::NAN).is_err());
    assert!(TimelineAligner::new(16_000, f64::INFINITY).is_err());
    let mut aligner = TimelineAligner::with_defaults(16_000).unwrap();
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(aligner.place(100, bad).is_err());
    }
    // A rejected reading leaves the aligner untouched.
    assert_eq!(aligner.frames_written(), 0);
    assert_eq!(aligner.place(160, 10.0).unwrap(), 0);
    assert_eq!(
        to_mono_int16(&[0.0; 3], 2),
        Err(AudioError::BadShape {
            len: 3,
            channels: 2
        })
    );
    assert!(to_mono_int16(&[], 0).is_err());
    assert_eq!(to_mono_int16(&[], 2).unwrap(), Vec::<i16>::new());
}

#[test]
fn first_block_is_padded_to_its_start_and_jitter_is_not() {
    let mut aligner = TimelineAligner::with_defaults(1000).unwrap();
    // Block of 100 frames ending at 600 ms starts at 500 ms: 500 frames of silence.
    assert_eq!(aligner.place(100, 600.0).unwrap(), 500);
    // 50 ms of jitter is under the 150 ms tolerance.
    assert_eq!(aligner.place(100, 750.0).unwrap(), 0);
    // The next block ends at 1950 ms, so it starts at 1850 ms with 700 frames
    // written: 1150 frames of silence.
    assert_eq!(aligner.place(100, 1950.0).unwrap(), 1150);
    assert_eq!(aligner.frames_written(), 1950);
    assert_eq!(aligner.padded_frames(), 1650);
}

#[test]
fn timeline_maps_samples_both_ways() {
    let timeline = AudioTimeline::new(16_000).unwrap();
    assert_eq!(timeline.sample_to_mono_ms(16_000), 1000.0);
    assert_eq!(timeline.mono_ms_to_sample(1000.0), Some(16_000));
    assert_eq!(timeline.mono_ms_to_sample(-5.0), Some(0));
    assert_eq!(timeline.duration_ms(8_000), 500.0);
    assert_eq!(timeline.mono_ms_to_sample(f64::NAN), None);
    assert_eq!(timeline.mono_ms_to_sample(f64::INFINITY), None);
    assert!(AudioTimeline::new(0).is_err());
}
