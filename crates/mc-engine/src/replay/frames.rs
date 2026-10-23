//! Rebuilds the per-frame observations the reconciler saw from `captions.jsonl`.

use mc_core::{Decision, RawObservation, Stamp};
use mc_store::jsonl::{CaptionLine, ParsedLine, ReadReport};

use super::ReplayError;

#[derive(Debug, Clone, PartialEq)]
pub enum ReplayOp {
    Frame {
        frame: u64,
        stamp: Stamp,
        read_ms: Option<f64>,
        nodes: Vec<RawObservation>,
    },
    /// Out-of-frame records (the end-of-session flush).
    Flush { stamp: Stamp },
}

fn stamp_of(line: &CaptionLine) -> Stamp {
    Stamp {
        wall_ms: line.wall_ms,
        mono_ms: line.mono_ms,
    }
}

/// Records produced as a side effect of a node; they are not observations.
fn is_side_effect(decision: &Decision) -> bool {
    matches!(
        decision,
        Decision::Committed | Decision::DupCommittedSkip | Decision::Amended
    )
}

fn node_of(line: &CaptionLine, decision: &Decision) -> RawObservation {
    let text = match decision {
        // Python cleaned a whitespace-only text to "" and logged that; feeding
        // "" back would be classified `empty_text` instead.
        Decision::NoAlnum if line.text.is_empty() => " ".to_string(),
        _ => line.text.clone(),
    };
    RawObservation {
        speaker: Some(
            line.speaker_raw
                .clone()
                .unwrap_or_else(|| line.speaker.clone()),
        ),
        text: Some(text),
        node_id: None,
    }
}

/// Frames are skipped when silent (they left no record and changed no state),
/// so frame numbers may have gaps but must increase.
pub fn reconstruct(report: &ReadReport) -> Result<Vec<ReplayOp>, ReplayError> {
    let mut ops: Vec<ReplayOp> = Vec::new();
    let mut last_frame = 0u64;
    let mut flush_seen = false;

    for (line_no, parsed) in &report.lines {
        let line = match parsed {
            ParsedLine::V2(line) => line,
            ParsedLine::V1(_) => return Err(ReplayError::NoFrames),
        };
        let decision: Decision = line
            .decision
            .parse()
            .map_err(|e| ReplayError::Invalid(format!("line {line_no}: {e}")))?;

        match line.frame {
            None => {
                if !is_side_effect(&decision) {
                    return Err(ReplayError::Invalid(format!(
                        "line {line_no}: observation decision {:?} outside a frame",
                        line.decision
                    )));
                }
                if !flush_seen {
                    flush_seen = true;
                    ops.push(ReplayOp::Flush {
                        stamp: stamp_of(line),
                    });
                }
            }
            Some(frame) => {
                if flush_seen {
                    return Err(ReplayError::Invalid(format!(
                        "line {line_no}: frame record after the flush"
                    )));
                }
                let stamp = stamp_of(line);
                let same_frame = match ops.last() {
                    Some(ReplayOp::Frame {
                        frame: current,
                        stamp: current_stamp,
                        read_ms,
                        ..
                    }) if *current == frame => {
                        if *current_stamp != stamp || *read_ms != line.read_ms {
                            return Err(ReplayError::Invalid(format!(
                                "line {line_no}: frame {frame} has inconsistent stamps"
                            )));
                        }
                        true
                    }
                    _ => false,
                };
                if !same_frame {
                    if frame <= last_frame {
                        return Err(ReplayError::Invalid(format!(
                            "line {line_no}: frame {frame} does not follow {last_frame}"
                        )));
                    }
                    last_frame = frame;
                    ops.push(ReplayOp::Frame {
                        frame,
                        stamp,
                        read_ms: line.read_ms,
                        nodes: Vec::new(),
                    });
                }
                if !is_side_effect(&decision)
                    && let Some(ReplayOp::Frame { nodes, .. }) = ops.last_mut()
                {
                    nodes.push(node_of(line, &decision));
                }
            }
        }
    }
    Ok(ops)
}

#[cfg(test)]
mod tests {
    use mc_store::jsonl::read_bytes;

    use super::*;

    fn ops_of(jsonl: &str) -> Result<Vec<ReplayOp>, ReplayError> {
        reconstruct(&read_bytes(jsonl.as_bytes()))
    }

    fn frame_numbers(ops: &[ReplayOp]) -> Vec<u64> {
        ops.iter()
            .filter_map(|op| match op {
                ReplayOp::Frame { frame, .. } => Some(*frame),
                ReplayOp::Flush { .. } => None,
            })
            .collect()
    }

    fn nodes_of(op: &ReplayOp) -> &[RawObservation] {
        match op {
            ReplayOp::Frame { nodes, .. } => nodes,
            ReplayOp::Flush { .. } => panic!("expected a frame, got {op:?}"),
        }
    }

    #[test]
    fn frame_gaps_are_preserved() {
        let ops = ops_of(
            r#"{"v": 2, "seq": 1, "frame": 1, "t": 1.0, "wall_ms": 1000, "mono_ms": 1000.0, "read_ms": 5.0, "speaker": "Ana", "text": "Hola", "decision": "new_open"}
{"v": 2, "seq": 2, "frame": 4, "t": 2.0, "wall_ms": 2000, "mono_ms": 2000.0, "read_ms": 5.0, "speaker": "Ana", "text": "Hola a todos", "decision": "grow"}
{"v": 2, "seq": 3, "frame": 9, "t": 3.0, "wall_ms": 3000, "mono_ms": 3000.0, "read_ms": null, "speaker": "Ana", "text": "Hola a todos", "decision": "dup_exact"}
"#,
        )
        .unwrap();
        assert_eq!(frame_numbers(&ops), vec![1, 4, 9]);
        match &ops[2] {
            ReplayOp::Frame { read_ms, .. } => assert_eq!(*read_ms, None),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn committed_and_dup_committed_skip_are_not_nodes() {
        let ops = ops_of(
            r#"{"v": 2, "seq": 1, "frame": 3, "t": 3.0, "wall_ms": 3000, "mono_ms": 3000.0, "read_ms": 5.0, "speaker": "Ana", "text": "Hola a todos", "decision": "grow"}
{"v": 2, "seq": 2, "frame": 3, "t": 3.0, "wall_ms": 3000, "mono_ms": 3000.0, "read_ms": 5.0, "speaker": "Ana", "text": "Hola a todos", "decision": "committed", "first_mono_ms": 1000.0, "last_mono_ms": 3000.0}
{"v": 2, "seq": 3, "frame": 3, "t": 3.0, "wall_ms": 3000, "mono_ms": 3000.0, "read_ms": 5.0, "speaker": "Luis", "text": "Yo", "decision": "dup_committed_skip"}
{"v": 2, "seq": 4, "frame": null, "t": 4.0, "wall_ms": 4000, "mono_ms": 4000.0, "read_ms": null, "speaker": "Ana", "text": "Hola a todos", "decision": "committed"}
"#,
        )
        .unwrap();
        assert_eq!(ops.len(), 2);
        let nodes = nodes_of(&ops[0]);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].text.as_deref(), Some("Hola a todos"));
        assert!(matches!(ops[1], ReplayOp::Flush { .. }));
    }

    #[test]
    fn node_speaker_prefers_speaker_raw() {
        let ops = ops_of(
            r#"{"v": 2, "seq": 1, "frame": 1, "t": 1.0, "wall_ms": 1000, "mono_ms": 1000.0, "read_ms": 5.0, "speaker": "Ana Pérez", "text": "Hola", "decision": "new_open", "speaker_raw": "ana perez "}
{"v": 2, "seq": 2, "frame": 2, "t": 2.0, "wall_ms": 2000, "mono_ms": 2000.0, "read_ms": 5.0, "speaker": "Luis", "text": "Yo", "decision": "new_open"}
"#,
        )
        .unwrap();
        assert_eq!(nodes_of(&ops[0])[0].speaker.as_deref(), Some("ana perez "));
        assert_eq!(nodes_of(&ops[1])[0].speaker.as_deref(), Some("Luis"));
    }

    #[test]
    fn no_alnum_with_empty_text_becomes_a_space() {
        let ops = ops_of(
            r#"{"v": 2, "seq": 1, "frame": 1, "t": 1.0, "wall_ms": 1000, "mono_ms": 1000.0, "read_ms": 5.0, "speaker": "Ana", "text": "", "decision": "no_alnum"}
{"v": 2, "seq": 2, "frame": 1, "t": 1.0, "wall_ms": 1000, "mono_ms": 1000.0, "read_ms": 5.0, "speaker": "Ana", "text": "...", "decision": "no_alnum"}
{"v": 2, "seq": 3, "frame": 1, "t": 1.0, "wall_ms": 1000, "mono_ms": 1000.0, "read_ms": 5.0, "speaker": "Ana", "text": "", "decision": "empty_text"}
"#,
        )
        .unwrap();
        let texts: Vec<Option<&str>> = nodes_of(&ops[0])
            .iter()
            .map(|n| n.text.as_deref())
            .collect();
        assert_eq!(texts, vec![Some(" "), Some("..."), Some("")]);
    }

    #[test]
    fn inconsistent_stamps_within_a_frame_are_an_error() {
        let wall_differs = ops_of(
            r#"{"v": 2, "seq": 1, "frame": 1, "t": 1.0, "wall_ms": 1000, "mono_ms": 1000.0, "read_ms": 5.0, "speaker": "Ana", "text": "Hola", "decision": "new_open"}
{"v": 2, "seq": 2, "frame": 1, "t": 1.0, "wall_ms": 1001, "mono_ms": 1000.0, "read_ms": 5.0, "speaker": "Luis", "text": "Yo", "decision": "new_open"}
"#,
        );
        match wall_differs {
            Err(ReplayError::Invalid(msg)) => assert!(msg.contains("inconsistent"), "{msg}"),
            other => panic!("expected Invalid, got {other:?}"),
        }

        let read_differs = ops_of(
            r#"{"v": 2, "seq": 1, "frame": 1, "t": 1.0, "wall_ms": 1000, "mono_ms": 1000.0, "read_ms": 5.0, "speaker": "Ana", "text": "Hola", "decision": "new_open"}
{"v": 2, "seq": 2, "frame": 1, "t": 1.0, "wall_ms": 1000, "mono_ms": 1000.0, "read_ms": 6.0, "speaker": "Luis", "text": "Yo", "decision": "new_open"}
"#,
        );
        assert!(matches!(read_differs, Err(ReplayError::Invalid(_))));
    }

    #[test]
    fn non_increasing_frame_is_an_error() {
        let result = ops_of(
            r#"{"v": 2, "seq": 1, "frame": 2, "t": 1.0, "wall_ms": 1000, "mono_ms": 1000.0, "read_ms": 5.0, "speaker": "Ana", "text": "Hola", "decision": "new_open"}
{"v": 2, "seq": 2, "frame": 3, "t": 2.0, "wall_ms": 2000, "mono_ms": 2000.0, "read_ms": 5.0, "speaker": "Ana", "text": "Hola", "decision": "dup_exact"}
{"v": 2, "seq": 3, "frame": 2, "t": 3.0, "wall_ms": 3000, "mono_ms": 3000.0, "read_ms": 5.0, "speaker": "Ana", "text": "Hola", "decision": "dup_exact"}
"#,
        );
        assert!(matches!(result, Err(ReplayError::Invalid(_))));
    }

    #[test]
    fn observation_outside_a_frame_is_an_error() {
        let result = ops_of(
            r#"{"v": 2, "seq": 1, "frame": null, "t": 1.0, "wall_ms": 1000, "mono_ms": 1000.0, "read_ms": null, "speaker": "Ana", "text": "Hola", "decision": "grow"}
"#,
        );
        assert!(matches!(result, Err(ReplayError::Invalid(_))));
    }

    #[test]
    fn frame_after_flush_is_an_error() {
        let result = ops_of(
            r#"{"v": 2, "seq": 1, "frame": 1, "t": 1.0, "wall_ms": 1000, "mono_ms": 1000.0, "read_ms": 5.0, "speaker": "Ana", "text": "Hola", "decision": "new_open"}
{"v": 2, "seq": 2, "frame": null, "t": 2.0, "wall_ms": 2000, "mono_ms": 2000.0, "read_ms": null, "speaker": "Ana", "text": "Hola", "decision": "committed"}
{"v": 2, "seq": 3, "frame": 2, "t": 3.0, "wall_ms": 3000, "mono_ms": 3000.0, "read_ms": 5.0, "speaker": "Ana", "text": "Hola", "decision": "dup_exact"}
"#,
        );
        assert!(matches!(result, Err(ReplayError::Invalid(_))));
    }

    #[test]
    fn legacy_v1_line_is_no_frames() {
        let result = ops_of(
            r#"{"t": 1.0, "speaker": "Ana", "text": "Hola", "decision": "new_open"}
"#,
        );
        assert!(matches!(result, Err(ReplayError::NoFrames)));
    }

    #[test]
    fn empty_file_has_no_ops() {
        assert!(ops_of("").unwrap().is_empty());
        assert!(ops_of("\n  \n").unwrap().is_empty());
    }
}
