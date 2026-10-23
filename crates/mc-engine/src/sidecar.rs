//! What the user adds to a session while it runs: notes, marks, term fixes and speaker
//! names. Small files next to the captions; none of them rewrites `captions.jsonl`.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use mc_core::Stamp;
use serde_json::{Value, json};

pub const NOTES_FILE: &str = "notes.jsonl";
pub const MARKS_FILE: &str = "marks.jsonl";
pub const TERM_FIXES_FILE: &str = "term_fixes.jsonl";
pub const SPEAKER_NAMES_FILE: &str = "speaker_names.json";

#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub mono_ms: f64,
    pub wall_ms: i64,
    pub text: String,
}

/// A term the user corrected: what the captions said and what it should be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermFix {
    pub heard: String,
    pub correct: String,
}

fn append(dir: &Path, file: &str, value: &Value) -> Result<(), String> {
    let path = dir.join(file);
    let mut line = value.to_string();
    line.push('\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        // One write per line, so a reader never sees half of one.
        .and_then(|mut f| f.write_all(line.as_bytes()))
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn read_lines(dir: &Path, file: &str) -> Vec<Value> {
    std::fs::read_to_string(dir.join(file))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

pub fn append_note(dir: &Path, at: Stamp, text: &str) -> Result<Note, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("the note is empty".to_string());
    }
    append(
        dir,
        NOTES_FILE,
        &json!({"mono_ms": at.mono_ms, "wall_ms": at.wall_ms, "text": text}),
    )?;
    Ok(Note {
        mono_ms: at.mono_ms,
        wall_ms: at.wall_ms,
        text: text.to_string(),
    })
}

pub fn append_mark(dir: &Path, at: Stamp) -> Result<(), String> {
    append(
        dir,
        MARKS_FILE,
        &json!({"mono_ms": at.mono_ms, "wall_ms": at.wall_ms}),
    )
}

pub fn append_term_fix(dir: &Path, at: Stamp, fix: &TermFix) -> Result<(), String> {
    if fix.heard.trim().is_empty() || fix.correct.trim().is_empty() {
        return Err("a correction needs both the heard and the correct term".to_string());
    }
    append(
        dir,
        TERM_FIXES_FILE,
        &json!({"mono_ms": at.mono_ms, "wall_ms": at.wall_ms,
                "heard": fix.heard.trim(), "correct": fix.correct.trim()}),
    )
}

pub fn read_notes(dir: &Path) -> Vec<Note> {
    read_lines(dir, NOTES_FILE)
        .iter()
        .filter_map(|v| {
            Some(Note {
                mono_ms: v["mono_ms"].as_f64()?,
                wall_ms: v["wall_ms"].as_i64()?,
                text: v["text"].as_str()?.to_string(),
            })
        })
        .collect()
}

/// When each mark was made, in the session's milliseconds, in order.
pub fn read_marks(dir: &Path) -> Vec<f64> {
    read_lines(dir, MARKS_FILE)
        .iter()
        .filter_map(|v| v["mono_ms"].as_f64())
        .collect()
}

pub fn read_term_fixes(dir: &Path) -> Vec<TermFix> {
    read_lines(dir, TERM_FIXES_FILE)
        .iter()
        .filter_map(|v| {
            Some(TermFix {
                heard: v["heard"].as_str()?.to_string(),
                correct: v["correct"].as_str()?.to_string(),
            })
        })
        .collect()
}

/// Speaker name as the captions gave it, to the name the user chose.
pub type SpeakerNames = BTreeMap<String, String>;

/// A missing or broken file is no mapping: the captions' own names show.
pub fn read_speaker_names(dir: &Path) -> SpeakerNames {
    std::fs::read_to_string(dir.join(SPEAKER_NAMES_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Replaces the whole file at once, so a reader never sees a half-written mapping.
pub fn set_speaker_name(dir: &Path, from: &str, to: &str) -> Result<SpeakerNames, String> {
    let to = to.trim();
    if to.is_empty() {
        return Err("the name is empty".to_string());
    }
    let mut names = read_speaker_names(dir);
    names.insert(from.to_string(), to.to_string());
    let path = dir.join(SPEAKER_NAMES_FILE);
    let tmp = dir.join(format!(".{SPEAKER_NAMES_FILE}.tmp"));
    let text = serde_json::to_string_pretty(&names).map_err(|e| e.to_string())? + "\n";
    std::fs::write(&tmp, text)
        .and_then(|()| std::fs::rename(&tmp, &path))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(names)
}

/// The name to show for `speaker`.
pub fn display_name<'a>(names: &'a SpeakerNames, speaker: &'a str) -> &'a str {
    names.get(speaker).map_or(speaker, String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: Stamp = Stamp {
        wall_ms: 1_700_000_000_123,
        mono_ms: 4500.5,
    };

    #[test]
    fn notes_marks_and_fixes_append_one_line_each_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        append_note(dir.path(), AT, "  revisar el pull request ").unwrap();
        append_note(dir.path(), AT, "otra").unwrap();
        append_mark(dir.path(), AT).unwrap();
        let fix = TermFix {
            heard: "meets".into(),
            correct: "Meet".into(),
        };
        append_term_fix(dir.path(), AT, &fix).unwrap();
        let notes = read_notes(dir.path());
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].text, "revisar el pull request");
        assert_eq!((notes[0].mono_ms, notes[0].wall_ms), (4500.5, AT.wall_ms));
        let mark = std::fs::read_to_string(dir.path().join(MARKS_FILE)).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&mark).unwrap(),
            json!({"mono_ms": 4500.5, "wall_ms": AT.wall_ms})
        );
        assert_eq!(read_term_fixes(dir.path()), [fix]);
    }

    #[test]
    fn empty_input_is_refused_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(append_note(dir.path(), AT, "  ").is_err());
        let blank = TermFix {
            heard: "x".into(),
            correct: " ".into(),
        };
        assert!(append_term_fix(dir.path(), AT, &blank).is_err());
        assert!(set_speaker_name(dir.path(), "Unknown", " ").is_err());
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    #[test]
    fn speaker_names_accumulate_and_a_broken_file_is_no_mapping() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_speaker_names(dir.path()).is_empty());
        set_speaker_name(dir.path(), "Unknown", "Mario").unwrap();
        let names = set_speaker_name(dir.path(), "Ana R.", "Ana").unwrap();
        assert_eq!(display_name(&names, "Unknown"), "Mario");
        assert_eq!(display_name(&names, "Luis"), "Luis");
        assert_eq!(read_speaker_names(dir.path()), names);
        std::fs::write(dir.path().join(SPEAKER_NAMES_FILE), "{broken").unwrap();
        assert!(read_speaker_names(dir.path()).is_empty());
    }
}
