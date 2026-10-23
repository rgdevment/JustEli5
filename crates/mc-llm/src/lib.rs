//! A synchronous, provider-agnostic text-completion client.

mod cli;
mod config;
mod error;
mod fake;
mod http;
mod json;
mod models;
mod process;
mod usage;

pub use cli::{Invocation, build_invocation, claude_args};
pub use config::{LlmConfig, Provider, from_config};
pub use error::LlmError;
pub use fake::FakeLlm;
pub use http::{Anthropic, OpenAiCompatible};
pub use json::extract_json;
pub use models::list_models;
pub use process::{check_argv, find_on_path, resolve_command};
pub use usage::{CallUsage, Meter, usage_json};

const JSON_ONLY: &str =
    "Answer with one valid JSON value and nothing else: no prose, no code fences.";

#[derive(Debug, Clone, PartialEq)]
pub struct LlmRequest {
    pub system: String,
    pub user: String,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
    /// Ask for a JSON-only answer.
    pub json: bool,
}

pub trait Llm: Send {
    fn name(&self) -> String;
    fn complete(&self, req: &LlmRequest) -> Result<String, LlmError>;
    /// The answered calls so far; empty for a client that does not measure them.
    fn usage(&self) -> Vec<CallUsage> {
        Vec::new()
    }
}

/// The system prompt, with the JSON-only instruction when the request asks for it.
pub(crate) fn system_text(req: &LlmRequest) -> String {
    match (req.system.trim(), req.json) {
        (system, false) => system.to_string(),
        ("", true) => JSON_ONLY.to_string(),
        (system, true) => format!("{system}\n\n{JSON_ONLY}"),
    }
}
