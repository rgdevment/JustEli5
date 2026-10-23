//! The ledger: what the AI has understood so far, every item tied to segment ids.
//! It is derived data, recomputable from the transcript, and never part of the corpus.

use serde::{Deserialize, Serialize};

use crate::carry::CarriedStatus;
use crate::hash::fnv64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TopicStatus {
    #[default]
    Active,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Topic {
    pub id: String,
    pub title: String,
    pub status: TopicStatus,
    /// Start of the earliest cited segment, in meeting milliseconds.
    pub started_at: i64,
    pub evidence: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub inferred: bool,
    pub evidence: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Commitment {
    pub id: String,
    pub who: String,
    pub what: String,
    /// The deadline as said ("el viernes"), never normalised.
    pub due: Option<String>,
    pub confidence: f64,
    #[serde(default)]
    pub inferred: bool,
    pub evidence: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenQuestion {
    pub id: String,
    pub text: String,
    pub evidence: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Risk {
    pub id: String,
    pub text: String,
    pub evidence: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TermFix {
    pub id: String,
    pub heard: String,
    pub correct: String,
    pub context: String,
    pub evidence: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GlossaryCandidate {
    pub id: String,
    pub term: String,
    /// Distinct segments that mention it.
    pub count: u64,
    pub evidence: Vec<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Ledger {
    #[serde(default)]
    pub topics: Vec<Topic>,
    #[serde(default)]
    pub decisions: Vec<Decision>,
    #[serde(default)]
    pub commitments: Vec<Commitment>,
    #[serde(default)]
    pub open_questions: Vec<OpenQuestion>,
    #[serde(default)]
    pub risks: Vec<Risk>,
    #[serde(default)]
    pub term_fixes: Vec<TermFix>,
    #[serde(default)]
    pub glossary_candidates: Vec<GlossaryCandidate>,
    /// What this meeting says about the commitments carried from the previous one.
    #[serde(default)]
    pub carried: Vec<CarriedStatus>,
}

impl Ledger {
    pub fn item_count(&self) -> usize {
        self.topics.len()
            + self.decisions.len()
            + self.commitments.len()
            + self.open_questions.len()
            + self.risks.len()
            + self.term_fixes.len()
            + self.glossary_candidates.len()
            + self.carried.len()
    }
}

/// An item that is new or different since an earlier ledger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangedItem {
    pub kind: &'static str,
    pub id: String,
    pub text: String,
}

fn changed_in<T: PartialEq>(
    kind: &'static str,
    before: &[T],
    after: &[T],
    describe: impl Fn(&T) -> (&str, String),
    out: &mut Vec<ChangedItem>,
) {
    for item in after.iter().filter(|item| !before.contains(item)) {
        let (id, text) = describe(item);
        out.push(ChangedItem {
            kind,
            id: id.to_string(),
            text,
        });
    }
}

impl Ledger {
    /// The items of `self` that `before` does not hold as they are now.
    pub fn changed_since(&self, before: &Ledger) -> Vec<ChangedItem> {
        let mut out = Vec::new();
        changed_in(
            "topic",
            &before.topics,
            &self.topics,
            |i| (&i.id, i.title.clone()),
            &mut out,
        );
        changed_in(
            "decision",
            &before.decisions,
            &self.decisions,
            |i| (&i.id, i.text.clone()),
            &mut out,
        );
        changed_in(
            "commitment",
            &before.commitments,
            &self.commitments,
            |i| (&i.id, format!("{}: {}", i.who, i.what)),
            &mut out,
        );
        changed_in(
            "open_question",
            &before.open_questions,
            &self.open_questions,
            |i| (&i.id, i.text.clone()),
            &mut out,
        );
        changed_in(
            "risk",
            &before.risks,
            &self.risks,
            |i| (&i.id, i.text.clone()),
            &mut out,
        );
        changed_in(
            "term_fix",
            &before.term_fixes,
            &self.term_fixes,
            |i| (&i.id, format!("{} -> {}", i.heard, i.correct)),
            &mut out,
        );
        changed_in(
            "glossary_candidate",
            &before.glossary_candidates,
            &self.glossary_candidates,
            |i| (&i.id, i.term.clone()),
            &mut out,
        );
        changed_in(
            "carried",
            &before.carried,
            &self.carried,
            |i| (&i.source, i.status.label().to_string()),
            &mut out,
        );
        out
    }
}

fn retain_cited<T>(
    items: &mut Vec<T>,
    known: &std::collections::BTreeSet<u64>,
    dropped: &mut Vec<String>,
    parts: impl Fn(&T) -> (&str, &[u64]),
) {
    items.retain(|item| {
        let (id, evidence) = parts(item);
        let valid = !evidence.is_empty() && evidence.iter().all(|e| known.contains(e));
        if !valid {
            dropped.push(id.to_string());
        }
        valid
    });
}

impl Ledger {
    /// Drops every item whose evidence is empty or cites a segment outside `known`;
    /// returns the ids dropped.
    pub fn retain_cited(&mut self, known: &std::collections::BTreeSet<u64>) -> Vec<String> {
        let mut dropped = Vec::new();
        retain_cited(&mut self.topics, known, &mut dropped, |i| {
            (&i.id, &i.evidence)
        });
        retain_cited(&mut self.decisions, known, &mut dropped, |i| {
            (&i.id, &i.evidence)
        });
        retain_cited(&mut self.commitments, known, &mut dropped, |i| {
            (&i.id, &i.evidence)
        });
        retain_cited(&mut self.open_questions, known, &mut dropped, |i| {
            (&i.id, &i.evidence)
        });
        retain_cited(&mut self.risks, known, &mut dropped, |i| {
            (&i.id, &i.evidence)
        });
        retain_cited(&mut self.term_fixes, known, &mut dropped, |i| {
            (&i.id, &i.evidence)
        });
        retain_cited(&mut self.glossary_candidates, known, &mut dropped, |i| {
            (&i.id, &i.evidence)
        });
        retain_cited(&mut self.carried, known, &mut dropped, |i| {
            (&i.source, &i.evidence)
        });
        dropped
    }
}

/// Lowercase words only, so the same sentence with other punctuation or case has the same id.
fn normalise(text: &str) -> String {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    words.join(" ")
}

/// One line: whitespace runs (line breaks included) become single spaces.
pub fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A stable id: the kind prefix and a hash of the normalised parts.
pub fn item_id(kind: char, parts: &[&str]) -> String {
    let text: Vec<String> = parts.iter().map(|p| normalise(p)).collect();
    let hash = fnv64(text.iter().map(String::as_str));
    format!("{kind}{:08x}", hash >> 32)
}

fn unaccented(c: char) -> char {
    match c {
        'á' | 'à' | 'ä' | 'â' => 'a',
        'é' | 'è' | 'ë' | 'ê' => 'e',
        'í' | 'ì' | 'ï' | 'î' => 'i',
        'ó' | 'ò' | 'ö' | 'ô' => 'o',
        'ú' | 'ù' | 'ü' | 'û' => 'u',
        'ñ' => 'n',
        other => other,
    }
}

/// Words that say nothing about which task it is, in Spanish and English.
const FILLER: [&str; 34] = [
    "a", "al", "con", "de", "del", "el", "en", "la", "las", "lo", "los", "para", "por", "que",
    "se", "su", "un", "una", "y", "o", "e", "bien", "the", "a", "an", "and", "of", "to", "for",
    "in", "on", "with", "it", "is",
];

fn content_words(text: &str) -> std::collections::BTreeSet<String> {
    normalise(text)
        .chars()
        .map(unaccented)
        .collect::<String>()
        .split(' ')
        .filter(|w| !w.is_empty() && !FILLER.contains(w))
        .map(str::to_string)
        .collect()
}

/// The same person restating the same task in other words (Dice over the content words);
/// models add a new commitment every time a meeting comes back to one. "Bruno" and
/// "Bruno André Hayes Ruiz" are one person.
pub fn same_commitment(a: (&str, &str), b: (&str, &str)) -> bool {
    if !is_me(a.0, b.0) && !is_me(b.0, a.0) {
        return false;
    }
    let (x, y) = (content_words(a.1), content_words(b.1));
    let total = x.len() + y.len();
    total > 0 && 2 * x.intersection(&y).count() * 10 >= 7 * total
}

/// Whether `who` names the person called `me`: every word of `me` is in `who`, so
/// "Mario Hidalgo" is "Mario Hidalgo Rojas" too, whatever the case or accents.
pub fn is_me(who: &str, me: &str) -> bool {
    let words = |t: &str| -> std::collections::BTreeSet<String> {
        normalise(t)
            .chars()
            .map(unaccented)
            .collect::<String>()
            .split(' ')
            .filter(|w| !w.is_empty())
            .map(str::to_string)
            .collect()
    };
    let mine = words(me);
    !mine.is_empty() && mine.is_subset(&words(who))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_is_known_by_every_word_of_the_configured_name() {
        assert!(is_me("Mario Hidalgo Rojas", "mario hidalgo"));
        assert!(is_me("MARIO  HIDALGO", "Mario Hidalgo"));
        assert!(!is_me("Mario Torres Vidal", "Mario Hidalgo"));
        assert!(!is_me("Mario Hidalgo", "  "));
    }

    #[test]
    fn restated_commitments_of_one_person_are_the_same_and_others_are_not() {
        let valeria = "Valeria Andrea Pinto Klein";
        let bruno = "Bruno André Hayes Ruiz";
        let meeting = "Poner una reunión mañana temprano para revisar el tema de facturación y definir siguientes pasos";
        assert!(same_commitment(
            (valeria, meeting),
            (
                valeria,
                "Poner una reunión mañana temprano para revisar bien el punto de facturación y definir siguientes pasos"
            )
        ));
        assert!(same_commitment(
            (valeria, meeting),
            (
                valeria,
                "Poner una reunión mañana temprano para ver qué sale y definir siguientes pasos"
            )
        ));
        assert!(same_commitment(
            (
                bruno,
                "Confirmar con el equipo de Bogotá el tema de facturación"
            ),
            (
                "bruno andre hayes  ruiz",
                "Confirmar bien con el equipo de Bogotá el tema de facturación"
            )
        ));
        assert!(!same_commitment(
            (
                bruno,
                "Confirmar con el equipo de Bogotá el tema de facturación"
            ),
            (
                bruno,
                "Confirmar bien con el equipo de Bogotá el mapping de impuestos por región con Nimbus Group"
            )
        ));
        assert!(!same_commitment(
            (
                bruno,
                "Reforzar a Rajesh la claridad sobre el estado de Billing Hub para tenerla mañana"
            ),
            (
                bruno,
                "Reforzar a Rajesh la claridad sobre que el impuesto no se calcula automáticamente con las reglas genéricas de facturación, para tenerla en un posible arreglo mañana"
            )
        ));
        assert!(!same_commitment((valeria, meeting), (bruno, meeting)));
        assert!(same_commitment(
            (
                "Bruno",
                "Confirmar con el equipo de Bogotá el tema de facturación"
            ),
            (
                bruno,
                "Confirmar con el equipo de Bogotá el tema de facturación"
            )
        ));
    }

    #[test]
    fn ids_ignore_case_and_punctuation() {
        assert_eq!(
            item_id('d', &["Migrar a Postgres."]),
            item_id('d', &["migrar a  postgres"])
        );
        assert_ne!(item_id('d', &["a"]), item_id('r', &["a"]));
    }
}
