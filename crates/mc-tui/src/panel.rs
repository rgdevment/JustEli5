//! The ledger panel: what the analyst has understood so far, each item with the ids of the
//! segments it rests on.

use mc_analyst::{Ledger, TopicStatus};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

use crate::app::wrap;

/// Narrower terminals have no room for the panel next to the transcript.
pub const MIN_COLS: u16 = 100;
const MIN_WIDTH: u16 = 36;
const MAX_WIDTH: u16 = 60;
const MAX_IDS: usize = 5;

/// The panel's width on a terminal `total` columns wide, if it fits.
pub fn width_for(total: u16) -> Option<u16> {
    (total >= MIN_COLS).then(|| (total * 2 / 5).clamp(MIN_WIDTH, MAX_WIDTH))
}

/// `[#3 #5]`, cut after a few ids.
fn ids(evidence: &[u64]) -> String {
    let shown: Vec<String> = evidence
        .iter()
        .take(MAX_IDS)
        .map(|id| format!("#{id}"))
        .collect();
    let more = if evidence.len() > MAX_IDS { " …" } else { "" };
    format!("[{}{more}]", shown.join(" "))
}

fn heading(title: &str, count: Option<usize>) -> Line<'static> {
    let text = count.map_or_else(|| title.to_string(), |n| format!("{title} ({n})"));
    Line::styled(text, Style::default().add_modifier(Modifier::BOLD))
}

fn item(text: &str, evidence: &[u64], width: usize, out: &mut Vec<Line<'static>>) {
    let full = format!("{text} {}", ids(evidence));
    for (index, piece) in wrap(&full, width.saturating_sub(2).max(1))
        .into_iter()
        .enumerate()
    {
        let lead = if index == 0 { "• " } else { "  " };
        out.push(Line::raw(format!("{lead}{piece}")));
    }
}

fn none(out: &mut Vec<Line<'static>>) {
    out.push(Line::styled("—", Style::default().fg(Color::DarkGray)));
}

/// The panel's lines wrapped to `width` columns; the view scrolls through them.
pub fn lines(ledger: &Ledger, width: usize) -> Vec<Line<'static>> {
    let mut out = vec![heading("Topic", None)];
    let current = ledger
        .topics
        .iter()
        .rev()
        .find(|t| t.status == TopicStatus::Active);
    match current {
        Some(topic) => item(&topic.title, &topic.evidence, width, &mut out),
        None => none(&mut out),
    }
    out.push(heading("Decisions", Some(ledger.decisions.len())));
    for decision in &ledger.decisions {
        let text = if decision.inferred {
            format!("{} (inferred)", decision.text)
        } else {
            decision.text.clone()
        };
        item(&text, &decision.evidence, width, &mut out);
    }
    if ledger.decisions.is_empty() {
        none(&mut out);
    }
    out.push(heading("Commitments", Some(ledger.commitments.len())));
    for c in &ledger.commitments {
        let due = c
            .due
            .as_deref()
            .map_or(String::new(), |d| format!(" ({d})"));
        let inferred = if c.inferred { " (inferred)" } else { "" };
        let text = format!("{}: {}{due}{inferred}", c.who, c.what);
        item(&text, &c.evidence, width, &mut out);
    }
    if ledger.commitments.is_empty() {
        none(&mut out);
    }
    out.push(heading("Open questions", Some(ledger.open_questions.len())));
    for question in &ledger.open_questions {
        item(&question.text, &question.evidence, width, &mut out);
    }
    if ledger.open_questions.is_empty() {
        none(&mut out);
    }
    out
}
