//! A small synchronous MCP client over stdio: the server is a child process and
//! JSON-RPC 2.0 messages travel one per line.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

pub const PROTOCOL_VERSION: &str = "2025-06-18";
const STDERR_TAIL: usize = 2000;
const EXIT_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("cannot start {0}")]
    Spawn(String),
    #[error("no answer to {what} within {secs}s")]
    Timeout { what: String, secs: u64 },
    #[error("the server closed the connection{}", tail_note(.stderr))]
    Closed { stderr: String },
    #[error("the server answered {message} (code {code})")]
    Rpc { code: i64, message: String },
    #[error("unexpected answer from the server: {0}")]
    Protocol(String),
}

fn tail_note(stderr: &str) -> String {
    if stderr.trim().is_empty() {
        String::new()
    } else {
        format!(": {}", stderr.trim())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    pub init: Duration,
    pub call: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            init: Duration::from_secs(15),
            call: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    pub name: String,
    pub protocol_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
}

/// What a tool returned: its text parts joined by newlines, its data, and whether it flagged an error.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    pub text: String,
    /// The answer as data (`structuredContent`), when the server sends one beside the text.
    pub structured: Option<Value>,
    pub is_error: bool,
}

pub struct McpClient {
    child: Child,
    /// Lines for the writer thread; dropping it closes the server's stdin.
    outgoing: Option<Sender<String>>,
    messages: Receiver<Value>,
    stderr: Arc<Mutex<String>>,
    next_id: u64,
    timeouts: Timeouts,
}

impl McpClient {
    pub fn spawn(program: &Path, args: &[String], timeouts: Timeouts) -> Result<Self, McpError> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| McpError::Spawn(format!("{}: {e}", program.display())))?;
        let mut stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr_pipe = child.stderr.take().expect("stderr was piped");

        let (tx, messages) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = Vec::new();
            loop {
                line.clear();
                if !matches!(reader.read_until(0x0A, &mut line), Ok(n) if n > 0) {
                    break;
                }
                // Lossy: a line that is not UTF-8 must not end the conversation, and
                // anything that is not JSON (a banner, a log line) is not ours to read.
                let text = String::from_utf8_lossy(&line);
                if let Ok(value) = serde_json::from_str::<Value>(text.trim())
                    && tx.send(value).is_err()
                {
                    break;
                }
            }
        });
        // Writing happens off this thread: a server that stops reading blocks the writer,
        // not the caller, whose wait for the answer is bounded by its timeout.
        let (outgoing, lines) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            for line in lines {
                if writeln!(stdin, "{line}")
                    .and_then(|()| stdin.flush())
                    .is_err()
                {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&stderr);
        std::thread::spawn(move || drain_tail(stderr_pipe, &sink));

        Ok(Self {
            child,
            outgoing: Some(outgoing),
            messages,
            stderr,
            next_id: 1,
            timeouts,
        })
    }

    /// Handshake: `initialize` then `notifications/initialized`.
    pub fn initialize(&mut self) -> Result<ServerInfo, McpError> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "justeli5", "version": env!("CARGO_PKG_VERSION")},
        });
        let result = self.request("initialize", params, self.timeouts.init)?;
        self.notify("notifications/initialized")?;
        Ok(ServerInfo {
            name: result["serverInfo"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            protocol_version: result["protocolVersion"]
                .as_str()
                .unwrap_or(PROTOCOL_VERSION)
                .to_string(),
        })
    }

    pub fn list_tools(&mut self) -> Result<Vec<ToolInfo>, McpError> {
        let result = self.request("tools/list", json!({}), self.timeouts.init)?;
        let tools = result["tools"]
            .as_array()
            .ok_or_else(|| McpError::Protocol("tools/list has no `tools` list".into()))?;
        Ok(tools
            .iter()
            .filter_map(|t| {
                Some(ToolInfo {
                    name: t["name"].as_str()?.to_string(),
                    description: t["description"].as_str().unwrap_or_default().to_string(),
                })
            })
            .collect())
    }

    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Result<ToolOutput, McpError> {
        let params = json!({"name": name, "arguments": arguments});
        let result = self.request(&format!("tools/call {name}"), params, self.timeouts.call)?;
        let text = result["content"]
            .as_array()
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let structured = Some(result["structuredContent"].clone()).filter(|v| !v.is_null());
        Ok(ToolOutput {
            text,
            structured,
            is_error: result["isError"].as_bool().unwrap_or(false),
        })
    }

    /// Ends the session: closes the server's stdin, waits briefly, then kills it.
    pub fn close(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.outgoing = None;
        let deadline = Instant::now() + EXIT_GRACE;
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.kill();
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn send(&mut self, message: &Value) -> Result<(), McpError> {
        let sent = self
            .outgoing
            .as_ref()
            .is_some_and(|tx| tx.send(message.to_string()).is_ok());
        if sent {
            Ok(())
        } else {
            Err(McpError::Closed {
                stderr: self.stderr_tail(),
            })
        }
    }

    fn notify(&mut self, method: &str) -> Result<(), McpError> {
        self.send(&json!({"jsonrpc": "2.0", "method": method}))
    }

    /// `what` is a label for errors; its first word is the JSON-RPC method.
    fn request(&mut self, what: &str, params: Value, timeout: Duration) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        let method = what.split(' ').next().unwrap_or(what);
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let message = match self.messages.recv_timeout(left) {
                Ok(message) => message,
                Err(RecvTimeoutError::Timeout) => {
                    self.kill();
                    return Err(McpError::Timeout {
                        what: what.to_string(),
                        secs: timeout.as_secs(),
                    });
                }
                Err(RecvTimeoutError::Disconnected) => {
                    // Lets the stderr thread read what the server said last.
                    std::thread::sleep(Duration::from_millis(50));
                    return Err(McpError::Closed {
                        stderr: self.stderr_tail(),
                    });
                }
            };
            if message.get("method").is_none() && message["id"] == json!(id) {
                return answer(&message);
            }
            // A request from the server gets a refusal, so it never waits on us.
            if message.get("method").is_some() && message.get("id").is_some() {
                let reply = json!({
                    "jsonrpc": "2.0", "id": message["id"],
                    "error": {"code": -32601, "message": "method not supported"},
                });
                self.send(&reply)?;
            }
        }
    }

    fn stderr_tail(&self) -> String {
        self.stderr.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.shutdown();
        }
    }
}

fn answer(message: &Value) -> Result<Value, McpError> {
    if let Some(error) = message.get("error") {
        return Err(McpError::Rpc {
            code: error["code"].as_i64().unwrap_or(0),
            message: error["message"].as_str().unwrap_or("error").to_string(),
        });
    }
    message
        .get("result")
        .cloned()
        .ok_or_else(|| McpError::Protocol("the answer has neither result nor error".into()))
}

/// Reads to the end, keeping only the last `STDERR_TAIL` bytes.
fn drain_tail(mut pipe: impl Read, sink: &Mutex<String>) {
    let mut chunk = [0u8; 1024];
    while let Ok(n) = pipe.read(&mut chunk) {
        if n == 0 {
            break;
        }
        if let Ok(mut text) = sink.lock() {
            text.push_str(&String::from_utf8_lossy(&chunk[..n]));
            if text.len() > STDERR_TAIL {
                let mut cut = text.len() - STDERR_TAIL;
                while !text.is_char_boundary(cut) {
                    cut += 1;
                }
                text.drain(..cut);
            }
        }
    }
}
