use std::path::Path;
use std::time::{Duration, Instant};

use mc_mcp_client::{McpClient, McpError, Timeouts};
use serde_json::json;

const FAKE: &str = env!("CARGO_BIN_EXE_mc-fake-mcp");

fn start(args: &[&str], call: Duration) -> McpClient {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let timeouts = Timeouts {
        init: Duration::from_secs(10),
        call,
    };
    let mut client = McpClient::spawn(Path::new(FAKE), &args, timeouts).expect("spawn");
    client.initialize().expect("initialize");
    client
}

#[test]
fn handshake_lists_tools_and_calls_one() {
    let mut client = start(&[], Duration::from_secs(10));
    let names: Vec<String> = client
        .list_tools()
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert!(names.contains(&"write_doc".to_string()) && names.contains(&"propose".to_string()));
    let out = client.call_tool("lists", json!({})).unwrap();
    assert!(!out.is_error);
    assert!(out.text.contains("Tareas"), "{}", out.text);
    assert_eq!(out.structured, Some(json!({"lists": ["Tareas"]})));
    client.close();
}

#[test]
fn tool_errors_come_back_flagged_not_as_failures() {
    let mut client = start(&[], Duration::from_secs(10));
    let out = client
        .call_tool("write_doc", json!({"body": "x", "folder": "Nope"}))
        .unwrap();
    assert!(out.is_error);
    assert!(out.text.contains("Unknown folder"), "{}", out.text);
}

#[test]
fn a_server_that_rejects_calls_is_reported_by_text() {
    let mut client = start(&["--not-admitted"], Duration::from_secs(10));
    let out = client.call_tool("rules", json!({})).unwrap();
    assert!(
        out.is_error && out.text.contains("not admitted"),
        "{}",
        out.text
    );
}

#[test]
fn unknown_tools_are_flagged_errors() {
    let mut client = start(&[], Duration::from_secs(10));
    let out = client.call_tool("nope", json!({})).unwrap();
    assert!(out.is_error);
}

#[test]
fn a_silent_server_times_out_and_is_killed() {
    let mut client = start(&["--hang"], Duration::from_millis(300));
    let started = Instant::now();
    let err = client.call_tool("rules", json!({})).unwrap_err();
    assert!(matches!(err, McpError::Timeout { .. }), "{err}");
    assert!(started.elapsed() < Duration::from_secs(5));
    // The child is gone: later calls fail instead of hanging.
    assert!(client.call_tool("rules", json!({})).is_err());
}

#[test]
fn a_server_that_dies_reports_its_stderr() {
    let args = vec!["--exit-on-init".to_string()];
    let mut client = McpClient::spawn(Path::new(FAKE), &args, Timeouts::default()).unwrap();
    let err = client.initialize().unwrap_err();
    match err {
        McpError::Closed { stderr } => assert!(stderr.contains("fatal start-up"), "{stderr}"),
        other => panic!("{other}"),
    }
}

#[test]
fn a_missing_program_is_a_spawn_error() {
    let err = McpClient::spawn(Path::new("/definitely/not/here"), &[], Timeouts::default());
    assert!(matches!(err, Err(McpError::Spawn(_))));
}

#[test]
fn lines_that_are_not_utf8_do_not_end_the_conversation() {
    let mut client = start(&["--garbage"], Duration::from_secs(10));
    let out = client.call_tool("lists", json!({})).unwrap();
    assert!(out.text.contains("Tareas"), "{}", out.text);
}

#[test]
fn a_json_rpc_error_is_an_error_of_that_call_only() {
    let mut client = start(&["--rpc-error-title", "boom"], Duration::from_secs(10));
    let err = client
        .call_tool("propose", json!({"title": "boom"}))
        .unwrap_err();
    assert!(matches!(err, McpError::Rpc { code: -32602, .. }), "{err}");
    let out = client
        .call_tool("propose", json!({"title": "fine", "source": "s"}))
        .unwrap();
    assert!(!out.is_error);
}

#[test]
fn a_server_that_stops_reading_cannot_block_the_caller() {
    let mut client = start(&["--deaf"], Duration::from_millis(500));
    let big = "x".repeat(8 * 1024 * 1024);
    let started = Instant::now();
    let err = client
        .call_tool("write_doc", json!({"body": big}))
        .unwrap_err();
    assert!(matches!(err, McpError::Timeout { .. }), "{err}");
    assert!(started.elapsed() < Duration::from_secs(10));
}
