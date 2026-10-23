//! `fused.jsonl`, `fused.txt` and `fused.md`.

use crate::fuse::FusedSegment;

fn clock(ms: f64) -> String {
    let total = (ms.max(0.0) / 1000.0) as u64;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

pub fn render_jsonl(segments: &[FusedSegment]) -> String {
    segments
        .iter()
        .map(|s| serde_json::to_string(s).expect("a fused segment serialises") + "\n")
        .collect()
}

/// `[speaker]: text`, the shape of the captions-only transcript.
pub fn render_txt(segments: &[FusedSegment]) -> String {
    segments
        .iter()
        .map(|s| format!("[{}]: {}\n", s.speaker, s.text))
        .collect()
}

pub fn render_md(segments: &[FusedSegment], title: &str) -> String {
    let mut out = format!("# {title}\n\n");
    for s in segments {
        let guess = if s.speaker_inferred {
            " *(sin nombre en Teams)*"
        } else {
            ""
        };
        out.push_str(&format!(
            "- **[{}] {}**{}: {}\n",
            clock(s.start_ms),
            s.speaker,
            guess,
            s.text
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fuse::Source;

    fn seg(start: f64, speaker: &str, text: &str, inferred: bool) -> FusedSegment {
        FusedSegment {
            start_ms: start,
            end_ms: start + 1000.0,
            speaker: speaker.into(),
            speaker_inferred: inferred,
            ch: "system".into(),
            text: text.into(),
            source: Source::Merged,
            caption_text: None,
            asr_text: None,
            edits: vec![],
        }
    }

    #[test]
    fn renders_the_three_formats() {
        let all = [
            seg(3_000.0, "Luis", "hola", false),
            seg(3_725_000.0, "Participante", "ok", true),
        ];
        assert_eq!(render_txt(&all), "[Luis]: hola\n[Participante]: ok\n");
        let md = render_md(&all, "Daily");
        assert!(
            md.starts_with("# Daily\n\n- **[00:03] Luis**: hola\n"),
            "{md}"
        );
        assert!(md.contains("[1:02:05] Participante**"), "{md}");
        assert!(md.contains("sin nombre en Teams"));
        let jsonl = render_jsonl(&all);
        assert_eq!(jsonl.lines().count(), 2);
        let first: serde_json::Value = serde_json::from_str(jsonl.lines().next().unwrap()).unwrap();
        assert_eq!(first["source"], "merged");
        assert_eq!(first["speaker"], "Luis");
    }
}
