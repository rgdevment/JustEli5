//! Pure UIA tooling: the HRESULT and ControlType tables, the child-index
//! mirror, selector validation, the Teams window tiers, and the anonymiser
//! checked against every fixture in `fixtures/uia`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use mc_captions::ui::anon::{anonymise, pseudonyms};
use mc_captions::ui::backend::UiError;
use mc_captions::ui::fake::Fixture;
use mc_captions::ui::mirror::Mirror;
use mc_captions::ui::node::{Role, UiNode, WindowInfo};
use mc_captions::ui::selectors::Selectors;
use mc_captions::ui::teams::{self, Tier, pick_window_tier};
use mc_captions::ui::uia_map::{control_type_name, map_hresult, role_for};
use mc_captions::ui::zoom;

// --- helpers --------------------------------------------------------------------

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/uia")
}

/// Every scenario fixture: not the expectations, not the item vectors, not the
/// dumps (those live in a subdirectory and are read by `uia_dumps.rs`).
fn scenario_fixtures() -> Vec<(String, Fixture)> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(fixtures_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            name.ends_with(".json")
                && !name.ends_with(".expected.json")
                && name != "zoom_items.json"
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let name = p.file_stem().unwrap().to_string_lossy().to_string();
            let text = std::fs::read_to_string(&p).unwrap();
            let fixture = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
            (name, fixture)
        })
        .collect()
}

fn info(handle: u64, title: &str, class_name: &str) -> WindowInfo {
    WindowInfo {
        handle,
        title: title.to_string(),
        class_name: class_name.to_string(),
        pid: 1,
        process: None,
    }
}

fn infos(fixture: &Fixture) -> Vec<WindowInfo> {
    fixture.windows.iter().map(|w| w.info.clone()).collect()
}

/// The spelling of the nth speaker pseudonym, as `anon.rs` assigns them.
fn expected_pseudonym(index: usize) -> String {
    let letter = |i: usize| char::from(b'A' + u8::try_from(i).unwrap());
    if index < 26 {
        format!("Speaker {}", letter(index))
    } else {
        format!("Speaker {}{}", letter(index / 26 - 1), letter(index % 26))
    }
}

/// Every string a reader could show: names, values and the window title. The
/// automation id, class and process are structural and kept verbatim.
fn shown_strings(node: &UiNode, out: &mut Vec<String>) {
    out.push(node.name.clone());
    if let Some(value) = &node.value {
        out.push(value.clone());
    }
    for child in &node.children {
        shown_strings(child, out);
    }
}

/// Lowercased alphabetic words of at least three letters.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphabetic())
        .filter(|w| w.chars().count() >= 3)
        .map(str::to_lowercase)
        .collect()
}

/// Words the anonymiser is allowed to keep: those of the protected strings,
/// `Speaker` and the Zoom name. Mask-only words (`xxx`, `Xxx`) are never leaks.
fn allowed_words(sel: &Selectors) -> HashSet<String> {
    let t = &sel.teams;
    let z = &sel.zoom;
    let mut set: HashSet<String> = HashSet::new();
    let protected = t
        .caption_keywords
        .iter()
        .chain(&t.meeting_keywords)
        .chain(&t.reject_keywords)
        .chain(&t.noise)
        .chain(&t.title_prefixes)
        .chain(&t.title_suffixes)
        .chain(&z.title_hints)
        .chain(&z.list_name_hints)
        .chain(&sel.browser.names)
        .chain([&t.required_title]);
    for phrase in protected {
        set.extend(words(phrase));
    }
    for extra in [
        "zoom",
        "speaker",
        "unknown",
        "user",
        "usuario",
        "desconocido",
    ] {
        set.insert(extra.to_string());
    }
    set
}

fn is_mask_only(word: &str) -> bool {
    word.chars().all(|c| c == 'x')
}

// --- hresult table -----------------------------------------------------------------

#[test]
fn stale_hresults_mean_stale_inside_a_live_window_and_gone_otherwise() {
    // UIA_E_ELEMENTNOTAVAILABLE, RPC_E_DISCONNECTED, CO_E_OBJNOTCONNECTED,
    // RPC_E_SERVER_DIED, RPC_E_SERVER_DIED_DNE, RPC_S_SERVER_UNAVAILABLE
    let codes = [
        0x8004_0201u32,
        0x8001_0108,
        0x8004_01FD,
        0x8001_0007,
        0x8001_0012,
        0x8007_06BA,
    ];
    for code in codes {
        assert_eq!(
            map_hresult(code, true),
            UiError::Stale,
            "{code:#010X} alive"
        );
        assert_eq!(
            map_hresult(code, false),
            UiError::WindowGone,
            "{code:#010X} dead"
        );
    }
}

#[test]
fn busy_hresults_are_a_budget_miss_whatever_the_window_state() {
    // UIA_E_TIMEOUT, RPC_E_CALL_REJECTED, RPC_E_CALL_CANCELED
    for code in [0x8013_1505u32, 0x8001_0001, 0x8001_0002] {
        assert_eq!(
            map_hresult(code, true),
            UiError::Budget,
            "{code:#010X} alive"
        );
        assert_eq!(
            map_hresult(code, false),
            UiError::Budget,
            "{code:#010X} dead"
        );
    }
}

#[test]
fn access_denied_says_to_match_the_elevation_of_the_apps() {
    for alive in [true, false] {
        match map_hresult(0x8007_0005, alive) {
            UiError::Backend(message) => {
                assert!(message.contains("access denied"), "{message}");
                assert!(message.contains("administrator"), "{message}");
            }
            other => panic!("E_ACCESSDENIED alive={alive}: {other:?}"),
        }
    }
}

#[test]
fn unknown_hresults_are_backend_errors_with_the_code_in_hex() {
    assert_eq!(
        map_hresult(0x8000_4005, true),
        UiError::Backend("HRESULT 0x80004005".to_string())
    );
    assert_eq!(
        map_hresult(0x8000_4005, false),
        UiError::Backend("HRESULT 0x80004005".to_string())
    );
    assert_eq!(
        map_hresult(0, true),
        UiError::Backend("HRESULT 0x00000000".to_string())
    );
    assert_eq!(
        map_hresult(u32::MAX, false),
        UiError::Backend("HRESULT 0xFFFFFFFF".to_string())
    );
}

// --- control types ---------------------------------------------------------------------

/// The UIA ControlType ids 50000..=50040, in order, as the Python package names them.
const CONTROL_TYPES: [&str; 41] = [
    "Button",
    "Calendar",
    "CheckBox",
    "ComboBox",
    "Edit",
    "Hyperlink",
    "Image",
    "ListItem",
    "List",
    "Menu",
    "MenuBar",
    "MenuItem",
    "ProgressBar",
    "RadioButton",
    "ScrollBar",
    "Slider",
    "Spinner",
    "StatusBar",
    "Tab",
    "TabItem",
    "Text",
    "ToolBar",
    "ToolTip",
    "Tree",
    "TreeItem",
    "Custom",
    "Group",
    "Thumb",
    "DataGrid",
    "DataItem",
    "Document",
    "SplitButton",
    "Window",
    "Pane",
    "Header",
    "HeaderItem",
    "Table",
    "TitleBar",
    "Separator",
    "SemanticZoom",
    "AppBar",
];

#[test]
fn all_41_control_type_ids_have_names() {
    for (offset, short) in CONTROL_TYPES.iter().enumerate() {
        let id = 50000 + i32::try_from(offset).unwrap();
        assert_eq!(
            control_type_name(id),
            Some(format!("{short}Control").as_str()),
            "id {id}"
        );
    }
}

#[test]
fn control_type_spot_checks() {
    assert_eq!(control_type_name(50026), Some("GroupControl"));
    assert_eq!(control_type_name(50020), Some("TextControl"));
    assert_eq!(control_type_name(50030), Some("DocumentControl"));
    assert_eq!(control_type_name(50032), Some("WindowControl"));
    assert_eq!(control_type_name(50033), Some("PaneControl"));
    assert_eq!(control_type_name(50008), Some("ListControl"));
    assert_eq!(control_type_name(50007), Some("ListItemControl"));
    assert_eq!(control_type_name(50004), Some("EditControl"));
}

#[test]
fn ids_outside_the_table_have_no_name() {
    for id in [49999, 50041, i32::MIN, i32::MAX, 0, -1] {
        assert_eq!(control_type_name(id), None, "id {id}");
    }
}

#[test]
fn every_control_type_maps_to_its_name_and_the_named_roles_are_not_other() {
    for (offset, short) in CONTROL_TYPES.iter().enumerate() {
        let id = 50000 + i32::try_from(offset).unwrap();
        // Role has variants for the roles the parsers read; the rest keep their short name.
        let role = role_for(id);
        assert_eq!(role, Role::from_uia(&format!("{short}Control")), "id {id}");
        if matches!(
            *short,
            "Window"
                | "Pane"
                | "Document"
                | "Group"
                | "Text"
                | "List"
                | "ListItem"
                | "Edit"
                | "Button"
        ) {
            assert!(
                !matches!(role, Role::Other(_)),
                "id {id} ({short}) -> {role:?}"
            );
        } else {
            assert_eq!(role, Role::Other((*short).to_string()), "id {id}");
        }
    }
    assert_eq!(role_for(50026), Role::Group);
    assert_eq!(role_for(50032), Role::Window);
    assert_eq!(role_for(50033), Role::Pane);
    assert_eq!(role_for(50030), Role::Document);
    assert_eq!(role_for(50008), Role::List);
    assert_eq!(role_for(50007), Role::ListItem);
    assert_eq!(role_for(50004), Role::Edit);
    assert_eq!(role_for(50020), Role::Text);
    assert_eq!(role_for(50000), Role::Button);
    // Custom has no fixture role of its own; it keeps its name.
    assert_eq!(role_for(50025), Role::Other("Custom".to_string()));
}

#[test]
fn unknown_control_types_keep_their_number_and_round_trip_through_serde() {
    for id in [49999, 50041, i32::MAX] {
        let role = role_for(id);
        assert_eq!(role, Role::Other(format!("ControlType{id}")));
        let json = serde_json::to_string(&role).unwrap();
        assert_eq!(json, format!("\"ControlType{id}\""));
        let back: Role = serde_json::from_str(&json).unwrap();
        assert_eq!(back, role, "id {id}");
    }
}

// --- mirror -------------------------------------------------------------------------------

fn sample_mirror() -> Mirror<&'static str> {
    Mirror {
        elem: "root",
        children: vec![
            Mirror {
                elem: "a",
                children: vec![Mirror::leaf("a0"), Mirror::leaf("a1")],
            },
            Mirror::leaf("b"),
        ],
    }
}

#[test]
fn mirror_paths_find_their_nodes() {
    let tree = sample_mirror();
    assert_eq!(tree.at(&[]).map(|m| m.elem), Some("root"));
    assert_eq!(tree.at(&[0]).map(|m| m.elem), Some("a"));
    assert_eq!(tree.at(&[0, 1]).map(|m| m.elem), Some("a1"));
    assert_eq!(tree.at(&[1]).map(|m| m.elem), Some("b"));
    assert!(Mirror::leaf(7).at(&[]).is_some());
}

#[test]
fn mirror_out_of_range_and_past_a_leaf_are_none() {
    let tree = sample_mirror();
    assert!(tree.at(&[2]).is_none());
    assert!(tree.at(&[0, 2]).is_none());
    assert!(tree.at(&[1, 0]).is_none(), "a leaf has no children");
    assert!(tree.at(&[usize::MAX]).is_none());
    assert!(tree.at(&[0, usize::MAX, 0]).is_none());
    assert!(Mirror::leaf(7).at(&[0]).is_none());
}

#[test]
fn mirror_follows_a_deep_chain_exactly_to_its_end() {
    let mut chain = Mirror::leaf(0usize);
    for depth in 1..=60 {
        chain = Mirror {
            elem: depth,
            children: vec![chain],
        };
    }
    let path = vec![0usize; 60];
    assert_eq!(chain.at(&path).map(|m| m.elem), Some(0));
    assert_eq!(chain.at(&path).map(|m| m.children.len()), Some(0));
    let too_deep = vec![0usize; 61];
    assert!(chain.at(&too_deep).is_none());
}

// --- selectors ----------------------------------------------------------------------------

#[test]
fn default_selectors_are_valid_version_five() {
    let sel = Selectors::default();
    assert_eq!(sel.version, 5);
    sel.validate().expect("defaults validate");
}

#[test]
fn every_selector_list_rejects_an_empty_keyword() {
    type Field = fn(&mut Selectors) -> &mut Vec<String>;
    let lists: [(&str, Field); 23] = [
        ("teams.classes", |s| &mut s.teams.classes),
        ("teams.caption_keywords", |s| &mut s.teams.caption_keywords),
        ("teams.meeting_keywords", |s| &mut s.teams.meeting_keywords),
        ("teams.reject_keywords", |s| &mut s.teams.reject_keywords),
        ("teams.noise", |s| &mut s.teams.noise),
        ("teams.title_prefixes", |s| &mut s.teams.title_prefixes),
        ("teams.title_suffixes", |s| &mut s.teams.title_suffixes),
        ("zoom.meeting_classes", |s| &mut s.zoom.meeting_classes),
        ("zoom.title_hints", |s| &mut s.zoom.title_hints),
        ("zoom.list_name_hints", |s| &mut s.zoom.list_name_hints),
        ("browser.classes", |s| &mut s.browser.classes),
        ("browser.names", |s| &mut s.browser.names),
        ("browser.processes", |s| &mut s.browser.processes),
        ("meet.title_prefixes", |s| &mut s.meet.title_prefixes),
        ("meet.region_names", |s| &mut s.meet.region_names),
        ("meet.noise", |s| &mut s.meet.noise),
        ("teams_web.title_hints", |s| &mut s.teams_web.title_hints),
        ("slack.app_processes", |s| &mut s.slack.app_processes),
        ("slack.app_classes", |s| &mut s.slack.app_classes),
        ("slack.title_hints", |s| &mut s.slack.title_hints),
        ("slack.huddle_hints", |s| &mut s.slack.huddle_hints),
        ("slack.region_names", |s| &mut s.slack.region_names),
        ("slack.noise", |s| &mut s.slack.noise),
    ];
    for (name, field) in lists {
        let mut sel = Selectors::default();
        field(&mut sel).push(String::new());
        let err = sel.validate().expect_err(name);
        assert!(err.contains(name), "{name}: {err}");
        assert!(err.contains("empty string"), "{name}: {err}");
    }
}

#[test]
fn an_empty_required_title_is_rejected() {
    let mut sel = Selectors::default();
    sel.teams.required_title.clear();
    let err = sel.validate().unwrap_err();
    assert!(err.contains("required_title"), "{err}");
}

#[test]
fn empty_depth_lists_are_rejected() {
    let mut sel = Selectors::default();
    sel.teams.root_depths.clear();
    assert!(sel.validate().unwrap_err().contains("depth lists"));
    let mut sel = Selectors::default();
    sel.teams.walk_depths.clear();
    assert!(sel.validate().unwrap_err().contains("depth lists"));
}

#[test]
fn cache_ttl_must_be_a_finite_number_of_at_least_zero() {
    for ttl in [-0.5, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut sel = Selectors::default();
        sel.teams.cache_ttl_s = ttl;
        let err = sel.validate().expect_err("teams ttl");
        assert!(err.contains("teams.cache_ttl_s"), "{ttl}: {err}");

        let mut sel = Selectors::default();
        sel.zoom.cache_ttl_s = ttl;
        let err = sel.validate().expect_err("zoom ttl");
        assert!(err.contains("zoom.cache_ttl_s"), "{ttl}: {err}");
    }
    let mut sel = Selectors::default();
    sel.teams.cache_ttl_s = 0.0;
    sel.zoom.cache_ttl_s = 0.0;
    sel.validate().expect("zero is allowed");
}

// --- window tiers ---------------------------------------------------------------------

const TEAMS: &str = "TeamsWebView";

#[test]
fn no_windows_no_teams_window() {
    let sel = Selectors::default().teams;
    assert_eq!(pick_window_tier(&[], &sel), None);
}

#[test]
fn only_teams_class_windows_with_the_teams_title_qualify() {
    let sel = Selectors::default().teams;
    let wrong_class = [info(1, "Reunión | Microsoft Teams", "Chrome_WidgetWin_1")];
    assert_eq!(pick_window_tier(&wrong_class, &sel), None);
    let no_title = [info(1, "", TEAMS)];
    assert_eq!(pick_window_tier(&no_title, &sel), None);
    let other_app = [info(1, "Notas - Bloc de notas", TEAMS)];
    assert_eq!(pick_window_tier(&other_app, &sel), None);
}

#[test]
fn chat_and_channel_windows_are_never_picked() {
    let sel = Selectors::default().teams;
    assert_eq!(
        pick_window_tier(&[info(1, "Chat | Microsoft Teams", TEAMS)], &sel),
        None
    );
    assert_eq!(
        pick_window_tier(
            &[info(1, "Teams and channels | Microsoft Teams", TEAMS)],
            &sel
        ),
        None
    );
    // Reject beats a caption keyword: a chat about captions is still chat.
    assert_eq!(
        pick_window_tier(&[info(1, "Chat captions | Microsoft Teams", TEAMS)], &sel),
        None
    );
}

#[test]
fn tiers_fallback_meeting_and_caption_popout() {
    let sel = Selectors::default().teams;
    let fallback = [info(1, "Microsoft Teams", TEAMS)];
    assert_eq!(pick_window_tier(&fallback, &sel), Some((0, Tier::Fallback)));
    let meeting = [info(1, "Reunión con Ana | Microsoft Teams", TEAMS)];
    assert_eq!(pick_window_tier(&meeting, &sel), Some((0, Tier::Meeting)));
    let popout = [info(1, "Subtítulos | Reunión X | Microsoft Teams", TEAMS)];
    assert_eq!(
        pick_window_tier(&popout, &sel),
        Some((0, Tier::CaptionPopout))
    );
}

#[test]
fn the_caption_popout_beats_a_meeting_which_beats_the_fallback() {
    let sel = Selectors::default().teams;
    let windows = [
        info(1, "Microsoft Teams", TEAMS),
        info(2, "Reunión con Ana | Microsoft Teams", TEAMS),
        info(3, "Captions | Weekly sync | Microsoft Teams", TEAMS),
    ];
    assert_eq!(
        pick_window_tier(&windows, &sel),
        Some((2, Tier::CaptionPopout))
    );
    let without_popout = &windows[..2];
    assert_eq!(
        pick_window_tier(without_popout, &sel),
        Some((1, Tier::Meeting))
    );
}

#[test]
fn among_equals_the_last_popout_or_meeting_and_the_first_fallback_win() {
    let sel = Selectors::default().teams;
    let popouts = [
        info(1, "Subtítulos | A | Microsoft Teams", TEAMS),
        info(2, "Captions | B | Microsoft Teams", TEAMS),
    ];
    assert_eq!(
        pick_window_tier(&popouts, &sel),
        Some((1, Tier::CaptionPopout))
    );
    let meetings = [
        info(1, "Reunión A | Microsoft Teams", TEAMS),
        info(2, "Reunión B | Microsoft Teams", TEAMS),
    ];
    assert_eq!(pick_window_tier(&meetings, &sel), Some((1, Tier::Meeting)));
    let fallbacks = [
        info(1, "Microsoft Teams", TEAMS),
        info(2, "Microsoft Teams - Ventana", TEAMS),
    ];
    assert_eq!(
        pick_window_tier(&fallbacks, &sel),
        Some((0, Tier::Fallback))
    );
}

#[test]
fn titles_match_regardless_of_case_and_selector_keyword_case() {
    let sel = Selectors::default().teams;
    assert_eq!(
        pick_window_tier(&[info(1, "SUBTÍTULOS | X | MICROSOFT TEAMS", TEAMS)], &sel),
        Some((0, Tier::CaptionPopout))
    );
    // A keyword written in capitals in the config still matches.
    let mut loud = Selectors::default().teams;
    loud.caption_keywords = vec!["CAPTIONS".to_string()];
    assert_eq!(
        pick_window_tier(
            &[info(1, "captions | Weekly | Microsoft Teams", TEAMS)],
            &loud
        ),
        Some((0, Tier::CaptionPopout))
    );
}

#[test]
fn a_teams_window_is_found_in_a_window_list_of_any_size() {
    let sel = Selectors::default().teams;
    let mut windows: Vec<WindowInfo> = (0..200)
        .map(|i| info(i, "Notas - Bloc de notas", "Notepad"))
        .collect();
    windows.push(info(999, "Reunión | Microsoft Teams", TEAMS));
    assert_eq!(pick_window_tier(&windows, &sel), Some((200, Tier::Meeting)));
}

// --- anonymiser: structure, speakers and leaks over every fixture ---------------------

#[test]
fn anonymised_fixtures_keep_picks_containers_counts_roles_and_shape() {
    let sel = Selectors::default();
    let mut problems = Vec::new();
    for (name, fixture) in scenario_fixtures() {
        let anon = anonymise(&fixture, &sel);
        let before = infos(&fixture);
        let after = infos(&anon);
        if teams::pick_window_tier(&before, &sel.teams)
            != teams::pick_window_tier(&after, &sel.teams)
        {
            problems.push(format!("{name}: teams pick/tier changed"));
        }
        if zoom::pick_window(&before, &sel.zoom) != zoom::pick_window(&after, &sel.zoom) {
            problems.push(format!("{name}: zoom pick changed"));
        }
        if anon.windows.len() != fixture.windows.len() {
            problems.push(format!("{name}: window count changed"));
            continue;
        }
        for (index, (orig, masked)) in fixture.windows.iter().zip(&anon.windows).enumerate() {
            let t_path = teams::find_container(&orig.tree, &sel.teams);
            if t_path != teams::find_container(&masked.tree, &sel.teams) {
                problems.push(format!("{name} window {index}: teams container changed"));
            }
            let z_path = zoom::find_container(&orig.tree, &sel.zoom);
            if z_path != zoom::find_container(&masked.tree, &sel.zoom) {
                problems.push(format!("{name} window {index}: zoom container changed"));
            }
            let t_orig = orig
                .tree
                .at(&t_path)
                .map_or(0, |n| teams::captions(n, &sel.teams).len());
            let t_anon = masked
                .tree
                .at(&t_path)
                .map_or(0, |n| teams::captions(n, &sel.teams).len());
            if t_orig != t_anon {
                problems.push(format!(
                    "{name} window {index}: teams caption count {t_orig} -> {t_anon}"
                ));
            }
            let z_orig = z_path
                .as_ref()
                .and_then(|p| orig.tree.at(p))
                .map(zoom::captions);
            let z_anon = z_path
                .as_ref()
                .and_then(|p| masked.tree.at(p))
                .map(zoom::captions);
            if z_orig.as_ref().map(Vec::len) != z_anon.as_ref().map(Vec::len) {
                problems.push(format!("{name} window {index}: zoom caption count changed"));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Walks two trees in step: same roles, same child counts, and every string
/// keeps its length unless it mentions a speaker (those become pseudonyms).
fn compare_shape(
    orig: &UiNode,
    masked: &UiNode,
    speakers: &[String],
    at: &str,
    problems: &mut Vec<String>,
) {
    if orig.role != masked.role {
        problems.push(format!("{at}: role {:?} -> {:?}", orig.role, masked.role));
    }
    if orig.children.len() != masked.children.len() {
        problems.push(format!(
            "{at}: child count {} -> {}",
            orig.children.len(),
            masked.children.len()
        ));
        return;
    }
    let (a, b) = (&orig.name, &masked.name);
    let mentions_speaker = speakers.iter().any(|s| a.contains(s.as_str()));
    if !mentions_speaker && a.chars().count() != b.chars().count() {
        problems.push(format!(
            "{at}: name length {} -> {}",
            a.chars().count(),
            b.chars().count()
        ));
    }
    match (&orig.value, &masked.value) {
        (Some(a), Some(b)) if !speakers.iter().any(|s| a.contains(s.as_str())) => {
            if a.chars().count() != b.chars().count() {
                problems.push(format!("{at}: value length changed"));
            }
        }
        (None, None) | (Some(_), Some(_)) => {}
        _ => problems.push(format!("{at}: value presence changed")),
    }
    for (index, (a, b)) in orig.children.iter().zip(&masked.children).enumerate() {
        compare_shape(a, b, speakers, &format!("{at}/{index}"), problems);
    }
}

#[test]
fn anonymised_trees_have_the_same_roles_child_counts_and_string_lengths() {
    let sel = Selectors::default();
    let mut problems = Vec::new();
    for (name, fixture) in scenario_fixtures() {
        let anon = anonymise(&fixture, &sel);
        let speakers: Vec<String> = pseudonyms(&fixture, &sel)
            .into_keys()
            .filter(|s| !s.is_empty())
            .collect();
        for (index, (orig, masked)) in fixture.windows.iter().zip(&anon.windows).enumerate() {
            let mut local = Vec::new();
            compare_shape(
                &orig.tree,
                &masked.tree,
                &speakers,
                &format!("{name}[{index}]"),
                &mut local,
            );
            problems.extend(local);
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn speaker_pseudonyms_are_a_bijection_onto_speaker_a_b_c() {
    let sel = Selectors::default();
    let mut problems = Vec::new();
    for (name, fixture) in scenario_fixtures() {
        let map = pseudonyms(&fixture, &sel);
        let mut values: Vec<String> = map.values().cloned().collect();
        values.sort();
        let mut expected: Vec<String> = (0..map.len()).map(expected_pseudonym).collect();
        expected.sort();
        if values != expected {
            problems.push(format!("{name}: pseudonyms {values:?} != {expected:?}"));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// SUSPECTED BUG (anon.rs `speaker_names`): speakers are collected from
/// `teams::captions(&window.tree)`, i.e. from the window root with the shallow
/// walk depths, not from the caption container `find_container` returns. Captions
/// that sit deeper (teams_captions_depth_25, teams_rootwebarea_at_20_groups_at_25)
/// are never mapped, so their speakers are masked ("Xxx Xxxxxx") instead of
/// becoming "Speaker A". Run with `--ignored` to see it fail.
#[test]
fn every_caption_speaker_becomes_the_pseudonym_of_the_original_speaker() {
    let sel = Selectors::default();
    let mut problems = Vec::new();
    for (name, fixture) in scenario_fixtures() {
        let anon = anonymise(&fixture, &sel);
        let map = pseudonyms(&fixture, &sel);
        for (index, (orig, masked)) in fixture.windows.iter().zip(&anon.windows).enumerate() {
            let path = teams::find_container(&orig.tree, &sel.teams);
            let (Some(a), Some(b)) = (orig.tree.at(&path), masked.tree.at(&path)) else {
                continue;
            };
            let before = teams::captions(a, &sel.teams);
            let after = teams::captions(b, &sel.teams);
            for (caption, (x, y)) in before.iter().zip(&after).enumerate() {
                let original = x.speaker.clone().unwrap_or_default();
                let got = y.speaker.clone().unwrap_or_default();
                let want = if original.is_empty() {
                    String::new()
                } else {
                    map.get(&original)
                        .cloned()
                        .unwrap_or_else(|| format!("<unmapped {original}>"))
                };
                if got != want {
                    problems.push(format!("{name}[{index}] caption {caption}: speaker {original:?} -> {got:?}, want {want:?}"));
                }
            }
        }
        for (index, (orig, masked)) in fixture.windows.iter().zip(&anon.windows).enumerate() {
            let Some(path) = zoom::find_container(&orig.tree, &sel.zoom) else {
                continue;
            };
            let (Some(a), Some(b)) = (orig.tree.at(&path), masked.tree.at(&path)) else {
                continue;
            };
            for (caption, (x, y)) in zoom::captions(a)
                .iter()
                .zip(zoom::captions(b).iter())
                .enumerate()
            {
                let (Some(original), Some(got)) = (&x.speaker, &y.speaker) else {
                    if x.speaker.is_some() != y.speaker.is_some() {
                        problems.push(format!(
                            "{name}[{index}] zoom item {caption}: speaker presence changed"
                        ));
                    }
                    continue;
                };
                if map.get(original) != Some(got) {
                    problems.push(format!(
                        "{name}[{index}] zoom item {caption}: {original:?} -> {got:?}"
                    ));
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn no_original_word_of_three_letters_survives_unless_protected_or_a_pseudonym() {
    let sel = Selectors::default();
    let allowed = allowed_words(&sel);
    let mut problems = Vec::new();
    for (name, fixture) in scenario_fixtures() {
        let anon = anonymise(&fixture, &sel);
        for (index, window) in anon.windows.iter().enumerate() {
            let mut texts = vec![window.info.title.clone()];
            shown_strings(&window.tree, &mut texts);
            let mut leaked: Vec<String> = texts
                .iter()
                .flat_map(|t| words(t))
                .filter(|w| !allowed.contains(w) && !is_mask_only(w))
                .collect();
            leaked.sort();
            leaked.dedup();
            if !leaked.is_empty() {
                problems.push(format!("{name}[{index}]: {leaked:?}"));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn anonymising_twice_changes_nothing() {
    let sel = Selectors::default();
    for (name, fixture) in scenario_fixtures() {
        let once = anonymise(&fixture, &sel);
        let twice = anonymise(&once, &sel);
        assert_eq!(twice, once, "{name}: second pass changed the fixture");
    }
}

#[test]
fn anonymised_fixtures_round_trip_through_serde() {
    let sel = Selectors::default();
    for (name, fixture) in scenario_fixtures() {
        let anon = anonymise(&fixture, &sel);
        let compact = serde_json::to_string(&anon).unwrap();
        let back: Fixture = serde_json::from_str(&compact).unwrap();
        assert_eq!(back, anon, "{name}: compact round trip");
        let pretty = serde_json::to_string_pretty(&anon).unwrap();
        let back: Fixture = serde_json::from_str(&pretty).unwrap();
        assert_eq!(back, anon, "{name}: pretty round trip");
    }
}

#[test]
fn anonymised_handles_are_one_to_n_and_pids_count_from_4000() {
    let sel = Selectors::default();
    for (name, fixture) in scenario_fixtures() {
        let anon = anonymise(&fixture, &sel);
        assert!(anon.anonymized, "{name}");
        for (index, window) in anon.windows.iter().enumerate() {
            assert_eq!(
                window.info.handle,
                index as u64 + 1,
                "{name}[{index}] handle"
            );
            assert_eq!(window.info.pid, 4000 + index as u32, "{name}[{index}] pid");
            assert_eq!(window.tree.role, Role::Window, "{name}[{index}] root");
        }
    }
}

#[test]
fn a_single_speaker_fixture_maps_to_speaker_a_and_keeps_its_structure() {
    let sel = Selectors::default();
    let (_, fixture) = scenario_fixtures()
        .into_iter()
        .find(|(name, _)| name == "teams_one_speaker")
        .unwrap();
    let anon = anonymise(&fixture, &sel);
    let captions = teams::captions(
        anon.windows[0]
            .tree
            .at(&teams::find_container(&anon.windows[0].tree, &sel.teams))
            .unwrap(),
        &sel.teams,
    );
    assert_eq!(captions.len(), 1);
    assert_eq!(captions[0].speaker.as_deref(), Some("Speaker A"));
    assert_eq!(
        captions[0].text.as_deref().map(|t| t.chars().count()),
        Some(
            teams::captions(
                fixture.windows[0]
                    .tree
                    .at(&teams::find_container(&fixture.windows[0].tree, &sel.teams))
                    .unwrap(),
                &sel.teams
            )[0]
            .text
            .as_deref()
            .unwrap()
            .chars()
            .count()
        )
    );
}

// --- anonymiser probes with synthetic meetings ---------------------------------------

/// A Teams meeting whose captions sit shallowly under a RootWebArea.
fn synthetic_teams(groups: &[(&str, &str)]) -> Fixture {
    let caption = |speaker: &str, text: &str| {
        UiNode::new(Role::Group, "").with_children(vec![
            UiNode::new(Role::Text, speaker),
            UiNode::new(Role::Text, text),
        ])
    };
    let document = UiNode::new(Role::Document, "")
        .with_aid("RootWebArea")
        .with_children(groups.iter().map(|(s, t)| caption(s, t)).collect());
    let tree = UiNode::new(Role::Window, "").with_children(vec![
        UiNode::new(Role::Pane, "").with_children(vec![document]),
    ]);
    Fixture {
        format: "mc-uia-tree".to_string(),
        version: 1,
        platform: "teams".to_string(),
        app: "Microsoft Teams".to_string(),
        anonymized: false,
        note: String::new(),
        windows: vec![mc_captions::ui::fake::FakeWindow {
            info: info(1, "Reunión | Microsoft Teams", TEAMS),
            tree,
        }],
        script: Vec::new(),
    }
}

fn anon_speakers(fixture: &Fixture) -> Vec<String> {
    let sel = Selectors::default();
    let anon = anonymise(fixture, &sel);
    let tree = &anon.windows[0].tree;
    let path = teams::find_container(tree, &sel.teams);
    teams::captions(tree.at(&path).unwrap(), &sel.teams)
        .into_iter()
        .map(|c| c.speaker.unwrap_or_default())
        .collect()
}

#[test]
fn distinct_speakers_stay_distinct_and_keep_their_pseudonyms() {
    let fixture = synthetic_teams(&[("Ana Soto", "hola"), ("Eva Mora", "buenos días")]);
    assert_eq!(anon_speakers(&fixture), vec!["Speaker A", "Speaker B"]);
}

/// SUSPECTED BUG (anon.rs `Anonymiser::text`): speaker names shorter than two
/// characters are skipped by the replacement, so the speaker field is masked
/// letter by letter: "A" and "B" both become "X" and two people merge into one.
#[test]
fn one_letter_speakers_stay_distinct() {
    let fixture = synthetic_teams(&[("A", "hola a todos"), ("B", "buenos días")]);
    assert_eq!(anon_speakers(&fixture), vec!["Speaker A", "Speaker B"]);
}

/// SUSPECTED BUG (anon.rs `Anonymiser::text`): names are replaced in order of
/// first appearance, so "Ana" (seen first) is replaced inside "Ana García" too,
/// leaving "Speaker A Xxxxxx" where the pseudonym "Speaker B" belongs.
#[test]
fn a_speaker_whose_name_contains_another_speaker_gets_its_own_pseudonym() {
    let fixture = synthetic_teams(&[("Ana", "hola"), ("Ana García", "buenos días")]);
    assert_eq!(anon_speakers(&fixture), vec!["Speaker A", "Speaker B"]);
}
