//! The model names a provider accepts, so a person picks one instead of guessing it: Claude Code
//! takes the aliases and ids it knows, Copilot CLI and an HTTP server are asked for their own list.

use std::time::Duration;

use serde_json::Value;

use crate::config::{LlmConfig, Provider, api_key};
use crate::error::LlmError;
use crate::http::{ANTHROPIC_DEFAULT_URL, OPENAI_DEFAULT_URL, get};
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

/// Slash commands only exist inside a session; this answers without one, or even a login.
const COPILOT_LIST: [&str; 2] = ["help", "config"];

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

/// The `- "id"` lines under the help's `model` setting.
fn copilot_ids(help: &str) -> Vec<String> {
    help.lines()
        .skip_while(|line| !line.trim_start().starts_with("`model`:"))
        .skip(1)
        .map_while(|line| {
            let id = line.trim().strip_prefix("- \"")?.strip_suffix('"')?;
            Some(id.to_string())
        })
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
    fn copilot_ids_come_from_the_model_setting_of_its_help() {
        let help = "  `logLevel`: log level.\r\n\r\n  `model`: AI model to use.\r\n    - \"gpt-6-astra\"\r\n    - \"claude-opus-5.5\"\r\n\r\n  `contextTier`: tier.\r\n    - \"default\"\r\n";
        assert_eq!(copilot_ids(help), ["gpt-6-astra", "claude-opus-5.5"]);
        assert!(copilot_ids("error: Invalid command format.").is_empty());
    }

    #[test]
    fn ids_come_from_the_data_list() {
        let answer = serde_json::json!({"data": [{"id": "llama3"}, {"name": "x"}, {"id": "qwen"}]});
        assert_eq!(ids(&answer), ["llama3", "qwen"]);
        assert!(ids(&serde_json::json!({})).is_empty());
    }
}
