//! Providers that call an HTTP API: `anthropic` and `openai-compatible`.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::error::tail;
use crate::{CallUsage, Llm, LlmError, LlmRequest, Meter, system_text};

pub const ANTHROPIC_DEFAULT_MODEL: &str = "claude-sonnet-5-5";
pub const ANTHROPIC_DEFAULT_URL: &str = "https://api.anthropic.com";
pub const OPENAI_DEFAULT_URL: &str = "http://localhost:1234";
const OPENAI_DEFAULT_MODEL: &str = "local-model";

fn endpoint(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    // A base that already ends in /v1 (a common way to write it) is not doubled.
    let path = if base.ends_with("/v1") {
        path.trim_start_matches("/v1")
    } else {
        path
    };
    format!("{base}{path}")
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build()
        .into()
}

fn transport(url: &str, error: ureq::Error) -> LlmError {
    match error {
        ureq::Error::Timeout(_) => LlmError::Timeout,
        other => LlmError::Transport(format!("{url}: {other}")),
    }
}

fn post(
    agent: &ureq::Agent,
    url: &str,
    headers: &[(&str, String)],
    body: &Value,
) -> Result<Value, LlmError> {
    let mut request = agent.post(url).header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    let mut response = request
        .send(body.to_string())
        .map_err(|e| transport(url, e))?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|e| transport(url, e))?;
    if status >= 400 {
        return Err(LlmError::Http {
            status,
            body: tail(&text),
        });
    }
    serde_json::from_str(&text)
        .map_err(|e| LlmError::BadResponse(format!("not JSON ({e}): {}", tail(&text))))
}

/// A GET under `base`, as JSON; for the lists a server publishes.
pub(crate) fn get(
    base: &str,
    path: &str,
    headers: &[(&str, String)],
    timeout: Duration,
) -> Result<Value, LlmError> {
    let url = endpoint(base, path);
    let mut request = agent(timeout).get(&url);
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    let mut response = request.call().map_err(|e| transport(&url, e))?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|e| transport(&url, e))?;
    if status >= 400 {
        return Err(LlmError::Http {
            status,
            body: tail(&text),
        });
    }
    serde_json::from_str(&text)
        .map_err(|e| LlmError::BadResponse(format!("not JSON ({e}): {}", tail(&text))))
}

fn non_empty(answer: String) -> Result<String, LlmError> {
    if answer.trim().is_empty() {
        return Err(LlmError::BadResponse("the model answered nothing".into()));
    }
    Ok(answer)
}

pub struct Anthropic {
    agent: ureq::Agent,
    url: String,
    model: String,
    key: String,
    meter: Meter,
}

impl Anthropic {
    pub fn new(
        base_url: Option<&str>,
        model: Option<&str>,
        key: String,
        timeout: Duration,
    ) -> Self {
        Self {
            agent: agent(timeout),
            url: endpoint(base_url.unwrap_or(ANTHROPIC_DEFAULT_URL), "/v1/messages"),
            model: model.unwrap_or(ANTHROPIC_DEFAULT_MODEL).to_string(),
            key,
            meter: Meter::default(),
        }
    }
}

impl Llm for Anthropic {
    fn name(&self) -> String {
        format!("anthropic ({})", self.model)
    }

    fn usage(&self) -> Vec<CallUsage> {
        self.meter.calls()
    }

    fn complete(&self, req: &LlmRequest) -> Result<String, LlmError> {
        let system = system_text(req);
        let mut body = json!({
            "model": self.model,
            "max_tokens": req.max_tokens,
            "messages": [{"role": "user", "content": req.user}],
        });
        if !system.is_empty() {
            body["system"] = json!(system);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        let headers = [
            ("x-api-key", self.key.clone()),
            ("anthropic-version", "2023-06-01".to_string()),
        ];
        let started = Instant::now();
        let answer = post(&self.agent, &self.url, &headers, &body)?;
        let usage = &answer["usage"];
        let cached = |key: &str| usage[key].as_u64().unwrap_or(0);
        let read = cached("cache_read_input_tokens");
        self.meter.record_cached(
            started,
            usage["input_tokens"]
                .as_u64()
                .map(|n| n + cached("cache_creation_input_tokens") + read),
            usage["input_tokens"].as_u64().map(|_| read),
            usage["output_tokens"].as_u64(),
        );
        let blocks = answer["content"]
            .as_array()
            .ok_or_else(|| LlmError::BadResponse("no `content` in the answer".into()))?;
        non_empty(
            blocks
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect(),
        )
    }
}

pub struct OpenAiCompatible {
    agent: ureq::Agent,
    url: String,
    model: String,
    key: Option<String>,
    json_mode: bool,
    meter: Meter,
}

impl OpenAiCompatible {
    /// `json_mode` adds `response_format: json_object` for servers that accept it.
    pub fn new(
        base_url: Option<&str>,
        model: Option<&str>,
        key: Option<String>,
        json_mode: bool,
        timeout: Duration,
    ) -> Self {
        Self {
            agent: agent(timeout),
            url: endpoint(
                base_url.unwrap_or(OPENAI_DEFAULT_URL),
                "/v1/chat/completions",
            ),
            model: model.unwrap_or(OPENAI_DEFAULT_MODEL).to_string(),
            key,
            json_mode,
            meter: Meter::default(),
        }
    }
}

impl Llm for OpenAiCompatible {
    fn name(&self) -> String {
        format!("openai-compatible ({})", self.model)
    }

    fn usage(&self) -> Vec<CallUsage> {
        self.meter.calls()
    }

    fn complete(&self, req: &LlmRequest) -> Result<String, LlmError> {
        let system = system_text(req);
        let mut messages = Vec::new();
        if !system.is_empty() {
            messages.push(json!({"role": "system", "content": system}));
        }
        messages.push(json!({"role": "user", "content": req.user}));
        let mut body = json!({
            "model": self.model,
            "max_tokens": req.max_tokens,
            "messages": messages,
        });
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        if req.json && self.json_mode {
            body["response_format"] = json!({"type": "json_object"});
        }
        let headers: Vec<_> = self
            .key
            .iter()
            .map(|k| ("authorization", format!("Bearer {k}")))
            .collect();
        let started = Instant::now();
        let answer = post(&self.agent, &self.url, &headers, &body)?;
        self.meter.record(
            started,
            answer["usage"]["prompt_tokens"].as_u64(),
            answer["usage"]["completion_tokens"].as_u64(),
        );
        let text = answer["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| LlmError::BadResponse("no `choices[0].message.content`".into()))?;
        non_empty(text.to_string())
    }
}
