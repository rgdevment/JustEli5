//! `eli5 llm models`: the names the AI provider accepts, for the window to offer.

mod common;

use common::Sandbox;
use serde_json::Value;

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
fn copilot_is_asked_for_the_models_it_accepts() {
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    let script = sb.path("copilot");
    let help = "  `model`: AI model to use.\n    - \"gpt-6-astra\"\n    - \"claude-opus-5.5\"\n";
    std::fs::write(
        &script,
        format!("#!/bin/sh\n[ \"$*\" = \"help config\" ] && printf '{help}'\n"),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let config = sb.path("config.json");
    let with_command = |command: &std::path::Path| {
        let settings = json!({"llm": {"provider": "copilot-cli", "command": command}});
        std::fs::write(&config, settings.to_string()).unwrap();
    };
    let list = [
        "--json",
        "--config",
        common::path_str(&config),
        "llm",
        "models",
    ];
    with_command(&script);
    let out = sb.mc(&list);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        out.json()["models"],
        json!(["gpt-6-astra", "claude-opus-5.5"])
    );
    with_command(&sb.path("missing/copilot"));
    let missing = sb.mc(&list);
    assert_eq!(missing.code, 3);
    assert!(missing.stderr.contains("not found"), "{}", missing.stderr);
}
