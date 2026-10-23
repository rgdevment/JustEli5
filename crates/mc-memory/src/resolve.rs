//! Which concept a form means here. One candidate is the answer; among homonyms the one the
//! meeting's context points to, and only when it clearly does: a guess is worse than asking.

use serde::Serialize;

/// What is known about the meeting a form came up in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Context {
    pub series: Option<String>,
    /// Concepts already known in this meeting: the people present and the terms resolved so far.
    pub present: Vec<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Candidate {
    pub id: i64,
    /// Twice the meetings of this series it came up in, plus the meetings it shared with `present`.
    pub score: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "resolution", rename_all = "lowercase")]
pub enum Resolution {
    Unknown,
    Known {
        id: i64,
    },
    /// Best first.
    Ambiguous {
        candidates: Vec<Candidate>,
    },
}

/// The leader wins when it has context behind it and at least twice the runner-up's.
pub(crate) fn decide(mut candidates: Vec<Candidate>) -> Resolution {
    candidates.sort_by(|a, b| b.score.cmp(&a.score).then(a.id.cmp(&b.id)));
    match candidates.as_slice() {
        [] => Resolution::Unknown,
        [only] => Resolution::Known { id: only.id },
        [first, second, ..] if first.score > 0 && first.score >= second.score * 2 => {
            Resolution::Known { id: first.id }
        }
        _ => Resolution::Ambiguous { candidates },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scored(pairs: &[(i64, u32)]) -> Vec<Candidate> {
        pairs
            .iter()
            .map(|&(id, score)| Candidate { id, score })
            .collect()
    }

    #[test]
    fn one_candidate_needs_no_context() {
        assert_eq!(decide(scored(&[(7, 0)])), Resolution::Known { id: 7 });
        assert_eq!(decide(Vec::new()), Resolution::Unknown);
    }

    #[test]
    fn a_homonym_is_chosen_only_when_the_context_clearly_points_to_it() {
        assert_eq!(
            decide(scored(&[(1, 1), (2, 4)])),
            Resolution::Known { id: 2 }
        );
        assert_eq!(
            decide(scored(&[(1, 3), (2, 4)])),
            Resolution::Ambiguous {
                candidates: scored(&[(2, 4), (1, 3)])
            }
        );
        assert!(matches!(
            decide(scored(&[(1, 0), (2, 0)])),
            Resolution::Ambiguous { .. }
        ));
    }
}
