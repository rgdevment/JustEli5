//! Writes a session to disk as it is captured: `captions.jsonl` (unbuffered,
//! so a crash loses at most the line being written) and `manifest.json`
//! (rewritten atomically). The layout and manifest match what the Python
//! recorder produces.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use mc_core::Stamp;
use mc_core::config::{ANONYMOUS_SPEAKER_ALIASES, ANONYMOUS_SPEAKER_LABEL};
use mc_core::time::{dir_stamp, utc_iso_ms};
use mc_reconcile::Policy;
use mc_store::StoreError;
use mc_store::jsonl::{CaptionLine, CaptionWriter};
use mc_store::manifest::{AppInfo, CaptionsInfo, ClockInfo, MANIFEST_SCHEMA, Manifest, OsInfo};
use serde_json::{Map, Value, json};

use crate::runs::policy_name;

/// How often the manifest is refreshed with a "last seen" mark.
const HEARTBEAT_MS: f64 = 60_000.0;

pub struct SessionSpec<'a> {
    pub platform: &'a str,
    pub capture_source: &'a str,
    pub meeting_name: Option<String>,
    /// How the user consented: `"flag"` or `"prompt"`.
    pub consent_method: &'a str,
    pub t0_wall_ms: i64,
    /// `"mono"` for live capture; replay honours it.
    pub debounce_clock: &'a str,
}

pub struct CorpusSession {
    dir: PathBuf,
    manifest: Manifest,
    writer: CaptionWriter,
    last_heartbeat_mono_ms: f64,
    /// A name the user chose (`--name`) wins over what the source reports.
    name_pinned: bool,
}

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 32 lowercase hex digits. `RandomState` has per-process random keys, so two
/// processes starting in the same nanosecond still get different ids.
pub fn generate_session_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let counter = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut parts = [0u64; 2];
    for (index, part) in parts.iter_mut().enumerate() {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u128(nanos);
        hasher.write_u64(counter);
        hasher.write_u32(std::process::id());
        hasher.write_usize(index);
        *part = hasher.finish();
    }
    format!("{:016x}{:016x}", parts[0], parts[1])
}

fn sanitize_platform(platform: &str) -> String {
    let cleaned: String = platform
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "meeting".to_string()
    } else {
        cleaned
    }
}

/// A session that never recorded its end (the process was killed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UncleanSession {
    pub dir: PathBuf,
    pub session_id: String,
}

impl CorpusSession {
    pub fn create(root: impl AsRef<Path>, spec: &SessionSpec<'_>) -> Result<Self, StoreError> {
        let root = root.as_ref();
        std::fs::create_dir_all(root).map_err(|e| StoreError::io(root, e))?;
        let base = format!(
            "{}_{}",
            dir_stamp(spec.t0_wall_ms),
            sanitize_platform(spec.platform)
        );
        // `create_dir` (not `create_dir_all`) fails on an existing folder, so two
        // sessions in the same second never share one.
        let mut dir = root.join(&base);
        let mut suffix = 1u32;
        loop {
            match std::fs::create_dir(&dir) {
                Ok(()) => break,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && suffix < 999 => {
                    suffix += 1;
                    dir = root.join(format!("{base}_{suffix}"));
                }
                Err(e) => return Err(StoreError::io(&dir, e)),
            }
        }
        let iso = utc_iso_ms(spec.t0_wall_ms);
        let mut captions_extra = Map::new();
        captions_extra.insert("capture_all_nodes".into(), json!(true));
        captions_extra.insert("debounce_clock".into(), json!(spec.debounce_clock));
        let mut extra = Map::new();
        extra.insert(
            "consent".into(),
            json!({"captions": true, "audio": false, "method": spec.consent_method,
                   "wall_ms": spec.t0_wall_ms}),
        );
        let manifest = Manifest {
            schema: MANIFEST_SCHEMA.to_string(),
            session_id: generate_session_id(),
            app: AppInfo {
                version: env!("CARGO_PKG_VERSION").to_string(),
                git_commit: None,
                extra: Map::new(),
            },
            python: None,
            runtime: Some(format!("rust {}", env!("CARGO_PKG_VERSION"))),
            os: OsInfo {
                system: std::env::consts::OS.to_string(),
                release: String::new(),
                version: String::new(),
                machine: std::env::consts::ARCH.to_string(),
                extra: Map::new(),
            },
            platform_requested: spec.platform.to_string(),
            capture_source: Some(spec.capture_source.to_string()),
            meeting_name: spec.meeting_name.clone(),
            clock: ClockInfo {
                t0_wall_ms: spec.t0_wall_ms,
                mono_source: "Instant".to_string(),
                t0_utc_iso: iso.clone(),
                extra: Map::new(),
            },
            started_wall_ms: spec.t0_wall_ms,
            started_utc_iso: iso,
            ended_wall_ms: None,
            ended_utc_iso: None,
            ended_mono_ms: None,
            end_reason: None,
            libs: json!({}),
            captions: CaptionsInfo {
                file: "captions.jsonl".to_string(),
                schema: 2,
                anonymous_label: ANONYMOUS_SPEAKER_LABEL.to_string(),
                anonymous_aliases: ANONYMOUS_SPEAKER_ALIASES.map(String::from).to_vec(),
                extra: captions_extra,
            },
            audio: json!({
                "mic": {"enabled": false, "status": "disabled"},
                "system": {"enabled": false, "status": "disabled"},
            }),
            extra,
        };
        let writer = CaptionWriter::create_new(dir.join("captions.jsonl"))?;
        let session = Self {
            dir,
            manifest,
            writer,
            last_heartbeat_mono_ms: 0.0,
            name_pinned: spec.meeting_name.is_some(),
        };
        session.save_manifest()?;
        Ok(session)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn session_id(&self) -> &str {
        &self.manifest.session_id
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The reconciler policy this capture runs, so replay rebuilds the same one.
    pub fn set_policy(&mut self, policy: Policy) -> Result<(), StoreError> {
        self.manifest
            .captions
            .extra
            .insert("policy".into(), json!(policy_name(policy)));
        self.save_manifest()
    }

    /// The `seq` the next line gets.
    pub fn next_seq(&self) -> u64 {
        self.writer.next_seq()
    }

    fn save_manifest(&self) -> Result<(), StoreError> {
        self.manifest.save_atomic(self.dir.join("manifest.json"))
    }

    pub fn write_line(&mut self, line: &CaptionLine) -> Result<(), StoreError> {
        self.writer.write_line(line)
    }

    /// The user also consented to audio: recorded in the consent block and a
    /// "starting" placeholder in `audio` until the recorder reports.
    pub fn enable_audio(&mut self) -> Result<(), StoreError> {
        if let Some(Value::Object(consent)) = self.manifest.extra.get_mut("consent") {
            consent.insert("audio".into(), json!(true));
        }
        self.manifest.audio = json!({
            "mic": {"enabled": true, "status": "starting"},
            "system": {"enabled": true, "status": "starting"},
        });
        self.save_manifest()
    }

    /// Final state of the audio channels (and the backend's versions).
    pub fn set_audio_info(&mut self, info: Value, libs: Value) -> Result<(), StoreError> {
        self.manifest.audio = info;
        if !libs.is_null() {
            self.manifest.libs = libs;
        }
        self.save_manifest()
    }

    pub fn set_meeting_name(&mut self, name: &str) -> Result<(), StoreError> {
        if self.name_pinned || self.manifest.meeting_name.as_deref() == Some(name) {
            return Ok(());
        }
        self.manifest.meeting_name = Some(name.to_string());
        self.save_manifest()
    }

    /// Refreshes the "last seen" marks at most once a minute. It never touches
    /// `ended_*`, so a killed session still reads as unclean.
    pub fn heartbeat(&mut self, stamp: Stamp) -> Result<(), StoreError> {
        if stamp.mono_ms - self.last_heartbeat_mono_ms < HEARTBEAT_MS {
            return Ok(());
        }
        self.last_heartbeat_mono_ms = stamp.mono_ms;
        self.manifest
            .extra
            .insert("last_seen_wall_ms".into(), json!(stamp.wall_ms));
        self.manifest
            .extra
            .insert("last_seen_mono_ms".into(), json!(stamp.mono_ms));
        self.save_manifest()
    }

    /// Records a clean end.
    pub fn close(&mut self, reason: &str, stamp: Stamp) -> Result<(), StoreError> {
        self.manifest.ended_wall_ms = Some(stamp.wall_ms);
        self.manifest.ended_utc_iso = Some(utc_iso_ms(stamp.wall_ms));
        self.manifest.ended_mono_ms = Some(mc_core::pytext::py_round3(stamp.mono_ms));
        self.manifest.end_reason = Some(reason.to_string());
        self.save_manifest()
    }
}

/// A file touched this recently belongs to a capture that may still be running.
const LIVE_WINDOW: std::time::Duration = std::time::Duration::from_secs(120);

fn touched_recently(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < LIVE_WINDOW)
}

/// Whether a session folder was written to in the last two minutes (a capture may be running).
pub fn is_capturing(dir: &Path) -> bool {
    touched_recently(&dir.join("captions.jsonl"))
        || touched_recently(&dir.join("manifest.json"))
        || touched_recently(&dir.join("manifest.json.tmp"))
        // The audio threads can outlive a hung caption thread.
        || touched_recently(&dir.join("mic.wav"))
        || touched_recently(&dir.join("system.wav"))
        || touched_recently(&dir.join("audio_sync.jsonl"))
}

/// Sessions under `root` whose manifest has no end, and cleanup of stale
/// `manifest.json.tmp` files. Reports only: nothing is invented or appended.
/// A session whose files changed in the last two minutes may be capturing right
/// now (even in another process): it is neither reported nor touched.
pub fn scan_unclean(root: impl AsRef<Path>) -> Vec<UncleanSession> {
    let Ok(entries) = std::fs::read_dir(root.as_ref()) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        let live = is_capturing(&dir);
        if live {
            continue;
        }
        let _ = std::fs::remove_file(dir.join("manifest.json.tmp"));
        let Ok(manifest) = Manifest::load(dir.join("manifest.json")) else {
            continue;
        };
        if !manifest.ended_cleanly() {
            found.push(UncleanSession {
                dir,
                session_id: manifest.session_id,
            });
        }
    }
    found.sort_by(|a, b| a.dir.cmp(&b.dir));
    found
}

/// Fix the headers of `mic.wav` / `system.wav` left by a killed session.
/// Returns the files that needed repair (a missing or unreadable one is skipped).
pub fn repair_session_audio(dir: &Path) -> Vec<String> {
    ["mic.wav", "system.wav"]
        .into_iter()
        .filter(|name| {
            let path = dir.join(name);
            path.is_file() && mc_audio::wav::repair_wav(&path).is_ok_and(|r| r.repaired)
        })
        .map(str::to_string)
        .collect()
}
