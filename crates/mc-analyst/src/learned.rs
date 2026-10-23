//! What a meeting's minutes teach the memory: the terms they explained and the people in them,
//! read from the lines the model wrote for the glossary and the participants.

use serde::{Deserialize, Serialize};

use crate::ledger::flat;

const MENTIONED: &str = "Mencionados sin asistir:";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Learned {
    pub terms: Vec<LearnedTerm>,
    pub people: Vec<LearnedPerson>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LearnedTerm {
    pub term: String,
    pub meaning: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LearnedPerson {
    /// As the captions name them, or as the model wrote it for someone only mentioned.
    pub name: String,
    /// Other forms of the name, such as the short one the minutes show.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub attended: bool,
}

/// `- Term: what it is` lines as (term, meaning), without the bold the model may add.
pub(crate) fn term_pairs(terms: &str) -> Vec<(String, String)> {
    terms
        .lines()
        .filter_map(|line| {
            let line = line.trim().trim_start_matches(['-', '*']).trim();
            let (term, what) = line.split_once(':')?;
            let (term, what) = (flat(&term.replace("**", "")), flat(what));
            (!term.is_empty() && !what.is_empty()).then_some((term, what))
        })
        .collect()
}

/// `- Name: role` lines as (name, role), and the «Mencionados sin asistir» line's text.
pub(crate) fn people_lines(people: &str) -> (Vec<(String, String)>, String) {
    let mut roles = Vec::new();
    let mut mentioned = String::new();
    for line in people.lines() {
        let line = line.trim().trim_start_matches(['-', '*']).trim();
        if let Some(rest) = line.strip_prefix(MENTIONED) {
            mentioned = flat(rest);
        } else if let Some((who, role)) = line.split_once(':') {
            roles.push((flat(who).replace("**", ""), flat(role)));
        }
    }
    (roles, mentioned)
}

/// «Ignacia (PO Web), el Flaco (negocio)» as (name, role); a comma inside the
/// parentheses belongs to the role.
pub(crate) fn mentioned_people(text: &str) -> Vec<(String, Option<String>)> {
    let mut items = Vec::new();
    let (mut depth, mut start) = (0usize, 0usize);
    for (i, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                items.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    items.push(&text[start..]);
    items
        .into_iter()
        .filter_map(|item| {
            let item = flat(item.trim_end_matches('.'));
            let (name, role) = match item.split_once('(') {
                Some((name, role)) => (flat(name), Some(flat(role.trim_end_matches(')')))),
                None => (item.clone(), None),
            };
            (!name.is_empty()).then(|| (name, role.filter(|r| !r.is_empty())))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_and_roles_come_out_of_the_models_lines() {
        assert_eq!(
            term_pairs("- **RUT**: Rol Único Tributario\n- vacío:\nsin dos puntos"),
            [("RUT".to_string(), "Rol Único Tributario".to_string())]
        );
        let (roles, mentioned) =
            people_lines("- Raul: Product Owner\nMencionados sin asistir: Ignacia (PO Web)");
        assert_eq!(roles, [("Raul".to_string(), "Product Owner".to_string())]);
        assert_eq!(mentioned, "Ignacia (PO Web)");
    }

    #[test]
    fn mentioned_people_keep_a_comma_inside_their_role() {
        assert_eq!(
            mentioned_people("Ignacia (PO Web, pagos), el Flaco (negocio), Luis."),
            [
                ("Ignacia".to_string(), Some("PO Web, pagos".to_string())),
                ("el Flaco".to_string(), Some("negocio".to_string())),
                ("Luis".to_string(), None),
            ]
        );
        assert!(mentioned_people("").is_empty());
    }
}
