//! The terminal loop: raw mode, input, drawing. Everything else is pure.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mc_engine::controller::{Capture, FinishError};
use mc_engine::live::LiveSummary;
use ratatui::crossterm::event::{self, Event};

use crate::app::{
    App, Effect, EndState, Msg, Phase, TistyTarget, save_finished, tisty_previewed,
    tisty_published, update,
};
use crate::view::view;

const LIVE_TICK: Duration = Duration::from_millis(50);
const IDLE_TICK: Duration = Duration::from_millis(100);
const REDRAW_EVERY: Duration = Duration::from_secs(1);
/// How long leaving waits for the capture thread before giving up on it.
const FINISH_WAIT: Duration = Duration::from_secs(3);
/// How long leaving waits for the analyst's last window.
const ANALYST_WAIT: Duration = Duration::from_secs(30);

fn is_ctrl_c(key: &ratatui::crossterm::event::KeyEvent) -> bool {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
}

/// What the end menu can do. Each call runs on the UI thread (after drawing "working"),
/// so a slow one blocks the screen; the person asked for it and has nothing else to do.
pub trait Actions {
    /// Saves the transcript as markdown: the session folder and the output folder to the file.
    fn save_md(&mut self, session: &Path, folder: &Path) -> Result<PathBuf, String>;
    /// Builds the plan for Tisty (making the minutes if needed): the text to confirm.
    fn tisty_preview(&mut self, session: &Path, target: &TistyTarget) -> Result<String, String>;
    /// Sends the plan: the report, as `Err` when something failed.
    fn tisty_publish(&mut self, session: &Path, target: &TistyTarget) -> Result<String, String>;
}

/// How the TUI ended.
#[derive(Debug, Default)]
pub struct Outcome {
    pub summary: Option<LiveSummary>,
    /// Set when the capture did not end properly (panic, hung source).
    pub failure: Option<String>,
}

fn collect(
    capture: Capture,
    wait: Duration,
    outcome: &mut Outcome,
) -> Option<mc_engine::live::EndReason> {
    match capture.finish_timeout(wait) {
        Ok(summary) => {
            let reason = summary.reason;
            outcome.summary = Some(summary);
            Some(reason)
        }
        Err(FinishError::Panicked) => {
            outcome.failure = Some(
                "the capture thread panicked; the session folder keeps what was recorded".into(),
            );
            None
        }
        Err(FinishError::Hung) => {
            outcome.failure = Some(
                "the source did not stop; the session was left to be flushed by `mc run`".into(),
            );
            None
        }
    }
}

/// Runs the TUI until the user quits. `ratatui::try_init` puts the terminal in
/// raw mode on the alternate screen and installs a panic hook that restores it;
/// `ratatui::restore` undoes it on every exit path. Mouse capture and bracketed
/// paste stay off so the terminal's own text selection keeps working. The
/// capture is stopped (never abandoned) if the terminal cannot be set up or the
/// loop fails: dropping a [`Capture`] asks it to stop and waits briefly.
pub fn run(mut app: App, capture: Capture, actions: &mut dyn Actions) -> io::Result<Outcome> {
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(error) => {
            capture.request_stop();
            let mut outcome = Outcome::default();
            collect(capture, FINISH_WAIT, &mut outcome);
            return Err(error);
        }
    };
    let result = event_loop(&mut terminal, &mut app, capture, actions);
    ratatui::restore();
    result
}

/// Carries out what the user asked for in the session folder and tells the screen how it went.
fn annotate(app: &mut App, capture: Option<&Capture>, effect: Effect) {
    let Some(active) = capture else { return };
    let done = match effect {
        Effect::Note(text) => active.add_note(&text).map(Msg::Live),
        Effect::Mark => active.mark().map(Msg::Live),
        Effect::FixTerm { heard, correct } => active
            .fix_term(&heard, &correct)
            .map(|()| Msg::Notice(format!("\"{heard}\" will be read as \"{correct}\""))),
        Effect::RenameSpeaker { from, to } => active
            .rename_speaker(&from, &to)
            .map(|_| Msg::Notice(format!("{from} is now {to}"))),
        _ => return,
    };
    update(
        app,
        done.unwrap_or_else(|message| Msg::Notice(format!("not saved: {message}"))),
    );
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    capture: Capture,
    actions: &mut dyn Actions,
) -> io::Result<Outcome> {
    let size = terminal.size()?;
    app.size = (size.width, size.height);
    let mut capture = Some(capture);
    let mut outcome = Outcome::default();
    let mut dirty = true;
    let mut last_draw = Instant::now();
    let mut finishing_since: Option<Instant> = None;
    let mut skip = false;

    loop {
        if dirty || last_draw.elapsed() >= REDRAW_EVERY {
            terminal.draw(|frame| view(app, frame))?;
            dirty = false;
            last_draw = Instant::now();
        }

        // Live events first (this also paces the loop while capturing).
        let wait = if matches!(app.phase, Phase::Live) {
            LIVE_TICK
        } else {
            Duration::ZERO
        };
        if let Some(active) = capture.as_ref() {
            // Read before polling: a thread that ends in between is seen next round.
            let finished = active.is_finished();
            let events = active.poll(wait);
            let silent_end = finished && events.is_empty() && matches!(app.phase, Phase::Live);
            let mono = active.mono_ms();
            if !events.is_empty() {
                dirty = true;
            }
            update(app, Msg::Tick(mono));
            for event in events {
                let effect = update(app, Msg::Live(event));
                annotate(app, capture.as_ref(), effect);
            }
            let ended = !matches!(app.phase, Phase::Live);
            // The analyst's last window gets its time, unless the user skips it.
            let waiting = ended
                && !skip
                && active.analyst_pending()
                && finishing_since.is_none_or(|t| t.elapsed() < ANALYST_WAIT);
            if waiting {
                if finishing_since.is_none() {
                    finishing_since = Some(Instant::now());
                    active.stop_analyst();
                }
                app.finishing = true;
                dirty = true;
            } else if (ended || silent_end)
                && let Some(done) = capture.take()
            {
                app.finishing = false;
                let reason = collect(done, FINISH_WAIT, &mut outcome);
                if silent_end {
                    let _ = app.close_modal();
                    // The thread died without announcing its end.
                    app.ended = Some(reason.map_or("panic", |r| r.as_str()).to_string());
                    app.phase = Phase::End(EndState::Menu { sel: 0 });
                }
                dirty = true;
            }
        }

        // Keys. With nothing live to wait on, block briefly instead of spinning.
        let key_wait = if capture.is_some() && !app.finishing {
            Duration::ZERO
        } else {
            IDLE_TICK
        };
        let mut first = true;
        while event::poll(if first { key_wait } else { Duration::ZERO })? {
            first = false;
            dirty = true;
            let effect = match event::read()? {
                Event::Key(key) if app.finishing && !is_ctrl_c(&key) => {
                    skip = true;
                    Effect::None
                }
                Event::Key(key) => update(app, Msg::Key(key)),
                Event::Resize(w, h) => {
                    terminal.autoresize()?;
                    update(app, Msg::Resize(w, h))
                }
                _ => Effect::None,
            };
            match effect {
                Effect::None => {}
                Effect::Stop => {
                    if let Some(active) = capture.as_ref() {
                        active.request_stop();
                    }
                }
                Effect::ForceQuit | Effect::Quit => {
                    if let Some(active) = capture.take() {
                        active.request_stop();
                        collect(active, FINISH_WAIT, &mut outcome);
                    }
                    return Ok(outcome);
                }
                Effect::SaveMd(dir) => {
                    terminal.draw(|frame| view(app, frame))?;
                    let result = with_session(app, |session| actions.save_md(session, &dir));
                    save_finished(app, result);
                    discard_typed_keys()?;
                }
                Effect::TistyPreview(target) => {
                    terminal.draw(|frame| view(app, frame))?;
                    let result =
                        with_session(app, |session| actions.tisty_preview(session, &target));
                    tisty_previewed(app, target, result);
                    discard_typed_keys()?;
                }
                Effect::TistyPublish(target) => {
                    terminal.draw(|frame| view(app, frame))?;
                    let result =
                        with_session(app, |session| actions.tisty_publish(session, &target));
                    tisty_published(app, result);
                    discard_typed_keys()?;
                }
                asked => annotate(app, capture.as_ref(), asked),
            }
        }
    }
}

fn with_session<T>(
    app: &App,
    action: impl FnOnce(&Path) -> Result<T, String>,
) -> Result<T, String> {
    match app.session_dir.as_deref() {
        Some(session) => action(session),
        None => Err("nothing was recorded".to_string()),
    }
}

/// Keys typed while an action blocked would replay now.
fn discard_typed_keys() -> io::Result<()> {
    while event::poll(Duration::ZERO)? {
        event::read()?;
    }
    Ok(())
}
