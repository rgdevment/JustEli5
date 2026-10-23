//! Notes, marks, term fixes, speaker names and the ledger panel keys, as pure transitions.

use std::path::PathBuf;

use mc_core::Segment;
use mc_engine::live::LiveEvent;
use mc_engine::sidecar::Note;
use mc_tui::app::{Warning, update};
use mc_tui::{App, Effect, Modal, Msg};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn recording() -> App {
    let mut app = App::new(
        true,
        Some("flag".into()),
        PathBuf::from("/home/ana/meetings"),
        (120, 30),
    );
    feed(
        &mut app,
        LiveEvent::Session {
            dir: PathBuf::from("/data/corpus/20261020_teams"),
            id: "a8bf".into(),
        },
    );
    app
}

fn feed(app: &mut App, event: LiveEvent) -> Effect {
    update(app, Msg::Live(event))
}

fn press(app: &mut App, code: KeyCode) -> Effect {
    update(app, Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

fn say(app: &mut App, speaker: &str) {
    let segment = Segment {
        speaker: speaker.into(),
        speaker_raw: None,
        text: "hola".into(),
        first_mono_ms: Some(0.0),
        last_mono_ms: Some(1.0),
        committed_mono_ms: Some(2.0),
        committed_wall_ms: None,
    };
    feed(app, LiveEvent::Segment { idx: 0, segment });
}

fn notice(app: &App) -> Option<&str> {
    app.notice.as_deref()
}

fn ledger_event(ledger: mc_analyst::Ledger) -> LiveEvent {
    LiveEvent::Ledger(Box::new(mc_engine::live_analyst::LedgerUpdate {
        ledger,
        changed: Vec::new(),
        event: mc_analyst::LedgerEvent {
            first_id: 0,
            last_id: 0,
            segments: 1,
            model: "fake".into(),
            attempts: 1,
            accepted: 1,
            rejected: 0,
            rejections: Vec::new(),
            error: None,
        },
    }))
}

#[test]
fn n_asks_for_a_note_and_enter_saves_it() {
    let mut app = recording();
    assert_eq!(press(&mut app, KeyCode::Char('n')), Effect::None);
    assert_eq!(app.modal, Some(Modal::Note(String::new())));
    // Keys that mean something outside the question are text inside it.
    type_text(&mut app, "q pedir el PR");
    press(&mut app, KeyCode::Backspace);
    press(&mut app, KeyCode::Backspace);
    type_text(&mut app, "PR!");
    assert!(!app.stopping);
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        Effect::Note("q pedir el PR!".into())
    );
    assert!(app.modal.is_none());
}

#[test]
fn escape_cancels_and_an_empty_answer_is_not_sent() {
    let mut app = recording();
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(press(&mut app, KeyCode::Enter), Effect::None);
    assert!(app.modal.is_some(), "an empty note keeps asking");
    type_text(&mut app, "algo");
    assert_eq!(press(&mut app, KeyCode::Esc), Effect::None);
    assert!(app.modal.is_none());
}

#[test]
fn ctrl_c_still_stops_while_a_question_is_open() {
    let mut app = recording();
    press(&mut app, KeyCode::Char('n'));
    let effect = update(
        &mut app,
        Msg::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
    );
    assert_eq!(effect, Effect::Stop);
    assert!(app.modal.is_none());
}

#[test]
fn m_marks_the_moment_at_once() {
    let mut app = recording();
    assert_eq!(press(&mut app, KeyCode::Char('m')), Effect::Mark);
    assert!(app.modal.is_none());
}

#[test]
fn c_asks_what_was_heard_and_then_what_it_should_be() {
    let mut app = recording();
    press(&mut app, KeyCode::Char('c'));
    assert_eq!(app.modal, Some(Modal::TermHeard(String::new())));
    type_text(&mut app, "meets");
    assert_eq!(press(&mut app, KeyCode::Enter), Effect::None);
    assert_eq!(
        app.modal,
        Some(Modal::TermCorrect {
            heard: "meets".into(),
            input: String::new()
        })
    );
    type_text(&mut app, "Meet");
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        Effect::FixTerm {
            heard: "meets".into(),
            correct: "Meet".into()
        }
    );
}

#[test]
fn s_picks_a_current_speaker_and_asks_for_the_new_name() {
    let mut app = recording();
    say(&mut app, "Ana");
    say(&mut app, "Unknown");
    press(&mut app, KeyCode::Char('s'));
    assert_eq!(app.modal, Some(Modal::PickSpeaker { sel: 0 }));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    assert_eq!(
        app.modal,
        Some(Modal::PickSpeaker { sel: 0 }),
        "wraps around"
    );
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.modal,
        Some(Modal::RenameSpeaker {
            from: "Unknown".into(),
            input: String::new()
        })
    );
    type_text(&mut app, "Mario");
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        Effect::RenameSpeaker {
            from: "Unknown".into(),
            to: "Mario".into()
        }
    );
    assert_eq!(app.name_of("Unknown"), "Mario");
    assert_eq!(app.name_of("Ana"), "Ana");
    // The colour belongs to the speaker, not to the name.
    assert_eq!(app.speaker_index("Unknown"), 1);
}

#[test]
fn s_needs_someone_to_have_spoken() {
    let mut app = recording();
    press(&mut app, KeyCode::Char('s'));
    assert!(app.modal.is_none());
    assert_eq!(notice(&app), Some("no speaker has talked yet"));
}

#[test]
fn annotating_needs_a_recording_with_a_session_folder() {
    let mut live_only = App::new(false, None, PathBuf::from("/x"), (120, 30));
    let mut waiting = App::new(true, Some("flag".into()), PathBuf::from("/x"), (120, 30));
    for app in [&mut live_only, &mut waiting] {
        for key in ['n', 'm', 'c', 's'] {
            assert_eq!(press(app, KeyCode::Char(key)), Effect::None);
            assert!(app.modal.is_none());
            assert!(notice(app).unwrap().contains("nothing is being recorded"));
        }
    }
}

#[test]
fn a_notice_stays_until_the_next_key() {
    let mut app = recording();
    feed(
        &mut app,
        LiveEvent::Note(Note {
            mono_ms: 1.0,
            wall_ms: 2,
            text: "x".into(),
        }),
    );
    assert_eq!(notice(&app), Some("note saved"));
    say(&mut app, "Ana");
    assert_eq!(notice(&app), Some("note saved"), "captions do not wipe it");
    feed(&mut app, LiveEvent::WriteError("disk full".into()));
    assert_eq!(notice(&app), Some("note saved"));
    assert_eq!(app.warning, Some(Warning::Write("disk full".into())));
    press(&mut app, KeyCode::Char('p'));
    assert!(notice(&app).is_none());
    feed(
        &mut app,
        LiveEvent::Mark {
            mono_ms: 65_000.0,
            wall_ms: 3,
        },
    );
    assert_eq!(notice(&app), Some("moment marked at 01:05"));
}

#[test]
fn l_toggles_the_panel_only_when_the_analyst_runs() {
    let mut app = recording();
    press(&mut app, KeyCode::Char('l'));
    assert!(notice(&app).unwrap().contains("--analyst"));
    assert!(app.panel_width().is_none());

    let mut app = recording().with_analyst(true);
    assert!(app.panel_width().is_some());
    press(&mut app, KeyCode::Char('l'));
    assert!(app.panel_width().is_none());
    press(&mut app, KeyCode::Char('l'));
    assert!(app.panel_width().is_some());
}

#[test]
fn the_panel_needs_about_a_hundred_columns() {
    let mut app = recording().with_analyst(true);
    update(&mut app, Msg::Resize(99, 30));
    assert!(app.panel_width().is_none());
    update(&mut app, Msg::Resize(100, 30));
    assert!(app.panel_width().is_some());
    assert!(app.main_width() < 100);
}

#[test]
fn ledger_updates_replace_the_ledger_and_clear_the_last_error() {
    let mut app = recording().with_analyst(true);
    feed(&mut app, LiveEvent::AnalystError("model down".into()));
    assert_eq!(app.analyst_error.as_deref(), Some("model down"));
    assert!(app.ledger.is_none());
    let ledger: mc_analyst::Ledger =
        serde_json::from_str(r#"{"decisions":[{"id":"d1","text":"Seguir","evidence":[1]}]}"#)
            .unwrap();
    feed(&mut app, ledger_event(ledger));
    assert!(app.analyst_error.is_none());
    assert_eq!(app.ledger.as_ref().unwrap().decisions.len(), 1);
}

#[test]
fn the_panel_scrolls_within_its_content() {
    let mut app = recording().with_analyst(true);
    update(&mut app, Msg::Resize(100, 10));
    let decisions: Vec<String> = (0..20)
        .map(|i| format!(r#"{{"id":"d{i}","text":"decision numero {i}","evidence":[{i}]}}"#))
        .collect();
    let ledger: mc_analyst::Ledger =
        serde_json::from_str(&format!(r#"{{"decisions":[{}]}}"#, decisions.join(","))).unwrap();
    feed(&mut app, ledger_event(ledger));
    press(&mut app, KeyCode::Char('['));
    assert_eq!(app.panel_scroll, 0);
    for _ in 0..200 {
        press(&mut app, KeyCode::Char(']'));
    }
    let bottom = app.panel_scroll;
    assert!(bottom > 10 && bottom < 200, "stops at the end: {bottom}");
    press(&mut app, KeyCode::Char('['));
    assert_eq!(app.panel_scroll, bottom - 1);
}

#[test]
fn altgr_characters_are_text_not_shortcuts() {
    let mut app = recording();
    press(&mut app, KeyCode::Char('n'));
    // Windows reports AltGr as Ctrl+Alt.
    for c in ['@', '#', '€'] {
        let altgr = KeyModifiers::CONTROL | KeyModifiers::ALT;
        update(&mut app, Msg::Key(KeyEvent::new(KeyCode::Char(c), altgr)));
    }
    // A real Ctrl+letter still is not text.
    update(
        &mut app,
        Msg::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)),
    );
    assert_eq!(press(&mut app, KeyCode::Enter), Effect::Note("@#€".into()));
}

#[test]
fn a_question_open_when_the_meeting_ends_is_saved_if_complete() {
    let ended = || LiveEvent::Ended {
        reason: mc_engine::live::EndReason::Stop,
        frames: 1,
        records: 1,
    };

    let mut app = recording();
    press(&mut app, KeyCode::Char('n'));
    type_text(&mut app, "pedir el informe");
    assert_eq!(
        feed(&mut app, ended()),
        Effect::Note("pedir el informe".into())
    );
    assert!(app.modal.is_none());
    // Ctrl-C leaves the end screen.
    let quit = update(
        &mut app,
        Msg::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
    );
    assert_eq!(quit, Effect::Quit);

    let mut app = recording();
    say(&mut app, "Unknown");
    press(&mut app, KeyCode::Char('s'));
    press(&mut app, KeyCode::Enter);
    type_text(&mut app, "Mario");
    assert_eq!(
        feed(&mut app, ended()),
        Effect::RenameSpeaker {
            from: "Unknown".into(),
            to: "Mario".into()
        }
    );
    assert_eq!(app.name_of("Unknown"), "Mario");

    // An unfinished correction cannot be saved: the user is told.
    let mut app = recording();
    press(&mut app, KeyCode::Char('c'));
    type_text(&mut app, "meets");
    assert_eq!(feed(&mut app, ended()), Effect::None);
    assert!(notice(&app).unwrap().contains("discarded"));
}

#[test]
fn swapped_names_show_each_once() {
    let mut app = recording();
    say(&mut app, "Ana");
    say(&mut app, "Luis");
    for (from_index, to) in [(0, "Luis"), (1, "Ana")] {
        press(&mut app, KeyCode::Char('s'));
        for _ in 0..from_index {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Enter);
        type_text(&mut app, to);
        press(&mut app, KeyCode::Enter);
    }
    assert_eq!(app.name_of("Ana"), "Luis");
    assert_eq!(app.name_of("Luis"), "Ana");
}
