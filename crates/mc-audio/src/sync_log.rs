//! `audio_sync.jsonl`: sync points and events of every audio channel.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use mc_core::clock::Clock;
use mc_core::pytext::py_round3;
use serde_json::{Map, Value, json};

/// Thread-safe; a log that cannot be opened silently drops its events (audio
/// is best effort and must never disturb capture).
pub struct SyncLog {
    clock: Arc<dyn Clock>,
    file: Mutex<Option<File>>,
}

impl SyncLog {
    pub fn open(path: &Path, clock: Arc<dyn Clock>) -> Self {
        let file = OpenOptions::new().create(true).append(true).open(path).ok();
        Self {
            clock,
            file: Mutex::new(file),
        }
    }

    pub fn is_open(&self) -> bool {
        self.file
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    /// `fields` is a JSON object; its null values are left out.
    pub fn write(&self, ch: &str, event: &str, fields: Value) {
        let stamp = self.clock.now();
        let mut entry = Map::new();
        entry.insert("ch".into(), json!(ch));
        entry.insert("event".into(), json!(event));
        entry.insert("mono_ms".into(), json!(py_round3(stamp.mono_ms)));
        entry.insert("wall_ms".into(), json!(stamp.wall_ms));
        if let Value::Object(map) = fields {
            entry.extend(map.into_iter().filter(|(_, v)| !v.is_null()));
        }
        let mut line = Value::Object(entry).to_string();
        line.push('\n');
        let mut guard = self.file.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(file) = guard.as_mut() {
            // A failed write is not worth interrupting a recording for.
            let _ = file.write_all(line.as_bytes()).and_then(|()| file.flush());
        }
    }

    pub fn close(&self) {
        *self.file.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}
