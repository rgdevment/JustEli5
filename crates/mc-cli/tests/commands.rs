//! In-process tests of every `mc` command through `cli::run`: arguments,
//! replay, import, run, status, export, doctor, compare and the `--json`
//! error contract. Config precedence lives in `tests/config.rs`.

mod common;

use std::fs;

use common::*;

fn read(path: &std::path::Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Run ids of a session, from `mc status <session> --json`.
fn runs_of(s: &Sandbox, session: &str) -> Vec<String> {
    let out = s.mc(&["status", session, "--json"]);
    assert_eq!(out.code, 0, "status: {}", out.stderr);
    out.json()["runs"]
        .as_array()
        .expect("runs array")
        .iter()
        .map(|r| r["run_id"].as_str().expect("run_id").to_string())
        .collect()
}

// ---------------------------------------------------------------- args

mod args {
    use super::*;

    #[test]
    fn unknown_option_is_usage_error() {
        let s = Sandbox::new();
        let out = s.mc(&["doctor", "--bogus"]);
        assert_eq!(out.code, 2);
        assert!(
            out.stderr.contains("unknown option --bogus"),
            "{}",
            out.stderr
        );
        let out = s.mc(&["doctor", "-x"]);
        assert_eq!(out.code, 2);
        assert!(out.stderr.contains("unknown option -x"), "{}", out.stderr);
    }

    #[test]
    fn missing_value_is_usage_error() {
        let s = Sandbox::new();
        let out = s.mc(&["export", BASIC_SESSION, "--format"]);
        assert_eq!(out.code, 2);
        assert!(
            out.stderr.contains("--format needs a value"),
            "{}",
            out.stderr
        );
        assert_eq!(s.mc(&["run", "/tmp", "--debounce"]).code, 2);
        assert_eq!(s.mc(&["doctor", "--data-dir"]).code, 2);
    }

    #[test]
    fn equals_form_is_accepted() {
        let s = Sandbox::new();
        let dir = s.corpus("synthetic_basic");
        assert_eq!(s.mc(&["import", path_str(&dir)]).code, 0);
        let out = s.mc(&["run", path_str(&dir), "--debounce=2", "--json"]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert_eq!(out.json()["report"]["config"]["debounce_s"], 2.0);

        assert_eq!(s.mc(&["run", path_str(&dir)]).code, 0);
        let out = s.mc(&["export", BASIC_SESSION, "--format=txt"]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert!(out.stdout.contains("Buenos días"), "{}", out.stdout);
    }

    #[test]
    fn boolean_flag_rejects_a_value() {
        let s = Sandbox::new();
        let out = s.mc(&[
            "replay",
            path_str(&fixture("synthetic_basic")),
            "--verify=yes",
        ]);
        assert_eq!(out.code, 2);
        assert!(out.stderr.contains("takes no value"), "{}", out.stderr);
    }

    #[test]
    fn double_dash_ends_options() {
        let s = Sandbox::new();
        let dir = fixture("synthetic_basic");
        let dir = path_str(&dir);
        // After `--`, `--verify` is a positional, so the command gets one
        // positional too many.
        assert_eq!(s.mc(&["replay", dir, "--", "--verify"]).code, 2);
        assert_eq!(s.mc(&["replay", "--verify", "--", dir]).code, 0);
        assert_eq!(s.mc(&["replay", "--", dir, "--verify"]).code, 2);
    }

    #[test]
    fn help_prints_usage_on_stdout() {
        let s = Sandbox::new();
        for args in [
            &["--help"][..],
            &["-h"][..],
            &["run", "-h"][..],
            &["status", "--help"][..],
        ] {
            let out = s.mc(args);
            assert_eq!(out.code, 0, "{args:?}: {}", out.stderr);
            assert!(out.stdout.contains("usage:"), "{args:?}: {}", out.stdout);
            assert!(out.stderr.is_empty(), "{args:?}: {}", out.stderr);
        }
    }

    #[test]
    fn no_arguments_prints_usage() {
        let s = Sandbox::new();
        let out = s.mc(&[]);
        assert_eq!(out.code, 0);
        assert!(out.stdout.contains("usage:"), "{}", out.stdout);
    }

    #[test]
    fn version_prints_package_version() {
        let s = Sandbox::new();
        for args in [&["--version"][..], &["doctor", "--version"][..]] {
            let out = s.mc(args);
            assert_eq!(out.code, 0, "{args:?}");
            assert_eq!(
                out.stdout,
                format!("eli5 {}\n", env!("CARGO_PKG_VERSION")),
                "{args:?}"
            );
        }
    }

    #[test]
    fn unknown_command_is_usage_error() {
        let s = Sandbox::new();
        let out = s.mc(&["frobnicate"]);
        assert_eq!(out.code, 2);
        assert!(out.stderr.contains("unknown command"), "{}", out.stderr);
    }

    #[test]
    fn options_without_command_is_usage_error() {
        let s = Sandbox::new();
        let out = s.mc(&["--data-dir", "/tmp/x"]);
        assert_eq!(out.code, 2);
        assert!(out.stderr.contains("missing command"), "{}", out.stderr);
    }
}

// ---------------------------------------------------------------- replay

mod replay {
    use super::*;

    #[test]
    fn basic_strict_passes() {
        let s = Sandbox::new();
        let dir = fixture("synthetic_basic");
        assert_eq!(
            s.mc(&["replay", path_str(&dir), "--verify", "--strict"])
                .code,
            0
        );
    }

    #[test]
    fn skew_divergent_passes_without_strict_and_fails_with_it() {
        let s = Sandbox::new();
        let dir = fixture("synthetic_skew_divergent");
        assert_eq!(s.mc(&["replay", path_str(&dir), "--verify"]).code, 0);
        let out = s.mc(&["replay", path_str(&dir), "--verify", "--strict"]);
        assert_eq!(out.code, 1);
        assert!(out.stdout.contains("timing"), "{}", out.stdout);
    }

    #[test]
    fn missing_directory_is_runtime_error() {
        let s = Sandbox::new();
        let out = s.mc(&["replay", "/no/such/corpus", "--verify"]);
        assert_eq!(out.code, 3);
        assert_json_error(
            &s.mc(&["replay", "/no/such/corpus", "--verify", "--json"]),
            3,
            "io",
        );
    }

    #[test]
    fn json_reports_passed() {
        let s = Sandbox::new();
        let basic = fixture("synthetic_basic");
        let out = s.mc(&["replay", path_str(&basic), "--verify", "--json"]);
        assert_eq!(out.code, 0);
        let value = out.json();
        assert_eq!(value["passed"], true);
        assert_eq!(value["transcript_ok"], true);

        let skew = fixture("synthetic_skew_divergent");
        let out = s.mc(&["replay", path_str(&skew), "--verify", "--strict", "--json"]);
        assert_eq!(out.code, 1);
        assert_eq!(out.json()["passed"], false);
    }
}

// ---------------------------------------------------------------- import

mod import {
    use super::*;

    #[test]
    fn second_import_is_unchanged() {
        let s = Sandbox::new();
        let dir = s.corpus("synthetic_basic");
        let first = s.mc(&["import", path_str(&dir)]);
        assert_eq!(first.code, 0, "{}", first.stderr);
        assert!(first.stdout.contains("created"), "{}", first.stdout);

        let second = s.mc(&["import", path_str(&dir), "--json"]);
        assert_eq!(second.code, 0);
        assert_eq!(second.json()["sessions"][0]["action"], "unchanged");
    }

    #[test]
    fn json_shape() {
        let s = Sandbox::new();
        let dir = s.corpus("synthetic_basic");
        let out = s.mc(&["import", path_str(&dir), "--json"]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        let value = out.json();
        assert_eq!(value["failed"], 0);
        let item = &value["sessions"][0];
        assert_eq!(item["session_id"], BASIC_SESSION);
        assert_eq!(item["action"], "created");
        assert!(item["lines_added"].as_u64().unwrap() > 0);
        assert_eq!(item["bad_lines"], 0);
        assert_eq!(item["tail_pending_bytes"], 0);
        assert_eq!(item["ended_cleanly"], true);
        assert!(item["dir"].is_string());
    }

    #[test]
    fn corrupt_session_in_root_fails_but_others_import() {
        let s = Sandbox::new();
        let root = s.path("root");
        copy_tree(&fixture("synthetic_basic"), &root.join("good-a"));
        copy_tree(&fixture("synthetic_unicode"), &root.join("good-b"));
        copy_tree(&fixture("synthetic_basic"), &root.join("bad"));
        fs::write(root.join("bad").join("manifest.json"), "{not json").unwrap();

        let out = s.mc(&["import", path_str(&root)]);
        assert_eq!(
            out.code, 1,
            "stdout: {}\nstderr: {}",
            out.stdout, out.stderr
        );
        assert!(out.stdout.contains("FAILED"), "{}", out.stdout);

        let out = s.mc(&["import", path_str(&root), "--json"]);
        assert_eq!(out.code, 1);
        let value = out.json();
        assert_eq!(value["failed"], 1);
        let items = value["sessions"].as_array().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items.iter().filter(|i| i["error"].is_string()).count(), 1);

        let status = s.mc(&["status", "--json"]);
        assert_eq!(status.code, 0);
        let sessions = status.json()["sessions"].as_array().unwrap().len();
        assert_eq!(sessions, 2, "the two good sessions are indexed");
    }

    #[test]
    fn nonexistent_path_is_runtime_error() {
        let s = Sandbox::new();
        let out = s.mc(&["import", "/no/such/root"]);
        assert_eq!(out.code, 3);
        assert!(out.stderr.contains("not a directory"), "{}", out.stderr);
    }

    #[test]
    fn without_argument_imports_the_data_corpus_dir() {
        let s = Sandbox::new();
        let corpus = s.data().join("corpus");
        copy_tree(&fixture("synthetic_basic"), &corpus.join("basic"));
        let out = s.mc(&["import"]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert_eq!(
            s.mc(&["status", "--json"]).json()["sessions"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}

// ---------------------------------------------------------------- run

mod run {
    use super::*;

    #[test]
    fn created_then_unchanged_then_force_rewrites() {
        let s = Sandbox::new();
        let dir = s.corpus("synthetic_basic");
        let dir_s = path_str(&dir);
        assert_eq!(s.mc(&["import", dir_s]).code, 0);

        let first = s.mc(&["run", dir_s, "--json"]);
        assert_eq!(first.code, 0, "{}", first.stderr);
        assert_eq!(first.json()["action"], "created");
        let run_id = first.run_id();
        let transcript = dir.join("runs").join(&run_id).join("transcript.txt");
        let original = read(&transcript);

        let again = s.mc(&["run", dir_s, "--json"]);
        assert_eq!(again.json()["action"], "unchanged");

        // Unchanged leaves the files alone; --force rewrites them.
        fs::write(&transcript, "garbage").unwrap();
        assert_eq!(s.mc(&["run", dir_s]).code, 0);
        assert_eq!(read(&transcript), "garbage");

        let forced = s.mc(&["run", dir_s, "--force", "--json"]);
        assert_eq!(forced.code, 0, "{}", forced.stderr);
        assert_eq!(forced.json()["action"], "created");
        assert_eq!(forced.run_id(), run_id);
        assert_eq!(read(&transcript), original);
    }

    #[test]
    fn debounce_changes_the_run_id() {
        let s = Sandbox::new();
        let dir = s.corpus("synthetic_skew_divergent");
        let dir_s = path_str(&dir);
        assert_eq!(s.mc(&["import", dir_s]).code, 0);
        let default = s.mc(&["run", dir_s, "--json"]).run_id();
        let slow = s.mc(&["run", dir_s, "--debounce", "2", "--json"]);
        assert_eq!(slow.code, 0, "{}", slow.stderr);
        assert_ne!(slow.run_id(), default);
        assert_eq!(runs_of(&s, SKEW_SESSION).len(), 2);
    }

    #[test]
    fn unclean_corpus_prints_flush_note() {
        let s = Sandbox::new();
        let dir = s.corpus("synthetic_unclean_end");
        let out = s.mc(&["run", path_str(&dir)]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert!(out.stdout.contains("did not end cleanly"), "{}", out.stdout);
        let json = s.mc(&["run", path_str(&dir), "--json"]).json();
        assert_eq!(json["report"]["synthetic_flush"], true);
    }

    #[test]
    fn nonexistent_directory_is_runtime_error() {
        let s = Sandbox::new();
        let out = s.mc(&["run", "/no/such/session"]);
        assert_eq!(out.code, 3);
        assert_json_error(
            &s.mc(&["run", "/no/such/session", "--json"]),
            3,
            "not_found",
        );
    }

    #[test]
    fn invalid_debounce_and_flush_are_rejected() {
        let s = Sandbox::new();
        let dir = s.corpus("synthetic_basic");
        let dir_s = path_str(&dir);
        assert_eq!(s.mc(&["run", dir_s, "--debounce", "0"]).code, 2);
        assert_eq!(s.mc(&["run", dir_s, "--debounce", "soon"]).code, 2);
        assert_eq!(s.mc(&["run", dir_s, "--flush", "sometimes"]).code, 2);
        assert!(
            !dir.join("runs").exists(),
            "rejected flags must not write a run"
        );
    }
}

// ---------------------------------------------------------------- status

mod status {
    use super::*;

    #[test]
    fn empty_store_says_no_sessions() {
        let s = Sandbox::new();
        let out = s.mc(&["status"]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert!(out.stdout.contains("no sessions"), "{}", out.stdout);
        assert_eq!(
            s.mc(&["status", "--json"]).json()["sessions"],
            serde_json::json!([])
        );
    }

    #[test]
    fn lists_run_counts_after_import_and_run() {
        let s = Sandbox::new();
        s.imported_and_run("synthetic_basic");
        let out = s.mc(&["status"]);
        assert_eq!(out.code, 0);
        assert!(out.stdout.contains(BASIC_SESSION), "{}", out.stdout);
        assert!(out.stdout.contains("1 runs"), "{}", out.stdout);
        assert_eq!(s.mc(&["status", "--json"]).json()["sessions"][0]["runs"], 1);
    }

    #[test]
    fn unique_prefix_resolves_to_the_session() {
        let s = Sandbox::new();
        s.imported_and_run("synthetic_basic");
        let out = s.mc(&["status", "a8bf", "--json"]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert_eq!(out.json()["session_id"], BASIC_SESSION);
        assert_eq!(out.json()["runs"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn session_without_run_says_so() {
        let s = Sandbox::new();
        let dir = s.corpus("synthetic_basic");
        assert_eq!(s.mc(&["import", path_str(&dir)]).code, 0);
        let out = s.mc(&["status", BASIC_SESSION]);
        assert_eq!(out.code, 0);
        assert!(out.stdout.contains("no runs"), "{}", out.stdout);
    }

    #[test]
    fn ambiguous_prefix_is_usage_error() {
        // Both sessions start with "7" (7f1d... and 72cc...).
        let s = Sandbox::new();
        s.imported_and_run("synthetic_clock_jitter");
        s.imported_and_run("synthetic_single_node");
        let out = s.mc(&["status", "7"]);
        assert_eq!(out.code, 2, "{}", out.stderr);
        assert!(out.stderr.contains("ambiguous"), "{}", out.stderr);
    }

    #[test]
    fn unknown_prefix_is_runtime_error() {
        let s = Sandbox::new();
        s.imported_and_run("synthetic_basic");
        let out = s.mc(&["status", "zzz", "--json"]);
        assert_json_error(&out, 3, "not_found");
    }
}

// ---------------------------------------------------------------- export

mod export {
    use super::*;

    /// A sandbox with the basic corpus imported and run once.
    fn exported() -> (Sandbox, std::path::PathBuf, String) {
        let s = Sandbox::new();
        let dir = s.imported_and_run("synthetic_basic");
        let run_id = runs_of(&s, BASIC_SESSION).remove(0);
        (s, dir, run_id)
    }

    #[test]
    fn stdout_for_txt_md_and_json() {
        let (s, dir, run_id) = exported();
        let run_dir = dir.join("runs").join(&run_id);

        let txt = s.mc(&["export", BASIC_SESSION, "--format", "txt"]);
        assert_eq!(txt.code, 0, "{}", txt.stderr);
        assert_eq!(txt.stdout, read(&run_dir.join("transcript.txt")));

        let md = s.mc(&["export", BASIC_SESSION, "--format", "md"]);
        assert_eq!(md.code, 0);
        assert_eq!(md.stdout, read(&run_dir.join("transcript.md")));

        let json = s.mc(&["export", BASIC_SESSION, "--format", "json"]);
        assert_eq!(json.code, 0);
        let value = json.json();
        assert!(value["report"].is_object());
        let segments = value["segments"].as_array().expect("segments array");
        assert!(!segments.is_empty());
        assert_eq!(
            segments.len() as u64,
            value["report"]["segments"].as_u64().unwrap()
        );
    }

    #[test]
    fn out_file_refuses_overwrite_and_force_replaces() {
        let (s, dir, run_id) = exported();
        let target = s.path("out.txt");
        let target_s = path_str(&target);
        let expected = read(&dir.join("runs").join(&run_id).join("transcript.txt"));

        let out = s.mc(&[
            "export",
            BASIC_SESSION,
            "--format",
            "txt",
            "--out",
            target_s,
        ]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert_eq!(read(&target), expected);

        let again = s.mc(&[
            "export",
            BASIC_SESSION,
            "--format",
            "txt",
            "--out",
            target_s,
        ]);
        assert_eq!(again.code, 3);
        assert!(again.stderr.contains("already exists"), "{}", again.stderr);
        assert!(again.stderr.contains("--force"), "{}", again.stderr);

        fs::write(&target, "stale").unwrap();
        let forced = s.mc(&[
            "export",
            BASIC_SESSION,
            "--format",
            "txt",
            "--out",
            target_s,
            "--force",
        ]);
        assert_eq!(forced.code, 0, "{}", forced.stderr);
        assert_eq!(read(&target), expected);
    }

    #[test]
    fn out_directory_gets_session_and_run_name() {
        let (s, _dir, run_id) = exported();
        let with_slash = format!("{}/", s.path("exports").display());
        let out = s.mc(&[
            "export",
            BASIC_SESSION,
            "--format",
            "txt",
            "--out",
            &with_slash,
        ]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        let name = format!("{}-{}.txt", &BASIC_SESSION[..8], &run_id[..9]);
        assert!(s.path("exports").join(&name).is_file(), "missing {name}");

        // An existing directory without a trailing slash works the same way.
        let existing = s.path("existing");
        fs::create_dir_all(&existing).unwrap();
        let existing_s = path_str(&existing);
        let out = s.mc(&[
            "export",
            BASIC_SESSION,
            "--format",
            "md",
            "--out",
            existing_s,
        ]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        let name = format!("{}-{}.md", &BASIC_SESSION[..8], &run_id[..9]);
        assert!(existing.join(&name).is_file(), "missing {name}");
    }

    #[test]
    fn out_dash_is_stdout() {
        let (s, _dir, _run) = exported();
        let plain = s.mc(&["export", BASIC_SESSION, "--format", "txt"]);
        let dash = s.mc(&["export", BASIC_SESSION, "--format", "txt", "--out", "-"]);
        assert_eq!(dash.code, 0);
        assert_eq!(dash.stdout, plain.stdout);
    }

    #[test]
    fn save_writes_into_output_dir() {
        let (s, _dir, run_id) = exported();
        let out = s.mc(&["export", BASIC_SESSION, "--format", "md", "--save"]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        let name = format!("{}-{}.md", &BASIC_SESSION[..8], &run_id[..9]);
        let saved = s.out().join(&name);
        assert!(saved.is_file(), "missing {}", saved.display());
        assert!(out.stdout.contains(&name), "{}", out.stdout);
    }

    #[test]
    fn no_run_is_runtime_error_with_hint() {
        let s = Sandbox::new();
        let dir = s.corpus("synthetic_unicode");
        assert_eq!(s.mc(&["import", path_str(&dir)]).code, 0);
        let out = s.mc(&["export", UNICODE_SESSION, "--format", "txt"]);
        assert_eq!(out.code, 3);
        assert!(out.stderr.contains("has no run"), "{}", out.stderr);
        assert!(out.stderr.contains("hint: mc run"), "{}", out.stderr);
    }

    #[test]
    fn tisty_cannot_be_combined_with_a_file_format() {
        let (s, _dir, _run) = exported();
        let out = s.mc(&["export", BASIC_SESSION, "--format", "txt", "--tisty"]);
        assert_eq!(out.code, 2);
        assert!(out.stderr.contains("cannot be combined"), "{}", out.stderr);
    }

    #[test]
    fn bad_or_missing_format_is_usage_error() {
        let (s, _dir, _run) = exported();
        assert_eq!(s.mc(&["export", BASIC_SESSION, "--format", "pdf"]).code, 2);
        assert_eq!(s.mc(&["export", BASIC_SESSION]).code, 2);
    }

    #[test]
    fn save_and_out_are_exclusive() {
        let (s, _dir, _run) = exported();
        let out = s.mc(&[
            "export",
            BASIC_SESSION,
            "--format",
            "txt",
            "--save",
            "--out",
            "-",
        ]);
        assert_eq!(out.code, 2);
        assert!(out.stderr.contains("exclusive"), "{}", out.stderr);
    }

    #[test]
    fn run_option_selects_the_run() {
        let s = Sandbox::new();
        let dir = s.imported_and_run("synthetic_skew_divergent");
        let default = runs_of(&s, SKEW_SESSION).remove(0);
        let slow = s
            .mc(&["run", path_str(&dir), "--debounce", "2", "--json"])
            .run_id();
        assert_ne!(slow, default);

        let out = s.mc(&[
            "export",
            SKEW_SESSION,
            "--format",
            "json",
            "--run",
            &slow[..10],
            "--out",
            "-",
        ]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert_eq!(out.json()["report"]["config"]["debounce_s"], 2.0);

        let unknown = s.mc(&[
            "export",
            SKEW_SESSION,
            "--format",
            "txt",
            "--run",
            "rdeadbeef",
        ]);
        assert_eq!(unknown.code, 3);
    }
}

// ---------------------------------------------------------------- doctor

mod doctor {
    use super::*;

    fn check_status(value: &serde_json::Value, name: &str) -> String {
        value["checks"]
            .as_array()
            .expect("checks array")
            .iter()
            .find(|c| c["name"] == name)
            .unwrap_or_else(|| panic!("no {name} check in {value}"))["status"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn tisty_on_path_is_ok() {
        // The host's PATH rules (separator, PATHEXT), since the file is real.
        let s = Sandbox::with_os(mc_cli::cli::Os::current());
        let bin = s.path("bin");
        let tisty = fake_command(&bin, "tisty");
        let env = s.env_with(&[("PATH", path_str(&bin))]);
        let out = mc(&env, &["doctor", "--json"]);
        assert_eq!(out.code, 0, "{}", out.stdout);
        let value = out.json();
        assert_eq!(check_status(&value, "tisty"), "ok");
        let detail = value["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "tisty")
            .unwrap()["detail"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(same_path(&detail, &tisty), "{detail}");

        let human = mc(&env, &["doctor"]);
        assert!(
            human
                .stdout
                .lines()
                .any(|l| l.starts_with("ok") && l.contains("tisty")),
            "{}",
            human.stdout
        );
    }

    #[test]
    fn tisty_absent_warns_but_exits_0() {
        let s = Sandbox::new();
        let out = s.mc(&["doctor", "--json"]);
        assert_eq!(out.code, 0, "{}", out.stdout);
        assert_eq!(check_status(&out.json(), "tisty"), "warn");
        assert_eq!(out.json()["ok"], true);
    }

    #[test]
    fn data_dir_that_is_a_file_fails() {
        let s = Sandbox::new();
        let file = s.path("not-a-dir");
        fs::write(&file, "x").unwrap();
        let env = s.env_with(&[("MC_DATA_DIR", path_str(&file))]);
        let out = mc(&env, &["doctor", "--json"]);
        assert_eq!(out.code, 1, "{}", out.stdout);
        let value = out.json();
        assert_eq!(check_status(&value, "data_dir"), "FAIL");
        assert_eq!(value["ok"], false);
        assert!(mc(&env, &["doctor"]).stdout.contains("FAIL"));
    }

    #[test]
    fn json_has_checks_array() {
        let s = Sandbox::new();
        let out = s.mc(&["doctor", "--json"]);
        let value = out.json();
        let names: Vec<&str> = value["checks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "config",
                "data_dir",
                "index",
                "output_dir",
                "tisty",
                "llm",
                "whisper"
            ]
        );
        for check in value["checks"].as_array().unwrap() {
            assert!(check["status"].is_string());
            assert!(check["detail"].is_string());
        }
    }

    #[test]
    fn positional_argument_is_usage_error() {
        let s = Sandbox::new();
        assert_eq!(s.mc(&["doctor", "extra"]).code, 2);
    }
}

// ---------------------------------------------------------------- compare

mod compare {
    use super::*;

    /// Two V1 runs of the skew corpus (V2 ignores the debounce): default debounce and `--debounce 2`.
    fn two_runs() -> (Sandbox, String, String) {
        let s = Sandbox::new();
        let dir = s.imported_and_run("synthetic_skew_divergent");
        let run = |extra: &[&str]| {
            let mut args = vec!["run", path_str(&dir), "--policy", "v1_legacy", "--json"];
            args.extend_from_slice(extra);
            s.mc(&args).run_id()
        };
        let (a, b) = (run(&[]), run(&["--debounce", "2"]));
        (s, a, b)
    }

    #[test]
    fn same_run_is_identical() {
        let (s, a, _b) = two_runs();
        let out = s.mc(&["compare", &a, &a]);
        assert_eq!(out.code, 0, "{}", out.stdout);
        assert!(out.stdout.contains("identical"), "{}", out.stdout);
        assert_eq!(
            s.mc(&["compare", &a, &a, "--json"]).json()["identical"],
            true
        );
    }

    #[test]
    fn different_debounce_differs_with_timing_deltas() {
        let (s, a, b) = two_runs();
        let out = s.mc(&["compare", &a, &b]);
        assert_eq!(out.code, 1, "{}", out.stdout);
        assert!(out.stdout.contains("differ"), "{}", out.stdout);
        assert!(out.stdout.contains("config debounce_s"), "{}", out.stdout);

        let value = s.mc(&["compare", &a, &b, "--json"]).json();
        assert_eq!(value["identical"], false);
        let timing = value["timing_div"].as_array().unwrap();
        assert_ne!(
            timing[0], timing[1],
            "timing divergences should differ: {value}"
        );
        assert!(timing[1].as_u64().unwrap() > 0 || timing[0].as_u64().unwrap() > 0);
    }

    #[test]
    fn ambiguous_and_unknown_runs() {
        let (s, _a, _b) = two_runs();
        // Every run id starts with "r", so the prefix matches both.
        let ambiguous = s.mc(&["compare", "r", "r"]);
        assert_eq!(ambiguous.code, 2, "{}", ambiguous.stderr);
        let unknown = s.mc(&["compare", "rdeadbeef", "rdeadbeef"]);
        assert_eq!(unknown.code, 3, "{}", unknown.stderr);
    }

    #[test]
    fn json_parses_and_needs_two_positionals() {
        let (s, a, b) = two_runs();
        let value = s.mc(&["compare", &a, &b, "--json"]).json();
        assert_eq!(value["a"], a);
        assert_eq!(value["b"], b);
        assert!(value["diff"].is_object());
        assert_eq!(s.mc(&["compare", &a]).code, 2);
    }
}

// ---------------------------------------------------------------- contract

mod contract {
    use super::*;

    /// Every error with `--json`: empty stdout, error object on stderr.
    #[test]
    fn every_error_is_json_on_stderr_only() {
        let s = Sandbox::new();
        let basic = s.imported_and_run("synthetic_basic");
        let unicode = s.corpus("synthetic_unicode");
        assert_eq!(s.mc(&["import", path_str(&unicode)]).code, 0);
        s.imported_and_run("synthetic_clock_jitter");
        s.imported_and_run("synthetic_single_node");
        assert_eq!(s.mc(&["run", path_str(&basic), "--debounce", "2"]).code, 0);

        let existing = s.path("existing.txt");
        fs::write(&existing, "keep").unwrap();
        let existing_s = path_str(&existing);
        let basic_s = path_str(&basic);
        let replay_dir = fixture("synthetic_basic");
        let replay_s = path_str(&replay_dir);

        let cases: Vec<(Vec<&str>, u8, &str)> = vec![
            (vec!["doctor", "--bogus", "--json"], 2, "usage"),
            (vec!["frobnicate", "--json"], 2, "usage"),
            (vec!["doctor", "extra", "--json"], 2, "usage"),
            (vec!["status", "a", "b", "--json"], 2, "usage"),
            (vec!["export", BASIC_SESSION, "--json"], 2, "usage"),
            (
                vec![
                    "export",
                    BASIC_SESSION,
                    "--format",
                    "txt",
                    "--tisty",
                    "--json",
                ],
                2,
                "usage",
            ),
            (
                vec![
                    "export",
                    BASIC_SESSION,
                    "--format",
                    "txt",
                    "--save",
                    "--out",
                    "-",
                    "--json",
                ],
                2,
                "usage",
            ),
            (
                vec![
                    "export",
                    BASIC_SESSION,
                    "--format",
                    "txt",
                    "--out",
                    &existing_s,
                    "--json",
                ],
                3,
                "exists",
            ),
            (
                vec!["export", UNICODE_SESSION, "--format", "txt", "--json"],
                3,
                "not_found",
            ),
            (
                vec!["export", "zzz", "--format", "txt", "--json"],
                3,
                "not_found",
            ),
            (vec!["status", "7", "--json"], 2, "usage"),
            (vec!["status", "zzz", "--json"], 3, "not_found"),
            (vec!["compare", "r", "r", "--json"], 2, "usage"),
            (
                vec!["compare", "rdeadbeef", "rdeadbeef", "--json"],
                3,
                "not_found",
            ),
            (vec!["run", "/no/such/session", "--json"], 3, "not_found"),
            (
                vec!["run", &basic_s, "--debounce", "0", "--json"],
                2,
                "usage",
            ),
            (
                vec!["run", &basic_s, "--flush", "sometimes", "--json"],
                2,
                "usage",
            ),
            (vec!["import", "/no/such/root", "--json"], 3, "not_found"),
            (
                vec!["replay", "/no/such/corpus", "--verify", "--json"],
                3,
                "io",
            ),
            (vec!["replay", &replay_s, "--json"], 2, "usage"),
        ];
        for (args, exit, code) in cases {
            let out = s.mc(&args);
            assert_json_error(&out, exit, code);
            assert!(
                !out.err_json()["error"]["message"]
                    .as_str()
                    .unwrap()
                    .is_empty(),
                "{args:?}"
            );
        }
    }

    /// Without any data directory, `import` is a config error (exit 2).
    #[test]
    fn missing_home_and_data_dir_is_config_error_for_import() {
        let s = Sandbox::new();
        let env = s.env_without(&["MC_DATA_DIR", "HOME"]);
        let out = mc(&env, &["import", "--json"]);
        assert_json_error(&out, 2, "config");
        assert!(
            out.err_json()["error"]["message"]
                .as_str()
                .unwrap()
                .contains("MC_DATA_DIR")
        );
    }
}

// ------------------------------------------------- audit regressions

mod audit {
    use super::*;

    #[test]
    fn doctor_reports_a_broken_config_instead_of_aborting() {
        let s = Sandbox::new();
        let bad = s.path("bad.json");
        std::fs::write(&bad, "{ not json").unwrap();
        let out = s.mc(&["doctor", "--config", path_str(&bad), "--json"]);
        assert_eq!(out.code, 1);
        assert_eq!(out.json()["checks"][0]["status"], "FAIL");
        assert!(out.stderr.is_empty(), "{}", out.stderr);
    }

    #[test]
    fn export_json_without_out_is_one_json_document() {
        let s = Sandbox::new();
        let dir = s.imported_and_run("synthetic_basic");
        let _ = dir;
        let status = s.mc(&["status", "--json"]);
        let session = status.json()["sessions"][0]["session_id"]
            .as_str()
            .unwrap()
            .to_string();
        let out = s.mc(&["export", &session, "--format", "txt", "--json"]);
        assert_eq!(out.code, 0);
        let value = out.json();
        assert_eq!(value["format"], "txt");
        assert!(value["content"].as_str().unwrap().contains("Ana"));
    }

    #[test]
    fn empty_ids_and_empty_out_are_usage_errors() {
        let s = Sandbox::new();
        s.imported_and_run("synthetic_basic");
        assert_eq!(s.mc(&["status", ""]).code, 2);
        let status = s.mc(&["status", "--json"]);
        let session = status.json()["sessions"][0]["session_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            s.mc(&["export", &session, "--format", "txt", "--out", ""])
                .code,
            2
        );
    }

    #[test]
    fn import_on_a_fresh_install_is_not_an_error() {
        let s = Sandbox::new();
        let out = s.mc(&["import", "--json"]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert_eq!(out.json()["sessions"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn config_version_other_than_one_is_rejected() {
        let s = Sandbox::new();
        let file = s.path("config.json");
        std::fs::write(&file, r#"{"version": 2}"#).unwrap();
        let out = s.mc(&["doctor", "--config", path_str(&file), "--json"]);
        assert_eq!(out.code, 1);
        assert!(
            out.json()["checks"][0]["detail"]
                .as_str()
                .unwrap()
                .contains("version")
        );
    }

    #[test]
    fn tilde_paths_and_manifest_only_spelling() {
        let s = Sandbox::new();
        let out = s.mc(&["doctor", "--data-dir", "~/mc", "--json"]);
        assert_eq!(out.json()["checks"][0]["status"], "FAIL");
        let file = s.path("config.json");
        std::fs::write(&file, r#"{"run": {"flush": "manifest_only"}}"#).unwrap();
        let out = s.mc(&["doctor", "--config", path_str(&file), "--json"]);
        assert_eq!(out.json()["checks"][0]["status"], "ok");
    }

    #[test]
    fn a_session_id_with_a_separator_is_not_imported() {
        let s = Sandbox::new();
        let dir = s.corpus("synthetic_basic");
        let manifest = dir.join("manifest.json");
        let text = std::fs::read_to_string(&manifest).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
        value["session_id"] = serde_json::json!("../evil");
        std::fs::write(&manifest, value.to_string()).unwrap();
        let out = s.mc(&["import", path_str(&dir), "--json"]);
        assert_eq!(out.code, 1);
    }
}
