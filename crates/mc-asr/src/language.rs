//! Which languages a meeting may use, and what to do when the engine's guess
//! is outside them.

/// Spanish with English terms is the normal case here, so the engine detects
/// the language of every utterance; a detection outside `allowed` (short
/// utterances are easy to misjudge) is redone in the fallback language.
#[derive(Debug, Clone, PartialEq)]
pub struct LanguagePolicy {
    pub allowed: Vec<String>,
    pub fallback: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Keep,
    Retry(String),
}

impl Default for LanguagePolicy {
    fn default() -> Self {
        Self {
            allowed: vec!["es".to_string(), "en".to_string()],
            fallback: "es".to_string(),
        }
    }
}

impl LanguagePolicy {
    /// `"es,en"`; the first is the fallback.
    pub fn parse(list: &str) -> Option<Self> {
        let allowed: Vec<String> = list
            .split(',')
            .map(|l| l.trim().to_lowercase())
            .filter(|l| !l.is_empty())
            .collect();
        let valid = !allowed.is_empty()
            && allowed
                .iter()
                .all(|l| l.len() == 2 && l.chars().all(|c| c.is_ascii_lowercase()));
        valid.then(|| Self {
            fallback: allowed[0].clone(),
            allowed,
        })
    }

    pub fn decide(&self, detected: &str) -> Decision {
        if self.allowed.iter().any(|l| l == detected) {
            Decision::Keep
        } else {
            Decision::Retry(self.fallback.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_languages_are_kept_and_others_retried_in_the_fallback() {
        let policy = LanguagePolicy::default();
        assert_eq!(policy.decide("es"), Decision::Keep);
        assert_eq!(policy.decide("en"), Decision::Keep);
        assert_eq!(policy.decide("pt"), Decision::Retry("es".into()));
        assert_eq!(policy.decide(""), Decision::Retry("es".into()));
    }

    #[test]
    fn parses_lists_and_rejects_nonsense() {
        let p = LanguagePolicy::parse("EN, es").unwrap();
        assert_eq!((p.fallback.as_str(), p.allowed.len()), ("en", 2));
        for bad in ["", ",", "spanish", "e", "es;en", "es,e1"] {
            assert!(LanguagePolicy::parse(bad).is_none(), "{bad:?}");
        }
    }
}
