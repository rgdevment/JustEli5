//! The TUI state and how messages change it. No I/O here.

use std::path::{Path, PathBuf};

use mc_analyst::Ledger;
use mc_core::Segment;
use mc_engine::live::LiveEvent;
use mc_engine::runs::{fmt_rel, rel_ms};
use mc_engine::sidecar::{SpeakerNames, display_name};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use unicode_width::UnicodeWidthChar;

use crate::panel;

/// Segments kept for display; the full transcript is on disk.
const MAX_SEGMENTS: usize = 5000;
/// Palette index range for speaker colours.
pub const PALETTE: usize = 6;

#[derive(Debug, Clone)]
pub enum Msg {
    Live(LiveEvent),
    Key(KeyEvent),
    /// Session-clock milliseconds, for the timer.
    Tick(f64),
    Resize(u16, u16),
    /// Something the user did that the loop carried out (or could not).
    Notice(String),
}

/// What the loop must do after an update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    None,
    Stop,
    /// A second quit while stopping: give up waiting for the capture.
    ForceQuit,
    Quit,
    SaveMd(PathBuf),
    /// Save a note at this moment.
    Note(String),
    Mark,
    FixTerm {
        heard: String,
        correct: String,
    },
    RenameSpeaker {
        from: String,
        to: String,
    },
    /// Build the plan for Tisty and show it for confirmation.
    TistyPreview(TistyTarget),
    /// The person confirmed: publish.
    TistyPublish(TistyTarget),
}

/// Where the minutes and commitments go in Tisty; an empty list means its inbox.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TistyTarget {
    pub folder: String,
    pub list: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TistyField {
    Folder,
    List,
}

/// A question the user is answering; it takes every key until it is answered or cancelled.
#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    Note(String),
    TermHeard(String),
    TermCorrect { heard: String, input: String },
    PickSpeaker { sel: usize },
    RenameSpeaker { from: String, input: String },
}

/// Longest text a note or a name may have.
const MAX_INPUT: usize = 300;

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Live,
    End(EndState),
}

#[derive(Debug, Clone, PartialEq)]
pub enum EndState {
    Menu {
        sel: usize,
    },
    Showing {
        scroll: usize,
    },
    AskFolder {
        input: String,
    },
    Working(String),
    Done(PathBuf),
    Error(String),
    TistyForm {
        target: TistyTarget,
        field: TistyField,
    },
    /// What will be created, waiting for the person to say yes.
    TistyConfirm {
        target: TistyTarget,
        preview: String,
        scroll: usize,
    },
    /// What Tisty did with each item; `ok` is false when something failed.
    TistyReport {
        text: String,
        ok: bool,
        scroll: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    Show,
    SaveMd,
    SaveTisty,
    Exit,
}

impl MenuItem {
    pub fn label(self) -> &'static str {
        match self {
            Self::Show => "Show the transcript here",
            Self::SaveMd => "Save as .md",
            Self::SaveTisty => "Save to Tisty",
            Self::Exit => "Exit",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Warning {
    NoCaptions(u64),
    Source(String),
    Write(String),
    /// An audio channel failed or was lost; captions keep going.
    Audio(String),
}

#[derive(Debug, Clone)]
pub struct App {
    pub phase: Phase,
    pub source_state: String,
    pub consent: Option<String>,
    pub session_dir: Option<PathBuf>,
    pub recording: bool,
    /// At least one audio channel is recording.
    pub audio_rec: bool,
    pub now_mono_ms: f64,
    pub segments: Vec<Segment>,
    pub open: Vec<String>,
    pub warning: Option<Warning>,
    /// Speakers in order of first appearance (their index picks the colour).
    pub speakers: Vec<String>,
    /// Number of segments shown while paused (`None`: follow the newest).
    pub frozen: Option<usize>,
    /// Wrapped lines scrolled up from the bottom of the shown segments.
    pub scroll_back: usize,
    pub help: bool,
    pub stopping: bool,
    pub size: (u16, u16),
    pub default_dir: PathBuf,
    /// Pre-filled answers of the Tisty form (from the configuration).
    pub tisty_default: TistyTarget,
    /// The configuration names the folder: saving goes straight to the preview.
    pub tisty_configured: bool,
    /// Why the capture ended, once it has.
    pub ended: Option<String>,
    /// The live analyst runs (the panel has something to show or wait for).
    pub analyst: bool,
    /// The meeting ended and the analyst is doing its last window.
    pub finishing: bool,
    /// What the user's last action did, apart from the warnings; it stays until the next key.
    pub notice: Option<String>,
    /// The user wants the panel (`l`); it still needs room.
    pub panel: bool,
    pub ledger: Option<Ledger>,
    pub analyst_error: Option<String>,
    /// Lines the panel is scrolled down from its top.
    pub panel_scroll: usize,
    pub modal: Option<Modal>,
    /// Names the user gave the speakers: shown instead of the captions' own.
    pub speaker_names: SpeakerNames,
}

impl App {
    pub fn new(
        recording: bool,
        consent: Option<String>,
        default_dir: PathBuf,
        size: (u16, u16),
    ) -> Self {
        Self {
            phase: Phase::Live,
            source_state: "starting".to_string(),
            consent,
            session_dir: None,
            recording,
            audio_rec: false,
            now_mono_ms: 0.0,
            segments: Vec::new(),
            open: Vec::new(),
            warning: None,
            speakers: Vec::new(),
            frozen: None,
            scroll_back: 0,
            help: false,
            stopping: false,
            size,
            default_dir,
            tisty_default: TistyTarget::default(),
            tisty_configured: false,
            ended: None,
            analyst: false,
            finishing: false,
            notice: None,
            panel: true,
            ledger: None,
            analyst_error: None,
            panel_scroll: 0,
            modal: None,
            speaker_names: SpeakerNames::new(),
        }
    }

    /// Closes the open question. What it held that is complete becomes the effect that saves it.
    pub fn close_modal(&mut self) -> Effect {
        match self.modal.take() {
            Some(Modal::Note(text)) if !text.trim().is_empty() => {
                Effect::Note(text.trim().to_string())
            }
            Some(Modal::TermCorrect { heard, input }) if !input.trim().is_empty() => {
                Effect::FixTerm {
                    heard,
                    correct: input.trim().to_string(),
                }
            }
            Some(Modal::RenameSpeaker { from, input }) if !input.trim().is_empty() => {
                let to = input.trim().to_string();
                self.speaker_names.insert(from.clone(), to.clone());
                Effect::RenameSpeaker { from, to }
            }
            Some(Modal::TermHeard(text)) if !text.trim().is_empty() => {
                self.notice = Some("an unfinished correction was discarded".to_string());
                Effect::None
            }
            _ => Effect::None,
        }
    }

    pub fn with_analyst(mut self, on: bool) -> Self {
        self.analyst = on;
        self
    }

    /// The name to show for a speaker.
    pub fn name_of<'a>(&'a self, speaker: &'a str) -> &'a str {
        display_name(&self.speaker_names, speaker)
    }

    /// The panel's width when it is shown.
    pub fn panel_width(&self) -> Option<u16> {
        if self.analyst && self.panel && matches!(self.phase, Phase::Live) {
            panel::width_for(self.size.0)
        } else {
            None
        }
    }

    /// Columns the transcript has (the panel takes the rest).
    pub fn main_width(&self) -> u16 {
        self.size.0.saturating_sub(self.panel_width().unwrap_or(0))
    }

    /// How far the panel can scroll: its lines minus the room it has.
    fn panel_max_scroll(&self) -> usize {
        let (Some(width), Some(ledger)) = (self.panel_width(), self.ledger.as_ref()) else {
            return 0;
        };
        let inner = usize::from(width.saturating_sub(2)).max(1);
        let room = usize::from(self.size.1.saturating_sub(4)).max(1);
        panel::lines(ledger, inner).len().saturating_sub(room)
    }

    /// Notes, marks and corrections need a session folder to land in.
    fn can_annotate(&self) -> bool {
        self.recording && self.session_dir.is_some()
    }

    pub fn with_tisty_default(mut self, folder: &str, list: &str) -> Self {
        self.tisty_default = TistyTarget {
            folder: folder.to_string(),
            list: list.to_string(),
        };
        self
    }

    pub fn with_tisty_configured(mut self, configured: bool) -> Self {
        self.tisty_configured = configured;
        self
    }

    pub fn speaker_index(&self, speaker: &str) -> usize {
        self.speakers.iter().position(|s| s == speaker).unwrap_or(0) % PALETTE
    }

    pub fn menu_items(&self) -> Vec<MenuItem> {
        if self.session_dir.is_some() {
            vec![
                MenuItem::Show,
                MenuItem::SaveMd,
                MenuItem::SaveTisty,
                MenuItem::Exit,
            ]
        } else {
            vec![MenuItem::Show, MenuItem::Exit]
        }
    }

    /// Segments the transcript pane shows.
    pub fn visible_segments(&self) -> &[Segment] {
        let end = self
            .frozen
            .unwrap_or(self.segments.len())
            .min(self.segments.len());
        &self.segments[..end]
    }

    /// How far the transcript can be scrolled up: wrapped lines minus the
    /// pane's height (the same geometry the view uses).
    pub fn max_back(&self, segments: &[Segment]) -> usize {
        let width = usize::from(self.main_width().saturating_sub(2)).max(1);
        let height = usize::from(self.size.1.saturating_sub(4)).max(1);
        let total: usize = segments
            .iter()
            .map(|s| {
                wrap(
                    &format!(
                        "{} {}: {}",
                        stamp_label(s),
                        self.name_of(&s.speaker),
                        s.text
                    ),
                    width,
                )
                .len()
            })
            .sum();
        total.saturating_sub(height)
    }

    pub fn elapsed_ms(&self) -> f64 {
        self.now_mono_ms
    }

    fn live_event(&mut self, event: LiveEvent) {
        match event {
            LiveEvent::Started {
                session_dir,
                source,
                ..
            } => {
                self.session_dir = session_dir;
                self.source_state = format!("starting {source}");
            }
            LiveEvent::Session { dir, .. } => self.session_dir = Some(dir),
            LiveEvent::Source(state) => {
                self.source_state = match state {
                    mc_captions::Discovery::Searching => "searching".to_string(),
                    mc_captions::Discovery::Attached { window } => format!("attached: {window}"),
                    mc_captions::Discovery::Ended => "ended".to_string(),
                };
            }
            LiveEvent::Segment { segment, .. } => {
                if !self.speakers.contains(&segment.speaker) {
                    self.speakers.push(segment.speaker.clone());
                }
                self.segments.push(segment);
                if self.segments.len() > MAX_SEGMENTS {
                    self.segments.remove(0);
                    if let Some(frozen) = self.frozen.as_mut() {
                        *frozen = frozen.saturating_sub(1);
                    }
                }
                // A write error stays until the capture ends: lines are being lost.
                if !matches!(self.warning, Some(Warning::Write(_))) {
                    self.warning = None;
                }
            }
            LiveEvent::Open(lines) => self.open = lines,
            LiveEvent::NoCaptions { secs } => self.warning = Some(Warning::NoCaptions(secs)),
            LiveEvent::WriteError(m) => self.warning = Some(Warning::Write(m)),
            LiveEvent::SourceError(m) => self.warning = Some(Warning::Source(m)),
            LiveEvent::Rec(on) => self.audio_rec = on,
            LiveEvent::Ledger(update) => {
                self.ledger = Some(update.ledger);
                self.analyst_error = None;
            }
            LiveEvent::AnalystError(message) => self.analyst_error = Some(message),
            LiveEvent::Note(_) => self.notice = Some("note saved".to_string()),
            LiveEvent::Fixed { heard, correct } => {
                self.notice = Some(format!("{heard} -> {correct}"));
            }
            LiveEvent::Named { from, to } => self.notice = Some(format!("{from} is {to}")),
            LiveEvent::Mark { mono_ms, .. } => {
                self.notice = Some(format!("moment marked at {}", fmt_rel(Some(mono_ms))));
            }
            // The live draft goes to asr_live.jsonl; the screen keeps to the captions.
            LiveEvent::Asr(_) => {}
            LiveEvent::Audio(m) => {
                if !matches!(self.warning, Some(Warning::Write(_) | Warning::Source(_))) {
                    self.warning = Some(Warning::Audio(m));
                }
            }
            LiveEvent::Ended { reason, .. } => {
                self.ended = Some(reason.as_str().to_string());
                self.source_state = "ended".to_string();
                self.open.clear();
                self.help = false;
                self.frozen = None;
                self.scroll_back = 0;
                self.phase = Phase::End(EndState::Menu { sel: 0 });
            }
        }
    }
}

/// Pure transition.
pub fn update(app: &mut App, msg: Msg) -> Effect {
    match msg {
        Msg::Live(event) => {
            let ending = matches!(event, LiveEvent::Ended { .. });
            app.live_event(event);
            // What was being typed when the meeting ended is still worth keeping.
            if ending {
                app.close_modal()
            } else {
                Effect::None
            }
        }
        Msg::Tick(mono_ms) => {
            app.now_mono_ms = mono_ms;
            Effect::None
        }
        Msg::Resize(w, h) => {
            app.size = (w, h);
            Effect::None
        }
        Msg::Notice(text) => {
            app.notice = Some(text);
            Effect::None
        }
        Msg::Key(key) => {
            // Windows delivers Press and Release (and Repeat): act on Press only.
            if key.kind != KeyEventKind::Press {
                return Effect::None;
            }
            on_key(app, key)
        }
    }
}

/// Control without Alt: Windows reports AltGr as Ctrl+Alt, and that is a character, not a shortcut.
fn is_ctrl(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT)
}

fn types_text(key: &KeyEvent) -> bool {
    !is_ctrl(key)
}

fn is_quit(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('q')) || (key.code == KeyCode::Char('c') && is_ctrl(key))
}

fn on_key(app: &mut App, key: KeyEvent) -> Effect {
    let ctrl_c = key.code == KeyCode::Char('c') && is_ctrl(&key);
    if app.help && !ctrl_c {
        app.help = false;
        return Effect::None;
    }
    app.help = false;
    app.notice = None;
    if let Some(modal) = app.modal.take() {
        if ctrl_c {
            // Leaving is never blocked by a question.
            return on_key_live(app, key);
        }
        return on_key_modal(app, modal, key);
    }
    match app.phase.clone() {
        Phase::Live => on_key_live(app, key),
        Phase::End(state) => on_key_end(app, state, key),
    }
}

fn on_key_live(app: &mut App, key: KeyEvent) -> Effect {
    if is_quit(&key) {
        if app.stopping {
            // Stop was asked already and the capture has not ended: the user
            // may leave without waiting for a source that is stuck.
            return Effect::ForceQuit;
        }
        app.stopping = true;
        return Effect::Stop;
    }
    match key.code {
        KeyCode::Char('?') => app.help = true,
        KeyCode::Char('l') => toggle_panel(app),
        KeyCode::Char('[') => app.panel_scroll = app.panel_scroll.saturating_sub(1),
        KeyCode::Char(']') => {
            app.panel_scroll = (app.panel_scroll + 1).min(app.panel_max_scroll());
        }
        KeyCode::Char('n') if annotating(app) => app.modal = Some(Modal::Note(String::new())),
        KeyCode::Char('c') if annotating(app) => {
            app.modal = Some(Modal::TermHeard(String::new()));
        }
        KeyCode::Char('m') if annotating(app) => return Effect::Mark,
        KeyCode::Char('s') if annotating(app) => {
            if app.speakers.is_empty() {
                notice(app, "no speaker has talked yet");
            } else {
                app.modal = Some(Modal::PickSpeaker { sel: 0 });
            }
        }
        KeyCode::Char('n' | 'm' | 'c' | 's') => {}
        KeyCode::Char('p') => {
            if app.frozen.is_some() {
                app.frozen = None;
                app.scroll_back = 0;
            } else {
                app.frozen = Some(app.segments.len());
            }
        }
        KeyCode::Up => scroll_up(app, 1),
        KeyCode::PageUp => scroll_up(app, usize::from(app.size.1.saturating_sub(6)).max(1)),
        KeyCode::Down => app.scroll_back = app.scroll_back.saturating_sub(1),
        KeyCode::PageDown => {
            app.scroll_back = app
                .scroll_back
                .saturating_sub(usize::from(app.size.1.saturating_sub(6)).max(1));
        }
        KeyCode::End => {
            app.frozen = None;
            app.scroll_back = 0;
        }
        _ => {}
    }
    Effect::None
}

fn notice(app: &mut App, text: &str) {
    app.notice = Some(text.to_string());
}

fn toggle_panel(app: &mut App) {
    if app.analyst {
        app.panel = !app.panel;
    } else {
        notice(app, "the analyst is off: start with --analyst");
    }
}

/// Whether n, m, c and s work now; says why when they do not.
fn annotating(app: &mut App) -> bool {
    let ok = app.can_annotate();
    if !ok {
        notice(
            app,
            "nothing is being recorded yet: there is no session folder",
        );
    }
    ok
}

/// A key inside a question. Returns what the loop must carry out once it is answered.
fn on_key_modal(app: &mut App, modal: Modal, key: KeyEvent) -> Effect {
    if key.code == KeyCode::Esc {
        return Effect::None;
    }
    if let Modal::PickSpeaker { sel } = modal {
        return pick_speaker(app, sel, key);
    }
    let (mut input, ask) = match &modal {
        Modal::Note(input) | Modal::TermHeard(input) => (input.clone(), None),
        Modal::TermCorrect { heard, input } => (input.clone(), Some(heard.clone())),
        Modal::RenameSpeaker { from, input } => (input.clone(), Some(from.clone())),
        Modal::PickSpeaker { .. } => return Effect::None,
    };
    match key.code {
        KeyCode::Enter if input.trim().is_empty() => {}
        KeyCode::Enter => return answered(app, &modal, input.trim().to_string()),
        KeyCode::Backspace => {
            input.pop();
        }
        KeyCode::Char(c) if types_text(&key) && input.chars().count() < MAX_INPUT => {
            input.push(c);
        }
        _ => {}
    }
    let ask = ask.unwrap_or_default();
    app.modal = Some(match modal {
        Modal::Note(_) => Modal::Note(input),
        Modal::TermHeard(_) => Modal::TermHeard(input),
        Modal::TermCorrect { .. } => Modal::TermCorrect { heard: ask, input },
        Modal::RenameSpeaker { .. } => Modal::RenameSpeaker { from: ask, input },
        Modal::PickSpeaker { sel } => Modal::PickSpeaker { sel },
    });
    Effect::None
}

fn pick_speaker(app: &mut App, sel: usize, key: KeyEvent) -> Effect {
    let count = app.speakers.len().max(1);
    let sel = match key.code {
        KeyCode::Up => (sel + count - 1) % count,
        KeyCode::Down => (sel + 1) % count,
        KeyCode::Enter => {
            if let Some(from) = app.speakers.get(sel).cloned() {
                app.modal = Some(Modal::RenameSpeaker {
                    from,
                    input: String::new(),
                });
            }
            return Effect::None;
        }
        _ => sel,
    };
    app.modal = Some(Modal::PickSpeaker { sel });
    Effect::None
}

/// The text of a question was confirmed.
fn answered(app: &mut App, modal: &Modal, text: String) -> Effect {
    match modal {
        Modal::Note(_) => Effect::Note(text),
        Modal::TermHeard(_) => {
            app.modal = Some(Modal::TermCorrect {
                heard: text,
                input: String::new(),
            });
            Effect::None
        }
        Modal::TermCorrect { heard, .. } => Effect::FixTerm {
            heard: heard.clone(),
            correct: text,
        },
        Modal::RenameSpeaker { from, .. } => {
            app.speaker_names.insert(from.clone(), text.clone());
            Effect::RenameSpeaker {
                from: from.clone(),
                to: text,
            }
        }
        Modal::PickSpeaker { .. } => Effect::None,
    }
}

fn scroll_up(app: &mut App, lines: usize) {
    // Scrolling back pauses following, so new text does not drag the view.
    if app.frozen.is_none() {
        app.frozen = Some(app.segments.len());
    }
    let max = app.max_back(app.visible_segments());
    app.scroll_back = (app.scroll_back + lines).min(max);
}

/// Scrolls the full-transcript pager, never past its top.
fn showing(app: &mut App, scroll: usize) {
    let max = app.max_back(&app.segments);
    set_end(
        app,
        EndState::Showing {
            scroll: scroll.min(max),
        },
    );
}

fn set_end(app: &mut App, state: EndState) {
    app.phase = Phase::End(state);
}

fn on_key_end(app: &mut App, state: EndState, key: KeyEvent) -> Effect {
    // Ctrl-C is the user's way out from every screen but a running save.
    let ctrl_c = key.code == KeyCode::Char('c') && is_ctrl(&key);
    if ctrl_c && !matches!(state, EndState::Working(_)) {
        return Effect::Quit;
    }
    match state {
        EndState::Menu { sel } => {
            let items = app.menu_items();
            let count = items.len();
            match key.code {
                KeyCode::Up => set_end(
                    app,
                    EndState::Menu {
                        sel: (sel + count - 1) % count,
                    },
                ),
                KeyCode::Down => set_end(
                    app,
                    EndState::Menu {
                        sel: (sel + 1) % count,
                    },
                ),
                KeyCode::Char('q') | KeyCode::Esc => return Effect::Quit,
                KeyCode::Char('c') if is_ctrl(&key) => {
                    return Effect::Quit;
                }
                KeyCode::Enter => match items.get(sel).copied() {
                    Some(MenuItem::Show) => set_end(app, EndState::Showing { scroll: 0 }),
                    Some(MenuItem::SaveMd) => {
                        let input = app.default_dir.display().to_string();
                        set_end(app, EndState::AskFolder { input });
                    }
                    Some(MenuItem::SaveTisty) if app.tisty_configured => {
                        let message = "preparing the minutes (this can take a while)...";
                        set_end(app, EndState::Working(message.to_string()));
                        return Effect::TistyPreview(app.tisty_default.clone());
                    }
                    Some(MenuItem::SaveTisty) => {
                        let target = app.tisty_default.clone();
                        let field = TistyField::Folder;
                        set_end(app, EndState::TistyForm { target, field });
                    }
                    Some(MenuItem::Exit) | None => return Effect::Quit,
                },
                _ => {}
            }
        }
        EndState::Showing { scroll } => match key.code {
            KeyCode::Esc | KeyCode::Char('q') => set_end(app, EndState::Menu { sel: 0 }),
            KeyCode::Up => showing(app, scroll.saturating_add(1)),
            KeyCode::Down => showing(app, scroll.saturating_sub(1)),
            KeyCode::PageUp => showing(app, scroll.saturating_add(10)),
            KeyCode::PageDown => showing(app, scroll.saturating_sub(10)),
            KeyCode::Home => showing(app, usize::MAX),
            KeyCode::End => set_end(app, EndState::Showing { scroll: 0 }),
            _ => {}
        },
        EndState::AskFolder { mut input } => match key.code {
            KeyCode::Esc => set_end(app, EndState::Menu { sel: 1 }),
            KeyCode::Enter => {
                let dir = input.trim();
                if dir.is_empty() {
                    return Effect::None;
                }
                let dir = PathBuf::from(dir);
                set_end(
                    app,
                    EndState::Working("running the analysis and saving...".to_string()),
                );
                return Effect::SaveMd(dir);
            }
            KeyCode::Backspace => {
                input.pop();
                set_end(app, EndState::AskFolder { input });
            }
            KeyCode::Char(c) if types_text(&key) => {
                input.push(c);
                set_end(app, EndState::AskFolder { input });
            }
            _ => {}
        },
        EndState::TistyForm { target, field } => return on_key_tisty_form(app, target, field, key),
        EndState::TistyConfirm {
            target,
            preview,
            scroll,
        } => match key.code {
            code if scroll_key(code).is_some() => {
                let scroll = scrolled(app, &preview, true, scroll, code);
                set_end(
                    app,
                    EndState::TistyConfirm {
                        target,
                        preview,
                        scroll,
                    },
                );
            }
            KeyCode::Enter | KeyCode::Char('y') => {
                set_end(app, EndState::Working("publishing to Tisty...".to_string()));
                return Effect::TistyPublish(target);
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q') => {
                set_end(app, EndState::Menu { sel: 2 });
            }
            _ => {}
        },
        EndState::TistyReport { text, ok, scroll } => match key.code {
            code if scroll_key(code).is_some() => {
                let scroll = scrolled(app, &text, false, scroll, code);
                set_end(app, EndState::TistyReport { text, ok, scroll });
            }
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                set_end(app, EndState::Menu { sel: 0 });
            }
            _ => {}
        },
        EndState::Done(_) | EndState::Error(_) => match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                set_end(app, EndState::Menu { sel: 0 });
            }
            _ => {}
        },
        EndState::Working(_) => {}
    }
    Effect::None
}

/// Lines of a long text as the Tisty screens show them (wrapped to the window).
pub fn text_lines(app: &App, text: &str) -> Vec<String> {
    let width = usize::from(app.size.0.saturating_sub(4));
    text.lines().flat_map(|l| wrap(l, width)).collect()
}

/// Rows the text gets: the window minus header, footer and borders (and the question).
pub fn text_rows(app: &App, with_question: bool) -> usize {
    usize::from(app.size.1.saturating_sub(if with_question { 6 } else { 4 })).max(1)
}

fn scroll_key(code: KeyCode) -> Option<()> {
    matches!(
        code,
        KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Home
            | KeyCode::End
    )
    .then_some(())
}

/// The first row shown after `code`, never past the last full page.
fn scrolled(app: &App, text: &str, with_question: bool, scroll: usize, code: KeyCode) -> usize {
    let rows = text_rows(app, with_question);
    let last = text_lines(app, text).len().saturating_sub(rows);
    let page = rows.saturating_sub(1).max(1);
    let next = match code {
        KeyCode::Up => scroll.saturating_sub(1),
        KeyCode::Down => scroll + 1,
        KeyCode::PageUp => scroll.saturating_sub(page),
        KeyCode::PageDown => scroll + page,
        KeyCode::Home => 0,
        _ => last,
    };
    next.min(last)
}

fn on_key_tisty_form(
    app: &mut App,
    mut target: TistyTarget,
    field: TistyField,
    key: KeyEvent,
) -> Effect {
    match key.code {
        KeyCode::Esc => {
            set_end(app, EndState::Menu { sel: 2 });
            return Effect::None;
        }
        KeyCode::Tab | KeyCode::Up | KeyCode::Down => {
            let field = match field {
                TistyField::Folder => TistyField::List,
                TistyField::List => TistyField::Folder,
            };
            set_end(app, EndState::TistyForm { target, field });
            return Effect::None;
        }
        KeyCode::Enter if target.folder.trim().is_empty() => {}
        KeyCode::Enter if field == TistyField::Folder => {
            let field = TistyField::List;
            set_end(app, EndState::TistyForm { target, field });
            return Effect::None;
        }
        KeyCode::Enter => {
            let message = "preparing the minutes (this can take a while)...";
            set_end(app, EndState::Working(message.to_string()));
            return Effect::TistyPreview(target);
        }
        KeyCode::Backspace => {
            typed(&mut target, field).pop();
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            typed(&mut target, field).push(c);
        }
        _ => {}
    }
    set_end(app, EndState::TistyForm { target, field });
    Effect::None
}

fn typed(target: &mut TistyTarget, field: TistyField) -> &mut String {
    match field {
        TistyField::Folder => &mut target.folder,
        TistyField::List => &mut target.list,
    }
}

/// The loop reports the plan built for a Tisty publication, or why there is none.
pub fn tisty_previewed(app: &mut App, target: TistyTarget, result: Result<String, String>) {
    set_end(
        app,
        match result {
            Ok(preview) => EndState::TistyConfirm {
                target,
                preview,
                scroll: 0,
            },
            Err(message) => EndState::Error(message),
        },
    );
}

/// The loop reports what Tisty did: `Err` carries the report of a publication that had failures.
pub fn tisty_published(app: &mut App, result: Result<String, String>) {
    let (text, ok) = match result {
        Ok(text) => (text, true),
        Err(text) => (text, false),
    };
    set_end(
        app,
        EndState::TistyReport {
            text,
            ok,
            scroll: 0,
        },
    );
}

/// The loop reports the outcome of a save.
pub fn save_finished(app: &mut App, result: Result<PathBuf, String>) {
    set_end(
        app,
        match result {
            Ok(path) => EndState::Done(path),
            Err(message) => EndState::Error(message),
        },
    );
}

/// Display width of a string (wide characters count 2).
pub fn text_width(text: &str) -> usize {
    text.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// Greedy wrap by display width, preferring to break at spaces. A line never
/// exceeds `width` unless a single character is wider than it.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut used = 0usize;
    // Byte index of the last space that is not at the start of the line.
    let mut last_space: Option<usize> = None;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        while used + w > width && !current.is_empty() {
            match last_space.filter(|_| ch != ' ') {
                Some(byte) => {
                    let rest = current[byte..].trim_start().to_string();
                    current.truncate(byte);
                    lines.push(std::mem::take(&mut current));
                    used = text_width(&rest);
                    current = rest;
                    last_space = None;
                }
                None => {
                    lines.push(std::mem::take(&mut current));
                    used = 0;
                    last_space = None;
                }
            }
        }
        if ch == ' ' && current.is_empty() && !lines.is_empty() {
            continue; // a break swallows the space it happened at
        }
        if ch == ' ' && !current.is_empty() {
            last_space = Some(current.len());
        }
        current.push(ch);
        used += w;
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

/// `[mm:ss]` of a segment.
pub fn stamp_label(segment: &Segment) -> String {
    format!("[{}]", mc_engine::runs::fmt_rel(rel_ms(segment)))
}

pub fn folder_of(path: &Path) -> String {
    path.display().to_string()
}
