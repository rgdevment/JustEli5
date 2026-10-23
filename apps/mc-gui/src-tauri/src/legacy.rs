//! Folders named after the app's former names, moved once to the current ones.

use std::path::Path;

use tauri::{AppHandle, Manager};

/// The CLI's folder names before JustEli5, newest first.
pub const OLD_APP_DIRS: [&str; 2] = ["onmuted", "meet-copilot"];
const OLD_IDENTIFIERS: [&str; 2] = ["dev.rgdevment.onmuted", "dev.rgdevment.meet-copilot"];

/// Renames `old` to `new` when only `old` exists; both sit in the same parent folder.
pub fn move_if_only_old(old: &Path, new: &Path) {
    if old.is_dir() && !new.exists() {
        let _ = std::fs::rename(old, new);
    }
}

/// Moves the newest of `olds` found in `parent` to `new`.
pub fn move_first_old(parent: &Path, olds: &[&str], new: &Path) {
    for old in olds {
        move_if_only_old(&parent.join(old), new);
    }
}

/// The window's own settings (settings.json, people.json, context.txt) follow the new identifier.
pub fn migrate_app_dir(app: &AppHandle) {
    if let Ok(dir) = app.path().app_config_dir()
        && let Some(parent) = dir.parent()
    {
        move_first_old(parent, &OLD_IDENTIFIERS, &dir);
    }
}
