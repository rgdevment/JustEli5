use std::time::Duration;

use crate::{FakeLlm, Llm, LlmError, cli, http};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    ClaudeCli,
    CopilotCli,
    Command,
    Anthropic,
    OpenAiCompatible,
    Fake,
}

impl Provider {
    pub const ALL: [Provider; 6] = [
        Provider::ClaudeCli,
        Provider::CopilotCli,
        Provider::Command,
        Provider::Anthropic,
        Provider::OpenAiCompatible,
        Provider::Fake,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Provider::ClaudeCli => "claude-cli",
            Provider::CopilotCli => "copilot-cli",
            Provider::Command => "command",
            Provider::Anthropic => "anthropic",
            Provider::OpenAiCompatible => "openai-compatible",
            Provider::Fake => "fake",
        }
    }

    pub fn parse(name: &str) -> Option<Provider> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LlmConfig {
    pub provider: String,
    pub model: Option<String>,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub base_url: Option<String>,
    /// Empty: `ANTHROPIC_API_KEY` or `OPENAI_API_KEY`, by provider.
    pub api_key_env: String,
    pub timeout_s: u64,
    /// `openai-compatible` only: send `response_format: json_object` (some servers reject it).
    pub json_mode: bool,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: Provider::ClaudeCli.name().to_string(),
            model: None,
            command: None,
            args: Vec::new(),
            base_url: None,
            api_key_env: String::new(),
            timeout_s: 180,
            json_mode: false,
        }
    }
}

impl LlmConfig {
    /// The one place that says whether the settings can build a client.
    pub fn validate(&self) -> Result<Provider, LlmError> {
        let provider = Provider::parse(&self.provider).ok_or_else(|| {
            let known: Vec<_> = Provider::ALL.iter().map(|p| p.name()).collect();
            LlmError::Config(format!(
                "unknown provider {:?} (known: {})",
                self.provider,
                known.join(", ")
            ))
        })?;
        if provider == Provider::Command && self.command.is_none() {
            return Err(LlmError::Config(
                "provider `command` needs `command`".into(),
            ));
        }
        Ok(provider)
    }

    /// The executable a CLI provider runs; none for the other providers.
    pub fn command_name(&self) -> Option<&str> {
        let command = self.command.as_deref();
        match Provider::parse(&self.provider)? {
            Provider::ClaudeCli => Some(command.unwrap_or("claude")),
            Provider::CopilotCli => Some(command.unwrap_or("copilot")),
            Provider::Command => command,
            _ => None,
        }
    }

    /// The environment variable holding the API key, when the provider needs or was told to use one.
    pub fn key_env(&self) -> Option<String> {
        let explicit = (!self.api_key_env.is_empty()).then(|| self.api_key_env.clone());
        match Provider::parse(&self.provider)? {
            Provider::Anthropic => Some(explicit.unwrap_or_else(|| "ANTHROPIC_API_KEY".into())),
            Provider::OpenAiCompatible => explicit,
            _ => None,
        }
    }
}

pub(crate) fn api_key(env: &str) -> Option<String> {
    std::env::var(env).ok().filter(|k| !k.trim().is_empty())
}

pub fn from_config(config: &LlmConfig) -> Result<Box<dyn Llm>, LlmError> {
    let provider = config.validate()?;
    let timeout = match config.timeout_s {
        // No limit: a month, which no call reaches and no deadline overflows.
        0 => Duration::from_secs(30 * 24 * 3600),
        secs => Duration::from_secs(secs),
    };
    let model = config.model.as_deref();
    let command = config.command.as_deref();
    Ok(match provider {
        Provider::ClaudeCli => {
            Box::new(cli::ClaudeCli::new(command, model, &config.args, timeout)?)
        }
        Provider::CopilotCli => Box::new(cli::CommandLlm::copilot(
            command,
            model,
            &config.args,
            timeout,
        )?),
        Provider::Command => Box::new(cli::CommandLlm::new(
            "command",
            command.unwrap_or_default(),
            &config.args,
            "set `llm.command` to an installed program or a full path",
            timeout,
        )?),
        Provider::Anthropic => {
            let env = config.key_env().unwrap_or_default();
            let key = api_key(&env).ok_or(LlmError::Auth { env })?;
            Box::new(http::Anthropic::new(
                config.base_url.as_deref(),
                model,
                key,
                timeout,
            ))
        }
        Provider::OpenAiCompatible => {
            let env = config.key_env().unwrap_or_else(|| "OPENAI_API_KEY".into());
            Box::new(http::OpenAiCompatible::new(
                config.base_url.as_deref(),
                model,
                api_key(&env),
                config.json_mode,
                timeout,
            ))
        }
        // The scripted answers of a fake come from `args`.
        Provider::Fake => Box::new(FakeLlm::new(config.args.iter().cloned())),
    })
}
