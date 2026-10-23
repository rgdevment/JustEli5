//! Defaults shared with the Python implementation (`config.CAPTURE_DEFAULTS`).
//! A test checks them against `fixtures/vectors/config_defaults.json`.

/// Seconds a line must stay unchanged before it is committed.
pub const LINE_DEBOUNCE_SEC: f64 = 1.2;
/// Similarity at or above which a divergent frame still counts as a correction.
pub const MERGE_SIMILARITY: f64 = 0.80;
/// How many recently committed (speaker, text) signatures are remembered.
pub const RECENT_COMMITTED_CAPACITY: usize = 80;

/// Names Teams gives to external/anonymous participants (compared case-folded).
pub const ANONYMOUS_SPEAKER_ALIASES: [&str; 2] = ["usuario desconocido", "unknown user"];
/// Label every anonymous participant is kept under.
pub const ANONYMOUS_SPEAKER_LABEL: &str = "Invitado sin nombre";
