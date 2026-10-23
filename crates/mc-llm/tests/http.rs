//! The HTTP providers against a throwaway local server.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

use mc_llm::{Anthropic, Llm, LlmConfig, LlmError, LlmRequest, OpenAiCompatible, from_config};
use serde_json::{Value, json};

struct Captured {
    head: String,
    body: Value,
}

impl Captured {
    fn header(&self, name: &str) -> Option<String> {
        self.head.lines().skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().to_string())
        })
    }
}

/// Serves one request with `status` and `body` (after `delay`), reporting what it received.
fn serve(status: u16, body: String, delay: Duration) -> (String, mpsc::Receiver<Captured>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        let (head, length) = loop {
            let n = stream.read(&mut buf).unwrap();
            raw.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&raw).into_owned();
            if let Some(end) = text.find("\r\n\r\n") {
                let head = text[..end].to_string();
                let length = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                break (head, length);
            }
        };
        let start = head.len() + 4;
        while raw.len() < start + length {
            let n = stream.read(&mut buf).unwrap();
            raw.extend_from_slice(&buf[..n]);
        }
        let body_in = serde_json::from_slice(&raw[start..start + length]).unwrap_or(Value::Null);
        let _ = tx.send(Captured {
            head,
            body: body_in,
        });
        std::thread::sleep(delay);
        let response = format!(
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
    });
    (url, rx)
}

fn request(json: bool) -> LlmRequest {
    LlmRequest {
        system: "be brief".into(),
        user: "hello".into(),
        max_tokens: 64,
        temperature: Some(0.5),
        json,
    }
}

const TIMEOUT: Duration = Duration::from_secs(5);

fn anthropic(url: &str) -> Anthropic {
    Anthropic::new(Some(url), None, "sk-test".into(), TIMEOUT)
}

fn openai(url: &str, key: Option<&str>, timeout: Duration) -> OpenAiCompatible {
    OpenAiCompatible::new(Some(url), None, key.map(String::from), false, timeout)
}

#[test]
fn anthropic_sends_the_messages_shape_and_joins_text_blocks() {
    let reply = json!({"content": [
        {"type": "text", "text": "one "},
        {"type": "tool_use"},
        {"type": "text", "text": "two"}
    ]});
    let (url, rx) = serve(200, reply.to_string(), Duration::ZERO);
    let llm = anthropic(&url);
    assert_eq!(llm.name(), "anthropic (claude-sonnet-5-5)");
    assert_eq!(llm.complete(&request(true)).unwrap(), "one two");

    let seen = rx.recv().unwrap();
    assert!(seen.head.starts_with("POST /v1/messages "), "{}", seen.head);
    assert_eq!(seen.header("x-api-key").as_deref(), Some("sk-test"));
    assert_eq!(
        seen.header("anthropic-version").as_deref(),
        Some("2023-06-01")
    );
    assert_eq!(seen.body["model"], "claude-sonnet-5-5");
    assert_eq!(seen.body["max_tokens"], 64);
    assert_eq!(
        seen.body["messages"][0],
        json!({"role": "user", "content": "hello"})
    );
    assert!(
        seen.body["system"]
            .as_str()
            .unwrap()
            .starts_with("be brief")
    );
    assert!(seen.body["system"].as_str().unwrap().contains("JSON"));
}

#[test]
fn anthropic_without_a_key_is_an_auth_error() {
    let cfg = LlmConfig {
        provider: "anthropic".into(),
        api_key_env: "MC_LLM_TEST_KEY_MISSING".into(),
        ..LlmConfig::default()
    };
    let error = from_config(&cfg).err().unwrap();
    assert!(matches!(&error, LlmError::Auth { env } if env == "MC_LLM_TEST_KEY_MISSING"));
}

#[test]
fn http_errors_carry_status_and_body() {
    let (url, _rx) = serve(429, r#"{"error":"slow down"}"#.into(), Duration::ZERO);
    let llm = anthropic(&url);
    let error = llm.complete(&request(false)).unwrap_err();
    assert!(
        matches!(&error, LlmError::Http { status: 429, body } if body.contains("slow down")),
        "{error}"
    );
}

#[test]
fn a_body_without_text_is_a_bad_response() {
    let (url, _rx) = serve(200, json!({"content": []}).to_string(), Duration::ZERO);
    let llm = anthropic(&url);
    assert!(matches!(
        llm.complete(&request(false)),
        Err(LlmError::BadResponse(_))
    ));
}

#[test]
fn a_slow_server_times_out() {
    let (url, _rx) = serve(200, "{}".into(), Duration::from_secs(4));
    let llm = openai(&url, None, Duration::from_secs(1));
    assert!(matches!(
        llm.complete(&request(false)),
        Err(LlmError::Timeout)
    ));
}

#[test]
fn openai_compatible_asks_for_json_in_the_prompt_and_reads_the_first_choice() {
    let reply = json!({"choices": [{"message": {"role": "assistant", "content": "{\"a\":1}"}}]});
    let (url, rx) = serve(200, reply.to_string(), Duration::ZERO);
    let llm = openai(&format!("{url}/v1"), None, TIMEOUT);
    assert_eq!(llm.complete(&request(true)).unwrap(), "{\"a\":1}");

    let seen = rx.recv().unwrap();
    assert!(
        seen.head.starts_with("POST /v1/chat/completions "),
        "{}",
        seen.head
    );
    assert!(seen.header("authorization").is_none());
    // LM Studio rejects response_format: the instruction rides in the system prompt.
    assert!(seen.body.get("response_format").is_none());
    let system = seen.body["messages"][0]["content"].as_str().unwrap();
    assert!(
        system.starts_with("be brief") && system.contains("JSON"),
        "{system}"
    );
    assert_eq!(
        seen.body["messages"][1],
        json!({"role": "user", "content": "hello"})
    );
}

#[test]
fn openai_compatible_sends_the_bearer_key_when_set() {
    let reply = json!({"choices": [{"message": {"content": "hi"}}]});
    let (url, rx) = serve(200, reply.to_string(), Duration::ZERO);
    let llm = openai(&url, Some("lm-key"), TIMEOUT);
    assert_eq!(llm.complete(&request(false)).unwrap(), "hi");
    let seen = rx.recv().unwrap();
    assert_eq!(
        seen.header("authorization").as_deref(),
        Some("Bearer lm-key")
    );
    assert!(seen.body.get("response_format").is_none());
}

#[test]
fn openai_compatible_json_mode_is_opt_in() {
    let reply = json!({"choices": [{"message": {"content": "{}"}}]});
    let (url, rx) = serve(200, reply.to_string(), Duration::ZERO);
    let llm = OpenAiCompatible::new(Some(&url), None, None, true, TIMEOUT);
    llm.complete(&request(true)).unwrap();
    let seen = rx.recv().unwrap();
    assert_eq!(seen.body["response_format"], json!({"type": "json_object"}));
    let (url, rx) = serve(200, reply.to_string(), Duration::ZERO);
    let llm = OpenAiCompatible::new(Some(&url), None, None, true, TIMEOUT);
    llm.complete(&request(false)).unwrap();
    assert!(rx.recv().unwrap().body.get("response_format").is_none());
}

#[test]
fn a_closed_port_is_a_transport_error() {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let llm = openai(&format!("http://127.0.0.1:{port}"), None, TIMEOUT);
    assert!(matches!(
        llm.complete(&request(false)),
        Err(LlmError::Transport(_))
    ));
}
