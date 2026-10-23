//! Speaker-name normalization, ported from the original Python app.

use caseless::default_case_fold_str;

use crate::config::{ANONYMOUS_SPEAKER_ALIASES, ANONYMOUS_SPEAKER_LABEL};
use crate::pytext::py_isspace;

/// Returns `(label, is_anonymous)` for a speaker name read from a caption.
///
/// Anonymous aliases collapse to one label: from the UI tuple alone two
/// anonymous guests are indistinguishable, and inventing "#1"/"#2" would be a
/// fake identity. Empty or missing stays `""` (a separate, known gap).
pub fn normalize_speaker(raw: Option<&str>) -> (String, bool) {
    let name = raw
        .unwrap_or("")
        .split(py_isspace)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if name.is_empty() {
        return (String::new(), false);
    }
    if ANONYMOUS_SPEAKER_ALIASES.contains(&default_case_fold_str(&name).as_str()) {
        return (ANONYMOUS_SPEAKER_LABEL.to_string(), true);
    }
    (name, false)
}
