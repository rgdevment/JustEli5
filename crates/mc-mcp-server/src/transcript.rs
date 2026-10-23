//! The transcripts a session can be read as. Ids cited by a ledger, minutes or commitments
//! belong to one of them, so a reader must ask for the same one.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::SystemTime;

use mc_analyst::{TranscriptSegment, load_fusion, load_run};
use mc_core::Segment;
use mc_engine::runs::{FlushMode, RunConfig, build, rel_ms};
use mc_engine::sidecar::{display_name, read_speaker_names};

use crate::meetings::Meeting;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// What the live driver commits; `mc run` with the default settings gives the same ids.
    Live,
    Run(String),
    Fusion(String),
}

impl Source {
    /// `live`, `run:<id>` or `fusion:<id>`; the id is a folder name, never a path.
    pub fn parse(label: &str) -> Result<Self, String> {
        let bad = || format!("{label:?} is not live, run:<id> or fusion:<id>");
        let valid = |id: &str| {
            !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        };
        match label.split_once(':') {
            None if label == "live" => Ok(Self::Live),
            Some(("run", id)) if valid(id) => Ok(Self::Run(id.to_string())),
            Some(("fusion", id)) if valid(id) => Ok(Self::Fusion(id.to_string())),
            _ => Err(bad()),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Live => "live".to_string(),
            Self::Run(id) => format!("run:{id}"),
            Self::Fusion(id) => format!("fusion:{id}"),
        }
    }
}

/// One transcript line as shown to the user; `idx` is the id the analysis cites.
pub struct Line {
    pub idx: u64,
    pub speaker: String,
    pub text: String,
    pub t_ms: Option<f64>,
}

/// File length, modification time and flush mode the cached segments were built from.
type Stamp = (u64, Option<SystemTime>, bool);

type Built = (Stamp, Rc<Vec<Segment>>);

/// Replaying a long captions file is not free, and a client polls: remember the last build.
#[derive(Default)]
pub struct SegmentCache {
    built: RefCell<HashMap<PathBuf, Built>>,
}

impl SegmentCache {
    fn live_segments(&self, meeting: &Meeting) -> Result<Rc<Vec<Segment>>, String> {
        let captions = meeting.dir.join("captions.jsonl");
        let meta =
            std::fs::metadata(&captions).map_err(|e| format!("{}: {e}", captions.display()))?;
        // While capturing the last open line is not committed yet, as in the live driver.
        let capturing = meeting.is_live();
        let stamp = (meta.len(), meta.modified().ok(), capturing);
        if let Some((cached, segments)) = self.built.borrow().get(&meeting.dir)
            && *cached == stamp
        {
            return Ok(Rc::clone(segments));
        }
        let cfg = RunConfig {
            flush: if capturing {
                FlushMode::ManifestOnly
            } else {
                FlushMode::Auto
            },
            // Same ids as `mc run` and the live capture of a browser meeting.
            policy_by_platform: true,
            ..RunConfig::default()
        };
        let segments = Rc::new(
            build(&meeting.dir, &cfg)
                .map_err(|e| e.to_string())?
                .segments,
        );
        self.built
            .borrow_mut()
            .insert(meeting.dir.clone(), (stamp, Rc::clone(&segments)));
        Ok(segments)
    }

    pub fn lines(&self, meeting: &Meeting, source: &Source) -> Result<Vec<Line>, String> {
        let names = read_speaker_names(&meeting.dir);
        let show = |speaker: &str| display_name(&names, speaker).to_string();
        match source {
            Source::Live => Ok(self
                .live_segments(meeting)?
                .iter()
                .enumerate()
                .map(|(i, s)| Line {
                    idx: i as u64,
                    speaker: show(&s.speaker),
                    text: s.text.clone(),
                    t_ms: rel_ms(s),
                })
                .collect()),
            Source::Run(id) => {
                let path = meeting.dir.join("runs").join(id).join("segments.jsonl");
                stored(&path, load_run, show)
            }
            Source::Fusion(id) => {
                let path = meeting.dir.join("fused").join(id).join("fused.jsonl");
                stored(&path, load_fusion, show)
            }
        }
    }
}

fn stored(
    path: &Path,
    load: fn(&Path) -> Result<Vec<TranscriptSegment>, mc_analyst::TranscriptError>,
    show: impl Fn(&str) -> String,
) -> Result<Vec<Line>, String> {
    let segments = load(path).map_err(|e| e.to_string())?;
    Ok(segments
        .into_iter()
        .map(|s| Line {
            idx: s.id,
            speaker: show(&s.speaker),
            text: s.text,
            t_ms: Some(s.start_ms as f64),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_round_trip_and_paths_are_refused() {
        for label in ["live", "run:r0123", "fusion:f-1_a"] {
            assert_eq!(Source::parse(label).unwrap().label(), label);
        }
        for label in ["", "run:", "run:../x", "fusion:a/b", "other:1", "live:1"] {
            assert!(Source::parse(label).is_err(), "{label}");
        }
    }
}
