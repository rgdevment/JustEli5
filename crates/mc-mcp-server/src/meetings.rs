//! The sessions under the corpus folder, read straight from their files so a capture that
//! is still running (and not indexed yet) is visible too.

use std::path::{Path, PathBuf};
use std::time::Duration;

use mc_analyst::candidates;
use mc_engine::corpus::is_capturing;
use mc_store::manifest::{MANIFEST_SCHEMA, Manifest};
use serde_json::{Value, json};

use crate::series::SeriesIndex;
use crate::transcript::{Line, SegmentCache, Source};

/// A session this old (or older) is no longer taking notes.
const RECENT_MS: i64 = 24 * 60 * 60 * 1000;
/// A capture rewrites `manifest.json` in place; a read can land in the middle of it.
const MANIFEST_TRIES: u32 = 3;
const MANIFEST_PAUSE: Duration = Duration::from_millis(20);

pub struct Meeting {
    pub dir: PathBuf,
    pub manifest: Manifest,
}

/// An analysis folder: which transcript its ids belong to.
pub struct Analysis {
    /// Folder name, or `live`.
    pub id: String,
    pub transcript_source: Option<String>,
    pub ledger: Value,
    complete: bool,
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

impl Meeting {
    pub fn id(&self) -> &str {
        &self.manifest.session_id
    }

    pub fn dir_name(&self) -> String {
        self.dir
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    }

    pub fn is_live(&self) -> bool {
        !self.manifest.ended_cleanly() && is_capturing(&self.dir)
    }

    /// Live, or started/ended within the last day.
    pub fn is_recent(&self, now_ms: i64) -> bool {
        let last = self
            .manifest
            .ended_wall_ms
            .unwrap_or(self.manifest.started_wall_ms);
        self.is_live() || now_ms - last < RECENT_MS
    }

    fn analysis(&self, name: &str) -> Option<Analysis> {
        let dir = self.dir.join("analysis").join(name);
        let meta = read_json(&dir.join("meta.json")).unwrap_or(Value::Null);
        Some(Analysis {
            id: name.to_string(),
            transcript_source: meta["source"].as_str().map(str::to_string),
            ledger: read_json(&dir.join("ledger.json"))?,
            complete: meta["complete"] == json!(true),
        })
    }

    /// The live ledger while capturing; afterwards the newest complete analysis, else the live
    /// ledger left behind, else the newest incomplete one.
    pub fn ledger(&self) -> Option<Analysis> {
        if self.is_live()
            && let Some(live) = self.analysis("live")
        {
            return Some(live);
        }
        let mut all: Vec<Analysis> =
            candidates(&self.dir.join("analysis"), "ledger.json", &["live"])
                .iter()
                .rev()
                .filter_map(|name| self.analysis(name))
                .collect();
        if let Some(at) = all.iter().position(|a| a.complete) {
            return Some(all.swap_remove(at));
        }
        self.analysis("live").or_else(|| all.into_iter().next())
    }

    pub fn has_ledger(&self) -> bool {
        !candidates(&self.dir.join("analysis"), "ledger.json", &[]).is_empty()
    }

    /// The newest `minutes/<id>` folder that holds `file`: `(id, folder)`.
    fn minutes_folder(&self, file: &str) -> Option<(String, PathBuf)> {
        let id = candidates(&self.dir.join("minutes"), file, &[]).pop()?;
        let dir = self.dir.join("minutes").join(&id);
        Some((id, dir))
    }

    pub fn has_minutes(&self) -> bool {
        self.minutes_folder("minutes.md").is_some()
    }

    pub fn has_commitments(&self) -> bool {
        self.minutes_folder("commitments.json").is_some()
    }

    /// The transcript the ids of these minutes belong to, if they say.
    fn minutes_source(dir: &Path) -> Option<String> {
        read_json(&dir.join("meta.json"))?["source"]
            .as_str()
            .map(str::to_string)
    }

    /// `(minutes id, transcript source, markdown)`.
    pub fn minutes(&self) -> Option<(String, Option<String>, String)> {
        let (id, dir) = self.minutes_folder("minutes.md")?;
        let markdown = std::fs::read_to_string(dir.join("minutes.md")).ok()?;
        Some((id, Self::minutes_source(&dir), markdown))
    }

    /// `(minutes id, transcript source, commitments)`.
    pub fn commitments(&self) -> Option<(String, Option<String>, Value)> {
        let (id, dir) = self.minutes_folder("commitments.json")?;
        let records = read_json(&dir.join("commitments.json"))?;
        Some((id, Self::minutes_source(&dir), records))
    }
}

fn load_meeting(dir: PathBuf) -> Option<Meeting> {
    if !dir.join("captions.jsonl").is_file() && !dir.join("manifest.json").is_file() {
        return None;
    }
    for attempt in 0..MANIFEST_TRIES {
        if let Ok(manifest) = Manifest::load(dir.join("manifest.json")) {
            return Some(Meeting { dir, manifest });
        }
        if attempt + 1 < MANIFEST_TRIES {
            std::thread::sleep(MANIFEST_PAUSE);
        }
    }
    // Still unreadable but being written to: a capture, whatever its manifest says right now.
    if !is_capturing(&dir) {
        return None;
    }
    let name = dir.file_name()?.to_string_lossy().into_owned();
    let stub = json!({"schema": MANIFEST_SCHEMA, "session_id": name}).to_string();
    Some(Meeting {
        manifest: Manifest::from_json(&stub).ok()?,
        dir,
    })
}

pub struct Meetings {
    root: PathBuf,
    series: SeriesIndex,
    cache: SegmentCache,
}

impl Meetings {
    pub fn new(root: impl Into<PathBuf>, series: SeriesIndex) -> Self {
        Self {
            root: root.into(),
            series,
            cache: SegmentCache::default(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn series(&self) -> &SeriesIndex {
        &self.series
    }

    /// Every session, newest first.
    pub fn list(&self) -> Vec<Meeting> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut found: Vec<Meeting> = entries
            .flatten()
            .filter_map(|entry| load_meeting(entry.path()))
            .collect();
        found.sort_by_key(|m| std::cmp::Reverse((m.manifest.started_wall_ms, m.dir_name())));
        found
    }

    /// Where the current meeting is in `all` (newest first): the capture in progress, else the latest.
    pub fn current_index(all: &[Meeting]) -> Option<usize> {
        if all.is_empty() {
            return None;
        }
        Some(all.iter().position(Meeting::is_live).unwrap_or(0))
    }

    /// `None` or `"current"` is the current session; otherwise a session id, a unique prefix
    /// of one, or a folder name. The text is only compared with known sessions, never joined
    /// to a path.
    pub fn resolve(&self, selector: Option<&str>) -> Result<Meeting, String> {
        let mut all = self.list();
        let selector = selector.map(str::trim).filter(|s| !s.is_empty());
        let Some(selector) = selector.filter(|s| *s != "current") else {
            let at = Self::current_index(&all).ok_or("there are no meetings yet")?;
            return Ok(all.swap_remove(at));
        };
        if selector.contains(['/', '\\', ':']) || selector.contains("..") {
            return Err(format!("{selector:?} is not a session id"));
        }
        let exact = |m: &Meeting| m.id() == selector || m.dir_name() == selector;
        let hits: Vec<usize> = match all.iter().position(exact) {
            Some(at) => vec![at],
            None => (0..all.len())
                .filter(|&i| all[i].id().starts_with(selector))
                .collect(),
        };
        match hits.as_slice() {
            [] => Err(format!(
                "no meeting matches {selector:?}; use list_meetings"
            )),
            [at] => Ok(all.swap_remove(*at)),
            _ => Err(format!("{selector:?} matches more than one meeting")),
        }
    }

    pub fn lines(&self, meeting: &Meeting, source: &Source) -> Result<Vec<Line>, String> {
        self.cache.lines(meeting, source)
    }
}
