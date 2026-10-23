//! Parity harness: replays every non-excluded scenario of
//! `fixtures/vectors/{characterized,fuzz}.json` (exported from the Python
//! `CaptureManager`) against the Rust reconciler and
//! compares, after every operation, the records, the committed lines and the
//! reconciliation state. Failures are collected, not aborted on, so the report
//! shows how far the port diverges.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;

use mc_core::{Clock, FrameRef, RawObservation, Record, ScriptedClock, Segment, Stamp};
use mc_reconcile::{Reconciler, ReconcilerConfig, StateSnapshot, StepCtx, StepOutcome};
use serde_json::Value;

/// `read_ms` used when a scenario's first record does not carry one.
const DEFAULT_READ_MS: f64 = 500.0;
/// How many distinct failures are printed in full.
const SHOWN_FAILURES: usize = 10;

// -- vector access -------------------------------------------------------

fn load(name: &str) -> Vec<Value> {
    let path = format!(
        "{}/../../fixtures/vectors/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("no se pudo leer {path}: {e}"));
    match serde_json::from_str::<Value>(&text)
        .unwrap_or_else(|e| panic!("{path} no es JSON valido: {e}"))
    {
        Value::Array(items) => items,
        other => panic!("{path}: se esperaba una lista, hay {other}"),
    }
}

fn f(v: &Value) -> f64 {
    v.as_f64()
        .unwrap_or_else(|| panic!("se esperaba un numero, hay {v}"))
}

fn s(v: &Value) -> String {
    v.as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("se esperaba un string, hay {v}"))
}

fn opt_str(v: &Value) -> Option<String> {
    if v.is_null() { None } else { Some(s(v)) }
}

fn stamp_of(v: &Value) -> Option<Stamp> {
    if v.is_null() {
        return None;
    }
    let wall_ms = v["wall_ms"]
        .as_i64()
        .unwrap_or_else(|| panic!("wall_ms no es entero: {v}"));
    Some(Stamp {
        wall_ms,
        mono_ms: f(&v["mono_ms"]),
    })
}

/// Absent `frame` -> NoClock, `null` -> OutOfFrame, integer `n` -> InFrame(n).
fn frame_of(v: Option<&Value>) -> FrameRef {
    match v {
        None => FrameRef::NoClock,
        Some(v) if v.is_null() => FrameRef::OutOfFrame,
        Some(v) => FrameRef::InFrame(
            v.as_u64()
                .unwrap_or_else(|| panic!("frame no es entero: {v}")),
        ),
    }
}

fn span_of(v: Option<&Value>) -> Option<(f64, f64)> {
    v.map(|e| (f(&e["first_mono_ms"]), f(&e["last_mono_ms"])))
}

fn raw_obs(speaker: &Value, text: &Value) -> RawObservation {
    RawObservation {
        speaker: opt_str(speaker),
        text: opt_str(text),
        node_id: None,
    }
}

/// `all`: value is a list of `[speaker, text]`. `single`: value is one pair,
/// observed only when its text is truthy (not null and not empty).
fn observations(obs: &Value) -> Vec<RawObservation> {
    let value = &obs["value"];
    match obs["mode"].as_str() {
        Some("all") => value
            .as_array()
            .unwrap_or_else(|| panic!("modo all sin lista: {value}"))
            .iter()
            .map(|pair| raw_obs(&pair[0], &pair[1]))
            .collect(),
        Some("single") => {
            let text_truthy = value[1].as_str().is_some_and(|t| !t.is_empty());
            if text_truthy {
                vec![raw_obs(&value[0], &value[1])]
            } else {
                Vec::new()
            }
        }
        other => panic!("modo de observacion desconocido: {other:?}"),
    }
}

/// `read_ms` of the first record of the operation (the source read that
/// produced it), or the default when the operation has no such record.
fn read_ms_for(op: &Value) -> Option<f64> {
    match op["records"]
        .get(0)
        .and_then(|r| r["kwargs"].get("read_ms"))
    {
        Some(v) if v.is_null() => None,
        Some(v) => Some(f(v)),
        None => Some(DEFAULT_READ_MS),
    }
}

// -- comparison ----------------------------------------------------------

#[derive(Debug, Clone)]
struct Failure {
    origin: String,
    scenario: usize,
    op: usize,
    op_name: String,
    field: String,
    expected: String,
    got: String,
}

/// Collects the differences of one scenario, tagged with where they happened.
struct Sink {
    origin: String,
    scenario: usize,
    op: usize,
    op_name: String,
    failures: Vec<Failure>,
}

impl Sink {
    fn check<T: PartialEq + Debug>(&mut self, field: impl Into<String>, expected: T, got: T) {
        if expected != got {
            self.push(field.into(), format!("{expected:?}"), format!("{got:?}"));
        }
    }

    fn push(&mut self, field: String, expected: String, got: String) {
        self.failures.push(Failure {
            origin: self.origin.clone(),
            scenario: self.scenario,
            op: self.op,
            op_name: self.op_name.clone(),
            field,
            expected,
            got,
        });
    }
}

fn compare_records(sink: &mut Sink, expected: &[Value], got: &[Record]) {
    sink.check("records.len", expected.len(), got.len());
    for (i, (e, g)) in expected.iter().zip(got).enumerate() {
        let kw = &e["kwargs"];
        // Strict: `null` <-> None, string <-> Some(string); a missing key is an error.
        let text = opt_str(
            e.get("text")
                .unwrap_or_else(|| panic!("record sin clave text: {e}")),
        );
        sink.check(
            format!("records[{i}].speaker"),
            s(&e["speaker"]),
            g.speaker.clone(),
        );
        sink.check(format!("records[{i}].text"), text, g.text.clone());
        sink.check(
            format!("records[{i}].decision"),
            s(&e["decision"]),
            g.decision.to_string(),
        );
        sink.check(
            format!("records[{i}].speaker_raw"),
            kw.get("speaker_raw").and_then(opt_str),
            g.speaker_raw.clone(),
        );
        sink.check(
            format!("records[{i}].stamp"),
            kw.get("stamp").and_then(stamp_of),
            g.stamp,
        );
        sink.check(
            format!("records[{i}].frame"),
            frame_of(kw.get("frame")),
            g.frame,
        );
        sink.check(
            format!("records[{i}].read_ms"),
            kw.get("read_ms").and_then(Value::as_f64),
            g.read_ms,
        );
        sink.check(
            format!("records[{i}].span"),
            span_of(kw.get("extra")),
            g.span,
        );
    }
}

/// Checks the committed segments of one operation. With a recorder, each
/// segment must pair with one `committed` record of the same operation (in
/// order). Without a recorder there are no records: the segments only have to
/// match the committed lines in number and carry no record stamp.
fn compare_segments(
    sink: &mut Sink,
    expected: &[Value],
    segments: &[Segment],
    has_recorder: bool,
    expected_lines: usize,
) {
    if !has_recorder {
        sink.check("segments.len", expected_lines, segments.len());
        for (i, seg) in segments.iter().enumerate() {
            // The span comes from the reconciliation state (not the recorder), so
            // it may be present; only the record stamp is absent without one.
            sink.check(
                format!("segments[{i}].stamp"),
                (None::<f64>, None::<i64>),
                (seg.committed_mono_ms, seg.committed_wall_ms),
            );
        }
        return;
    }

    let committed: Vec<&Value> = expected
        .iter()
        .filter(|r| r["decision"].as_str() == Some("committed"))
        .collect();
    sink.check("segments.len", committed.len(), segments.len());
    for (i, (rec, seg)) in committed.iter().zip(segments).enumerate() {
        let kw = &rec["kwargs"];
        sink.check(
            format!("segments[{i}].speaker"),
            s(&rec["speaker"]),
            seg.speaker.clone(),
        );
        sink.check(
            format!("segments[{i}].text"),
            opt_str(&rec["text"]),
            Some(seg.text.clone()),
        );
        // The record only keeps the raw name when it adds information.
        let speaker_raw = seg
            .speaker_raw
            .clone()
            .filter(|raw| !raw.is_empty() && *raw != seg.speaker);
        sink.check(
            format!("segments[{i}].speaker_raw"),
            kw.get("speaker_raw").and_then(opt_str),
            speaker_raw,
        );
        sink.check(
            format!("segments[{i}].span"),
            span_of(kw.get("extra")).map_or((None, None), |(a, b)| (Some(a), Some(b))),
            (seg.first_mono_ms, seg.last_mono_ms),
        );
        sink.check(
            format!("segments[{i}].stamp"),
            kw.get("stamp")
                .and_then(stamp_of)
                .map_or((None, None), |st| (Some(st.mono_ms), Some(st.wall_ms))),
            (seg.committed_mono_ms, seg.committed_wall_ms),
        );
    }
}

fn compare_state(sink: &mut Sink, exp: &Value, got: &StateSnapshot) {
    let tracks: Vec<(String, String, f64)> = exp["tracks"]
        .as_array()
        .expect("state.tracks")
        .iter()
        .map(|t| (s(&t[0]), s(&t[1]["text"]), f(&t[1]["ts"])))
        .collect();
    sink.check("state.tracks", tracks, got.tracks.clone());

    let span: Vec<(String, (f64, f64))> = exp["span"]
        .as_array()
        .expect("state.span")
        .iter()
        .map(|p| (s(&p[0]), (f(&p[1][0]), f(&p[1][1]))))
        .collect();
    sink.check("state.span", span, got.span.clone());

    let track_raw: Vec<(String, Option<String>)> = exp["track_raw"]
        .as_array()
        .expect("state.track_raw")
        .iter()
        .map(|p| (s(&p[0]), opt_str(&p[1])))
        .collect();
    sink.check("state.track_raw", track_raw, got.track_raw.clone());

    sink.check(
        "state.recent_len",
        exp["recent_len"].as_u64().expect("recent_len") as usize,
        got.recent_len,
    );
    sink.check(
        "state.recent_hash",
        s(&exp["recent_hash"]),
        got.recent_hash.clone(),
    );
    sink.check(
        "state.reads",
        exp["reads"].as_u64().expect("reads"),
        got.reads,
    );
    sink.check(
        "state.discarded",
        exp["discarded"].as_u64().expect("discarded"),
        got.discarded,
    );
    sink.check(
        "state.last_caption_time",
        f(&exp["last_caption_time"]),
        got.last_caption_time,
    );
    sink.check(
        "state.last_activity_time",
        f(&exp["last_activity_time"]),
        got.last_activity_time,
    );
    sink.check(
        "state.frame",
        exp["frame"].as_u64().expect("frame"),
        got.frame,
    );
    sink.check(
        "state.speaker_raw",
        opt_str(&exp["speaker_raw"]),
        got.speaker_raw.clone(),
    );
    sink.check(
        "state.frame_stamp",
        stamp_of(&exp["frame_stamp"]),
        got.frame_stamp,
    );
}

// -- replay --------------------------------------------------------------

/// Runs one recorded operation on the reconciler and compares the outcome.
fn run_op(op: &Value, rec: &mut Reconciler, sink: &mut Sink, has_recorder: bool) {
    let name = op["op"].as_str().expect("op.op");
    let cfg = &op["cfg"];
    rec.config_mut().debounce_s = f(&cfg["debounce"]);
    rec.config_mut().merge_similarity = f(&cfg["merge_similarity"]);

    let args: &[Value] = op["args"].as_array().map_or(&[], Vec::as_slice);
    // `returned` is the bool the Python call returned, when it returns one.
    let (returned, outcome): (Option<bool>, StepOutcome) = match name {
        "update" => {
            let ctx = StepCtx {
                now_s: f(&op["time_seen"][0]),
                read_ms: read_ms_for(op),
            };
            let out = rec.step(&observations(&op["observations"]), &ctx);
            (Some(out.changed), out)
        }
        "ingest" => {
            let (changed, out) = rec.ingest(&s(&args[0]), &s(&args[1]), f(&args[2]));
            (Some(changed), out)
        }
        "commit_stale" => {
            let (changed, out) = rec.commit_stale(f(&args[0]));
            (Some(changed), out)
        }
        "flush" => (None, rec.flush()),
        "check_snapshot" => {
            let forced =
                op["kwargs"]["force_flush"] == true || args.first() == Some(&Value::Bool(true));
            if forced {
                (None, rec.flush())
            } else {
                // Python did nothing here; the vector must say so.
                sink.check(
                    "check_snapshot(no-op).records",
                    0,
                    op["records"].as_array().map_or(0, Vec::len),
                );
                sink.check(
                    "check_snapshot(no-op).lines_committed",
                    0,
                    op["lines_committed"].as_array().map_or(0, Vec::len),
                );
                (None, StepOutcome::default())
            }
        }
        other => panic!("operacion desconocida en el vector: {other}"),
    };

    if matches!(name, "update" | "ingest" | "commit_stale") {
        sink.check("returns", op["returns"].as_bool(), returned);
    }
    let exp_records: &[Value] = op["records"].as_array().map_or(&[][..], Vec::as_slice);
    compare_records(sink, exp_records, &outcome.records);
    let exp_lines: Vec<String> = op["lines_committed"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .map(s)
        .collect();
    let exp_count = exp_lines.len();
    let got_lines: Vec<String> = outcome.committed.iter().map(Segment::line).collect();
    sink.check("lines_committed", exp_lines, got_lines);
    compare_segments(
        sink,
        exp_records,
        &outcome.committed,
        has_recorder,
        exp_count,
    );
    compare_state(sink, &op["state"], &rec.snapshot());
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "panic sin mensaje".to_owned())
}

#[derive(Default)]
struct Report {
    name: &'static str,
    scenarios_run: usize,
    scenarios_passed: usize,
    ops_run: usize,
    ops_passed: usize,
    ops_by_type: BTreeMap<String, usize>,
    /// Scenarios that ran: origin -> (has_clock, has_recorder, ops).
    ran: BTreeMap<String, (bool, bool, usize)>,
    excluded: Vec<(String, String)>,
    failures: Vec<Failure>,
}

fn run_scenario(sc: &Value, rep: &mut Report) {
    let origin = s(&sc["origin"]);
    let scenario = sc["index"].as_u64().expect("index") as usize;
    let init = &sc["init"];
    let ops = sc["ops"].as_array().expect("ops");
    let has_clock = init["has_clock"].as_bool().expect("has_clock");
    let has_recorder = init["has_recorder"].as_bool().expect("has_recorder");
    rep.ran
        .insert(origin.clone(), (has_clock, has_recorder, ops.len()));

    let mut sink = Sink {
        origin: origin.clone(),
        scenario,
        op: 0,
        op_name: String::new(),
        failures: Vec::new(),
    };

    // Every stamp Python asked for, in order, replayed by the scripted clock.
    let stamps: Vec<Stamp> = ops
        .iter()
        .flat_map(|op| op["stamps"].as_array().map_or(&[][..], Vec::as_slice))
        .filter_map(stamp_of)
        .collect();
    let scripted = Arc::new(ScriptedClock::new(stamps.clone()));
    let clock: Option<Arc<dyn Clock>> = if has_clock {
        Some(scripted.clone() as Arc<dyn Clock>)
    } else {
        sink.check("init.stamps_without_clock", 0, stamps.len());
        None
    };
    let mut rec = Reconciler::new(
        ReconcilerConfig::default(),
        f(&init["time_at_init"]),
        clock,
        has_recorder,
    );

    for (i, op) in ops.iter().enumerate() {
        let name = op["op"].as_str().unwrap_or("?").to_owned();
        sink.op = i;
        sink.op_name = name.clone();
        *rep.ops_by_type.entry(name).or_default() += 1;
        let before = sink.failures.len();

        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            run_op(op, &mut rec, &mut sink, has_recorder)
        }));
        if let Err(payload) = result {
            sink.push(
                "panic".to_owned(),
                "sin panico".to_owned(),
                panic_message(&*payload),
            );
        }

        rep.ops_run += 1;
        if sink.failures.len() == before {
            rep.ops_passed += 1;
        }
    }

    sink.op = ops.len();
    sink.op_name = "(fin de escenario)".to_owned();
    sink.check("scripted_clock.remaining", 0, scripted.remaining());

    rep.scenarios_run += 1;
    if sink.failures.is_empty() {
        rep.scenarios_passed += 1;
    }
    rep.failures.append(&mut sink.failures);
}

fn run_file(name: &'static str, rep: &mut Report) {
    for sc in load(name) {
        let origin = s(&sc["origin"]);
        match sc["excluded"].as_str() {
            Some(reason) => rep
                .excluded
                .push((format!("{origin}#{}", sc["index"]), reason.to_owned())),
            None => run_scenario(&sc, rep),
        }
    }
}

fn print_report(rep: &Report) {
    println!(
        "[{}] escenarios: {} ejecutados, {} pasan | ops: {} ejecutadas, {} pasan",
        rep.name, rep.scenarios_run, rep.scenarios_passed, rep.ops_run, rep.ops_passed
    );
    println!("[{}] ops por tipo: {:?}", rep.name, rep.ops_by_type);
    println!("[{}] excluidos: {}", rep.name, rep.excluded.len());
    for (id, reason) in &rep.excluded {
        println!("[{}]   excluido {id}: {reason}", rep.name);
    }
}

#[test]
fn parity_with_python_vectors() {
    let mut chars = Report {
        name: "characterized",
        ..Report::default()
    };
    run_file("characterized", &mut chars);
    let mut fuzz = Report {
        name: "fuzz",
        ..Report::default()
    };
    run_file("fuzz", &mut fuzz);
    print_report(&chars);
    print_report(&fuzz);

    let mut problems: Vec<String> = Vec::new();
    if chars.scenarios_run < 45 {
        problems.push(format!(
            "characterized: {} escenarios < 45",
            chars.scenarios_run
        ));
    }
    if chars.ops_run < 120 {
        problems.push(format!("characterized: {} ops < 120", chars.ops_run));
    }
    if fuzz.scenarios_run < 30 {
        problems.push(format!("fuzz: {} escenarios < 30", fuzz.scenarios_run));
    }
    if fuzz.ops_run < 1100 {
        problems.push(format!("fuzz: {} ops < 1100", fuzz.ops_run));
    }
    for required in ["scripted[dup_norm_bytes]", "scripted[clock_no_recorder]"] {
        if !fuzz.ran.contains_key(required) {
            problems.push(format!(
                "fuzz: falta el escenario '{required}' (no existe o esta excluido)"
            ));
        }
    }
    match fuzz.ran.get("scripted[clock_no_recorder]") {
        Some(&(true, false, ops)) if ops > 0 => {}
        Some(other) => problems.push(format!(
            "scripted[clock_no_recorder]: (has_clock, has_recorder, ops) = {other:?}, se esperaba (true, false, >0)"
        )),
        None => {}
    }
    let mut types: BTreeMap<String, usize> = BTreeMap::new();
    for (k, v) in chars.ops_by_type.iter().chain(fuzz.ops_by_type.iter()) {
        *types.entry(k.clone()).or_default() += v;
    }
    for required in ["update", "ingest", "commit_stale", "flush"] {
        if types.get(required).copied().unwrap_or(0) == 0 {
            problems.push(format!("ningun op de tipo '{required}' en los vectores"));
        }
    }

    let failures: Vec<&Failure> = chars.failures.iter().chain(fuzz.failures.iter()).collect();
    if !failures.is_empty() {
        println!(
            "FALLOS: {} en total; primeros {SHOWN_FAILURES}:",
            failures.len()
        );
        for fail in failures.iter().take(SHOWN_FAILURES) {
            println!(
                "  {} #{} op {} ({}) campo {}: esperado {}, obtenido {}",
                fail.origin,
                fail.scenario,
                fail.op,
                fail.op_name,
                fail.field,
                fail.expected,
                fail.got
            );
        }
        problems.push(format!("{} fallos de paridad", failures.len()));
    }

    assert!(
        problems.is_empty(),
        "paridad incompleta:\n  - {}",
        problems.join("\n  - ")
    );
}
