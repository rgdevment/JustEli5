//! `mc tui` and `mc capture --tui` without an interactive terminal: they fall
//! back to the plain capture, say so on stderr and exit like `mc capture`.

mod common;

use common::{Sandbox, path_str};

const FALLBACK: &str = "the TUI needs an interactive terminal; falling back to plain capture";

fn fake_source(sb: &Sandbox) -> String {
    let corpus = sb.corpus("synthetic_basic");
    format!("fake:{}", path_str(&corpus))
}

#[test]
fn tui_without_terminals_falls_back_to_plain_capture() {
    let sb = Sandbox::new();
    assert!(
        !sb.env.stdin_tty && !sb.env.stdout_tty,
        "the sandbox has no terminal"
    );
    let source = fake_source(&sb);
    let out = sb.mc(&["tui", "--source", &source, "--consent", "--poll-ms", "20"]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains(FALLBACK),
        "the fallback line goes to stderr, got: {}",
        out.stderr
    );
    assert!(
        out.stdout.contains("waiting for the meeting (FakeSource)"),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("ended (source_ended)"),
        "stdout: {}",
        out.stdout
    );
    assert!(
        sb.data().join("corpus").is_dir(),
        "the session is still recorded"
    );
}

#[test]
fn capture_with_the_tui_flag_falls_back_the_same_way() {
    let sb = Sandbox::new();
    let source = fake_source(&sb);
    let out = sb.mc(&[
        "capture",
        "--tui",
        "--source",
        &source,
        "--consent",
        "--poll-ms",
        "20",
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(out.stderr.contains(FALLBACK), "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("ended (source_ended)"),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn the_fallback_does_not_touch_stdout_with_the_tui_line() {
    let sb = Sandbox::new();
    let source = fake_source(&sb);
    let out = sb.mc(&["tui", "--source", &source, "--no-record", "--poll-ms", "20"]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr);
    assert!(!out.stdout.contains(FALLBACK));
    assert!(out.stderr.contains(FALLBACK));
}
