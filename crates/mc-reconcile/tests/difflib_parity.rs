//! Parity tests for `mc_reconcile::difflib` against CPython's `difflib`.
//!
//! Expected values were produced with the real Python interpreter, e.g.:
//!
//! ```text
//! python3 -c "import difflib as d; \
//!   print(repr(d.SequenceMatcher(None,'abxcd','abcd').ratio())); \
//!   print(d.SequenceMatcher(None,'abxcd','abcd').get_matching_blocks())"
//! ```
//!
//! (`get_matching_blocks()` in Python ends with a `(len(a), len(b), 0)`
//! sentinel, which is not part of `matching_blocks()` here.)

use mc_reconcile::difflib::{Match, SequenceMatcher, ratio, ratio_nojunk};
use serde::Deserialize;

#[derive(Deserialize)]
struct Pair {
    a: String,
    b: String,
    ratio: f64,
    ratio_nojunk: f64,
}

fn load_pairs() -> Vec<Pair> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/vectors/ratio.json"
    );
    let text = std::fs::read_to_string(path).expect("no se pudo leer ratio.json");
    serde_json::from_str(&text).expect("ratio.json no es JSON valido")
}

#[test]
fn ratio_matches_python_on_all_fixture_pairs() {
    let pairs = load_pairs();
    assert_eq!(
        pairs.len(),
        5000,
        "ratio.json debe tener exactamente 5000 pares"
    );

    let long_b = pairs.iter().filter(|p| p.b.chars().count() >= 200).count();
    assert!(
        long_b >= 1000,
        "se esperaban >= 1000 pares con len(b) >= 200 (autojunk activo), hay {long_b}"
    );

    let junk_visible = pairs.iter().filter(|p| p.ratio != p.ratio_nojunk).count();
    assert!(
        junk_visible > 0,
        "debe haber pares donde ratio != ratio_nojunk (autojunk activo)"
    );

    for (i, p) in pairs.iter().enumerate() {
        let got = ratio(&p.a, &p.b);
        assert_eq!(
            got, p.ratio,
            "par {i}: a={:?} b={:?} ratio esperado {} obtenido {}",
            p.a, p.b, p.ratio, got
        );
        let got_nj = ratio_nojunk(&p.a, &p.b);
        assert_eq!(
            got_nj, p.ratio_nojunk,
            "par {i}: a={:?} b={:?} ratio_nojunk esperado {} obtenido {}",
            p.a, p.b, p.ratio_nojunk, got_nj
        );
    }
}

#[test]
fn documented_examples() {
    // Python: SequenceMatcher(None,'abxcd','abcd').ratio() == 8/9
    let sm = SequenceMatcher::new("abxcd", "abcd");
    assert_eq!(sm.ratio(), 2.0 * 4.0 / 9.0);
    assert_eq!(
        sm.matching_blocks(),
        vec![
            Match {
                a_start: 0,
                b_start: 0,
                size: 2
            },
            Match {
                a_start: 3,
                b_start: 2,
                size: 2
            },
        ]
    );

    // Python: SequenceMatcher(None,'private Thread currentThread;',
    //         'private volatile Thread currentThread;').ratio()
    assert_eq!(
        ratio(
            "private Thread currentThread;",
            "private volatile Thread currentThread;"
        ),
        0.8656716417910447
    );

    // Python: SequenceMatcher(None,' abcd','abcd abcd').find_longest_match(0,5,0,9)
    //         == Match(a=0, b=4, size=5)
    let sm = SequenceMatcher::new(" abcd", "abcd abcd");
    assert_eq!(
        sm.find_longest_match(0, 5, 0, 9),
        Match {
            a_start: 0,
            b_start: 4,
            size: 5
        }
    );
}

#[test]
fn edge_cases() {
    assert_eq!(ratio("", ""), 1.0);
    assert_eq!(ratio("a", ""), 0.0);
    assert_eq!(ratio("", "a"), 0.0);
    assert_eq!(ratio("hola mundo", "hola mundo"), 1.0);
    assert_eq!(ratio("abc", "xyz"), 0.0);
    assert_eq!(ratio_nojunk("", ""), 1.0);
    assert_eq!(ratio_nojunk("a", ""), 0.0);

    // Caracteres por `char`, no por unidad UTF-8 ni UTF-16.
    // Python: SequenceMatcher(None,'\U0001F600a','\U0001F600b').ratio() == 0.5
    assert_eq!(ratio("\u{1F600}a", "\u{1F600}b"), 0.5);
    // Python: SequenceMatcher(None,'i̇a','ia').ratio() == 0.8
    // ('i' + combining dot above = 2 chars; 'ia' = 2 chars; matches 'i' only.)
    assert_eq!(ratio("i\u{307}a", "ia"), 0.8);
    assert_eq!(ratio_nojunk("i\u{307}a", "ia"), 0.8);
}

#[test]
fn autojunk_boundary_at_200_chars() {
    // b de 199 chars: autojunk inactivo (len(b) < 200), ratio == ratio_nojunk.
    // Python: a='a'*60+'b'*40, b='a'*150+'b'*49
    //   ratio == ratio_nojunk == 0.6688963210702341
    let a = format!("{}{}", "a".repeat(60), "b".repeat(40));
    let b199 = format!("{}{}", "a".repeat(150), "b".repeat(49));
    let b200 = format!("{}{}", "a".repeat(150), "b".repeat(50));
    assert_eq!(b199.chars().count(), 199);
    assert_eq!(b200.chars().count(), 200);

    assert_eq!(ratio(&a, &b199), 0.6688963210702341);
    assert_eq!(ratio_nojunk(&a, &b199), 0.6688963210702341);

    // b de 200 chars: autojunk activo, 'a' y 'b' son populares (> 1%) y se
    // descartan como semillas.
    // Python: ratio == 0.4, ratio_nojunk == 0.6666666666666666
    assert_eq!(ratio(&a, &b200), 0.4);
    assert_eq!(ratio_nojunk(&a, &b200), 0.6666666666666666);
}

/// LCG de Knuth (MMIX) propio, sin dependencias.
struct Lcg(u64);

impl Lcg {
    fn bits(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, n: usize) -> usize {
        (self.bits() % n as u64) as usize
    }
}

const ALPHABET: [char; 4] = ['a', 'b', 'c', '\u{1F600}'];

fn rand_str(rng: &mut Lcg, len: usize) -> String {
    (0..len)
        .map(|_| ALPHABET[rng.below(ALPHABET.len())])
        .collect()
}

#[test]
fn properties_hold_on_random_pairs() {
    let mut rng = Lcg(0x5EED_C0DE);
    for case in 0..2000 {
        let la = rng.below(301);
        let lb = rng.below(301);
        let a = rand_str(&mut rng, la);
        let b = rand_str(&mut rng, lb);
        let ac: Vec<char> = a.chars().collect();
        let bc: Vec<char> = b.chars().collect();
        let ctx = format!("caso {case}: a={a:?} b={b:?}");

        let sm = SequenceMatcher::new(&a, &b);
        let r = sm.ratio();
        assert!((0.0..=1.0).contains(&r), "{ctx}: ratio {r} fuera de [0,1]");

        if !a.is_empty() {
            assert_eq!(ratio(&a, &a), 1.0, "{ctx}: ratio(a,a) debe ser 1.0");
        }

        let blocks = sm.matching_blocks();
        let mut total = 0usize;
        for (idx, m) in blocks.iter().enumerate() {
            assert!(m.size > 0, "{ctx}: bloque vacio {m:?}");
            assert!(
                m.a_start + m.size <= ac.len() && m.b_start + m.size <= bc.len(),
                "{ctx}: bloque fuera de rango {m:?}"
            );
            assert_eq!(
                ac[m.a_start..m.a_start + m.size],
                bc[m.b_start..m.b_start + m.size],
                "{ctx}: bloque {m:?} no coincide"
            );
            if idx > 0 {
                let p = blocks[idx - 1];
                assert!(
                    p.a_start + p.size <= m.a_start && p.b_start + p.size <= m.b_start,
                    "{ctx}: bloques solapados o desordenados {p:?} {m:?}"
                );
                assert!(
                    p.a_start < m.a_start && p.b_start < m.b_start,
                    "{ctx}: bloques no estrictamente crecientes {p:?} {m:?}"
                );
            }
            total += m.size;
        }
        assert!(
            total <= ac.len().min(bc.len()),
            "{ctx}: suma de bloques {total} > min(len a, len b)"
        );

        let length = la + lb;
        let expected = if length == 0 {
            1.0
        } else {
            2.0 * total as f64 / length as f64
        };
        assert_eq!(r, expected, "{ctx}: ratio no coincide con 2*M/T");

        if lb < 200 {
            assert_eq!(
                ratio_nojunk(&a, &b),
                r,
                "{ctx}: sin autojunk (len(b) < 200) ratio y ratio_nojunk deben coincidir"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// fixtures/vectors/difflib_extra.json: find_longest_match, matching_blocks y
// ratios de secuencias largas, exportados desde la app Python original.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct FlmCase {
    a: String,
    b: String,
    range: [usize; 4],
    #[serde(rename = "match")]
    matched: [usize; 3],
}

#[derive(Deserialize)]
struct BlocksCase {
    a: String,
    b: String,
    blocks: Vec<[usize; 3]>,
}

#[derive(Deserialize)]
struct LongRatioCase {
    b: String,
    a: String,
    ratio: f64,
    ratio_nojunk: f64,
}

#[derive(Deserialize)]
struct DifflibExtra {
    find_longest_match: Vec<FlmCase>,
    blocks: Vec<BlocksCase>,
    long_ratio: Vec<LongRatioCase>,
}

fn load_extra() -> DifflibExtra {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/vectors/difflib_extra.json"
    );
    let text = std::fs::read_to_string(path).expect("no se pudo leer difflib_extra.json");
    serde_json::from_str(&text).expect("difflib_extra.json no es JSON valido")
}

fn triples(blocks: &[Match]) -> Vec<[usize; 3]> {
    blocks
        .iter()
        .map(|m| [m.a_start, m.b_start, m.size])
        .collect()
}

#[test]
fn extra_vectors_are_not_empty() {
    let extra = load_extra();
    assert!(
        extra.find_longest_match.len() >= 500,
        "find_longest_match: se esperaban >= 500 casos, hay {}",
        extra.find_longest_match.len()
    );
    assert!(
        extra.blocks.len() >= 1000,
        "blocks: se esperaban >= 1000 casos, hay {}",
        extra.blocks.len()
    );
    assert!(
        extra.long_ratio.len() >= 100,
        "long_ratio: se esperaban >= 100 pares, hay {}",
        extra.long_ratio.len()
    );
}

#[test]
fn find_longest_match_matches_python_on_all_extra_cases() {
    let extra = load_extra();
    for (idx, c) in extra.find_longest_match.iter().enumerate() {
        let [alo, ahi, blo, bhi] = c.range;
        let m = SequenceMatcher::new(&c.a, &c.b).find_longest_match(alo, ahi, blo, bhi);
        assert_eq!(
            [m.a_start, m.b_start, m.size],
            c.matched,
            "caso {idx}: a={:?} b={:?} range={:?}",
            c.a,
            c.b,
            c.range
        );
    }
}

#[test]
fn find_longest_match_explicit_cases() {
    // Python: SequenceMatcher(None,'xxabcdyy','zabcdw').find_longest_match(1,7,1,5)
    //         == Match(a=2, b=1, size=4)
    let sm = SequenceMatcher::new("xxabcdyy", "zabcdw");
    assert_eq!(
        sm.find_longest_match(1, 7, 1, 5),
        Match {
            a_start: 2,
            b_start: 1,
            size: 4
        }
    );
    // Python: find_longest_match(3,8,0,6) == Match(a=3, b=2, size=3)
    assert_eq!(
        sm.find_longest_match(3, 8, 0, 6),
        Match {
            a_start: 3,
            b_start: 2,
            size: 3
        }
    );
    // Python: SequenceMatcher(None,'ab','c').find_longest_match(0,2,0,1)
    //         == Match(a=0, b=0, size=0): sin coincidencias devuelve (alo, blo, 0).
    assert_eq!(
        SequenceMatcher::new("ab", "c").find_longest_match(0, 2, 0, 1),
        Match {
            a_start: 0,
            b_start: 0,
            size: 0
        }
    );
}

#[test]
fn matching_blocks_match_python_on_all_extra_cases() {
    let extra = load_extra();
    for (idx, c) in extra.blocks.iter().enumerate() {
        let got = triples(&SequenceMatcher::new(&c.a, &c.b).matching_blocks());
        assert_eq!(got, c.blocks, "caso {idx}: a={:?} b={:?}", c.a, c.b);
    }
}

#[test]
fn long_ratio_matches_python_on_all_extra_pairs() {
    let extra = load_extra();
    assert_eq!(
        extra.long_ratio.len(),
        100,
        "difflib_extra.json debe tener exactamente 100 pares long_ratio"
    );

    let junk_visible = extra
        .long_ratio
        .iter()
        .filter(|p| p.b.chars().count() >= 600 && p.ratio != p.ratio_nojunk)
        .count();
    assert!(
        junk_visible > 0,
        "debe haber pares con len(b) >= 600 donde ratio != ratio_nojunk"
    );

    for (idx, p) in extra.long_ratio.iter().enumerate() {
        assert_eq!(
            ratio(&p.a, &p.b),
            p.ratio,
            "par {idx}: ratio no coincide con Python"
        );
        assert_eq!(
            ratio_nojunk(&p.a, &p.b),
            p.ratio_nojunk,
            "par {idx}: ratio_nojunk no coincide con Python"
        );
    }
}

#[test]
fn ratio_autojunk_explicit_cases() {
    // Python: a='a'*200, b='a'*200: 'a' es popular, pero la re-extension
    // recupera el bloque entero, ratio == 1.0.
    let a200 = "a".repeat(200);
    assert_eq!(ratio(&a200, &a200), 1.0);
    // Python: 'ab'*150 contra si mismo: ambos caracteres son populares, ratio 1.0.
    let ab300 = "ab".repeat(150);
    assert_eq!(ratio(&ab300, &ab300), 1.0);

    // len(b) = 199 < 200: autojunk no aplica, el resultado coincide con
    // ratio_nojunk. Python: ratio == ratio_nojunk == 0.8844444444444445.
    let a = format!("{}b", "a".repeat(250));
    let b = "a".repeat(199);
    assert_eq!(b.chars().count(), 199);
    assert_eq!(ratio(&a, &b), 0.8844444444444445);
    assert_eq!(ratio(&a, &b), ratio_nojunk(&a, &b));
}

#[test]
fn matching_blocks_never_adjacent_on_random_pairs() {
    let mut rng = Lcg(0xAD1A_CE17_0BAD_F00D);
    for case in 0..2000 {
        let la = rng.below(301);
        let lb = rng.below(301);
        let a = rand_str(&mut rng, la);
        let b = rand_str(&mut rng, lb);
        let blocks = SequenceMatcher::new(&a, &b).matching_blocks();
        for pair in blocks.windows(2) {
            let (p, m) = (pair[0], pair[1]);
            assert!(
                !(p.a_start + p.size == m.a_start && p.b_start + p.size == m.b_start),
                "caso {case}: bloques adyacentes sin fusionar {p:?} {m:?} a={a:?} b={b:?}"
            );
        }
    }
}
