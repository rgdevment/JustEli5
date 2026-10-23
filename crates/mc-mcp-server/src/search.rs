//! Case and accent insensitive text search over transcripts and minutes.

use serde_json::{Value, json};

use crate::meetings::{Meeting, Meetings};
use crate::transcript::Source;

const SNIPPET_CONTEXT: usize = 60;

/// One lowercase char per char (so positions in the folded text are positions in the original),
/// with the common Latin accents removed.
fn fold_char(c: char) -> char {
    let lower = c.to_lowercase().next().unwrap_or(c);
    match lower {
        'á' | 'à' | 'ä' | 'â' | 'ã' => 'a',
        'é' | 'è' | 'ë' | 'ê' => 'e',
        'í' | 'ì' | 'ï' | 'î' => 'i',
        'ó' | 'ò' | 'ö' | 'ô' | 'õ' => 'o',
        'ú' | 'ù' | 'ü' | 'û' => 'u',
        'ñ' => 'n',
        'ç' => 'c',
        other => other,
    }
}

pub fn fold(text: &str) -> Vec<char> {
    text.chars().map(fold_char).collect()
}

pub fn find(haystack: &[char], needle: &[char]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn snippet(text: &str, at: usize, len: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = at.saturating_sub(SNIPPET_CONTEXT);
    let end = (at + len + SNIPPET_CONTEXT).min(chars.len());
    let body: String = chars[start..end].iter().collect();
    let body = body.split_whitespace().collect::<Vec<_>>().join(" ");
    format!(
        "{}{body}{}",
        if start > 0 { "…" } else { "" },
        if end < chars.len() { "…" } else { "" }
    )
}

/// Matches of `query` in the committed lines and the minutes of one meeting.
pub fn search_meeting(
    meetings: &Meetings,
    meeting: &Meeting,
    query: &str,
    out: &mut Vec<Value>,
    limit: usize,
) {
    let needle = fold(query.trim());
    if needle.is_empty() {
        return;
    }
    let hit = |source: &str, idx: Option<u64>, speaker: Option<&str>, text: &str| {
        let at = find(&fold(text), &needle)?;
        Some(json!({
            "session": meeting.id(),
            "meeting_name": meeting.manifest.meeting_name,
            "source": source,
            "idx": idx,
            "transcript_source": idx.map(|_| "live"),
            "speaker": speaker,
            "snippet": snippet(text, at, needle.len()),
        }))
    };
    for line in meetings.lines(meeting, &Source::Live).unwrap_or_default() {
        if out.len() >= limit {
            return;
        }
        out.extend(hit(
            "transcript",
            Some(line.idx),
            Some(&line.speaker),
            &line.text,
        ));
    }
    if let Some((_, _, markdown)) = meeting.minutes() {
        for paragraph in markdown.lines() {
            if out.len() >= limit {
                return;
            }
            out.extend(hit("minutes", None, None, paragraph));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_ignores_case_and_accents() {
        assert_eq!(fold("Reunión Áurea ÑU"), fold("reunion aurea nu"));
    }

    #[test]
    fn snippet_trims_around_the_match() {
        let text = format!("{}needle{}", "a ".repeat(60), " b".repeat(60));
        let at = text.find("needle").unwrap();
        let s = snippet(&text, at, 6);
        assert!(s.starts_with('…') && s.ends_with('…') && s.contains("needle"));
    }
}
