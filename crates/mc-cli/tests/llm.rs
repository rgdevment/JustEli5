//! `eli5 llm models`: the names the AI provider accepts, for the window to offer.

mod common;

use common::Sandbox;
use serde_json::{Value, json};

fn ok(sb: &Sandbox, args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let out = sb.mc(&all);
    assert_eq!(out.code, 0, "{args:?}: {}", out.stderr);
    out.json()
}

#[test]
fn the_model_list_follows_the_provider() {
    let sb = Sandbox::new();
    let claude = ok(&sb, &["llm", "models", "--llm-provider", "claude-cli"]);
    assert!(
        claude["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m == "opus")
    );
    assert_eq!(sb.mc(&["llm", "models", "--llm-provider", "nope"]).code, 2);
}

#[cfg(unix)]
#[test]
fn copilot_is_asked_for_the_models_its_plan_offers() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    let script = sb.path("copilot");
    let list = r#"[{"id":"gpt-6-astra","name":"GPT-6 Astra","current":true},{"id":"claude-opus-5.5","name":"Claude Opus 5.5"}]"#;
    std::fs::write(
        &script,
        format!("#!/bin/sh\n[ \"$*\" = \"/model --list --json\" ] && echo '{list}'\n"),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let models = |command: &std::path::Path| {
        let config = sb.path("config.json");
        let settings = json!({"llm": {"provider": "copilot-cli", "command": command}});
        std::fs::write(&config, settings.to_string()).unwrap();
        ok(
            &sb,
            &["--config", common::path_str(&config), "llm", "models"],
        )["models"]
            .clone()
    };
    assert_eq!(models(&script), json!(["gpt-6-astra", "claude-opus-5.5"]));
    assert_eq!(models(&sb.path("missing/copilot")), json!([]));
}
