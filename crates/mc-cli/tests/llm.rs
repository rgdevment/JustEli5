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
    let copilot = ok(&sb, &["llm", "models", "--llm-provider", "copilot-cli"]);
    assert_eq!(copilot["models"], json!([]));
    assert_eq!(sb.mc(&["llm", "models", "--llm-provider", "nope"]).code, 2);
}
