//! `UiaSource` over `FakeBackend` and scripted backends: caching, error mapping,
//! budgets, selector overrides, the pure window and caption parsers, and
//! random trees that must never panic. Parity with the original Python app lives in
//! `uia_parity.rs`; nothing here repeats it.

use std::path::{Path, PathBuf};

use mc_captions::ui::backend::{Budget, Target, UiBackend, UiError};
use mc_captions::ui::fake::{FIXTURE_FORMAT, FakeWindow, Fixture, World};
use mc_captions::ui::node::{Role, UiNode, WindowInfo};
use mc_captions::ui::selectors::{Selectors, TeamsSel, ZoomSel};
use mc_captions::ui::{FakeBackend, NodeMode, Platform, UiaSource, teams, zoom};
use mc_captions::{CaptureSource, Discovery, SourceError};

const TEAMS_CLASS: &str = "TeamsWebView";
const TEAMS_TITLE: &str = "Reunión con Ana | Microsoft Teams";

// ---------------------------------------------------------------- builders

fn group(speaker: &str, text: &str) -> UiNode {
    UiNode::new(Role::Group, "").with_children(vec![
        UiNode::new(Role::Text, speaker),
        UiNode::new(Role::Text, text),
    ])
}

/// The Teams caption document: a `RootWebArea` holding caption groups.
fn captions_doc(children: Vec<UiNode>) -> UiNode {
    UiNode::new(Role::Document, "")
        .with_aid("RootWebArea")
        .with_children(children)
}

fn teams_window(handle: u64, title: &str, tree: UiNode) -> FakeWindow {
    FakeWindow {
        info: WindowInfo {
            handle,
            title: title.to_string(),
            class_name: TEAMS_CLASS.to_string(),
            pid: 1,
            process: None,
        },
        tree,
    }
}

fn world(windows: Vec<FakeWindow>) -> World {
    World { windows }
}

/// One Teams window whose caption document holds the given speaker/text pairs.
fn teams_world(captions: &[(&str, &str)]) -> World {
    let groups = captions.iter().map(|(s, t)| group(s, t)).collect();
    let tree = UiNode::new(Role::Window, "").with_children(vec![captions_doc(groups)]);
    world(vec![teams_window(1, TEAMS_TITLE, tree)])
}

/// A window with `panes` empty panes before the caption document, so the
/// document sits at child index `panes`.
fn shifted_world(panes: usize, text: &str) -> World {
    let mut children: Vec<UiNode> = (0..panes).map(|_| UiNode::new(Role::Pane, "")).collect();
    children.push(captions_doc(vec![group("Ana García", text)]));
    let tree = UiNode::new(Role::Window, "").with_children(children);
    world(vec![teams_window(1, TEAMS_TITLE, tree)])
}

/// A window whose caption document has `nodes` empty panes and no captions.
fn huge_document_world(nodes: usize) -> World {
    let panes = (0..nodes).map(|_| UiNode::new(Role::Pane, "")).collect();
    let tree = UiNode::new(Role::Window, "").with_children(vec![captions_doc(panes)]);
    world(vec![teams_window(1, TEAMS_TITLE, tree)])
}

fn teams_source(backend: FakeBackend, selectors: Selectors) -> UiaSource<FakeBackend> {
    UiaSource::new(backend, Platform::Teams, selectors, NodeMode::All)
}

fn attached(source: &mut UiaSource<FakeBackend>) {
    assert!(
        matches!(source.discover(), Ok(Discovery::Attached { .. })),
        "discover did not attach"
    );
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/uia")
}

fn read_fixture(name: &str) -> Fixture {
    let text = std::fs::read_to_string(fixtures_dir().join(format!("{name}.json"))).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// Deterministic xorshift64: the same seed gives the same trees on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn percent(&mut self, p: u64) -> bool {
        self.below(100) < p
    }
}

fn random_role(rng: &mut Rng) -> Role {
    match rng.below(12) {
        0 => Role::Window,
        1 => Role::Pane,
        2 => Role::Document,
        3 => Role::Group,
        4 => Role::Text,
        5 => Role::List,
        6 => Role::ListItem,
        7 => Role::Edit,
        8 => Role::Button,
        9 => Role::Other("Slider".to_string()),
        10 => Role::Other(String::new()),
        _ => Role::Other("Custom".to_string()),
    }
}

fn random_text(rng: &mut Rng) -> String {
    const PIECES: [&str; 16] = [
        "Ana García",
        "Muted",
        "Micrófono",
        "Speaker 2 (Sala)",
        "Speaker 1 (Room, X)",
        ", ",
        ",",
        " | ",
        "Subtítulos | ",
        " | Ventana anclada",
        "😀",
        "\u{00a0}",
        "x",
        "(",
        ")",
        "Bob,text",
    ];
    let pieces = rng.below(5) as usize;
    (0..pieces)
        .map(|_| PIECES[rng.below(PIECES.len() as u64) as usize])
        .collect()
}

fn random_node(rng: &mut Rng) -> UiNode {
    let mut node = UiNode::new(random_role(rng), &random_text(rng));
    if rng.percent(30) {
        node.class_name = [
            "",
            "TeamsWebView",
            "ConfMultiTabContentWndClass",
            "ZConfCCRecieveWndExClass",
        ][rng.below(4) as usize]
            .to_string();
    }
    if rng.percent(30) {
        node.automation_id = ["RootWebArea", "other", ""][rng.below(3) as usize].to_string();
    }
    node
}

/// A random tree. `chain` is the percent chance a node has exactly one child,
/// which is what lets trees reach depth 60.
fn random_tree(rng: &mut Rng, depth: usize, chain: u64, budget: &mut usize) -> UiNode {
    let mut node = random_node(rng);
    *budget = budget.saturating_sub(1);
    if depth >= 60 || *budget == 0 {
        return node;
    }
    let children = if rng.percent(chain) {
        1
    } else {
        rng.below(4) as usize
    };
    for _ in 0..children {
        if *budget == 0 {
            break;
        }
        node.children
            .push(random_tree(rng, depth + 1, chain, budget));
    }
    node
}

fn depth_of(node: &UiNode) -> usize {
    node.children
        .iter()
        .map(depth_of)
        .max()
        .map_or(0, |d| d + 1)
}

fn for_each(node: &UiNode, f: &mut dyn FnMut(&UiNode)) {
    f(node);
    for child in &node.children {
        for_each(child, f);
    }
}

// ------------------------------------------------------- (a) caching and TTL

#[test]
fn polls_within_the_ttl_reuse_the_discovered_window() {
    let mut source = teams_source(
        FakeBackend::new(teams_world(&[("Ana García", "Hola a todos")])),
        Selectors::default(),
    );
    attached(&mut source);
    assert_eq!(source.backend().windows_calls, 1);

    for _ in 0..3 {
        let captions = source.poll().unwrap();
        assert_eq!(captions.len(), 1);
    }
    assert_eq!(
        source.backend().windows_calls,
        1,
        "windows() re-listed inside the TTL"
    );
    assert_eq!(source.stats.cache_hits, 3);
    assert_eq!(source.stats.discoveries, 1);
    assert_eq!(source.stats.polls, 3);
}

#[test]
fn a_zero_ttl_revalidates_on_every_poll() {
    let selectors: Selectors = serde_json::from_str(r#"{"teams":{"cache_ttl_s":0.0}}"#).unwrap();
    let mut source = teams_source(
        FakeBackend::new(teams_world(&[("Ana García", "Hola a todos")])),
        selectors,
    );
    attached(&mut source);
    for _ in 0..3 {
        source.poll().unwrap();
    }
    // The time to live is over at every poll, but the remembered container
    // still looks the same: it is revalidated instead of searched again.
    assert_eq!(source.backend().windows_calls, 1);
    assert_eq!(source.stats.cache_hits, 0);
    assert_eq!(source.stats.revalidations, 3);
    assert_eq!(source.stats.discoveries, 1);
}

// ----------------------------------------------------- (b) window closed

#[test]
fn a_killed_window_is_lost_then_searching_once_it_is_gone() {
    let backend = FakeBackend::scripted(vec![
        teams_world(&[("Ana García", "Hola a todos")]),
        World::default(),
    ]);
    let mut source = teams_source(backend, Selectors::default());
    attached(&mut source);

    source.backend_mut().kill(1);
    match source.poll() {
        Err(SourceError::Lost(message)) => assert_eq!(message, "window closed"),
        other => panic!("expected Lost, got {other:?}"),
    }

    // Still listed in the same world? No: killed handles are filtered out.
    assert!(matches!(source.poll(), Err(SourceError::Lost(_))));

    source.backend_mut().advance();
    assert!(
        matches!(source.discover(), Ok(Discovery::Searching)),
        "a world without the window must be Searching"
    );
    assert_eq!(source.meeting_name(), None, "no window, no name");
}

// ------------------------------------------------------ (c) node budgets

#[test]
fn discovery_over_the_node_budget_is_a_transient_error() {
    let panes = (0..20_000).map(|_| UiNode::new(Role::Pane, "")).collect();
    let tree = UiNode::new(Role::Window, "").with_children(panes);
    let backend = FakeBackend::new(world(vec![teams_window(1, TEAMS_TITLE, tree)]));
    let mut source = teams_source(backend, Selectors::default());

    // The budget error from the locate snapshot maps to Transient, not Searching.
    let err = source.discover().unwrap_err();
    assert!(matches!(err, SourceError::Transient(_)), "{err:?}");
    assert_eq!(source.backend().snapshot_calls, 1);
    assert!(
        source.backend().nodes_read <= 5001,
        "read {} nodes in one snapshot",
        source.backend().nodes_read
    );
}

#[test]
fn a_poll_over_the_node_budget_is_transient_and_reads_are_capped() {
    let backend = FakeBackend::scripted(vec![
        teams_world(&[("Ana García", "Hola a todos")]),
        huge_document_world(20_000),
    ]);
    let mut source = teams_source(backend, Selectors::default());
    attached(&mut source);

    let before = source.backend().nodes_read;
    source.backend_mut().advance();
    let err = source.poll().unwrap_err();
    assert!(matches!(err, SourceError::Transient(_)), "{err:?}");
    let read = source.backend().nodes_read - before;
    assert!(read <= 5001, "poll read {read} nodes");
}

/// Suspected bug: a locate that exceeds the budget returns early without
/// touching `transient_run`, so a tree that stays too big is Transient forever
/// and never becomes Lost. Expected: the third consecutive failure is Lost.
#[test]
fn a_persistently_oversized_tree_eventually_becomes_lost() {
    let backend = FakeBackend::scripted(vec![
        teams_world(&[("Ana García", "Hola a todos")]),
        huge_document_world(20_000),
    ]);
    let mut source = teams_source(backend, Selectors::default());
    attached(&mut source);
    source.backend_mut().advance();

    let results: Vec<_> = (0..3).map(|_| source.poll()).collect();
    assert!(
        matches!(results[2], Err(SourceError::Lost(_))),
        "third poll: {:?}",
        results[2]
    );
}

// --------------------------------------------- (d) container path goes stale

/// A backend whose remembered container is always stale: whole-window
/// snapshots work, snapshots at a path do not.
struct StaleBelowTheWindow(FakeBackend);

impl UiBackend for StaleBelowTheWindow {
    fn windows(&mut self) -> Result<Vec<WindowInfo>, UiError> {
        self.0.windows()
    }

    fn snapshot(&mut self, target: &Target, budget: &Budget) -> Result<UiNode, UiError> {
        if target.path.is_empty() {
            self.0.snapshot(target, budget)
        } else {
            Err(UiError::Stale)
        }
    }

    fn alive(&mut self, window: u64) -> bool {
        self.0.alive(window)
    }
}

#[test]
fn a_container_that_keeps_vanishing_is_transient_twice_then_lost() {
    let backend = StaleBelowTheWindow(FakeBackend::new(teams_world(&[("Ana García", "Hola")])));
    let mut source = UiaSource::new(
        backend,
        Platform::Teams,
        Selectors::default(),
        NodeMode::All,
    );
    assert!(matches!(source.discover(), Ok(Discovery::Attached { .. })));

    let first = source.poll();
    assert!(
        matches!(first, Err(SourceError::Transient(_))),
        "first: {first:?}"
    );
    let second = source.poll();
    assert!(
        matches!(second, Err(SourceError::Transient(_))),
        "second: {second:?}"
    );
    match source.poll() {
        Err(SourceError::Lost(_)) => {}
        other => panic!("third: expected Lost, got {other:?}"),
    }
}

#[test]
fn a_container_that_moves_is_noticed_and_found_again() {
    // World 1 puts the caption document one index earlier than world 0: the
    // remembered path now holds something else, which is Stale, and the next
    // poll finds the container again. A single hiccup is never Lost.
    let backend = FakeBackend::scripted(vec![
        shifted_world(3, "a"),
        shifted_world(2, "b"),
        shifted_world(2, "b"),
        shifted_world(2, "b"),
    ])
    .auto_advance(true);
    let mut source = teams_source(backend, Selectors::default());
    attached(&mut source);
    assert!(source.poll().is_ok(), "world 0 is as discovered");
    assert!(
        matches!(source.poll(), Err(SourceError::Transient(_))),
        "world 1 moved the container"
    );
    for _ in 0..5 {
        assert!(source.poll().is_ok(), "healthy polls must not be Lost");
    }
}

#[test]
fn a_banner_inserted_before_the_container_is_not_read_as_captions() {
    // Same path, different node: the identity check catches it as Stale.
    let backend = FakeBackend::scripted(vec![shifted_world(3, "a"), shifted_world(2, "b")]);
    let mut source = teams_source(backend, Selectors::default());
    attached(&mut source);
    source.backend_mut().advance();
    assert!(matches!(source.poll(), Err(SourceError::Transient(_))));
}

// --------------------------------------------------- (e) error mapping

struct Failing(UiError);

impl UiBackend for Failing {
    fn windows(&mut self) -> Result<Vec<WindowInfo>, UiError> {
        Err(self.0.clone())
    }

    fn snapshot(&mut self, _: &Target, _: &Budget) -> Result<UiNode, UiError> {
        Err(self.0.clone())
    }

    fn alive(&mut self, _: u64) -> bool {
        false
    }
}

#[test]
fn init_errors_are_fatal_on_discover_and_poll() {
    let mut source = UiaSource::new(
        Failing(UiError::Init("no COM".to_string())),
        Platform::Teams,
        Selectors::default(),
        NodeMode::All,
    );
    assert_eq!(
        source.discover().unwrap_err(),
        SourceError::Fatal("no COM".to_string())
    );
    assert_eq!(
        source.poll().unwrap_err(),
        SourceError::Fatal("no COM".to_string())
    );
}

#[test]
fn backend_errors_are_transient() {
    let mut source = UiaSource::new(
        Failing(UiError::Backend("boom".to_string())),
        Platform::Zoom,
        Selectors::default(),
        NodeMode::All,
    );
    assert!(matches!(source.discover(), Err(SourceError::Transient(m)) if m.contains("boom")));
    assert!(matches!(source.poll(), Err(SourceError::Transient(m)) if m.contains("boom")));
}

// ------------------------------------------------ (f) selector overrides

fn caption_tree_texts(selectors: Selectors) -> Vec<String> {
    let backend = FakeBackend::new(teams_world(&[
        ("Ana García", "Muted, but speaking"),
        ("Luis Pérez", "XYZ suena raro"),
        ("María Soto", "Hola a todos"),
    ]));
    let mut source = teams_source(backend, selectors);
    attached(&mut source);
    source
        .poll()
        .unwrap()
        .into_iter()
        .filter_map(|o| o.text)
        .collect()
}

#[test]
fn a_selectors_json_override_changes_behaviour_without_a_code_change() {
    let defaults = caption_tree_texts(Selectors::default());
    assert_eq!(defaults, ["XYZ suena raro", "Hola a todos"]);

    let overridden: Selectors = serde_json::from_str(r#"{"teams":{"noise":["XYZ"]}}"#).unwrap();
    assert_eq!(overridden.teams.noise, ["XYZ"]);
    assert_eq!(
        overridden.teams.caption_keywords,
        TeamsSel::default().caption_keywords
    );
    assert_eq!(overridden.zoom, ZoomSel::default());
    assert_eq!(overridden.version, 5);
    let texts = caption_tree_texts(overridden);
    assert_eq!(texts, ["Muted, but speaking", "Hola a todos"]);
}

#[test]
fn unknown_selector_keys_are_rejected() {
    assert!(serde_json::from_str::<Selectors>(r#"{"teams":{"nope":1}}"#).is_err());
    assert!(serde_json::from_str::<Selectors>(r#"{"zoom":{"nope":1}}"#).is_err());
    assert!(serde_json::from_str::<Selectors>(r#"{"nope":1}"#).is_err());
}

#[test]
fn selectors_default_round_trips_through_json() {
    let text = serde_json::to_string(&Selectors::default()).unwrap();
    let back: Selectors = serde_json::from_str(&text).unwrap();
    assert_eq!(back, Selectors::default());
    let empty: Selectors = serde_json::from_str("{}").unwrap();
    assert_eq!(empty, Selectors::default());
}

/// Suspected bug: titles are lowercased before matching, but the selector
/// keywords are not, so an override written in the natural case never matches.
#[test]
fn selector_keywords_match_regardless_of_case() {
    let sel = TeamsSel {
        required_title: "Microsoft Teams".to_string(),
        ..TeamsSel::default()
    };
    let windows = [WindowInfo {
        handle: 1,
        title: "Reunión con Ana | Microsoft Teams".to_string(),
        class_name: TEAMS_CLASS.to_string(),
        pid: 1,
        process: None,
    }];
    assert_eq!(teams::pick_window(&windows, &sel), Some(0));
}

/// Suspected bug: `ttl()` calls `Duration::from_secs_f64`, which panics for
/// values beyond u64 seconds, so a huge `cache_ttl_s` in the config panics the
/// capture thread on the first cached poll. Expected: the poll succeeds.
#[test]
fn a_huge_cache_ttl_does_not_panic() {
    let selectors: Selectors = serde_json::from_str(r#"{"teams":{"cache_ttl_s":1e30}}"#).unwrap();
    let mut source = teams_source(
        FakeBackend::new(teams_world(&[("Ana García", "Hola a todos")])),
        selectors,
    );
    attached(&mut source);
    assert!(source.poll().is_ok());
}

// ------------------------------------------------------ (g) node modes

#[test]
fn last_mode_keeps_only_the_newest_caption() {
    let fixture = read_fixture("teams_three_speakers");
    let mut all = UiaSource::new(
        FakeBackend::from_fixture(fixture.clone()),
        Platform::Teams,
        Selectors::default(),
        NodeMode::All,
    );
    let mut last = UiaSource::new(
        FakeBackend::from_fixture(fixture),
        Platform::Teams,
        Selectors::default(),
        NodeMode::Last,
    );
    attached(&mut all);
    attached(&mut last);
    let all = all.poll().unwrap();
    let last = last.poll().unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(last.len(), 1);
    assert_eq!(last[0], all[2], "the newest is the last in document order");
}

#[test]
fn last_mode_with_a_single_caption_keeps_it() {
    let mut source = UiaSource::new(
        FakeBackend::new(teams_world(&[("Ana García", "Hola a todos")])),
        Platform::Teams,
        Selectors::default(),
        NodeMode::Last,
    );
    attached(&mut source);
    assert_eq!(source.poll().unwrap().len(), 1);
}

// ----------------------------------------------------- (h) names

#[test]
fn source_names_and_meeting_name_before_discovery() {
    let teams = UiaSource::new(
        FakeBackend::new(teams_world(&[])),
        Platform::Teams,
        Selectors::default(),
        NodeMode::All,
    );
    let zoom = UiaSource::new(
        FakeBackend::new(World::default()),
        Platform::Zoom,
        Selectors::default(),
        NodeMode::All,
    );
    assert_eq!(teams.name(), "TeamsUia");
    assert_eq!(zoom.name(), "ZoomUia");
    assert_eq!(teams.meeting_name(), None);
    assert_eq!(zoom.meeting_name(), None);

    let mut teams = teams;
    attached(&mut teams);
    assert_eq!(teams.meeting_name().as_deref(), Some(TEAMS_TITLE));
}

// --------------------------------------------- (i) fixtures and tree model

#[test]
fn every_scenario_fixture_round_trips_through_json() {
    let mut count = 0;
    let mut entries: Vec<PathBuf> = std::fs::read_dir(fixtures_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !name.ends_with(".json") || name.ends_with(".expected.json") || name == "zoom_items.json"
        {
            continue;
        }
        let fixture: Fixture = serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(fixture.format, FIXTURE_FORMAT, "{name}");
        let text = serde_json::to_string(&fixture).unwrap();
        let back: Fixture = serde_json::from_str(&text).unwrap();
        assert_eq!(back, fixture, "{name}: round trip changed the fixture");
        count += 1;
    }
    assert!(count >= 25, "only {count} fixtures");
}

#[test]
fn roles_parse_and_serialise_unknown_names() {
    assert_eq!(Role::parse("Slider"), Role::Other("Slider".to_string()));
    assert_eq!(Role::parse("Slider").as_str(), "Slider");
    assert_eq!(
        serde_json::to_string(&Role::parse("Slider")).unwrap(),
        "\"Slider\""
    );
    let back: Role = serde_json::from_str("\"Slider\"").unwrap();
    assert_eq!(back, Role::Other("Slider".to_string()));
    assert!(
        serde_json::from_str::<Role>("\"\"").is_err(),
        "empty role names are refused"
    );
}

#[test]
fn uia_control_type_names_map_to_roles() {
    assert_eq!(Role::from_uia("GroupControl"), Role::Group);
    assert_eq!(Role::from_uia("TextControl"), Role::Text);
    assert_eq!(Role::from_uia("ListItemControl"), Role::ListItem);
    assert_eq!(Role::from_uia("Slider"), Role::Other("Slider".to_string()));
}

/// Suspected bug: `from_uia("Control")` strips to an empty name, giving
/// `Other("")`, which serialises to `""` and then fails to deserialise.
#[test]
fn a_bare_control_suffix_round_trips() {
    let role = Role::from_uia("Control");
    let text = serde_json::to_string(&role).unwrap();
    assert_eq!(serde_json::from_str::<Role>(&text).unwrap(), role);
}

#[test]
fn node_count_and_path_lookup() {
    let tree = UiNode::new(Role::Window, "root").with_children(vec![
        UiNode::new(Role::Pane, "a").with_children(vec![
            UiNode::new(Role::Text, "a1"),
            UiNode::new(Role::Group, "a2").with_children(vec![UiNode::new(Role::Edit, "a21")]),
        ]),
        UiNode::new(Role::Button, "b"),
    ]);
    assert_eq!(tree.count(), 6);
    assert_eq!(tree.at(&[]).unwrap().name, "root");
    assert_eq!(tree.at(&[0, 1, 0]).unwrap().name, "a21");
    assert_eq!(tree.at(&[1]).unwrap().name, "b");
    assert!(tree.at(&[2]).is_none());
    assert!(tree.at(&[0, 0, 0]).is_none(), "a Text node has no children");
}

// ------------------------------------------------ (j) pure window and title rules

fn window(handle: u64, title: &str, class: &str) -> WindowInfo {
    WindowInfo {
        handle,
        title: title.to_string(),
        class_name: class.to_string(),
        pid: 0,
        process: None,
    }
}

#[test]
fn teams_meeting_name_strips_decorations() {
    let sel = TeamsSel::default();
    let name = |title: &str| teams::meeting_name(title, &sel);
    assert_eq!(
        name("Subtítulos | Reunión con Ana | Ventana anclada").as_deref(),
        Some("Reunión con Ana")
    );
    assert_eq!(
        name("Captions | Sprint review | Pinned window").as_deref(),
        Some("Sprint review")
    );
    assert_eq!(name("Subtitulos | Team").as_deref(), Some("Team"));
    // Both prefixes are stripped in turn, so a doubled prefix is removed too.
    assert_eq!(name("Captions | Subtitles | Team").as_deref(), Some("Team"));
    assert_eq!(name("\t  Team \n").as_deref(), Some("Team"));
    assert_eq!(
        name("Reunión con Ana | Microsoft Teams").as_deref(),
        Some(TEAMS_TITLE)
    );
    assert_eq!(name(""), None);
    assert_eq!(name("Captions | "), None);
    assert_eq!(name("Captions |    "), None);
}

#[test]
fn teams_meeting_name_respects_custom_affixes() {
    let sel = TeamsSel {
        title_prefixes: vec!["Pop | ".to_string()],
        title_suffixes: Vec::new(),
        ..TeamsSel::default()
    };
    assert_eq!(
        teams::meeting_name("Pop | Standup", &sel).as_deref(),
        Some("Standup")
    );
    assert_eq!(
        teams::meeting_name("Captions | Standup", &sel).as_deref(),
        Some("Captions | Standup"),
        "defaults are replaced, not merged"
    );
}

#[test]
fn teams_pick_window_priority_and_order() {
    let sel = TeamsSel::default();
    let pick = |windows: &[WindowInfo]| teams::pick_window(windows, &sel);
    let fallback = window(1, "Microsoft Teams", TEAMS_CLASS);
    let meeting = window(2, "Reunión con Ana | Microsoft Teams", TEAMS_CLASS);
    let captions = window(3, "Subtítulos | Microsoft Teams", TEAMS_CLASS);

    // Caption pop-out beats a meeting, which beats a plain Teams window.
    assert_eq!(
        pick(&[fallback.clone(), meeting.clone(), captions.clone()]),
        Some(2)
    );
    assert_eq!(pick(&[captions.clone(), meeting.clone()]), Some(0));
    assert_eq!(pick(&[fallback.clone(), meeting.clone()]), Some(1));

    // Last wins among equal captions and among equal meetings.
    let captions_b = window(4, "Captions | Microsoft Teams", TEAMS_CLASS);
    assert_eq!(pick(&[captions.clone(), captions_b.clone()]), Some(1));
    let meeting_b = window(5, "Call with Luis | Microsoft Teams", TEAMS_CLASS);
    assert_eq!(pick(&[meeting.clone(), meeting_b]), Some(1));

    // The first plain window wins among fallbacks.
    let fallback_b = window(6, "Microsoft Teams (2)", TEAMS_CLASS);
    assert_eq!(pick(&[fallback.clone(), fallback_b]), Some(0));
}

#[test]
fn teams_pick_window_rejects_chat_wrong_class_and_missing_title() {
    let sel = TeamsSel::default();
    let pick = |windows: &[WindowInfo]| teams::pick_window(windows, &sel);
    assert_eq!(
        pick(&[window(1, "Chat | Microsoft Teams", TEAMS_CLASS)]),
        None
    );
    assert_eq!(
        pick(&[window(1, "Captions | Chat | Microsoft Teams", TEAMS_CLASS)]),
        None
    );
    assert_eq!(
        pick(&[window(1, "Microsoft Teams and channels", TEAMS_CLASS)]),
        None
    );

    // A rejected window does not hide an accepted one.
    let meeting = window(2, "Reunión | Microsoft Teams", TEAMS_CLASS);
    assert_eq!(
        pick(&[
            window(1, "Captions | Chat | Microsoft Teams", TEAMS_CLASS),
            meeting
        ]),
        Some(1)
    );

    assert_eq!(
        pick(&[window(
            1,
            "Captions | Microsoft Teams",
            "Chrome_WidgetWin_1"
        )]),
        None
    );
    assert_eq!(pick(&[window(1, "Captions", TEAMS_CLASS)]), None);
    assert_eq!(pick(&[window(1, "", TEAMS_CLASS)]), None);
}

#[test]
fn zoom_pick_window_prefers_the_meeting_class_over_titles() {
    let sel = ZoomSel::default();
    let pick = |windows: &[WindowInfo]| zoom::pick_window(windows, &sel);
    let home = window(1, "Zoom Meeting", "ZPPTMainFrmWndClassEx");
    let untitled = window(2, "Untitled", "ConfMultiTabContentWndClass");
    assert_eq!(pick(&[home.clone(), untitled.clone()]), Some(1));

    // Class order is the selector order, not the window order.
    let content = window(3, "Webinar", "ZPContentViewWndClass");
    assert_eq!(pick(&[content.clone(), untitled.clone()]), Some(1));
    assert_eq!(pick(&[untitled, content]), Some(0));
}

#[test]
fn zoom_pick_window_excludes_the_home_window_and_takes_the_first_title_hit() {
    let sel = ZoomSel::default();
    let pick = |windows: &[WindowInfo]| zoom::pick_window(windows, &sel);
    assert_eq!(
        pick(&[window(1, "Zoom Meeting", "ZPPTMainFrmWndClassEx")]),
        None
    );
    assert_eq!(
        pick(&[window(1, "Zoom Workplace", "ZPPTMainFrmWndClassEx")]),
        None
    );
    // Zoom 7's signed-out start window, seen on a real Windows desktop.
    assert_eq!(
        pick(&[window(
            1,
            "Zoom Workplace",
            "ZPPTEnhanceLoginMainWindowClassEx"
        )]),
        None
    );

    let first = window(2, "Webinar semanal", "OtherClassA");
    let second = window(3, "Reunión de producto", "OtherClassB");
    assert_eq!(pick(&[first.clone(), second.clone()]), Some(0));
    assert_eq!(pick(&[second, first]), Some(0));
    assert_eq!(
        pick(&[window(4, "WEBINAR", "OtherClassC")]),
        Some(0),
        "hints ignore case"
    );
}

#[test]
fn zoom_meeting_name_is_the_title_or_nothing() {
    assert_eq!(zoom::meeting_name(""), None);
    assert_eq!(
        zoom::meeting_name("Reunión de producto").as_deref(),
        Some("Reunión de producto")
    );
}

// ------------------------------------------------ (k) random trees

#[test]
fn random_trees_never_panic_and_snapshots_respect_their_budget() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let teams_sel = TeamsSel::default();
    let zoom_sel = ZoomSel::default();
    let mut deepest = 0;

    for case in 0..400 {
        // One chain in three is a pure chain (always one child), so depth 60 is reached.
        let chain = match case % 3 {
            0 => 100,
            1 => 90,
            _ => 35,
        };
        let mut budget = 2_000;
        let tree = random_tree(&mut rng, 0, chain, &mut budget);
        deepest = deepest.max(depth_of(&tree));

        let _ = teams::captions(&tree, &teams_sel);
        let _ = zoom::captions(&tree);
        let _ = teams::find_container(&tree, &teams_sel);
        let _ = zoom::find_container(&tree, &zoom_sel);
        let _ = teams::find_first(&tree, 60, &|n: &UiNode| n.role == Role::Text);
        for_each(&tree, &mut |node| {
            let _ = zoom::parse_item(node);
            let _ = zoom::captions(node);
            let _ = teams::captions(node, &teams_sel);
            let _ = teams::meeting_name(&node.name, &teams_sel);
        });

        let max_depth = rng.below(61) as usize;
        let max_nodes = 1 + rng.below(300) as usize;
        let windows = vec![FakeWindow {
            info: window(1, TEAMS_TITLE, TEAMS_CLASS),
            tree: tree.clone(),
        }];
        let mut backend = FakeBackend::new(world(windows));
        let budget = Budget::new(max_depth, max_nodes);
        match backend.snapshot(
            &Target {
                window: 1,
                path: Vec::new(),
                expect: None,
            },
            &budget,
        ) {
            Ok(copy) => {
                assert!(
                    copy.count() <= max_nodes,
                    "case {case}: {} > {max_nodes}",
                    copy.count()
                );
                assert!(
                    depth_of(&copy) <= max_depth,
                    "case {case}: depth beyond {max_depth}"
                );
                if depth_of(&tree) <= max_depth && tree.count() <= max_nodes {
                    assert_eq!(copy, tree, "case {case}: a fitting tree is copied whole");
                }
            }
            Err(UiError::Budget) => {
                assert!(
                    tree.count() > max_nodes,
                    "case {case}: budget hit on a small tree"
                );
            }
            Err(other) => panic!("case {case}: unexpected {other:?}"),
        }
    }
    assert!(deepest >= 50, "random trees reached only depth {deepest}");
}

#[test]
fn random_worlds_never_panic_through_the_source() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    for case in 0..150 {
        let mut budget = 1_500;
        let tree = random_tree(&mut rng, 0, 50, &mut budget);
        let title = if case % 3 == 0 { "" } else { TEAMS_TITLE };
        let class = if case % 4 == 0 {
            "ConfMultiTabContentWndClass"
        } else {
            TEAMS_CLASS
        };
        let windows = vec![FakeWindow {
            info: window(1, title, class),
            tree,
        }];
        let platform = if case % 2 == 0 {
            Platform::Teams
        } else {
            Platform::Zoom
        };
        let mut source = UiaSource::new(
            FakeBackend::new(world(windows)).auto_advance(case % 5 == 0),
            platform,
            Selectors::default(),
            if case % 2 == 0 {
                NodeMode::All
            } else {
                NodeMode::Last
            },
        );
        for _ in 0..3 {
            let _ = source.discover();
            let _ = source.poll();
            let _ = source.meeting_name();
        }
    }
}
