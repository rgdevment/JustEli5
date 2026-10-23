//! The series of the meetings and what each one leaves open, from the index `mc` keeps.

use std::path::{Path, PathBuf};

use mc_engine::corpus::is_capturing;
use mc_engine::series::{Chain, chain, index_corpus};
use mc_store::manifest::Manifest;
use mc_store::{SeriesRule, Store, series_for_name};

use crate::meetings::{Meeting, Meetings};

/// A capture in progress: its index row would go stale as soon as it was written.
fn is_live(dir: &Path) -> bool {
    let ended = Manifest::load(dir.join("manifest.json")).is_ok_and(|m| m.ended_cleanly());
    !ended && is_capturing(dir)
}

/// Where the series memory lives: the index and the rules that file sessions under a series.
pub struct SeriesIndex {
    pub path: PathBuf,
    pub rules: Vec<SeriesRule>,
}

/// What one series leaves open.
pub struct OpenSeries {
    pub key: String,
    pub chain: Chain,
}

impl SeriesIndex {
    /// The index as it is, without creating or filling it.
    fn existing(&self) -> Option<Store> {
        self.path
            .is_file()
            .then(|| Store::open(&self.path).ok())
            .flatten()
    }

    /// The series of each meeting, in the order given: the index says (a manual choice stands),
    /// else its name does.
    pub fn keys(&self, meetings: &[Meeting]) -> Vec<Option<String>> {
        let store = self.existing();
        meetings
            .iter()
            .map(|m| {
                let indexed = store
                    .as_ref()
                    .and_then(|s| s.series_of(m.id()).ok().flatten());
                indexed.map(|(key, _)| key).or_else(|| {
                    let name = m.manifest.meeting_name.as_deref()?;
                    series_for_name(name, &self.rules).map(|(key, _)| key)
                })
            })
            .collect()
    }

    /// Indexes what the captures saved since (a capture in progress is left alone), then walks
    /// the chain of `key`, or of every series. Why that could not be done goes in `warnings`.
    pub fn open(&self, meetings: &Meetings, key: Option<&str>) -> Result<Vec<OpenSeries>, String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut store = Store::open(&self.path).map_err(|e| e.to_string())?;
        index_corpus(&mut store, meetings.root(), &self.rules, is_live)
            .map_err(|e| e.to_string())?;
        let keys = match key {
            Some(key) => vec![key.to_string()],
            None => store
                .list_series()
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|s| s.key)
                .collect(),
        };
        let mut out = Vec::new();
        for key in keys {
            let members = store.series_sessions(&key).map_err(|e| e.to_string())?;
            out.push(OpenSeries {
                chain: chain(&members, None),
                key,
            });
        }
        Ok(out)
    }
}
