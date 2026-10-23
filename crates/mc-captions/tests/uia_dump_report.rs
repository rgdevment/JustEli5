//! `mc doctor --dump` (through the fake backend) and `mc doctor --uia` (the
//! report, with an injected timer). Golden files live in tests/golden; to
//! regenerate them run the tests with MC_UPDATE_GOLDEN=1 and read the diff.

use std::fs;
use std::path::{Path, PathBuf};

use mc_captions::ui::backend::{Budget, Target, UiBackend, UiError};
use mc_captions::ui::dump::{DumpOptions, dump};
use mc_captions::ui::fake::{FakeBackend, Fixture};
use mc_captions::ui::node::{Role, UiNode, WindowInfo};
use mc_captions::ui::report::{UiaReport, format_report, uia_report};
use mc_captions::ui::selectors::Selectors;
use mc_captions::ui::uia_map::map_hresult;
use mc_captions::ui::{teams, zoom};

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/uia")
        .join(format!("{name}.json"))
}

fn load(name: &str) -> Fixture {
    let text = fs::read_to_string(fixture_path(name)).unwrap();
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn backend(name: &str) -> FakeBackend {
    FakeBackend::from_fixture(load(name))
}

fn golden(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("MC_UPDATE_GOLDEN").is_some() {
        fs::write(&path, actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("{} is missing: run with MC_UPDATE_GOLDEN=1", path.display()));
    assert_eq!(
        actual, expected,
        "{name} differs from the golden file (MC_UPDATE_GOLDEN=1 regenerates it)"
    );
}

/// A timer that advances by `step` milliseconds on every call.
fn stepping(step: f64) -> impl FnMut() -> f64 {
    let mut now = 0.0;
    move || {
        now += step;
        now
    }
}

fn json_text(fixture: &Fixture) -> String {
    serde_json::to_string_pretty(fixture).unwrap() + "\n"
}

// --- dump ----------------------------------------------------------------------------

#[test]
fn dump_of_teams_three_speakers_matches_the_golden_file() {
    let sel = Selectors::default();
    let mut fake = backend("teams_three_speakers");
    let fixture = dump(&mut fake, &sel, &DumpOptions::default()).unwrap();
    assert!(fixture.anonymized);
    assert_eq!(fixture.windows.len(), 1);
    golden("dump_teams_three_speakers.json", &json_text(&fixture));
}

#[test]
fn raw_dump_keeps_the_real_names_and_says_so() {
    let sel = Selectors::default();
    let options = DumpOptions {
        raw: true,
        ..DumpOptions::default()
    };
    let fixture = dump(&mut backend("teams_three_speakers"), &sel, &options).unwrap();
    assert!(!fixture.anonymized);
    assert!(fixture.note.contains("raw"), "{}", fixture.note);
    let text = json_text(&fixture);
    for name in ["Ana García", "Luis Pérez", "María Soto", "Buenos días."] {
        assert!(text.contains(name), "raw dump lost {name}");
    }
    assert!(!text.contains("Speaker A"));
}

#[test]
fn anonymised_dump_contains_no_real_speaker_or_caption_text() {
    let sel = Selectors::default();
    let text = json_text(
        &dump(
            &mut backend("teams_three_speakers"),
            &sel,
            &DumpOptions::default(),
        )
        .unwrap(),
    );
    for name in [
        "Ana", "García", "Luis", "Pérez", "María", "Soto", "Buenos", "Tengo", "Adelante",
    ] {
        assert!(!text.contains(name), "anonymised dump still says {name:?}");
    }
    assert!(text.contains("Speaker A") && text.contains("Speaker B") && text.contains("Speaker C"));
}

#[test]
fn window_selection_follows_the_requested_indexes_in_their_order() {
    let sel = Selectors::default();
    let raw = |windows: Option<Vec<usize>>| DumpOptions {
        raw: true,
        windows,
        ..DumpOptions::default()
    };
    let handles = |f: &Fixture| f.windows.iter().map(|w| w.info.handle).collect::<Vec<_>>();
    let title = |f: &Fixture| f.windows[0].info.title.clone();

    // teams_popout_and_main lists the main window (handle 1) first, the pop-out (2) second.
    let popout = dump(
        &mut backend("teams_popout_and_main"),
        &sel,
        &raw(Some(vec![1])),
    )
    .unwrap();
    assert_eq!(handles(&popout), vec![2]);
    assert_eq!(title(&popout), "Subtítulos | Reunión X | Microsoft Teams");

    let both = dump(
        &mut backend("teams_popout_and_main"),
        &sel,
        &raw(Some(vec![1, 0])),
    )
    .unwrap();
    assert_eq!(
        handles(&both),
        vec![2, 1],
        "indexes keep the order asked for"
    );

    let every = dump(&mut backend("teams_popout_and_main"), &sel, &raw(None)).unwrap();
    assert_eq!(
        handles(&every),
        vec![1, 2],
        "no index: every candidate window"
    );
}

#[test]
fn every_dumped_tree_is_rooted_at_a_bare_window_raw_or_not() {
    let sel = Selectors::default();
    let mut problems = Vec::new();
    let mut names: Vec<String> = fs::read_dir(fixture_path("x").parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .filter(|n| {
            n.ends_with(".json") && !n.ends_with(".expected.json") && n != "zoom_items.json"
        })
        .map(|n| n.trim_end_matches(".json").to_string())
        .collect();
    names.sort();
    assert!(names.len() >= 25, "{} fixtures", names.len());
    for name in &names {
        let fixture = load(name);
        for raw in [false, true] {
            let options = DumpOptions {
                platform: fixture.platform.clone(),
                raw,
                ..DumpOptions::default()
            };
            let dumped = dump(&mut backend(name), &sel, &options).unwrap();
            for (index, window) in dumped.windows.iter().enumerate() {
                let root = &window.tree;
                if root.role != Role::Window
                    || !root.name.is_empty()
                    || !root.class_name.is_empty()
                    || !root.automation_id.is_empty()
                {
                    problems.push(format!("{name}[{index}] raw={raw}: root is {root:?}"));
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

// --- report --------------------------------------------------------------------------

fn report_of(
    name: &str,
    selectors: &Selectors,
    max_nodes: usize,
    timer: &mut dyn FnMut() -> f64,
) -> UiaReport {
    uia_report(&mut backend(name), selectors, max_nodes, timer).unwrap()
}

#[test]
fn teams_report_fields_on_three_speakers() {
    let sel = Selectors::default();
    let fixture = load("teams_three_speakers");
    let mut timer = stepping(1.0);
    let report = report_of("teams_three_speakers", &sel, 20_000, &mut timer);

    assert_eq!(report.selectors_version, 5);
    assert_eq!(report.windows.len(), 1);
    assert_eq!(report.windows[0].class, "TeamsWebView");
    assert_eq!(report.windows[0].index, 0);
    // The Zoom title fallback also matches this window ("Reunión", see the
    // suspected-bug test below), so pick the Teams row by platform.
    let pick = report
        .picks
        .iter()
        .find(|p| p.platform == "teams")
        .expect("a teams pick");
    assert_eq!(pick.window_index, 0);
    assert_eq!(pick.tier, "Meeting");
    assert_eq!(pick.discovery_nodes, fixture.windows[0].tree.count());
    assert_eq!(pick.discovery_ms, 1.0);
    let container = teams::find_container(&fixture.windows[0].tree, &sel.teams);
    assert!(!container.is_empty(), "the RootWebArea is the container");
    assert_eq!(pick.container_path.as_deref(), Some(container.as_slice()));
    assert!(!pick.whole_window_fallback);
    assert_eq!(pick.captions, 3);
    assert_eq!(pick.poll_ms, 1.0);
    assert!(pick.poll_nodes > 0 && pick.poll_nodes < pick.discovery_nodes);
    assert!(pick.error.is_none());
    assert!(report.advice.contains(&"Teams tree is tiny: Chromium accessibility may be off. Retry in a few seconds, or start Narrator once.".to_string()), "{:?}", report.advice);
}

#[test]
fn teams_three_speakers_report_matches_the_golden_text() {
    let mut timer = stepping(1.0);
    let report = report_of(
        "teams_three_speakers",
        &Selectors::default(),
        20_000,
        &mut timer,
    );
    golden("report_teams_three_speakers.txt", &format_report(&report));
}

#[test]
fn zoom_report_on_class_match_matches_the_golden_text_and_fields() {
    let sel = Selectors::default();
    let fixture = load("zoom_class_match");
    let mut timer = stepping(2.0);
    let report = report_of("zoom_class_match", &sel, 20_000, &mut timer);

    assert_eq!(report.windows.len(), 2);
    assert_eq!(report.picks.len(), 1, "no Teams window, one Zoom pick");
    let pick = &report.picks[0];
    assert_eq!(pick.platform, "zoom");
    assert_eq!(pick.window_index, 1, "the meeting class beats the browser");
    assert_eq!(pick.tier, "Meeting");
    let list = zoom::find_container(&fixture.windows[1].tree, &sel.zoom);
    assert_eq!(pick.container_path, list);
    assert_eq!(pick.captions, 2);
    assert!(report.advice.is_empty(), "{:?}", report.advice);
    golden("report_zoom_class_match.txt", &format_report(&report));
}

#[test]
fn teams_popout_is_reported_with_its_tier() {
    let mut timer = stepping(1.0);
    let report = report_of(
        "teams_popout_and_main",
        &Selectors::default(),
        20_000,
        &mut timer,
    );
    let pick = report
        .picks
        .iter()
        .find(|p| p.platform == "teams")
        .expect("a teams pick");
    assert_eq!(pick.window_index, 1);
    assert_eq!(pick.tier, "CaptionPopout");
    assert_eq!(pick.captions, 1);
}

#[test]
fn format_report_lists_every_window_row_and_marks_the_fallback() {
    let mut timer = stepping(1.0);
    let report = report_of(
        "teams_popout_and_main",
        &Selectors::default(),
        20_000,
        &mut timer,
    );
    let text = format_report(&report);
    assert!(text.starts_with("selectors v5\nwindows:\n"), "{text}");
    assert!(text.contains("  [0] TeamsWebView"), "{text}");
    assert!(text.contains("  [1] TeamsWebView"), "{text}");
    assert!(text.contains("teams: window [1] (CaptionPopout)"), "{text}");
}

// --- advice --------------------------------------------------------------------------

#[test]
fn no_meeting_window_says_how_to_get_one() {
    let mut fake = FakeBackend::new(mc_captions::ui::fake::World::default());
    let report = uia_report(&mut fake, &Selectors::default(), 20_000, &mut stepping(1.0)).unwrap();
    assert!(report.picks.is_empty());
    assert_eq!(
        report.advice,
        vec![
            "No meeting window found (Teams, Zoom, Meet, Teams web, Slack): open the meeting (or the caption pop-out)."
                .to_string()
        ]
    );
}

#[test]
fn a_tree_of_fifty_or_more_nodes_is_not_called_tiny() {
    let caption = |i: usize| {
        UiNode::new(Role::Group, "").with_children(vec![
            UiNode::new(
                Role::Text,
                &format!("Persona {}", char::from(b'A' + i as u8)),
            ),
            UiNode::new(Role::Text, "texto de prueba"),
        ])
    };
    let document = UiNode::new(Role::Document, "")
        .with_aid("RootWebArea")
        .with_children((0..20).map(caption).collect());
    let tree = UiNode::new(Role::Window, "").with_children(vec![
        UiNode::new(Role::Pane, "").with_children(vec![document]),
    ]);
    assert!(tree.count() >= 50, "{}", tree.count());
    let fixture = Fixture {
        format: "mc-uia-tree".to_string(),
        version: 1,
        platform: "teams".to_string(),
        app: String::new(),
        anonymized: true,
        note: String::new(),
        windows: vec![mc_captions::ui::fake::FakeWindow {
            info: WindowInfo {
                handle: 1,
                title: "Reunión | Microsoft Teams".to_string(),
                class_name: "TeamsWebView".to_string(),
                pid: 4001,
                process: None,
            },
            tree,
        }],
        script: Vec::new(),
    };
    let mut fake = FakeBackend::from_fixture(fixture);
    let report = uia_report(&mut fake, &Selectors::default(), 20_000, &mut stepping(1.0)).unwrap();
    assert_eq!(report.picks[0].captions, 20);
    assert!(
        report.advice.iter().all(|a| !a.contains("tiny")),
        "{:?}",
        report.advice
    );
}

#[test]
fn a_tiny_teams_tree_is_flagged() {
    let report = report_of(
        "teams_three_speakers",
        &Selectors::default(),
        20_000,
        &mut stepping(1.0),
    );
    assert!(report.picks[0].discovery_nodes < 50);
    assert!(
        report
            .advice
            .iter()
            .any(|a| a.starts_with("Teams tree is tiny"))
    );
}

/// SUSPECTED BUG (zoom::pick_window, inherited from the original Python capture):
/// the title fallback matches any window whose title contains "reunión",
/// "meeting", "zoom" or "webinar", so a Teams meeting window is also reported
/// as a Zoom meeting ("container none", "no caption list" advice).
#[test]
fn zoom_does_not_pick_a_teams_window() {
    let report = report_of(
        "teams_three_speakers",
        &Selectors::default(),
        20_000,
        &mut stepping(1.0),
    );
    assert!(
        report.picks.iter().all(|p| p.platform != "zoom"),
        "{:?}",
        report.picks
    );
}

#[test]
fn zoom_without_a_caption_list_says_to_turn_captions_on() {
    let report = report_of(
        "zoom_named_children_no_list",
        &Selectors::default(),
        20_000,
        &mut stepping(1.0),
    );
    assert_eq!(report.picks.len(), 1);
    assert_eq!(report.picks[0].platform, "zoom");
    assert!(report.picks[0].container_path.is_none());
    assert!(
        report.advice.contains(
            &"Zoom: no caption list found; turn on Closed Captions in the meeting.".to_string()
        ),
        "{:?}",
        report.advice
    );
}

#[test]
fn a_budget_that_is_too_small_is_reported_as_an_error_with_advice() {
    let report = report_of(
        "teams_three_speakers",
        &Selectors::default(),
        5,
        &mut stepping(1.0),
    );
    let pick = &report.picks[0];
    assert_eq!(
        pick.error.as_deref(),
        Some("the tree walk exceeded its node or time budget")
    );
    assert_eq!(pick.discovery_nodes, 0);
    assert!(
        report
            .advice
            .contains(&"teams: the window tree exceeds 5 nodes; raise capture limits.".to_string()),
        "{:?}",
        report.advice
    );
}

#[test]
fn a_poll_slower_than_250_ms_is_flagged() {
    // 300 ms per timer call: discovery and poll both take 300 ms.
    let report = report_of(
        "teams_three_speakers",
        &Selectors::default(),
        20_000,
        &mut stepping(300.0),
    );
    let pick = &report.picks[0];
    assert_eq!(pick.poll_ms, 300.0);
    assert!(
        report
            .advice
            .contains(&"teams: a poll takes 300 ms (> 250 ms); the capture will lag.".to_string()),
        "{:?}",
        report.advice
    );
}

#[test]
fn a_poll_at_exactly_250_ms_is_not_flagged() {
    let report = report_of(
        "teams_three_speakers",
        &Selectors::default(),
        20_000,
        &mut stepping(250.0),
    );
    assert_eq!(report.picks[0].poll_ms, 250.0);
    assert!(
        report.advice.iter().all(|a| !a.contains("poll takes")),
        "{:?}",
        report.advice
    );
}

/// A window that is listed but refuses every snapshot, as an elevated app does.
struct Refusing {
    windows: Vec<WindowInfo>,
    error: UiError,
}

impl UiBackend for Refusing {
    fn windows(&mut self) -> Result<Vec<WindowInfo>, UiError> {
        Ok(self.windows.clone())
    }
    fn snapshot(&mut self, _: &Target, _: &Budget) -> Result<UiNode, UiError> {
        Err(self.error.clone())
    }
    fn alive(&mut self, _: u64) -> bool {
        true
    }
}

#[test]
fn access_denied_gets_the_elevation_advice() {
    let mut refusing = Refusing {
        windows: vec![WindowInfo {
            handle: 1,
            title: "Reunión | Microsoft Teams".to_string(),
            class_name: "TeamsWebView".to_string(),
            pid: 4001,
            process: None,
        }],
        error: map_hresult(0x8007_0005, true),
    };
    let report = uia_report(
        &mut refusing,
        &Selectors::default(),
        20_000,
        &mut stepping(1.0),
    )
    .unwrap();
    assert!(
        report.picks[0]
            .error
            .as_deref()
            .unwrap()
            .contains("access denied")
    );
    assert!(
        report.advice.contains(
            &"Access denied: one of the apps runs elevated; run both at the same level."
                .to_string()
        ),
        "{:?}",
        report.advice
    );
}

#[test]
fn other_snapshot_errors_are_named_in_the_advice() {
    let mut refusing = Refusing {
        windows: vec![WindowInfo {
            handle: 1,
            title: "Reunión | Microsoft Teams".to_string(),
            class_name: "TeamsWebView".to_string(),
            pid: 4001,
            process: None,
        }],
        error: UiError::WindowGone,
    };
    let report = uia_report(
        &mut refusing,
        &Selectors::default(),
        20_000,
        &mut stepping(1.0),
    )
    .unwrap();
    assert_eq!(report.picks[0].error.as_deref(), Some("window is gone"));
    assert!(
        report
            .advice
            .contains(&"teams: could not read the window (window is gone).".to_string())
    );
}
