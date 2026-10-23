use std::collections::BTreeMap;

use mc_reconcile::difflib;
use mc_store::SegmentRow;
use serde_json::{Value, json};

use crate::cli::{CliError, Ctx, Io, emit, open_store, resolve_one};

/// Beyond this many table cells only counts are compared.
const MAX_CELLS: usize = 4_000_000;
const MAX_SHOWN: usize = 5;
const CHANGED_RATIO: f64 = 0.8;
const MAX_RATIO_CHARS: usize = 20_000;
/// Beyond this many removed x added pairs in one gap, nothing is paired as changed.
const MAX_PAIRS: usize = 10_000;

type Entry = (String, String);

#[derive(Default)]
struct SegmentDiff {
    equal: usize,
    removed: Vec<Entry>,
    added: Vec<Entry>,
    changed: Vec<(Entry, Entry)>,
}

fn entries(rows: &[SegmentRow]) -> Vec<Entry> {
    rows.iter()
        .map(|r| (r.speaker.clone(), r.text.clone()))
        .collect()
}

/// LCS over (speaker, text); gaps between equal anchors become removed/added,
/// and similar removed/added pairs are reported as changed.
fn diff_segments(a: &[Entry], b: &[Entry]) -> Option<SegmentDiff> {
    let (n, m) = (a.len(), b.len());
    if (n + 1).saturating_mul(m + 1) > MAX_CELLS {
        return None;
    }
    let width = m + 1;
    let mut table = vec![0u32; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i * width + j] = if a[i] == b[j] {
                table[(i + 1) * width + j + 1] + 1
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }
    let mut diff = SegmentDiff::default();
    let (mut i, mut j) = (0, 0);
    let (mut gap_a, mut gap_b): (Vec<Entry>, Vec<Entry>) = (Vec::new(), Vec::new());
    let flush = |diff: &mut SegmentDiff, gap_a: &mut Vec<Entry>, gap_b: &mut Vec<Entry>| {
        let mut unmatched_b: Vec<Option<Entry>> = gap_b.drain(..).map(Some).collect();
        let pairable = gap_a.len().saturating_mul(unmatched_b.len()) <= MAX_PAIRS;
        for removed in gap_a.drain(..) {
            if !pairable {
                diff.removed.push(removed);
                continue;
            }
            let similar = unmatched_b.iter().position(|candidate| {
                candidate.as_ref().is_some_and(|c| {
                    c.0 == removed.0 && difflib::ratio(&removed.1, &c.1) >= CHANGED_RATIO
                })
            });
            match similar {
                Some(index) => {
                    let added = unmatched_b[index].take().unwrap_or_default();
                    diff.changed.push((removed, added));
                }
                None => diff.removed.push(removed),
            }
        }
        diff.added.extend(unmatched_b.into_iter().flatten());
    };
    while i < n && j < m {
        if a[i] == b[j] {
            flush(&mut diff, &mut gap_a, &mut gap_b);
            diff.equal += 1;
            i += 1;
            j += 1;
        } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            gap_a.push(a[i].clone());
            i += 1;
        } else {
            gap_b.push(b[j].clone());
            j += 1;
        }
    }
    gap_a.extend(a[i..].iter().cloned());
    gap_b.extend(b[j..].iter().cloned());
    flush(&mut diff, &mut gap_a, &mut gap_b);
    Some(diff)
}

fn speaker_counts(rows: &[SegmentRow]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for row in rows {
        *counts.entry(row.speaker.clone()).or_default() += 1;
    }
    counts
}

fn config_changes(a: &str, b: &str) -> Vec<(String, Value, Value)> {
    let (a, b): (Value, Value) = (
        serde_json::from_str(a).unwrap_or(Value::Null),
        serde_json::from_str(b).unwrap_or(Value::Null),
    );
    let mut keys: Vec<&String> = a
        .as_object()
        .into_iter()
        .chain(b.as_object())
        .flat_map(|o| o.keys())
        .collect();
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .filter(|k| a.get(k.as_str()) != b.get(k.as_str()))
        .map(|k| {
            (
                k.clone(),
                a.get(k.as_str()).cloned().unwrap_or(Value::Null),
                b.get(k.as_str()).cloned().unwrap_or(Value::Null),
            )
        })
        .collect()
}

fn show(entry: &Entry) -> String {
    format!("[{}]: {}", entry.0, entry.1)
}

pub fn run(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    let [first, second] = ctx.args.positional.as_slice() else {
        return Err(CliError::usage("usage: eli5 compare <runA> <runB>"));
    };
    let store = open_store(&ctx.cfg)?;
    let id_a = resolve_one("run", first, store.resolve_run(first)?)?;
    let id_b = resolve_one("run", second, store.resolve_run(second)?)?;
    let load = |id: &str| -> Result<_, CliError> {
        let row = store
            .run(id)?
            .ok_or_else(|| CliError::not_found(format!("run {id} is not indexed"), "mc status"))?;
        Ok((row, store.run_segments(id)?))
    };
    let ((row_a, segs_a), (row_b, segs_b)) = (load(&id_a)?, load(&id_b)?);
    let (ea, eb) = (entries(&segs_a), entries(&segs_b));
    let diff = diff_segments(&ea, &eb);
    let (counts_a, counts_b) = (speaker_counts(&segs_a), speaker_counts(&segs_b));
    let speakers: Vec<&String> = counts_a
        .keys()
        .chain(counts_b.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let config = config_changes(&row_a.config_json, &row_b.config_json);

    let joined = |e: &[Entry]| e.iter().map(show).collect::<Vec<_>>().join("\n");
    let (text_a, text_b) = (joined(&ea), joined(&eb));
    let ratio = (text_a.len() <= MAX_RATIO_CHARS && text_b.len() <= MAX_RATIO_CHARS)
        .then(|| difflib::ratio(&text_a, &text_b));

    let timings = |rows: &[SegmentRow]| -> Vec<[Option<u64>; 3]> {
        rows.iter()
            .map(|r| {
                [r.first_mono_ms, r.last_mono_ms, r.committed_mono_ms].map(|v| v.map(f64::to_bits))
            })
            .collect()
    };
    let identical = ea == eb
        && timings(&segs_a) == timings(&segs_b)
        && row_a.content_div == row_b.content_div
        && row_a.timing_div == row_b.timing_div
        && config.is_empty();
    let transcripts_equal = ea == eb;

    let mut human = format!("A {id_a}\nB {id_b}\n");
    human.push_str(&format!("segments: {} vs {}\n", ea.len(), eb.len()));
    human.push_str(&format!(
        "divergences: content {} vs {}, timing {} vs {}\n",
        row_a.content_div, row_b.content_div, row_a.timing_div, row_b.timing_div
    ));
    for speaker in &speakers {
        let (x, y) = (
            counts_a.get(*speaker).copied().unwrap_or(0),
            counts_b.get(*speaker).copied().unwrap_or(0),
        );
        human.push_str(&format!(
            "  {speaker}: {x} vs {y}{}\n",
            if x == y {
                String::new()
            } else {
                format!(" ({:+})", y as i64 - x as i64)
            }
        ));
    }
    for (key, x, y) in &config {
        human.push_str(&format!("config {key}: {x} vs {y}\n"));
    }
    if let Some(ratio) = ratio {
        human.push_str(&format!("transcript similarity: {ratio:.4}\n"));
    }
    match &diff {
        Some(d) => {
            human.push_str(&format!(
                "segments: {} equal, {} changed, {} only in A, {} only in B\n",
                d.equal,
                d.changed.len(),
                d.removed.len(),
                d.added.len()
            ));
            for (x, y) in d.changed.iter().take(MAX_SHOWN) {
                human.push_str(&format!("  ~ {}\n    -> {}\n", show(x), show(y)));
            }
            for x in d.removed.iter().take(MAX_SHOWN) {
                human.push_str(&format!("  - {}\n", show(x)));
            }
            for y in d.added.iter().take(MAX_SHOWN) {
                human.push_str(&format!("  + {}\n", show(y)));
            }
        }
        None => human.push_str("segment diff skipped: the runs are too large\n"),
    }
    human.push_str(if identical { "identical\n" } else { "differ\n" });

    let pairs = |list: &[Entry]| -> Vec<Value> {
        list.iter()
            .take(MAX_SHOWN)
            .map(|e| json!({"speaker": e.0, "text": e.1}))
            .collect()
    };
    let value = json!({
        "a": id_a,
        "b": id_b,
        "identical": identical,
        "transcripts_equal": transcripts_equal,
        "segments": [ea.len(), eb.len()],
        "content_div": [row_a.content_div, row_b.content_div],
        "timing_div": [row_a.timing_div, row_b.timing_div],
        "speakers": speakers.iter().map(|s| json!({
            "name": s,
            "a": counts_a.get(*s).copied().unwrap_or(0),
            "b": counts_b.get(*s).copied().unwrap_or(0),
        })).collect::<Vec<_>>(),
        "config": config.iter().map(|(k, x, y)| json!({"key": k, "a": x, "b": y})).collect::<Vec<_>>(),
        "similarity": ratio,
        "diff": diff.as_ref().map(|d| json!({
            "equal": d.equal,
            "changed": d.changed.len(),
            "only_a": d.removed.len(),
            "only_b": d.added.len(),
            "first_changed": d.changed.iter().take(MAX_SHOWN).map(|(x, y)| json!({
                "a": {"speaker": x.0, "text": x.1}, "b": {"speaker": y.0, "text": y.1}
            })).collect::<Vec<_>>(),
            "first_only_a": pairs(&d.removed),
            "first_only_b": pairs(&d.added),
        })),
    });
    emit(io, ctx.json, human, value)?;
    Ok(u8::from(!identical))
}
