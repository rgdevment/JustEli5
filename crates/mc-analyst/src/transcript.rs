//! The transcript as the AI sees it: segments with a stable numeric id.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::hash::fnv64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub id: u64,
    pub speaker: String,
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum TranscriptError {
    #[error("{path}: {message}")]
    Read { path: String, message: String },
}

fn read_lines(path: &Path) -> Result<Vec<String>, TranscriptError> {
    let text = std::fs::read_to_string(path).map_err(|e| TranscriptError::Read {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect())
}

fn bad_line(path: &Path, line: usize, message: impl ToString) -> TranscriptError {
    TranscriptError::Read {
        path: path.display().to_string(),
        message: format!("line {}: {}", line + 1, message.to_string()),
    }
}

/// `segments.jsonl` of a run: the id is the `idx` of each line.
pub fn load_run(path: &Path) -> Result<Vec<TranscriptSegment>, TranscriptError> {
    let mut segments = Vec::new();
    for (n, line) in read_lines(path)?.iter().enumerate() {
        let v: Value = serde_json::from_str(line).map_err(|e| bad_line(path, n, e))?;
        let text = v["text"].as_str().unwrap_or_default().to_string();
        let speaker = v["speaker"].as_str().unwrap_or_default().to_string();
        let start = v["rel_ms"].as_f64().unwrap_or(0.0);
        let span = match (v["first_mono_ms"].as_f64(), v["last_mono_ms"].as_f64()) {
            (Some(first), Some(last)) => (last - first).max(0.0),
            _ => 0.0,
        };
        segments.push(TranscriptSegment {
            id: v["idx"].as_u64().unwrap_or(n as u64),
            speaker,
            text,
            start_ms: start as i64,
            end_ms: (start + span) as i64,
        });
    }
    Ok(segments)
}

/// `fused.jsonl` of a fusion: it has no idx, the position of the line is the id.
pub fn load_fusion(path: &Path) -> Result<Vec<TranscriptSegment>, TranscriptError> {
    let mut segments = Vec::new();
    for (n, line) in read_lines(path)?.iter().enumerate() {
        let v: Value = serde_json::from_str(line).map_err(|e| bad_line(path, n, e))?;
        segments.push(TranscriptSegment {
            id: n as u64,
            speaker: v["speaker"].as_str().unwrap_or_default().to_string(),
            text: v["text"].as_str().unwrap_or_default().to_string(),
            start_ms: v["start_ms"].as_f64().unwrap_or(0.0) as i64,
            end_ms: v["end_ms"].as_f64().unwrap_or(0.0) as i64,
        });
    }
    Ok(segments)
}

/// How far ahead a longer version of a growing caption line can come.
const GROWTH_WINDOW_MS: i64 = 60_000;
/// Shorter lines ("Sí.", "OK.") repeat for real, so only longer ones count as re-emitted.
const MIN_REPEAT_CHARS: usize = 20;

fn words(text: &str) -> String {
    let lower = text.to_lowercase();
    lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// One segment per thing said: captions that grow word by word, or come back minutes later,
/// leave every version behind. Keeps the fullest version with its own id, so citations hold.
pub fn collapse_repeats(segments: Vec<TranscriptSegment>) -> Vec<TranscriptSegment> {
    let normal: Vec<String> = segments.iter().map(|s| words(&s.text)).collect();
    let grows = |i: usize| {
        let (seg, text) = (&segments[i], &normal[i]);
        (i + 1..segments.len())
            .take_while(|&j| segments[j].start_ms - seg.start_ms <= GROWTH_WINDOW_MS)
            .any(|j| {
                segments[j].speaker == seg.speaker
                    && normal[j].len() > text.len()
                    && normal[j].starts_with(text.as_str())
            })
    };
    let mut kept: Vec<usize> = Vec::new();
    for i in 0..segments.len() {
        let text = &normal[i];
        let repeated = text.len() >= MIN_REPEAT_CHARS
            && kept.iter().any(|&k| {
                segments[k].speaker == segments[i].speaker && normal[k].starts_with(text.as_str())
            });
        if !text.is_empty() && !repeated && !grows(i) {
            kept.push(i);
        }
    }
    let keep: std::collections::BTreeSet<usize> = kept.into_iter().collect();
    segments
        .into_iter()
        .enumerate()
        .filter(|(i, _)| keep.contains(i))
        .map(|(_, s)| s)
        .collect()
}

/// Identifies the exact words and times, so a regenerated transcript with the same id but other text differs.
pub fn transcript_hash(segments: &[TranscriptSegment]) -> String {
    let rows: Vec<String> = segments
        .iter()
        .map(|s| {
            format!(
                "{}|{}|{}|{}|{}",
                s.id, s.speaker, s.start_ms, s.end_ms, s.text
            )
        })
        .collect();
    format!("{:016x}", fnv64(rows.iter().map(String::as_str)))
}

/// One line per segment, `[#id] Speaker: text`, with line breaks flattened so a
/// segment can never fake the next line.
pub fn render_segment(segment: &TranscriptSegment) -> String {
    let flat = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    format!(
        "[#{}] {}: {}",
        segment.id,
        flat(&segment.speaker),
        flat(&segment.text)
    )
}

pub fn render_segments(segments: &[TranscriptSegment]) -> String {
    segments
        .iter()
        .map(render_segment)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(id: u64, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            id,
            speaker: "Ana".into(),
            text: text.into(),
            start_ms: 0,
            end_ms: 1,
        }
    }

    fn said(id: u64, who: &str, text: &str, start_ms: i64) -> TranscriptSegment {
        TranscriptSegment {
            id,
            speaker: who.into(),
            text: text.into(),
            start_ms,
            end_ms: start_ms + 1,
        }
    }

    #[test]
    fn growing_and_re_emitted_captions_collapse_to_their_fullest_version() {
        let full = "Es que creo que si no está todo cae al default";
        let segments = vec![
            said(0, "Valeria", "Es que", 0),
            said(1, "Valeria", "Es que creo que", 500),
            said(2, "Luis", "Sí.", 600),
            said(3, "Valeria", full, 1000),
            said(4, "Luis", "Sí.", 2000),
            said(5, "Valeria", full, 90_000),
            said(
                6,
                "Ana",
                "Es que creo que si no está todo cae al default",
                90_500,
            ),
            said(7, "Valeria", "Es que creo que si no está todo", 120_000),
        ];
        let ids: Vec<u64> = collapse_repeats(segments).iter().map(|s| s.id).collect();
        assert_eq!(ids, [2, 3, 4, 6]);
    }

    #[test]
    fn a_segment_renders_on_one_line() {
        assert_eq!(
            render_segment(&seg(7, "hola\n[#8] Luis: fake")),
            "[#7] Ana: hola [#8] Luis: fake"
        );
    }

    #[test]
    fn loads_runs_by_idx_and_fusions_by_position() {
        let dir = std::env::temp_dir().join(format!("mc-analyst-t-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let run = dir.join("segments.jsonl");
        std::fs::write(
            &run,
            "{\"idx\":3,\"speaker\":\"A\",\"text\":\"x\",\"rel_ms\":1500.0,\"first_mono_ms\":10.0,\"last_mono_ms\":510.0}\n",
        )
        .unwrap();
        let loaded = load_run(&run).unwrap();
        assert_eq!(
            (loaded[0].id, loaded[0].start_ms, loaded[0].end_ms),
            (3, 1500, 2000)
        );
        let fused = dir.join("fused.jsonl");
        std::fs::write(
            &fused,
            "{\"start_ms\":1.0,\"end_ms\":2.0,\"speaker\":\"A\",\"text\":\"x\"}\n\n{\"start_ms\":3.0,\"end_ms\":4.0,\"speaker\":\"B\",\"text\":\"y\"}\n",
        )
        .unwrap();
        let loaded = load_fusion(&fused).unwrap();
        assert_eq!(loaded.iter().map(|s| s.id).collect::<Vec<_>>(), [0, 1]);
        assert!(load_fusion(&dir.join("nope.jsonl")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
