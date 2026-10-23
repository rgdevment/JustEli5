//! Transcribing a session folder with a scripted engine.

use std::path::Path;

use mc_asr::{
    Dropped, FakeAsr, RawSegment, Recognized, TranscribeCtx, TranscribeOptions, Transcriber,
    read_asr_jsonl, transcribe_session,
};
use mc_audio::wav::WavWriter;

fn speech(ms: usize) -> Vec<i16> {
    (0..ms * 16)
        .map(|n| (0.3 * (n as f32 * 0.25).sin() * 32767.0) as i16)
        .collect()
}

fn silence(ms: usize) -> Vec<i16> {
    vec![0; ms * 16]
}

fn write(path: &Path, rate: u32, samples: &[i16]) {
    let mut w = WavWriter::create(path, rate).unwrap();
    w.write_samples(samples).unwrap();
    w.finish().unwrap();
}

/// mic: 0.5 s silence, 1 s speech, 1.5 s silence, 0.8 s speech, 0.5 s silence
fn session() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mic: Vec<i16> = [
        silence(500),
        speech(1000),
        silence(1500),
        speech(800),
        silence(500),
    ]
    .concat();
    write(&dir.path().join("mic.wav"), 16_000, &mic);
    let system: Vec<i16> = [silence(3000), speech(900), silence(300)].concat();
    write(&dir.path().join("system.wav"), 16_000, &system);
    dir
}

#[test]
fn utterances_become_segments_on_the_session_clock() {
    let dir = session();
    let mut asr = FakeAsr::new(["hola a todos", "vamos a empezar", "ok, push it"]);
    let run = transcribe_session(dir.path(), &mut asr, &TranscribeOptions::default()).unwrap();
    assert!(!run.reused);
    assert!(run.missing.is_empty());
    assert_eq!(run.segments.len(), 3);
    // Mic's two phrases, then the system one, by start time.
    let texts: Vec<&str> = run.segments.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, ["hola a todos", "vamos a empezar", "ok, push it"]);
    let channels: Vec<&str> = run.segments.iter().map(|s| s.ch.as_str()).collect();
    assert_eq!(channels, ["mic", "mic", "system"]);
    let first = &run.segments[0];
    assert!((first.start_ms - 300.0).abs() <= 40.0, "{first:?}");
    assert!(
        first.end_ms > 1_400.0 && first.end_ms < 1_900.0,
        "{first:?}"
    );
    assert!(run.segments[2].start_ms >= 2_700.0);
    for pair in run.segments.windows(2) {
        assert!(pair[0].start_ms <= pair[1].start_ms);
    }

    let on_disk = read_asr_jsonl(&run.dir.join("asr.jsonl")).unwrap();
    assert_eq!(on_disk, run.segments);
    assert_eq!(run.meta.segments, 3);
    assert_eq!(run.meta.channels[0].utterances, 2);
    assert_eq!(run.meta.channels[1].segments, 1);
    assert_eq!(run.dir, dir.path().join("asr").join(&run.asr_id));
}

#[test]
fn the_same_input_reuses_the_run_and_force_redoes_it() {
    let dir = session();
    let opts = TranscribeOptions::default();
    let mut first = FakeAsr::new(["a", "b", "c"]);
    let a = transcribe_session(dir.path(), &mut first, &opts).unwrap();
    let mut second = FakeAsr::new(["a", "b", "c"]);
    let b = transcribe_session(dir.path(), &mut second, &opts).unwrap();
    assert!(b.reused);
    assert_eq!(second.calls(), 0, "a reused run never calls the engine");
    assert_eq!(a.asr_id, b.asr_id);
    assert_eq!(a.segments, b.segments);

    let mut third = FakeAsr::new(["a", "b", "c"]);
    let forced = TranscribeOptions {
        force: true,
        ..TranscribeOptions::default()
    };
    let c = transcribe_session(dir.path(), &mut third, &forced).unwrap();
    assert!(!c.reused);
    assert_eq!(third.calls(), 3);
    assert_eq!(a.asr_id, c.asr_id);
}

#[test]
fn anything_that_changes_the_output_changes_the_id() {
    let dir = session();
    let id = |texts: &[&str], opts: &TranscribeOptions| {
        let mut asr = FakeAsr::new(texts.iter().copied());
        transcribe_session(dir.path(), &mut asr, opts)
            .unwrap()
            .asr_id
    };
    let base = id(&["a", "b", "c"], &TranscribeOptions::default());
    assert_ne!(base, id(&["a", "b", "x"], &TranscribeOptions::default()));
    let with_prompt = TranscribeOptions {
        prompt: Some("Kubernetes, push".into()),
        ..TranscribeOptions::default()
    };
    assert_ne!(base, id(&["a", "b", "c"], &with_prompt));
    let spanish = TranscribeOptions {
        language: Some("es".into()),
        ..TranscribeOptions::default()
    };
    assert_ne!(base, id(&["a", "b", "c"], &spanish));
    // Different audio: a tail of silence was appended to the mic.
    let mut longer = mc_audio::wav::read_wav(&dir.path().join("mic.wav"))
        .unwrap()
        .1;
    longer.extend(silence(200));
    write(&dir.path().join("mic.wav"), 16_000, &longer);
    assert_ne!(base, id(&["a", "b", "c"], &TranscribeOptions::default()));
}

#[test]
fn the_prompt_language_and_channel_reach_the_engine() {
    struct Spy(Vec<(String, Option<String>, Option<String>)>);
    impl Transcriber for Spy {
        fn engine(&self) -> &str {
            "spy"
        }
        fn model(&self) -> &str {
            "spy"
        }
        fn transcribe(
            &mut self,
            _audio: &[f32],
            ctx: &TranscribeCtx<'_>,
        ) -> Result<Recognized, mc_asr::AsrError> {
            self.0.push((
                ctx.channel.to_string(),
                ctx.prompt.map(str::to_string),
                ctx.language.map(str::to_string),
            ));
            Ok(Recognized::default())
        }
    }
    let dir = session();
    let mut spy = Spy(Vec::new());
    let opts = TranscribeOptions {
        prompt: Some("sprint, deploy".into()),
        language: Some("es".into()),
        ..TranscribeOptions::default()
    };
    let run = transcribe_session(dir.path(), &mut spy, &opts).unwrap();
    assert!(run.segments.is_empty());
    assert_eq!(spy.0.len(), 3);
    assert!(
        spy.0
            .iter()
            .all(|(_, p, l)| p.as_deref() == Some("sprint, deploy") && l.as_deref() == Some("es"))
    );
    assert_eq!(spy.0[2].0, "system");
}

#[test]
fn a_missing_channel_is_reported_and_no_audio_at_all_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut asr = FakeAsr::numbered();
    let error = transcribe_session(dir.path(), &mut asr, &TranscribeOptions::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("no audio"), "{error}");

    write(
        &dir.path().join("mic.wav"),
        16_000,
        &[silence(300), speech(600), silence(900)].concat(),
    );
    let run = transcribe_session(dir.path(), &mut asr, &TranscribeOptions::default()).unwrap();
    assert_eq!(run.missing, ["system"]);
    assert_eq!(run.segments.len(), 1);
    assert_eq!(run.segments[0].text, "utterance 1");
}

#[test]
fn audio_at_another_rate_keeps_its_times() {
    let dir = tempfile::tempdir().unwrap();
    // 48 kHz: 1 s silence, 1 s tone, 1 s silence.
    let tone: Vec<i16> = (0..48_000)
        .map(|n| (0.3 * (n as f32 * 0.0833).sin() * 32767.0) as i16)
        .collect();
    let all: Vec<i16> = [vec![0; 48_000], tone, vec![0; 48_000]].concat();
    write(&dir.path().join("mic.wav"), 48_000, &all);
    let mut asr = FakeAsr::new(["hola"]);
    let opts = TranscribeOptions {
        channels: vec!["mic".into()],
        ..TranscribeOptions::default()
    };
    let run = transcribe_session(dir.path(), &mut asr, &opts).unwrap();
    assert_eq!(run.segments.len(), 1);
    let s = &run.segments[0];
    assert!((s.start_ms - 800.0).abs() <= 60.0, "{s:?}");
    assert!((s.end_ms - 2_200.0).abs() <= 120.0, "{s:?}");
    assert_eq!(run.meta.channels[0].samplerate, 48_000);
}

#[test]
fn engine_text_is_trimmed_and_blank_results_are_dropped() {
    let dir = tempfile::tempdir().unwrap();
    write(
        &dir.path().join("mic.wav"),
        16_000,
        &[
            silence(300),
            speech(600),
            silence(900),
            speech(600),
            silence(900),
        ]
        .concat(),
    );
    let mut asr = FakeAsr::new(["  hola  ", "   "]);
    let opts = TranscribeOptions {
        channels: vec!["mic".into()],
        ..TranscribeOptions::default()
    };
    let run = transcribe_session(dir.path(), &mut asr, &opts).unwrap();
    assert_eq!(run.segments.len(), 1);
    assert_eq!(run.segments[0].text, "hola");
    assert_eq!(run.meta.channels[0].utterances, 2);
}

#[test]
fn a_dropped_phrase_keeps_its_own_time_on_the_session_clock() {
    struct Dropping;
    impl Transcriber for Dropping {
        fn engine(&self) -> &str {
            "dropping"
        }
        fn model(&self) -> &str {
            "dropping"
        }
        fn transcribe(
            &mut self,
            _audio: &[f32],
            _ctx: &TranscribeCtx<'_>,
        ) -> Result<Recognized, mc_asr::AsrError> {
            Ok(Recognized {
                segments: Vec::new(),
                dropped: vec![Dropped {
                    segment: RawSegment::text(100.0, 400.0, "Suscríbete"),
                    reason: "stock phrase",
                }],
            })
        }
    }
    let dir = session();
    let opts = TranscribeOptions {
        channels: vec!["mic".into()],
        ..TranscribeOptions::default()
    };
    let run = transcribe_session(dir.path(), &mut Dropping, &opts).unwrap();
    let dropped = &run.meta.channels[0].dropped;
    assert_eq!(dropped.len(), 2);
    // 100-400 ms into the first utterance, which starts at 300 ms with its lead-in.
    assert!((dropped[0].start_ms - 400.0).abs() <= 60.0, "{dropped:?}");
    assert!((dropped[0].end_ms - dropped[0].start_ms - 300.0).abs() < 1.0);
    assert!(dropped[1].start_ms > 2_000.0 && dropped[0].reason == "stock phrase");
}

#[test]
fn non_finite_engine_output_is_sanitised_and_the_run_stays_readable() {
    struct Broken;
    impl Transcriber for Broken {
        fn engine(&self) -> &str {
            "broken"
        }
        fn model(&self) -> &str {
            "broken"
        }
        fn transcribe(
            &mut self,
            _audio: &[f32],
            _ctx: &TranscribeCtx<'_>,
        ) -> Result<Recognized, mc_asr::AsrError> {
            let mut seg = RawSegment::text(f64::NAN, f64::INFINITY, "hola");
            seg.avg_logprob = Some(f64::NAN);
            seg.no_speech_prob = Some(f64::INFINITY);
            seg.words = vec![
                mc_asr::AsrWord {
                    s: f64::NAN,
                    e: 1.0,
                    t: "x".into(),
                    p: None,
                },
                mc_asr::AsrWord {
                    s: 10.0,
                    e: 5.0e9,
                    t: "hola".into(),
                    p: Some(f64::NAN),
                },
            ];
            Ok(vec![seg].into())
        }
    }
    let dir = session();
    let opts = TranscribeOptions::default();
    let run = transcribe_session(dir.path(), &mut Broken, &opts).unwrap();
    for s in &run.segments {
        assert!(s.start_ms.is_finite() && s.end_ms.is_finite() && s.end_ms >= s.start_ms);
        assert_eq!((s.conf, s.no_speech), (None, None));
        assert_eq!(s.words.len(), 1);
        assert!(s.words[0].e <= s.end_ms + 1.0 && s.words[0].p.is_none());
    }
    let again = transcribe_session(dir.path(), &mut Broken, &opts).unwrap();
    assert!(again.reused, "a sanitised run reads back and is reused");
}

#[test]
fn a_run_whose_meta_disagrees_with_its_segments_is_redone() {
    let dir = session();
    let opts = TranscribeOptions::default();
    let mut asr = FakeAsr::new(["a", "b", "c"]);
    let run = transcribe_session(dir.path(), &mut asr, &opts).unwrap();
    let meta_path = run.dir.join("meta.json");
    let text = std::fs::read_to_string(&meta_path).unwrap().replace(
        "\"segments\": 3,\n  \"created",
        "\"segments\": 9,\n  \"created",
    );
    std::fs::write(&meta_path, text).unwrap();
    let mut again = FakeAsr::new(["a", "b", "c"]);
    let redone = transcribe_session(dir.path(), &mut again, &opts).unwrap();
    assert!(!redone.reused);
    assert_eq!(again.calls(), 3);
}

#[test]
fn meta_records_the_params_that_were_hashed() {
    let dir = session();
    let mut asr = FakeAsr::new(["a", "b", "c"]);
    let run = transcribe_session(dir.path(), &mut asr, &TranscribeOptions::default()).unwrap();
    // The script was full when the id was computed; the meta must say so too.
    assert_eq!(run.meta.params["script"].as_array().unwrap().len(), 3);
}
