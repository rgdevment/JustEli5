//! Series: meetings that recur under one name ("Daily de equipo 21/10", "Daily de equipo 22/10").

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeriesOrigin {
    /// Derived from the meeting name.
    Auto,
    /// A `series.rules` entry of the configuration.
    Rule,
    /// Set by the user for one session; nothing else overrides it.
    Manual,
}

impl SeriesOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Rule => "rule",
            Self::Manual => "manual",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "auto" => Some(Self::Auto),
            "rule" => Some(Self::Rule),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }
}

/// A meeting name that contains `pattern` (any case) belongs to the series `key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesRule {
    pub pattern: String,
    pub key: String,
}

/// The name with dates, numbers and punctuation removed, lowercase; `None` when nothing is left.
pub fn series_key(name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        // A word that starts with a digit is a date or a counter ("21", "2026", "3rd").
        .filter(|w| !w.is_empty() && !w.starts_with(|c: char| c.is_ascii_digit()))
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

/// A key the user typed, normalised like an automatic one ("Daily de equipo 21/10" is "daily de equipo");
/// a key with nothing left to normalise keeps its words. `None` when empty.
pub fn user_key(key: &str) -> Option<String> {
    series_key(key).or_else(|| clean_key(key))
}

/// Lowercase, single spaces. `None` when empty.
pub fn clean_key(key: &str) -> Option<String> {
    let flat = key.split_whitespace().collect::<Vec<_>>().join(" ");
    (!flat.is_empty()).then(|| flat.to_lowercase())
}

/// The series of a meeting name: the first matching rule, else the normalised name.
pub fn series_for_name(name: &str, rules: &[SeriesRule]) -> Option<(String, SeriesOrigin)> {
    let lower = name.to_lowercase();
    let by_rule = rules
        .iter()
        .filter(|r| !r.pattern.trim().is_empty())
        .find(|r| lower.contains(&r.pattern.trim().to_lowercase()))
        .and_then(|r| user_key(&r.key));
    match by_rule {
        Some(key) => Some((key, SeriesOrigin::Rule)),
        None => series_key(name).map(|key| (key, SeriesOrigin::Auto)),
    }
}

/// Words that say nothing about which meeting it is.
const FILLER: &[&str] = &[
    "a", "and", "con", "de", "del", "el", "en", "la", "las", "los", "of", "the", "with", "y",
];

fn telling_words(name: &str) -> Vec<String> {
    mc_core::fold::fold_words(&name.replace(|c: char| !c.is_alphanumeric(), " "))
        .split(' ')
        .filter(|w| !w.is_empty() && !FILLER.contains(w) && !w.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_string)
        .collect()
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (above + 1)
                .min(row[j] + 1)
                .min(diagonal + usize::from(ca != *cb));
            diagonal = above;
        }
    }
    row[b.len()]
}

/// One word written two ways: «Hito2» and «hito», or one letter off in a long word.
fn same_word(a: &str, b: &str) -> bool {
    let bare = |w: &str| w.trim_end_matches(|c: char| c.is_ascii_digit()).to_string();
    let (a, b) = (bare(a), bare(b));
    a == b || (a.chars().count() >= 4 && b.chars().count() >= 4 && edit_distance(&a, &b) <= 1)
}

/// Mostly how much of the shorter name the longer holds, so a name with more words still matches.
pub fn name_similarity(a: &str, b: &str) -> f64 {
    let (a, b) = (telling_words(a), telling_words(b));
    let (short, long) = if a.len() <= b.len() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    if short.is_empty() {
        return 0.0;
    }
    let shared = short
        .iter()
        .filter(|w| long.iter().any(|v| same_word(w, v)))
        .count() as f64;
    0.7 * shared / short.len() as f64 + 0.3 * shared / long.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_written_a_little_differently_are_alike_and_other_meetings_are_not() {
        let messy = "Actividad | B2B-Acme-Hito2: Sincronización | Microsoft Teams";
        assert!(name_similarity("B2B Acme Hito 2", messy) >= 0.6);
        assert!(name_similarity("Daily equipo", "Daily de equipo") > 0.99);
        assert!(name_similarity("Sincronizacion pagos", "Sincronización de pago") >= 0.6);
        assert!(name_similarity("Retro pagos", "Retro login") < 0.6);
        assert!(name_similarity("Planning", "Daily de equipo") < 0.6);
        assert_eq!(name_similarity("21/10", "Daily"), 0.0);
    }

    #[test]
    fn the_key_drops_dates_numbers_and_punctuation() {
        assert_eq!(
            series_key("Daily de equipo 21/10").as_deref(),
            Some("daily de equipo")
        );
        assert_eq!(
            series_key("  DAILY - de equipo (2026-10-21)  ").as_deref(),
            Some("daily de equipo")
        );
        assert_eq!(
            series_key("Sprint 42 Planning").as_deref(),
            Some("sprint planning")
        );
        assert_eq!(
            series_key("Revisión técnica").as_deref(),
            Some("revisión técnica")
        );
        assert_eq!(series_key("21/10 - 2026"), None);
        assert_eq!(series_key(""), None);
    }

    #[test]
    fn a_rule_beats_the_name_and_the_first_rule_wins() {
        let rules = [
            SeriesRule {
                pattern: "stand-up".into(),
                key: "Daily Equipo".into(),
            },
            SeriesRule {
                pattern: "stand".into(),
                key: "otra".into(),
            },
            SeriesRule {
                pattern: " ".into(),
                key: "vacía".into(),
            },
        ];
        assert_eq!(
            series_for_name("Team STAND-UP 3", &rules),
            Some(("daily equipo".to_string(), SeriesOrigin::Rule))
        );
        assert_eq!(
            series_for_name("Retro 4", &rules),
            Some(("retro".to_string(), SeriesOrigin::Auto))
        );
        assert_eq!(series_for_name("12", &rules), None);
    }

    #[test]
    fn user_keys_are_normalised_like_automatic_ones() {
        assert_eq!(
            user_key("Daily de equipo 21/10").as_deref(),
            Some("daily de equipo")
        );
        assert_eq!(user_key("2026").as_deref(), Some("2026"));
        assert_eq!(user_key("  "), None);
    }

    #[test]
    fn user_keys_are_cleaned() {
        assert_eq!(
            clean_key("  Daily   De Equipo ").as_deref(),
            Some("daily de equipo")
        );
        assert_eq!(clean_key("   "), None);
    }
}
