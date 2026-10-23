//! The meetings of a series and what they leave open, read from the index and the minutes on disk.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use mc_analyst::{CarriedCommitment, CarriedRecord, CommitmentRecord, inherit};
use mc_store::{SeriesMember, SeriesRule, Store, StoreError, series_for_name};
use serde::de::DeserializeOwned;

/// The minutes of one session: what it committed to and how it left what it inherited.
pub struct SessionMinutes {
    pub commitments: Vec<CommitmentRecord>,
    pub carried: Vec<CarriedRecord>,
}

fn read_list<T: DeserializeOwned>(path: &Path) -> Option<Vec<T>> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// The newest minutes of a session that were written with their commitments.
fn newest_minutes(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir.join("minutes"))
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let file = entry.path().join("commitments.json");
            let modified = file.metadata().ok()?.modified().ok()?;
            Some((modified, entry.file_name(), entry.path()))
        })
        .max()
        .map(|(_, _, path)| path)
}

/// `None` when the session has no minutes; a minutes folder from before this feature has
/// no `carried.json`, which reads as nothing carried.
pub fn session_minutes(dir: &Path) -> Option<SessionMinutes> {
    let folder = newest_minutes(dir)?;
    Some(SessionMinutes {
        commitments: read_list(&folder.join("commitments.json"))?,
        carried: read_list(&folder.join("carried.json")).unwrap_or_default(),
    })
}

/// Whether the session in `dir` has minutes.
pub fn has_minutes(dir: &str) -> bool {
    newest_minutes(Path::new(dir)).is_some()
}

/// Gives every indexed session its series (the manual ones stay as the user set them).
fn refresh_series(store: &mut Store, rules: &[SeriesRule]) -> Result<(), StoreError> {
    for session in store.list_sessions()? {
        let Some(name) = session.meeting_name else {
            continue;
        };
        if let Some((key, origin)) = series_for_name(&name, rules) {
            store.assign_series(&session.session_id, &key, origin)?;
        }
    }
    Ok(())
}

/// Indexes the sessions under `corpus` that the index does not know yet (the ones it knows are
/// not read again; `mc import` refreshes them) except those `skip` says, then files every
/// session under its series.
pub fn index_corpus(
    store: &mut Store,
    corpus: &Path,
    rules: &[SeriesRule],
    skip: impl Fn(&Path) -> bool,
) -> Result<(), StoreError> {
    if let Ok(entries) = std::fs::read_dir(corpus) {
        let known: BTreeSet<String> = store.list_sessions()?.into_iter().map(|s| s.dir).collect();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.join("manifest.json").is_file()
                && !skip(&path)
                && !known.contains(&path.display().to_string())
            {
                // A session that fails to import cannot be a series member anyway.
                let _ = store.import_session(&path);
            }
        }
    }
    refresh_series(store, rules)
}

/// What the meetings of a series leave open, oldest first: each meeting with minutes closes,
/// keeps or adds to what the earlier ones left, so out-of-order or pre-feature minutes never
/// lose an item.
pub struct Chain {
    /// The latest meeting with minutes that was counted.
    pub last: Option<String>,
    pub open: Vec<CarriedCommitment>,
    /// Meetings left out, and why.
    pub warnings: Vec<String>,
}

/// With `before` (session id and start), the meeting itself and later ones do not count.
/// Meetings without a start date or without minutes are left out, and said so.
pub fn chain(members: &[SeriesMember], before: Option<(&str, i64)>) -> Chain {
    let mut out = Chain {
        last: None,
        open: Vec::new(),
        warnings: Vec::new(),
    };
    for m in members {
        // A session folder the user deleted is gone from the series, not a gap in it.
        if before.is_some_and(|(id, _)| m.session_id == id)
            || !Path::new(&m.dir).join("manifest.json").is_file()
        {
            continue;
        }
        let Some(started) = m.started_wall_ms else {
            out.warnings.push(format!(
                "{} has no start date: it is left out of the series",
                m.session_id
            ));
            continue;
        };
        if before.is_some_and(|(_, at)| started >= at) {
            continue;
        }
        match session_minutes(Path::new(&m.dir)) {
            Some(minutes) => {
                out.open = inherit(&out.open, &minutes.commitments, &minutes.carried);
                out.last = Some(m.session_id.clone());
            }
            None => out.warnings.push(format!(
                "{} has no minutes: what it committed to is not carried (the chain is incomplete)",
                m.session_id
            )),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use mc_store::SeriesOrigin;

    use super::*;

    fn member(id: &str, dir: &Path, started: i64) -> SeriesMember {
        SeriesMember {
            session_id: id.to_string(),
            dir: dir.display().to_string(),
            meeting_name: Some("Daily".to_string()),
            started_wall_ms: Some(started),
            origin: SeriesOrigin::Auto,
        }
    }

    #[test]
    fn a_deleted_session_is_left_out_without_a_warning() {
        let root = tempfile::tempdir().unwrap();
        let kept = root.path().join("kept");
        std::fs::create_dir_all(&kept).unwrap();
        std::fs::write(kept.join("manifest.json"), "{}").unwrap();
        let members = [
            member("gone", &root.path().join("gone"), 1),
            member("kept", &kept, 2),
        ];
        let walked = chain(&members, None);
        assert_eq!(walked.warnings.len(), 1, "{:?}", walked.warnings);
        assert!(walked.warnings[0].starts_with("kept has no minutes"));
    }
}
