//! `Policy::V2` driven through the replay API with explicit frame numbers and
//! stamps (`for_replay`, `step_with`, `flush_with`). One group per defect
//! D1..D9 from `reconciler/v2.rs`, then the invariants the code guarantees,
//! then regression tests for the rule changes of `V2_REV` 2.
//!
//! Every test here passes on the current code and none is `#[ignore]`d. Tests
//! that were written for a suspected bug are kept as regression tests; their
//! comments say which defect they guard.

use std::collections::HashSet;

use mc_core::{FrameRef, RawObservation, Record, Segment, Stamp};
use mc_reconcile::{Policy, Reconciler, ReconcilerConfig, StepCtx, StepOutcome};

const EPOCH_MS: i64 = 1_700_000_000_000;

fn obs(speaker: &str, text: &str) -> RawObservation {
    RawObservation {
        speaker: Some(speaker.to_string()),
        text: Some(text.to_string()),
        node_id: None,
    }
}

fn stamp_at(now_s: f64) -> Stamp {
    Stamp {
        wall_ms: EPOCH_MS + (now_s * 1000.0).round() as i64,
        mono_ms: now_s * 1000.0,
    }
}

/// A reconciler driven like a replay: frame numbers count from 1 and every
/// operation carries its own stamp. Everything it produces is kept in order.
struct Replay {
    reconciler: Reconciler,
    frame: u64,
    records: Vec<Record>,
    segments: Vec<Segment>,
}

impl Replay {
    fn new(policy: Policy) -> Self {
        let cfg = ReconcilerConfig {
            policy,
            ..ReconcilerConfig::default()
        };
        Self {
            reconciler: Reconciler::for_replay(cfg, 0.0),
            frame: 0,
            records: Vec::new(),
            segments: Vec::new(),
        }
    }

    fn frame(&mut self, observations: Vec<RawObservation>, now_s: f64) -> StepOutcome {
        self.frame += 1;
        let ctx = StepCtx {
            now_s,
            read_ms: Some(5.0),
        };
        let out = self
            .reconciler
            .step_with(&observations, &ctx, self.frame, stamp_at(now_s));
        self.absorb(&out);
        out
    }

    fn flush(&mut self, now_s: f64) -> StepOutcome {
        let out = self.reconciler.flush_with(stamp_at(now_s));
        self.absorb(&out);
        out
    }

    fn absorb(&mut self, out: &StepOutcome) {
        self.records.extend(out.records.iter().cloned());
        self.segments.extend(out.committed.iter().cloned());
    }

    fn texts(&self) -> Vec<&str> {
        self.segments.iter().map(|s| s.text.as_str()).collect()
    }

    fn speakers(&self) -> Vec<&str> {
        self.segments.iter().map(|s| s.speaker.as_str()).collect()
    }

    fn decisions(&self) -> Vec<String> {
        self.records
            .iter()
            .map(|r| r.decision.to_string())
            .collect()
    }

    fn count(&self, label: &str) -> usize {
        self.records
            .iter()
            .filter(|r| r.decision.to_string() == label)
            .count()
    }
}

fn labels(out: &StepOutcome) -> Vec<String> {
    out.records.iter().map(|r| r.decision.to_string()).collect()
}

/// No segment is a whole-word prefix of another.
fn no_prefix_duplicates(texts: &[&str]) {
    for (i, a) in texts.iter().enumerate() {
        let a_words: Vec<&str> = a.split_whitespace().collect();
        for (j, b) in texts.iter().enumerate() {
            let b_words: Vec<&str> = b.split_whitespace().collect();
            assert!(
                i == j || !b_words.starts_with(&a_words),
                "{a:?} is a prefix of {b:?} in {texts:?}"
            );
        }
    }
}

// ------------------------------------------------------------------ D1

/// Two open lines from one speaker in one frame; the second grows. The
/// exact match claims the first line, the grow claims the second, and no
/// newline is ever emitted.
#[test]
fn d1_two_lines_from_one_speaker_are_matched_one_to_one() {
    let mut r = Replay::new(Policy::V2);
    r.frame(
        vec![obs("Ana", "Hola"), obs("Ana", "Buenas tardes a todos")],
        0.0,
    );
    r.frame(
        vec![
            obs("Ana", "Hola"),
            obs("Ana", "Buenas tardes a todos y bienvenidos"),
        ],
        0.5,
    );
    r.frame(vec![], 3.0);
    r.flush(100.0);

    assert!(!r.decisions().iter().any(|d| d == "newline"));
    assert_eq!(r.count("new_open"), 2);
    assert_eq!(r.count("dup_exact"), 1);
    assert_eq!(r.count("grow"), 1);
    assert_eq!(
        r.texts(),
        vec!["Hola", "Buenas tardes a todos y bienvenidos"]
    );
    no_prefix_duplicates(&r.texts());
}

// ------------------------------------------------------------------ D2

/// Committed by a pause at 1.5 s, grown at 2.0 s (inside the 6 s window): the
/// held segment is reopened and amended in place, so the transcript has one
/// final segment with the grown text.
#[test]
fn d2_line_grown_inside_the_amend_window_is_amended_in_place() {
    let mut r = Replay::new(Policy::V2);
    r.frame(vec![obs("Ana", "Hola")], 0.0);
    r.frame(vec![], 1.5);
    r.frame(vec![obs("Ana", "Hola a todos")], 2.0);
    r.frame(vec![], 3.3);
    r.frame(vec![], 10.0);

    assert_eq!(r.count("committed"), 1);
    assert_eq!(r.count("amend"), 1);
    assert_eq!(r.count("amended"), 1);
    assert_eq!(r.texts(), vec!["Hola a todos"]);
    assert_eq!(r.segments[0].committed_mono_ms, Some(3300.0));
}

/// The same growth 7.5 s after the commit is outside the window: the old
/// segment stays as it was and the longer line is a second segment.
#[test]
fn d2_line_grown_after_the_amend_window_is_a_second_segment() {
    let mut r = Replay::new(Policy::V2);
    r.frame(vec![obs("Ana", "Hola")], 0.0);
    r.frame(vec![], 1.5);
    r.frame(vec![obs("Ana", "Hola a todos")], 9.0);
    r.frame(vec![], 10.5);
    r.frame(vec![], 20.0);

    assert_eq!(r.count("amend"), 0);
    assert_eq!(r.count("new_open"), 2);
    assert_eq!(r.texts(), vec!["Hola", "Hola a todos"]);
}

// ------------------------------------------------------------------ D3

/// A correction of a committed line (word-level merge, same length) inside the
/// window is amended in place; the corrected text is the final segment.
#[test]
fn d3_correction_of_a_committed_line_is_amended_in_place() {
    let mut r = Replay::new(Policy::V2);
    r.frame(
        vec![obs("Ana", "Vamos a revisar el despliegue del servidor hoy")],
        0.0,
    );
    r.frame(vec![], 1.5);
    r.frame(
        vec![obs("Ana", "Vamos a revisar el despliegue del servicio hoy")],
        2.0,
    );
    r.frame(vec![], 3.5);
    r.frame(vec![], 12.0);

    assert_eq!(r.count("amend"), 1);
    assert_eq!(r.count("amended"), 1);
    assert_eq!(r.count("committed"), 1);
    assert_eq!(
        r.texts(),
        vec!["Vamos a revisar el despliegue del servicio hoy"]
    );
}

// ------------------------------------------------------------------ D4

const WORDS: &[&str] = &[
    "casa", "perro", "rio", "montana", "tren", "fuego", "libro", "mesa", "nube", "pan", "sol",
    "luna", "mar", "viento", "piedra", "camino", "ventana", "reloj", "jardin", "puente", "silla",
    "bosque", "lago", "flor", "carta", "tiempo", "mano", "ojo", "vaso", "cielo", "campo", "ciudad",
    "nieve", "techo", "puerta", "llave", "hoja", "barco", "arena", "sombra", "trigo",
];

/// xorshift64: a deterministic source for the generated caption text.
struct Rng(u64);

impl Rng {
    fn step_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.step_u64() % n
    }
}

fn phrase(rng: &mut Rng, words: u64) -> String {
    (0..words)
        .map(|_| WORDS[rng.below(WORDS.len() as u64) as usize])
        .collect::<Vec<_>>()
        .join(" ")
}

/// Commits a long line, then 85 dissimilar lines (more than the 80 entries V1
/// remembers), then the long line again 113.5 s after it was last seen, inside
/// the 120 s repeat window. It must not be committed twice. The assertions run
/// after the flush, so a repeat that is still held back is counted too.
#[test]
fn d4_old_long_line_is_not_committed_twice_after_many_distinct_lines() {
    let old = "Revisemos el presupuesto del trimestre";
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut r = Replay::new(Policy::V2);
    r.frame(vec![obs("Ana", old)], 0.0);
    r.frame(vec![], 1.5);
    // Each new line commits at the next frame, 1.25 s later (the debounce is 1.2 s).
    for i in 0..85 {
        let now = 3.0 + 1.25 * f64::from(i);
        r.frame(vec![obs("Ana", &phrase(&mut rng, 6))], now);
    }
    r.frame(vec![obs("Ana", old)], 115.0);
    r.frame(vec![], 116.5);
    r.flush(120.0);

    // The generated lines must all be distinct lines, or the test proves nothing.
    assert_eq!(r.segments.len(), 86);
    assert_eq!(r.texts().iter().filter(|t| **t == old).count(), 1);
    assert_eq!(r.count("dup_committed_skip"), 1);
}

// ------------------------------------------------------------------ D5

/// A short line seen again 1 s after its commit is the same line.
#[test]
fn d5_short_line_still_visible_is_one_segment() {
    let mut r = Replay::new(Policy::V2);
    r.frame(vec![obs("Ana", "Sí")], 0.0);
    r.frame(vec![], 1.5);
    r.frame(vec![obs("Ana", "Sí")], 2.5);
    r.frame(vec![], 4.0);
    r.flush(100.0);

    assert_eq!(r.texts(), vec!["Sí"]);
}

/// Re-appearing 8 s after the commit (outside the amend window) is a new
/// segment.
#[test]
fn d5_short_line_reappearing_after_a_long_gap_is_a_new_segment() {
    let mut r = Replay::new(Policy::V2);
    r.frame(vec![obs("Ana", "Sí")], 0.0);
    r.frame(vec![], 1.5);
    r.frame(vec![obs("Ana", "Sí")], 9.5);
    r.frame(vec![], 11.0);
    r.frame(vec![], 30.0);

    assert_eq!(r.texts(), vec!["Sí", "Sí"]);
}

/// Regression (D5). Re-appearing 4 s after the commit is a new segment. The
/// held line is still inside the 6 s amend window, but a short line only
/// matches a held line within REAPPEAR_GAP_S (3 s) of its last sighting.
#[test]
fn d5_short_line_reappearing_after_the_reappear_gap_is_a_new_segment() {
    let mut r = Replay::new(Policy::V2);
    r.frame(vec![obs("Ana", "Sí")], 0.0);
    r.frame(vec![], 1.5);
    r.frame(vec![obs("Ana", "Sí")], 5.5);
    r.frame(vec![], 7.0);
    r.frame(vec![], 30.0);

    assert_eq!(r.texts(), vec!["Sí", "Sí"]);
}

// ------------------------------------------------------------------ D6

/// CJK text has no ASCII letters. V2 counts it as content; V1 drops it.
#[test]
fn d6_cjk_only_text_is_kept_while_v1_drops_it() {
    let mut v2 = Replay::new(Policy::V2);
    v2.frame(vec![obs("Ana", "你好世界")], 0.0);
    v2.frame(vec![], 2.0);
    v2.flush(100.0);
    assert_eq!(v2.texts(), vec!["你好世界"]);
    assert_eq!(v2.count("new_open"), 1);

    let mut v1 = Replay::new(Policy::V1Legacy);
    v1.frame(vec![obs("Ana", "你好世界")], 0.0);
    v1.frame(vec![], 2.0);
    assert!(v1.texts().is_empty(), "V1 must keep dropping it");
    assert_eq!(v1.decisions(), vec!["no_alnum"]);
}

// ------------------------------------------------------------------ D7

/// "No" is a whole-word prefix of nothing in "Nosotros vamos", so the second
/// caption is a second line. V1 merges them as a grow.
#[test]
fn d7_no_and_nosotros_vamos_are_two_lines() {
    let mut r = Replay::new(Policy::V2);
    r.frame(vec![obs("Ana", "No")], 0.0);
    r.frame(vec![obs("Ana", "Nosotros vamos")], 0.5);
    r.frame(vec![], 3.0);
    r.flush(100.0);

    assert_eq!(r.count("grow"), 0);
    assert_eq!(r.count("new_open"), 2);
    assert_eq!(r.texts(), vec!["No", "Nosotros vamos"]);

    let mut v1 = Replay::new(Policy::V1Legacy);
    v1.frame(vec![obs("Ana", "No")], 0.0);
    v1.frame(vec![obs("Ana", "Nosotros vamos")], 0.5);
    v1.frame(vec![], 3.0);
    assert_eq!(v1.texts(), vec!["Nosotros vamos"], "V1 merges them");
}

/// A one-letter line does not absorb a longer line that merely starts with the
/// same letters.
#[test]
fn d7_single_letter_shared_prefix_does_not_merge() {
    let mut r = Replay::new(Policy::V2);
    r.frame(vec![obs("Ana", "A")], 0.0);
    r.frame(vec![obs("Ana", "Ahora sí")], 0.5);
    r.frame(vec![obs("Ana", "Analisis listo")], 1.0);
    r.frame(vec![], 4.0);
    r.flush(100.0);

    assert_eq!(r.count("grow"), 0);
    assert!(!r.decisions().iter().any(|d| d.starts_with("merge")));
    assert_eq!(r.texts().len(), 3);
    no_prefix_duplicates(&r.texts());
}

// ------------------------------------------------------------------ D8

/// The only commit in this frame is a duplicate skip (a long line committed
/// long ago comes back as a new open line). That is not a change.
#[test]
fn d8_frame_whose_only_commit_is_a_duplicate_skip_is_not_a_change() {
    let mut r = Replay::new(Policy::V2);
    r.frame(vec![obs("Ana", "Vamos a empezar ya")], 0.0);
    r.frame(vec![], 1.5);
    r.frame(vec![obs("Ana", "Vamos a empezar ya")], 30.0);
    let out = r.frame(vec![], 31.5);

    assert!(
        !out.changed,
        "a pure duplicate skip must not report a change"
    );
    assert_eq!(labels(&out), vec!["dup_committed_skip"]);
    assert!(out.committed.is_empty());
}

// ------------------------------------------------------------------ D9

/// The wall clock goes back after the line opened at 10 s. The line's
/// timestamp is clamped to the new clock, so it still commits after the
/// debounce measured from there.
#[test]
fn d9_clock_going_back_still_commits_after_the_debounce() {
    let mut r = Replay::new(Policy::V2);
    r.frame(vec![obs("Ana", "Hola a todos")], 10.0);
    let back = r.frame(vec![], 5.0);
    assert!(!labels(&back).iter().any(|d| d == "committed"));

    let after = r.frame(vec![], 6.3);
    assert_eq!(labels(&after), vec!["committed"]);
    assert_eq!(after.records[0].stamp, Some(stamp_at(6.3)));

    r.frame(vec![], 20.0);
    assert_eq!(r.texts(), vec!["Hola a todos"]);
}

/// Regression (D9, held lines). The clock is set back to 5.0 s while a line
/// committed at 11.3 s is still held. Its `last_seen` is clamped to the new
/// clock, so it is released at 11.5 s (6.5 s after 5.0 s), not stranded until
/// the clock passes 17.3 s.
#[test]
fn d9_held_line_is_not_stranded_by_a_clock_that_went_back() {
    let mut r = Replay::new(Policy::V2);
    r.frame(vec![obs("Ana", "Hola a todos")], 10.0);
    r.frame(vec![], 11.3);
    r.frame(vec![], 5.0);
    r.frame(vec![], 11.5);

    assert_eq!(r.texts(), vec!["Hola a todos"]);
}

// ----------------------------------------------------- one-to-one matching

/// Two identical observations in one frame: one claims the open line and the
/// other is a new line, which is then skipped as a duplicate on commit. The
/// transcript has one line.
#[test]
fn identical_observations_in_one_frame_yield_one_segment() {
    let mut r = Replay::new(Policy::V2);
    r.frame(
        vec![obs("Ana", "Hola a todos"), obs("Ana", "Hola a todos")],
        0.0,
    );
    r.frame(vec![], 1.5);
    r.flush(100.0);

    assert_eq!(r.texts(), vec!["Hola a todos"]);
    assert_eq!(r.count("dup_committed_skip"), 1);
}

/// An open line is claimed by one observation per frame: the second copy does
/// not also claim it.
#[test]
fn an_open_line_is_claimed_by_one_observation_per_frame() {
    let mut r = Replay::new(Policy::V2);
    r.frame(vec![obs("Ana", "Buenas tardes a todos")], 0.0);
    let out = r.frame(
        vec![
            obs("Ana", "Buenas tardes a todos"),
            obs("Ana", "Buenas tardes a todos"),
        ],
        0.5,
    );

    assert_eq!(labels(&out), vec!["dup_exact", "new_open"]);
}

/// A held segment that is reopened keeps its position: a later segment of
/// another speaker does not overtake it.
#[test]
fn reopened_older_line_keeps_its_place_in_the_transcript() {
    let mut r = Replay::new(Policy::V2);
    r.frame(vec![obs("Ana", "Primera linea de la reunion")], 0.0);
    r.frame(vec![], 1.5);
    r.frame(vec![obs("Luis", "Segunda linea de Luis aqui")], 2.0);
    r.frame(vec![], 3.5);
    r.frame(
        vec![obs("Ana", "Primera linea de la reunion completa")],
        4.0,
    );
    r.frame(vec![obs("Luis", "Segunda linea de Luis aqui")], 4.2);
    r.frame(vec![], 6.0);
    r.frame(vec![], 12.0);
    r.frame(vec![], 12.5);

    assert_eq!(r.speakers(), vec!["Ana", "Luis"]);
    assert_eq!(
        r.texts(),
        vec![
            "Primera linea de la reunion completa",
            "Segunda linea de Luis aqui"
        ]
    );
}

// ------------------------------------------------------------------ flush

/// Flush commits every open line (oldest first) and releases every held one.
#[test]
fn flush_releases_every_open_and_held_line() {
    let mut r = Replay::new(Policy::V2);
    r.frame(vec![obs("Ana", "Cierro la sesion")], 0.0);
    r.frame(vec![], 0.5);
    r.frame(vec![obs("Luis", "Gracias a todos")], 0.6);
    let out = r.flush(4.0);

    assert_eq!(r.texts(), vec!["Cierro la sesion", "Gracias a todos"]);
    assert_eq!(out.committed.len(), 2);
    assert!(r.reconciler.open_lines().is_empty());
    assert!(
        r.flush(5.0).committed.is_empty(),
        "a second flush has nothing"
    );
}

/// Flushed lines are out-of-frame records stamped with the flush time, as V1
/// does it.
#[test]
fn flush_records_carry_the_flush_stamp_like_v1() {
    let mut v1 = Replay::new(Policy::V1Legacy);
    v1.frame(vec![obs("Ana", "Cierro la sesion")], 0.0);
    v1.frame(vec![], 0.5);
    v1.flush(4.0);
    let flushed = v1.records.last().expect("a flushed record");
    assert_eq!(flushed.frame, FrameRef::OutOfFrame);
    assert_eq!(flushed.stamp, Some(stamp_at(4.0)));
}

/// Regression (in_frame). `step_with` sets `in_frame`, and the V2 path clears
/// it at the end of the frame. A flushed segment is therefore `OutOfFrame` with
/// the flush stamp (4000 ms), not the last frame's stamp (500 ms).
#[test]
fn v2_flush_records_carry_the_flush_stamp() {
    let mut r = Replay::new(Policy::V2);
    r.frame(vec![obs("Ana", "Cierro la sesion")], 0.0);
    r.frame(vec![], 0.5);
    r.flush(4.0);

    let flushed = r.records.last().expect("a flushed record");
    assert_eq!(flushed.frame, FrameRef::OutOfFrame);
    assert_eq!(flushed.stamp, Some(stamp_at(4.0)));
    assert_eq!(r.segments[0].committed_mono_ms, Some(4000.0));
}

/// Regression (shrinking amend). A shorter word-prefix re-emission of a
/// committed line does not replace it: the longer committed text is kept. (The
/// earlier amend path took the newest emission even when it was shorter, which
/// dropped words from committed lines in the synthetic_clock_jitter corpus.)
#[test]
fn a_truncated_reemission_does_not_shrink_a_committed_line() {
    let mut r = Replay::new(Policy::V2);
    r.frame(
        vec![obs(
            "Ana",
            "Vamos a revisar el despliegue del servidor hoy mismo",
        )],
        0.0,
    );
    r.frame(vec![], 1.5);
    r.frame(
        vec![obs("Ana", "Vamos a revisar el despliegue del servidor")],
        2.0,
    );
    r.frame(vec![], 3.5);
    r.frame(vec![], 12.0);

    assert_eq!(
        r.texts(),
        vec!["Vamos a revisar el despliegue del servidor hoy mismo"]
    );
}

// ----------------------------------------------- empty and no-alnum records

/// Empty and no-alphanumeric observations produce the same records as V1 and
/// change nothing.
#[test]
fn empty_and_unalnum_observations_are_recorded_as_v1_records() {
    let batch = vec![
        RawObservation {
            speaker: Some("Marta".to_string()),
            text: Some(String::new()),
            node_id: None,
        },
        obs("Marta", "..."),
        RawObservation {
            speaker: Some("Marta".to_string()),
            text: None,
            node_id: None,
        },
        obs("Marta", "  ¡¿  "),
    ];
    let mut v1 = Replay::new(Policy::V1Legacy);
    let mut v2 = Replay::new(Policy::V2);
    let o1 = v1.frame(batch.clone(), 0.0);
    let o2 = v2.frame(batch, 0.0);

    assert!(!o2.changed);
    assert_eq!(o1.changed, o2.changed);
    assert_eq!(o1.records, o2.records);
    assert_eq!(
        labels(&o2),
        vec!["empty_text", "no_alnum", "empty_text", "no_alnum"]
    );
    assert_eq!(
        v1.reconciler.snapshot().discarded,
        v2.reconciler.snapshot().discarded
    );
    assert!(v2.reconciler.open_lines().is_empty());
}

// --------------------------------------------------- random-frame invariants

/// A deterministic stream of frames: several speakers (including an alias and
/// an unnamed one), empty, punctuation-only and short texts, grown and shrunk
/// repeats of the previous text, and a clock that now and then goes back.
fn scripted(seed: u64, frames: usize, clock_goes_back: bool) -> Vec<(Vec<RawObservation>, f64)> {
    let mut rng = Rng(seed);
    let speakers = ["Ana", "Luis", "ana ", "Marta", ""];
    let mut now = 0.0;
    let mut previous: Option<String> = None;
    let mut out = Vec::with_capacity(frames);
    for _ in 0..frames {
        let mut observations = Vec::new();
        for _ in 0..rng.below(4) {
            let speaker = speakers[rng.below(speakers.len() as u64) as usize];
            let roll = rng.below(10);
            let fresh_words = 1 + rng.below(6);
            let fresh = phrase(&mut rng, fresh_words);
            let text: Option<String> = match roll {
                0 => None,
                1 => Some(String::new()),
                2 => Some("...".to_string()),
                3 => Some("¿¡".to_string()),
                4 | 5 => previous.as_ref().map(|p| format!("{p} {fresh}")),
                6 => previous.as_ref().map(|p| {
                    let mut words: Vec<&str> = p.split_whitespace().collect();
                    words.pop();
                    if words.is_empty() {
                        p.clone()
                    } else {
                        words.join(" ")
                    }
                }),
                7 => Some("Sí".to_string()),
                _ => Some(fresh),
            };
            if let Some(t) = &text {
                previous = Some(t.clone());
            }
            observations.push(RawObservation {
                speaker: if speaker.is_empty() {
                    None
                } else {
                    Some(speaker.to_string())
                },
                text,
                node_id: None,
            });
        }
        now += rng.below(1500) as f64 / 1000.0;
        if clock_goes_back && rng.below(50) == 0 {
            now -= 3.0;
        }
        out.push((observations, now));
    }
    out
}

fn replay_all(frames: &[(Vec<RawObservation>, f64)]) -> Replay {
    let mut r = Replay::new(Policy::V2);
    for (observations, now) in frames {
        r.frame(observations.clone(), *now);
    }
    let last = frames.last().map_or(0.0, |(_, now)| *now);
    r.flush(last + 1.0);
    r
}

/// Same input, same records and segments, same open lines before the flush.
#[test]
fn same_input_gives_the_same_records_and_segments() {
    let frames = scripted(0xDEAD_BEEF_CAFE_F00D, 300, true);
    let a = replay_all(&frames);
    let b = replay_all(&frames);

    assert!(!a.records.is_empty());
    assert_eq!(a.records, b.records);
    assert_eq!(a.segments, b.segments);
    assert_eq!(a.reconciler.snapshot(), b.reconciler.snapshot());
}

/// 2000 random frames: no panic, one observation record per observation, no
/// `newline`, every `amended` follows a `committed` of the same speaker, and
/// each segment's committed time is not before its last-seen time.
#[test]
fn two_thousand_random_frames_hold_the_invariants() {
    let frames = scripted(0x2545_F491_4F6C_DD1D, 2000, true);
    let mut r = Replay::new(Policy::V2);
    for (observations, now) in &frames {
        let expected = observations.len();
        let out = r.frame(observations.clone(), *now);
        let observation_records = out
            .records
            .iter()
            .filter(|rec| {
                !matches!(
                    rec.decision.to_string().as_str(),
                    "committed" | "dup_committed_skip" | "amended"
                )
            })
            .count();
        assert_eq!(observation_records, expected, "frame at {now}");
    }
    r.flush(frames.last().map_or(0.0, |(_, now)| *now) + 1.0);

    let mut committed_by: HashSet<String> = HashSet::new();
    for rec in &r.records {
        match rec.decision.to_string().as_str() {
            "newline" => panic!("V2 must never emit newline"),
            "committed" => {
                committed_by.insert(rec.speaker.clone());
            }
            "amended" => assert!(
                committed_by.contains(&rec.speaker),
                "amended {:?} without an earlier committed line",
                rec.speaker
            ),
            _ => {}
        }
    }
    assert!(!r.segments.is_empty());
    assert!(r.reconciler.open_lines().is_empty());
}

/// Segment stamps on a monotone clock: every segment is stamped, its span
/// starts before it ends, and it is committed no earlier than its last
/// sighting. (A backward clock step breaks the last two: spans are not
/// clamped the way open timestamps are, so they are not checked there.)
#[test]
fn segment_stamps_are_ordered_on_a_monotone_clock() {
    let frames = scripted(0x0BAD_5EED_0000_0001, 2000, false);
    let r = replay_all(&frames);
    assert!(!r.segments.is_empty());
    for seg in &r.segments {
        let committed = seg.committed_mono_ms.expect("every segment is stamped");
        let (first, last) = (seg.first_mono_ms.unwrap(), seg.last_mono_ms.unwrap());
        assert!(first <= last, "{seg:?}");
        // Spans are rounded to 1e-3 ms on release; the commit stamp is not.
        assert!(last <= committed + 0.001, "{seg:?}");
    }
}

// ------------------------------------------- regressions for V2_REV 2 rules

/// Regression (clock). A line committed and then released; the clock goes back
/// to before its commit and the line grows. The grow is a new line, never an
/// amendment of the released segment, and every word stays in the transcript.
#[test]
fn a_released_line_is_never_amended_after_the_clock_goes_back() {
    let mut r = Replay::new(Policy::V2);
    r.frame(vec![obs("Ana", "Hola a todos")], 0.0);
    r.frame(vec![], 1.5);
    r.frame(vec![], 9.0);
    assert_eq!(r.texts(), vec!["Hola a todos"]);

    r.frame(vec![obs("Ana", "Hola a todos y bienvenidos")], 1.0);
    r.frame(vec![], 2.5);
    r.flush(10.0);

    assert_eq!(r.count("amend"), 0);
    assert_eq!(r.count("grow"), 0);
    assert_eq!(r.count("new_open"), 2);
    assert_eq!(
        r.texts(),
        vec!["Hola a todos", "Hola a todos y bienvenidos"]
    );
}

/// Regression (merge). A committed line is not replaced by a later line that
/// only shares its first words: the new line is a second segment.
#[test]
fn a_held_line_is_not_merged_into_a_line_that_shares_only_its_start() {
    let mut r = Replay::new(Policy::V2);
    r.frame(vec![obs("Ana", "vamos a ver el presupuesto del año")], 0.0);
    r.frame(vec![], 1.5);
    r.frame(
        vec![obs("Ana", "vamos a ver el calendario de entregas")],
        4.5,
    );
    r.frame(vec![], 6.5);
    r.flush(100.0);

    assert!(!r.decisions().iter().any(|d| d.starts_with("merge")));
    assert_eq!(
        r.texts(),
        vec![
            "vamos a ver el presupuesto del año",
            "vamos a ver el calendario de entregas",
        ]
    );
}

/// Regression (short line). A short line that arrives after a longer line was
/// committed is its own utterance, not a repeat seen while committed.
#[test]
fn a_short_line_after_a_longer_committed_line_is_its_own_segment() {
    let mut r = Replay::new(Policy::V2);
    r.frame(vec![obs("Ana", "vale entonces lo vemos mañana")], 0.0);
    r.frame(vec![], 1.5);
    r.frame(vec![obs("Ana", "vale")], 4.0);
    r.frame(vec![], 5.5);
    r.flush(100.0);

    assert_eq!(r.count("seen_committed"), 0);
    assert_eq!(r.texts(), vec!["vale entonces lo vemos mañana", "vale"]);
}

/// Regression (short line). "de acuerdo" is committed; a longer line that
/// contains it 5 s later is a new utterance, not a grow of the committed line.
#[test]
fn a_short_committed_line_is_not_grown_into_a_later_utterance() {
    let mut r = Replay::new(Policy::V2);
    r.frame(vec![obs("Ana", "de acuerdo")], 0.0);
    r.frame(vec![], 1.5);
    r.frame(vec![obs("Ana", "no estoy de acuerdo con eso")], 6.5);
    r.frame(vec![], 8.0);
    r.flush(100.0);

    assert_eq!(r.count("amend"), 0);
    assert_eq!(r.texts(), vec!["de acuerdo", "no estoy de acuerdo con eso"]);
}

/// Regression (open line). An open line that lost its end keeps its long text;
/// the shorter re-emission is recorded as a duplicate (`dup_norm`).
#[test]
fn an_open_line_that_lost_its_end_keeps_its_text() {
    let mut r = Replay::new(Policy::V2);
    r.frame(
        vec![obs(
            "Ana",
            "Vamos a revisar el despliegue del servidor hoy mismo",
        )],
        0.0,
    );
    r.frame(
        vec![obs("Ana", "Vamos a revisar el despliegue del servidor")],
        0.5,
    );
    r.frame(vec![], 2.0);
    r.flush(100.0);

    assert_eq!(r.count("dup_norm"), 1);
    assert_eq!(
        r.texts(),
        vec!["Vamos a revisar el despliegue del servidor hoy mismo"]
    );
}

/// Regression (grow ties). Two open lines that the next text extends: the grow
/// goes to the line it covers most, not to the first one opened.
#[test]
fn a_grow_goes_to_the_open_line_it_covers_most() {
    let mut r = Replay::new(Policy::V2);
    r.frame(vec![obs("Ana", "sí"), obs("Ana", "sí claro")], 0.0);
    r.frame(vec![obs("Ana", "sí claro lo")], 0.5);
    r.frame(vec![], 2.0);
    r.flush(100.0);

    assert_eq!(r.count("grow"), 1);
    assert_eq!(r.texts(), vec!["sí", "sí claro lo"]);
}

/// Regression (long repeat window). A long line said again 200 s after its
/// last sighting is a new utterance. Said again 60 s later, it is still one.
#[test]
fn a_long_line_repeats_as_one_only_within_the_repeat_window() {
    let line = "muchas gracias a todos por venir";

    let mut far = Replay::new(Policy::V2);
    far.frame(vec![obs("Ana", line)], 0.0);
    far.frame(vec![], 1.5);
    far.frame(vec![obs("Ana", line)], 200.0);
    far.frame(vec![], 201.5);
    far.flush(300.0);
    assert_eq!(far.texts(), vec![line, line]);

    let mut near = Replay::new(Policy::V2);
    near.frame(vec![obs("Ana", line)], 0.0);
    near.frame(vec![], 1.5);
    near.frame(vec![obs("Ana", line)], 60.0);
    near.frame(vec![], 61.5);
    near.flush(300.0);
    assert_eq!(near.texts(), vec![line]);
    assert_eq!(near.count("dup_committed_skip"), 1);
}

/// Regression (open_lines). A committed line still held back is listed by
/// `open_lines()` until it is released. An amend in progress is listed once,
/// as the open line that reopened it.
#[test]
fn open_lines_lists_a_committed_line_until_it_is_released() {
    let mut r = Replay::new(Policy::V2);
    r.frame(vec![obs("Ana", "Hola a todos")], 0.0);
    r.frame(vec![], 1.5);
    assert_eq!(r.reconciler.open_lines(), vec!["[Ana]: Hola a todos"]);
    assert!(r.texts().is_empty());

    r.frame(vec![obs("Ana", "Hola a todos y bienvenidos")], 2.0);
    assert_eq!(
        r.reconciler.open_lines(),
        vec!["[Ana]: Hola a todos y bienvenidos"]
    );

    r.frame(vec![], 3.5);
    r.frame(vec![], 10.0);
    assert!(r.reconciler.open_lines().is_empty());
    assert_eq!(r.texts(), vec!["Hola a todos y bienvenidos"]);
}

/// Regression (flush time). Flush commits open lines at the last frame's time,
/// not at the end of time, so a repeat of a short line within REAPPEAR_GAP_S
/// is still a duplicate when it is flushed.
#[test]
fn flush_uses_the_last_frame_time_for_the_repeat_window() {
    let mut r = Replay::new(Policy::V2);
    r.frame(vec![obs("Ana", "Sí")], 8.0);
    r.frame(vec![], 9.5);
    // One copy matches the committed line; the second is a new open line.
    r.frame(vec![obs("Ana", "Sí"), obs("Ana", "Sí")], 10.0);
    // The committed line (still held) and the new open line.
    assert_eq!(r.reconciler.open_lines().len(), 2);

    r.flush(500.0);

    assert_eq!(r.count("dup_committed_skip"), 1);
    assert_eq!(r.texts(), vec!["Sí"]);
    assert!(r.reconciler.open_lines().is_empty());
}
