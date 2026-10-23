//! The `eli5` configuration file, edited as raw JSON so keys this app does not know survive.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::programs::{command, home_dir};

const EXAMPLE: &str = include_str!("../../../../scripts/config.example.json");

/// Same lookup as `mc`: `MC_CONFIG`, else the per-user config folder.
pub fn path() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("MC_CONFIG").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(explicit));
    }
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        home_dir().map(|h| h.join("Library").join("Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|h| h.join(".config")))
    };
    let base = base?;
    let dir = base.join("justeli5");
    crate::legacy::move_first_old(&base, &crate::legacy::OLD_APP_DIRS, &dir);
    Some(dir.join("config.json"))
}

#[derive(Serialize)]
pub struct Loaded {
    path: Option<String>,
    exists: bool,
    value: Value,
    /// The file exists but is not JSON: the editor starts empty and says why.
    error: Option<String>,
}

pub fn load() -> Loaded {
    let path = path();
    let text = path.as_deref().map(std::fs::read_to_string);
    let (exists, value, error) = match text {
        Some(Ok(text)) => match serde_json::from_str(&text) {
            Ok(value) => (true, value, None),
            Err(e) => (true, Value::Object(Default::default()), Some(e.to_string())),
        },
        _ => (false, Value::Object(Default::default()), None),
    };
    Loaded {
        path: path.map(|p| p.display().to_string()),
        exists,
        value,
        error,
    }
}

/// Writes the example configuration when there is none yet.
pub fn create() -> Result<String, String> {
    let path = path().ok_or("no configuration folder is known on this system")?;
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, EXAMPLE).map_err(|e| e.to_string())?;
    }
    Ok(path.display().to_string())
}

/// The reason `mc doctor` gives for refusing a configuration, if it does.
fn refusal(mc: &Path, candidate: &Path) -> Result<Option<String>, String> {
    let out = command(mc)
        .arg("--config")
        .arg(candidate)
        .args(["--json", "doctor"])
        .output()
        .map_err(|e| e.to_string())?;
    let report: Value = serde_json::from_slice(&out.stdout)
        .map_err(|_| String::from_utf8_lossy(&out.stderr).trim().to_string())?;
    let failed = report["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| c["name"] == "config" && c["status"] == "FAIL");
    Ok(failed.map(|c| c["detail"].as_str().unwrap_or("invalid").to_string()))
}

/// Saves only what `mc` accepts: the candidate is checked from a file next to the real one.
pub fn save(mc: &Path, value: &Value) -> Result<String, String> {
    let path = path().ok_or("no configuration folder is known on this system")?;
    let parent = path.parent().ok_or("bad configuration path")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let candidate = parent.join("config.gui-check.json");
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())? + "\n";
    std::fs::write(&candidate, text).map_err(|e| e.to_string())?;
    match refusal(mc, &candidate) {
        Ok(None) => std::fs::rename(&candidate, &path).map_err(|e| e.to_string())?,
        Ok(Some(reason)) | Err(reason) => {
            let _ = std::fs::remove_file(&candidate);
            return Err(reason);
        }
    }
    Ok(path.display().to_string())
}
