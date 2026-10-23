//! Pure transitions of the TUI model: keys, live events, menus, saving and
//! the text helpers (`wrap`, `text_width`).

use std::path::PathBuf;

use mc_core::Segment;
use mc_engine::live::{EndReason, LiveEvent};
use mc_tui::app::{
    PALETTE, TistyField, save_finished, text_width, tisty_previewed, tisty_published, update, wrap,
};
use mc_tui::{App, Effect, EndState, Msg, Phase, TistyTarget};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

fn app() -> App {
    App::new(
        true,
        Some("flag".to_string()),
        PathBuf::from("/home/ana/meetings"),
        (80, 24),
    )
}

fn session_app() -> App {
    let mut app = app();
    feed(
        &mut app,
        LiveEvent::Started {
            session_dir: Some(PathBuf::from("/data/corpus/20261020_teams")),
            session_id: Some("a8bf3c69037539ed23407ca2c775546c".to_string()),
            source: "Teams".to_string(),
        },
    );
    app
}

fn seg(speaker: &str, text: &str, at_ms: f64) -> Segment {
    Segment {
        speaker: speaker.to_string(),
        speaker_raw: None,
        text: text.to_string(),
        first_mono_ms: Some(at_ms),
        last_mono_ms: Some(at_ms + 500.0),
        committed_mono_ms: Some(at_ms + 1000.0),
        committed_wall_ms: None,
    }
}

/// The idx does not matter to the screen.
fn segment_event(segment: Segment) -> LiveEvent {
    LiveEvent::Segment { idx: 0, segment }
}

fn feed(app: &mut App, event: LiveEvent) -> Effect {
    update(app, Msg::Live(event))
}

fn press(app: &mut App, code: KeyCode) -> Effect {
    update(app, Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn press_mod(app: &mut App, code: KeyCode, mods: KeyModifiers) -> Effect {
    update(app, Msg::Key(KeyEvent::new(code, mods)))
}

fn with_kind(app: &mut App, code: KeyCode, kind: KeyEventKind) -> Effect {
    update(
        app,
        Msg::Key(KeyEvent::new_with_kind(code, KeyModifiers::NONE, kind)),
    )
}

fn ended(app: &mut App) {
    feed(
        app,
        LiveEvent::Ended {
            reason: EndReason::SourceEnded,
            frames: 1200,
            records: 340,
        },
    );
}

fn with_segments(app: &mut App, count: usize) {
    for i in 0..count {
        feed(
            app,
            segment_event(seg("Ana", &format!("frase {i}"), i as f64 * 1000.0)),
        );
    }
}

#[test]
fn open_replaces_the_open_lines() {
    let mut app = app();
    feed(&mut app, LiveEvent::Open(vec!["a".into(), "b".into()]));
    feed(&mut app, LiveEvent::Open(vec!["c".into()]));
    assert_eq!(app.open, vec!["c".to_string()]);
}

#[test]
fn segments_append_and_speakers_keep_first_appearance_order() {
    let mut app = app();
    for (speaker, text) in [
        ("Ana", "uno"),
        ("Bruno", "dos"),
        ("Ana", "tres"),
        ("Carla", "cuatro"),
        ("Bruno", "cinco"),
    ] {
        feed(&mut app, segment_event(seg(speaker, text, 0.0)));
    }
    assert_eq!(app.segments.len(), 5);
    assert_eq!(app.segments[4].text, "cinco");
    assert_eq!(app.speakers, ["Ana", "Bruno", "Carla"]);
    assert_eq!(app.speaker_index("Ana"), 0);
    assert_eq!(app.speaker_index("Carla"), 2);
}

#[test]
fn q_stops_once_and_a_second_press_forces_the_exit() {
    let mut app = app();
    assert_eq!(press(&mut app, KeyCode::Char('q')), Effect::Stop);
    assert!(app.stopping);
    assert_eq!(press(&mut app, KeyCode::Char('q')), Effect::ForceQuit);
    assert_eq!(
        press_mod(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL),
        Effect::ForceQuit
    );
}

#[test]
fn ctrl_c_also_stops_then_forces() {
    let mut app = app();
    assert_eq!(
        press_mod(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL),
        Effect::Stop
    );
    assert!(app.stopping);
    assert_eq!(
        press_mod(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL),
        Effect::ForceQuit
    );
}

#[test]
fn ended_moves_to_the_menu_and_clears_open_lines() {
    let mut app = session_app();
    feed(&mut app, LiveEvent::Open(vec!["Ana: falando".into()]));
    ended(&mut app);
    assert_eq!(app.phase, Phase::End(EndState::Menu { sel: 0 }));
    assert!(app.open.is_empty());
    assert_eq!(app.ended.as_deref(), Some("source_ended"));
    assert_eq!(app.frozen, None);
}

#[test]
fn only_key_presses_are_acted_upon() {
    let mut app = app();
    assert_eq!(
        with_kind(&mut app, KeyCode::Char('q'), KeyEventKind::Release),
        Effect::None
    );
    assert_eq!(
        with_kind(&mut app, KeyCode::Char('q'), KeyEventKind::Repeat),
        Effect::None
    );
    assert!(!app.stopping);
    assert_eq!(
        with_kind(&mut app, KeyCode::Char('q'), KeyEventKind::Press),
        Effect::Stop
    );
}

#[test]
fn release_of_enter_does_not_submit_the_folder() {
    let mut app = session_app();
    ended(&mut app);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert!(matches!(app.phase, Phase::End(EndState::AskFolder { .. })));
    assert_eq!(
        with_kind(&mut app, KeyCode::Enter, KeyEventKind::Release),
        Effect::None
    );
    assert!(matches!(app.phase, Phase::End(EndState::AskFolder { .. })));
}

#[test]
fn p_toggles_pause_and_freezes_the_shown_segments() {
    let mut app = app();
    with_segments(&mut app, 3);
    press(&mut app, KeyCode::Char('p'));
    assert_eq!(app.frozen, Some(3));
    with_segments(&mut app, 2);
    assert_eq!(app.visible_segments().len(), 3);
    assert_eq!(app.segments.len(), 5);
    press(&mut app, KeyCode::Char('p'));
    assert_eq!(app.frozen, None);
    assert_eq!(app.visible_segments().len(), 5);
}

#[test]
fn up_scrolls_and_freezes_and_end_resumes() {
    let mut app = app();
    // A pane two lines high holding four one-line segments scrolls back by two.
    app.size = (80, 6);
    with_segments(&mut app, 4);
    press(&mut app, KeyCode::Up);
    assert_eq!(app.frozen, Some(4));
    assert_eq!(app.scroll_back, 1);
    press(&mut app, KeyCode::Up);
    assert_eq!(app.scroll_back, 2);
    press(&mut app, KeyCode::Up);
    assert_eq!(app.scroll_back, 2, "never past the top");
    with_segments(&mut app, 1);
    assert_eq!(
        app.visible_segments().len(),
        4,
        "new text does not drag the view"
    );
    press(&mut app, KeyCode::End);
    assert_eq!(app.frozen, None);
    assert_eq!(app.scroll_back, 0);
    assert_eq!(app.visible_segments().len(), 5);
}

#[test]
fn help_opens_with_question_mark_and_any_key_closes_it() {
    let mut app = app();
    press(&mut app, KeyCode::Char('?'));
    assert!(app.help);
    // The key that closes help is swallowed: `q` must not stop the capture.
    assert_eq!(press(&mut app, KeyCode::Char('q')), Effect::None);
    assert!(!app.help);
    assert!(!app.stopping);
    press(&mut app, KeyCode::Char('?'));
    assert_eq!(press(&mut app, KeyCode::Char('x')), Effect::None);
    assert!(!app.help);
}

#[test]
fn menu_navigation_wraps_with_a_session() {
    let mut app = session_app();
    ended(&mut app);
    assert_eq!(app.menu_items().len(), 4);
    press(&mut app, KeyCode::Up);
    assert_eq!(app.phase, Phase::End(EndState::Menu { sel: 3 }));
    press(&mut app, KeyCode::Down);
    assert_eq!(app.phase, Phase::End(EndState::Menu { sel: 0 }));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    assert_eq!(app.phase, Phase::End(EndState::Menu { sel: 0 }));
}

#[test]
fn menu_navigation_wraps_without_a_session() {
    let mut app = app();
    ended(&mut app);
    assert_eq!(app.menu_items().len(), 2);
    press(&mut app, KeyCode::Up);
    assert_eq!(app.phase, Phase::End(EndState::Menu { sel: 1 }));
    press(&mut app, KeyCode::Down);
    assert_eq!(app.phase, Phase::End(EndState::Menu { sel: 0 }));
}

#[test]
fn no_session_menu_has_show_and_exit_only() {
    let mut app = App::new(false, None, PathBuf::from("/x"), (80, 24));
    feed(
        &mut app,
        LiveEvent::Ended {
            reason: EndReason::Stop,
            frames: 0,
            records: 0,
        },
    );
    let items = app.menu_items();
    assert_eq!(items.len(), 2);
    // Down from Show goes to Exit, and Enter on it quits.
    press(&mut app, KeyCode::Down);
    assert_eq!(press(&mut app, KeyCode::Enter), Effect::Quit);
}

fn tisty_form_app() -> App {
    let mut app = session_app().with_tisty_default("Reuniones", "Trabajo");
    ended(&mut app);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    assert_eq!(press(&mut app, KeyCode::Enter), Effect::None);
    app
}

fn target(folder: &str, list: &str) -> TistyTarget {
    TistyTarget {
        folder: folder.to_string(),
        list: list.to_string(),
    }
}

#[test]
fn the_tisty_form_starts_with_the_configured_defaults_and_escape_goes_back() {
    let mut app = tisty_form_app();
    assert_eq!(
        app.phase,
        Phase::End(EndState::TistyForm {
            target: target("Reuniones", "Trabajo"),
            field: TistyField::Folder
        })
    );
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.phase, Phase::End(EndState::Menu { sel: 2 }));
}

#[test]
fn the_form_edits_each_field_and_previews_on_the_second_enter() {
    let mut app = tisty_form_app();
    for _ in 0.."Reuniones".len() {
        press(&mut app, KeyCode::Backspace);
    }
    for c in "Notas".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    assert_eq!(press(&mut app, KeyCode::Enter), Effect::None);
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Tab);
    for _ in 0.."Trabajo".len() {
        press(&mut app, KeyCode::Backspace);
    }
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        Effect::TistyPreview(target("Notas", ""))
    );
    assert!(matches!(app.phase, Phase::End(EndState::Working(_))));
}

#[test]
fn an_empty_folder_is_not_accepted() {
    let mut app = tisty_form_app();
    for _ in 0.."Reuniones".len() {
        press(&mut app, KeyCode::Backspace);
    }
    assert_eq!(press(&mut app, KeyCode::Enter), Effect::None);
    assert!(matches!(
        app.phase,
        Phase::End(EndState::TistyForm {
            field: TistyField::Folder,
            ..
        })
    ));
}

#[test]
fn the_plan_waits_for_a_yes_before_publishing() {
    let mut app = tisty_form_app();
    let plan = target("Reuniones", "Trabajo");
    tisty_previewed(
        &mut app,
        plan.clone(),
        Ok("minutes: x
"
        .to_string()),
    );
    assert!(matches!(
        app.phase,
        Phase::End(EndState::TistyConfirm { .. })
    ));
    // Anything but a yes leaves nothing sent.
    assert_eq!(press(&mut app, KeyCode::Char('x')), Effect::None);
    assert_eq!(press(&mut app, KeyCode::Esc), Effect::None);
    assert_eq!(app.phase, Phase::End(EndState::Menu { sel: 2 }));

    tisty_previewed(
        &mut app,
        plan.clone(),
        Ok("minutes: x
"
        .to_string()),
    );
    assert_eq!(press(&mut app, KeyCode::Enter), Effect::TistyPublish(plan));
    assert!(matches!(app.phase, Phase::End(EndState::Working(_))));
}

#[test]
fn a_preview_that_fails_shows_the_error_and_a_report_returns_to_the_menu() {
    let mut app = tisty_form_app();
    tisty_previewed(&mut app, target("a", ""), Err("no minutes".to_string()));
    assert_eq!(
        app.phase,
        Phase::End(EndState::Error("no minutes".to_string()))
    );

    tisty_published(&mut app, Err("FAILED".to_string()));
    assert_eq!(
        app.phase,
        Phase::End(EndState::TistyReport {
            text: "FAILED".to_string(),
            ok: false,
            scroll: 0
        })
    );
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.phase, Phase::End(EndState::Menu { sel: 0 }));
}

#[test]
fn exit_on_the_session_menu_quits() {
    let mut app = session_app();
    ended(&mut app);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    assert_eq!(press(&mut app, KeyCode::Enter), Effect::Quit);
}

#[test]
fn show_opens_the_transcript_and_escape_goes_back() {
    let mut app = session_app();
    with_segments(&mut app, 2);
    ended(&mut app);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.phase, Phase::End(EndState::Showing { scroll: 0 }));
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.phase, Phase::End(EndState::Menu { sel: 0 }));
}

#[test]
fn save_md_asks_for_a_folder_prefilled_with_the_default() {
    let mut app = session_app();
    ended(&mut app);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.phase,
        Phase::End(EndState::AskFolder {
            input: "/home/ana/meetings".to_string()
        })
    );
}

#[test]
fn folder_input_edits_with_unicode_and_backspace() {
    let mut app = session_app();
    ended(&mut app);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    let start = "/home/ana/meetings".to_string();

    press(&mut app, KeyCode::Char('/'));
    press(&mut app, KeyCode::Char('ñ'));
    press(&mut app, KeyCode::Char('é'));
    assert_eq!(
        app.phase,
        Phase::End(EndState::AskFolder {
            input: format!("{start}/ñé")
        })
    );
    // Backspace removes one whole character, not one byte.
    press(&mut app, KeyCode::Backspace);
    assert_eq!(
        app.phase,
        Phase::End(EndState::AskFolder {
            input: format!("{start}/ñ")
        })
    );
    press(&mut app, KeyCode::Backspace);
    press(&mut app, KeyCode::Backspace);
    assert_eq!(
        app.phase,
        Phase::End(EndState::AskFolder {
            input: start.clone()
        })
    );
    // Ctrl-chars are not typed into the folder.
    press_mod(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
    assert_eq!(
        app.phase,
        Phase::End(EndState::AskFolder {
            input: start.clone()
        })
    );
}

#[test]
fn escape_in_the_folder_prompt_goes_back_to_the_menu() {
    let mut app = session_app();
    ended(&mut app);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.phase, Phase::End(EndState::Menu { sel: 1 }));
}

#[test]
fn enter_in_the_folder_prompt_returns_the_save_effect() {
    let mut app = session_app();
    ended(&mut app);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    // Replace the default with a relative folder, padded with spaces.
    for _ in 0.."/home/ana/meetings".chars().count() {
        press(&mut app, KeyCode::Backspace);
    }
    for c in "  notas/2026  ".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        Effect::SaveMd(PathBuf::from("notas/2026"))
    );
    assert_eq!(
        app.phase,
        Phase::End(EndState::Working(
            "running the analysis and saving...".to_string()
        ))
    );
}

#[test]
fn empty_folder_does_not_save() {
    let mut app = session_app();
    ended(&mut app);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    for _ in 0.."/home/ana/meetings".chars().count() {
        press(&mut app, KeyCode::Backspace);
    }
    assert_eq!(press(&mut app, KeyCode::Enter), Effect::None);
    assert!(matches!(app.phase, Phase::End(EndState::AskFolder { .. })));
}

#[test]
fn save_finished_sets_done_or_error_and_enter_returns_to_the_menu() {
    let mut app = session_app();
    ended(&mut app);
    save_finished(&mut app, Ok(PathBuf::from("/out/a8bf3c69-run_01234.md")));
    assert_eq!(
        app.phase,
        Phase::End(EndState::Done(PathBuf::from("/out/a8bf3c69-run_01234.md")))
    );
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.phase, Phase::End(EndState::Menu { sel: 0 }));

    save_finished(&mut app, Err("permission denied".to_string()));
    assert_eq!(
        app.phase,
        Phase::End(EndState::Error("permission denied".to_string()))
    );
    press(&mut app, KeyCode::Char('q'));
    assert_eq!(app.phase, Phase::End(EndState::Menu { sel: 0 }));
}

#[test]
fn segments_are_capped_and_a_frozen_index_follows_the_drop() {
    let mut app = app();
    with_segments(&mut app, 5100);
    assert_eq!(app.segments.len(), 5000);
    assert_eq!(app.segments[0].text, "frase 100");

    let mut app = app_with_cap_and_pause();
    feed(&mut app, segment_event(seg("Ana", "nueva", 0.0)));
    assert_eq!(app.segments.len(), 5000);
    assert_eq!(app.frozen, Some(4999));
    assert_eq!(app.visible_segments().len(), 4999);
}

fn app_with_cap_and_pause() -> App {
    let mut app = app();
    with_segments(&mut app, 5000);
    press(&mut app, KeyCode::Char('p'));
    assert_eq!(app.frozen, Some(5000));
    app
}

#[test]
fn tick_and_resize_update_the_model() {
    let mut app = app();
    update(&mut app, Msg::Tick(61_500.0));
    assert_eq!(app.elapsed_ms(), 61_500.0);
    update(&mut app, Msg::Resize(100, 30));
    assert_eq!(app.size, (100, 30));
}

#[test]
fn wrap_fits_exact_width_in_one_line() {
    assert_eq!(wrap("abcd", 4), ["abcd"]);
    assert_eq!(wrap("ab cd", 5), ["ab cd"]);
}

#[test]
fn wrap_hard_breaks_a_word_longer_than_the_width() {
    assert_eq!(wrap("abcdefgh", 3), ["abc", "def", "gh"]);
}

#[test]
fn wrap_prefers_breaking_at_spaces() {
    assert_eq!(wrap("hello world", 8), ["hello", "world"]);
    assert_eq!(wrap("hello world foo", 11), ["hello world", "foo"]);
    assert_eq!(wrap("abcd efghij", 5), ["abcd", "efghi", "j"]);
}

#[test]
fn wrap_counts_wide_characters_as_two_columns() {
    assert_eq!(wrap("你好世界", 4), ["你好", "世界"]);
    assert_eq!(wrap("你好世界", 3), ["你", "好", "世", "界"]);
    assert_eq!(text_width("你a"), 3);
}

#[test]
fn wrap_of_the_empty_string_is_one_empty_line() {
    assert_eq!(wrap("", 5), [""]);
    assert_eq!(wrap("", 0), [""]);
}

#[test]
fn wrap_with_width_zero_or_one_breaks_every_character() {
    assert_eq!(wrap("abc", 0), ["a", "b", "c"]);
    assert_eq!(wrap("abc", 1), ["a", "b", "c"]);
}

#[test]
fn palette_is_the_number_of_speaker_colours() {
    assert_eq!(PALETTE, 6);
}

// ------------------------------------------------- audit regressions

#[test]
fn ctrl_c_quits_even_with_the_help_open() {
    let mut app = session_app();
    press(&mut app, KeyCode::Char('?'));
    assert!(app.help);
    let effect = press_mod(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(effect, Effect::Stop);
    assert!(!app.help);
}

#[test]
fn the_help_does_not_survive_the_end_of_the_capture() {
    let mut app = session_app();
    press(&mut app, KeyCode::Char('?'));
    ended(&mut app);
    assert!(!app.help);
    // The first key after the end reaches the menu instead of closing a help.
    press(&mut app, KeyCode::Down);
    assert!(matches!(app.phase, Phase::End(EndState::Menu { sel: 1 })));
}

#[test]
fn a_write_error_stays_until_the_end_while_other_warnings_clear() {
    let mut app = session_app();
    feed(&mut app, LiveEvent::WriteError("disk full".into()));
    feed(&mut app, segment_event(seg("Ana", "hola", 1000.0)));
    assert!(app.warning.is_some(), "write errors are sticky");

    let mut other = session_app();
    feed(&mut other, LiveEvent::SourceError("blip".into()));
    feed(&mut other, segment_event(seg("Ana", "hola", 1000.0)));
    assert!(other.warning.is_none());
}

#[test]
fn ctrl_c_quits_from_every_end_screen_except_a_running_save() {
    for state in [
        EndState::Showing { scroll: 0 },
        EndState::AskFolder { input: "x".into() },
        EndState::Done(PathBuf::from("/tmp/x.md")),
        EndState::Error("boom".into()),
        EndState::TistyForm {
            target: target("a", "b"),
            field: TistyField::List,
        },
        EndState::TistyConfirm {
            target: target("a", "b"),
            preview: "x".into(),
            scroll: 0,
        },
        EndState::TistyReport {
            text: "x".into(),
            ok: true,
            scroll: 0,
        },
    ] {
        let mut app = session_app();
        ended(&mut app);
        app.phase = Phase::End(state);
        assert_eq!(
            press_mod(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL),
            Effect::Quit
        );
    }
    let mut app = session_app();
    ended(&mut app);
    app.phase = Phase::End(EndState::Working("saving".into()));
    assert_eq!(
        press_mod(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL),
        Effect::None
    );
}

#[test]
fn the_pager_scroll_stops_at_the_top_and_home_goes_there() {
    let mut app = session_app();
    app.size = (80, 6);
    with_segments(&mut app, 10);
    ended(&mut app);
    app.phase = Phase::End(EndState::Showing { scroll: 0 });
    press(&mut app, KeyCode::Home);
    let Phase::End(EndState::Showing { scroll: top }) = app.phase.clone() else {
        panic!("not showing");
    };
    assert!(top < 1000, "clamped to the real top, got {top}");
    press(&mut app, KeyCode::Up);
    assert_eq!(app.phase, Phase::End(EndState::Showing { scroll: top }));
    press(&mut app, KeyCode::Down);
    assert_eq!(app.phase, Phase::End(EndState::Showing { scroll: top - 1 }));
}

#[test]
fn wrap_never_exceeds_the_width_with_wide_characters_or_leading_spaces() {
    for (text, width) in [(" 中中", 2), ("ab 中中中", 3), ("  x", 1), ("中", 4)] {
        for line in wrap(text, width) {
            assert!(
                text_width(&line) <= width.max(2),
                "{text:?} at {width}: {line:?}"
            );
        }
    }
    assert!(wrap(" 中中", 2).iter().all(|l| !l.is_empty()));
}

#[test]
fn audio_events_drive_the_rec_mark_and_a_warning_that_never_hides_a_worse_one() {
    use mc_tui::app::Warning;
    let mut app = session_app();
    assert!(!app.audio_rec);
    feed(&mut app, LiveEvent::Rec(true));
    assert!(app.audio_rec);
    feed(&mut app, LiveEvent::Audio("Audio mic: lost".to_string()));
    assert_eq!(
        app.warning,
        Some(Warning::Audio("Audio mic: lost".to_string()))
    );
    feed(&mut app, LiveEvent::WriteError("disk full".to_string()));
    feed(&mut app, LiveEvent::Audio("Audio mic: again".to_string()));
    assert_eq!(app.warning, Some(Warning::Write("disk full".to_string())));
    feed(&mut app, LiveEvent::Rec(false));
    assert!(!app.audio_rec);
}

fn long_text(lines: usize) -> String {
    (1..=lines)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join(
            "
",
        )
}

#[test]
fn a_long_plan_scrolls_and_never_past_its_last_page() {
    let mut app = tisty_form_app();
    tisty_previewed(&mut app, target("a", ""), Ok(long_text(40)));
    let scroll = |app: &App| match &app.phase {
        Phase::End(EndState::TistyConfirm { scroll, .. }) => *scroll,
        other => panic!("{other:?}"),
    };
    // 24 rows: header and footer, borders and the question leave 18 for the text.
    assert_eq!(scroll(&app), 0);
    press(&mut app, KeyCode::Down);
    assert_eq!(scroll(&app), 1);
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Up);
    assert_eq!(scroll(&app), 0);
    press(&mut app, KeyCode::PageDown);
    assert_eq!(scroll(&app), 17);
    press(&mut app, KeyCode::End);
    assert_eq!(scroll(&app), 22);
    press(&mut app, KeyCode::Down);
    assert_eq!(scroll(&app), 22);
    press(&mut app, KeyCode::Home);
    assert_eq!(scroll(&app), 0);
    // Scrolling is not a yes.
    assert!(matches!(
        app.phase,
        Phase::End(EndState::TistyConfirm { .. })
    ));
}

#[test]
fn a_long_report_scrolls_too() {
    let mut app = tisty_form_app();
    tisty_published(&mut app, Ok(long_text(30)));
    press(&mut app, KeyCode::End);
    match &app.phase {
        Phase::End(EndState::TistyReport { scroll, .. }) => assert_eq!(*scroll, 30 - 20),
        other => panic!("{other:?}"),
    }
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.phase, Phase::End(EndState::Menu { sel: 0 }));
}

#[test]
fn a_configured_folder_goes_straight_to_the_preview() {
    let mut app = session_app()
        .with_tisty_default("Trabajo / Meets", "Trabajo")
        .with_tisty_configured(true);
    ended(&mut app);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        Effect::TistyPreview(target("Trabajo / Meets", "Trabajo"))
    );
}
