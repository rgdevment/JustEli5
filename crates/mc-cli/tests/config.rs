//! Configuration: precedence per field (flag > env > file > default), the
//! default config and data locations per OS, file validation errors, and
//! behaviour when no data directory can be located.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::*;
use mc_cli::args::{Parsed, Spec, parse};
use mc_cli::cli::{CliError, Env, Os};
use mc_cli::config::{Config, default_config_path, default_data_dir};
use mc_engine::runs::FlushMode;
use mc_reconcile::{Policy, ReconcilerConfig};
use serde_json::{Value, json};

/// Parses option-only arguments the way `mc run` does.
fn parsed(args: &[&str]) -> Parsed {
    let spec = Spec {
        values: &["flush", "debounce", "llm-provider", "llm-model"],
        flags: &["force"],
    };
    let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    parse(&owned, &spec).expect("arguments parse")
}

fn load(env: &Env, args: &[&str]) -> Result<Config, CliError> {
    Config::load(env, &parsed(args), true)
}

fn source(cfg: &Config, key: &str) -> Option<&'static str> {
    cfg.sources.get(key).copied()
}

fn write_json(path: &Path, value: &Value) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, value.to_string()).unwrap();
    path.to_path_buf()
}

// ------------------------------------------------------ default locations

mod default_locations {
    use super::*;

    #[test]
    fn windows_uses_appdata_and_localappdata() {
        let env = env(
            Os::Windows,
            &[
                ("APPDATA", r"C:\Users\me\AppData\Roaming"),
                ("LOCALAPPDATA", r"C:\Users\me\AppData\Local"),
                ("USERPROFILE", r"C:\Users\me"),
                ("HOME", "/should/not/be/used"),
            ],
        );
        assert_eq!(
            default_config_path(&env),
            Some(
                PathBuf::from(r"C:\Users\me\AppData\Roaming")
                    .join("justeli5")
                    .join("config.json")
            )
        );
        assert_eq!(
            default_data_dir(&env),
            Some(PathBuf::from(r"C:\Users\me\AppData\Local").join("justeli5"))
        );
    }

    #[test]
    fn windows_without_appdata_or_localappdata_has_no_location() {
        let env = env(Os::Windows, &[("HOME", "/home/me")]);
        assert_eq!(default_config_path(&env), None);
        assert_eq!(default_data_dir(&env), None);
    }

    #[test]
    fn mac_uses_application_support_and_ignores_xdg() {
        let env = env(
            Os::Mac,
            &[
                ("HOME", "/Users/me"),
                ("XDG_CONFIG_HOME", "/xdg/config"),
                ("XDG_DATA_HOME", "/xdg/data"),
            ],
        );
        let support = PathBuf::from("/Users/me/Library/Application Support");
        assert_eq!(
            default_config_path(&env),
            Some(support.join("justeli5").join("config.json"))
        );
        assert_eq!(default_data_dir(&env), Some(support.join("justeli5")));
    }

    #[test]
    fn mac_without_home_has_no_location() {
        let env = env(Os::Mac, &[("APPDATA", "/x"), ("LOCALAPPDATA", "/y")]);
        assert_eq!(default_config_path(&env), None);
        assert_eq!(default_data_dir(&env), None);
    }

    #[test]
    fn linux_prefers_xdg_directories() {
        let env = env(
            Os::Linux,
            &[
                ("HOME", "/h"),
                ("XDG_CONFIG_HOME", "/xc"),
                ("XDG_DATA_HOME", "/xd"),
            ],
        );
        assert_eq!(
            default_config_path(&env),
            Some(PathBuf::from("/xc/justeli5/config.json"))
        );
        assert_eq!(default_data_dir(&env), Some(PathBuf::from("/xd/justeli5")));
    }

    #[test]
    fn linux_falls_back_to_home() {
        let env = env(Os::Linux, &[("HOME", "/h")]);
        assert_eq!(
            default_config_path(&env),
            Some(PathBuf::from("/h/.config/justeli5/config.json"))
        );
        assert_eq!(
            default_data_dir(&env),
            Some(PathBuf::from("/h/.local/share/justeli5"))
        );
    }

    #[test]
    fn linux_empty_xdg_counts_as_unset() {
        let env = env(
            Os::Linux,
            &[
                ("HOME", "/h"),
                ("XDG_CONFIG_HOME", ""),
                ("XDG_DATA_HOME", ""),
            ],
        );
        assert_eq!(
            default_config_path(&env),
            Some(PathBuf::from("/h/.config/justeli5/config.json"))
        );
        assert_eq!(
            default_data_dir(&env),
            Some(PathBuf::from("/h/.local/share/justeli5"))
        );
    }

    #[test]
    fn linux_without_home_or_xdg_has_no_location() {
        let env = env(Os::Linux, &[("MC_DATA_DIR", "/x")]);
        assert_eq!(default_config_path(&env), None);
        assert_eq!(default_data_dir(&env), None);
        let xdg_only = env_xdg_only();
        assert!(default_config_path(&xdg_only).is_some());
        assert!(default_data_dir(&xdg_only).is_some());
    }

    fn env_xdg_only() -> Env {
        env(
            Os::Linux,
            &[("XDG_CONFIG_HOME", "/xc"), ("XDG_DATA_HOME", "/xd")],
        )
    }

    #[test]
    fn default_config_file_is_read_from_the_default_location() {
        let s = Sandbox::new();
        let env = s.env_without(&["MC_DATA_DIR"]);
        let config_file = s.path("home/.config/justeli5/config.json");
        write_json(&config_file, &json!({"run": {"debounce_s": 9.0}}));
        let cfg = load(&env, &[]).unwrap();
        assert_eq!(cfg.path.as_deref(), Some(config_file.as_path()));
        assert!(cfg.file_found);
        assert_eq!(cfg.run.debounce_s, 9.0);
        assert_eq!(source(&cfg, "run.debounce_s"), Some("file"));
        assert_eq!(cfg.data_dir, s.path("home/.local/share/justeli5"));
    }

    #[test]
    fn missing_default_config_file_means_defaults() {
        let s = Sandbox::new();
        let env = s.env_without(&["MC_DATA_DIR"]);
        let cfg = load(&env, &[]).unwrap();
        assert!(!cfg.file_found);
        assert_eq!(cfg.run.debounce_s, ReconcilerConfig::default().debounce_s);
    }
}

// ------------------------------------------------------ data_dir

mod data_dir {
    use super::*;

    #[test]
    fn precedence_flag_env_file_default() {
        let s = Sandbox::new();
        let file_data = s.path("file-data");
        let env_data = s.path("env-data");
        let flag_data = s.path("flag-data");
        let config = write_json(
            &s.path("config.json"),
            &json!({"data_dir": file_data.display().to_string()}),
        );
        let config_s = path_str(&config);
        let base = s.env_without(&["MC_DATA_DIR"]);

        // default: no variable, no file, no flag
        let no_file = s.path("none.json");
        let cfg = load(&base, &["--config", path_str(&no_file)]);
        // A missing explicit file is an error, so use the HOME default instead.
        assert!(cfg.is_err());
        let cfg = load(&base, &[]).unwrap();
        assert_eq!(cfg.data_dir, s.path("home/.local/share/justeli5"));
        assert_eq!(source(&cfg, "data_dir"), Some("default"));

        // file over default
        let cfg = load(&base, &["--config", config_s]).unwrap();
        assert_eq!(cfg.data_dir, file_data);
        assert_eq!(source(&cfg, "data_dir"), Some("file"));

        // env over file
        let with_env = s.env_with(&[("MC_DATA_DIR", path_str(&env_data))]);
        let cfg = load(&with_env, &["--config", config_s]).unwrap();
        assert_eq!(cfg.data_dir, env_data);
        assert_eq!(source(&cfg, "data_dir"), Some("env"));

        // flag over env over file (both spellings of the flag)
        let cfg = load(
            &with_env,
            &["--config", config_s, "--data-dir", path_str(&flag_data)],
        )
        .unwrap();
        assert_eq!(cfg.data_dir, flag_data);
        assert_eq!(source(&cfg, "data_dir"), Some("flag"));
        let inline = format!("--data-dir={}", flag_data.display());
        let cfg = load(&with_env, &["--config", config_s, &inline]).unwrap();
        assert_eq!(cfg.data_dir, flag_data);
    }

    #[test]
    fn empty_env_value_is_unset() {
        let s = Sandbox::new();
        let env = s.env_with(&[("MC_DATA_DIR", "")]);
        let cfg = load(&env, &[]).unwrap();
        assert_eq!(cfg.data_dir, s.path("home/.local/share/justeli5"));
    }

    #[test]
    fn data_dir_without_any_source_is_config_error_only_when_needed() {
        let s = Sandbox::new();
        let env = s.env_without(&["MC_DATA_DIR", "HOME"]);
        let err = load(&env, &[]).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.message().contains("MC_DATA_DIR"), "{}", err.message());

        // Commands that never touch the store (replay) still load a config.
        let cfg = Config::load(&env, &parsed(&[]), false).unwrap();
        assert!(cfg.data_dir.as_os_str().is_empty());
    }

    #[test]
    fn output_dir_precedence_and_defaults() {
        let s = Sandbox::new();
        let file_out = s.path("file-out");
        let config = write_json(
            &s.path("config.json"),
            &json!({"output_dir": file_out.display().to_string()}),
        );
        let config_s = path_str(&config);
        let env = s.env_without(&["MC_OUTPUT_DIR"]);

        let cfg = load(&env, &["--config", config_s]).unwrap();
        assert_eq!(cfg.output_dir, file_out);
        assert_eq!(source(&cfg, "output_dir"), Some("file"));

        let with_env = s.env_with(&[("MC_OUTPUT_DIR", path_str(&s.path("env-out")))]);
        let cfg = load(&with_env, &["--config", config_s]).unwrap();
        assert_eq!(cfg.output_dir, s.path("env-out"));
        assert_eq!(source(&cfg, "output_dir"), Some("env"));

        // default: HOME/Documents/justeli5
        let cfg = load(&env, &[]).unwrap();
        assert_eq!(cfg.output_dir, s.path("home/Documents/justeli5"));
        assert_eq!(source(&cfg, "output_dir"), Some("default"));

        // no HOME: the exports directory under the data directory
        let no_home = s.env_without(&["MC_OUTPUT_DIR", "HOME"]);
        let cfg = load(&no_home, &[]).unwrap();
        assert_eq!(cfg.output_dir, s.data().join("exports"));
    }

    #[test]
    fn windows_default_output_uses_userprofile() {
        let env = env(
            Os::Windows,
            &[
                ("MC_DATA_DIR", "/data"),
                ("USERPROFILE", r"C:\Users\me"),
                ("HOME", "/unused"),
            ],
        );
        let cfg = load(&env, &[]).unwrap();
        assert_eq!(
            cfg.output_dir,
            PathBuf::from(r"C:\Users\me")
                .join("Documents")
                .join("justeli5")
        );
    }

    #[test]
    fn mac_default_output_is_documents() {
        let env = env(Os::Mac, &[("HOME", "/Users/me"), ("MC_DATA_DIR", "/d")]);
        let cfg = load(&env, &[]).unwrap();
        assert_eq!(
            cfg.output_dir,
            PathBuf::from("/Users/me/Documents/justeli5")
        );
    }
}

// ------------------------------------------------------ run settings

mod run_settings {
    use super::*;

    fn base(s: &Sandbox) -> Env {
        s.env_without(&["MC_DATA_DIR", "MC_DEBOUNCE_S", "MC_FLUSH", "MC_POLICY"])
    }

    #[test]
    fn defaults_come_from_the_reconciler() {
        let s = Sandbox::new();
        let cfg = load(&base(&s), &[]).unwrap();
        let defaults = ReconcilerConfig::default();
        assert_eq!(cfg.run.debounce_s, defaults.debounce_s);
        assert_eq!(cfg.run.merge_similarity, defaults.merge_similarity);
        assert_eq!(cfg.run.recent_capacity, defaults.recent_capacity);
        assert_eq!(cfg.run.policy, defaults.policy);
        assert_eq!(cfg.run.flush, FlushMode::Auto);
        assert_eq!(source(&cfg, "run.debounce_s"), Some("default"));
    }

    #[test]
    fn file_values_are_used_and_recorded() {
        let s = Sandbox::new();
        let config = write_json(
            &s.path("config.json"),
            &json!({"run": {
                "debounce_s": 3.0,
                "merge_similarity": 0.5,
                "recent_capacity": 7,
                "policy": "v1_legacy",
                "flush": "manifest-only"
            }}),
        );
        let cfg = load(&base(&s), &["--config", path_str(&config)]).unwrap();
        assert_eq!(cfg.run.debounce_s, 3.0);
        assert_eq!(cfg.run.merge_similarity, 0.5);
        assert_eq!(cfg.run.recent_capacity, 7);
        assert_eq!(cfg.run.policy, Policy::V1Legacy);
        assert_eq!(cfg.run.flush, FlushMode::ManifestOnly);
        assert_eq!(source(&cfg, "run.flush"), Some("file"));
        assert_eq!(source(&cfg, "run.merge_similarity"), Some("file"));
    }

    #[test]
    fn debounce_precedence_flag_env_file_default() {
        let s = Sandbox::new();
        let config = write_json(&s.path("config.json"), &json!({"run": {"debounce_s": 3.0}}));
        let config_s = path_str(&config);
        let with_env = s.env_with(&[
            ("MC_DATA_DIR", path_str(&s.data())),
            ("MC_DEBOUNCE_S", "4.5"),
        ]);

        let cfg = load(&with_env, &["--config", config_s]).unwrap();
        assert_eq!(cfg.run.debounce_s, 4.5);
        assert_eq!(source(&cfg, "run.debounce_s"), Some("env"));

        let mut cfg = load(&with_env, &["--config", config_s]).unwrap();
        cfg.apply_run_flags(&parsed(&["--debounce", "6"])).unwrap();
        assert_eq!(cfg.run.debounce_s, 6.0);
        assert_eq!(source(&cfg, "run.debounce_s"), Some("flag"));
        assert_eq!(cfg.run_config().reconciler.debounce_s, 6.0);

        let cfg = load(
            &s.env_with(&[("MC_DATA_DIR", path_str(&s.data()))]),
            &["--config", config_s],
        )
        .unwrap();
        assert_eq!(cfg.run.debounce_s, 3.0);
        assert_eq!(source(&cfg, "run.debounce_s"), Some("file"));
    }

    #[test]
    fn empty_debounce_env_is_unset() {
        let s = Sandbox::new();
        let env = s.env_with(&[("MC_DEBOUNCE_S", "")]);
        let cfg = load(&env, &[]).unwrap();
        assert_eq!(cfg.run.debounce_s, ReconcilerConfig::default().debounce_s);
        assert_eq!(source(&cfg, "run.debounce_s"), Some("default"));
    }

    #[test]
    fn flush_precedence_flag_env_file_default() {
        let s = Sandbox::new();
        let config = write_json(
            &s.path("config.json"),
            &json!({"run": {"flush": "manifest-only"}}),
        );
        let config_s = path_str(&config);
        let with_env = s.env_with(&[("MC_DATA_DIR", path_str(&s.data())), ("MC_FLUSH", "auto")]);

        let cfg = load(&with_env, &["--config", config_s]).unwrap();
        assert_eq!(cfg.run.flush, FlushMode::Auto);
        assert_eq!(source(&cfg, "run.flush"), Some("env"));

        let mut cfg = load(&with_env, &["--config", config_s]).unwrap();
        cfg.apply_run_flags(&parsed(&["--flush", "manifest-only"]))
            .unwrap();
        assert_eq!(cfg.run.flush, FlushMode::ManifestOnly);
        assert_eq!(source(&cfg, "run.flush"), Some("flag"));
    }

    #[test]
    fn policy_from_env_is_recorded() {
        let s = Sandbox::new();
        let env = s.env_with(&[("MC_POLICY", "v1_legacy")]);
        let cfg = load(&env, &[]).unwrap();
        assert_eq!(cfg.run.policy, Policy::V1Legacy);
        assert_eq!(source(&cfg, "run.policy"), Some("env"));
        assert_eq!(cfg.policy_label(), "v1_legacy");
    }

    #[test]
    fn invalid_values_from_env_and_flags_are_rejected() {
        let s = Sandbox::new();
        let bad_env = s.env_with(&[("MC_DEBOUNCE_S", "soon")]);
        let err = load(&bad_env, &[]).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.message().contains("MC_DEBOUNCE_S"), "{}", err.message());

        let bad_flush = s.env_with(&[("MC_FLUSH", "sometimes")]);
        assert_eq!(load(&bad_flush, &[]).unwrap_err().exit_code(), 2);

        let mut cfg = load(&base(&s), &[]).unwrap();
        let err = cfg
            .apply_run_flags(&parsed(&["--debounce", "soon"]))
            .unwrap_err();
        assert_eq!(err.exit_code(), 2);
        let err = cfg
            .apply_run_flags(&parsed(&["--flush", "sometimes"]))
            .unwrap_err();
        assert_eq!(err.exit_code(), 2);
        let err = cfg
            .apply_run_flags(&parsed(&["--debounce", "0"]))
            .unwrap_err();
        assert_eq!(err.exit_code(), 2);
    }
}

// ------------------------------------------------------ file validation

mod file_validation {
    use super::*;

    /// Runs `mc doctor --config` on a file with this body, with the sandbox
    /// environment, and returns the outcome.
    fn doctor_on(s: &Sandbox, body: &str) -> Out {
        let path = s.path("config.json");
        fs::write(&path, body).unwrap();
        s.mc(&["status", "--config", path_str(&path), "--json"])
    }

    fn doctor_on_value(s: &Sandbox, value: Value) -> Out {
        doctor_on(s, &value.to_string())
    }

    #[test]
    fn malformed_json_is_config_error() {
        let s = Sandbox::new();
        let out = doctor_on(&s, "{ not json");
        assert_json_error(&out, 2, "config");
        assert!(
            out.err_json()["error"]["message"]
                .as_str()
                .unwrap()
                .contains("config.json")
        );
    }

    #[test]
    fn unknown_top_level_key_is_config_error() {
        let s = Sandbox::new();
        assert_json_error(&doctor_on_value(&s, json!({"datadir": "/x"})), 2, "config");
    }

    #[test]
    fn unknown_nested_keys_are_config_errors() {
        let s = Sandbox::new();
        assert_json_error(
            &doctor_on_value(&s, json!({"run": {"debounce": 1}})),
            2,
            "config",
        );
        assert_json_error(
            &doctor_on_value(&s, json!({"tisty": {"cmd": "x"}})),
            2,
            "config",
        );
    }

    #[test]
    fn unknown_policy_is_config_error() {
        let s = Sandbox::new();
        let out = doctor_on_value(&s, json!({"run": {"policy": "v9"}}));
        assert_json_error(&out, 2, "config");
        assert!(
            out.err_json()["error"]["message"]
                .as_str()
                .unwrap()
                .contains("unknown policy")
        );
    }

    #[test]
    fn bad_template_placeholder_is_config_error() {
        let s = Sandbox::new();
        let out = doctor_on_value(&s, json!({"tisty": {"prompt_template": "hi {nope}"}}));
        assert_json_error(&out, 2, "config");
        assert!(
            out.err_json()["error"]["message"]
                .as_str()
                .unwrap()
                .contains("{nope}")
        );
    }

    #[test]
    fn known_placeholders_are_accepted() {
        let s = Sandbox::new();
        let template = "{session_id} {meeting_name} {date} {transcript_md} {commitments_json}";
        let out = doctor_on_value(
            &s,
            json!({"version": 1, "tisty": {"prompt_template": template}}),
        );
        assert_eq!(out.code, 0, "{}", out.stderr);
    }

    #[test]
    fn non_positive_debounce_is_config_error() {
        let s = Sandbox::new();
        assert_json_error(
            &doctor_on_value(&s, json!({"run": {"debounce_s": 0}})),
            2,
            "config",
        );
        assert_json_error(
            &doctor_on_value(&s, json!({"run": {"debounce_s": -1.5}})),
            2,
            "config",
        );
    }

    #[test]
    fn merge_similarity_outside_unit_interval_is_config_error() {
        let s = Sandbox::new();
        assert_json_error(
            &doctor_on_value(&s, json!({"run": {"merge_similarity": 1.5}})),
            2,
            "config",
        );
        assert_json_error(
            &doctor_on_value(&s, json!({"run": {"merge_similarity": 0}})),
            2,
            "config",
        );
        let ok = doctor_on_value(&s, json!({"run": {"merge_similarity": 1}}));
        assert_eq!(ok.code, 0, "{}", ok.stderr);
    }

    #[test]
    fn other_invalid_run_values_are_config_errors() {
        let s = Sandbox::new();
        assert_json_error(
            &doctor_on_value(&s, json!({"run": {"recent_capacity": 0}})),
            2,
            "config",
        );
        assert_json_error(
            &doctor_on_value(&s, json!({"run": {"flush": "sometimes"}})),
            2,
            "config",
        );
        assert_json_error(&doctor_on_value(&s, json!({"data_dir": 5})), 2, "config");
    }

    #[test]
    fn human_error_names_the_command() {
        let s = Sandbox::new();
        let path = s.path("config.json");
        fs::write(&path, "{").unwrap();
        let out = s.mc(&["status", "--config", path_str(&path)]);
        assert_eq!(out.code, 2);
        assert!(out.stderr.starts_with("eli5 status:"), "{}", out.stderr);
    }
}

// ------------------------------------------------------ explicit files

mod explicit_config {
    use super::*;

    #[test]
    fn explicit_missing_file_is_config_error() {
        let s = Sandbox::new();
        let missing = s.path("missing.json");
        let out = s.mc(&["status", "--config", path_str(&missing), "--json"]);
        assert_json_error(&out, 2, "config");
        assert!(
            out.err_json()["error"]["message"]
                .as_str()
                .unwrap()
                .contains("file not found")
        );
    }

    #[test]
    fn mc_config_variable_must_exist_too() {
        let s = Sandbox::new();
        let missing = s.path("missing.json");
        let env = s.env_with(&[("MC_CONFIG", path_str(&missing))]);
        assert_json_error(&mc(&env, &["status", "--json"]), 2, "config");
    }

    #[test]
    fn flag_overrides_mc_config() {
        let s = Sandbox::new();
        let good = write_json(&s.path("good.json"), &json!({"run": {"debounce_s": 2.0}}));
        let env = s.env_with(&[("MC_CONFIG", path_str(&s.path("missing.json")))]);
        let out = mc(&env, &["doctor", "--config", path_str(&good), "--json"]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert!(
            out.json()["checks"][0]["detail"]
                .as_str()
                .unwrap()
                .contains("found")
        );
    }

    #[test]
    fn absent_default_file_is_not_an_error() {
        let s = Sandbox::new();
        let out = s.mc(&["doctor", "--json"]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        let detail = out.json()["checks"][0]["detail"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(detail.contains("not found, using defaults"), "{detail}");
    }
}

// ------------------------------------------------------ tisty settings

mod tisty {
    use super::*;

    #[test]
    fn command_from_file_is_looked_up_on_path() {
        let s = Sandbox::with_os(Os::current());
        let bin = s.path("bin");
        let mytisty = fake_command(&bin, "mytisty");
        let config = write_json(
            &s.path("config.json"),
            &json!({"tisty": {"command": "mytisty"}}),
        );
        let env = s.env_with(&[("PATH", path_str(&bin))]);
        let out = mc(&env, &["doctor", "--config", path_str(&config), "--json"]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        let tisty = out.json()["checks"][4].clone();
        assert_eq!(tisty["name"], "tisty");
        assert_eq!(tisty["status"], "ok");
        assert!(same_path(tisty["detail"].as_str().unwrap(), &mytisty));
    }

    #[test]
    fn defaults_are_tisty_mcp() {
        let s = Sandbox::new();
        let cfg = load(&s.env, &[]).unwrap();
        assert_eq!(cfg.tisty.command, "tisty");
        assert_eq!(cfg.tisty.args, vec!["mcp".to_string()]);
        assert!(cfg.tisty.prompt_template.is_none());
        assert_eq!(cfg.tisty.folder, "Reuniones");
        assert_eq!(cfg.tisty.list, None);
        assert!(cfg.tisty.tags.is_empty());
        assert!(cfg.tisty.ask);
    }

    #[test]
    fn publishing_settings_come_from_the_file() {
        let s = Sandbox::new();
        let config = write_json(
            &s.path("config.json"),
            &json!({"tisty": {"folder": " Notas ", "list": "Trabajo", "tags": ["x"], "ask": false}}),
        );
        let env = s.env_with(&[("MC_CONFIG", path_str(&config))]);
        let cfg = load(&env, &[]).unwrap();
        assert_eq!(cfg.tisty.folder, "Notas");
        assert_eq!(cfg.tisty.list.as_deref(), Some("Trabajo"));
        assert_eq!(cfg.tisty.tags, vec!["x".to_string()]);
        assert!(!cfg.tisty.ask);
    }

    #[test]
    fn an_empty_folder_is_a_config_error() {
        let s = Sandbox::new();
        let config = write_json(&s.path("config.json"), &json!({"tisty": {"folder": " "}}));
        let env = s.env_with(&[("MC_CONFIG", path_str(&config))]);
        assert!(load(&env, &[]).is_err());
    }
}

// ------------------------------------------------------ llm settings

mod llm {
    use super::*;

    fn with_file(s: &Sandbox, value: Value) -> Env {
        let config = write_json(&s.path("llm-config.json"), &value);
        s.env_with(&[("MC_CONFIG", path_str(&config))])
    }

    fn llm_check(out: &Out) -> Value {
        out.json()["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "llm")
            .cloned()
            .expect("an llm check")
    }

    #[test]
    fn defaults_to_the_claude_cli() {
        let s = Sandbox::new();
        let cfg = load(&s.env, &[]).unwrap();
        assert_eq!(cfg.llm.provider, "claude-cli");
        assert!(cfg.llm.model.is_none());
        assert_eq!(source(&cfg, "llm.provider"), Some("default"));
        assert_eq!(source(&cfg, "llm.model"), None);
    }

    #[test]
    fn provider_and_model_precedence_flag_env_file_default() {
        let s = Sandbox::new();
        let file = json!({"llm": {"provider": "anthropic", "model": "from-file", "timeout_s": 30}});
        let env = with_file(&s, file);
        let cfg = load(&env, &[]).unwrap();
        assert_eq!(cfg.llm.provider, "anthropic");
        assert_eq!(cfg.llm.model.as_deref(), Some("from-file"));
        assert_eq!(cfg.llm.timeout_s, 30);
        assert_eq!(source(&cfg, "llm.provider"), Some("file"));

        let mut vars = env.clone();
        vars.vars
            .insert("MC_LLM_PROVIDER".into(), "openai-compatible".into());
        vars.vars.insert("MC_LLM_MODEL".into(), "from-env".into());
        let cfg = load(&vars, &[]).unwrap();
        assert_eq!(cfg.llm.provider, "openai-compatible");
        assert_eq!(cfg.llm.model.as_deref(), Some("from-env"));
        assert_eq!(source(&cfg, "llm.model"), Some("env"));

        let flags = ["--llm-provider", "copilot-cli", "--llm-model", "from-flag"];
        let cfg = load(&vars, &flags).unwrap();
        assert_eq!(cfg.llm.provider, "copilot-cli");
        assert_eq!(cfg.llm.model.as_deref(), Some("from-flag"));
        assert_eq!(source(&cfg, "llm.provider"), Some("flag"));
    }

    #[test]
    fn unknown_keys_are_config_errors() {
        let s = Sandbox::new();
        let env = with_file(&s, json!({"llm": {"nope": 1}}));
        assert!(load(&env, &[]).is_err());
    }

    #[test]
    fn a_bad_llm_section_loads_and_only_fails_where_a_client_is_built() {
        let s = Sandbox::new();
        for file in [
            json!({"llm": {"provider": "gpt-9000"}}),
            json!({"llm": {"provider": "command"}}),
        ] {
            let env = with_file(&s, file.clone());
            let cfg = load(&env, &[]).expect("loading does not validate the llm section");
            assert!(cfg.checked_llm().is_err(), "{file}");
        }
        let mut env = s.env.clone();
        env.vars.insert("MC_LLM_PROVIDER".into(), "bogus".into());
        let cfg = load(&env, &[]).unwrap();
        assert!(cfg.checked_llm().is_err());
        assert!(load(&s.env, &[]).unwrap().checked_llm().is_ok());
        // Zero is no limit, not a mistake.
        let unlimited = with_file(&s, json!({"llm": {"timeout_s": 0}}));
        assert!(load(&unlimited, &[]).unwrap().checked_llm().is_ok());
    }

    #[test]
    fn doctor_fails_the_llm_check_but_other_commands_still_run() {
        let s = Sandbox::with_os(Os::current());
        let config = write_json(
            &s.path("config.json"),
            &json!({"llm": {"provider": "gpt-9000"}}),
        );
        let out = mc(&s.env, &["doctor", "--config", path_str(&config), "--json"]);
        assert_eq!(out.code, 1);
        let check = llm_check(&out);
        assert_eq!(check["status"], "FAIL");
        assert!(check["detail"].as_str().unwrap().contains("gpt-9000"));
        let status = mc(&s.env, &["status", "--config", path_str(&config), "--json"]);
        assert_eq!(status.code, 0, "{}", status.stderr);
    }

    #[test]
    fn doctor_flags_reach_the_llm_settings() {
        let s = Sandbox::with_os(Os::current());
        let out = mc(
            &s.env,
            &[
                "doctor",
                "--llm-provider",
                "fake",
                "--llm-model",
                "m1",
                "--json",
            ],
        );
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert_eq!(llm_check(&out)["detail"], "fake (m1)");
        assert!(
            out.json()["checks"][0]["detail"]
                .as_str()
                .unwrap()
                .contains("llm.provider=flag")
        );
    }

    #[test]
    fn doctor_warns_about_a_missing_api_key_variable() {
        let s = Sandbox::with_os(Os::current());
        let config = write_json(
            &s.path("config.json"),
            &json!({"llm": {"provider": "anthropic", "api_key_env": "MC_TEST_KEY"}}),
        );
        let out = mc(&s.env, &["doctor", "--config", path_str(&config), "--json"]);
        let missing = llm_check(&out);
        assert_eq!(missing["status"], "warn");
        assert!(missing["detail"].as_str().unwrap().contains("MC_TEST_KEY"));
        let env = s.env_with(&[("MC_TEST_KEY", "secret")]);
        let out = mc(&env, &["doctor", "--config", path_str(&config), "--json"]);
        assert_eq!(llm_check(&out)["status"], "ok");
    }

    #[test]
    fn doctor_resolves_a_command_path_like_the_runtime() {
        let s = Sandbox::with_os(Os::current());
        let bin = s.path("bin");
        let tool = fake_command(&bin, "mytool");
        for (command, status) in [
            (tool.display().to_string(), "ok"),
            (s.path("bin").join("nothing").display().to_string(), "warn"),
        ] {
            let config = write_json(
                &s.path("config.json"),
                &json!({"llm": {"provider": "command", "command": command}}),
            );
            let out = mc(&s.env, &["doctor", "--config", path_str(&config), "--json"]);
            assert_eq!(llm_check(&out)["status"], status, "{command}");
        }
    }

    #[test]
    fn doctor_warns_when_the_cli_is_missing_and_finds_it_on_path() {
        let s = Sandbox::with_os(Os::current());
        let config = write_json(
            &s.path("config.json"),
            &json!({"llm": {"provider": "claude-cli", "command": "myclaude"}}),
        );
        let empty = s.path("empty-bin");
        fs::create_dir_all(&empty).unwrap();
        let env = s.env_with(&[("PATH", path_str(&empty))]);
        let out = mc(&env, &["doctor", "--config", path_str(&config), "--json"]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        let missing = llm_check(&out);
        assert_eq!(missing["status"], "warn");
        assert!(missing["detail"].as_str().unwrap().contains("myclaude"));

        let bin = s.path("bin");
        let myclaude = fake_command(&bin, "myclaude");
        let env = s.env_with(&[("PATH", path_str(&bin))]);
        let out = mc(&env, &["doctor", "--config", path_str(&config), "--json"]);
        let found = llm_check(&out);
        assert_eq!(found["status"], "ok");
        let detail = found["detail"].as_str().unwrap();
        assert!(detail.contains("claude-cli"), "{detail}");
        assert!(same_path(detail.rsplit(": ").next().unwrap(), &myclaude));
    }

    #[test]
    fn doctor_reports_an_http_provider_without_looking_for_a_command() {
        let s = Sandbox::with_os(Os::current());
        let config = write_json(
            &s.path("config.json"),
            &json!({"llm": {"provider": "openai-compatible", "model": "qwen"}}),
        );
        let out = mc(&s.env, &["doctor", "--config", path_str(&config), "--json"]);
        let check = llm_check(&out);
        assert_eq!(check["status"], "ok");
        assert_eq!(check["detail"], "openai-compatible (qwen)");
    }
}

// ------------------------------------------------------ no HOME, no data dir

mod no_data_location {
    use super::*;

    #[test]
    fn missing_home_and_data_dir_fails_import_but_replay_works() {
        let s = Sandbox::new();
        let env = s.env_without(&["MC_DATA_DIR", "HOME"]);

        let imported = mc(&env, &["import", "--json"]);
        assert_json_error(&imported, 2, "config");

        let replayed = mc(
            &env,
            &["replay", path_str(&fixture("synthetic_basic")), "--verify"],
        );
        assert_eq!(replayed.code, 0, "{}", replayed.stderr);
    }

    #[test]
    fn other_store_commands_also_need_a_data_dir() {
        let s = Sandbox::new();
        let env = s.env_without(&["MC_DATA_DIR", "HOME"]);
        assert_eq!(mc(&env, &["status"]).code, 2);
        assert_eq!(mc(&env, &["status"]).code, 2);
        assert_eq!(mc(&env, &["compare", "a", "b"]).code, 2);
    }

    #[test]
    fn data_dir_from_env_is_enough_without_home() {
        let s = Sandbox::new();
        let env = s.env_without(&["HOME"]);
        let dir = s.corpus("synthetic_basic");
        let out = mc(&env, &["import", path_str(&dir)]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert!(s.data().join("index.sqlite").is_file());
    }

    #[test]
    fn windows_without_localappdata_has_no_data_dir() {
        let env = env(Os::Windows, &[("HOME", "/x"), ("USERPROFILE", r"C:\me")]);
        let out = mc(&env, &["import", "--json"]);
        assert_json_error(&out, 2, "config");
    }
}

// ------------------------------------------------------ end to end

mod end_to_end {
    use super::*;

    #[test]
    fn config_file_data_dir_is_used_by_import() {
        let s = Sandbox::new();
        let cfg_data = s.path("cfg-data");
        let config = write_json(
            &s.path("config.json"),
            &json!({"data_dir": cfg_data.display().to_string()}),
        );
        let env = s.env_without(&["MC_DATA_DIR"]);
        let dir = s.corpus("synthetic_basic");
        let out = mc(
            &env,
            &["--config", path_str(&config), "import", path_str(&dir)],
        );
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert!(cfg_data.join("index.sqlite").is_file());
    }

    #[test]
    fn mc_data_dir_wins_over_the_config_file() {
        let s = Sandbox::new();
        let cfg_data = s.path("cfg-data");
        let config = write_json(
            &s.path("config.json"),
            &json!({"data_dir": cfg_data.display().to_string()}),
        );
        let dir = s.corpus("synthetic_basic");
        let out = s.mc(&["--config", path_str(&config), "import", path_str(&dir)]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert!(s.data().join("index.sqlite").is_file());
        assert!(!cfg_data.exists());
    }

    #[test]
    fn doctor_reports_each_layer() {
        let s = Sandbox::new();
        let config = write_json(&s.path("config.json"), &json!({"run": {"debounce_s": 3.0}}));
        let env = s.env_with(&[("MC_FLUSH", "manifest-only")]);
        let out = mc(&env, &["doctor", "--config", path_str(&config), "--json"]);
        assert_eq!(out.code, 0, "{}", out.stderr);
        let detail = out.json()["checks"][0]["detail"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(detail.contains("found"), "{detail}");
        assert!(detail.contains("run.debounce_s=file"), "{detail}");
        assert!(detail.contains("run.flush=env"), "{detail}");
        assert!(detail.contains("data_dir=env"), "{detail}");
    }
}
