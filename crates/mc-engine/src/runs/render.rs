//! Pure renderers for a run's outputs.

use std::collections::BTreeSet;

use mc_core::Segment;
use mc_store::manifest::Manifest;
use serde_json::{Value, json};

use crate::replay::ReplayError;
use crate::sidecar::SpeakerNames;

/// Session-relative time of a segment: when it started, else when it committed.
pub fn rel_ms(segment: &Segment) -> Option<f64> {
    segment
        .first_mono_ms
        .or(segment.committed_mono_ms)
        .filter(|v| v.is_finite())
        .map(|v| v.max(0.0))
}

/// `mm:ss`, or `h:mm:ss` from one hour on; `--:--` when unknown.
pub fn fmt_rel(ms: Option<f64>) -> String {
    let Some(ms) = ms.filter(|v| v.is_finite()) else {
        return "--:--".to_string();
    };
    let total = (ms.max(0.0) / 1000.0).floor() as u64;
    let (h, m, s) = (total / 3600, total % 3600 / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// Escapes Markdown syntax in transcript text (it is arbitrary speech).
pub fn md_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '\\' | '*' | '_' | '`' | '[' | ']' | '<' | '>' | '|') {
            out.push('\\');
        }
        if ch == '\n' || ch == '\r' {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    // A line that starts like block syntax must not become a heading/list.
    // Leading spaces would make an indented code block.
    let mut out = out.trim_start().to_string();
    let trimmed = out.as_str();
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    let numbered = digits > 0 && trimmed[digits..].starts_with(['.', ')']);
    if numbered {
        // CommonMark escapes the delimiter, not the digits: `1\. x`.
        out.insert(digits, '\\');
    } else if trimmed.starts_with(['#', '-', '+', '=']) {
        out.insert(0, '\\');
    }
    out
}

pub fn render_txt(segments: &[Segment]) -> String {
    segments.iter().map(|s| s.line() + "\n").collect()
}

pub struct MdMeta<'a> {
    pub manifest: &'a Manifest,
    pub run_id: &'a str,
    pub ended_cleanly: bool,
    pub synthetic_flush: bool,
    pub duration_ms: Option<i64>,
}

fn yaml_str(value: &str) -> String {
    // JSON strings are valid YAML double-quoted scalars, except that JSON leaves
    // some characters YAML treats as line breaks or rejects unescaped.
    let mut out = String::new();
    for ch in Value::String(value.to_string()).to_string().chars() {
        if matches!(ch, '\u{85}' | '\u{2028}' | '\u{2029}' | '\u{feff}')
            || ('\u{80}'..='\u{9f}').contains(&ch)
        {
            out.push_str(&format!("\\u{:04X}", ch as u32));
        } else {
            out.push(ch);
        }
    }
    out
}

pub fn render_md(meta: &MdMeta<'_>, segments: &[Segment]) -> String {
    let manifest = meta.manifest;
    let title = manifest
        .meeting_name
        .as_deref()
        .filter(|n| !n.is_empty())
        .unwrap_or(&manifest.session_id);
    let mut participants: Vec<&str> = Vec::new();
    let mut seen = BTreeSet::new();
    for segment in segments {
        if seen.insert(segment.speaker.as_str()) {
            participants.push(&segment.speaker);
        }
    }
    let mut md = String::from("---\n");
    md.push_str(&format!("session_id: {}\n", yaml_str(&manifest.session_id)));
    md.push_str(&format!("meeting_name: {}\n", yaml_str(title)));
    md.push_str(&format!(
        "started: {}\n",
        yaml_str(&manifest.started_utc_iso)
    ));
    md.push_str(&format!(
        "platform: {}\n",
        yaml_str(&manifest.platform_requested)
    ));
    let names: Vec<String> = participants.iter().map(|p| yaml_str(p)).collect();
    md.push_str(&format!("participants: [{}]\n", names.join(", ")));
    md.push_str(&format!(
        "duration: {}\n",
        yaml_str(&fmt_rel(meta.duration_ms.map(|d| d as f64)))
    ));
    md.push_str(&format!("run_id: {}\n", yaml_str(meta.run_id)));
    md.push_str(&format!("ended_cleanly: {}\n---\n\n", meta.ended_cleanly));
    md.push_str(&format!("# {}\n\n", md_escape(title)));
    if !meta.ended_cleanly {
        md.push_str(if meta.synthetic_flush {
            "> ⚠ La sesión no terminó limpiamente; las últimas líneas se confirmaron al cerrar el análisis.\n\n"
        } else {
            "> ⚠ La sesión no terminó limpiamente; puede faltar el final.\n\n"
        });
    }
    if segments.is_empty() {
        md.push_str("_Sin transcripción: no se confirmaron líneas._\n");
        return md;
    }
    let mut previous: Option<&str> = None;
    for segment in segments {
        if previous != Some(segment.speaker.as_str()) {
            if previous.is_some() {
                md.push('\n');
            }
            md.push_str(&format!(
                "**[{}] {}**\n\n",
                fmt_rel(rel_ms(segment)),
                md_escape(&segment.speaker)
            ));
            previous = Some(&segment.speaker);
        }
        md.push_str(&md_escape(&segment.text));
        md.push_str("  \n");
    }
    md
}

fn number(name: &str, value: Option<f64>) -> Result<Value, ReplayError> {
    match value {
        None => Ok(Value::Null),
        Some(v) if v.is_finite() => Ok(json!(v)),
        Some(_) => Err(ReplayError::Invalid(format!("{name} is not finite"))),
    }
}

/// One JSON object per segment, missing values as `null`.
/// `display_speaker` appears only for a speaker the user renamed; `speaker` is always what
/// the platform reported.
pub fn render_segments_jsonl(
    segments: &[Segment],
    names: &SpeakerNames,
) -> Result<String, ReplayError> {
    let mut out = String::new();
    for (idx, s) in segments.iter().enumerate() {
        let mut value = json!({
            "v": 1,
            "idx": idx,
            "speaker": s.speaker,
            "speaker_raw": s.speaker_raw,
            "text": s.text,
            "first_mono_ms": number("first_mono_ms", s.first_mono_ms)?,
            "last_mono_ms": number("last_mono_ms", s.last_mono_ms)?,
            "committed_mono_ms": number("committed_mono_ms", s.committed_mono_ms)?,
            "committed_wall_ms": s.committed_wall_ms,
            "rel_ms": number("rel_ms", rel_ms(s))?,
        });
        if let Some(shown) = names.get(&s.speaker) {
            value["display_speaker"] = json!(shown);
        }
        out.push_str(&value.to_string());
        out.push('\n');
    }
    Ok(out)
}
