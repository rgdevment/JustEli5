//! Writing a folder of files as one step.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// A swap that fails is tried again this many times (antivirus and Explorer hold folders for a moment).
const SWAP_TRIES: u32 = 10;
const SWAP_PAUSE: Duration = Duration::from_millis(40);

#[derive(Debug, thiserror::Error)]
#[error("{}: {source}", path.display())]
pub struct FolderError {
    pub path: PathBuf,
    pub source: std::io::Error,
}

fn failed(path: &Path) -> impl FnOnce(std::io::Error) -> FolderError {
    let path = path.to_path_buf();
    move |source| FolderError { path, source }
}

/// Folder names under `parent` that hold `file` (skipping hidden ones and those in `skip`),
/// oldest first by the file's modification time, then by name.
pub fn candidates(parent: &Path, file: &str, skip: &[&str]) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut found: Vec<(std::time::SystemTime, String)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let modified = e.path().join(file).metadata().ok()?.modified().ok()?;
            (!name.starts_with('.') && !skip.contains(&name.as_str())).then_some((modified, name))
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, name)| name).collect()
}

/// Runs `step` until it works or the tries run out, pausing between them.
fn retrying(mut step: impl FnMut() -> std::io::Result<()>) -> std::io::Result<()> {
    let mut last = Ok(());
    for _ in 0..SWAP_TRIES {
        last = step();
        if last.is_ok() {
            break;
        }
        std::thread::sleep(SWAP_PAUSE);
    }
    last
}

/// Writes `files` into `out` as one step: a temporary folder is built beside it and
/// swapped in, so a failure never touches an existing good result and two runs never mix.
/// `marker` is the file that tells a finished folder: if the swap fails but one is there, an
/// identical result made by a concurrent run stands (ids follow the content).
pub fn write_folder(out: &Path, files: &[(&str, String)], marker: &str) -> Result<(), FolderError> {
    swap_in(out, files, Some(marker))
}

/// Like [`write_folder`] for a folder whose content changes (a ledger that grows): the new
/// content always lands or this is an error, never a silent keep of the old one.
pub fn replace_folder(out: &Path, files: &[(&str, String)]) -> Result<(), FolderError> {
    swap_in(out, files, None)
}

fn swap_in(
    out: &Path,
    files: &[(&str, String)],
    accept_existing: Option<&str>,
) -> Result<(), FolderError> {
    let name = out.file_name().and_then(|n| n.to_str()).unwrap_or("out");
    let parent = out.parent().unwrap_or(Path::new("."));
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let suffix = format!("{}.{nanos}", std::process::id());
    let tmp = parent.join(format!(".{name}.{suffix}.tmp"));
    let old = parent.join(format!(".{name}.{suffix}.old"));
    let write = || -> Result<(), FolderError> {
        std::fs::create_dir_all(parent).map_err(failed(parent))?;
        std::fs::create_dir(&tmp).map_err(failed(&tmp))?;
        for (file, content) in files {
            let path = tmp.join(file);
            std::fs::write(&path, content).map_err(failed(&path))?;
        }
        let standing = || accept_existing.is_some_and(|marker| out.join(marker).is_file());
        let displaced = if out.exists() {
            match retrying(|| std::fs::rename(out, &old)) {
                Ok(()) => true,
                Err(_) if standing() => return Ok(()),
                Err(e) => return Err(failed(out)(e)),
            }
        } else {
            false
        };
        match retrying(|| std::fs::rename(&tmp, out)) {
            Ok(()) => Ok(()),
            Err(_) if standing() => Ok(()),
            Err(e) => {
                if displaced {
                    let _ = std::fs::rename(&old, out);
                }
                Err(failed(out)(e))
            }
        }
    };
    let outcome = write();
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_dir_all(&old);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(text: &str) -> Vec<(&'static str, String)> {
        vec![("ledger.json", text.to_string())]
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mc-folder-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn replacing_swaps_the_whole_folder() {
        let dir = scratch("swap");
        let out = dir.join("analysis").join("live");
        replace_folder(&out, &files("one")).unwrap();
        replace_folder(&out, &files("two")).unwrap();
        assert_eq!(
            std::fs::read_to_string(out.join("ledger.json")).unwrap(),
            "two"
        );
        let leftovers = std::fs::read_dir(out.parent().unwrap()).unwrap().count();
        assert_eq!(leftovers, 1, "no temporary folders are left");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_swap_that_cannot_happen_is_an_error() {
        let dir = scratch("blocked");
        std::fs::write(dir.join("analysis"), "a file").unwrap();
        let out = dir.join("analysis").join("live");
        assert!(replace_folder(&out, &files("x")).is_err());
        assert!(write_folder(&out, &files("x"), "ledger.json").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A handle held without sharing stops the folder from being renamed, as an antivirus does.
    #[cfg(windows)]
    #[test]
    fn a_held_folder_is_an_error_and_the_old_result_stays_until_it_is_released() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = scratch("held");
        let out = dir.join("live");
        replace_folder(&out, &files("old")).unwrap();
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(out.join("ledger.json"))
            .unwrap();
        assert!(replace_folder(&out, &files("new")).is_err());
        drop(held);
        assert_eq!(
            std::fs::read_to_string(out.join("ledger.json")).unwrap(),
            "old"
        );
        replace_folder(&out, &files("new")).unwrap();
        assert_eq!(
            std::fs::read_to_string(out.join("ledger.json")).unwrap(),
            "new"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
