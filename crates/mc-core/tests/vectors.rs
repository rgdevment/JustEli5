//! Parity tests: mc-core against vectors exported from the original Python app.
//!
//! The vectors live in `fixtures/vectors/` at the repository root. Every case
//! asserts with its index and input, so a failure names the exact entry.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::PathBuf;

use caseless::default_case_fold_str;
use mc_core::config;
use mc_core::pytext::{clean_text, common_prefix_len, fmt2, has_alnum, normalize_text, py_round3};
use mc_core::speakers::normalize_speaker;
use mc_core::types::Decision;
use serde::Deserialize;

fn vectors_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/vectors")
        .join(name)
}

fn load<T: serde::de::DeserializeOwned>(name: &str) -> T {
    let path = vectors_path(name);
    let text =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("cannot parse {name}: {e}"))
}

#[derive(Deserialize)]
struct Primitives {
    clean_text: Vec<CleanCase>,
    common_prefix_len: Vec<PrefixCase>,
    float_format: Vec<FloatCase>,
    fmt2_grid: Vec<(f64, String)>,
    merge_metrics: Vec<MergeCase>,
    normalize_speaker: Vec<SpeakerCase>,
    normalize_text: Vec<TextCase>,
    punctuation: String,
}

#[derive(Deserialize)]
struct TextCase {
    #[serde(rename = "in")]
    input: String,
    out: String,
}

#[derive(Deserialize)]
struct CleanCase {
    #[serde(rename = "in")]
    input: String,
    out: String,
    alnum: bool,
}

#[derive(Deserialize)]
struct PrefixCase {
    a: String,
    b: String,
    out: usize,
}

#[derive(Deserialize)]
struct FloatCase {
    #[serde(rename = "in")]
    input: f64,
    fmt2: String,
    round3: f64,
}

#[derive(Deserialize)]
struct SpeakerCase {
    #[serde(rename = "in")]
    input: Option<String>,
    label: String,
    anonymous: bool,
}

#[derive(Deserialize)]
struct MergeCase {
    prev: String,
    text: String,
    np: String,
    nt: String,
    norm_equal: bool,
    grows: bool,
    prefix_ratio: f64,
    ratio: f64,
    merges: bool,
    label: String,
}

/// `fixtures/vectors/unicode_case.json`: code points whose `lower()` /
/// `casefold()` differs from themselves in the original Python app.
#[derive(Deserialize)]
struct UnicodeCase {
    unidata_version: String,
    lower: Vec<(u32, String)>,
    casefold: Vec<(u32, String)>,
}

#[derive(Deserialize)]
struct CaptureDefaults {
    line_debounce_sec: f64,
    merge_similarity: f64,
}

#[derive(Deserialize)]
struct ConfigDefaults {
    anonymous_speaker_aliases: Vec<String>,
    anonymous_speaker_label: String,
    capture_defaults: CaptureDefaults,
    recent_committed_capacity: usize,
}

fn primitives() -> Primitives {
    load("primitives.json")
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

#[test]
fn normalize_text_matches_python() {
    let p = primitives();
    assert!(!p.normalize_text.is_empty(), "no normalize_text vectors");
    for (i, case) in p.normalize_text.iter().enumerate() {
        let got = normalize_text(&case.input);
        assert!(
            got == case.out,
            "normalize_text[{i}] input {:?}: expected {:?}, got {:?}",
            case.input,
            case.out,
            got
        );
    }
}

#[test]
fn clean_text_and_has_alnum_match_python() {
    let p = primitives();
    assert!(!p.clean_text.is_empty(), "no clean_text vectors");
    for (i, case) in p.clean_text.iter().enumerate() {
        let cleaned = clean_text(&case.input);
        assert!(
            cleaned == case.out,
            "clean_text[{i}] input {:?}: expected out {:?}, got {:?}",
            case.input,
            case.out,
            cleaned
        );
        let alnum = has_alnum(&cleaned);
        assert!(
            alnum == case.alnum,
            "clean_text[{i}] input {:?}: expected alnum {}, got {} (cleaned {:?})",
            case.input,
            case.alnum,
            alnum,
            cleaned
        );
    }
}

#[test]
fn normalize_speaker_matches_python() {
    let p = primitives();
    assert!(
        !p.normalize_speaker.is_empty(),
        "no normalize_speaker vectors"
    );
    for (i, case) in p.normalize_speaker.iter().enumerate() {
        let (label, anonymous) = normalize_speaker(case.input.as_deref());
        assert!(
            label == case.label && anonymous == case.anonymous,
            "normalize_speaker[{i}] input {:?}: expected ({:?}, {}), got ({:?}, {})",
            case.input,
            case.label,
            case.anonymous,
            label,
            anonymous
        );
    }
}

#[test]
fn common_prefix_len_matches_python() {
    let p = primitives();
    assert!(
        !p.common_prefix_len.is_empty(),
        "no common_prefix_len vectors"
    );
    for (i, case) in p.common_prefix_len.iter().enumerate() {
        let got = common_prefix_len(&case.a, &case.b);
        assert!(
            got == case.out,
            "common_prefix_len[{i}] a {:?} b {:?}: expected {}, got {}",
            case.a,
            case.b,
            case.out,
            got
        );
    }
}

#[test]
fn float_format_matches_python_exactly() {
    let p = primitives();
    assert!(!p.float_format.is_empty(), "no float_format vectors");
    for (i, case) in p.float_format.iter().enumerate() {
        let got2 = fmt2(case.input);
        assert!(
            got2 == case.fmt2,
            "float_format[{i}] x {:?}: fmt2 expected {:?}, got {:?}",
            case.input,
            case.fmt2,
            got2
        );
        let got3 = py_round3(case.input);
        assert!(
            got3 == case.round3,
            "float_format[{i}] x {:?}: round3 expected {:?}, got {:?}",
            case.input,
            case.round3,
            got3
        );
    }
}

#[test]
fn fmt2_grid_matches_python() {
    let p = primitives();
    let total = p.fmt2_grid.len();
    assert!(
        total >= 27_000,
        "fmt2_grid has only {total} pairs, expected >= 27000"
    );
    for (i, (x, s)) in p.fmt2_grid.iter().enumerate() {
        let got = fmt2(*x);
        assert!(
            got == *s,
            "fmt2_grid[{i}] x {:?}: expected {:?}, got {:?}",
            x,
            s,
            got
        );
    }
}

#[test]
fn merge_metrics_match_python() {
    let p = primitives();
    assert!(!p.merge_metrics.is_empty(), "no merge_metrics vectors");
    for (i, case) in p.merge_metrics.iter().enumerate() {
        let ctx = format!(
            "merge_metrics[{i}] prev {:?} text {:?}",
            case.prev, case.text
        );

        let np = normalize_text(&case.prev);
        assert!(
            np == case.np,
            "{ctx}: np expected {:?}, got {:?}",
            case.np,
            np
        );

        let nt = normalize_text(&case.text);
        assert!(
            nt == case.nt,
            "{ctx}: nt expected {:?}, got {:?}",
            case.nt,
            nt
        );

        let norm_equal = np == nt;
        assert!(
            norm_equal == case.norm_equal,
            "{ctx}: norm_equal expected {}, got {}",
            case.norm_equal,
            norm_equal
        );

        let grows = !np.is_empty() && (nt.starts_with(&np) || nt.contains(&np));
        assert!(
            grows == case.grows,
            "{ctx}: grows expected {}, got {}",
            case.grows,
            grows
        );

        let min_len = char_len(&np).min(char_len(&nt));
        let prefix_ratio = if min_len == 0 {
            0.0
        } else {
            common_prefix_len(&np, &nt) as f64 / min_len as f64
        };
        assert!(
            prefix_ratio == case.prefix_ratio,
            "{ctx}: prefix_ratio expected {:?}, got {:?}",
            case.prefix_ratio,
            prefix_ratio
        );

        let label = Decision::Merge {
            prefix_ratio: case.prefix_ratio,
            ratio: case.ratio,
        }
        .to_string();
        assert!(
            label == case.label,
            "{ctx}: label expected {:?}, got {:?}",
            case.label,
            label
        );

        let merges = case.prefix_ratio >= 0.5 || case.ratio >= config::MERGE_SIMILARITY;
        assert!(
            merges == case.merges,
            "{ctx}: merges expected {}, got {} (prefix_ratio {:?}, ratio {:?})",
            case.merges,
            merges,
            case.prefix_ratio,
            case.ratio
        );
    }
}

#[test]
fn config_matches_python_defaults() {
    let d: ConfigDefaults = load("config_defaults.json");
    let c = &d.capture_defaults;
    assert!(
        config::LINE_DEBOUNCE_SEC == c.line_debounce_sec,
        "LINE_DEBOUNCE_SEC expected {:?}, got {:?}",
        c.line_debounce_sec,
        config::LINE_DEBOUNCE_SEC
    );
    assert!(
        config::MERGE_SIMILARITY == c.merge_similarity,
        "MERGE_SIMILARITY expected {:?}, got {:?}",
        c.merge_similarity,
        config::MERGE_SIMILARITY
    );
    assert!(
        config::RECENT_COMMITTED_CAPACITY == d.recent_committed_capacity,
        "RECENT_COMMITTED_CAPACITY expected {}, got {}",
        d.recent_committed_capacity,
        config::RECENT_COMMITTED_CAPACITY
    );
    assert!(
        config::ANONYMOUS_SPEAKER_ALIASES.as_slice() == d.anonymous_speaker_aliases.as_slice(),
        "ANONYMOUS_SPEAKER_ALIASES expected {:?}, got {:?}",
        d.anonymous_speaker_aliases,
        config::ANONYMOUS_SPEAKER_ALIASES
    );
    assert!(
        config::ANONYMOUS_SPEAKER_LABEL == d.anonymous_speaker_label,
        "ANONYMOUS_SPEAKER_LABEL expected {:?}, got {:?}",
        d.anonymous_speaker_label,
        config::ANONYMOUS_SPEAKER_LABEL
    );
}

#[test]
fn punctuation_is_exactly_what_normalize_text_strips() {
    let p = primitives();
    let declared: Vec<char> = p.punctuation.chars().collect();
    assert_eq!(declared.len(), 32, "punctuation must have 32 characters");
    let declared_set: BTreeSet<char> = declared.iter().copied().collect();
    assert_eq!(
        declared_set.len(),
        declared.len(),
        "punctuation has duplicates"
    );

    // Every declared character is removed entirely by normalize_text.
    for c in &declared {
        let got = normalize_text(&c.to_string());
        assert!(
            got.is_empty(),
            "punctuation {c:?} not stripped: got {got:?}"
        );
    }

    // Among printable ASCII, the non-alphanumeric, non-space characters are
    // exactly the declared set: nothing is stripped that Python keeps, and
    // nothing Python strips is missing.
    let printable: BTreeSet<char> = (0x20u8..=0x7e).map(char::from).collect();
    let non_alnum_non_space: BTreeSet<char> = printable
        .iter()
        .copied()
        .filter(|c| !c.is_ascii_alphanumeric() && *c != ' ')
        .collect();
    assert!(
        non_alnum_non_space == declared_set,
        "printable ASCII non-alnum set {non_alnum_non_space:?} != punctuation {declared_set:?}"
    );

    // Alphanumerics are kept (lowercased); a lone space collapses to "".
    for c in printable.iter().filter(|c| c.is_ascii_alphanumeric()) {
        let got = normalize_text(&c.to_string());
        let expected = c.to_ascii_lowercase().to_string();
        assert!(got == expected, "{c:?} expected {expected:?}, got {got:?}");
    }
    assert!(
        normalize_text(" ").is_empty(),
        "lone space must normalize to \"\""
    );
}

/// Code points where Rust's `char::to_lowercase` (Unicode 17 in toolchain 1.97)
/// differs from Python 3.13's `lower()` (UCD 15.1). They were added after 15.1,
/// so Python keeps them unchanged while Rust lowercases them. None can occur in
/// a speaker alias.
fn known_lower_diffs() -> BTreeSet<u32> {
    let mut set: BTreeSet<u32> = [
        0x1C89, 0xA7CB, 0xA7CC, 0xA7CE, 0xA7D2, 0xA7D4, 0xA7DA, 0xA7DC,
    ]
    .into_iter()
    .collect();
    set.extend(0x10D50..=0x10D65);
    set.extend(0x16EA0..=0x16EB8);
    set
}

/// Code points where `caseless::default_case_fold_str` (Unicode 16) differs
/// from Python 3.13's `casefold()` (UCD 15.1). Every one was added in Unicode
/// 16, so all are at least U+1C89 and none is ASCII.
const KNOWN_CASEFOLD_DIFFS: [u32; 27] = [
    0x1C89, 0xA7CB, 0xA7CC, 0xA7DA, 0xA7DC, 0x10D50, 0x10D51, 0x10D52, 0x10D53, 0x10D54, 0x10D55,
    0x10D56, 0x10D57, 0x10D58, 0x10D59, 0x10D5A, 0x10D5B, 0x10D5C, 0x10D5D, 0x10D5E, 0x10D5F,
    0x10D60, 0x10D61, 0x10D62, 0x10D63, 0x10D64, 0x10D65,
];

fn fmt_cps(set: &BTreeSet<u32>) -> String {
    set.iter()
        .map(|cp| format!("U+{cp:04X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn unicode_case_matches_python_except_known_tables() {
    let data: UnicodeCase = load("unicode_case.json");
    assert!(!data.unidata_version.is_empty());
    let py_lower: HashMap<u32, &str> = data.lower.iter().map(|(cp, s)| (*cp, s.as_str())).collect();
    let py_fold: HashMap<u32, &str> = data
        .casefold
        .iter()
        .map(|(cp, s)| (*cp, s.as_str()))
        .collect();
    assert_eq!(py_lower.len(), data.lower.len(), "duplicate lower entries");
    assert_eq!(
        py_fold.len(),
        data.casefold.len(),
        "duplicate casefold entries"
    );

    let mut lower_diffs = BTreeSet::new();
    let mut fold_diffs = BTreeSet::new();
    for cp in 0..=0x10FFFF_u32 {
        // Surrogates are not `char`s, so they cannot be compared.
        let Some(c) = char::from_u32(cp) else {
            continue;
        };
        let own = c.to_string();

        let want_lower = py_lower.get(&cp).map_or(own.as_str(), |s| s);
        if c.to_lowercase().collect::<String>() != want_lower {
            lower_diffs.insert(cp);
        }

        let want_fold = py_fold.get(&cp).map_or(own.as_str(), |s| s);
        if default_case_fold_str(&own) != want_fold {
            fold_diffs.insert(cp);
        }
    }

    let known_lower = known_lower_diffs();
    assert_eq!(known_lower.len(), 55, "lower list must have 55 points");
    assert_eq!(
        lower_diffs,
        known_lower,
        "lower differences changed.\n  got:      {}\n  expected: {}",
        fmt_cps(&lower_diffs),
        fmt_cps(&known_lower)
    );

    let known_fold: BTreeSet<u32> = KNOWN_CASEFOLD_DIFFS.into_iter().collect();
    assert_eq!(
        known_fold.len(),
        27,
        "casefold list must have 27 distinct points"
    );
    assert!(
        known_fold
            .iter()
            .all(|&cp| cp >= 0x1C89 && char::from_u32(cp).is_some_and(|c| !c.is_ascii())),
        "known casefold points must be non-ASCII and >= U+1C89"
    );
    assert_eq!(
        fold_diffs,
        known_fold,
        "casefold differences changed.\n  got:      {}\n  expected: {}",
        fmt_cps(&fold_diffs),
        fmt_cps(&known_fold)
    );
}
