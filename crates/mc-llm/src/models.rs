//! The model names a provider accepts, so a person picks one instead of guessing it: the CLI
//! agents take the aliases and ids they know, and an HTTP server is asked for its own list.

use std::time::Duration;

use serde_json::Value;

use crate::config::{LlmConfig, Provider, api_key};
use crate::error::LlmError;
use crate::http::{ANTHROPIC_DEFAULT_URL, OPENAI_DEFAULT_URL, get};

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

/// The models `config`'s provider accepts; empty when it cannot say (copilot, a command).
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
        Provider::CopilotCli | Provider::Command | Provider::Fake => Vec::new(),
    })
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
    fn the_cli_agents_need_no_call_and_unknown_ones_list_nothing() {
        let claude = LlmConfig {
            provider: "claude-cli".into(),
            ..LlmConfig::default()
        };
        assert!(list_models(&claude).unwrap().contains(&"opus".to_string()));
        let copilot = LlmConfig {
            provider: "copilot-cli".into(),
            ..LlmConfig::default()
        };
        assert!(list_models(&copilot).unwrap().is_empty());
    }

    #[test]
    fn ids_come_from_the_data_list() {
        let answer = serde_json::json!({"data": [{"id": "llama3"}, {"name": "x"}, {"id": "qwen"}]});
        assert_eq!(ids(&answer), ["llama3", "qwen"]);
        assert!(ids(&serde_json::json!({})).is_empty());
    }
}
