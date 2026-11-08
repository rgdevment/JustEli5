//! The model names a provider accepts, so a person picks one instead of guessing it: Claude Code
//! takes the aliases and ids it knows, Copilot CLI and an HTTP server are asked for their own list.

use std::time::Duration;

use serde_json::Value;

use crate::config::{LlmConfig, Provider, api_key};
use crate::error::LlmError;
use crate::http::{ANTHROPIC_DEFAULT_URL, OPENAI_DEFAULT_URL, get};
use crate::json::extract_json;
use crate::process;

/// Claude Code takes these aliases, which follow the newest model of each size, and full ids.
const CLAUDE_CODE: [&str; 7] = [
    "opus",
    "sonnet",
    "haiku",
    "claude-fable-5-1",
    "claude-opus-5-5",
    "claude-sonnet-5-5",
    "claude-haiku-5-5",
];

const LIST_TIMEOUT: Duration = Duration::from_secs(10);

/// Copilot's catalog depends on the plan and the organisation, so only the CLI itself knows it.
const COPILOT_LIST: [&str; 3] = ["/model", "--list", "--json"];

/// The models `config`'s provider accepts; empty when it cannot say (a command, a copilot that
/// is missing or does not answer).
pub fn list_models(config: &LlmConfig) -> Result<Vec<String>, LlmError> {
    let provider = config.validate()?;
    Ok(match provider {
        Provider::ClaudeCli => CLAUDE_CODE.iter().map(|m| (*m).to_string()).collect(),
        Provider::Anthropic => {
            let env = config.key_env().unwrap_or_default();
            let key = api_key(&env).ok_or(LlmError::Auth { env })?;
            let base = config.base_url.as_deref().unwrap_or(ANTHROPIC_DEFAULT_URL);
            ids(&get(
                base,
                "/v1/models?limit=100",
                &[
                    ("x-api-key", key),
                    ("anthropic-version", "2023-06-01".into()),
                ],
                LIST_TIMEOUT,
            )?)
        }
        Provider::OpenAiCompatible => {
            let env = config.key_env().unwrap_or_else(|| "OPENAI_API_KEY".into());
            let auth: Vec<(&str, String)> = api_key(&env)
                .map(|k| ("authorization", format!("Bearer {k}")))
                .into_iter()
                .collect();
            let base = config.base_url.as_deref().unwrap_or(OPENAI_DEFAULT_URL);
            ids(&get(base, "/v1/models", &auth, LIST_TIMEOUT)?)
        }
        Provider::CopilotCli => copilot_models(config.command_name().unwrap_or("copilot")),
        Provider::Command | Provider::Fake => Vec::new(),
    })
}

fn copilot_models(command: &str) -> Vec<String> {
    let args = COPILOT_LIST.map(String::from);
    process::resolve(command, "")
        .and_then(|program| process::run(&program, &args, "", LIST_TIMEOUT))
        .map(|out| copilot_ids(&out))
        .unwrap_or_default()
}

/// `[{"id": "...", "name": "..."}]`, possibly after a notice.
fn copilot_ids(out: &str) -> Vec<String> {
    let Some(list) = extract_json(out).and_then(|j| serde_json::from_str::<Value>(j).ok()) else {
        return Vec::new();
    };
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["id"].as_str().map(str::to_string))
        .collect()
}

/// `{"data": [{"id": "..."}]}`, the shape both APIs answer with.
fn ids(answer: &Value) -> Vec<String> {
    answer["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["id"].as_str().map(str::to_string))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_needs_no_call_and_a_missing_copilot_lists_nothing() {
        let claude = LlmConfig {
            provider: "claude-cli".into(),
            ..LlmConfig::default()
        };
        assert!(list_models(&claude).unwrap().contains(&"opus".to_string()));
        let copilot = LlmConfig {
            provider: "copilot-cli".into(),
            command: Some("/nonexistent/copilot".into()),
            ..LlmConfig::default()
        };
        assert!(list_models(&copilot).unwrap().is_empty());
    }

    #[test]
    fn copilot_ids_come_from_its_json_list() {
        let out = "notice\n[{\"id\":\"gpt-6-astra\",\"name\":\"GPT-6 Astra\",\"current\":true},{\"id\":\"claude-opus-5.5\"}]\n";
        assert_eq!(copilot_ids(out), ["gpt-6-astra", "claude-opus-5.5"]);
        assert!(copilot_ids("Unknown command").is_empty());
    }

    #[test]
    fn ids_come_from_the_data_list() {
        let answer = serde_json::json!({"data": [{"id": "llama3"}, {"name": "x"}, {"id": "qwen"}]});
        assert_eq!(ids(&answer), ["llama3", "qwen"]);
        assert!(ids(&serde_json::json!({})).is_empty());
    }
}
