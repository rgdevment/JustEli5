//! Data exchanged between the capture sources, the reconciler and the stores.

use std::fmt;
use std::str::FromStr;

use crate::clock::Stamp;
use crate::pytext::fmt2;

/// One caption line as read from the UI, before any cleaning.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RawObservation {
    pub speaker: Option<String>,
    pub text: Option<String>,
    /// Stable identity of the UI node, when the source can provide one.
    pub node_id: Option<Vec<i32>>,
}

/// Which frame a record belongs to (mirrors the Python `frame` argument:
/// absent / `None` / an integer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameRef {
    NoClock,
    OutOfFrame,
    InFrame(u64),
}

/// What the reconciler decided about one observation. `Display` is the exact
/// string Python wrote into `captions.jsonl`, and equality compares those
/// labels: a parsed `merge_p0.67_r0.67` equals the `Merge` it was logged from
/// even though its floats were rounded to two decimals.
#[derive(Debug, Clone)]
pub enum Decision {
    EmptyText,
    NoAlnum,
    NewOpen,
    DupExact,
    DupNorm,
    DupNormLonger,
    Grow,
    Merge {
        prefix_ratio: f64,
        ratio: f64,
    },
    Newline,
    Committed,
    DupCommittedSkip,
    /// V2: the line was seen again after being committed.
    SeenCommitted,
    /// V2: a committed line is being rewritten; it is reopened.
    Amend,
    /// V2: the reopened line was committed again, in place.
    Amended,
}

impl PartialEq for Decision {
    fn eq(&self, other: &Self) -> bool {
        self.to_string() == other.to_string()
    }
}

impl fmt::Display for Decision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyText => f.write_str("empty_text"),
            Self::NoAlnum => f.write_str("no_alnum"),
            Self::NewOpen => f.write_str("new_open"),
            Self::DupExact => f.write_str("dup_exact"),
            Self::DupNorm => f.write_str("dup_norm"),
            Self::DupNormLonger => f.write_str("dup_norm_longer"),
            Self::Grow => f.write_str("grow"),
            Self::Merge {
                prefix_ratio,
                ratio,
            } => write!(f, "merge_p{}_r{}", fmt2(*prefix_ratio), fmt2(*ratio)),
            Self::Newline => f.write_str("newline"),
            Self::Committed => f.write_str("committed"),
            Self::DupCommittedSkip => f.write_str("dup_committed_skip"),
            Self::SeenCommitted => f.write_str("seen_committed"),
            Self::Amend => f.write_str("amend"),
            Self::Amended => f.write_str("amended"),
        }
    }
}

/// A decision string that is not one the Python code ever wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownDecision(pub String);

impl fmt::Display for UnknownDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown decision {:?}", self.0)
    }
}

impl std::error::Error for UnknownDecision {}

impl FromStr for Decision {
    type Err = UnknownDecision;

    /// Parses a decision label. `merge_p*_r*` yields the two-decimal values
    /// that were logged. Only canonical labels are accepted (the ones Python
    /// writes): anything that does not print back identically, such as
    /// `merge_p0.5_r1` or `merge_pnan_r1`, is rejected.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let decision = Self::parse_label(s)?;
        if decision.to_string() == s {
            Ok(decision)
        } else {
            Err(UnknownDecision(s.to_string()))
        }
    }
}

impl Decision {
    fn parse_label(s: &str) -> Result<Self, UnknownDecision> {
        Ok(match s {
            "empty_text" => Self::EmptyText,
            "no_alnum" => Self::NoAlnum,
            "new_open" => Self::NewOpen,
            "dup_exact" => Self::DupExact,
            "dup_norm" => Self::DupNorm,
            "dup_norm_longer" => Self::DupNormLonger,
            "grow" => Self::Grow,
            "newline" => Self::Newline,
            "committed" => Self::Committed,
            "dup_committed_skip" => Self::DupCommittedSkip,
            "seen_committed" => Self::SeenCommitted,
            "amend" => Self::Amend,
            "amended" => Self::Amended,
            other => {
                let rest = other
                    .strip_prefix("merge_p")
                    .ok_or_else(|| UnknownDecision(other.to_string()))?;
                let (p, r) = rest
                    .split_once("_r")
                    .ok_or_else(|| UnknownDecision(other.to_string()))?;
                let parse = |v: &str| {
                    v.parse::<f64>()
                        .map_err(|_| UnknownDecision(other.to_string()))
                };
                Self::Merge {
                    prefix_ratio: parse(p)?,
                    ratio: parse(r)?,
                }
            }
        })
    }
}

/// One line of the raw caption log, as the reconciler emits it.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub speaker: String,
    /// `None` only for `empty_text` when the source gave no text at all (Python
    /// logs `null` there, and an empty string when the node text was `""`).
    pub text: Option<String>,
    pub decision: Decision,
    pub speaker_raw: Option<String>,
    pub stamp: Option<Stamp>,
    pub frame: FrameRef,
    pub read_ms: Option<f64>,
    /// `(first_mono_ms, last_mono_ms)` of a committed line.
    pub span: Option<(f64, f64)>,
}

/// A committed caption line with its position on the timeline.
///
/// `speaker_raw` is the unfiltered raw name (it may equal `speaker`, unlike the
/// record's `speaker_raw`, which is only set when it differs). The
/// `committed_*` stamps come from the matching `committed` record, so they are
/// `None` when the reconciler runs without a recorder.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub speaker: String,
    pub speaker_raw: Option<String>,
    pub text: String,
    pub first_mono_ms: Option<f64>,
    pub last_mono_ms: Option<f64>,
    pub committed_mono_ms: Option<f64>,
    pub committed_wall_ms: Option<i64>,
}

impl Segment {
    /// The transcript line Python wrote for this segment: `[speaker]: text`.
    pub fn line(&self) -> String {
        format!("[{}]: {}", self.speaker, self.text)
    }
}
