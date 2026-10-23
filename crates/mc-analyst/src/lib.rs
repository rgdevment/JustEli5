//! The meeting analyst: a ledger of topics, decisions and commitments built by a
//! model over the transcript, and Spanish minutes checked against their evidence.

mod analyst;
mod carry;
mod folder;
mod hash;
mod learned;
mod ledger;
mod minutes;
mod narrative;
mod patch;
mod prompt;
mod references;
mod transcript;

pub use analyst::{
    Analysis, Analyst, AnalystConfig, Background, LedgerEvent, Step, analyze, render_events,
    windows,
};
pub use carry::{
    CarriedCommitment, CarriedRecord, CarriedStatus, CarryStatus, inherit, open_after, resolve,
};
pub use folder::{FolderError, candidates, replace_folder, write_folder};
pub use learned::{Learned, LearnedPerson, LearnedTerm};
pub use ledger::*;
pub use minutes::{
    Checked, CommitmentRecord, MINUTES_VERSION, Minutes, MinutesContext, MinutesError, MinutesMeta,
    Verdict, VerifyReport, minutes, verify, verify_carried,
};
pub use patch::{
    Applied, Known, PatchError, Rejection, apply_patch, apply_patch_carried, parse_patch,
};
pub use prompt::{ANALYST_SYSTEM, PROMPT_VERSION, analysis_user, untrusted_block};
pub use transcript::{
    TranscriptError, TranscriptSegment, collapse_repeats, load_fusion, load_run, render_segment,
    render_segments, transcript_hash,
};
