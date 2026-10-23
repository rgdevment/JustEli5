//! `manifest.json`: the description of one recorded session.
//!
//! The shape matches what the Python `CorpusSession` writes. Unknown fields are
//! kept (`extra`), so a manifest read and saved by Rust never loses what a
//! newer or Python writer put in it.

use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::error::StoreError;

pub const MANIFEST_SCHEMA: &str = "meet-copilot.corpus/1";

/// `null` (or a missing value) reads as the type's default, so one null field
/// from another writer never makes the whole manifest unreadable.
fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

const TOP_KEYS: [&str; 19] = [
    "schema",
    "session_id",
    "app",
    "python",
    "runtime",
    "os",
    "platform_requested",
    "capture_source",
    "meeting_name",
    "clock",
    "started_wall_ms",
    "started_utc_iso",
    "ended_wall_ms",
    "ended_utc_iso",
    "ended_mono_ms",
    "end_reason",
    "libs",
    "captions",
    "audio",
];

fn strip_known(extra: &mut Map<String, Value>, known: &[&str]) {
    extra.retain(|key, _| !known.contains(&key.as_str()));
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AppInfo {
    #[serde(default, deserialize_with = "null_default")]
    pub version: String,
    #[serde(default)]
    pub git_commit: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct OsInfo {
    #[serde(default, deserialize_with = "null_default")]
    pub system: String,
    #[serde(default, deserialize_with = "null_default")]
    pub release: String,
    #[serde(default, deserialize_with = "null_default")]
    pub version: String,
    #[serde(default, deserialize_with = "null_default")]
    pub machine: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ClockInfo {
    #[serde(default, deserialize_with = "null_default")]
    pub t0_wall_ms: i64,
    #[serde(default, deserialize_with = "null_default")]
    pub mono_source: String,
    #[serde(default, deserialize_with = "null_default")]
    pub t0_utc_iso: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CaptionsInfo {
    #[serde(default, deserialize_with = "null_default")]
    pub file: String,
    #[serde(default, deserialize_with = "null_default")]
    pub schema: u64,
    #[serde(default, deserialize_with = "null_default")]
    pub anonymous_label: String,
    #[serde(default, deserialize_with = "null_default")]
    pub anonymous_aliases: Vec<String>,
    /// `capture_all_nodes`, `debounce_clock`, ... written by either side.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: String,
    pub session_id: String,
    #[serde(default)]
    pub app: AppInfo,
    /// Python version when the corpus came from Python.
    #[serde(default)]
    pub python: Option<String>,
    /// Runtime description when it came from Rust (`"rust 1.97"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(default)]
    pub os: OsInfo,
    #[serde(default, deserialize_with = "null_default")]
    pub platform_requested: String,
    #[serde(default)]
    pub capture_source: Option<String>,
    #[serde(default)]
    pub meeting_name: Option<String>,
    #[serde(default)]
    pub clock: ClockInfo,
    #[serde(default, deserialize_with = "null_default")]
    pub started_wall_ms: i64,
    #[serde(default, deserialize_with = "null_default")]
    pub started_utc_iso: String,
    /// `None` means the session did not end cleanly; every file is still readable.
    #[serde(default)]
    pub ended_wall_ms: Option<i64>,
    #[serde(default)]
    pub ended_utc_iso: Option<String>,
    #[serde(default)]
    pub ended_mono_ms: Option<f64>,
    #[serde(default)]
    pub end_reason: Option<String>,
    #[serde(default)]
    pub libs: Value,
    #[serde(default)]
    pub captions: CaptionsInfo,
    #[serde(default)]
    pub audio: Value,
    /// Anything a writer added that this version does not know.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Manifest {
    pub fn from_json(text: &str) -> Result<Self, StoreError> {
        let manifest: Self = serde_json::from_str(text)?;
        if manifest.schema != MANIFEST_SCHEMA {
            return Err(StoreError::Format(format!(
                "unsupported manifest schema {:?} (expected {MANIFEST_SCHEMA:?})",
                manifest.schema
            )));
        }
        Ok(manifest)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| StoreError::io(path, e))?;
        Self::from_json(&text)
    }

    /// Pretty JSON. An `extra` entry that reuses a typed key is dropped: with
    /// the flattened maps it would be written twice and the file could no longer
    /// be read back.
    pub fn to_json(&self) -> Result<String, StoreError> {
        let mut clean = self.clone();
        strip_known(&mut clean.extra, &TOP_KEYS);
        strip_known(&mut clean.app.extra, &["version", "git_commit"]);
        strip_known(
            &mut clean.os.extra,
            &["system", "release", "version", "machine"],
        );
        strip_known(
            &mut clean.clock.extra,
            &["t0_wall_ms", "mono_source", "t0_utc_iso"],
        );
        strip_known(
            &mut clean.captions.extra,
            &["file", "schema", "anonymous_label", "anonymous_aliases"],
        );
        Ok(serde_json::to_string_pretty(&clean)?)
    }

    /// Whether the session closed cleanly (`ended_wall_ms` is set).
    pub fn ended_cleanly(&self) -> bool {
        self.ended_wall_ms.is_some()
    }

    /// Writes the manifest next to its final name and renames it into place, so
    /// a reader never sees a half-written file. Falls back to a direct write if
    /// the rename fails, and never leaves the temporary file behind.
    pub fn save_atomic(&self, path: impl AsRef<Path>) -> Result<(), StoreError> {
        let path = path.as_ref();
        let text = self.to_json()?;
        let tmp = path.with_extension("json.tmp");
        let atomic = std::fs::write(&tmp, &text)
            .and_then(|()| std::fs::File::open(&tmp)?.sync_all())
            .and_then(|()| std::fs::rename(&tmp, path));
        if atomic.is_ok() {
            return Ok(());
        }
        let direct = std::fs::write(path, &text).map_err(|e| StoreError::io(path, e));
        let _ = std::fs::remove_file(&tmp);
        direct
    }
}
