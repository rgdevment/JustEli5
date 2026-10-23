//! The folders the app used under its former names, moved once to the current one.

use std::path::{Path, PathBuf};

use crate::cli::Env;
use crate::config::{LEGACY_APP_DIRS, app_dir_name, config_base, data_base, documents_dir};

/// Moves `old` to `new` when only `old` exists. A move across volumes is a copy into a
/// side folder renamed into place at the end, so `new` is never left half-filled.
pub fn move_dir(old: &Path, new: &Path) -> Result<bool, String> {
    if new.exists() || !old.is_dir() {
        return Ok(false);
    }
    if std::fs::rename(old, new).is_ok() {
        return Ok(true);
    }
    let partial = new.with_extension("partial");
    let _ = std::fs::remove_dir_all(&partial);
    let copied = copy_tree(old, &partial).and_then(|()| std::fs::rename(&partial, new));
    match copied {
        Ok(()) => {
            // The copy is complete; an old folder that cannot be removed only wastes space.
            let _ = std::fs::remove_dir_all(old);
            Ok(true)
        }
        Err(error) => {
            let _ = std::fs::remove_dir_all(&partial);
            Err(format!("{} -> {}: {error}", old.display(), new.display()))
        }
    }
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// The (old, new) pairs of the default folders: configuration, data and exports, one per
/// former name. Once one old folder moves, the new one exists and the rest are left alone.
pub fn default_pairs(env: &Env) -> Vec<(PathBuf, PathBuf)> {
    [config_base(env), data_base(env), documents_dir(env)]
        .into_iter()
        .flatten()
        .flat_map(|base| LEGACY_APP_DIRS.map(|old| (base.join(old), base.join(app_dir_name()))))
        .collect()
}

/// What `migrate` did: one line per move or failure, and whether the data folder moved
/// (its index still names the old session folders until they are imported again).
pub struct Migration {
    pub lines: Vec<String>,
    pub data_moved: bool,
}

/// Moves every default folder that still has the old name.
pub fn migrate(env: &Env) -> Migration {
    let data = data_base(env).map(|base| base.join(app_dir_name()));
    let mut out = Migration {
        lines: Vec::new(),
        data_moved: false,
    };
    for (old, new) in default_pairs(env) {
        match move_dir(&old, &new) {
            Ok(true) => {
                out.data_moved |= data.as_ref() == Some(&new);
                out.lines
                    .push(format!("moved {} to {}", old.display(), new.display()));
            }
            Ok(false) => {}
            Err(error) => out
                .lines
                .push(format!("could not move the old folder: {error}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(root: &Path) {
        std::fs::create_dir_all(root.join("corpus").join("s1")).unwrap();
        std::fs::write(root.join("config.json"), "{}").unwrap();
        std::fs::write(root.join("corpus").join("s1").join("manifest.json"), "{}").unwrap();
    }

    #[test]
    fn the_old_folder_moves_whole_when_the_new_one_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let (old, new) = (dir.path().join("meet-copilot"), dir.path().join("justeli5"));
        tree(&old);
        assert_eq!(move_dir(&old, &new), Ok(true));
        assert!(!old.exists());
        assert!(
            new.join("corpus")
                .join("s1")
                .join("manifest.json")
                .is_file()
        );
        assert!(new.join("config.json").is_file());
    }

    #[test]
    fn an_existing_new_folder_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let (old, new) = (dir.path().join("meet-copilot"), dir.path().join("justeli5"));
        tree(&old);
        std::fs::create_dir_all(&new).unwrap();
        assert_eq!(move_dir(&old, &new), Ok(false));
        assert!(old.join("config.json").is_file());
        assert!(!new.join("config.json").exists());
    }

    #[test]
    fn the_newest_former_name_wins_and_older_ones_stay() {
        let dir = tempfile::tempdir().unwrap();
        let new = dir.path().join("justeli5");
        tree(&dir.path().join("onmuted"));
        tree(&dir.path().join("meet-copilot"));
        let moved: Vec<bool> = LEGACY_APP_DIRS
            .iter()
            .map(|old| move_dir(&dir.path().join(old), &new).unwrap())
            .collect();
        assert_eq!(moved, [true, false]);
        assert!(!dir.path().join("onmuted").exists());
        assert!(
            dir.path()
                .join("meet-copilot")
                .join("config.json")
                .is_file()
        );
    }

    #[test]
    fn nothing_happens_without_an_old_folder() {
        let dir = tempfile::tempdir().unwrap();
        let new = dir.path().join("justeli5");
        assert_eq!(move_dir(&dir.path().join("meet-copilot"), &new), Ok(false));
        assert!(!new.exists());
    }

    #[test]
    fn a_copy_lands_whole_and_leaves_no_side_folder() {
        let dir = tempfile::tempdir().unwrap();
        let (old, new) = (dir.path().join("old"), dir.path().join("new"));
        tree(&old);
        copy_tree(&old, &new.with_extension("partial")).unwrap();
        std::fs::rename(new.with_extension("partial"), &new).unwrap();
        assert!(
            new.join("corpus")
                .join("s1")
                .join("manifest.json")
                .is_file()
        );
        assert!(!new.with_extension("partial").exists());
    }
}
