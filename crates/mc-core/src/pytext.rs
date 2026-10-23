//! Python text semantics the reconciler depends on, reproduced exactly.
//!
//! Known gap: `str::to_lowercase` follows the Unicode version of the Rust
//! toolchain (17 in 1.97) while the original Python app (3.13) uses UCD 15.1, so 55
//! code points added in Unicode 16/17 (U+1C89, U+A7CB, U+A7CC, U+A7CE, U+A7D2,
//! U+A7D4, U+A7DA, U+A7DC, U+10D50..U+10D65, U+16EA0..U+16EB8) lowercase in Rust
//! but not in Python; `caseless` (Unicode 16) differs in 27 more for casefold.
//! None can occur in a speaker alias; `tests/units.rs` pins the exact sets so a
//! toolchain upgrade that changes them fails loudly.
//!
//! Rust's standard library differs from Python in small ways that would change
//! reconciliation decisions: `str.split()` / `re \s` treat U+001C..U+001F as
//! whitespace (Rust does not), punctuation stripping only touches ASCII, and
//! every length is a count of characters, not bytes.

/// ASCII punctuation, exactly Python's `string.punctuation`.
const ASCII_PUNCTUATION: &str = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";

/// Python's `str.isspace()` (also what `re` `\s` and `str.split()` use).
pub fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn collapse_whitespace(text: &str) -> String {
    text.split(py_isspace)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `re.sub(r"\s+", " ", text).strip()`.
pub fn clean_text(text: &str) -> String {
    collapse_whitespace(text)
}

/// `bool(re.search(r"[a-zA-Z0-9]", text))`: ASCII only on purpose.
pub fn has_alnum(text: &str) -> bool {
    text.chars().any(|c| c.is_ascii_alphanumeric())
}

/// `" ".join(text.translate(PUNCT).lower().split())`, with `""` for empty input.
///
/// Punctuation is stripped before lowercasing (that order decides final-sigma
/// handling) and only ASCII punctuation is removed, so `¿` and `¡` stay.
pub fn normalize_text(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let stripped: String = text
        .chars()
        .filter(|c| !ASCII_PUNCTUATION.contains(*c))
        .collect();
    collapse_whitespace(&stripped.to_lowercase())
}

/// Length of the common prefix of `a` and `b`, in characters.
pub fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// Python's `f"{x:.2f}"` (non-finite values print as Python does).
pub fn fmt2(x: f64) -> String {
    if x.is_nan() {
        "nan".to_string()
    } else if x.is_infinite() {
        if x > 0.0 { "inf" } else { "-inf" }.to_string()
    } else {
        format!("{x:.2}")
    }
}

/// Python's `round(x, 3)`: the correctly rounded 3-decimal value, ties to even
/// on the exact binary value.
pub fn py_round3(x: f64) -> f64 {
    format!("{x:.3}").parse().unwrap_or(x)
}
