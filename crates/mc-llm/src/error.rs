/// Why a completion failed; every variant says what to fix.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("`{command}` not found: {hint}")]
    NotFound { command: String, hint: String },
    #[error("the model did not answer in time")]
    Timeout,
    #[error("process failed ({}): {stderr}", code.map_or("no exit code".to_string(), |c| format!("exit {c}")))]
    Process { code: Option<i32>, stderr: String },
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("{env} is not set: export your API key there or pick another `llm.api_key_env`")]
    Auth { env: String },
    #[error("unusable answer: {0}")]
    BadResponse(String),
    #[error("cannot reach the server: {0}")]
    Transport(String),
    #[error("llm configuration: {0}")]
    Config(String),
}

const TAIL_BYTES: usize = 2000;

/// The end of a long text (errors put the useful part last), cut on a char boundary.
pub(crate) fn tail(text: &str) -> String {
    let text = text.trim();
    if text.len() <= TAIL_BYTES {
        return text.to_string();
    }
    let mut start = text.len() - TAIL_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("...{}", &text[start..])
}
