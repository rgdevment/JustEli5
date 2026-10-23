//! Text snapshots of the rendered screens, plus a few style checks made on the
//! raw buffer. Regenerate with `UPDATE_SNAPSHOTS=1 cargo test -p mc-tui --test
//! snapshots`, then read every changed file before committing it.

use std::fs;
use std::path::{Path, PathBuf};

use mc_captions::Discovery;
use mc_core::Segment;
use mc_engine::live::{EndReason, LiveEvent};
use mc_tui::App;
use mc_tui::app::{Msg, update};
use mc_tui::view::{render_text, view};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;

const DEFAULT_DIR: &str = "/home/ana/meetings";
const SESSION_DIR: &str = "/data/corpus/20261020T103000_teams";
const SESSION_ID: &str = "a8bf3c69037539ed23407ca2c775546c";

// --- helpers --------------------------------------------------------------

fn snapshot_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("{name}.txt"))
}

/// Compares `text` with the stored snapshot, or writes it when
/// `UPDATE_SNAPSHOTS=1`.
fn snap(name: &str, text: &str) {
    let path = snapshot_path(name);
    if std::env::var("UPDATE_SNAPSHOTS").as_deref() == Ok("1") {
        fs::create_dir_all(path.parent().expect("snapshot dir")).expect("create snapshot dir");
        fs::write(&path, text).expect("write snapshot");
        return;
    }
    let expected = fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing snapshot {}; generate it with UPDATE_SNAPSHOTS=1",
            path.display()
        )
    });
    assert_eq!(expected, text, "snapshot {name} changed");
}

fn live(recording: bool, consent: Option<&str>) -> App {
    App::new(
        recording,
        consent.map(str::to_string),
        PathBuf::from(DEFAULT_DIR),
        (80, 24),
    )
}

/// The idx does not matter to the screen.
fn segment_event(segment: Segment) -> LiveEvent {
    LiveEvent::Segment { idx: 0, segment }
}

fn feed(app: &mut App, event: LiveEvent) {
    update(app, Msg::Live(event));
}

fn tick(app: &mut App, ms: f64) {
    update(app, Msg::Tick(ms));
}

fn key(app: &mut App, code: KeyCode) {
    update(app, Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

fn seg(speaker: &str, text: &str, at_ms: f64) -> Segment {
    Segment {
        speaker: speaker.to_string(),
        speaker_raw: None,
        text: text.to_string(),
        first_mono_ms: Some(at_ms),
        last_mono_ms: Some(at_ms + 800.0),
        committed_mono_ms: Some(at_ms + 1200.0),
        committed_wall_ms: None,
    }
}

fn say(app: &mut App, speaker: &str, text: &str, at_ms: f64) {
    feed(app, segment_event(seg(speaker, text, at_ms)));
}

fn started(app: &mut App, source: &str) {
    feed(
        app,
        LiveEvent::Started {
            session_dir: Some(PathBuf::from(SESSION_DIR)),
            session_id: Some(SESSION_ID.to_string()),
            source: source.to_string(),
        },
    );
}

fn attached(app: &mut App, window: &str) {
    feed(
        app,
        LiveEvent::Source(Discovery::Attached {
            window: window.to_string(),
        }),
    );
}

fn ended(app: &mut App, reason: EndReason) {
    feed(
        app,
        LiveEvent::Ended {
            reason,
            frames: 1200,
            records: 340,
        },
    );
}

/// A live meeting with three speakers, saved to a session folder.
fn meeting() -> App {
    let mut app = live(true, Some("flag"));
    started(&mut app, "Teams");
    attached(&mut app, "Weekly sync | Microsoft Teams");
    say(&mut app, "Ana", "Bom dia a todos, vamos começar.", 3_000.0);
    say(
        &mut app,
        "Bruno",
        "Eu já mandei o relatório ontem.",
        12_000.0,
    );
    say(&mut app, "Carla", "Perfeito, obrigada.", 21_000.0);
    say(
        &mut app,
        "Ana",
        "Então o primeiro ponto é o orçamento.",
        30_000.0,
    );
    tick(&mut app, 95_000.0);
    app
}

/// Shows the menu of an ended meeting that was recorded.
fn ended_meeting() -> App {
    let mut app = meeting();
    ended(&mut app, EndReason::SourceEnded);
    app
}

fn draw(app: &App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
    terminal.draw(|frame| view(app, frame)).expect("draw");
    terminal.backend().buffer().clone()
}

/// Column and row of the first occurrence of `needle` (ASCII needles only).
fn find(buffer: &Buffer, needle: &str) -> Option<(u16, u16)> {
    for y in 0..buffer.area.height {
        let row: String = (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect();
        if let Some(byte) = row.find(needle) {
            // One cell per ASCII character, so the byte offset is the column.
            return Some((byte as u16, y));
        }
    }
    None
}

// --- snapshots -------------------------------------------------------------

#[test]
fn empty() {
    let app = live(true, Some("flag"));
    snap("empty", &render_text(&app, 80, 24));
}

#[test]
fn searching() {
    let mut app = live(false, None);
    feed(&mut app, LiveEvent::Source(Discovery::Searching));
    tick(&mut app, 2_000.0);
    snap("searching", &render_text(&app, 80, 24));
}

#[test]
fn recording_3_speakers() {
    snap("recording_3_speakers", &render_text(&meeting(), 80, 24));
}

#[test]
fn recording_with_audio() {
    let mut app = meeting();
    feed(&mut app, LiveEvent::Rec(true));
    feed(
        &mut app,
        LiveEvent::Audio("Audio system: audio device lost and could not be reopened".to_string()),
    );
    snap("recording_with_audio", &render_text(&app, 80, 24));
}

#[test]
fn long_lines_wrap() {
    let mut app = live(true, Some("flag"));
    started(&mut app, "Teams");
    attached(&mut app, "Weekly sync | Microsoft Teams");
    say(
        &mut app,
        "Ana",
        "Esta é uma frase muito longa para comprovar que o texto quebra em várias linhas \
         dentro da moldura sem sair da largura do terminal, e que a continuação fica \
         alinhada à esquerda do painel de transcrição, sem invadir a borda direita.",
        5_000.0,
    );
    say(
        &mut app,
        "Bruno",
        "supercalifragilisticexpialidocious_pneumonoultramicroscopicsilicovolcanoconiosis_extra",
        9_000.0,
    );
    tick(&mut app, 12_000.0);
    snap("long_lines_wrap", &render_text(&app, 80, 24));
}

#[test]
fn narrow_40() {
    let mut app = live(true, Some("flag"));
    started(&mut app, "Teams");
    attached(&mut app, "Weekly sync | Microsoft Teams");
    say(&mut app, "Ana", "Bom dia a todos.", 3_000.0);
    say(
        &mut app,
        "Bruno",
        "Eu já mandei o relatório ontem à noite.",
        12_000.0,
    );
    feed(
        &mut app,
        LiveEvent::Open(vec!["Carla: perfeito, obrigada".to_string()]),
    );
    update(&mut app, Msg::Resize(40, 15));
    tick(&mut app, 30_000.0);
    snap("narrow_40", &render_text(&app, 40, 15));
}

#[test]
fn no_captions_warning() {
    let mut app = live(false, None);
    started(&mut app, "Teams");
    attached(&mut app, "Weekly sync | Microsoft Teams");
    feed(&mut app, LiveEvent::NoCaptions { secs: 30 });
    tick(&mut app, 45_000.0);
    snap("no_captions_warning", &render_text(&app, 80, 24));
}

#[test]
fn source_error() {
    let mut app = live(false, None);
    started(&mut app, "Teams");
    feed(
        &mut app,
        LiveEvent::SourceError("the meeting window closed; looking again".to_string()),
    );
    snap("source_error", &render_text(&app, 80, 24));
}

#[test]
fn write_error() {
    let mut app = meeting();
    feed(
        &mut app,
        LiveEvent::WriteError("no space left on device".to_string()),
    );
    snap("write_error", &render_text(&app, 80, 24));
}

#[test]
fn paused_scrolled() {
    let mut app = live(true, Some("flag"));
    started(&mut app, "Teams");
    attached(&mut app, "Weekly sync | Microsoft Teams");
    for i in 0..30 {
        say(
            &mut app,
            if i % 2 == 0 { "Ana" } else { "Bruno" },
            &format!("Linha número {}", i + 1),
            i as f64 * 2_000.0,
        );
    }
    tick(&mut app, 60_000.0);
    key(&mut app, KeyCode::Char('p'));
    for _ in 0..3 {
        key(&mut app, KeyCode::Up);
    }
    for i in 30..35 {
        say(
            &mut app,
            "Carla",
            &format!("Linha número {}", i + 1),
            i as f64 * 2_000.0,
        );
    }
    tick(&mut app, 75_000.0);
    snap("paused_scrolled", &render_text(&app, 80, 24));
}

#[test]
fn help_overlay() {
    let mut app = meeting();
    key(&mut app, KeyCode::Char('?'));
    snap("help_overlay", &render_text(&app, 80, 24));
}

#[test]
fn ended_menu() {
    snap("ended_menu", &render_text(&ended_meeting(), 80, 24));
}

#[test]
fn ended_menu_no_record() {
    let mut app = App::new(false, None, PathBuf::from(DEFAULT_DIR), (80, 24));
    started_without_session(&mut app);
    say(&mut app, "Ana", "Só para ver na tela.", 3_000.0);
    ended(&mut app, EndReason::Stop);
    snap("ended_menu_no_record", &render_text(&app, 80, 24));
}

fn started_without_session(app: &mut App) {
    feed(
        app,
        LiveEvent::Started {
            session_dir: None,
            session_id: None,
            source: "Teams".to_string(),
        },
    );
}

#[test]
fn ask_folder() {
    let mut app = ended_meeting();
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    for c in "/Notas".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    snap("ask_folder", &render_text(&app, 80, 24));
}

fn tisty_form_app() -> App {
    let mut app = ended_meeting().with_tisty_default("Reuniones", "Trabajo");
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    app
}

#[test]
fn tisty_form() {
    let app = tisty_form_app();
    snap("tisty_form", &render_text(&app, 80, 24));
}

#[test]
fn tisty_confirm() {
    let mut app = tisty_form_app();
    mc_tui::app::tisty_previewed(
        &mut app,
        mc_tui::TistyTarget {
            folder: "Reuniones".into(),
            list: "Trabajo".into(),
        },
        Ok([
            "minutes: «Minuta: Daily de equipo (2026-10-20)» -> folder Reuniones",
            "task: Ana: cerrar el bug -> Trabajo, due 2026-10-27",
            "task: Luis: revisar el PR -> Trabajo",
        ]
        .join(
            "
",
        )),
    );
    snap("tisty_confirm", &render_text(&app, 80, 24));
}

#[test]
fn tisty_report_with_a_failure() {
    let mut app = tisty_form_app();
    mc_tui::app::tisty_published(
        &mut app,
        Err([
            "  created          Minuta: Daily de equipo (2026-10-20)",
            "  FAILED: Unknown list \"Trabajo\"",
            "nothing was lost: the minutes and commitments are still on disk. To try again: mc export /data/corpus/x --tisty",
        ]
        .join("
")),
    );
    snap("tisty_report_failed", &render_text(&app, 80, 24));
}

#[test]
fn done_saved() {
    let mut app = ended_meeting();
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Enter);
    mc_tui::app::save_finished(
        &mut app,
        Ok(PathBuf::from("/home/ana/meetings/a8bf3c69-run_01234.md")),
    );
    snap("done_saved", &render_text(&app, 80, 24));
}

#[test]
fn error_state() {
    let mut app = ended_meeting();
    mc_tui::app::save_finished(
        &mut app,
        Err("/ro/notas: permission denied (os error 13)".to_string()),
    );
    snap("error_state", &render_text(&app, 80, 24));
}

#[test]
fn showing_transcript() {
    let mut app = ended_meeting();
    key(&mut app, KeyCode::Enter);
    snap("showing_transcript", &render_text(&app, 80, 24));
}

#[test]
fn unicode_wide() {
    let mut app = live(true, Some("flag"));
    started(&mut app, "Teams");
    attached(&mut app, "Reunião | Teams");
    say(&mut app, "李雷", "我们今天讨论预算。", 3_000.0);
    say(&mut app, "Zoë Émile", "Está bien, café con leche.", 9_000.0);
    say(&mut app, "Ana 🎉", "太好了 🎉🎉 vamos lá", 15_000.0);
    say(&mut app, "李雷", "好的，谢谢大家。", 21_000.0);
    tick(&mut app, 30_000.0);
    snap("unicode_wide", &render_text(&app, 80, 24));
}

#[test]
fn open_lines_pane() {
    let mut app = meeting();
    feed(
        &mut app,
        LiveEvent::Open(vec![
            "Ana: estou falando agora".to_string(),
            "Bruno: e eu também".to_string(),
            "Carla: um, dois, três".to_string(),
            "Dani: quatro".to_string(),
        ]),
    );
    snap("open_lines_pane", &render_text(&app, 80, 24));
}

#[test]
fn tiny_10x5() {
    snap("tiny_10x5", &render_text(&meeting(), 10, 5));
}

#[test]
fn tiny_3x2() {
    snap("tiny_3x2", &render_text(&meeting(), 3, 2));
}

#[test]
fn degenerate_sizes_do_not_panic() {
    let app = meeting();
    let _ = render_text(&app, 80, 1);
    let _ = render_text(&app, 1, 24);
    let _ = render_text(&app, 1, 1);
    let _ = render_text(&ended_meeting(), 80, 1);
    let _ = render_text(&ended_meeting(), 1, 24);
    let mut help = meeting();
    key(&mut help, KeyCode::Char('?'));
    let _ = render_text(&help, 5, 4);
    let _ = render_text(&help, 12, 5);
}

// --- styles ------------------------------------------------------------------

#[test]
fn rec_led_is_red_only_while_recording() {
    let recording = draw(&meeting(), 80, 24);
    let cell = &recording[(0, 0)];
    assert_eq!(cell.symbol(), "●");
    assert_eq!(cell.fg, Color::Red);

    let mut idle = live(false, None);
    started(&mut idle, "Teams");
    let cell = &draw(&idle, 80, 24)[(0, 0)];
    assert_eq!(cell.symbol(), "○");
    assert_ne!(cell.fg, Color::Red);
}

#[test]
fn first_two_speakers_have_different_colours_and_they_stay_put() {
    let mut app = live(true, Some("flag"));
    started(&mut app, "Teams");
    attached(&mut app, "Weekly sync | Microsoft Teams");
    say(&mut app, "Ana", "primeira fala", 1_000.0);
    say(&mut app, "Bruno", "segunda fala", 2_000.0);
    tick(&mut app, 5_000.0);

    let before = draw(&app, 80, 24);
    let (ax, ay) = find(&before, "Ana:").expect("Ana is shown");
    let (bx, by) = find(&before, "Bruno:").expect("Bruno is shown");
    let ana = before[(ax, ay)].fg;
    let bruno = before[(bx, by)].fg;
    assert_ne!(ana, bruno, "the first two speakers get different colours");

    say(&mut app, "Carla", "terceira fala", 3_000.0);
    say(&mut app, "Ana", "de novo", 4_000.0);
    tick(&mut app, 6_000.0);
    let after = draw(&app, 80, 24);
    let (ax, ay) = find(&after, "Ana:").expect("Ana is shown");
    let (bx, by) = find(&after, "Bruno:").expect("Bruno is shown");
    assert_eq!(after[(ax, ay)].fg, ana, "Ana keeps her colour");
    assert_eq!(after[(bx, by)].fg, bruno, "Bruno keeps his colour");
}

// --- the ledger panel and the questions --------------------------------------

fn ledger() -> mc_analyst::Ledger {
    serde_json::from_str(
        r#"{
        "topics": [
            {"id": "t1", "title": "Plan del sprint", "status": "closed", "started_at": 0, "evidence": [0]},
            {"id": "t2", "title": "Bloqueo del deploy en staging", "status": "active", "started_at": 12000, "evidence": [1, 2]}
        ],
        "decisions": [
            {"id": "d1", "text": "Cerrar el pipeline el viernes", "inferred": false, "evidence": [2]},
            {"id": "d2", "text": "Mover la demo al martes", "inferred": true, "evidence": [3, 4, 5, 6, 7, 8]}
        ],
        "commitments": [
            {"id": "c1", "who": "Bruno", "what": "enviar el informe de ayer", "due": "el viernes", "confidence": 0.9, "inferred": false, "evidence": [1]}
        ],
        "open_questions": [
            {"id": "q1", "text": "¿Quién avisa al cliente del retraso?", "evidence": [3]}
        ]
    }"#,
    )
    .expect("a ledger")
}

fn ledger_event(ledger: mc_analyst::Ledger) -> LiveEvent {
    LiveEvent::Ledger(Box::new(mc_engine::live_analyst::LedgerUpdate {
        ledger,
        changed: Vec::new(),
        event: mc_analyst::LedgerEvent {
            first_id: 0,
            last_id: 3,
            segments: 4,
            model: "fake".into(),
            attempts: 1,
            accepted: 5,
            rejected: 0,
            rejections: Vec::new(),
            error: None,
        },
    }))
}

fn analysed(width: u16, height: u16) -> App {
    let mut app = meeting().with_analyst(true);
    update(&mut app, Msg::Resize(width, height));
    feed(&mut app, ledger_event(ledger()));
    app
}

#[test]
fn ledger_panel_wide() {
    snap(
        "ledger_panel_wide",
        &render_text(&analysed(120, 24), 120, 24),
    );
}

#[test]
fn ledger_panel_scrolled() {
    let mut app = analysed(100, 12);
    for _ in 0..4 {
        key(&mut app, KeyCode::Char(']'));
    }
    snap("ledger_panel_scrolled", &render_text(&app, 100, 12));
}

#[test]
fn ledger_panel_waiting_with_an_error() {
    let mut app = meeting().with_analyst(true);
    update(&mut app, Msg::Resize(110, 20));
    feed(
        &mut app,
        LiveEvent::AnalystError(
            "segments #0-#19 not analysed: the model did not answer in time".into(),
        ),
    );
    snap("ledger_panel_waiting", &render_text(&app, 110, 20));
}

#[test]
fn the_panel_is_hidden_on_a_narrow_terminal() {
    let app = analysed(99, 24);
    assert!(app.panel_width().is_none());
    let text = render_text(&app, 99, 24);
    assert!(!text.contains("Ledger"));
    snap("ledger_panel_narrow", &text);
}

#[test]
fn the_panel_can_be_hidden_with_l() {
    let mut app = analysed(120, 24);
    assert!(render_text(&app, 120, 24).contains("Ledger"));
    key(&mut app, KeyCode::Char('l'));
    assert!(!render_text(&app, 120, 24).contains("Ledger"));
    key(&mut app, KeyCode::Char('l'));
    assert!(render_text(&app, 120, 24).contains("Ledger"));
}

#[test]
fn help_overlay_with_the_analyst() {
    let mut app = analysed(80, 24);
    key(&mut app, KeyCode::Char('?'));
    snap("help_overlay_analyst", &render_text(&app, 80, 24));
}

#[test]
fn asking_for_a_note() {
    let mut app = meeting();
    key(&mut app, KeyCode::Char('n'));
    for c in "revisar el PR".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    snap("modal_note", &render_text(&app, 80, 24));
}

#[test]
fn choosing_a_speaker_to_rename() {
    let mut app = meeting();
    key(&mut app, KeyCode::Char('s'));
    key(&mut app, KeyCode::Down);
    snap("modal_speaker_pick", &render_text(&app, 80, 24));
}

#[test]
fn a_renamed_speaker_shows_with_the_new_name() {
    let mut app = meeting();
    key(&mut app, KeyCode::Char('s'));
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    for c in "Bruno S.".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    key(&mut app, KeyCode::Enter);
    snap("speaker_renamed", &render_text(&app, 80, 24));
}

#[test]
fn a_notice_rides_in_front_of_a_write_warning() {
    let mut app = meeting();
    feed(&mut app, LiveEvent::WriteError("disk full".into()));
    key(&mut app, KeyCode::Char('x'));
    update(&mut app, Msg::Notice("note saved".into()));
    snap("notice_with_write_error", &render_text(&app, 80, 24));
}

#[test]
fn tisty_confirm_long_plan_scrolled() {
    let mut app = tisty_form_app();
    let plan: Vec<String> = (1..=30)
        .map(|i| format!("task: Ana: tarea número {i} -> Trabajo"))
        .collect();
    mc_tui::app::tisty_previewed(
        &mut app,
        mc_tui::TistyTarget::default(),
        Ok(plan.join(
            "
",
        )),
    );
    key(&mut app, KeyCode::PageDown);
    snap("tisty_confirm_scrolled", &render_text(&app, 80, 24));
}
