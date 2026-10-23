//! The on-disk session: ids, folder naming, manifest fields, heartbeat cadence,
//! close, and the scan for sessions that never closed.

use std::collections::HashSet;
use std::path::Path;

use mc_core::Stamp;
use mc_core::time::{dir_stamp, utc_iso_ms};
use mc_engine::corpus::{CorpusSession, SessionSpec, generate_session_id, scan_unclean};
use mc_store::manifest::Manifest;
use serde_json::Value;

const T0: i64 = 1_700_000_000_123;

fn spec(platform: &str) -> SessionSpec<'_> {
    SessionSpec {
        platform,
        capture_source: "FakeSource",
        meeting_name: None,
        consent_method: "flag",
        t0_wall_ms: T0,
        debounce_clock: "mono",
    }
}

fn stamp(wall_ms: i64, mono_ms: f64) -> Stamp {
    Stamp { wall_ms, mono_ms }
}

fn load(dir: &Path) -> Manifest {
    Manifest::load(dir.join("manifest.json")).expect("manifest loads")
}

fn raw(dir: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap()
}

#[test]
fn session_ids_are_32_lowercase_hex_and_unique() {
    let mut seen = HashSet::new();
    for _ in 0..10_000 {
        let id = generate_session_id();
        assert_eq!(id.len(), 32, "{id}");
        assert!(
            id.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "not lowercase hex: {id}"
        );
        assert!(seen.insert(id.clone()), "duplicate id {id}");
    }
}

#[test]
fn each_session_gets_its_own_id() {
    let root = tempfile::tempdir().unwrap();
    let a = CorpusSession::create(root.path(), &spec("teams")).unwrap();
    let b = CorpusSession::create(root.path(), &spec("teams")).unwrap();
    assert_eq!(a.session_id().len(), 32);
    assert_ne!(a.session_id(), b.session_id());
    assert_eq!(load(a.dir()).session_id, a.session_id());
}

#[test]
fn folder_is_named_by_start_time_and_platform() {
    let root = tempfile::tempdir().unwrap();
    let session = CorpusSession::create(root.path(), &spec("teams")).unwrap();
    let expected = format!("{}_teams", dir_stamp(T0));
    assert_eq!(session.dir().file_name().unwrap(), expected.as_str());
    assert!(session.dir().join("manifest.json").is_file());
    assert!(session.dir().join("captions.jsonl").is_file());
    assert!(!session.dir().join("manifest.json.tmp").exists());
}

#[test]
fn same_second_same_platform_gets_a_numeric_suffix() {
    let root = tempfile::tempdir().unwrap();
    let base = format!("{}_teams", dir_stamp(T0));
    let first = CorpusSession::create(root.path(), &spec("teams")).unwrap();
    let second = CorpusSession::create(root.path(), &spec("teams")).unwrap();
    let third = CorpusSession::create(root.path(), &spec("teams")).unwrap();
    assert_eq!(first.dir().file_name().unwrap(), base.as_str());
    assert_eq!(
        second.dir().file_name().unwrap(),
        format!("{base}_2").as_str()
    );
    assert_eq!(
        third.dir().file_name().unwrap(),
        format!("{base}_3").as_str()
    );
    // Nothing was written over the first session.
    assert_eq!(load(first.dir()).session_id, first.session_id());
}

#[test]
fn platform_names_are_sanitised_for_the_folder() {
    let root = tempfile::tempdir().unwrap();
    let stamp_part = dir_stamp(T0);
    let cases = [
        ("Microsoft Teams", "Microsoft_Teams"),
        ("Zoom/Web", "Zoom_Web"),
        ("a\\b c/d", "a_b_c_d"),
        ("", "meeting"),
        ("zoom_2", "zoom_2"),
    ];
    for (platform, folder) in cases {
        let session = CorpusSession::create(root.path(), &spec(platform)).unwrap();
        let name = session
            .dir()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert_eq!(
            name,
            format!("{stamp_part}_{folder}"),
            "platform {platform:?}"
        );
        // The requested name is kept verbatim in the manifest.
        assert_eq!(load(session.dir()).platform_requested, platform);
    }
}

#[test]
fn manifest_has_the_python_compatible_fields_before_close() {
    let root = tempfile::tempdir().unwrap();
    let session = CorpusSession::create(root.path(), &spec("teams")).unwrap();
    let raw = raw(session.dir());

    assert_eq!(
        raw["runtime"].as_str().unwrap().split(' ').next(),
        Some("rust")
    );
    assert!(raw["runtime"].as_str().unwrap().starts_with("rust "));
    assert!(raw["python"].is_null());
    assert_eq!(raw["captions"]["debounce_clock"], "mono");
    assert_eq!(raw["captions"]["file"], "captions.jsonl");
    assert_eq!(raw["captions"]["schema"], 2);
    assert_eq!(raw["capture_source"], "FakeSource");
    assert_eq!(raw["started_wall_ms"], T0);
    assert_eq!(raw["started_utc_iso"], utc_iso_ms(T0));
    assert_eq!(raw["clock"]["t0_wall_ms"], T0);
    assert_eq!(raw["clock"]["t0_utc_iso"], utc_iso_ms(T0));

    let consent = &raw["consent"];
    assert_eq!(consent["captions"], true);
    assert_eq!(consent["audio"], false);
    assert_eq!(consent["method"], "flag");
    assert_eq!(consent["wall_ms"], T0);

    for key in [
        "ended_wall_ms",
        "ended_utc_iso",
        "ended_mono_ms",
        "end_reason",
    ] {
        assert!(raw[key].is_null(), "{key} must be null until close");
    }
    assert!(!load(session.dir()).ended_cleanly());
}

#[test]
fn close_records_the_end_and_rounds_the_monotonic_mark() {
    let root = tempfile::tempdir().unwrap();
    let mut session = CorpusSession::create(root.path(), &spec("teams")).unwrap();
    let end_wall = T0 + 1_234;
    session
        .close("stop", stamp(end_wall, 1_234.567_89))
        .unwrap();

    let manifest = load(session.dir());
    assert!(manifest.ended_cleanly());
    assert_eq!(manifest.ended_wall_ms, Some(end_wall));
    assert_eq!(
        manifest.ended_utc_iso.as_deref(),
        Some(utc_iso_ms(end_wall).as_str())
    );
    assert_eq!(manifest.ended_mono_ms, Some(1_234.568));
    assert_eq!(manifest.end_reason.as_deref(), Some("stop"));
}

#[test]
fn close_keeps_every_other_field() {
    let root = tempfile::tempdir().unwrap();
    let mut session = CorpusSession::create(root.path(), &spec("teams")).unwrap();
    session.close("source_ended", stamp(T0 + 5, 5.0)).unwrap();
    let raw = raw(session.dir());
    assert_eq!(raw["end_reason"], "source_ended");
    assert_eq!(raw["consent"]["method"], "flag");
    assert_eq!(raw["captions"]["debounce_clock"], "mono");
    assert_eq!(raw["session_id"], session.session_id());
}

#[test]
fn heartbeat_writes_at_most_once_a_minute_and_never_ends_the_session() {
    let root = tempfile::tempdir().unwrap();
    let mut session = CorpusSession::create(root.path(), &spec("teams")).unwrap();
    let dir = session.dir().to_path_buf();
    let seen = |dir: &Path| {
        let raw = raw(dir);
        (
            raw["last_seen_mono_ms"].as_f64(),
            raw["last_seen_wall_ms"].as_i64(),
        )
    };

    // Before the first minute: nothing written.
    session.heartbeat(stamp(T0 + 59_999, 59_999.0)).unwrap();
    assert_eq!(seen(&dir), (None, None));

    // At exactly one minute: written.
    session.heartbeat(stamp(T0 + 60_000, 60_000.0)).unwrap();
    assert_eq!(seen(&dir), (Some(60_000.0), Some(T0 + 60_000)));

    // Less than a minute after that: skipped, even with new wall time.
    session.heartbeat(stamp(T0 + 100_000, 100_000.0)).unwrap();
    assert_eq!(seen(&dir), (Some(60_000.0), Some(T0 + 60_000)));

    // A minute later: written again.
    session.heartbeat(stamp(T0 + 120_000, 120_000.0)).unwrap();
    assert_eq!(seen(&dir), (Some(120_000.0), Some(T0 + 120_000)));

    let raw = raw(&dir);
    assert!(raw["ended_wall_ms"].is_null());
    assert!(raw["ended_utc_iso"].is_null());
    assert!(raw["ended_mono_ms"].is_null());
    assert!(raw["end_reason"].is_null());
    assert!(!load(&dir).ended_cleanly(), "a heartbeat is not a close");
}

#[test]
fn set_meeting_name_saves_and_skips_an_unchanged_name() {
    let root = tempfile::tempdir().unwrap();
    let mut session = CorpusSession::create(root.path(), &spec("teams")).unwrap();
    assert_eq!(load(session.dir()).meeting_name, None);

    session.set_meeting_name("Daily standup").unwrap();
    assert_eq!(
        load(session.dir()).meeting_name.as_deref(),
        Some("Daily standup")
    );

    session.set_meeting_name("Daily standup").unwrap();
    session.set_meeting_name("Retro").unwrap();
    assert_eq!(load(session.dir()).meeting_name.as_deref(), Some("Retro"));
}

#[test]
fn set_meeting_name_keeps_unicode_names() {
    let root = tempfile::tempdir().unwrap();
    let mut session = CorpusSession::create(root.path(), &spec("teams")).unwrap();
    session.set_meeting_name("Reunión: ñandú 🐦").unwrap();
    assert_eq!(
        load(session.dir()).meeting_name.as_deref(),
        Some("Reunión: ñandú 🐦")
    );
}

#[test]
fn scan_unclean_lists_open_sessions_and_removes_stale_tmp_files() {
    let root = tempfile::tempdir().unwrap();

    // Killed session: never closed.
    let open = CorpusSession::create(root.path(), &spec("teams")).unwrap();
    let open_dir = open.dir().to_path_buf();
    let open_id = open.session_id().to_string();
    std::fs::write(open_dir.join("manifest.json.tmp"), "{ half written").unwrap();
    drop(open);

    // Closed session: ignored, but its stale tmp file is still cleaned.
    let mut closed = CorpusSession::create(root.path(), &spec("zoom")).unwrap();
    closed.close("stop", stamp(T0 + 1, 1.0)).unwrap();
    let closed_dir = closed.dir().to_path_buf();
    drop(closed);
    std::fs::write(closed_dir.join("manifest.json.tmp"), "junk").unwrap();

    // A folder without a manifest is not a session.
    std::fs::create_dir(root.path().join("not-a-session")).unwrap();

    // Files just written look like a capture that is still running; these
    // sessions were killed an hour ago.
    age_files(root.path());

    let found = scan_unclean(root.path());
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].dir, open_dir);
    assert_eq!(found[0].session_id, open_id);

    assert!(
        !open_dir.join("manifest.json.tmp").exists(),
        "stale tmp removed"
    );
    assert!(
        !closed_dir.join("manifest.json.tmp").exists(),
        "stale tmp removed"
    );
    // Reporting only: the open session is not closed or changed.
    assert!(!load(&open_dir).ended_cleanly());
    assert_eq!(load(&open_dir).session_id, open_id);
}

#[test]
fn scan_unclean_on_a_missing_root_is_empty() {
    let root = tempfile::tempdir().unwrap();
    assert!(scan_unclean(root.path().join("nothing-here")).is_empty());
}

#[test]
fn scan_unclean_reports_sessions_in_folder_order() {
    let root = tempfile::tempdir().unwrap();
    let late = CorpusSession::create(
        root.path(),
        &SessionSpec {
            t0_wall_ms: T0 + 60_000,
            ..spec("teams")
        },
    )
    .unwrap();
    let early = CorpusSession::create(root.path(), &spec("teams")).unwrap();
    age_files(root.path());
    let found: Vec<_> = scan_unclean(root.path())
        .into_iter()
        .map(|u| u.dir)
        .collect();
    assert_eq!(
        found,
        vec![early.dir().to_path_buf(), late.dir().to_path_buf()]
    );
}

/// Makes every file under `dir` look an hour old.
fn age_files(dir: &std::path::Path) {
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            age_files(&path);
        } else {
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
    }
}

#[test]
fn a_session_touched_in_the_last_two_minutes_is_left_alone() {
    let root = tempfile::tempdir().unwrap();
    let live = CorpusSession::create(root.path(), &spec("teams")).unwrap();
    let tmp = live.dir().join("manifest.json.tmp");
    std::fs::write(&tmp, "being written").unwrap();
    assert!(scan_unclean(root.path()).is_empty());
    assert!(tmp.exists(), "a live session's tmp file must survive");
}

#[test]
fn a_pinned_meeting_name_wins_over_the_source() {
    let root = tempfile::tempdir().unwrap();
    let mut pinned = CorpusSession::create(
        root.path(),
        &SessionSpec {
            meeting_name: Some("Mi nombre".to_string()),
            ..spec("teams")
        },
    )
    .unwrap();
    pinned.set_meeting_name("Nombre de la fuente").unwrap();
    assert_eq!(pinned.manifest().meeting_name.as_deref(), Some("Mi nombre"));
}
