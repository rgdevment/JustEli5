//! SQLite index over the corpus. The JSONL files stay the source of truth: the
//! database can always be deleted and rebuilt, and importing is idempotent.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Map, Value};

use crate::error::StoreError;
use crate::jsonl::{BadLine, CaptionLine, LegacyLine, ParsedLine, read_bytes_at};
use crate::manifest::Manifest;
use crate::series::SeriesOrigin;

/// Entry `i` upgrades the schema from version `i` to `i + 1`.
const MIGRATIONS: &[&str] = &[
    r"
CREATE TABLE sessions (
    id INTEGER PRIMARY KEY,
    session_id TEXT NOT NULL UNIQUE,
    dir TEXT NOT NULL,
    meeting_name TEXT,
    capture_source TEXT,
    platform TEXT,
    started_wall_ms INTEGER,
    ended_wall_ms INTEGER,
    end_reason TEXT,
    captions_file TEXT NOT NULL,
    manifest_json TEXT NOT NULL,
    imported_at_ms INTEGER NOT NULL
);
CREATE TABLE captions (
    session INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    line_no INTEGER NOT NULL,
    v INTEGER NOT NULL,
    seq INTEGER,
    frame INTEGER,
    t REAL NOT NULL,
    wall_ms INTEGER,
    mono_ms REAL,
    read_ms REAL,
    speaker TEXT NOT NULL,
    text TEXT NOT NULL,
    decision TEXT NOT NULL,
    speaker_raw TEXT,
    extra TEXT,
    PRIMARY KEY (session, line_no)
) WITHOUT ROWID;
CREATE INDEX captions_seq ON captions(session, seq);
CREATE INDEX captions_decision ON captions(session, decision);
CREATE VIEW committed_lines AS
    SELECT * FROM captions WHERE decision = 'committed' ORDER BY session, line_no;
CREATE TABLE import_state (
    session INTEGER PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    file TEXT NOT NULL,
    file_size INTEGER NOT NULL,
    file_mtime_ns INTEGER,
    consumed_bytes INTEGER NOT NULL,
    consumed_lines INTEGER NOT NULL,
    prefix_fnv64 INTEGER NOT NULL,
    bad_lines INTEGER NOT NULL,
    last_import_ms INTEGER NOT NULL
);
",
    r"
CREATE TABLE runs (
    id INTEGER PRIMARY KEY,
    run_id TEXT NOT NULL UNIQUE,
    session INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    dir TEXT NOT NULL,
    config_json TEXT NOT NULL,
    tool_version TEXT NOT NULL,
    input_bytes INTEGER NOT NULL,
    input_fnv INTEGER NOT NULL,
    ended_cleanly INTEGER NOT NULL,
    synthetic_flush INTEGER NOT NULL,
    segments INTEGER NOT NULL,
    content_div INTEGER NOT NULL,
    timing_div INTEGER NOT NULL,
    duration_ms INTEGER,
    report_json TEXT NOT NULL,
    created_ms INTEGER NOT NULL
);
CREATE INDEX runs_session ON runs(session);
CREATE TABLE run_segments (
    run INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    idx INTEGER NOT NULL,
    speaker TEXT NOT NULL,
    speaker_raw TEXT,
    text TEXT NOT NULL,
    first_mono_ms REAL,
    last_mono_ms REAL,
    committed_mono_ms REAL,
    committed_wall_ms INTEGER,
    PRIMARY KEY (run, idx)
) WITHOUT ROWID;
CREATE INDEX run_segments_speaker ON run_segments(run, speaker);
",
    r"
CREATE TABLE asr_runs (
    id INTEGER PRIMARY KEY,
    asr_id TEXT NOT NULL,
    session INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    dir TEXT NOT NULL,
    engine TEXT NOT NULL,
    model TEXT NOT NULL,
    segments INTEGER NOT NULL,
    meta_json TEXT NOT NULL,
    created_ms INTEGER NOT NULL,
    UNIQUE (session, asr_id)
);
CREATE INDEX asr_runs_session ON asr_runs(session);
CREATE TABLE fusions (
    id INTEGER PRIMARY KEY,
    fuse_id TEXT NOT NULL UNIQUE,
    session INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    run_id TEXT NOT NULL,
    asr_id TEXT NOT NULL,
    dir TEXT NOT NULL,
    config_json TEXT NOT NULL,
    segments INTEGER NOT NULL,
    merged INTEGER NOT NULL,
    caption_only INTEGER NOT NULL,
    asr_only INTEGER NOT NULL,
    edits_applied INTEGER NOT NULL,
    lag_median_ms REAL,
    report_json TEXT NOT NULL,
    created_ms INTEGER NOT NULL
);
CREATE INDEX fusions_session ON fusions(session);
CREATE TABLE fused_segments (
    fusion INTEGER NOT NULL REFERENCES fusions(id) ON DELETE CASCADE,
    idx INTEGER NOT NULL,
    start_ms REAL NOT NULL,
    end_ms REAL NOT NULL,
    speaker TEXT NOT NULL,
    speaker_inferred INTEGER NOT NULL,
    ch TEXT NOT NULL,
    source TEXT NOT NULL,
    text TEXT NOT NULL,
    caption_text TEXT,
    asr_text TEXT,
    PRIMARY KEY (fusion, idx)
) WITHOUT ROWID;
",
    r"
CREATE TABLE series (
    session INTEGER PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    series_key TEXT NOT NULL,
    origin TEXT NOT NULL
);
CREATE INDEX series_by_key ON series(series_key);
",
];

pub const FNV_OFFSET: u64 = 0xCBF2_9CE4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01B3;

pub fn fnv_extend(mut state: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        state ^= u64::from(*byte);
        state = state.wrapping_mul(FNV_PRIME);
    }
    state
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildReason {
    Shrunk,
    Replaced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportAction {
    Created,
    Appended,
    Unchanged,
    Rebuilt(RebuildReason),
}

#[derive(Debug, Clone)]
pub struct ImportReport {
    pub session_id: String,
    pub action: ImportAction,
    pub lines_added: usize,
    /// Lines of this import that could not be stored.
    pub bad: Vec<BadLine>,
    /// Bytes after the last newline: a line still being written (or cut).
    pub tail_pending_bytes: usize,
    pub ended_cleanly: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    pub session_id: String,
    pub dir: String,
    pub meeting_name: Option<String>,
    pub started_wall_ms: Option<i64>,
    pub ended_wall_ms: Option<i64>,
    pub captions: u64,
    pub committed: u64,
    pub last_wall_ms: Option<i64>,
    pub bad_lines: u64,
    pub imported_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesSummary {
    pub key: String,
    pub sessions: u64,
    pub last_wall_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesMember {
    pub session_id: String,
    pub dir: String,
    pub meeting_name: Option<String>,
    pub started_wall_ms: Option<i64>,
    pub origin: SeriesOrigin,
}

/// One directory's import result from `import_all`.
pub type ImportOutcome = (PathBuf, Result<ImportReport, StoreError>);

/// A run to index (built by `mc-engine`; the store never depends on it).
#[derive(Debug, Clone, PartialEq)]
pub struct RunRow {
    pub run_id: String,
    pub session_id: String,
    pub dir: String,
    pub config_json: String,
    pub tool_version: String,
    pub input_bytes: i64,
    pub input_fnv: u64,
    pub ended_cleanly: bool,
    pub synthetic_flush: bool,
    pub content_div: i64,
    pub timing_div: i64,
    pub duration_ms: Option<i64>,
    pub report_json: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SegmentRow {
    pub speaker: String,
    pub speaker_raw: Option<String>,
    pub text: String,
    pub first_mono_ms: Option<f64>,
    pub last_mono_ms: Option<f64>,
    pub committed_mono_ms: Option<f64>,
    pub committed_wall_ms: Option<i64>,
}

/// A local speech-recognition run of a session (built by `mc-asr`'s caller).
#[derive(Debug, Clone, PartialEq)]
pub struct AsrRunRow {
    pub asr_id: String,
    pub session_id: String,
    pub dir: String,
    pub engine: String,
    pub model: String,
    pub segments: i64,
    pub meta_json: String,
}

/// A fused transcript (captions + ASR) to index.
#[derive(Debug, Clone, PartialEq)]
pub struct FusionRow {
    pub fuse_id: String,
    pub session_id: String,
    /// The captions run (V2) it was built on.
    pub run_id: String,
    pub asr_id: String,
    pub dir: String,
    pub config_json: String,
    pub merged: i64,
    pub caption_only: i64,
    pub asr_only: i64,
    pub edits_applied: i64,
    pub lag_median_ms: Option<f64>,
    pub report_json: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FusedRow {
    pub start_ms: f64,
    pub end_ms: f64,
    pub speaker: String,
    pub speaker_inferred: bool,
    pub ch: String,
    pub source: String,
    pub text: String,
    pub caption_text: Option<String>,
    pub asr_text: Option<String>,
}

pub struct Store {
    conn: Connection,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;
        // Returns a row, so it cannot go through `execute`.
        // Switching the journal mode takes a lock the busy timeout does not
        // cover, so a concurrent first open can get SQLITE_BUSY: retry.
        let mut attempt = 0;
        loop {
            match conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(())) {
                Ok(()) => break,
                Err(rusqlite::Error::SqliteFailure(err, _))
                    if err.code == rusqlite::ErrorCode::DatabaseBusy && attempt < 50 =>
                {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(err) => return Err(err.into()),
            }
        }
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    pub fn schema_version(&self) -> Result<i64, StoreError> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?)
    }

    fn migrate(&mut self) -> Result<(), StoreError> {
        let known = MIGRATIONS.len() as i64;
        // Immediate, then re-read: two processes opening a new database at once
        // must not both run migration 1.
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let found: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if found > known {
            return Err(StoreError::SchemaTooNew { found, known });
        }
        let start = usize::try_from(found)
            .map_err(|_| StoreError::Format(format!("negative schema version {found}")))?;
        for (index, sql) in MIGRATIONS.iter().enumerate().skip(start) {
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", index as i64 + 1)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Imports (or incrementally refreshes) one corpus directory.
    pub fn import_session(&mut self, dir: impl AsRef<Path>) -> Result<ImportReport, StoreError> {
        let dir = dir.as_ref();
        let manifest = Manifest::load(dir.join("manifest.json"))?;
        // The id becomes part of file names later: no separators or traversal.
        if manifest.session_id.is_empty()
            || manifest.session_id == ".."
            || manifest.session_id.contains(['/', '\\', '\0'])
        {
            return Err(StoreError::Format(format!(
                "invalid session_id {:?}",
                manifest.session_id
            )));
        }
        let captions_file = if manifest.captions.file.is_empty() {
            "captions.jsonl".to_string()
        } else {
            manifest.captions.file.clone()
        };
        let captions_path = dir.join(&captions_file);
        let meta =
            std::fs::metadata(&captions_path).map_err(|e| StoreError::io(&captions_path, e))?;
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_nanos()).ok());
        // The bytes read are what is indexed, whatever the writer appended
        // since the metadata call.
        let bytes = std::fs::read(&captions_path).map_err(|e| StoreError::io(&captions_path, e))?;
        let size = bytes.len();
        let dir_text = std::fs::canonicalize(dir)
            .unwrap_or_else(|_| dir.to_path_buf())
            .display()
            .to_string();

        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let manifest_json = manifest.to_json()?;
        let existing: Option<(i64, String)> = tx
            .query_row(
                "SELECT id, dir FROM sessions WHERE session_id = ?1",
                [&manifest.session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let session = match &existing {
            Some((id, _)) => *id,
            None => {
                tx.execute(
                    "INSERT INTO sessions (session_id, dir, captions_file, manifest_json, imported_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![manifest.session_id, dir_text, captions_file, manifest_json, now_ms()],
                )?;
                tx.last_insert_rowid()
            }
        };
        type State = (i64, i64, i64, String);
        let state: Option<State> = tx
            .query_row(
                "SELECT consumed_bytes, consumed_lines, prefix_fnv64, file FROM import_state WHERE session = ?1",
                [session],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;

        let (action, offset, base_lines, fnv_state) = match state {
            None => (ImportAction::Created, 0usize, 0i64, FNV_OFFSET),
            Some((consumed, lines, stored_fnv, stored_file)) => {
                let consumed = usize::try_from(consumed).unwrap_or(usize::MAX);
                let rebuild = |reason| (ImportAction::Rebuilt(reason), 0usize, 0i64, FNV_OFFSET);
                if stored_file != captions_file {
                    rebuild(RebuildReason::Replaced)
                } else if size < consumed {
                    rebuild(RebuildReason::Shrunk)
                } else if fnv_extend(FNV_OFFSET, &bytes[..consumed]) as i64 != stored_fnv {
                    rebuild(RebuildReason::Replaced)
                } else {
                    (ImportAction::Appended, consumed, lines, stored_fnv as u64)
                }
            }
        };
        if let (Some((_, old_dir)), ImportAction::Rebuilt(_)) = (&existing, action)
            && old_dir != &dir_text
            && Manifest::load(Path::new(old_dir).join("manifest.json"))
                .is_ok_and(|old| old.session_id == manifest.session_id)
        {
            // Only a live second copy conflicts; a moved corpus just re-points.
            return Err(StoreError::Conflict(format!(
                "session {} is already indexed from {old_dir} with different content",
                manifest.session_id
            )));
        }
        if matches!(action, ImportAction::Rebuilt(_)) {
            tx.execute("DELETE FROM captions WHERE session = ?1", [session])?;
        }

        // Only complete lines are consumed; a tail without a newline is still
        // being written (or was cut by a crash).
        let region = &bytes[offset..];
        let cut = region
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |index| index + 1);
        let chunk = &region[..cut];
        let parsed = read_bytes_at(chunk, offset == 0);
        let line_base = usize::try_from(base_lines).unwrap_or(0);
        // Parser line numbers are relative to the chunk.
        let mut bad: Vec<BadLine> = parsed
            .bad
            .iter()
            .map(|b| BadLine {
                line_no: line_base + b.line_no,
                reason: b.reason.clone(),
            })
            .collect();
        let mut added = 0usize;
        {
            let mut insert = tx.prepare_cached(
                "INSERT INTO captions
                 (session, line_no, v, seq, frame, t, wall_ms, mono_ms, read_ms, speaker, text, decision, speaker_raw, extra)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            )?;
            for (line_no, line) in &parsed.lines {
                let absolute = line_base + line_no;
                match insert_row(&mut insert, session, absolute, line) {
                    Ok(()) => added += 1,
                    Err(RowErr::Invalid(reason)) => bad.push(BadLine {
                        line_no: absolute,
                        reason,
                    }),
                    // A database failure must abort (and roll back) the import:
                    // recording it as a bad line would skip the line for good.
                    Err(RowErr::Db(err)) => return Err(err.into()),
                }
            }
        }
        let newlines = chunk.iter().filter(|b| **b == b'\n').count();
        let consumed_bytes = offset + cut;
        let consumed_lines = base_lines + newlines as i64;
        let new_fnv = fnv_extend(fnv_state, chunk);
        let previous_bad: i64 = if action == ImportAction::Appended {
            tx.query_row(
                "SELECT bad_lines FROM import_state WHERE session = ?1",
                [session],
                |row| row.get(0),
            )?
        } else {
            0
        };
        tx.execute(
            "INSERT INTO import_state
             (session, file, file_size, file_mtime_ns, consumed_bytes, consumed_lines, prefix_fnv64, bad_lines, last_import_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(session) DO UPDATE SET
                file = excluded.file, file_size = excluded.file_size,
                file_mtime_ns = excluded.file_mtime_ns,
                consumed_bytes = excluded.consumed_bytes,
                consumed_lines = excluded.consumed_lines,
                prefix_fnv64 = excluded.prefix_fnv64,
                bad_lines = excluded.bad_lines,
                last_import_ms = excluded.last_import_ms",
            params![
                session,
                captions_file,
                size as i64,
                mtime_ns,
                consumed_bytes as i64,
                consumed_lines,
                new_fnv as i64,
                previous_bad + bad.len() as i64,
                now_ms()
            ],
        )?;
        tx.execute(
            "UPDATE sessions SET dir = ?2, meeting_name = ?3, capture_source = ?4, platform = ?5,
                 started_wall_ms = ?6, ended_wall_ms = ?7, end_reason = ?8, captions_file = ?9,
                 manifest_json = ?10, imported_at_ms = ?11
             WHERE id = ?1",
            params![
                session,
                dir_text,
                manifest.meeting_name,
                manifest.capture_source,
                manifest.platform_requested,
                manifest.started_wall_ms,
                manifest.ended_wall_ms,
                manifest.end_reason,
                captions_file,
                manifest_json,
                now_ms()
            ],
        )?;
        tx.commit()?;

        let action = if action == ImportAction::Appended && added == 0 && bad.is_empty() {
            ImportAction::Unchanged
        } else {
            action
        };
        Ok(ImportReport {
            session_id: manifest.session_id.clone(),
            action,
            lines_added: added,
            bad,
            tail_pending_bytes: region.len() - cut,
            ended_cleanly: manifest.ended_cleanly(),
        })
    }

    /// Records a run and its segments. Returns `false` (and changes nothing)
    /// when the run id is already indexed. The session must have been imported.
    pub fn index_run(&mut self, run: &RunRow, segments: &[SegmentRow]) -> Result<bool, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session: Option<i64> = tx
            .query_row(
                "SELECT id FROM sessions WHERE session_id = ?1",
                [&run.session_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(session) = session else {
            return Err(StoreError::Format(format!(
                "session {} is not imported",
                run.session_id
            )));
        };
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO runs
             (run_id, session, dir, config_json, tool_version, input_bytes, input_fnv, ended_cleanly,
              synthetic_flush, segments, content_div, timing_div, duration_ms, report_json, created_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                run.run_id,
                session,
                run.dir,
                run.config_json,
                run.tool_version,
                run.input_bytes,
                run.input_fnv as i64,
                run.ended_cleanly,
                run.synthetic_flush,
                segments.len() as i64,
                run.content_div,
                run.timing_div,
                run.duration_ms,
                run.report_json,
                now_ms()
            ],
        )?;
        if inserted == 0 {
            // Same run, maybe a moved corpus: keep the location current.
            // Also bumps `created_ms`, so "latest" follows the last re-index.
            tx.execute(
                "UPDATE runs SET dir = ?2, created_ms = ?3 WHERE run_id = ?1",
                params![run.run_id, run.dir, now_ms()],
            )?;
            tx.commit()?;
            return Ok(false);
        }
        let id = tx.last_insert_rowid();
        {
            let mut insert = tx.prepare_cached(
                "INSERT INTO run_segments
                 (run, idx, speaker, speaker_raw, text, first_mono_ms, last_mono_ms, committed_mono_ms, committed_wall_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for (idx, seg) in segments.iter().enumerate() {
                insert.execute(params![
                    id,
                    idx as i64,
                    seg.speaker,
                    seg.speaker_raw,
                    seg.text,
                    seg.first_mono_ms,
                    seg.last_mono_ms,
                    seg.committed_mono_ms,
                    seg.committed_wall_ms
                ])?;
            }
        }
        tx.commit()?;
        Ok(true)
    }

    fn session_row(tx: &rusqlite::Transaction<'_>, session_id: &str) -> Result<i64, StoreError> {
        tx.query_row(
            "SELECT id FROM sessions WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| StoreError::Format(format!("session {session_id} is not imported")))
    }

    /// Index an ASR run; false when it was already there (its row is refreshed).
    pub fn index_asr_run(&mut self, run: &AsrRunRow) -> Result<bool, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session = Self::session_row(&tx, &run.session_id)?;
        let updated = tx.execute(
            "UPDATE asr_runs SET dir = ?3, engine = ?4, model = ?5, segments = ?6, meta_json = ?7,
                    created_ms = ?8
             WHERE session = ?1 AND asr_id = ?2",
            params![
                session,
                run.asr_id,
                run.dir,
                run.engine,
                run.model,
                run.segments,
                run.meta_json,
                now_ms()
            ],
        )?;
        if updated == 0 {
            tx.execute(
                "INSERT INTO asr_runs
                 (asr_id, session, dir, engine, model, segments, meta_json, created_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    run.asr_id,
                    session,
                    run.dir,
                    run.engine,
                    run.model,
                    run.segments,
                    run.meta_json,
                    now_ms()
                ],
            )?;
        }
        tx.commit()?;
        Ok(updated == 0)
    }

    /// One indexed ASR run, or `None`.
    pub fn asr_run(&self, session_id: &str, asr_id: &str) -> Result<Option<AsrRunRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT a.asr_id, s.session_id, a.dir, a.engine, a.model, a.segments, a.meta_json
                 FROM asr_runs a JOIN sessions s ON s.id = a.session
                 WHERE s.session_id = ?1 AND a.asr_id = ?2",
                params![session_id, asr_id],
                |row| {
                    Ok(AsrRunRow {
                        asr_id: row.get(0)?,
                        session_id: row.get(1)?,
                        dir: row.get(2)?,
                        engine: row.get(3)?,
                        model: row.get(4)?,
                        segments: row.get(5)?,
                        meta_json: row.get(6)?,
                    })
                },
            )
            .optional()?)
    }

    /// One indexed fusion (without its segments), or `None`.
    pub fn fusion(&self, fuse_id: &str) -> Result<Option<FusionRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT f.fuse_id, s.session_id, f.run_id, f.asr_id, f.dir, f.config_json, f.merged,
                        f.caption_only, f.asr_only, f.edits_applied, f.lag_median_ms, f.report_json
                 FROM fusions f JOIN sessions s ON s.id = f.session WHERE f.fuse_id = ?1",
                [fuse_id],
                |row| {
                    Ok(FusionRow {
                        fuse_id: row.get(0)?,
                        session_id: row.get(1)?,
                        run_id: row.get(2)?,
                        asr_id: row.get(3)?,
                        dir: row.get(4)?,
                        config_json: row.get(5)?,
                        merged: row.get(6)?,
                        caption_only: row.get(7)?,
                        asr_only: row.get(8)?,
                        edits_applied: row.get(9)?,
                        lag_median_ms: row.get(10)?,
                        report_json: row.get(11)?,
                    })
                },
            )
            .optional()?)
    }

    /// ASR run ids of a session, oldest first.
    pub fn asr_runs(&self, session_id: &str) -> Result<Vec<String>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT a.asr_id FROM asr_runs a JOIN sessions s ON s.id = a.session
             WHERE s.session_id = ?1 ORDER BY a.id",
        )?;
        let rows = statement.query_map([session_id], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Index a fusion with its segments; false when it was already there (its
    /// row and segments are replaced by these).
    pub fn index_fusion(
        &mut self,
        fusion: &FusionRow,
        segments: &[FusedRow],
    ) -> Result<bool, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session = Self::session_row(&tx, &fusion.session_id)?;
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM fusions WHERE fuse_id = ?1",
                [&fusion.fuse_id],
                |row| row.get(0),
            )
            .optional()?;
        let values = params![
            fusion.fuse_id,
            session,
            fusion.run_id,
            fusion.asr_id,
            fusion.dir,
            fusion.config_json,
            segments.len() as i64,
            fusion.merged,
            fusion.caption_only,
            fusion.asr_only,
            fusion.edits_applied,
            fusion.lag_median_ms,
            fusion.report_json,
            now_ms()
        ];
        let id = match existing {
            Some(id) => {
                // The same id with new content (a forced redo): the files were
                // rewritten, so the index must follow them.
                tx.execute(
                    "UPDATE fusions SET session = ?2, run_id = ?3, asr_id = ?4, dir = ?5,
                            config_json = ?6, segments = ?7, merged = ?8, caption_only = ?9,
                            asr_only = ?10, edits_applied = ?11, lag_median_ms = ?12,
                            report_json = ?13, created_ms = ?14
                     WHERE fuse_id = ?1",
                    values,
                )?;
                tx.execute("DELETE FROM fused_segments WHERE fusion = ?1", [id])?;
                id
            }
            None => {
                tx.execute(
                    "INSERT INTO fusions
                     (fuse_id, session, run_id, asr_id, dir, config_json, segments, merged,
                      caption_only, asr_only, edits_applied, lag_median_ms, report_json, created_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                    values,
                )?;
                tx.last_insert_rowid()
            }
        };
        {
            let mut insert = tx.prepare_cached(
                "INSERT INTO fused_segments
                 (fusion, idx, start_ms, end_ms, speaker, speaker_inferred, ch, source, text, caption_text, asr_text)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?;
            for (idx, seg) in segments.iter().enumerate() {
                insert.execute(params![
                    id,
                    idx as i64,
                    seg.start_ms,
                    seg.end_ms,
                    seg.speaker,
                    seg.speaker_inferred,
                    seg.ch,
                    seg.source,
                    seg.text,
                    seg.caption_text,
                    seg.asr_text
                ])?;
            }
        }
        tx.commit()?;
        Ok(existing.is_none())
    }

    /// Fusion ids of a session, oldest first.
    pub fn fusions(&self, session_id: &str) -> Result<Vec<String>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT f.fuse_id FROM fusions f JOIN sessions s ON s.id = f.session
             WHERE s.session_id = ?1 ORDER BY f.id",
        )?;
        let rows = statement.query_map([session_id], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn fused_segments(&self, fuse_id: &str) -> Result<Vec<FusedRow>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT g.start_ms, g.end_ms, g.speaker, g.speaker_inferred, g.ch, g.source, g.text,
                    g.caption_text, g.asr_text
             FROM fused_segments g JOIN fusions f ON f.id = g.fusion
             WHERE f.fuse_id = ?1 ORDER BY g.idx",
        )?;
        let rows = statement.query_map([fuse_id], |row| {
            Ok(FusedRow {
                start_ms: row.get(0)?,
                end_ms: row.get(1)?,
                speaker: row.get(2)?,
                speaker_inferred: row.get(3)?,
                ch: row.get(4)?,
                source: row.get(5)?,
                text: row.get(6)?,
                caption_text: row.get(7)?,
                asr_text: row.get(8)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Run ids of a session, oldest first.
    pub fn runs(&self, session_id: &str) -> Result<Vec<String>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT r.run_id FROM runs r JOIN sessions s ON s.id = r.session
             WHERE s.session_id = ?1 ORDER BY r.id",
        )?;
        let rows = statement.query_map([session_id], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// One indexed run, or `None`.
    pub fn run(&self, run_id: &str) -> Result<Option<RunRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT r.run_id, s.session_id, r.dir, r.config_json, r.tool_version, r.input_bytes,
                        r.input_fnv, r.ended_cleanly, r.synthetic_flush, r.content_div, r.timing_div,
                        r.duration_ms, r.report_json
                 FROM runs r JOIN sessions s ON s.id = r.session WHERE r.run_id = ?1",
                [run_id],
                |row| {
                    Ok(RunRow {
                        run_id: row.get(0)?,
                        session_id: row.get(1)?,
                        dir: row.get(2)?,
                        config_json: row.get(3)?,
                        tool_version: row.get(4)?,
                        input_bytes: row.get(5)?,
                        input_fnv: row.get::<_, i64>(6)? as u64,
                        ended_cleanly: row.get(7)?,
                        synthetic_flush: row.get(8)?,
                        content_div: row.get(9)?,
                        timing_div: row.get(10)?,
                        duration_ms: row.get(11)?,
                        report_json: row.get(12)?,
                    })
                },
            )
            .optional()?)
    }

    /// The most recently indexed run of a session.
    pub fn latest_run(&self, session_id: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT r.run_id FROM runs r JOIN sessions s ON s.id = r.session
                 WHERE s.session_id = ?1 ORDER BY r.created_ms DESC, r.id DESC LIMIT 1",
                [session_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Session ids that start with `prefix` (an exact id always wins alone).
    pub fn resolve_session(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        self.resolve("sessions", "session_id", prefix)
    }

    /// Run ids of one session that start with `prefix`.
    pub fn resolve_run_in(
        &self,
        session_id: &str,
        prefix: &str,
    ) -> Result<Vec<String>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT r.run_id FROM runs r JOIN sessions s ON s.id = r.session
             WHERE s.session_id = ?1 AND substr(r.run_id, 1, length(?2)) = ?2 ORDER BY r.run_id",
        )?;
        let rows = statement.query_map([session_id, prefix], |row| row.get(0))?;
        let all: Vec<String> = rows.collect::<Result<_, _>>()?;
        Ok(if all.iter().any(|id| id == prefix) {
            vec![prefix.to_string()]
        } else {
            all
        })
    }

    /// Run ids that start with `prefix` (an exact id always wins alone).
    pub fn resolve_run(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        self.resolve("runs", "run_id", prefix)
    }

    fn resolve(&self, table: &str, column: &str, prefix: &str) -> Result<Vec<String>, StoreError> {
        // Table and column are internal constants; the prefix is always bound.
        let exact: Option<String> = self
            .conn
            .query_row(
                &format!("SELECT {column} FROM {table} WHERE {column} = ?1"),
                [prefix],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = exact {
            return Ok(vec![id]);
        }
        let mut statement = self.conn.prepare(&format!(
            "SELECT {column} FROM {table} WHERE substr({column}, 1, length(?1)) = ?1 ORDER BY {column}"
        ))?;
        let rows = statement.query_map([prefix], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn run_segments(&self, run_id: &str) -> Result<Vec<SegmentRow>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT g.speaker, g.speaker_raw, g.text, g.first_mono_ms, g.last_mono_ms,
                    g.committed_mono_ms, g.committed_wall_ms
             FROM run_segments g JOIN runs r ON r.id = g.run
             WHERE r.run_id = ?1 ORDER BY g.idx",
        )?;
        let rows = statement.query_map([run_id], |row| {
            Ok(SegmentRow {
                speaker: row.get(0)?,
                speaker_raw: row.get(1)?,
                text: row.get(2)?,
                first_mono_ms: row.get(3)?,
                last_mono_ms: row.get(4)?,
                committed_mono_ms: row.get(5)?,
                committed_wall_ms: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Imports every corpus directory directly under `root`.
    pub fn import_all(&mut self, root: impl AsRef<Path>) -> Result<Vec<ImportOutcome>, StoreError> {
        let root = root.as_ref();
        let mut dirs: Vec<PathBuf> = Vec::new();
        for entry in std::fs::read_dir(root).map_err(|e| StoreError::io(root, e))? {
            let path = entry.map_err(|e| StoreError::io(root, e))?.path();
            if path.join("manifest.json").is_file() {
                dirs.push(path);
            }
        }
        dirs.sort();
        Ok(dirs
            .into_iter()
            .map(|dir| {
                let report = self.import_session(&dir);
                (dir, report)
            })
            .collect())
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionSummary>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT s.session_id, s.dir, s.meeting_name, s.started_wall_ms, s.ended_wall_ms,
                    (SELECT COUNT(*) FROM captions c WHERE c.session = s.id),
                    (SELECT COUNT(*) FROM captions c WHERE c.session = s.id AND c.decision = 'committed'),
                    (SELECT MAX(c.wall_ms) FROM captions c WHERE c.session = s.id),
                    COALESCE((SELECT bad_lines FROM import_state i WHERE i.session = s.id), 0),
                    s.imported_at_ms
             FROM sessions s ORDER BY s.started_wall_ms IS NULL, s.started_wall_ms, s.session_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(SessionSummary {
                session_id: row.get(0)?,
                dir: row.get(1)?,
                meeting_name: row.get(2)?,
                started_wall_ms: row.get(3)?,
                ended_wall_ms: row.get(4)?,
                captions: row.get::<_, i64>(5)?.max(0) as u64,
                committed: row.get::<_, i64>(6)?.max(0) as u64,
                last_wall_ms: row.get(7)?,
                bad_lines: row.get::<_, i64>(8)?.max(0) as u64,
                imported_at_ms: row.get(9)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Puts a session in a series. Anything but `Manual` leaves a manual choice alone;
    /// returns whether the row now says `key`.
    pub fn assign_series(
        &mut self,
        session_id: &str,
        key: &str,
        origin: SeriesOrigin,
    ) -> Result<bool, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session = Self::session_row(&tx, session_id)?;
        let current: Option<String> = tx
            .query_row(
                "SELECT origin FROM series WHERE session = ?1",
                [session],
                |row| row.get(0),
            )
            .optional()?;
        let pinned = origin != SeriesOrigin::Manual
            && current.as_deref().and_then(SeriesOrigin::parse) == Some(SeriesOrigin::Manual);
        if !pinned {
            tx.execute(
                "INSERT INTO series (session, series_key, origin) VALUES (?1, ?2, ?3)
                 ON CONFLICT(session) DO UPDATE SET series_key = ?2, origin = ?3",
                params![session, key, origin.as_str()],
            )?;
        }
        tx.commit()?;
        Ok(!pinned)
    }

    /// The series a session was put in and how.
    pub fn series_of(
        &self,
        session_id: &str,
    ) -> Result<Option<(String, SeriesOrigin)>, StoreError> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT r.series_key, r.origin FROM series r
                 JOIN sessions s ON s.id = r.session WHERE s.session_id = ?1",
                [session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row.and_then(|(key, origin)| Some((key, SeriesOrigin::parse(&origin)?))))
    }

    /// Every series with its session count and the start of its newest session.
    pub fn list_series(&self) -> Result<Vec<SeriesSummary>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT r.series_key, COUNT(*), MAX(s.started_wall_ms)
             FROM series r JOIN sessions s ON s.id = r.session
             GROUP BY r.series_key ORDER BY r.series_key",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(SeriesSummary {
                key: row.get(0)?,
                sessions: row.get::<_, i64>(1)?.max(0) as u64,
                last_wall_ms: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The sessions of a series, oldest first.
    pub fn series_sessions(&self, key: &str) -> Result<Vec<SeriesMember>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT s.session_id, s.dir, s.meeting_name, s.started_wall_ms, r.origin
             FROM series r JOIN sessions s ON s.id = r.session
             WHERE r.series_key = ?1
             ORDER BY s.started_wall_ms IS NULL, s.started_wall_ms, s.session_id",
        )?;
        let rows = statement.query_map([key], |row| {
            Ok(SeriesMember {
                session_id: row.get(0)?,
                dir: row.get(1)?,
                meeting_name: row.get(2)?,
                started_wall_ms: row.get(3)?,
                origin: SeriesOrigin::parse(&row.get::<_, String>(4)?)
                    .unwrap_or(SeriesOrigin::Auto),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Every stored line of a session in file order.
    pub fn captions(&self, session_id: &str) -> Result<Vec<ParsedLine>, StoreError> {
        self.query_lines(session_id, false)
    }

    /// The recorded transcript: lines with decision `committed`.
    pub fn committed_lines(&self, session_id: &str) -> Result<Vec<ParsedLine>, StoreError> {
        self.query_lines(session_id, true)
    }

    fn query_lines(
        &self,
        session_id: &str,
        committed_only: bool,
    ) -> Result<Vec<ParsedLine>, StoreError> {
        let table = if committed_only {
            "committed_lines"
        } else {
            "captions"
        };
        let sql = format!(
            "SELECT c.v, c.seq, c.frame, c.t, c.wall_ms, c.mono_ms, c.read_ms, c.speaker, c.text,
                    c.decision, c.speaker_raw, c.extra
             FROM {table} c JOIN sessions s ON s.id = c.session
             WHERE s.session_id = ?1 ORDER BY c.line_no"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let mut rows = statement.query([session_id])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let v: i64 = row.get(0)?;
            let t: f64 = row.get(3)?;
            let speaker: String = row.get(7)?;
            let text: String = row.get(8)?;
            let decision: String = row.get(9)?;
            if v == 1 {
                out.push(ParsedLine::V1(LegacyLine {
                    t,
                    speaker,
                    text,
                    decision,
                }));
                continue;
            }
            let extra: Option<String> = row.get(11)?;
            let extra: Map<String, Value> = match extra {
                Some(text) => serde_json::from_str(&text)?,
                None => Map::new(),
            };
            out.push(ParsedLine::V2(CaptionLine {
                seq: u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
                frame: row
                    .get::<_, Option<i64>>(2)?
                    .and_then(|n| u64::try_from(n).ok()),
                t,
                wall_ms: row.get(4)?,
                mono_ms: row.get(5)?,
                read_ms: row.get(6)?,
                speaker,
                text,
                decision,
                speaker_raw: row.get(10)?,
                extra,
            }));
        }
        Ok(out)
    }
}

enum RowErr {
    /// The line cannot be represented; it is reported and skipped.
    Invalid(String),
    Db(rusqlite::Error),
}

fn finite(name: &str, value: f64) -> Result<f64, RowErr> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(RowErr::Invalid(format!("{name} is not finite")))
    }
}

fn to_i64(name: &str, value: u64) -> Result<i64, RowErr> {
    i64::try_from(value)
        .map_err(|_| RowErr::Invalid(format!("{name} {value} does not fit a 64-bit integer")))
}

fn insert_row(
    insert: &mut rusqlite::CachedStatement<'_>,
    session: i64,
    line_no: usize,
    line: &ParsedLine,
) -> Result<(), RowErr> {
    let line_no =
        i64::try_from(line_no).map_err(|_| RowErr::Invalid("line number overflow".into()))?;
    let result = match line {
        ParsedLine::V1(l) => insert.execute(params![
            session,
            line_no,
            1,
            None::<i64>,
            None::<i64>,
            finite("t", l.t)?,
            None::<i64>,
            None::<f64>,
            None::<f64>,
            l.speaker,
            l.text,
            l.decision,
            None::<String>,
            None::<String>,
        ]),
        ParsedLine::V2(l) => {
            let extra = if l.extra.is_empty() {
                None
            } else {
                Some(Value::Object(l.extra.clone()).to_string())
            };
            insert.execute(params![
                session,
                line_no,
                2,
                to_i64("seq", l.seq)?,
                l.frame.map(|f| to_i64("frame", f)).transpose()?,
                finite("t", l.t)?,
                l.wall_ms,
                finite("mono_ms", l.mono_ms)?,
                l.read_ms.map(|r| finite("read_ms", r)).transpose()?,
                l.speaker,
                l.text,
                l.decision,
                l.speaker_raw,
                extra,
            ])
        }
    };
    result.map(|_| ()).map_err(RowErr::Db)
}
