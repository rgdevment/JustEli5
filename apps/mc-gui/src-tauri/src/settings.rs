//! What the window remembers between runs (platform, devices, toggles, recent names),
//! in the app's own config folder, and the context file handed to `mc`.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tauri::{AppHandle, Manager};

fn file(app: &AppHandle, name: &str) -> Result<PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join(name))
}

pub fn load(app: &AppHandle) -> Value {
    load_file(app, "settings.json")
}

/// A JSON file of the app's config folder; `null` when missing or broken.
pub fn load_file(app: &AppHandle, name: &str) -> Value {
    file(app, name)
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

/// The path of a file of the app's config folder, when it is there.
pub fn existing_file(app: &AppHandle, name: &str) -> Option<String> {
    file(app, name)
        .ok()
        .filter(|path| path.is_file())
        .map(|path| path.display().to_string())
}

pub fn save(app: &AppHandle, value: &Value) -> Result<(), String> {
    save_file(app, "settings.json", value)
}

pub fn save_file(app: &AppHandle, name: &str, value: &Value) -> Result<(), String> {
    let path = file(app, name)?;
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| e.to_string())
}

/// The context for the analyst and the minutes, as the file `--context` reads; none when empty.
pub fn write_context(app: &AppHandle, text: &str) -> Result<Option<String>, String> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    let path = file(app, "context.txt")?;
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    Ok(Some(path.display().to_string()))
}

/// An image of a note, written under `<session>/notes/`; returns its path relative to the session.
pub fn save_note_image(session_dir: &str, ext: &str, bytes: &[u8]) -> Result<String, String> {
    let session = Path::new(session_dir);
    if !session.join("manifest.json").is_file() {
        return Err(format!("{session_dir} is not a meeting session"));
    }
    if ext.is_empty() || ext.len() > 5 || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(format!("unexpected image type {ext:?}"));
    }
    let dir = session.join("notes");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let name = format!("{stamp}.{}", ext.to_ascii_lowercase());
    std::fs::write(dir.join(&name), bytes).map_err(|e| e.to_string())?;
    Ok(format!("notes/{name}"))
}
