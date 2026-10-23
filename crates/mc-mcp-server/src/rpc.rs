//! JSON-RPC 2.0 over newline-delimited streams: framing and the request loop only.

use std::io::{self, BufRead, Write};

use serde_json::{Value, json};

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
/// MCP: the requested resource does not exist.
pub const RESOURCE_NOT_FOUND: i64 = -32002;

#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(INVALID_PARAMS, message)
    }
}

pub trait Handler {
    fn request(&mut self, method: &str, params: &Value) -> Result<Value, RpcError>;

    fn notification(&mut self, _method: &str, _params: &Value) {}
}

fn error_response(id: &Value, error: &RpcError) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": error.code, "message": error.message}})
}

fn write_message(out: &mut dyn Write, message: &Value) -> io::Result<()> {
    let mut line = message.to_string();
    line.push('\n');
    // One write and a flush per message: the peer reads line by line.
    out.write_all(line.as_bytes())?;
    out.flush()
}

/// Answers one parsed line; `None` for a notification.
fn respond(handler: &mut impl Handler, message: &Value) -> Option<Value> {
    let Some(object) = message.as_object() else {
        let error = RpcError::new(INVALID_REQUEST, "a message must be a JSON object");
        return Some(error_response(&Value::Null, &error));
    };
    let id = object.get("id").filter(|id| !id.is_null());
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        // A response from the peer (we send no requests) is not answered; anything else is garbage.
        let is_response = object.contains_key("result") || object.contains_key("error");
        return id
            .filter(|_| !is_response)
            .map(|id| error_response(id, &RpcError::new(INVALID_REQUEST, "missing method")));
    };
    let params = object.get("params").unwrap_or(&Value::Null);
    let Some(id) = id else {
        handler.notification(method, params);
        return None;
    };
    Some(match handler.request(method, params) {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => error_response(id, &error),
    })
}

/// Reads requests until EOF and writes one response line for each.
pub fn serve(
    mut input: impl BufRead,
    out: &mut dyn Write,
    handler: &mut impl Handler,
) -> io::Result<()> {
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if input.read_until(b'\n', &mut buf)? == 0 {
            return Ok(());
        }
        let text = String::from_utf8_lossy(&buf);
        if text.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&text) {
            Ok(message) => respond(handler, &message),
            Err(e) => Some(error_response(
                &Value::Null,
                &RpcError::new(PARSE_ERROR, format!("invalid JSON: {e}")),
            )),
        };
        if let Some(reply) = reply {
            write_message(out, &reply)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    impl Handler for Echo {
        fn request(&mut self, method: &str, params: &Value) -> Result<Value, RpcError> {
            match method {
                "echo" => Ok(params.clone()),
                _ => Err(RpcError::new(METHOD_NOT_FOUND, "nope")),
            }
        }
    }

    fn run(input: &str) -> Vec<Value> {
        let mut out = Vec::new();
        serve(input.as_bytes(), &mut out, &mut Echo).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn answers_requests_and_ignores_notifications() {
        let replies = run(concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"echo\",\"params\":{\"a\":1}}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            "\n",
            "{\"jsonrpc\":\"2.0\",\"id\":\"x\",\"method\":\"other\"}\n",
        ));
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0]["result"], json!({"a": 1}));
        assert_eq!(replies[1]["id"], "x");
        assert_eq!(replies[1]["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn bad_input_gets_an_error_and_the_loop_goes_on() {
        let replies = run("{oops\n[1]\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"echo\"}\n");
        assert_eq!(replies[0]["error"]["code"], PARSE_ERROR);
        assert_eq!(replies[1]["error"]["code"], INVALID_REQUEST);
        assert_eq!(replies[2]["id"], 2);
    }
}
