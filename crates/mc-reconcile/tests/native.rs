//! Pruebas nativas del reconciliador: lo que los vectores de Python no
//! expresan. Stamps del reloj y su orden, independencia del consumidor,
//! Unicode exotico con un generador determinista, determinismo, orden de
//! commit con empates y NaN, configuracion y etiquetas de decision.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use mc_core::config::RECENT_COMMITTED_CAPACITY as RECENT_MAX;
use mc_core::pytext::clean_text;
use mc_core::speakers::normalize_speaker;
use mc_core::{
    Clock, Decision, FakeClock, FrameRef, RawObservation, Record, ScriptedClock, Segment, Stamp,
};
use mc_reconcile::{Reconciler, ReconcilerConfig, StepCtx, StepOutcome};

/// Escenarios del generador de Unicode y pasos por escenario.
const SCENARIOS: u64 = 300;
const STEPS: usize = 30;

const SPEAKERS: &[Option<&str>] = &[
    Some("Ana"),
    Some("ana"),
    Some("ANA "),
    Some(""),
    None,
    Some("   "),
    Some("Usuario desconocido"),
    Some("unknown user"),
    Some("UNKNOWN  USER"),
    Some("Ana\u{a0}Pérez"),
    Some("Σίσυφος"),
    Some("İlker"),
    Some("Ñandú\u{200d}"),
    Some("\u{1c}Beto\u{1f}"),
    Some("Ελένη"),
    Some("\u{202e}שלום"),
    Some("👩\u{200d}💻 Dev"),
    Some("Juan\u{301}"),
    Some("Invitado sin nombre"),
    Some("Carla\u{2028}López"),
    Some("Zoë"),
];

const FRAGMENTS: &[&str] = &[
    "hola",
    "¿Qué tal?",
    "Σ",
    "ΟΔΟΣ",
    "ΟΔΟΣ.",
    "ΑΣ ΣΑΣ",
    "İstanbul",
    "i\u{307}",
    "ſtraße",
    "STRASSE",
    "שלום עולם",
    "مرحبا بالعالم",
    "\u{200f}כן",
    "👩\u{200d}💻🚀",
    "🇪🇸",
    "e\u{301}",
    "ｆｕｌｌ ｗｉｄｔｈ",
    "ＡＢＣ",
    "...",
    "!!!",
    "¡¿?!",
    "   ",
    "\u{a0}",
    "\u{2028}",
    "\u{1c}",
    "\u{1d}\u{1e}\u{1f}",
    "\u{0}",
    "a\u{0}b",
    "\u{feff}",
    "\t\n",
    "1234",
    "dos  espacios",
    "--",
    "",
    "x",
    "\u{301}\u{302}",
    "\u{2003}palabra\u{2003}",
];

const SEPARATORS: &[&str] = &[" ", "\u{a0}", "\u{1c}", "\u{2028}", "", "  ", "\t"];

/// Incrementos de tiempo entre pasos (incluye 0 y frontera exacta del debounce).
const DELTAS: [f64; 6] = [0.0, 0.3, 0.9, 1.2, 1.5, 2.5];

// -- helpers -------------------------------------------------------------

/// Generador congruencial propio: determinista y sin dependencias.
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) % n as u64) as usize
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

fn obs(speaker: Option<&str>, text: Option<&str>) -> RawObservation {
    RawObservation {
        speaker: speaker.map(str::to_owned),
        text: text.map(str::to_owned),
        node_id: None,
    }
}

fn open_obs(speaker: &str, text: &str) -> RawObservation {
    obs(Some(speaker), Some(text))
}

fn ctx(now_s: f64) -> StepCtx {
    StepCtx {
        now_s,
        read_ms: Some(12.5),
    }
}

fn stamp(i: usize) -> Stamp {
    Stamp {
        wall_ms: 1_700_000_000_000 + (i as i64) * 100,
        mono_ms: i as f64 * 100.0 + 0.25,
    }
}

/// Reloj con `n` stamps guionados, con el handle concreto para contar llamadas.
fn scripted(n: usize) -> (Arc<ScriptedClock>, Arc<dyn Clock>) {
    let script = Arc::new(ScriptedClock::new((0..n).map(stamp)));
    let dynamic: Arc<dyn Clock> = script.clone();
    (script, dynamic)
}

fn say(rec: &mut Reconciler, speaker: &str, text: &str, now: f64) -> StepOutcome {
    rec.step(&[open_obs(speaker, text)], &ctx(now))
}

fn silence(rec: &mut Reconciler, now: f64) -> StepOutcome {
    rec.step(&[], &ctx(now))
}

/// Lineas comprometidas en el outcome, como `[speaker]: text`.
fn lines(out: &StepOutcome) -> Vec<String> {
    out.committed.iter().map(Segment::line).collect()
}

/// Comprueba para un outcome: decisiones parseables con ida y vuelta, y que
/// cada `committed` tiene su `Segment` en el mismo orden.
fn check_outcome(out: &StepOutcome, tag: &str) {
    for record in &out.records {
        let label = record.decision.to_string();
        let parsed: Decision = label
            .parse()
            .unwrap_or_else(|e| panic!("{tag}: {label:?} no parsea: {e}"));
        assert_eq!(parsed.to_string(), label, "{tag}: ida y vuelta");
        assert_eq!(parsed, record.decision, "{tag}: igualdad tras parsear");
    }
    let committed: Vec<&Record> = out
        .records
        .iter()
        .filter(|r| r.decision == Decision::Committed)
        .collect();
    assert_eq!(
        committed.len(),
        out.committed.len(),
        "{tag}: committed sin segmento"
    );
    for (record, seg) in committed.iter().zip(&out.committed) {
        assert_eq!(record.speaker, seg.speaker, "{tag}: speaker del commit");
        assert_eq!(
            record.text.as_deref(),
            Some(seg.text.as_str()),
            "{tag}: texto del commit"
        );
    }
}

/// Invariantes del estado tras cada operacion.
fn check_state(rec: &Reconciler, reads: u64, prev_caption: &mut f64, tag: &str) {
    let snap = rec.snapshot();
    let keys: HashSet<&str> = snap.tracks.iter().map(|(sp, _, _)| sp.as_str()).collect();
    assert_eq!(
        keys.len(),
        snap.tracks.len(),
        "{tag}: claves de tracks repetidas"
    );
    assert!(
        snap.recent_len <= RECENT_MAX,
        "{tag}: recent_len = {}",
        snap.recent_len
    );
    assert_eq!(snap.reads, reads, "{tag}: reads");
    assert!(snap.discarded <= snap.reads, "{tag}: discarded > reads");
    assert!(
        snap.last_caption_time >= *prev_caption,
        "{tag}: last_caption_time retrocede de {prev_caption} a {}",
        snap.last_caption_time
    );
    *prev_caption = snap.last_caption_time;
    assert_eq!(
        rec.open_lines().len(),
        snap.tracks.len(),
        "{tag}: open_lines vs tracks"
    );
}

// -- generador de Unicode ------------------------------------------------

fn random_text(rng: &mut Lcg) -> String {
    let mut out = String::new();
    for _ in 0..rng.below(6) {
        out.push_str(rng.pick(FRAGMENTS));
        if rng.chance(50) {
            out.push_str(rng.pick(SEPARATORS));
        }
    }
    out
}

/// Mezcla crecimientos, correcciones de caso, repeticiones y textos nuevos,
/// para ejercitar todas las ramas de `ingest`.
fn next_text(rng: &mut Lcg, previous: Option<&String>) -> Option<String> {
    let roll = rng.below(100);
    match previous {
        Some(prev) if roll < 40 => Some(format!("{prev}{}", random_text(rng))),
        Some(prev) if roll < 55 => Some(prev.to_uppercase()),
        Some(prev) if roll < 70 => Some(prev.clone()),
        Some(prev) if roll < 80 => Some(prev.chars().skip(1).collect()),
        _ if rng.chance(5) => None,
        _ => Some(random_text(rng)),
    }
}

type Script = Vec<(Vec<RawObservation>, f64)>;

fn gen_script(seed: u64) -> Script {
    let mut rng = Lcg(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1));
    let mut last: Vec<Option<String>> = vec![None; SPEAKERS.len()];
    let mut now = 0.0_f64;
    let mut script = Script::with_capacity(STEPS);
    for _ in 0..STEPS {
        now += *rng.pick(&DELTAS);
        let mut observations = Vec::new();
        for _ in 0..rng.below(5) {
            let idx = rng.below(SPEAKERS.len());
            let text = next_text(&mut rng, last[idx].as_ref());
            last[idx].clone_from(&text);
            observations.push(RawObservation {
                speaker: SPEAKERS[idx].map(str::to_owned),
                text,
                node_id: None,
            });
        }
        script.push((observations, now));
    }
    script
}

/// Ejecuta un guion completo (pasos y flush final) comprobando invariantes.
fn run(tag: &str, script: &Script, with_clock: bool) -> Vec<StepOutcome> {
    let fake = Arc::new(FakeClock::new(1_700_000_000_000));
    let clock: Option<Arc<dyn Clock>> = if with_clock {
        let dynamic: Arc<dyn Clock> = fake.clone();
        Some(dynamic)
    } else {
        None
    };
    let mut rec = Reconciler::new(ReconcilerConfig::default(), 0.0, clock, true);
    let mut reads = 0u64;
    let mut prev_caption = rec.snapshot().last_caption_time;
    let mut outcomes = Vec::with_capacity(script.len() + 1);
    for (step, (observations, now)) in script.iter().enumerate() {
        reads += observations.len() as u64;
        fake.set_mono_ms(now * 1000.0);
        let out = rec.step(
            observations,
            &StepCtx {
                now_s: *now,
                read_ms: Some(1.0),
            },
        );
        let label = format!("{tag} paso {step}");
        check_outcome(&out, &label);
        check_state(&rec, reads, &mut prev_caption, &label);
        outcomes.push(out);
    }
    let flushed = rec.flush();
    check_outcome(&flushed, &format!("{tag} flush"));
    let snap = rec.snapshot();
    assert!(snap.tracks.is_empty(), "{tag}: flush deja tracks abiertos");
    assert!(rec.open_lines().is_empty(), "{tag}: flush deja open_lines");
    outcomes.push(flushed);
    outcomes
}

// -- (1) flush y stamps --------------------------------------------------

#[test]
fn flush_pide_un_stamp_por_linea_abierta_fuera_de_frame() {
    for n in [1usize, 2, 5, 12] {
        // 1 stamp del step 1, 1 del step 2 y N del flush.
        let (script, clock) = scripted(n + 2);
        let mut rec = Reconciler::new(ReconcilerConfig::default(), 0.0, Some(clock), true);
        let speakers: Vec<String> = (0..n).map(|i| format!("Hablante{i}")).collect();

        let first: Vec<RawObservation> = (0..n)
            .map(|i| open_obs(&speakers[i], &format!("frase numero {i}")))
            .collect();
        let out1 = rec.step(&first, &ctx(100.0));
        assert_eq!(script.calls(), 1, "n={n}: el step pide un stamp");
        assert_eq!(out1.records.len(), n, "n={n}: un new_open por hablante");
        for r in &out1.records {
            assert_eq!(r.decision, Decision::NewOpen);
            assert_eq!(r.frame, FrameRef::InFrame(1));
            assert_eq!(r.stamp, Some(stamp(0)));
        }

        // Crecer al primer hablante dentro de un segundo step (frame 2).
        let out2 = rec.step(
            &[open_obs(&speakers[0], "frase numero 0 ampliada")],
            &ctx(100.5),
        );
        assert_eq!(out2.records.len(), 1, "n={n}: un solo record en step 2");
        assert_eq!(out2.records[0].decision, Decision::Grow);
        assert_eq!(out2.records[0].frame, FrameRef::InFrame(2));
        assert_eq!(out2.records[0].stamp, Some(stamp(1)));
        assert!(out2.committed.is_empty(), "n={n}: aun no hay commit");

        let before = script.calls();
        let flushed = rec.flush();
        assert_eq!(
            script.calls() - before,
            n,
            "n={n}: flush pide un stamp por linea"
        );
        assert_eq!(script.calls(), n + 2, "n={n}: llamadas totales");
        assert_eq!(script.remaining(), 0, "n={n}: todos los stamps usados");

        assert_eq!(flushed.records.len(), n, "n={n}: un committed por linea");
        assert_eq!(flushed.committed.len(), n, "n={n}: un segmento por linea");
        for (i, r) in flushed.records.iter().enumerate() {
            assert_eq!(r.decision, Decision::Committed);
            assert_eq!(r.frame, FrameRef::OutOfFrame, "n={n}: flush fuera de frame");
            assert_eq!(r.stamp, Some(stamp(2 + i)), "n={n}: stamp {i} del guion");
        }
        for (i, seg) in flushed.committed.iter().enumerate() {
            assert_eq!(seg.committed_mono_ms, Some(stamp(2 + i).mono_ms));
            assert_eq!(seg.committed_wall_ms, Some(stamp(2 + i).wall_ms));
        }

        // Orden del flush: por ts; el primer hablante creci despues, asi que va el ultimo.
        let mut expected: Vec<String> = speakers[1..].to_vec();
        expected.push(speakers[0].clone());
        let got: Vec<String> = flushed
            .committed
            .iter()
            .map(|s| s.speaker.clone())
            .collect();
        assert_eq!(got, expected, "n={n}: orden del flush");
    }
}

#[test]
fn sin_recorder_el_flush_no_pide_stamps() {
    let (script, clock) = scripted(1 + 3);
    let mut rec = Reconciler::new(ReconcilerConfig::default(), 0.0, Some(clock), false);
    let out = rec.step(
        &[
            open_obs("A", "uno"),
            open_obs("B", "dos"),
            open_obs("C", "tres"),
        ],
        &ctx(50.0),
    );
    assert!(out.records.is_empty(), "sin recorder no hay records");
    assert_eq!(script.calls(), 1);

    let flushed = rec.flush();
    assert_eq!(
        script.calls(),
        1,
        "el flush no debe pedir stamps sin recorder"
    );
    assert_eq!(script.remaining(), 3);
    assert!(flushed.records.is_empty());
    assert_eq!(flushed.committed.len(), 3);
    assert!(
        flushed
            .committed
            .iter()
            .all(|s| s.committed_mono_ms.is_none() && s.committed_wall_ms.is_none()),
        "sin records no hay stamps de commit"
    );
}

#[test]
fn sin_clock_records_sin_stamp_y_frame_noclock() {
    let mut rec = Reconciler::new(ReconcilerConfig::default(), 0.0, None, true);
    let out = rec.step(&[open_obs("A", "uno")], &ctx(10.0));
    let flushed = rec.flush();
    assert_eq!(out.records.len(), 1);
    assert_eq!(flushed.records.len(), 1);
    for r in out.records.iter().chain(&flushed.records) {
        assert_eq!(r.stamp, None);
        assert_eq!(r.frame, FrameRef::NoClock);
    }
}

#[test]
fn commits_dentro_de_step_comparten_stamp_del_frame() {
    let (script, clock) = scripted(2);
    let mut rec = Reconciler::new(ReconcilerConfig::default(), 0.0, Some(clock), true);
    let out1 = rec.step(&[open_obs("A", "uno"), open_obs("B", "dos")], &ctx(10.0));
    assert_eq!(out1.records.len(), 2);

    // Silencio posterior: el commit ocurre dentro del frame 2.
    let out2 = silence(&mut rec, 20.0);
    assert_eq!(out2.committed.len(), 2);
    assert_eq!(out2.records.len(), 2);
    for r in &out2.records {
        assert_eq!(r.decision, Decision::Committed);
        assert_eq!(r.frame, FrameRef::InFrame(2));
        assert_eq!(r.stamp, Some(stamp(1)));
    }
    assert_eq!(script.calls(), 2);
    assert_eq!(script.remaining(), 0);
}

// -- (2) independencia del consumidor ------------------------------------

#[test]
fn descartar_outcomes_no_afecta_al_estado() {
    let steps: Vec<(Vec<RawObservation>, f64)> = vec![
        (
            vec![
                open_obs("Ana", "buenos dias a todos"),
                open_obs("Beto", "hola"),
            ],
            10.0,
        ),
        (vec![open_obs("Ana", "buenos dias a todos y todas")], 11.0),
        (
            vec![open_obs("Carla", "gracias"), obs(None, Some("   "))],
            13.0,
        ),
    ];
    let mut discarding = Reconciler::new(ReconcilerConfig::default(), 0.0, None, true);
    let mut keeping = Reconciler::new(ReconcilerConfig::default(), 0.0, None, true);
    let mut kept = Vec::new();
    for (observations, now) in &steps {
        let _ = discarding.step(observations, &ctx(*now));
        kept.push(keeping.step(observations, &ctx(*now)));
    }

    assert_eq!(
        discarding.snapshot(),
        keeping.snapshot(),
        "el estado no depende del consumidor"
    );
    let snap = discarding.snapshot();
    assert_eq!(snap.reads, 5);
    assert_eq!(snap.discarded, 1);
    assert_eq!(snap.tracks.len(), 1);
    assert_eq!(snap.tracks[0].0, "Carla");

    // Los outcomes guardados traen todo lo comprometido.
    assert_eq!(
        lines(&kept[2]),
        ["[Beto]: hola", "[Ana]: buenos dias a todos y todas"]
    );

    // El siguiente flush del consumidor que descarto funciona igual.
    let flushed = discarding.flush();
    assert_eq!(lines(&flushed), ["[Carla]: gracias"]);
}

// -- (3) Unicode exotico --------------------------------------------------

#[test]
fn unicode_exotico_300_escenarios_sin_panic() {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut steps = 0usize;
    for seed in 0..SCENARIOS {
        let script = gen_script(seed);
        steps += script.len();
        for out in run(&format!("escenario {seed}"), &script, seed % 2 == 0) {
            for r in &out.records {
                let label = r.decision.to_string();
                let kind = if label.starts_with("merge_") {
                    "merge".to_string()
                } else {
                    label
                };
                seen.insert(kind);
            }
        }
    }
    assert_eq!(steps, SCENARIOS as usize * STEPS);
    // Cobertura: las ramas exoticas deben aparecer de verdad en el generador.
    for kind in [
        "empty_text",
        "no_alnum",
        "new_open",
        "dup_exact",
        "dup_norm",
        "dup_norm_longer",
        "grow",
        "merge",
        "newline",
        "committed",
        "dup_committed_skip",
    ] {
        assert!(
            seen.contains(kind),
            "el generador nunca produjo {kind}: {seen:?}"
        );
    }
}

// -- (4) determinismo ----------------------------------------------------

#[test]
fn determinismo_mismo_guion_mismos_outcomes() {
    for seed in 0..25u64 {
        let script = gen_script(seed);
        assert_eq!(script, gen_script(seed), "el generador es determinista");
        let with_clock = seed % 2 == 0;
        let first = run("determinismo a", &script, with_clock);
        let second = run("determinismo b", &script, with_clock);
        assert_eq!(first.len(), second.len());
        for (x, y) in first.iter().zip(&second) {
            assert_eq!(x.changed, y.changed, "seed {seed}");
            assert_eq!(x.records, y.records, "seed {seed}");
            assert_eq!(x.committed, y.committed, "seed {seed}");
            assert_eq!(format!("{x:?}"), format!("{y:?}"), "seed {seed}");
        }
    }
}

// -- (5) orden de commit ---------------------------------------------------

#[test]
fn commit_stale_con_ts_iguales_sigue_orden_de_insercion() {
    let mut rec = Reconciler::new(ReconcilerConfig::default(), 0.0, None, true);
    let _ = rec.ingest("Beto", "dos", 5.0);
    let _ = rec.ingest("Ana", "uno", 5.0);
    let (_, out) = rec.commit_stale(10.0);
    assert_eq!(lines(&out), ["[Beto]: dos", "[Ana]: uno"]);
}

#[test]
fn commit_stale_con_zero_negativo_y_positivo_sigue_insercion() {
    // Insercion: Ana y Beto con ts -0.0 / 0.0 (en ambos ordenes), y Carla antes
    // con ts 1.0, que debe ir la ultima por ser la mas antigua en el orden de ts.
    for (ana_ts, beto_ts) in [(0.0_f64, -0.0_f64), (-0.0, 0.0)] {
        for ana_first in [true, false] {
            let mut rec = Reconciler::new(ReconcilerConfig::default(), 0.0, None, true);
            let _ = rec.ingest("Carla", "tres", 1.0);
            if ana_first {
                let _ = rec.ingest("Ana", "uno", ana_ts);
                let _ = rec.ingest("Beto", "dos", beto_ts);
            } else {
                let _ = rec.ingest("Beto", "dos", beto_ts);
                let _ = rec.ingest("Ana", "uno", ana_ts);
            }
            let (_, out) = rec.commit_stale(3.0);
            // Carla (ts 1.0) va la ultima; los dos ceros (-0.0 == 0.0) salen en
            // orden de insercion.
            let expected: Vec<&str> = if ana_first {
                vec!["[Ana]: uno", "[Beto]: dos", "[Carla]: tres"]
            } else {
                vec!["[Beto]: dos", "[Ana]: uno", "[Carla]: tres"]
            };
            assert_eq!(lines(&out), expected, "ana_ts={ana_ts} beto_ts={beto_ts}");
        }
    }
}

#[test]
fn ts_nan_en_commit_stale_no_provoca_panic() {
    let mut rec = Reconciler::new(ReconcilerConfig::default(), 0.0, None, true);
    let _ = rec.ingest("Ana", "uno", f64::NAN);
    let _ = rec.ingest("Beto", "dos", 1.0);
    let (changed, out) = rec.commit_stale(10.0);
    assert!(changed);
    assert_eq!(lines(&out), ["[Beto]: dos"], "la pista NaN nunca es stale");
    assert_eq!(rec.open_lines(), ["[Ana]: uno"]);
    let (changed, out) = rec.commit_stale(f64::NAN);
    assert!(!changed && out.committed.is_empty());
}

/// Prueba de regresion del hallazgo: con NaN en `ts`, `flush` y `open_lines`
/// ordenaban con `partial_cmp` + `unwrap_or(Equal)`, que no es un orden total.
/// El `sort` estable de Rust (>= 1.81) detecta la violacion y entra en panico
/// (21 pistas con NaN en varias posiciones y ts desordenados). Se corrigio con
/// `cmp_ts`; esta prueba fija que no hay panico y que el orden es el esperado.
#[test]
fn ts_nan_en_flush_y_open_lines_no_provoca_panic() {
    for n in [2usize, 3, 10, 21, 40, 200] {
        let mut rec = Reconciler::new(ReconcilerConfig::default(), 0.0, None, true);
        for i in 0..n {
            let ts = if i % 3 == 0 { f64::NAN } else { (n - i) as f64 };
            let _ = rec.ingest(&format!("H{i}"), &format!("texto {i}"), ts);
        }
        // Numeros de menor a mayor ts (= de mayor a menor indice), NaN al final
        // en orden de insercion.
        let orden: Vec<usize> = (0..n)
            .rev()
            .filter(|i| i % 3 != 0)
            .chain((0..n).filter(|i| i % 3 == 0))
            .collect();
        let esperado_open: Vec<String> =
            orden.iter().map(|i| format!("[H{i}]: texto {i}")).collect();
        assert_eq!(rec.open_lines(), esperado_open, "n={n}: open_lines");
        let esperado_flush: Vec<String> =
            orden.iter().map(|i| format!("[H{i}]: texto {i}")).collect();
        let flushed = rec.flush();
        assert_eq!(lines(&flushed), esperado_flush, "n={n}: orden del flush");
    }
}

#[test]
fn ts_nan_orden_fijado() {
    // F es 0.0 / 0.0 con signo negativo en x86 (NaN distinto de f64::NAN).
    let nan_negativo = 0.0_f64 / std::hint::black_box(0.0_f64);
    assert!(nan_negativo.is_nan());
    let mut rec = Reconciler::new(ReconcilerConfig::default(), 0.0, None, true);
    let _ = rec.ingest("A", "a", 1.0);
    let _ = rec.ingest("B", "b", -f64::NAN);
    let _ = rec.ingest("C", "c", f64::NAN);
    let _ = rec.ingest("D", "d", f64::NEG_INFINITY);
    let _ = rec.ingest("E", "e", f64::INFINITY);
    let _ = rec.ingest("F", "f", nan_negativo);
    // Numeros por ts ascendente, luego los NaN (cualquier signo) en insercion.
    assert_eq!(
        rec.open_lines(),
        ["[D]: d", "[A]: a", "[E]: e", "[B]: b", "[C]: c", "[F]: f"]
    );
    let flushed = rec.flush();
    let speakers: Vec<&str> = flushed
        .committed
        .iter()
        .map(|s| s.speaker.as_str())
        .collect();
    assert_eq!(speakers, ["D", "A", "E", "B", "C", "F"], "orden del flush");
    assert_eq!(
        lines(&flushed),
        ["[D]: d", "[A]: a", "[E]: e", "[B]: b", "[C]: c", "[F]: f"]
    );
}

// -- (6) configuracion ----------------------------------------------------

#[test]
fn recent_capacity_cero_nunca_almacena() {
    let cfg = ReconcilerConfig {
        recent_capacity: 0,
        ..ReconcilerConfig::default()
    };
    let mut rec = Reconciler::new(cfg, 0.0, None, true);
    let mut now = 0.0;
    for round in 0..5 {
        let out = say(&mut rec, "Ana", "la misma frase", now);
        let out2 = silence(&mut rec, now + 2.0);
        assert_eq!(rec.snapshot().recent_len, 0, "ronda {round}");
        for o in [&out, &out2] {
            assert!(
                o.records
                    .iter()
                    .all(|r| r.decision != Decision::DupCommittedSkip),
                "ronda {round}: nunca hay dup_committed_skip"
            );
        }
        assert_eq!(
            lines(&out2),
            ["[Ana]: la misma frase"],
            "ronda {round}: se compromete otra vez"
        );
        now += 3.0;
    }
}

#[test]
fn recent_capacity_tres_expulsa_fifo() {
    let cfg = ReconcilerConfig {
        recent_capacity: 3,
        ..ReconcilerConfig::default()
    };
    let mut rec = Reconciler::new(cfg, 0.0, None, true);
    let frases = [
        "primera frase",
        "segunda frase",
        "tercera frase",
        "cuarta frase",
    ];
    let mut now = 0.0;
    for frase in frases {
        let _ = say(&mut rec, "Ana", frase, now);
        let out = silence(&mut rec, now + 2.0);
        assert_eq!(lines(&out).len(), 1, "{frase} se compromete");
        now += 3.0;
    }
    assert_eq!(rec.snapshot().recent_len, 3);

    // Recent = [segunda, tercera, cuarta]: re-emitir la cuarta es duplicado.
    let _ = say(&mut rec, "Ana", "cuarta frase", now);
    let skip = silence(&mut rec, now + 2.0);
    assert!(skip.committed.is_empty());
    assert!(
        skip.records
            .iter()
            .any(|r| r.decision == Decision::DupCommittedSkip)
    );
    now += 3.0;

    // Re-emitir la primera (ya expulsada) la compromete otra vez y expulsa "segunda".
    let _ = say(&mut rec, "Ana", "primera frase", now);
    let again = silence(&mut rec, now + 2.0);
    assert_eq!(lines(&again), ["[Ana]: primera frase"]);
    assert_eq!(rec.snapshot().recent_len, 3);
    now += 3.0;

    // "Segunda" ya no esta en recent: se compromete de nuevo.
    let _ = say(&mut rec, "Ana", "segunda frase", now);
    let segunda = silence(&mut rec, now + 2.0);
    assert_eq!(
        lines(&segunda),
        ["[Ana]: segunda frase"],
        "la FIFO expulso la segunda"
    );
    now += 3.0;

    // "Cuarta" sigue dentro: duplicado.
    let _ = say(&mut rec, "Ana", "cuarta frase", now);
    let skip2 = silence(&mut rec, now + 2.0);
    assert!(skip2.committed.is_empty(), "la cuarta aun esta en recent");
}

#[test]
fn debounce_cambiado_con_config_mut_se_respeta_en_la_frontera() {
    let mut rec = Reconciler::new(ReconcilerConfig::default(), 0.0, None, true);
    let _ = say(&mut rec, "Ana", "uno", 10.0);

    rec.config_mut().debounce_s = 1.5;
    // Justo por debajo de la frontera (now - ts = 1.5 - ulp): no compromete.
    let just_below = f64::from_bits(11.5_f64.to_bits() - 1);
    let early = silence(&mut rec, just_below);
    assert!(
        early.committed.is_empty(),
        "por debajo de la frontera no compromete"
    );
    assert_eq!(rec.snapshot().tracks.len(), 1);
    // Exactamente en la frontera: compromete.
    let exact = silence(&mut rec, 11.5);
    assert_eq!(lines(&exact), ["[Ana]: uno"]);

    // Subir el debounce a 3.0 para la siguiente pista.
    rec.config_mut().debounce_s = 3.0;
    let _ = say(&mut rec, "Beto", "dos", 20.0);
    assert!(silence(&mut rec, 22.999_999).committed.is_empty());
    assert_eq!(lines(&silence(&mut rec, 23.0)), ["[Beto]: dos"]);

    // Bajar a 0.5: frontera exacta con valores representables.
    rec.config_mut().debounce_s = 0.5;
    let _ = say(&mut rec, "Carla", "tres", 30.0);
    assert!(silence(&mut rec, 30.25).committed.is_empty());
    assert_eq!(lines(&silence(&mut rec, 30.5)), ["[Carla]: tres"]);
}

// -- (7) open_lines ------------------------------------------------------

#[test]
fn open_lines_lista_por_ts_con_formato() {
    let mut rec = Reconciler::new(ReconcilerConfig::default(), 0.0, None, true);
    let _ = rec.ingest("Carla", "tercera", 9.0);
    let _ = rec.ingest("Ana", "primera", 2.0);
    let _ = rec.ingest("Beto", "segunda", 5.0);
    assert_eq!(
        rec.open_lines(),
        ["[Ana]: primera", "[Beto]: segunda", "[Carla]: tercera"]
    );

    // Crecer a Ana mueve su ts al final.
    let _ = rec.ingest("Ana", "primera ampliada", 11.0);
    assert_eq!(
        rec.open_lines(),
        [
            "[Beto]: segunda",
            "[Carla]: tercera",
            "[Ana]: primera ampliada"
        ]
    );

    // Un hablante vacio se formatea como "[]: texto".
    let _ = rec.ingest("", "sin nombre", 0.5);
    assert_eq!(rec.open_lines()[0], "[]: sin nombre");
    assert_eq!(rec.open_lines().len(), 4);
}

// -- (8) Decision ---------------------------------------------------------

#[test]
fn decision_display_y_fromstr_canonicos() {
    let casos = [
        (Decision::EmptyText, "empty_text"),
        (Decision::NoAlnum, "no_alnum"),
        (Decision::NewOpen, "new_open"),
        (Decision::DupExact, "dup_exact"),
        (Decision::DupNorm, "dup_norm"),
        (Decision::DupNormLonger, "dup_norm_longer"),
        (Decision::Grow, "grow"),
        (
            Decision::Merge {
                prefix_ratio: 0.67,
                ratio: 0.5,
            },
            "merge_p0.67_r0.50",
        ),
        (
            Decision::Merge {
                prefix_ratio: 1.0,
                ratio: 0.0,
            },
            "merge_p1.00_r0.00",
        ),
        (Decision::Newline, "newline"),
        (Decision::Committed, "committed"),
        (Decision::DupCommittedSkip, "dup_committed_skip"),
    ];
    for (decision, label) in &casos {
        assert_eq!(decision.to_string(), *label);
        let parsed: Decision = label.parse().unwrap_or_else(|e| panic!("{label}: {e}"));
        assert_eq!(parsed.to_string(), *label, "ida y vuelta");
        assert_eq!(parsed, *decision, "{label}");
    }
    // Todas distintas entre si (igualdad por etiqueta).
    for (i, (a, _)) in casos.iter().enumerate() {
        for (j, (b, _)) in casos.iter().enumerate() {
            assert_eq!(a == b, i == j, "{i} vs {j}");
        }
    }
}

#[test]
fn decision_fromstr_rechaza_formas_no_canonicas() {
    for bad in [
        "",
        "foo",
        "Grow",
        "grow ",
        "merge_p0.67",
        "merge_p0.5_r1",
        "merge_pnan_r1",
        "merge_p0.67_r0.67x",
        "committed ",
    ] {
        assert!(bad.parse::<Decision>().is_err(), "{bad:?} debe rechazarse");
    }
}

#[test]
fn decision_merge_igual_por_etiqueta_de_dos_decimales() {
    let a = Decision::Merge {
        prefix_ratio: 0.671,
        ratio: 0.801,
    };
    let b = Decision::Merge {
        prefix_ratio: 0.674,
        ratio: 0.804,
    };
    assert_eq!(a.to_string(), "merge_p0.67_r0.80");
    assert_eq!(a, b, "misma etiqueta de dos decimales");
    let parsed: Decision = "merge_p0.67_r0.80".parse().expect("etiqueta canonica");
    assert_eq!(parsed, a, "el parseado iguala al logueado");
    assert_ne!(
        a,
        Decision::Merge {
            prefix_ratio: 0.68,
            ratio: 0.80,
        }
    );
    assert_ne!(a, Decision::Grow);
}

// -- (9) conservacion, flush y bordes -------------------------------------

#[test]
fn conservacion_de_texto() {
    for seed in 0..SCENARIOS {
        let script = gen_script(seed);
        // Lo entregado, con la etiqueta que le asigna el reconciliador.
        let mut delivered: HashSet<(String, String)> = HashSet::new();
        for (observations, _) in &script {
            for o in observations {
                if let Some(text) = &o.text {
                    let (label, _) = normalize_speaker(o.speaker.as_deref());
                    delivered.insert((label, clean_text(text)));
                }
            }
        }

        let mut rec = Reconciler::new(ReconcilerConfig::default(), 0.0, None, true);
        let mut outcomes = Vec::with_capacity(script.len() + 1);
        for (observations, now) in &script {
            outcomes.push(rec.step(observations, &ctx(*now)));
        }
        let abiertas = rec.snapshot().tracks;
        let flushed = rec.flush();

        // Todo lo comprometido (steps y flush) sale de una observacion entregada.
        for out in outcomes.iter().chain(std::iter::once(&flushed)) {
            for seg in &out.committed {
                assert!(
                    delivered.contains(&(seg.speaker.clone(), seg.text.clone())),
                    "seed {seed}: {:?} sin observacion de origen",
                    seg.line()
                );
            }
        }

        // Cada pista abierta antes del flush sale al final como committed o
        // dup_committed_skip, con el mismo (speaker, text). El flush solo
        // produce esos records, asi que el total debe coincidir.
        assert_eq!(
            flushed.records.len(),
            abiertas.len(),
            "seed {seed}: records del flush"
        );
        let mut esperado: Vec<(String, String)> = abiertas
            .iter()
            .map(|(sp, text, _)| (sp.clone(), text.clone()))
            .collect();
        let mut obtenido: Vec<(String, String)> = Vec::new();
        for record in &flushed.records {
            assert!(
                matches!(
                    record.decision,
                    Decision::Committed | Decision::DupCommittedSkip
                ),
                "seed {seed}: decision inesperada en el flush"
            );
            obtenido.push((
                record.speaker.clone(),
                record.text.clone().unwrap_or_default(),
            ));
        }
        esperado.sort();
        obtenido.sort();
        assert_eq!(obtenido, esperado, "seed {seed}: pistas abiertas");
    }
}

#[test]
fn flush_idempotente() {
    let mut rec = Reconciler::new(ReconcilerConfig::default(), 0.0, None, true);
    let _ = rec.ingest("Ana", "uno", 1.0);
    let _ = rec.ingest("Beto", "dos", 2.0);
    let primero = rec.flush();
    assert_eq!(lines(&primero), ["[Ana]: uno", "[Beto]: dos"]);
    let segundo = rec.flush();
    assert!(!segundo.changed);
    assert!(segundo.records.is_empty(), "el segundo flush no registra");
    assert!(
        segundo.committed.is_empty(),
        "el segundo flush no compromete"
    );
}

#[test]
fn step_tras_flush_reabre() {
    // Sin reloj.
    let mut rec = Reconciler::new(ReconcilerConfig::default(), 0.0, None, true);
    let _ = say(&mut rec, "Ana", "uno", 1.0);
    let _ = rec.flush();
    let out = say(&mut rec, "Ana", "otra vez", 2.0);
    assert_eq!(out.records.len(), 1);
    assert_eq!(out.records[0].decision, Decision::NewOpen);
    assert!(out.committed.is_empty());

    // Con reloj: el nuevo step es el frame siguiente.
    let (_, clock) = scripted(8);
    let mut rec = Reconciler::new(ReconcilerConfig::default(), 0.0, Some(clock), true);
    let _ = rec.step(&[open_obs("Ana", "uno")], &ctx(1.0));
    let frame = rec.snapshot().frame;
    let _ = rec.flush();
    let out = rec.step(&[open_obs("Ana", "otra vez")], &ctx(2.0));
    assert_eq!(out.records.len(), 1);
    assert_eq!(out.records[0].decision, Decision::NewOpen);
    assert_eq!(out.records[0].frame, FrameRef::InFrame(frame + 1));
}

#[test]
fn now_s_extremos() {
    for now in [f64::INFINITY, f64::NEG_INFINITY, -5.0, f64::NAN] {
        let mut rec = Reconciler::new(ReconcilerConfig::default(), 0.0, None, true);
        let out = say(&mut rec, "Ana", "hola", now);
        assert!(out.committed.is_empty(), "now={now}: no compromete");
        assert_eq!(
            rec.open_lines(),
            ["[Ana]: hola"],
            "now={now}: sigue abierta"
        );
    }

    let mut rec = Reconciler::new(ReconcilerConfig::default(), 0.0, None, true);
    let _ = rec.ingest("Ana", "hola", 1.0);
    let (changed, out) = rec.commit_stale(f64::INFINITY);
    assert!(changed);
    assert_eq!(lines(&out), ["[Ana]: hola"]);
}

#[test]
fn read_ms_none_con_clock() {
    let (_, clock) = scripted(4);
    let mut rec = Reconciler::new(ReconcilerConfig::default(), 0.0, Some(clock), true);

    let sin_lectura = StepCtx {
        now_s: 1.0,
        read_ms: None,
    };
    let out = rec.step(&[open_obs("Ana", "uno")], &sin_lectura);
    assert!(!out.records.is_empty());
    for r in &out.records {
        assert!(matches!(r.frame, FrameRef::InFrame(_)));
        assert_eq!(r.read_ms, None, "dentro de frame sin read_ms");
    }

    let con_lectura = StepCtx {
        now_s: 2.0,
        read_ms: Some(7.5),
    };
    let out = rec.step(&[open_obs("Beto", "dos")], &con_lectura);
    assert!(!out.records.is_empty());
    for r in &out.records {
        assert!(matches!(r.frame, FrameRef::InFrame(_)));
        assert_eq!(r.read_ms, Some(7.5), "dentro de frame con read_ms");
    }

    let fl = rec.flush();
    assert!(!fl.records.is_empty());
    for r in &fl.records {
        assert_eq!(r.frame, FrameRef::OutOfFrame);
        assert_eq!(r.read_ms, None, "el flush no lleva read_ms");
    }
}

#[test]
fn ingest_speaker_vacio() {
    let mut rec = Reconciler::new(ReconcilerConfig::default(), 0.0, None, true);
    let _ = rec.ingest("", "sin nombre", 1.0);
    let (_, out) = rec.commit_stale(10.0);
    assert_eq!(out.committed.len(), 1);
    assert_eq!(out.committed[0].speaker, "");
    assert_eq!(lines(&out), ["[]: sin nombre"]);

    // Reaparicion del mismo texto: recent lo reconoce como duplicado.
    let _ = rec.ingest("", "sin nombre", 11.0);
    let (_, again) = rec.commit_stale(20.0);
    assert!(
        again.committed.is_empty(),
        "la reaparicion no se compromete"
    );
    assert!(
        again
            .records
            .iter()
            .any(|r| r.decision == Decision::DupCommittedSkip)
    );
}
