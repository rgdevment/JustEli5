//! What the memory holds: concepts (a term or a person) apart from the forms they are written or
//! heard in, so one form can name two concepts and one concept can have many forms.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Term,
    Person,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Term => "term",
            Self::Person => "person",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "term" => Some(Self::Term),
            "person" => Some(Self::Person),
            _ => None,
        }
    }
}

/// Proposed by the model or the minutes; confirmed by the user, and then the model never changes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Proposed,
    Confirmed,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Confirmed => "confirmed",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "proposed" => Some(Self::Proposed),
            "confirmed" => Some(Self::Confirmed),
            _ => None,
        }
    }
}

/// One way a concept shows up: as Teams writes it, as Whisper hears it, an acronym, a synonym.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Form {
    pub text: String,
    pub state: State,
}

/// What a person does, from a day on: the newest one is the current role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Role {
    pub role: String,
    pub since_ms: i64,
    pub state: State,
}

/// A meeting a concept came up in; for a person, `attended` is false when they were only mentioned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sighting {
    pub session: String,
    pub series: Option<String>,
    pub at_ms: i64,
    pub attended: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Concept {
    pub id: i64,
    pub kind: Kind,
    /// The canonical form; it is also one of `forms`.
    pub name: String,
    /// For a term what it is; for a person a free note.
    pub meaning: Option<String>,
    pub state: State,
    pub forms: Vec<Form>,
    /// Newest first; only people have them.
    pub roles: Vec<Role>,
    pub meetings: usize,
}

impl Concept {
    /// The newest role the user confirmed, or else the newest one proposed.
    pub fn role(&self) -> Option<&str> {
        self.roles
            .iter()
            .find(|r| r.state == State::Confirmed)
            .or_else(|| self.roles.first())
            .map(|r| r.role.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role(role: &str, since_ms: i64, state: State) -> Role {
        Role {
            role: role.to_string(),
            since_ms,
            state,
        }
    }

    #[test]
    fn a_confirmed_role_wins_over_a_newer_proposed_one() {
        let mut person = Concept {
            id: 1,
            kind: Kind::Person,
            name: "Raul Medina".to_string(),
            meaning: None,
            state: State::Confirmed,
            forms: Vec::new(),
            roles: vec![
                role("PM", 2, State::Proposed),
                role("PO", 1, State::Confirmed),
            ],
            meetings: 0,
        };
        assert_eq!(person.role(), Some("PO"));
        person.roles = vec![role("PM", 2, State::Proposed)];
        assert_eq!(person.role(), Some("PM"));
    }
}
