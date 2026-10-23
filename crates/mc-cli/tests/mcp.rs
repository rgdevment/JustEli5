//! `mc mcp` as a real process: JSON lines over pipes, nothing else on stdout.

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

mod common;
use common::{Sandbox, copy_tree, fixture};

fn request(id: u64, method: &str, params: Value) -> String {
    format!(
        "{}\n",
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    )
}

/// Runs `mc mcp --data-dir DIR`, writes `input`, closes stdin and returns (stdout lines, exit ok).
fn run_mcp(sb: &Sandbox, input: &str) -> (Vec<Value>, bool) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mc"))
        .arg("mcp")
        .arg("--data-dir")
        .arg(sb.data())
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mc");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let lines = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|l| {
            serde_json::from_str(l).unwrap_or_else(|e| panic!("stdout line is not JSON ({e}): {l}"))
        })
        .collect();
    (lines, output.status.success())
}

#[test]
fn serves_the_protocol_over_pipes_and_stops_at_eof() {
    let sb = Sandbox::new();
    copy_tree(
        &fixture("../fusion/fusion_basic"),
        &sb.data().join("corpus/2023-11-14_221320_teams"),
    );
    let input = [
        request(1, "initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}})),
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n".to_string(),
        request(2, "tools/list", json!({})),
        request(3, "tools/call", json!({"name": "get_transcript", "arguments": {"limit": 2}})),
        request(4, "tools/call", json!({"name": "search_meetings", "arguments": {"query": "QA"}})),
        request(5, "bogus", json!({})),
    ]
    .concat();
    let (replies, success) = run_mcp(&sb, &input);
    assert!(success, "EOF is a clean shutdown");
    assert_eq!(replies.len(), 5);
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "justeli5");
    assert_eq!(replies[1]["result"]["tools"].as_array().unwrap().len(), 9);
    let transcript = &replies[2]["result"]["structuredContent"];
    assert_eq!(transcript["lines"].as_array().unwrap().len(), 2);
    assert_eq!(transcript["untrusted"], true);
    assert!(
        !replies[3]["result"]["structuredContent"]["matches"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(replies[4]["error"]["code"], -32601);
}

#[test]
fn an_empty_data_dir_still_answers() {
    let sb = Sandbox::new();
    let (replies, success) = run_mcp(
        &sb,
        &request(1, "tools/call", json!({"name": "list_meetings"})),
    );
    assert!(success);
    assert_eq!(
        replies[0]["result"]["structuredContent"]["meetings"],
        json!([])
    );
}
