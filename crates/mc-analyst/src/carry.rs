//! Commitments carried from the previous meeting of a series, and what this meeting says about them.

use serde::{Deserialize, Serialize};

use crate::minutes::CommitmentRecord;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CarryStatus {
    Done,
    StillOpen,
    Dropped,
}

impl CarryStatus {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_lowercase().as_str() {
            "done" => Some(Self::Done),
            "still_open" => Some(Self::StillOpen),
            "dropped" => Some(Self::Dropped),
            _ => None,
        }
    }

    /// How the minutes say it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Done => "hecho",
            Self::StillOpen => "sigue abierto",
            Self::Dropped => "descartado",
        }
    }
}

/// A commitment of an earlier meeting that nobody has resolved yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CarriedCommitment {
    /// `<session>#<id>`, as in the earlier meeting's `commitments.json`.
    pub source: String,
    pub who: String,
    pub what: String,
    pub due: Option<String>,
}

/// What this meeting says about one carried commitment, with the segments that say it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CarriedStatus {
    pub source: String,
    pub status: CarryStatus,
    pub evidence: Vec<u64>,
}

/// One line of `carried.json`: a carried commitment and where this meeting left it.
/// No status means the meeting did not say, so it is carried on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CarriedRecord {
    #[serde(flatten)]
    pub commitment: CarriedCommitment,
    pub status: Option<CarryStatus>,
    pub evidence: Vec<u64>,
}

/// Joins what was carried with what the ledger resolved; the order of `carried` is kept.
pub fn resolve(carried: &[CarriedCommitment], statuses: &[CarriedStatus]) -> Vec<CarriedRecord> {
    carried
        .iter()
        .map(|c| {
            let said = statuses.iter().find(|s| s.source == c.source);
            CarriedRecord {
                commitment: c.clone(),
                status: said.map(|s| s.status),
                evidence: said.map(|s| s.evidence.clone()).unwrap_or_default(),
            }
        })
        .collect()
}

/// What the next meeting of the series inherits from a meeting with minutes, given what was
/// open before it (`open`, empty when the chain starts here): the open items that this meeting
/// did not close (a meeting that never heard of one leaves it open), what it carried and left
/// open or unsaid, then what it committed to itself. A carried commitment the meeting said again
/// as its own (same person and task) is inherited once, from the new statement.
pub fn inherit(
    open: &[CarriedCommitment],
    commitments: &[CommitmentRecord],
    carried: &[CarriedRecord],
) -> Vec<CarriedCommitment> {
    let closed = |source: &str| {
        carried.iter().any(|r| {
            r.commitment.source == source
                && matches!(r.status, Some(CarryStatus::Done | CarryStatus::Dropped))
        })
    };
    let said_again = |source: &str| {
        let id = source.rsplit_once('#').map_or(source, |(_, id)| id);
        commitments.iter().any(|c| c.id == id)
    };
    let left_open = carried
        .iter()
        .filter(|r| matches!(r.status, None | Some(CarryStatus::StillOpen)))
        .map(|r| r.commitment.clone());
    let new = commitments.iter().map(|c| CarriedCommitment {
        source: c.source.clone(),
        who: c.who.clone(),
        what: c.what.clone(),
        due: c.due.clone(),
    });
    let mut out: Vec<CarriedCommitment> = Vec::new();
    let older = open
        .iter()
        .filter(|c| !closed(&c.source))
        .cloned()
        .chain(left_open)
        .filter(|c| !said_again(&c.source));
    for item in older.chain(new) {
        if !out.iter().any(|o| o.source == item.source) {
            out.push(item);
        }
    }
    out
}

/// [`inherit`] for a chain that starts at this meeting.
pub fn open_after(
    commitments: &[CommitmentRecord],
    carried: &[CarriedRecord],
) -> Vec<CarriedCommitment> {
    inherit(&[], commitments, carried)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn carried(source: &str) -> CarriedCommitment {
        CarriedCommitment {
            source: source.into(),
            who: "Ana".into(),
            what: format!("tarea {source}"),
            due: None,
        }
    }

    fn record(source: &str) -> CommitmentRecord {
        CommitmentRecord {
            id: "c1".into(),
            who: "Luis".into(),
            what: "otra".into(),
            due: None,
            evidence: vec![1],
            said: Vec::new(),
            inferred: false,
            source: source.into(),
        }
    }

    #[test]
    fn done_and_dropped_are_not_inherited_but_unsaid_and_open_are() {
        let statuses = [
            CarriedStatus {
                source: "s#a".into(),
                status: CarryStatus::Done,
                evidence: vec![2],
            },
            CarriedStatus {
                source: "s#b".into(),
                status: CarryStatus::StillOpen,
                evidence: vec![3],
            },
            CarriedStatus {
                source: "s#c".into(),
                status: CarryStatus::Dropped,
                evidence: vec![4],
            },
        ];
        let carried_in = [
            carried("s#a"),
            carried("s#b"),
            carried("s#c"),
            carried("s#d"),
        ];
        let records = resolve(&carried_in, &statuses);
        assert_eq!(records[3].status, None);
        assert_eq!(records[0].evidence, [2]);
        let next = open_after(&[record("t#n")], &records);
        let sources: Vec<&str> = next.iter().map(|c| c.source.as_str()).collect();
        assert_eq!(sources, ["s#b", "s#d", "t#n"]);
    }

    #[test]
    fn a_source_is_inherited_once() {
        let records = resolve(&[carried("s#a")], &[]);
        let next = open_after(&[record("s#a")], &records);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].who, "Ana");
    }

    #[test]
    fn a_carried_commitment_said_again_is_inherited_once_from_the_new_statement() {
        let records = resolve(&[carried("s#c1"), carried("s#c2")], &[]);
        // `record` has id "c1": the same task, restated by the meeting.
        let next = open_after(&[record("t#c1")], &records);
        let sources: Vec<&str> = next.iter().map(|c| c.source.as_str()).collect();
        assert_eq!(sources, ["s#c2", "t#c1"]);
    }
    #[test]
    fn records_round_trip_with_the_commitment_fields_inline() {
        let records = resolve(&[carried("s#a")], &[]);
        let text = serde_json::to_string(&records).unwrap();
        assert!(text.contains("\"source\":\"s#a\"") && text.contains("\"status\":null"));
        assert_eq!(
            serde_json::from_str::<Vec<CarriedRecord>>(&text).unwrap(),
            records
        );
    }

    #[test]
    fn walking_the_chain_never_drops_what_a_meeting_did_not_know_about() {
        // Meeting 1 committed to "c1"; meeting 2 (minutes from before carry-over, no
        // carried.json) knew nothing of it; meeting 3 closes it.
        let after_one = inherit(&[], &[record("m1#c1")], &[]);
        let after_two = inherit(&after_one, &[], &[]);
        assert_eq!(after_two.len(), 1);
        let closed = resolve(
            &after_two,
            &[CarriedStatus {
                source: "m1#c1".into(),
                status: CarryStatus::Done,
                evidence: vec![1],
            }],
        );
        assert!(inherit(&after_two, &[], &closed).is_empty());
    }
}
