//! `captions.jsonl` v2 and the legacy `raw_uia_*.jsonl` (v1).
//!
//! Python wrote one JSON object per line. v2 lines carry
//! `{v, seq, frame, t, wall_ms, mono_ms, read_ms, speaker, text, decision,
//! [speaker_raw], ...extras}`; v1 lines only `{t, speaker, text, decision}`.
//! Reading is tolerant (a crash can leave a truncated last line, and a bad line
//! never aborts the file); writing always produces v2 and never lets an extra
//! overwrite a schema key.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use mc_core::pytext::py_round3;
use mc_core::{Clock, FrameRef, Record, Stamp};
use serde_json::{Map, Value, json};

use crate::error::StoreError;

pub const SCHEMA_VERSION: u64 = 2;

/// Keys owned by the schema; `extra` can add fields but never overwrite these.
pub const RESERVED_KEYS: [&str; 11] = [
    "v",
    "seq",
    "frame",
    "t",
    "wall_ms",
    "mono_ms",
    "read_ms",
    "speaker",
    "text",
    "decision",
    "speaker_raw",
];

/// One `captions.jsonl` v2 line.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptionLine {
    pub seq: u64,
    pub frame: Option<u64>,
    pub t: f64,
    pub wall_ms: i64,
    pub mono_ms: f64,
    pub read_ms: Option<f64>,
    pub speaker: String,
    pub text: String,
    /// Kept as a string: legacy files contain decisions Rust no longer emits.
    pub decision: String,
    pub speaker_raw: Option<String>,
    /// Any other key (`first_mono_ms`, `last_mono_ms`, ...). Never schema keys.
    pub extra: Map<String, Value>,
}

/// One legacy v1 line (`raw_uia_*.jsonl` before the timeline existed).
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyLine {
    pub t: f64,
    pub speaker: String,
    pub text: String,
    pub decision: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParsedLine {
    V2(CaptionLine),
    V1(LegacyLine),
}

impl CaptionLine {
    /// Serialises as Python's `RawRecorder.log` does (key order aside).
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("v".into(), json!(SCHEMA_VERSION));
        map.insert("seq".into(), json!(self.seq));
        map.insert("frame".into(), json!(self.frame));
        map.insert("t".into(), json!(self.t));
        map.insert("wall_ms".into(), json!(self.wall_ms));
        map.insert("mono_ms".into(), json!(self.mono_ms));
        map.insert("read_ms".into(), json!(self.read_ms));
        map.insert("speaker".into(), json!(self.speaker));
        map.insert("text".into(), json!(self.text));
        map.insert("decision".into(), json!(self.decision));
        if let Some(raw) = &self.speaker_raw {
            map.insert("speaker_raw".into(), json!(raw));
        }
        for (key, value) in &self.extra {
            if !RESERVED_KEYS.contains(&key.as_str()) {
                map.insert(key.clone(), value.clone());
            }
        }
        Value::Object(map)
    }

    /// Builds the v2 line for a reconciler record. `fallback` supplies the
    /// stamp when the record has none (Python's recorder stamped such records
    /// with its own clock).
    pub fn from_record(seq: u64, record: &Record, fallback: impl FnOnce() -> Stamp) -> Self {
        let stamp = record.stamp.unwrap_or_else(fallback);
        let mut extra = Map::new();
        if let Some((first, last)) = record.span {
            extra.insert("first_mono_ms".into(), json!(first));
            extra.insert("last_mono_ms".into(), json!(last));
        }
        Self {
            seq,
            frame: match record.frame {
                FrameRef::InFrame(n) => Some(n),
                FrameRef::NoClock | FrameRef::OutOfFrame => None,
            },
            t: py_round3(stamp.wall_ms as f64 / 1000.0),
            wall_ms: stamp.wall_ms,
            mono_ms: py_round3(stamp.mono_ms),
            read_ms: record.read_ms.map(py_round3),
            speaker: record.speaker.clone(),
            text: record.text.clone().unwrap_or_default(),
            decision: record.decision.to_string(),
            speaker_raw: record.speaker_raw.clone(),
            extra,
        }
    }
}

/// Why one line could not be used.
#[derive(Debug, Clone, PartialEq)]
pub struct BadLine {
    /// 1-based line number in the file.
    pub line_no: usize,
    pub reason: String,
}

/// Result of reading a whole file.
#[derive(Debug, Clone, Default)]
pub struct ReadReport {
    /// `(1-based line number, line)` for every usable line, in file order.
    pub lines: Vec<(usize, ParsedLine)>,
    /// Lines that are not valid in the middle of the file.
    pub bad: Vec<BadLine>,
    /// The last line had no newline and did not parse: a crash left it cut.
    pub truncated_tail: bool,
}

impl ReadReport {
    /// The `seq` a writer appending to this file should continue with.
    pub fn next_seq(&self) -> u64 {
        self.lines
            .iter()
            .filter_map(|(_, line)| match line {
                ParsedLine::V2(l) => Some(l.seq),
                ParsedLine::V1(_) => None,
            })
            .max()
            .map_or(1, |max| max + 1)
    }
}

fn field<'a>(obj: &'a Map<String, Value>, key: &str) -> Result<&'a Value, String> {
    obj.get(key).ok_or_else(|| format!("missing key {key:?}"))
}

fn string_field(obj: &Map<String, Value>, key: &str) -> Result<String, String> {
    field(obj, key)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{key:?} is not a string"))
}

fn f64_field(obj: &Map<String, Value>, key: &str) -> Result<f64, String> {
    field(obj, key)?
        .as_f64()
        .ok_or_else(|| format!("{key:?} is not a number"))
}

/// Parses one decoded JSON value as a v2 or v1 line.
pub fn parse_value(value: &Value) -> Result<ParsedLine, String> {
    let obj = value.as_object().ok_or("line is not a JSON object")?;
    match obj.get("v") {
        None => Ok(ParsedLine::V1(LegacyLine {
            t: f64_field(obj, "t")?,
            speaker: obj
                .get("speaker")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            text: obj
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            decision: string_field(obj, "decision")?,
        })),
        Some(v) if v.as_u64() == Some(SCHEMA_VERSION) => {
            let optional_u64 = |key: &str| -> Result<Option<u64>, String> {
                match obj.get(key) {
                    None | Some(Value::Null) => Ok(None),
                    Some(v) => v
                        .as_u64()
                        .map(Some)
                        .ok_or_else(|| format!("{key:?} is not an unsigned integer")),
                }
            };
            let read_ms = match obj.get("read_ms") {
                None | Some(Value::Null) => None,
                Some(v) => Some(v.as_f64().ok_or("\"read_ms\" is not a number")?),
            };
            let extra = obj
                .iter()
                .filter(|(k, _)| !RESERVED_KEYS.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            Ok(ParsedLine::V2(CaptionLine {
                seq: field(obj, "seq")?
                    .as_u64()
                    .ok_or("\"seq\" is not an unsigned integer")?,
                frame: optional_u64("frame")?,
                t: f64_field(obj, "t")?,
                wall_ms: field(obj, "wall_ms")?
                    .as_i64()
                    .ok_or("\"wall_ms\" is not an integer")?,
                mono_ms: f64_field(obj, "mono_ms")?,
                read_ms,
                speaker: string_field(obj, "speaker")?,
                text: string_field(obj, "text")?,
                decision: string_field(obj, "decision")?,
                speaker_raw: obj
                    .get("speaker_raw")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                extra,
            }))
        }
        Some(other) => Err(format!("unsupported schema version {other}")),
    }
}

/// Reads a JSONL file from memory. Blank lines are skipped, `\r\n` is accepted,
/// unusable lines are reported (never fatal), and an unparsable last line
/// without a trailing newline is flagged as a truncated tail.
pub fn read_bytes(bytes: &[u8]) -> ReadReport {
    read_bytes_at(bytes, true)
}

/// Like [`read_bytes`] for a slice that may start in the middle of a file: a
/// byte order mark is only skipped when `at_file_start`.
pub fn read_bytes_at(bytes: &[u8], at_file_start: bool) -> ReadReport {
    let mut report = ReadReport::default();
    // A UTF-8 byte order mark (Windows editors add one) is not part of line 1.
    let bytes = if at_file_start {
        bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)
    } else {
        bytes
    };
    let ends_with_newline = bytes.last() == Some(&b'\n');
    let mut chunks: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
    if ends_with_newline {
        chunks.pop();
    }
    let last = chunks.len().saturating_sub(1);
    for (i, raw) in chunks.iter().enumerate() {
        let line_no = i + 1;
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        if raw.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        // Only a line that is not even valid JSON can be a crash-cut tail; a
        // complete object with the wrong shape or version is a bad line.
        let decoded = std::str::from_utf8(raw)
            .map_err(|e| (format!("invalid UTF-8: {e}"), true))
            .and_then(|text| {
                serde_json::from_str::<Value>(text).map_err(|e| (e.to_string(), true))
            });
        let parsed = decoded.and_then(|value| parse_value(&value).map_err(|r| (r, false)));
        match parsed {
            Ok(line) => report.lines.push((line_no, line)),
            Err((reason, could_be_cut)) => {
                if could_be_cut && i == last && !ends_with_newline {
                    report.truncated_tail = true;
                } else {
                    report.bad.push(BadLine { line_no, reason });
                }
            }
        }
    }
    report
}

/// Reads a JSONL file from disk (see [`read_bytes`]).
pub fn read_file(path: impl AsRef<Path>) -> Result<ReadReport, StoreError> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).map_err(|e| StoreError::io(path, e))?;
    Ok(read_bytes(&bytes))
}

/// Append-only `captions.jsonl` v2 writer: one `write` per line straight to the
/// file (no buffering), so a crash loses at most the line being written.
pub struct CaptionWriter {
    path: PathBuf,
    file: File,
    next_seq: u64,
    /// A previous write failed part way: the file may end in a cut line, so the
    /// next line starts with a newline instead of being glued onto it.
    needs_newline: bool,
}

impl CaptionWriter {
    /// Creates a new file; fails if it already exists (a session never shares
    /// a file with another one).
    pub fn create_new(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| StoreError::io(&path, e))?;
        Ok(Self {
            path,
            file,
            next_seq: 1,
            needs_newline: false,
        })
    }

    /// Continues an existing file; `next_seq` must follow its last line
    /// (see [`ReadReport::next_seq`]).
    pub fn open_append(path: impl AsRef<Path>, next_seq: u64) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&path)
            .map_err(|e| StoreError::io(&path, e))?;
        // A crash can leave a last line without its newline; terminate it so the
        // first new line is not glued onto (and lost with) the cut one.
        let len = file.metadata().map_err(|e| StoreError::io(&path, e))?.len();
        if len > 0 {
            let mut last = [0u8; 1];
            file.seek(SeekFrom::End(-1))
                .and_then(|_| file.read_exact(&mut last))
                .map_err(|e| StoreError::io(&path, e))?;
            if last[0] != b'\n' {
                file.write_all(b"\n")
                    .map_err(|e| StoreError::io(&path, e))?;
            }
        }
        Ok(Self {
            path,
            file,
            next_seq,
            needs_newline: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The `seq` the next record will get.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Writes a reconciler record, asking `clock` for a stamp only when the
    /// record has none. Returns the `seq` it was written with.
    pub fn write_record(&mut self, record: &Record, clock: &dyn Clock) -> Result<u64, StoreError> {
        let seq = self.next_seq;
        let line = CaptionLine::from_record(seq, record, || clock.now());
        self.write_line(&line)?;
        Ok(seq)
    }

    /// Writes an already built line as-is (its `seq` is the caller's business).
    ///
    /// Non-finite numbers are refused before anything is written: JSON has no
    /// NaN/inf, and silently turning them into `null` would lose the value.
    /// The sequence number advances as soon as a write is attempted (like the
    /// Python recorder), so a failed write never makes two lines share a `seq`.
    pub fn write_line(&mut self, line: &CaptionLine) -> Result<(), StoreError> {
        let finite = line.t.is_finite()
            && line.mono_ms.is_finite()
            && line.read_ms.is_none_or(f64::is_finite);
        if !finite {
            return Err(StoreError::Format(format!(
                "line seq {} has a non-finite number (t, mono_ms or read_ms)",
                line.seq
            )));
        }
        let mut text = String::new();
        if self.needs_newline {
            text.push('\n');
        }
        text.push_str(&serde_json::to_string(&line.to_value())?);
        text.push('\n');

        self.next_seq = self.next_seq.max(line.seq + 1);
        match self.file.write_all(text.as_bytes()) {
            Ok(()) => {
                self.needs_newline = false;
                Ok(())
            }
            Err(e) => {
                self.needs_newline = true;
                Err(StoreError::io(&self.path, e))
            }
        }
    }
}
