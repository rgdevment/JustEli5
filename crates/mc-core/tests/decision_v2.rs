//! The V2 decision labels (`seen_committed`, `amend`, `amended`): display,
//! parse round trip, and the strictness of `FromStr`, which only accepts the
//! canonical labels.

use std::collections::HashSet;
use std::str::FromStr;

use mc_core::types::{Decision, UnknownDecision};

const V2_LABELS: [(Decision, &str); 3] = [
    (Decision::SeenCommitted, "seen_committed"),
    (Decision::Amend, "amend"),
    (Decision::Amended, "amended"),
];

fn every_variant() -> Vec<Decision> {
    vec![
        Decision::EmptyText,
        Decision::NoAlnum,
        Decision::NewOpen,
        Decision::DupExact,
        Decision::DupNorm,
        Decision::DupNormLonger,
        Decision::Grow,
        Decision::Merge {
            prefix_ratio: 0.5,
            ratio: 0.8,
        },
        Decision::Newline,
        Decision::Committed,
        Decision::DupCommittedSkip,
        Decision::SeenCommitted,
        Decision::Amend,
        Decision::Amended,
    ]
}

#[test]
fn v2_labels_display_exactly() {
    for (decision, label) in V2_LABELS {
        assert_eq!(decision.to_string(), label, "{decision:?}");
    }
}

#[test]
fn v2_labels_round_trip_through_from_str() {
    for (decision, label) in V2_LABELS {
        let parsed = Decision::from_str(label).expect("canonical label parses");
        assert_eq!(parsed, decision, "{label}");
        assert_eq!(parsed.to_string(), label, "{label} prints back identically");
    }
}

#[test]
fn every_label_is_unique_and_round_trips() {
    let mut seen = HashSet::new();
    for decision in every_variant() {
        let label = decision.to_string();
        assert!(seen.insert(label.clone()), "duplicate label {label}");
        let parsed = Decision::from_str(&label).expect("every label parses");
        assert_eq!(parsed.to_string(), label);
    }
}

#[test]
fn v2_variants_are_distinct_from_each_other_and_from_v1_labels() {
    assert_ne!(Decision::Amend, Decision::Amended);
    assert_ne!(Decision::Amend, Decision::SeenCommitted);
    assert_ne!(Decision::Amended, Decision::Committed);
    assert_ne!(Decision::SeenCommitted, Decision::DupExact);
    assert_ne!(Decision::Amend, Decision::Grow);
}

#[test]
fn v2_labels_are_strict_about_case_spacing_and_extra_text() {
    let bad = [
        "Amend",
        "AMENDED",
        "Seen_Committed",
        " amend",
        "amend ",
        "amended\n",
        "seen_committed\t",
        "amendx",
        "amend_",
        "amended_",
        "seen",
        "seen-committed",
        "seen committed",
        "committed_seen",
        "amends",
    ];
    for text in bad {
        assert_eq!(
            Decision::from_str(text),
            Err(UnknownDecision(text.to_string())),
            "{text:?} must not parse"
        );
    }
}

#[test]
fn v2_labels_are_not_accepted_as_prefixes_of_longer_labels() {
    // `amend` and `amended` share a prefix; each must still match whole.
    assert_eq!(Decision::from_str("amend"), Ok(Decision::Amend));
    assert_eq!(Decision::from_str("amended"), Ok(Decision::Amended));
    assert!(Decision::from_str("amende").is_err());
    assert!(Decision::from_str("seen_committe").is_err());
}
