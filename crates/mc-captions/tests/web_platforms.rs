//! Google Meet, Teams web and Slack over synthetic trees (`tests/web_fixtures`).
//! The real trees are unverified until dumped (see the README), so these pin the
//! rules the defaults encode: which window is the meeting and how captions are read.

use std::path::{Path, PathBuf};

use mc_captions::ui::anon::anonymise;
use mc_captions::ui::dump::{DumpOptions, dump};
use mc_captions::ui::report::uia_report;
use mc_captions::ui::{FakeBackend, Fixture, NodeMode, Platform, Selectors, UiaSource};
use mc_captions::{CaptureSource, Discovery, SourceError};

type Pairs = Vec<(Option<String>, String)>;

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/web_fixtures")
}

fn load(name: &str) -> Fixture {
    let text = std::fs::read_to_string(dir().join(format!("{name}.json"))).unwrap();
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn source(name: &str, platform: Platform) -> UiaSource<FakeBackend> {
    let backend = FakeBackend::from_fixture(load(name));
    UiaSource::new(backend, platform, Selectors::default(), NodeMode::All)
}

fn attach(source: &mut UiaSource<FakeBackend>) -> String {
    match source.discover().unwrap() {
        Discovery::Attached { window } => window,
        other => panic!("expected an attach, got {other:?}"),
    }
}

fn read(source: &mut UiaSource<FakeBackend>) -> Pairs {
    source
        .poll()
        .unwrap()
        .into_iter()
        .map(|o| (o.speaker, o.text.unwrap_or_default()))
        .collect()
}

fn said(speaker: &str, text: &str) -> (Option<String>, String) {
    (Some(speaker.to_string()), text.to_string())
}

#[test]
fn meet_reads_every_speaker_and_follows_a_growing_line() {
    let mut meet = source("meet_two_speakers_growing", Platform::Meet);
    assert_eq!(attach(&mut meet), "Meet - abc-defg-hij - Google Chrome");
    assert_eq!(meet.meeting_name().as_deref(), Some("abc-defg-hij"));
    assert_eq!(
        read(&mut meet),
        [
            said("Ana García", "Hello everyone."),
            said("Luis Pérez", "Good morning.")
        ]
    );
    meet.backend_mut().advance();
    assert_eq!(
        read(&mut meet)[0],
        said("Ana García", "Hello everyone. Let us start with the budget")
    );
    meet.backend_mut().advance();
    let all = read(&mut meet);
    assert_eq!(all.len(), 3);
    assert_eq!(all[2], said("María Soto", "Can you hear me?"));
}

#[test]
fn meet_audio_follows_the_browser_window_not_the_other_tabs_window() {
    let mut meet = source("meet_two_speakers_growing", Platform::Meet);
    assert_eq!(meet.window_pid(), None);
    attach(&mut meet);
    // The Gmail window comes first and mentions Meet in a tab; the meeting window is the other one.
    assert_eq!(meet.window_pid(), Some(7002));
}

#[test]
fn meet_with_spanish_labels_and_nested_groups() {
    let mut meet = source("meet_spanish_nested", Platform::Meet);
    assert_eq!(
        attach(&mut meet),
        "Meet \u{2013} abc-defg-hij - Microsoft Edge"
    );
    assert_eq!(meet.meeting_name().as_deref(), Some("abc-defg-hij"));
    assert_eq!(
        read(&mut meet),
        [
            said("Ana García", "Buenos días a todos, empezamos."),
            said("Luis Pérez", "Tengo una duda.")
        ]
    );
}

#[test]
fn meet_reads_the_flattened_tree_of_a_real_chrome_dump() {
    let mut meet = source("meet_chrome_flat_dump", Platform::Meet);
    assert_eq!(attach(&mut meet), "Meet - xxx-xxxx-xxx - Xxxxxx Xxxxxx");
    assert_eq!(
        read(&mut meet),
        [
            said("Xxx Xxxxx", "Xxxx xxxxxx, xxxxxxxxx xx xxxxx."),
            said(
                "Xxxx",
                "Xxx xxxxxx xx xxxxxxx x xxxxxx xx xxxxxx xx Xxxx, xxxxx xx xxxxx xxxx."
            )
        ]
    );
}

#[test]
fn meet_keeps_text_without_a_speaker_and_never_invents_one() {
    let mut meet = source("meet_missing_speaker", Platform::Meet);
    attach(&mut meet);
    assert_eq!(
        read(&mut meet),
        [
            (None, "Hello without a name".to_string()),
            said("Luis Pérez", "Hi.")
        ]
    );
}

#[test]
fn meet_attaches_with_captions_off_and_reads_them_once_on() {
    let mut meet = source("meet_captions_off_then_on", Platform::Meet);
    attach(&mut meet);
    assert!(read(&mut meet).is_empty());
    meet.backend_mut().advance();
    attach(&mut meet);
    assert_eq!(read(&mut meet), [said("Ana García", "Hello.")]);
}

#[test]
fn meet_last_mode_keeps_only_the_newest_caption() {
    let backend = FakeBackend::from_fixture(load("meet_two_speakers_growing"));
    let mut meet = UiaSource::new(
        backend,
        Platform::Meet,
        Selectors::default(),
        NodeMode::Last,
    );
    attach(&mut meet);
    assert_eq!(read(&mut meet), [said("Luis Pérez", "Good morning.")]);
}

#[test]
fn teams_web_and_teams_desktop_are_not_confused() {
    let mut web = source("teams_web_and_desktop", Platform::TeamsWeb);
    assert_eq!(
        attach(&mut web),
        "Reunión de equipo | Microsoft Teams - Microsoft Edge"
    );
    assert_eq!(web.window_pid(), Some(7101));
    assert_eq!(
        web.meeting_name().as_deref(),
        Some("Reunión de equipo | Microsoft Teams")
    );
    assert_eq!(
        read(&mut web),
        [
            said("Ana García", "Hola desde el navegador."),
            said("Luis Pérez", "Te oigo bien.")
        ]
    );

    let mut desktop = source("teams_web_and_desktop", Platform::Teams);
    assert_eq!(attach(&mut desktop), "Reunión de equipo | Microsoft Teams");
    assert_eq!(desktop.window_pid(), Some(7100));
    assert_eq!(
        read(&mut desktop),
        [said("Marta Ruiz", "Hola desde la aplicación.")]
    );
}

#[test]
fn slack_reads_the_huddle_window_and_not_the_main_window() {
    let mut slack = source("slack_huddle_desktop", Platform::Slack);
    assert_eq!(attach(&mut slack), "Huddle with Ana, Luis");
    assert_eq!(slack.window_pid(), Some(7202));
    assert_eq!(
        read(&mut slack),
        [
            said("Ana García", "Shall we start?"),
            said("Luis Pérez", "Yes.")
        ]
    );
    slack.backend_mut().advance();
    assert_eq!(read(&mut slack)[1], said("Luis Pérez", "Yes, go ahead."));
}

#[test]
fn slack_in_a_browser_with_spanish_labels() {
    let mut slack = source("slack_browser_spanish", Platform::Slack);
    attach(&mut slack);
    assert_eq!(slack.window_pid(), Some(7210));
    assert_eq!(read(&mut slack), [said("Ana García", "Hola equipo.")]);
}

#[test]
fn slack_without_a_huddle_does_not_attach() {
    let mut slack = source("slack_no_huddle", Platform::Slack);
    assert!(matches!(slack.discover().unwrap(), Discovery::Searching));
    assert!(matches!(slack.poll(), Err(SourceError::Lost(_))));
    assert_eq!(slack.window_pid(), None);
}

#[test]
fn a_platform_only_picks_its_own_windows() {
    // Zoom's title fallback ("reunión") is deliberately left out: it is Zoom's own heuristic.
    let cases: [(&str, &[Platform]); 4] = [
        (
            "meet_two_speakers_growing",
            &[Platform::Teams, Platform::TeamsWeb, Platform::Slack],
        ),
        ("teams_web_and_desktop", &[Platform::Meet, Platform::Slack]),
        (
            "slack_huddle_desktop",
            &[Platform::Meet, Platform::TeamsWeb, Platform::Teams],
        ),
        (
            "slack_browser_spanish",
            &[Platform::Meet, Platform::TeamsWeb, Platform::Teams],
        ),
    ];
    let selectors = Selectors::default();
    for (name, others) in cases {
        let windows: Vec<_> = load(name).windows.into_iter().map(|w| w.info).collect();
        for &platform in others {
            assert!(
                platform.pick(&windows, &selectors).is_none(),
                "{name}: {platform:?} must not pick any window"
            );
        }
    }
}

const FIXTURES: [&str; 9] = [
    "meet_chrome_flat_dump",
    "meet_two_speakers_growing",
    "meet_spanish_nested",
    "meet_missing_speaker",
    "meet_captions_off_then_on",
    "teams_web_and_desktop",
    "slack_huddle_desktop",
    "slack_browser_spanish",
    "slack_no_huddle",
];

#[test]
fn the_anonymiser_hides_speakers_keeps_the_parse_and_is_a_fixed_point() {
    let selectors = Selectors::default();
    for name in FIXTURES {
        let fixture = load(name);
        let once = anonymise(&fixture, &selectors);
        assert_eq!(
            anonymise(&once, &selectors),
            once,
            "{name}: not a fixed point"
        );
        let text = serde_json::to_string(&once).unwrap();
        for real in ["García", "Pérez", "Soto", "Marta"] {
            assert!(!text.contains(real), "{name}: {real} survived");
        }
        let platform = Platform::parse(&fixture.platform).unwrap();
        let count = |f: Fixture| {
            let mut source = UiaSource::new(
                FakeBackend::from_fixture(f),
                platform,
                Selectors::default(),
                NodeMode::All,
            );
            source.discover().unwrap();
            source.poll().map(|o| o.len()).unwrap_or(0)
        };
        assert_eq!(count(once), count(fixture), "{name}: parse changed");
    }
}

#[test]
fn the_doctor_report_lists_the_new_platforms() {
    let mut backend = FakeBackend::from_fixture(load("meet_two_speakers_growing"));
    let mut clock = 0.0;
    let mut timer = || {
        clock += 1.0;
        clock
    };
    let report = uia_report(&mut backend, &Selectors::default(), 20_000, &mut timer).unwrap();
    assert_eq!(report.picks.len(), 1);
    assert_eq!(report.picks[0].platform, "meet");
    assert_eq!(report.picks[0].window_index, 1);
    assert_eq!(report.picks[0].captions, 2);
    // The Gmail window is nobody's meeting: its title stays hidden.
    assert!(report.windows[0].title.starts_with("(hidden"));
}

#[test]
fn a_dump_for_a_new_platform_keeps_only_candidate_windows_and_names_the_app() {
    let mut backend = FakeBackend::from_fixture(load("meet_two_speakers_growing"));
    let options = DumpOptions {
        platform: "meet".to_string(),
        ..DumpOptions::default()
    };
    let fixture = dump(&mut backend, &Selectors::default(), &options).unwrap();
    assert_eq!(fixture.app, "Google Meet");
    assert_eq!(fixture.windows.len(), 1);
    assert!(fixture.anonymized);
    assert_eq!(
        fixture.windows[0].info.title,
        "Meet - xxx-xxxx-xxx - Google Chrome"
    );
}

#[test]
fn the_new_selector_sections_validate_and_can_be_overridden() {
    let sel: Selectors = serde_json::from_str(
        r#"{"meet":{"region_names":["Live captions"]},"slack":{"huddle_hints":["call"]}}"#,
    )
    .unwrap();
    sel.validate().unwrap();
    assert_eq!(sel.meet.region_names, ["Live captions"]);
    assert!(serde_json::from_str::<Selectors>(r#"{"meet":{"nope":1}}"#).is_err());
    assert!(serde_json::from_str::<Selectors>(r#"{"slack":{"nope":1}}"#).is_err());
    assert!(serde_json::from_str::<Selectors>(r#"{"teams_web":{"nope":1}}"#).is_err());
    let zero: Selectors = serde_json::from_str(r#"{"meet":{"walk_depth":0}}"#).unwrap();
    assert!(zero.validate().is_err());
    let blank: Selectors = serde_json::from_str(r#"{"slack":{"region_names":[""]}}"#).unwrap();
    assert!(blank.validate().is_err());
}

fn info(title: &str, class: &str, process: &str) -> mc_captions::ui::WindowInfo {
    mc_captions::ui::WindowInfo {
        handle: 1,
        title: title.to_string(),
        class_name: class.to_string(),
        pid: 9,
        process: Some(process.to_string()),
    }
}

#[test]
fn other_electron_apps_are_neither_browsers_nor_slack() {
    let selectors = Selectors::default();
    let class = "Chrome_WidgetWin_1";
    let windows = vec![
        info("Meet - abc-defg-hij", class, "code.exe"),
        info("Sprint | Microsoft Teams", class, "teams.exe"),
        info("Slack | Acme", class, "notion.exe"),
        info("Acme Slack roadmap - Google Chrome", class, "chrome.exe"),
    ];
    for platform in [Platform::Meet, Platform::TeamsWeb, Platform::Slack] {
        assert!(
            platform.pick(&windows, &selectors).is_none(),
            "{platform:?} picked an unrelated window"
        );
    }
    let slack_tab = vec![info(
        "Huddle | Acme | Slack - Google Chrome",
        class,
        "chrome.exe",
    )];
    assert!(Platform::Slack.pick(&slack_tab, &selectors).is_some());
    let slack_app = vec![info("Acme", class, "Slack.exe")];
    assert!(Platform::Slack.pick(&slack_app, &selectors).is_some());
}

#[test]
fn slack_tries_the_windows_in_order_until_one_has_captions() {
    let mut fixture = load("slack_huddle_desktop");
    // The huddle-titled window has no captions; the main window does.
    let captions = fixture.windows[1].tree.clone();
    fixture.windows[1].tree = fixture.windows[0].tree.clone();
    fixture.windows[0].tree = captions;
    let mut slack = UiaSource::new(
        FakeBackend::from_fixture(fixture),
        Platform::Slack,
        Selectors::default(),
        NodeMode::All,
    );
    assert_eq!(attach(&mut slack), "general - Acme - Slack");
    assert_eq!(read(&mut slack).len(), 2);
}

#[test]
fn teams_points_to_teams_web_when_only_a_browser_tab_exists() {
    let mut backend = FakeBackend::from_fixture(load("teams_web_and_desktop"));
    let mut clock = 0.0;
    let mut timer = || {
        clock += 1.0;
        clock
    };
    let report = uia_report(&mut backend, &Selectors::default(), 20_000, &mut timer).unwrap();
    // Teams desktop exists in this fixture: no hint.
    assert!(report.advice.iter().all(|a| !a.contains("teams-web")));

    let mut fixture = load("teams_web_and_desktop");
    fixture
        .windows
        .retain(|w| w.info.class_name != "TeamsWebView");
    let mut backend = FakeBackend::from_fixture(fixture);
    let report = uia_report(&mut backend, &Selectors::default(), 20_000, &mut timer).unwrap();
    assert!(
        report
            .advice
            .iter()
            .any(|a| a.contains("--source teams-web"))
    );
}
