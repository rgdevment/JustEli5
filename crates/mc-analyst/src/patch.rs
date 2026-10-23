//! The model answers with a patch to the ledger. Nothing in it is trusted: every
//! item must cite segments the model was shown, and what fails is dropped and counted.
//!
//! ```json
//! {"add": {"topics": [], "decisions": [], "commitments": [], "open_questions": [],
//!          "risks": [], "term_fixes": [], "glossary_candidates": []},
//!  "update": [{"id": "c1a2b3c4d", "due": "el viernes", "evidence": [12]}],
//!  "close_topics": ["t1a2b3c4d"], "close_questions": ["q1a2b3c4d"],
//!  "carried": [{"source": "<session>#c1a2b3c4d", "status": "done", "evidence": [12]}]}
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::carry::{CarriedCommitment, CarriedStatus, CarryStatus};
use crate::ledger::*;

/// Segment id to start time, for every segment the model has been shown.
pub type Known = BTreeMap<u64, i64>;

#[derive(Debug, thiserror::Error)]
pub enum PatchError {
    #[error("the answer has no JSON object")]
    NoJson,
    #[error("the answer is not a patch: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rejection {
    pub kind: String,
    pub reason: String,
    pub item: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    pub ledger: Ledger,
    pub accepted: usize,
    pub rejected: Vec<Rejection>,
}

pub fn parse_patch(answer: &str) -> Result<Value, PatchError> {
    let json = mc_llm::extract_json(answer).ok_or(PatchError::NoJson)?;
    let value: Value =
        serde_json::from_str(json).map_err(|e| PatchError::Invalid(e.to_string()))?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(PatchError::Invalid("expected a JSON object".into()))
    }
}

/// Pure: returns the new ledger and what was dropped; the input is untouched.
pub fn apply_patch(ledger: &Ledger, patch: &Value, known: &Known) -> Applied {
    apply_patch_carried(ledger, patch, known, &[])
}

/// Like [`apply_patch`], and `carried` are the commitments the model was shown from the
/// previous meeting: only those can be marked as done, still open or dropped.
pub fn apply_patch_carried(
    ledger: &Ledger,
    patch: &Value,
    known: &Known,
    carried: &[CarriedCommitment],
) -> Applied {
    let mut applier = Applier {
        ledger: ledger.clone(),
        known,
        accepted: 0,
        rejected: Vec::new(),
    };
    let add = &patch["add"];
    applier.topics(&add["topics"]);
    applier.decisions(&add["decisions"]);
    applier.commitments(&add["commitments"]);
    applier.questions(&add["open_questions"]);
    applier.risks(&add["risks"]);
    applier.term_fixes(&add["term_fixes"]);
    applier.glossary(&add["glossary_candidates"]);
    for update in items(&patch["update"]) {
        applier.update(update);
    }
    applier.close_topics(&patch["close_topics"]);
    applier.close_questions(&patch["close_questions"]);
    applier.carried(&patch["carried"], carried);
    Applied {
        ledger: applier.ledger,
        accepted: applier.accepted,
        rejected: applier.rejected,
    }
}

fn items(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

fn text(item: &Value, key: &str) -> String {
    flat(item[key].as_str().unwrap_or_default())
}

fn optional_text(item: &Value, key: &str) -> Option<String> {
    Some(text(item, key)).filter(|t| !t.is_empty())
}

/// Accepts `12`, `"12"` and `"#12"`, since models write ids all three ways; anything else is an error.
fn evidence_ids(item: &Value) -> Result<Vec<u64>, String> {
    let entries = match &item["evidence"] {
        Value::Null => &[][..],
        Value::Array(list) => list.as_slice(),
        other => {
            return Err(format!(
                "evidence is not a list: {}",
                short(&other.to_string())
            ));
        }
    };
    let mut ids = Vec::new();
    for entry in entries {
        let id = match entry {
            Value::Number(n) => n.as_u64(),
            Value::String(s) => s.trim().trim_start_matches('#').parse().ok(),
            _ => None,
        };
        ids.push(
            id.ok_or_else(|| format!("unreadable evidence entry {}", short(&entry.to_string())))?,
        );
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

fn union(into: &mut Vec<u64>, more: &[u64]) {
    into.extend_from_slice(more);
    into.sort_unstable();
    into.dedup();
}

fn clamp_confidence(item: &Value) -> f64 {
    match item["confidence"].as_f64() {
        Some(c) if c.is_finite() => c.clamp(0.0, 1.0),
        _ => 0.5,
    }
}

fn short(text: &str) -> String {
    text.chars().take(80).collect()
}

struct Applier<'a> {
    ledger: Ledger,
    known: &'a Known,
    accepted: usize,
    rejected: Vec<Rejection>,
}

impl Applier<'_> {
    fn reject(&mut self, kind: &str, reason: &str, item: &str) {
        self.rejected.push(Rejection {
            kind: kind.to_string(),
            reason: reason.to_string(),
            item: short(item),
        });
    }

    /// The cited ids when every one was shown to the model; `None` (and a
    /// rejection) otherwise. `required`: an item with no evidence is dropped.
    fn evidence(
        &mut self,
        kind: &str,
        label: &str,
        item: &Value,
        required: bool,
    ) -> Option<Vec<u64>> {
        let ids = match evidence_ids(item) {
            Ok(ids) => ids,
            Err(reason) => {
                self.reject(kind, &reason, label);
                return None;
            }
        };
        if required && ids.is_empty() {
            self.reject(kind, "no evidence", label);
            return None;
        }
        if let Some(bad) = ids.iter().find(|id| !self.known.contains_key(id)) {
            self.reject(kind, &format!("unknown segment id {bad}"), label);
            return None;
        }
        Some(ids)
    }

    fn started_at(&self, evidence: &[u64]) -> i64 {
        evidence
            .iter()
            .filter_map(|id| self.known.get(id))
            .copied()
            .min()
            .unwrap_or(0)
    }

    /// Common gate of the add sections: non-empty main text, valid evidence.
    fn gate(&mut self, kind: &str, item: &Value, main: &[&str]) -> Option<Vec<u64>> {
        let label = main
            .iter()
            .map(|k| text(item, k))
            .collect::<Vec<_>>()
            .join(" / ");
        if main.iter().any(|k| text(item, k).is_empty()) {
            self.reject(kind, "empty text", &label);
            return None;
        }
        let evidence = self.evidence(kind, &label, item, true)?;
        self.accepted += 1;
        Some(evidence)
    }

    fn topics(&mut self, list: &Value) {
        for item in items(list) {
            let Some(evidence) = self.gate("topic", item, &["title"]) else {
                continue;
            };
            let title = text(item, "title");
            let id = item_id('t', &[&title]);
            let started = self.started_at(&evidence);
            match self.ledger.topics.iter_mut().find(|t| t.id == id) {
                Some(t) => {
                    union(&mut t.evidence, &evidence);
                    t.started_at = t.started_at.min(started);
                }
                None => self.ledger.topics.push(Topic {
                    id,
                    title,
                    status: TopicStatus::Active,
                    started_at: started,
                    evidence,
                }),
            }
        }
    }

    fn decisions(&mut self, list: &Value) {
        for item in items(list) {
            let Some(evidence) = self.gate("decision", item, &["text"]) else {
                continue;
            };
            let body = text(item, "text");
            let id = item_id('d', &[&body]);
            let inferred = item["inferred"].as_bool().unwrap_or(false);
            match self.ledger.decisions.iter_mut().find(|d| d.id == id) {
                Some(d) => {
                    union(&mut d.evidence, &evidence);
                    d.inferred &= inferred;
                }
                None => self.ledger.decisions.push(Decision {
                    id,
                    text: body,
                    inferred,
                    evidence,
                }),
            }
        }
    }

    fn commitments(&mut self, list: &Value) {
        for item in items(list) {
            let Some(evidence) = self.gate("commitment", item, &["what"]) else {
                continue;
            };
            let what = text(item, "what");
            let who = optional_text(item, "who").unwrap_or_else(|| "Sin asignar".to_string());
            let id = item_id('c', &[&who, &what]);
            let due = optional_text(item, "due");
            let confidence = clamp_confidence(item);
            let inferred = item["inferred"].as_bool().unwrap_or(false);
            let existing = self
                .ledger
                .commitments
                .iter_mut()
                .find(|c| c.id == id || same_commitment((&c.who, &c.what), (&who, &what)));
            match existing {
                Some(c) => {
                    union(&mut c.evidence, &evidence);
                    c.confidence = c.confidence.max(confidence);
                    c.inferred &= inferred;
                    // The id stays; the wording that says more wins, so a restatement sharpens it.
                    if what.chars().count() > c.what.chars().count() {
                        c.what = what;
                    }
                    if c.due.is_none() {
                        c.due = due;
                    }
                }
                None => self.ledger.commitments.push(Commitment {
                    id,
                    who,
                    what,
                    due,
                    confidence,
                    inferred,
                    evidence,
                }),
            }
        }
    }

    fn questions(&mut self, list: &Value) {
        for item in items(list) {
            let Some(evidence) = self.gate("open_question", item, &["text"]) else {
                continue;
            };
            let body = text(item, "text");
            let id = item_id('q', &[&body]);
            match self.ledger.open_questions.iter_mut().find(|q| q.id == id) {
                Some(q) => union(&mut q.evidence, &evidence),
                None => self.ledger.open_questions.push(OpenQuestion {
                    id,
                    text: body,
                    evidence,
                }),
            }
        }
    }

    fn risks(&mut self, list: &Value) {
        for item in items(list) {
            let Some(evidence) = self.gate("risk", item, &["text"]) else {
                continue;
            };
            let body = text(item, "text");
            let id = item_id('r', &[&body]);
            match self.ledger.risks.iter_mut().find(|r| r.id == id) {
                Some(r) => union(&mut r.evidence, &evidence),
                None => self.ledger.risks.push(Risk {
                    id,
                    text: body,
                    evidence,
                }),
            }
        }
    }

    fn term_fixes(&mut self, list: &Value) {
        for item in items(list) {
            let Some(evidence) = self.gate("term_fix", item, &["heard", "correct"]) else {
                continue;
            };
            let (heard, correct) = (text(item, "heard"), text(item, "correct"));
            let id = item_id('x', &[&heard, &correct]);
            match self.ledger.term_fixes.iter_mut().find(|t| t.id == id) {
                Some(t) => union(&mut t.evidence, &evidence),
                None => self.ledger.term_fixes.push(TermFix {
                    id,
                    heard,
                    correct,
                    context: text(item, "context"),
                    evidence,
                }),
            }
        }
    }

    fn glossary(&mut self, list: &Value) {
        for item in items(list) {
            let Some(evidence) = self.gate("glossary_candidate", item, &["term"]) else {
                continue;
            };
            let term = text(item, "term");
            let id = item_id('g', &[&term]);
            match self
                .ledger
                .glossary_candidates
                .iter_mut()
                .find(|g| g.id == id)
            {
                Some(g) => {
                    union(&mut g.evidence, &evidence);
                    g.count = g.evidence.len() as u64;
                }
                None => self.ledger.glossary_candidates.push(GlossaryCandidate {
                    id,
                    term,
                    count: evidence.len() as u64,
                    evidence,
                }),
            }
        }
    }

    fn update(&mut self, item: &Value) {
        let id = text(item, "id");
        let Some(more) = self.evidence("update", &id, item, false) else {
            return;
        };
        if ["title", "text", "what", "who"]
            .iter()
            .any(|key| optional_text(item, key).is_some())
        {
            // The id comes from the text: a changed text is a different item.
            self.reject(
                "update",
                "text cannot change in an update: add a new item",
                &id,
            );
            return;
        }
        let inferred = item["inferred"].as_bool();
        let found = match id.chars().next() {
            Some('t') => self.ledger.topics.iter_mut().find(|t| t.id == id).map(|t| {
                match item["status"].as_str() {
                    Some("closed") => t.status = TopicStatus::Closed,
                    Some("active") => t.status = TopicStatus::Active,
                    _ => {}
                }
                union(&mut t.evidence, &more);
            }),
            Some('d') => self
                .ledger
                .decisions
                .iter_mut()
                .find(|d| d.id == id)
                .map(|d| {
                    d.inferred = inferred.unwrap_or(d.inferred);
                    union(&mut d.evidence, &more);
                }),
            Some('c') => self
                .ledger
                .commitments
                .iter_mut()
                .find(|c| c.id == id)
                .map(|c| {
                    c.due = optional_text(item, "due").or(c.due.take());
                    if item["confidence"].is_number() {
                        c.confidence = clamp_confidence(item);
                    }
                    c.inferred = inferred.unwrap_or(c.inferred);
                    union(&mut c.evidence, &more);
                }),
            Some('q') => self
                .ledger
                .open_questions
                .iter_mut()
                .find(|q| q.id == id)
                .map(|q| {
                    union(&mut q.evidence, &more);
                }),
            Some('r') => self.ledger.risks.iter_mut().find(|r| r.id == id).map(|r| {
                union(&mut r.evidence, &more);
            }),
            _ => None,
        };
        match found {
            Some(()) => self.accepted += 1,
            None => self.reject("update", "unknown item id", &id),
        }
    }

    fn close_topics(&mut self, list: &Value) {
        for entry in items(list).iter().filter_map(Value::as_str) {
            let by_title = item_id('t', &[entry]);
            let topic = self
                .ledger
                .topics
                .iter_mut()
                .find(|t| t.id == entry.trim() || t.id == by_title);
            match topic {
                Some(t) => {
                    t.status = TopicStatus::Closed;
                    self.accepted += 1;
                }
                None => self.reject("close_topic", "unknown topic", entry),
            }
        }
    }

    fn close_questions(&mut self, list: &Value) {
        for entry in items(list).iter().filter_map(Value::as_str) {
            let before = self.ledger.open_questions.len();
            self.ledger.open_questions.retain(|q| q.id != entry.trim());
            if self.ledger.open_questions.len() < before {
                self.accepted += 1;
            } else {
                self.reject("close_question", "unknown question", entry);
            }
        }
    }

    fn carried(&mut self, list: &Value, shown: &[CarriedCommitment]) {
        for item in items(list) {
            let source = text(item, "source");
            if !shown.iter().any(|c| c.source == source) {
                self.reject("carried", "not a carried commitment", &source);
                continue;
            }
            let Some(status) = item["status"].as_str().and_then(CarryStatus::parse) else {
                self.reject(
                    "carried",
                    "status is not done, still_open or dropped",
                    &source,
                );
                continue;
            };
            // Without evidence from this meeting the commitment stays unresolved.
            let Some(evidence) = self.evidence("carried", &source, item, true) else {
                continue;
            };
            self.accepted += 1;
            match self.ledger.carried.iter_mut().find(|c| c.source == source) {
                Some(c) => {
                    c.status = status;
                    union(&mut c.evidence, &evidence);
                }
                None => self.ledger.carried.push(CarriedStatus {
                    source,
                    status,
                    evidence,
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn known() -> Known {
        [(1, 1000), (2, 5000), (3, 9000)].into()
    }

    fn apply(ledger: &Ledger, patch: Value) -> Applied {
        apply_patch(ledger, &patch, &known())
    }

    #[test]
    fn parses_an_answer_wrapped_in_prose_and_fences() {
        let patch = parse_patch("Claro:\n```json\n{\"add\": {}}\n```").unwrap();
        assert!(patch["add"].is_object());
        assert!(matches!(parse_patch("sin json"), Err(PatchError::NoJson)));
        assert!(matches!(parse_patch("[1, 2]"), Err(PatchError::Invalid(_))));
    }

    #[test]
    fn accepts_items_with_known_evidence_in_any_id_spelling() {
        let applied = apply(
            &Ledger::default(),
            json!({"add": {
                "decisions": [{"text": "Migrar a Postgres", "evidence": [1, "#2", "3"]}],
                "topics": [{"title": "Migración", "evidence": [2, 3]}],
            }}),
        );
        assert!(applied.rejected.is_empty(), "{:?}", applied.rejected);
        assert_eq!(applied.ledger.decisions[0].evidence, [1, 2, 3]);
        assert_eq!(applied.ledger.topics[0].started_at, 5000);
        assert_eq!(applied.accepted, 2);
    }

    #[test]
    fn drops_unknown_ids_missing_evidence_and_empty_text() {
        let applied = apply(
            &Ledger::default(),
            json!({"add": {
                "decisions": [
                    {"text": "ok", "evidence": [1, 99]},
                    {"text": "sin evidencia", "evidence": []},
                    {"text": "  ", "evidence": [1]},
                ],
                "risks": [{"text": "bueno", "evidence": [1]}],
            }}),
        );
        assert!(applied.ledger.decisions.is_empty());
        assert_eq!(applied.ledger.risks.len(), 1);
        let reasons: Vec<&str> = applied.rejected.iter().map(|r| r.reason.as_str()).collect();
        assert_eq!(
            reasons,
            ["unknown segment id 99", "no evidence", "empty text"]
        );
    }

    #[test]
    fn clamps_confidence_and_defaults_the_owner() {
        let applied = apply(
            &Ledger::default(),
            json!({"add": {"commitments": [
                {"who": "Ana", "what": "cerrar el bug", "due": "el martes", "confidence": 4, "evidence": [1]},
                {"what": "enviar el informe", "confidence": -1, "evidence": [2]},
                {"who": "Luis", "what": "revisar", "evidence": [3]},
            ]}}),
        );
        let c = &applied.ledger.commitments;
        assert_eq!(
            (c[0].confidence, c[0].due.as_deref()),
            (1.0, Some("el martes"))
        );
        assert_eq!((c[1].who.as_str(), c[1].confidence), ("Sin asignar", 0.0));
        assert_eq!(c[2].confidence, 0.5);
    }

    #[test]
    fn applying_twice_changes_nothing() {
        let patch = json!({"add": {
            "commitments": [{"who": "Ana", "what": "Cerrar el bug", "evidence": [1], "inferred": true}],
            "glossary_candidates": [{"term": "Chakra", "evidence": [2]}],
        }});
        let once = apply(&Ledger::default(), patch.clone()).ledger;
        let twice = apply(&once, patch).ledger;
        assert_eq!(once, twice);
        assert_eq!(once.glossary_candidates[0].count, 1);
    }

    #[test]
    fn a_later_explicit_mention_clears_inferred_and_merges_evidence() {
        let first = apply(
            &Ledger::default(),
            json!({"add": {"decisions": [{"text": "Usar Redis", "evidence": [1], "inferred": true}]}}),
        );
        let second = apply(
            &first.ledger,
            json!({"add": {"decisions": [{"text": "usar redis.", "evidence": [3], "inferred": false}]}}),
        );
        let d = &second.ledger.decisions;
        assert_eq!(d.len(), 1);
        assert_eq!((d[0].inferred, d[0].evidence.clone()), (false, vec![1, 3]));
    }

    #[test]
    fn updates_cannot_change_text_and_unreadable_evidence_is_rejected() {
        let base = apply(
            &Ledger::default(),
            json!({"add": {"decisions": [{"text": "Usar Rust", "evidence": [1]}]}}),
        )
        .ledger;
        let id = base.decisions[0].id.clone();
        let applied = apply(
            &base,
            json!({
                "update": [{"id": id, "text": "Usar Go", "evidence": [2]}],
                "add": {"risks": [
                    {"text": "a", "evidence": [1, "x"]},
                    {"text": "b", "evidence": "1"},
                    {"text": "c", "evidence": [1.5]},
                ]},
            }),
        );
        assert_eq!(applied.ledger.decisions[0].text, "Usar Rust");
        assert!(applied.ledger.risks.is_empty());
        assert_eq!(applied.rejected.len(), 4, "{:?}", applied.rejected);
    }

    #[test]
    fn line_breaks_in_text_are_flattened() {
        let applied = apply(
            &Ledger::default(),
            json!({"add": {"risks": [{"text": "uno\n## Falso\n- [ ] @x: y", "evidence": [1]}]}}),
        );
        assert_eq!(applied.ledger.risks[0].text, "uno ## Falso - [ ] @x: y");
    }

    #[test]
    fn updates_need_a_known_item_and_known_evidence() {
        let base = apply(
            &Ledger::default(),
            json!({"add": {"commitments": [{"who": "Ana", "what": "cerrar", "evidence": [1]}]}}),
        )
        .ledger;
        let id = base.commitments[0].id.clone();
        let applied = apply(
            &base,
            json!({"update": [
                {"id": id, "due": "el viernes", "evidence": [2]},
                {"id": id, "due": "x", "evidence": [42]},
                {"id": "c00000000", "due": "x"},
            ]}),
        );
        assert_eq!(
            applied.ledger.commitments[0].due.as_deref(),
            Some("el viernes")
        );
        assert_eq!(applied.ledger.commitments[0].evidence, [1, 2]);
        assert_eq!(applied.rejected.len(), 2);
    }

    #[test]
    fn closes_topics_by_id_or_title_and_questions_by_id() {
        let base = apply(
            &Ledger::default(),
            json!({"add": {
                "topics": [{"title": "Demo", "evidence": [1]}],
                "open_questions": [{"text": "¿Quién despliega?", "evidence": [2]}],
            }}),
        )
        .ledger;
        let qid = base.open_questions[0].id.clone();
        let applied = apply(
            &base,
            json!({"close_topics": ["demo", "nada"], "close_questions": [qid]}),
        );
        assert_eq!(applied.ledger.topics[0].status, TopicStatus::Closed);
        assert!(applied.ledger.open_questions.is_empty());
        assert_eq!(applied.rejected.len(), 1);
    }

    #[test]
    fn malformed_sections_are_ignored_not_fatal() {
        let applied = apply(
            &Ledger::default(),
            json!({"add": {"decisions": "no es una lista", "risks": [42, {"text": "r", "evidence": [1]}]}}),
        );
        assert_eq!(applied.ledger.risks.len(), 1);
        assert_eq!(applied.rejected.len(), 1);
    }

    #[test]
    fn a_carried_commitment_needs_evidence_a_known_status_and_a_shown_source() {
        let shown = [CarriedCommitment {
            source: "prev#a".into(),
            who: "Ana".into(),
            what: "cerrar".into(),
            due: None,
        }];
        let known: Known = [(0, 0), (1, 10)].into();
        let patch = serde_json::json!({"carried": [
            {"source": "prev#a", "status": "done"},
            {"source": "prev#a", "status": "finished", "evidence": [0]},
            {"source": "prev#zzz", "status": "done", "evidence": [0]},
            {"source": "prev#a", "status": "still_open", "evidence": [1]},
            {"source": "prev#a", "status": "done", "evidence": [0]},
        ]});
        let applied = apply_patch_carried(&Ledger::default(), &patch, &known, &shown);
        let reasons: Vec<&str> = applied.rejected.iter().map(|r| r.reason.as_str()).collect();
        assert_eq!(
            reasons,
            [
                "no evidence",
                "status is not done, still_open or dropped",
                "not a carried commitment"
            ]
        );
        // The later answer replaces the status and the evidence adds up.
        let [one] = applied.ledger.carried.as_slice() else {
            panic!("one status");
        };
        assert_eq!(
            (one.status, one.evidence.as_slice()),
            (CarryStatus::Done, &[0, 1][..])
        );
        let none = apply_patch(&Ledger::default(), &patch, &known);
        assert!(none.ledger.carried.is_empty());
    }
}
