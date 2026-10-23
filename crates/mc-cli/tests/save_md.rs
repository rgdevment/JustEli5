//! `save_md`, the TUI's "Save as .md": names, no overwrite, folder creation
//! and sanitised ids.

use std::fs;

use mc_cli::cli::CliError;
use mc_cli::cmd::export::save_md;

const SESSION: &str = "a8bf3c69037539ed23407ca2c775546c";
const RUN: &str = "run_0123456789abc";

fn runtime_code(err: &CliError) -> &'static str {
    match err {
        CliError::Runtime { code, .. } => code,
        other => panic!("expected a runtime error, got {other:?}"),
    }
}

#[test]
fn writes_session8_run9_md_into_the_folder() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("notes");
    fs::create_dir(&out).unwrap();

    let path = save_md("# Reunião\n", SESSION, RUN, &out).unwrap();
    assert_eq!(path, out.join("a8bf3c69-run_01234.md"));
    assert_eq!(fs::read_to_string(&path).unwrap(), "# Reunião\n");
}

#[test]
fn refuses_to_overwrite_an_existing_file() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("notes");
    fs::create_dir(&out).unwrap();
    let path = save_md("primeiro\n", SESSION, RUN, &out).unwrap();

    let err = save_md("segundo\n", SESSION, RUN, &out).unwrap_err();
    assert_eq!(runtime_code(&err), "exists");
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "primeiro\n",
        "the first save is left untouched"
    );
}

#[test]
fn creates_the_folder_when_its_parent_exists() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("new_folder");
    assert!(!out.exists());

    let path = save_md("x", SESSION, RUN, &out).unwrap();
    assert!(out.is_dir());
    assert_eq!(path.parent(), Some(out.as_path()));
}

#[test]
fn errors_when_the_parent_folder_is_missing_and_creates_nothing() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("missing").join("child");

    let err = save_md("x", SESSION, RUN, &out).unwrap_err();
    assert_eq!(runtime_code(&err), "io");
    assert!(err.message().contains("the parent folder does not exist"));
    assert!(!root.path().join("missing").exists());
}

#[test]
fn errors_when_the_target_is_a_file_not_a_folder() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("afile");
    fs::write(&file, "not a folder").unwrap();

    assert!(save_md("x", SESSION, RUN, &file).is_err());
    assert_eq!(fs::read_to_string(&file).unwrap(), "not a folder");
}

#[test]
fn odd_ids_are_sanitised_so_nothing_escapes_the_folder() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("notes");
    fs::create_dir(&out).unwrap();

    let path = save_md("x", "../x", "../x", &out).unwrap();
    assert_eq!(path, out.join("x-x.md"));
    assert_eq!(path.parent(), Some(out.as_path()));

    // Nothing was written next to the folder, and an empty id still gets a name.
    let entries: Vec<_> = fs::read_dir(root.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(entries, vec![std::ffi::OsString::from("notes")]);

    // An id with nothing usable left becomes "x", never an empty name.
    let path = save_md("y", "", "run_1", &out).unwrap();
    assert_eq!(path, out.join("x-run_1.md"));
}
