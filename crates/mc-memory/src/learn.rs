//! A meeting's minutes, taken into the memory: what is known gains a meeting, what is new comes
//! in proposed, and a form the context cannot settle is reported instead of guessed.

use serde::Serialize;

use crate::error::MemoryError;
use crate::model::{Kind, Sighting, State};
use crate::resolve::{Context, Resolution};
use crate::store::Memory;

#[derive(Debug, Clone, Default)]
pub struct Lesson {
    pub session: String,
    pub series: Option<String>,
    pub at_ms: i64,
    pub people: Vec<Met>,
    /// (term, what it is).
    pub terms: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct Met {
    pub name: String,
    pub also: Vec<String>,
    pub role: Option<String>,
    pub attended: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Learnt {
    pub added: Vec<i64>,
    pub seen: Vec<i64>,
    /// The forms that name more than one entry here: the user tells which.
    pub ambiguous: Vec<String>,
}

impl Memory {
    /// People first, so the terms are told apart by who was there too.
    pub fn learn(&mut self, lesson: &Lesson) -> Result<Learnt, MemoryError> {
        let mut learnt = Learnt::default();
        let mut context = Context {
            series: lesson.series.clone(),
            present: Vec::new(),
        };
        for met in &lesson.people {
            let Some(id) =
                self.take(Kind::Person, &met.name, None, lesson, &context, &mut learnt)?
            else {
                continue;
            };
            for form in &met.also {
                self.add_form(id, form, State::Proposed)?;
            }
            if let Some(role) = &met.role {
                self.propose_role(id, role, lesson.at_ms)?;
            }
            self.record(
                id,
                &Sighting {
                    session: lesson.session.clone(),
                    series: lesson.series.clone(),
                    at_ms: lesson.at_ms,
                    attended: met.attended,
                },
            )?;
            if met.attended {
                context.present.push(id);
            }
        }
        for (term, meaning) in &lesson.terms {
            // A name the model put among the terms is a person the memory already knows.
            if !self.candidates(Kind::Person, term)?.is_empty() {
                continue;
            }
            if let Some(id) = self.take(
                Kind::Term,
                term,
                Some(meaning),
                lesson,
                &context,
                &mut learnt,
            )? {
                self.record(
                    id,
                    &Sighting {
                        session: lesson.session.clone(),
                        series: lesson.series.clone(),
                        at_ms: lesson.at_ms,
                        attended: true,
                    },
                )?;
                context.present.push(id);
            }
        }
        Ok(learnt)
    }

    /// The entry `text` names here, made when there is none; `None` when it is ambiguous.
    fn take(
        &mut self,
        kind: Kind,
        text: &str,
        meaning: Option<&str>,
        lesson: &Lesson,
        context: &Context,
        learnt: &mut Learnt,
    ) -> Result<Option<i64>, MemoryError> {
        match self.resolve(kind, text, context)? {
            Resolution::Known { id } => {
                let known = self.get(id)?;
                // What the model says never replaces what the user wrote.
                if known.meaning.is_none() && known.state == State::Proposed {
                    self.set_meaning(id, meaning, lesson.at_ms)?;
                }
                learnt.seen.push(id);
                Ok(Some(id))
            }
            Resolution::Unknown => {
                let id = self.add(kind, text, meaning, State::Proposed, lesson.at_ms)?;
                learnt.added.push(id);
                Ok(Some(id))
            }
            Resolution::Ambiguous { .. } => {
                learnt.ambiguous.push(text.to_string());
                Ok(None)
            }
        }
    }

    /// A role the minutes found, kept only when it is news: the same role again changes nothing.
    fn propose_role(&mut self, id: i64, role: &str, at_ms: i64) -> Result<(), MemoryError> {
        let person = self.get(id)?;
        let same = |known: &str| mc_core::fold::fold(known) == mc_core::fold::fold(role);
        if person.role().is_some_and(same) || person.roles.iter().any(|r| same(&r.role)) {
            return Ok(());
        }
        self.set_role(id, role, at_ms, State::Proposed)
    }
}
