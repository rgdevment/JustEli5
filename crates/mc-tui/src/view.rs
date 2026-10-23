//! Pure rendering of an [`App`] into a ratatui frame.

use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use unicode_width::UnicodeWidthChar;

use crate::app::{
    App, EndState, Modal, Phase, TistyField, Warning, stamp_label, text_lines, text_rows,
    text_width, wrap,
};
use crate::panel;

const PALETTE: [Color; 6] = [
    Color::Cyan,
    Color::Green,
    Color::Yellow,
    Color::Magenta,
    Color::Blue,
    Color::LightRed,
];
const NARROW: u16 = 50;

pub fn speaker_color(index: usize) -> Color {
    PALETTE[index % PALETTE.len()]
}

/// Cuts `text` to `width` columns, ending with `…` when it was longer.
pub fn truncate(text: &str, width: usize) -> String {
    if text_width(text) <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

fn timer(ms: f64) -> String {
    let total = (ms.max(0.0) / 1000.0) as u64;
    format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        total % 3600 / 60,
        total % 60
    )
}

/// Lines of the shown segments, wrapped to `width`.
fn transcript_lines(app: &App, segments: &[mc_core::Segment], width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for segment in segments {
        let color = speaker_color(app.speaker_index(&segment.speaker));
        let prefix = format!(
            "{} {}: ",
            stamp_label(segment),
            app.name_of(&segment.speaker)
        );
        let full = format!("{prefix}{}", segment.text);
        for (index, piece) in wrap(&full, width).into_iter().enumerate() {
            if index == 0 && piece.starts_with(&prefix) {
                let stamp = stamp_label(segment);
                let name_start = stamp.len() + 1;
                let name_end = prefix.len() - 2;
                lines.push(Line::from(vec![
                    Span::styled(stamp, Style::default().fg(Color::DarkGray)),
                    Span::raw(" "),
                    Span::styled(
                        prefix[name_start..name_end].to_string(),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(": "),
                    Span::raw(piece[prefix.len()..].to_string()),
                ]));
            } else if index == 0 {
                // The break fell inside "[mm:ss] Name: ": keep the speaker's colour.
                lines.push(Line::styled(piece, Style::default().fg(color)));
            } else {
                lines.push(Line::raw(piece));
            }
        }
    }
    lines
}

/// The `height` lines ending `back` lines above the bottom.
fn window(lines: Vec<Line<'static>>, height: usize, back: usize) -> Vec<Line<'static>> {
    let total = lines.len();
    let back = back.min(total.saturating_sub(height));
    let end = total - back;
    let start = end.saturating_sub(height);
    lines.into_iter().skip(start).take(end - start).collect()
}

fn header(app: &App, area: Rect, frame: &mut Frame) {
    let narrow = area.width < NARROW;
    let mut spans = Vec::new();
    match &app.phase {
        Phase::Live if app.recording && app.session_dir.is_some() => spans.push(Span::styled(
            if app.audio_rec {
                "● REC ♪"
            } else {
                "● REC"
            },
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        Phase::Live if app.recording => spans.push(Span::styled(
            "○ WAITING",
            Style::default().fg(Color::DarkGray),
        )),
        Phase::Live => spans.push(Span::styled("○ LIVE", Style::default().fg(Color::DarkGray))),
        Phase::End(_) => spans.push(Span::styled(
            format!("■ ENDED ({})", app.ended.as_deref().unwrap_or("?")),
            Style::default().add_modifier(Modifier::BOLD),
        )),
    }
    spans.push(Span::raw("  "));
    spans.push(Span::raw(app.source_state.clone()));
    if !narrow && let Some(consent) = &app.consent {
        spans.push(Span::styled(
            format!("  consent: {consent}"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    let [left, right] = Layout::horizontal([Constraint::Min(1), Constraint::Length(8)]).areas(area);
    let width = usize::from(left.width);
    let line = Line::from(spans);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    if text_width(&text) > width {
        frame.render_widget(Paragraph::new(truncate(&text, width)), left);
    } else {
        frame.render_widget(Paragraph::new(line), left);
    }
    frame.render_widget(Paragraph::new(timer(app.now_mono_ms)), right);
}

fn footer(app: &App, area: Rect, frame: &mut Frame) {
    let width = usize::from(area.width);
    let persistent = matches!(app.warning, Some(Warning::Write(_) | Warning::Source(_)));
    if let (Some(notice), false) = (&app.notice, persistent) {
        let line = Paragraph::new(truncate(notice, width)).style(Style::default().fg(Color::Cyan));
        frame.render_widget(line, area);
        return;
    }
    let (text, style) = match (&app.warning, &app.phase) {
        (Some(Warning::NoCaptions(secs)), Phase::Live) => (
            format!("no captions for {secs}s; are live captions on?"),
            Style::default().fg(Color::Yellow),
        ),
        (Some(Warning::Source(m)), Phase::Live) => (m.clone(), Style::default().fg(Color::Red)),
        (Some(Warning::Write(m)), Phase::Live) => {
            (format!("write error: {m}"), Style::default().fg(Color::Red))
        }
        (Some(Warning::Audio(m)), Phase::Live) => (m.clone(), Style::default().fg(Color::Yellow)),
        (_, Phase::Live) if app.modal.is_some() => (
            match app.modal {
                Some(Modal::PickSpeaker { .. }) => "↑↓ choose  Enter select  Esc cancel",
                _ => "Enter accept  Esc cancel",
            }
            .to_string(),
            Style::default().fg(Color::DarkGray),
        ),
        (_, Phase::Live) if app.stopping => ("stopping…".to_string(), Style::default()),
        (_, Phase::Live) if area.width < NARROW => ("q stop  ? help".to_string(), Style::default()),
        (_, Phase::Live) => (
            live_keys(app).to_string(),
            Style::default().fg(Color::DarkGray),
        ),
        (_, Phase::End(EndState::Menu { .. })) => (
            "↑↓ choose  Enter select  q exit".to_string(),
            Style::default().fg(Color::DarkGray),
        ),
        (_, Phase::End(EndState::Showing { .. })) => (
            "↑↓ PgUp/PgDn scroll  Esc back".to_string(),
            Style::default().fg(Color::DarkGray),
        ),
        (_, Phase::End(EndState::AskFolder { .. })) => (
            "Enter save  Esc back".to_string(),
            Style::default().fg(Color::DarkGray),
        ),
        (_, Phase::End(EndState::TistyForm { .. })) => (
            "Enter next/preview  Tab switch field  Esc back".to_string(),
            Style::default().fg(Color::DarkGray),
        ),
        (_, Phase::End(EndState::TistyConfirm { .. })) => (
            "↑↓ PgUp/PgDn scroll  Enter publish  Esc cancel".to_string(),
            Style::default().fg(Color::DarkGray),
        ),
        (_, Phase::End(EndState::TistyReport { .. })) => (
            "↑↓ PgUp/PgDn scroll  Enter back".to_string(),
            Style::default().fg(Color::DarkGray),
        ),
        (_, Phase::End(_)) => (
            "Enter back".to_string(),
            Style::default().fg(Color::DarkGray),
        ),
    };
    // A warning that persists keeps the footer; the notice rides in front of it.
    let text = match (&app.notice, persistent) {
        (Some(notice), true) => format!("{notice} · {text}"),
        _ => text,
    };
    frame.render_widget(Paragraph::new(truncate(&text, width)).style(style), area);
}

fn live_keys(app: &App) -> &'static str {
    match (app.recording, app.analyst) {
        (true, true) => {
            "q stop  p pause  n note  m mark  c fix  s name  l panel  ↑↓ scroll  ? help"
        }
        (true, false) => {
            "q stop  p pause  n note  m mark  c fix  s name  ↑↓ PgUp/PgDn scroll  ? help"
        }
        (false, _) => "q stop  p pause  ↑↓ PgUp/PgDn scroll  End follow  ? help",
    }
}

fn transcript(
    app: &App,
    area: Rect,
    frame: &mut Frame,
    segments: &[mc_core::Segment],
    back: usize,
    title: &str,
) {
    let block = Block::bordered().title(title.to_string());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let lines = transcript_lines(app, segments, usize::from(inner.width));
    let shown = window(lines, usize::from(inner.height), back);
    frame.render_widget(Paragraph::new(shown), inner);
}

/// The ledger next to the transcript: waiting, the items, and why the analyst last failed.
fn ledger_panel(app: &App, area: Rect, frame: &mut Frame) {
    let block = Block::bordered().title("Ledger");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let error = app
        .analyst_error
        .as_deref()
        .map(|e| wrap(&format!("! {e}"), usize::from(inner.width)))
        .unwrap_or_default();
    let error_height = error.len().min(3) as u16;
    let [body, foot] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(error_height)]).areas(inner);
    let lines = match &app.ledger {
        Some(ledger) => panel::lines(ledger, usize::from(body.width)),
        None => vec![Line::styled(
            "Waiting for the first window…",
            Style::default().fg(Color::DarkGray),
        )],
    };
    let height = usize::from(body.height);
    let start = app.panel_scroll.min(lines.len().saturating_sub(height));
    let shown: Vec<Line> = lines.into_iter().skip(start).take(height).collect();
    frame.render_widget(Paragraph::new(shown), body);
    let failed: Vec<Line> = error
        .into_iter()
        .take(3)
        .map(|l| Line::styled(l, Style::default().fg(Color::Red)))
        .collect();
    frame.render_widget(Paragraph::new(failed), foot);
}

fn live_body(app: &App, area: Rect, frame: &mut Frame) {
    let (area, side) = match app.panel_width() {
        Some(width) => {
            let [main, side] =
                Layout::horizontal([Constraint::Min(1), Constraint::Length(width)]).areas(area);
            (main, Some(side))
        }
        None => (area, None),
    };
    if let Some(side) = side {
        ledger_panel(app, side, frame);
    }
    let open_height = if app.open.is_empty() || area.height < 8 {
        0
    } else {
        app.open.len().min(3) as u16 + 2
    };
    let [main, open] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(open_height)]).areas(area);
    let title = if app.frozen.is_some() {
        "Transcript (paused)"
    } else {
        "Transcript"
    };
    transcript(
        app,
        main,
        frame,
        app.visible_segments(),
        app.scroll_back,
        title,
    );
    if open_height > 0 {
        let block = Block::bordered().title("Speaking now");
        let inner = block.inner(open);
        frame.render_widget(block, open);
        let width = usize::from(inner.width);
        let start = app.open.len().saturating_sub(3);
        let lines: Vec<Line> = app.open[start..]
            .iter()
            .map(|l| Line::raw(truncate(l, width)))
            .collect();
        frame.render_widget(Paragraph::new(lines), inner);
    }
}

fn end_body(app: &App, state: &EndState, area: Rect, frame: &mut Frame) {
    let width = usize::from(area.width.saturating_sub(4));
    match state {
        EndState::Menu { sel } => {
            let mut lines: Vec<Line> = vec![Line::raw("What now?"), Line::raw("")];
            for (index, item) in app.menu_items().into_iter().enumerate() {
                let marker = if index == *sel { "> " } else { "  " };
                let style = if index == *sel {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                lines.push(Line::styled(format!("{marker}{}", item.label()), style));
            }
            if app.finishing {
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    "Finishing the analysis… (any key skips)",
                    Style::default().fg(Color::Cyan),
                ));
            }
            // Why the capture ended badly, if it did (the footer only shows
            // warnings while live).
            let problem = match &app.warning {
                Some(Warning::Source(m)) => Some(m.clone()),
                Some(Warning::Write(m)) => Some(format!("write error: {m}")),
                _ => None,
            };
            let ended_badly = app
                .ended
                .as_deref()
                .is_some_and(|r| r != "stop" && r != "source_ended");
            if let Some(problem) = problem.filter(|_| ended_badly) {
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    truncate(&problem, width),
                    Style::default().fg(Color::Red),
                ));
            }
            if app.session_dir.is_none() {
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    "Nothing was recorded.",
                    Style::default().fg(Color::DarkGray),
                ));
            }
            frame.render_widget(
                Paragraph::new(lines).block(Block::bordered().title("Meeting ended")),
                area,
            );
        }
        EndState::Showing { scroll } => {
            transcript(app, area, frame, &app.segments, *scroll, "Transcript");
        }
        EndState::AskFolder { input } => {
            let lines = vec![
                Line::raw("Save the .md in this folder:"),
                Line::raw(""),
                Line::raw(format!("{}_", truncate(input, width.saturating_sub(1)))),
                Line::raw(""),
                Line::styled(
                    "An existing file is never overwritten.",
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            frame.render_widget(
                Paragraph::new(lines).block(Block::bordered().title("Save as .md")),
                area,
            );
        }
        EndState::Working(message) => frame.render_widget(
            Paragraph::new(truncate(message, width)).block(Block::bordered().title("Working")),
            area,
        ),
        EndState::Done(path) => {
            let lines = vec![
                Line::raw("Saved:"),
                Line::raw(truncate(&path.display().to_string(), width)),
            ];
            frame.render_widget(
                Paragraph::new(lines).block(Block::bordered().title("Done")),
                area,
            );
        }
        EndState::Error(message) => frame.render_widget(
            Paragraph::new(truncate(message, width))
                .style(Style::default().fg(Color::Red))
                .block(Block::bordered().title("Error")),
            area,
        ),
        EndState::TistyForm { target, field } => {
            let row = |label: &str, value: &str, active: bool| {
                let cursor = if active { "_" } else { "" };
                let shown = truncate(value, width.saturating_sub(label.len() + 3));
                Line::raw(format!("{label}: {shown}{cursor}"))
            };
            let lines = vec![
                Line::raw("Publish the minutes and the commitments to Tisty."),
                Line::raw(""),
                row(
                    "Folder (must exist)",
                    &target.folder,
                    *field == TistyField::Folder,
                ),
                row(
                    "List (empty: inbox)",
                    &target.list,
                    *field == TistyField::List,
                ),
                Line::raw(""),
                Line::styled(
                    "You will see what will be created before anything is sent.",
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            frame.render_widget(
                Paragraph::new(lines).block(Block::bordered().title("Save to Tisty")),
                area,
            );
        }
        EndState::TistyConfirm {
            preview, scroll, ..
        } => {
            let (mut lines, title) = text_window(app, preview, *scroll, true, "Confirm");
            lines.push(Line::raw(""));
            lines.push(Line::raw("Publish this to Tisty?"));
            frame.render_widget(
                Paragraph::new(lines).block(Block::bordered().title(title)),
                area,
            );
        }
        EndState::TistyReport { text, ok, scroll } => {
            let style = if *ok {
                Style::default()
            } else {
                Style::default().fg(Color::Red)
            };
            let (lines, title) = text_window(app, text, *scroll, false, "Tisty");
            frame.render_widget(
                Paragraph::new(lines)
                    .style(style)
                    .block(Block::bordered().title(title)),
                area,
            );
        }
    }
}

/// The rows of a long text from `scroll`, and a title that says which part is shown.
fn text_window(
    app: &App,
    text: &str,
    scroll: usize,
    with_question: bool,
    name: &str,
) -> (Vec<Line<'static>>, String) {
    let all = text_lines(app, text);
    let rows = text_rows(app, with_question);
    let first = scroll.min(all.len().saturating_sub(rows));
    let shown: Vec<Line> = all
        .iter()
        .skip(first)
        .take(rows)
        .cloned()
        .map(Line::raw)
        .collect();
    let title = if all.len() > rows {
        format!(
            "{name} (lines {}-{} of {})",
            first + 1,
            first + shown.len(),
            all.len()
        )
    } else {
        name.to_string()
    };
    (shown, title)
}

/// A box in the middle of `area`, cleared, or `None` when `area` is too small for it.
fn centered(area: Rect, width: u16, height: u16) -> Option<Rect> {
    let width = area.width.saturating_sub(2).min(width);
    let height = height.min(area.height);
    if width < 12 || height < 5 {
        return None;
    }
    Some(Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    })
}

fn help_overlay(app: &App, area: Rect, frame: &mut Frame) {
    let mut lines = vec![
        Line::raw("q / Ctrl-C   stop the capture"),
        Line::raw("p            pause / follow"),
        Line::raw("↑ ↓ PgUp PgDn scroll"),
        Line::raw("End          back to the newest"),
    ];
    if app.recording {
        lines.extend([
            Line::raw("n            add a note"),
            Line::raw("m            mark this moment"),
            Line::raw("c            correct a term"),
            Line::raw("s            rename a speaker"),
        ]);
    }
    if app.analyst {
        lines.extend([
            Line::raw("l            show / hide the ledger"),
            Line::raw("[ ]          scroll the ledger"),
        ]);
    }
    lines.push(Line::raw("any key      close this help"));
    let Some(rect) = centered(area, 50, lines.len() as u16 + 2) else {
        return;
    };
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title("Help")),
        rect,
    );
}

/// The question being answered: its title and the lines inside.
fn modal_text(app: &App, modal: &Modal) -> (&'static str, Vec<String>) {
    match modal {
        Modal::Note(input) => (
            "Note",
            vec![
                "Saved with this moment of the meeting:".into(),
                format!("{input}_"),
            ],
        ),
        Modal::TermHeard(input) => (
            "Correct a term (1/2)",
            vec!["What the captions say:".into(), format!("{input}_")],
        ),
        Modal::TermCorrect { heard, input } => (
            "Correct a term (2/2)",
            vec![
                format!("Instead of \"{heard}\" it is:"),
                format!("{input}_"),
            ],
        ),
        Modal::PickSpeaker { sel } => (
            "Rename a speaker",
            app.speakers
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let marker = if i == *sel { "> " } else { "  " };
                    format!("{marker}{}", app.name_of(s))
                })
                .collect(),
        ),
        Modal::RenameSpeaker { from, input } => (
            "Rename a speaker",
            vec![
                format!("New name for {}:", app.name_of(from)),
                format!("{input}_"),
            ],
        ),
    }
}

fn modal_overlay(app: &App, modal: &Modal, area: Rect, frame: &mut Frame) {
    let (title, text) = modal_text(app, modal);
    let Some(rect) = centered(area, 60, text.len() as u16 + 2) else {
        return;
    };
    let inner_width = usize::from(rect.width.saturating_sub(2));
    // The end of a long input is what the user is typing.
    let lines: Vec<Line> = text
        .iter()
        .map(|l| Line::raw(tail(l, inner_width)))
        .collect();
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(title)),
        rect,
    );
}

/// The last `width` columns of `text`.
fn tail(text: &str, width: usize) -> String {
    let mut kept: Vec<char> = Vec::new();
    let mut used = 0;
    for ch in text.chars().rev() {
        let w = ch.width().unwrap_or(0);
        if used + w > width {
            break;
        }
        kept.push(ch);
        used += w;
    }
    kept.iter().rev().collect()
}

pub fn view(app: &App, frame: &mut Frame) {
    let area = frame.area();
    if area.height < 3 || area.width < 10 {
        frame.render_widget(Paragraph::new("mc"), area);
        return;
    }
    let [head, body, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);
    header(app, head, frame);
    match &app.phase {
        Phase::Live => live_body(app, body, frame),
        Phase::End(state) => end_body(app, state, body, frame),
    }
    footer(app, foot, frame);
    if let Some(modal) = &app.modal {
        modal_overlay(app, modal, area, frame);
    }
    if app.help {
        help_overlay(app, area, frame);
    }
}

/// Renders into an off-screen buffer and returns it as text (one row per line,
/// wide characters counted once). Used by the snapshot tests.
pub fn render_text(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
    terminal.draw(|frame| view(app, frame)).expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        let mut skip = 0usize;
        let mut row = String::new();
        for x in 0..buffer.area.width {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let symbol = buffer[(x, y)].symbol();
            skip = text_width(symbol).saturating_sub(1);
            row.push_str(symbol);
        }
        out.push_str(row.trim_end());
        out.push('\n');
    }
    out
}
