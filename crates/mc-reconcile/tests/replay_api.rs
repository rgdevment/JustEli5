//! `Reconciler::for_replay` with `step_with` / `flush_with` must behave exactly
//! like the recording-side `Reconciler::new` driven with `step` when both are
//! given the same stamps and frame numbers.

use std::sync::Arc;

use mc_core::{Clock, RawObservation, Record, ScriptedClock, Segment, Stamp};
use mc_reconcile::{Reconciler, ReconcilerConfig, StateSnapshot, StepCtx};

const EPOCH_MS: i64 = 1_700_000_000_000;

/// One frame: the observations the source returned and the `now` the manager
/// read for it.
type Frame = (Vec<RawObservation>, f64);

fn obs(speaker: Option<&str>, text: Option<&str>) -> RawObservation {
    RawObservation {
        speaker: speaker.map(str::to_string),
        text: text.map(str::to_string),
        node_id: None,
    }
}

fn stamp_at(now_s: f64) -> Stamp {
    Stamp {
        wall_ms: EPOCH_MS + (now_s * 1000.0).round() as i64,
        mono_ms: now_s * 1000.0,
    }
}

fn flush_stamp() -> Stamp {
    Stamp {
        wall_ms: EPOCH_MS + 100_000,
        mono_ms: 100_000.0,
    }
}

#[derive(Debug, PartialEq)]
struct Outcome {
    records: Vec<Record>,
    committed: Vec<Segment>,
    snapshot: StateSnapshot,
    open: Vec<String>,
}

fn ctx(now_s: f64) -> StepCtx {
    StepCtx {
        now_s,
        read_ms: Some(5.0),
    }
}

/// The recording side: `step` asks the clock for one stamp per frame and one
/// per out-of-frame record (the flush).
fn recorded(frames: &[Frame], flush: bool) -> (Outcome, usize) {
    let stamps = frames
        .iter()
        .map(|(_, now)| stamp_at(*now))
        .chain(std::iter::repeat_n(flush_stamp(), 256));
    let clock = Arc::new(ScriptedClock::new(stamps));
    let mut reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        0.0,
        Some(clock.clone() as Arc<dyn Clock>),
        true,
    );
    let mut records = Vec::new();
    let mut committed = Vec::new();
    for (observations, now) in frames {
        let out = reconciler.step(observations, &ctx(*now));
        records.extend(out.records);
        committed.extend(out.committed);
    }
    if flush {
        let out = reconciler.flush();
        records.extend(out.records);
        committed.extend(out.committed);
    }
    let calls = clock.calls();
    (
        Outcome {
            records,
            committed,
            snapshot: reconciler.snapshot(),
            open: reconciler.open_lines(),
        },
        calls,
    )
}

/// The replay side: frame numbers and stamps are the ones the recording used.
fn replayed(frames: &[Frame], flush: bool) -> Outcome {
    let mut reconciler = Reconciler::for_replay(ReconcilerConfig::default(), 0.0);
    let mut records = Vec::new();
    let mut committed = Vec::new();
    for (index, (observations, now)) in frames.iter().enumerate() {
        let out = reconciler.step_with(observations, &ctx(*now), index as u64 + 1, stamp_at(*now));
        records.extend(out.records);
        committed.extend(out.committed);
    }
    if flush {
        let out = reconciler.flush_with(flush_stamp());
        records.extend(out.records);
        committed.extend(out.committed);
    }
    Outcome {
        records,
        committed,
        snapshot: reconciler.snapshot(),
        open: reconciler.open_lines(),
    }
}

/// Asserts the two drivers agree and that the sequence exercised real work.
fn assert_same(name: &str, frames: &[Frame], flush: bool) {
    let (rec, calls) = recorded(frames, flush);
    let rep = replayed(frames, flush);

    let flush_records = if flush {
        rec.records
            .iter()
            .filter(|r| r.frame == mc_core::FrameRef::OutOfFrame)
            .count()
    } else {
        0
    };
    assert_eq!(
        calls,
        frames.len() + flush_records,
        "{name}: recording asked for an unexpected number of stamps"
    );
    assert!(!rep.records.is_empty(), "{name}: produced no records");
    assert_eq!(rep.records, rec.records, "{name}: records differ");
    assert_eq!(rep.committed, rec.committed, "{name}: committed differ");
    assert_eq!(rep.snapshot, rec.snapshot, "{name}: snapshot differs");
    assert_eq!(rep.open, rec.open, "{name}: open lines differ");
}

#[test]
fn growing_line_commits_after_silence() {
    let frames: Vec<Frame> = vec![
        (vec![obs(Some("Ana"), Some("Hola"))], 0.0),
        (vec![obs(Some("Ana"), Some("Hola a todos"))], 0.5),
        (vec![obs(Some("Ana"), Some("Hola a todos, empecemos"))], 1.0),
        (vec![], 1.5),
        (vec![], 3.0),
    ];
    assert_same("growing_line", &frames, false);
}

#[test]
fn divergent_speakers_and_alias_raw_names() {
    let frames: Vec<Frame> = vec![
        (
            vec![
                obs(Some("Ana Pérez"), Some("Revisamos el deploy")),
                obs(Some("Luis"), Some("Yo tengo un bloqueo")),
            ],
            0.0,
        ),
        (
            vec![
                obs(Some("Ana Pérez"), Some("Revisamos el deploy")),
                obs(Some("Luis"), Some("Yo tengo un bloqueo con staging")),
            ],
            0.4,
        ),
        (
            vec![obs(Some("ana "), Some("Nada que ver con el tema anterior"))],
            0.8,
        ),
        (vec![], 5.0),
    ];
    assert_same("divergent_speakers", &frames, false);
}

#[test]
fn empty_and_unalnum_text_and_recommit() {
    let frames: Vec<Frame> = vec![
        (
            vec![
                obs(Some("Marta"), Some("")),
                obs(Some("Marta"), Some("...")),
                obs(Some("Marta"), None),
            ],
            0.0,
        ),
        (vec![obs(Some("Marta"), Some("Pruebo el micro"))], 0.2),
        (vec![], 2.0),
        (vec![obs(Some("Marta"), Some("Pruebo el micro"))], 2.5),
        (vec![], 4.5),
    ];
    assert_same("empty_and_recommit", &frames, false);
}

#[test]
fn open_lines_are_flushed_with_the_recorded_stamp() {
    let frames: Vec<Frame> = vec![
        (vec![obs(Some("Ana"), Some("Cierro la sesion"))], 0.0),
        (vec![obs(Some("Ana"), Some("Cierro la sesion ahora"))], 0.3),
        (vec![obs(Some("Luis"), Some("Gracias a todos"))], 0.6),
    ];
    assert_same("flush", &frames, true);
}

/// Opens one line at `ts` and then runs an empty frame at `now`; returns how
/// many lines committed on that frame and how many are still open.
fn boundary(init_s: f64, ts: (f64, Stamp), now: (f64, Stamp)) -> (usize, usize) {
    let mut reconciler = Reconciler::for_replay(ReconcilerConfig::default(), init_s);
    reconciler.step_with(
        &[obs(Some("Ana"), Some("Hola a todos"))],
        &ctx(ts.0),
        1,
        ts.1,
    );
    let out = reconciler.step_with(&[], &ctx(now.0), 2, now.1);
    (out.committed.len(), reconciler.open_lines().len())
}

/// Wall clock: the base is 1_700_000_005_000 ms. Its f64 difference across
/// the boundary is 1.2000000476837158, so `now = ts + 1.2` is on the commit
/// side. (1_700_000_005_002 would give 1.1999998092651367 and not commit.)
#[test]
fn debounce_boundary_commits_at_1_2_seconds_on_wall_clock() {
    let ts_ms: i64 = 1_700_000_005_000;
    let stamp = |ms: i64| Stamp {
        wall_ms: ms,
        mono_ms: (ms - EPOCH_MS) as f64,
    };
    let ts_s = ts_ms as f64 / 1000.0;
    let at_s = (ts_ms + 1200) as f64 / 1000.0;
    let under_s = (ts_ms + 1199) as f64 / 1000.0;
    assert!(at_s - ts_s >= 1.2, "fixture is not on the boundary");
    assert!(under_s - ts_s < 1.2, "fixture is not under the boundary");

    let at = boundary(0.0, (ts_s, stamp(ts_ms)), (at_s, stamp(ts_ms + 1200)));
    assert_eq!(at, (1, 0), "now = ts + 1.2 must commit");

    let under = boundary(0.0, (ts_s, stamp(ts_ms)), (under_s, stamp(ts_ms + 1199)));
    assert_eq!(under, (0, 1), "now = ts + 1.199 must not commit");
}

/// Mono clock, same boundary at 5.0 s seeded from a zero init time.
#[test]
fn debounce_boundary_commits_at_1_2_seconds_on_mono_clock() {
    let ts_ms: f64 = 5000.0;
    let stamp = |ms: f64| Stamp {
        wall_ms: EPOCH_MS + ms as i64,
        mono_ms: ms,
    };
    let ts_s = ts_ms / 1000.0;
    let at_s = (ts_ms + 1200.0) / 1000.0;
    let under_s = (ts_ms + 1199.0) / 1000.0;
    assert!(at_s - ts_s >= 1.2, "fixture is not on the boundary");
    assert!(under_s - ts_s < 1.2, "fixture is not under the boundary");

    let at = boundary(0.0, (ts_s, stamp(ts_ms)), (at_s, stamp(ts_ms + 1200.0)));
    assert_eq!(at, (1, 0), "now = ts + 1.2 must commit");

    let under = boundary(0.0, (ts_s, stamp(ts_ms)), (under_s, stamp(ts_ms + 1199.0)));
    assert_eq!(under, (0, 1), "now = ts + 1.199 must not commit");
}
