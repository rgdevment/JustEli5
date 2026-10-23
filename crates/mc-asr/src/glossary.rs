//! The team's vocabulary: terms with the misspellings recognisers give them. A configured
//! file and the user's own glossary (the one that learns) are merged into one.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::prompt::parse_glossary_entries;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    term: String,
    aliases: Vec<String>,
    /// What the term is, in the user's words: the minutes' glossary takes it first.
    meaning: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Glossary {
    entries: Vec<Entry>,
}

/// One term as `glossary.json` stores it: the format of `technical_glossary.json`.
#[derive(Serialize)]
struct StoredEntry<'a> {
    aliases: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    meaning: Option<&'a str>,
}

fn same(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl Glossary {
    /// The formats of [`parse_glossary_entries`].
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut glossary = Self::default();
        for (term, aliases) in parse_glossary_entries(text)? {
            glossary.learn(&term, None);
            for alias in aliases {
                glossary.learn(&term, Some(&alias));
            }
        }
        for (term, meaning) in stored_meanings(text) {
            glossary.explain(&term, &meaning);
        }
        Ok(glossary)
    }

    /// The glossary at `path`; a missing file is an empty glossary, a broken one an error.
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Writes the whole file at once, so a reader never sees half of it.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, self.to_json())
            .and_then(|()| std::fs::rename(&tmp, path))
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Terms sorted, so the file only changes when the vocabulary does.
    pub fn to_json(&self) -> String {
        let object: BTreeMap<&str, StoredEntry<'_>> = self
            .entries
            .iter()
            .map(|e| {
                (
                    e.term.as_str(),
                    StoredEntry {
                        aliases: &e.aliases,
                        meaning: e.meaning.as_deref(),
                    },
                )
            })
            .collect();
        serde_json::to_string_pretty(&object).expect("a glossary serialises") + "\n"
    }

    /// `base` first, then what `user` adds. A term both have keeps the spelling of `base`
    /// (a suggestion never rewrites a configured term) and the aliases of both.
    pub fn merged(base: Self, user: &Self) -> Self {
        let mut out = base;
        for entry in &user.entries {
            out.learn(&entry.term, None);
            for alias in &entry.aliases {
                out.learn(&entry.term, Some(alias));
            }
            if let Some(meaning) = &entry.meaning {
                out.explain(&entry.term, meaning);
            }
        }
        out
    }

    /// `correct` becomes a term (an existing one, in any case, keeps its spelling) and `heard` one
    /// of its misspellings. Returns whether anything changed.
    pub fn learn(&mut self, correct: &str, heard: Option<&str>) -> bool {
        let term = flat(correct);
        if term.is_empty() || term.contains('\0') {
            return false;
        }
        let mut changed = false;
        let at = match self.entries.iter().position(|e| same(&e.term, &term)) {
            Some(at) => at,
            None => {
                self.entries.push(Entry {
                    term: term.clone(),
                    aliases: Vec::new(),
                    meaning: None,
                });
                changed = true;
                self.entries.len() - 1
            }
        };
        // A term that differs only in case is the same term: the spelling it already has stays.
        let entry = &mut self.entries[at];
        let alias = heard
            .map(flat)
            .filter(|a| !a.is_empty() && !a.contains('\0'));
        if let Some(alias) = alias
            && !same(&alias, &term)
            && !entry.aliases.iter().any(|a| same(a, &alias))
        {
            entry.aliases.push(alias);
            changed = true;
        }
        changed
    }

    /// Says what `term` is (a new meaning replaces the old one); the term must be there.
    pub fn explain(&mut self, term: &str, meaning: &str) -> bool {
        let meaning = flat(meaning);
        let Some(entry) = self.entries.iter_mut().find(|e| same(&e.term, &flat(term))) else {
            return false;
        };
        if meaning.is_empty() || entry.meaning.as_deref() == Some(meaning.as_str()) {
            return false;
        }
        entry.meaning = Some(meaning);
        true
    }

    /// The terms the user explained, with what they are.
    pub fn meanings(&self) -> Vec<(String, String)> {
        self.entries
            .iter()
            .filter_map(|e| Some((e.term.clone(), e.meaning.clone()?)))
            .collect()
    }

    pub fn remove(&mut self, term: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| !same(&e.term, &flat(term)));
        self.entries.len() < before
    }

    pub fn contains(&self, term: &str) -> bool {
        self.entries.iter().any(|e| same(&e.term, &flat(term)))
    }

    /// Whether `alias` is already a misspelling of `term`.
    pub fn has_alias(&self, term: &str, alias: &str) -> bool {
        self.entries
            .iter()
            .filter(|e| same(&e.term, &flat(term)))
            .any(|e| e.aliases.iter().any(|a| same(a, &flat(alias))))
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The terms only, in order: what the recogniser prompt and the fusion get.
    pub fn terms_first(&self, first: &Self) -> Vec<String> {
        let (own, rest): (Vec<_>, Vec<_>) =
            self.entries.iter().partition(|e| first.contains(&e.term));
        own.into_iter()
            .chain(rest)
            .map(|e| e.term.clone())
            .collect()
    }

    /// The terms only, in order: what the fusion gets.
    pub fn terms(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.term.clone()).collect()
    }

    /// The misspellings of each term that has any.
    pub fn aliases(&self) -> Vec<(String, Vec<String>)> {
        self.entries
            .iter()
            .filter(|e| !e.aliases.is_empty())
            .map(|e| (e.term.clone(), e.aliases.clone()))
            .collect()
    }

    /// Text that changes whenever a term or an alias does, for ids.
    pub fn fingerprint(&self) -> String {
        self.entries
            .iter()
            .map(|e| {
                let meaning = e.meaning.as_deref().unwrap_or_default();
                format!("{}={}:{meaning}", e.term, e.aliases.join(","))
            })
            .collect::<Vec<_>>()
            .join(";")
    }
}

/// The `meaning` of each term of a JSON object glossary; other formats have none.
fn stored_meanings(text: &str) -> Vec<(String, String)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Ok(serde_json::Value::Object(entries)) = serde_json::from_str(text) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|(term, value)| Some((term.clone(), value["meaning"].as_str()?.to_string())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_meaning_is_kept_saved_and_the_user_s_wins_when_merged() {
        let base = glossary(r#"{"SAGA": {"aliases": ["saga"], "meaning": "viejo"}}"#);
        let mut user = Glossary::default();
        user.learn("saga", None);
        assert!(user.explain("SAGA", "Servicio de cuentas"));
        let merged = Glossary::merged(base, &user);
        assert_eq!(
            merged.meanings(),
            [("SAGA".to_string(), "Servicio de cuentas".to_string())]
        );
        let again = glossary(&merged.to_json());
        assert_eq!(again.meanings(), merged.meanings());
        assert!(!Glossary::default().explain("nada", "x"));
    }

    fn glossary(text: &str) -> Glossary {
        Glossary::parse(text).unwrap()
    }

    #[test]
    fn the_user_glossary_extends_the_configured_one_and_its_spelling_wins() {
        let base = glossary(r#"{"Docker": {"aliases": ["dokér"]}, "YAML": {"aliases": []}}"#);
        let user =
            glossary(r#"{"docker": {"aliases": ["doker"]}, "Jarvis": {"aliases": ["yarvis"]}}"#);
        let merged = Glossary::merged(base, &user);
        assert_eq!(merged.terms(), ["Docker", "YAML", "Jarvis"]);
        assert_eq!(
            merged.aliases(),
            [
                (
                    "Docker".to_string(),
                    vec!["dokér".to_string(), "doker".to_string()]
                ),
                ("Jarvis".to_string(), vec!["yarvis".to_string()]),
            ]
        );
    }

    #[test]
    fn learning_a_fix_adds_the_term_and_the_misspelling_once() {
        let mut g = Glossary::default();
        assert!(g.learn("Jarvis", Some("yarvis")));
        assert!(!g.learn("Jarvis", Some("YARVIS")));
        assert!(g.learn("Jarvis", Some("charvis")));
        // The spelling it was first learned with stays.
        assert!(!g.learn("JARVIS", None));
        assert_eq!(g.terms(), ["Jarvis"]);
        assert_eq!(g.aliases()[0].1, ["yarvis", "charvis"]);
        // A misspelling equal to the term, or an empty term, is nothing.
        assert!(!g.learn("JARVIS", Some("jarvis")));
        let user = glossary("jarvis");
        let configured = glossary(
            "Docker
Jarvis
YAML",
        );
        assert_eq!(configured.terms_first(&user), ["Jarvis", "Docker", "YAML"]);
        assert!(!g.learn("  ", Some("x")));
        assert!(g.has_alias("jarvis", "Charvis") && !g.has_alias("jarvis", "otro"));
    }

    #[test]
    fn remove_ignores_case_and_a_missing_term_is_not_an_error() {
        let mut g = glossary("Docker\nYAML");
        assert!(g.remove("docker") && !g.remove("docker"));
        assert_eq!(g.terms(), ["YAML"]);
    }

    #[test]
    fn the_file_round_trips_sorted_and_a_missing_one_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("glossary.json");
        assert!(Glossary::load(&path).unwrap().is_empty());
        let mut g = Glossary::default();
        g.learn("YAML", None);
        g.learn("Docker", Some("dokér"));
        g.save(&path).unwrap();
        let back = Glossary::load(&path).unwrap();
        assert_eq!(back.terms(), ["Docker", "YAML"]);
        assert_eq!(back.aliases(), g.aliases());
        std::fs::write(&path, "{broken").unwrap();
        assert!(Glossary::load(&path).unwrap_err().contains("glossary.json"));
    }

    #[test]
    fn the_fingerprint_changes_with_a_term_or_an_alias() {
        let a = glossary("Docker");
        let mut b = a.clone();
        assert_eq!(a.fingerprint(), b.fingerprint());
        b.learn("Docker", Some("doker"));
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert_eq!(a.terms(), b.terms());
    }
}
