//! CLI providers (through a helper binary), argv construction, JSON extraction and the fake.

use std::path::Path;

use mc_llm::{
    FakeLlm, Llm, LlmConfig, LlmError, LlmRequest, build_invocation, check_argv, claude_args,
    extract_json, from_config,
};

const ECHO: &str = env!("CARGO_BIN_EXE_mc-llm-echo");

fn request(system: &str, user: &str) -> LlmRequest {
    LlmRequest {
        system: system.into(),
        user: user.into(),
        max_tokens: 100,
        temperature: None,
        json: false,
    }
}

fn command(args: &[&str], timeout_s: u64) -> LlmConfig {
    LlmConfig {
        provider: "command".into(),
        command: Some(ECHO.into()),
        args: args.iter().map(|a| a.to_string()).collect(),
        timeout_s,
        ..LlmConfig::default()
    }
}

#[test]
fn command_gets_system_then_user_on_stdin() {
    let llm = from_config(&command(&["--flag"], 20)).unwrap();
    let out = llm.complete(&request("SYS", "USER TEXT")).unwrap();
    assert!(out.contains("ARGS:--flag"), "{out}");
    let stdin = out.split("STDIN:").nth(1).unwrap();
    assert!(stdin.starts_with("SYS"), "{out}");
    assert!(stdin.trim_end().ends_with("USER TEXT"), "{out}");
}

#[test]
fn system_placeholder_moves_the_system_prompt_to_the_arguments() {
    let llm = from_config(&command(&["--system={system}"], 20)).unwrap();
    let out = llm.complete(&request("SYS", "USER TEXT")).unwrap();
    assert!(out.contains("ARGS:--system=SYS"), "{out}");
    assert_eq!(out.split("STDIN:").nth(1).unwrap().trim(), "USER TEXT");
}

#[test]
fn a_failing_command_reports_code_and_stderr() {
    let llm = from_config(&command(&["--fail"], 20)).unwrap();
    let error = llm.complete(&request("", "x")).unwrap_err();
    assert!(
        matches!(&error, LlmError::Process { code: Some(3), stderr } if stderr == "boom"),
        "{error}"
    );
}

#[test]
fn a_hanging_command_is_killed_at_the_timeout() {
    let llm = from_config(&command(&["--sleep"], 1)).unwrap();
    let started = std::time::Instant::now();
    assert!(matches!(
        llm.complete(&request("", "x")),
        Err(LlmError::Timeout)
    ));
    assert!(started.elapsed().as_secs() < 30);
}

#[test]
fn a_missing_command_says_how_to_fix_it() {
    for (provider, name) in [
        ("claude-cli", "claude"),
        ("copilot-cli", "copilot"),
        ("command", "no-such-tool"),
    ] {
        let cfg = LlmConfig {
            provider: provider.into(),
            command: Some(format!("{name}-definitely-not-installed")),
            ..LlmConfig::default()
        };
        let error = from_config(&cfg).err().unwrap();
        assert!(
            matches!(&error, LlmError::NotFound { hint, .. } if !hint.is_empty()),
            "{provider}: {error}"
        );
    }
}

#[test]
fn command_provider_requires_a_command_and_unknown_providers_are_rejected() {
    let cfg = LlmConfig {
        provider: "command".into(),
        ..LlmConfig::default()
    };
    assert!(matches!(from_config(&cfg).err(), Some(LlmError::Config(_))));
    let cfg = LlmConfig {
        provider: "nope".into(),
        ..LlmConfig::default()
    };
    assert!(matches!(from_config(&cfg).err(), Some(LlmError::Config(_))));
}

#[test]
fn claude_argv_is_print_mode_without_tools_and_the_prompt_stays_off_it() {
    let file = std::path::Path::new("sys.txt");
    let args = claude_args("sonnet", Some(file), &["--effort".into(), "low".into()]);
    let joined = args.join("\u{1}");
    for expected in [
        "-p",
        "--model\u{1}sonnet",
        "--output-format\u{1}json",
        "--tools\u{1}\u{1}--no-session-persistence",
        "--system-prompt-file\u{1}sys.txt",
        "--effort\u{1}low",
    ] {
        assert!(
            joined.contains(expected),
            "{expected:?} missing from {args:?}"
        );
    }
    assert!(!args.contains(&"--system-prompt".to_string()));
    assert!(!claude_args("sonnet", None, &[]).contains(&"--system-prompt-file".to_string()));
}

#[test]
fn argv_limits_follow_the_program() {
    let long = vec!["x".repeat(9000)];
    assert!(check_argv(Path::new("tool.exe"), &long).is_ok());
    assert!(check_argv(Path::new("tool.cmd"), &long).is_err());
    assert!(check_argv(Path::new("TOOL.BAT"), &long).is_err());
    assert!(check_argv(Path::new("tool"), &["x".repeat(31_000)]).is_err());
    let multiline = vec!["a\nb".to_string()];
    assert!(check_argv(Path::new("tool.exe"), &multiline).is_ok());
    assert!(matches!(
        check_argv(Path::new("tool.cmd"), &multiline),
        Err(LlmError::Config(_))
    ));
}

#[test]
fn a_prompt_too_long_for_argv_is_refused_before_running() {
    let llm = from_config(&command(&["{prompt}"], 20)).unwrap();
    let error = llm.complete(&request("", &"x".repeat(40_000))).unwrap_err();
    assert!(matches!(error, LlmError::Config(_)), "{error}");
}

#[test]
fn placeholders_are_filled_in_one_pass() {
    let args = ["{system}|{prompt}".to_string()];
    let out = build_invocation(&args, "has {prompt} inside", "user {system}");
    let expected = "has {prompt} inside|has {prompt} inside\n\n--- user ---\n\nuser {system}";
    assert_eq!(out.args, [expected]);
    let only_system = build_invocation(&["{system}".to_string()], "{system}{system}", "u");
    assert_eq!(only_system.args, ["{system}{system}"]);
}

#[test]
fn an_orphaned_grandchild_holding_the_pipes_cannot_outlive_the_deadline() {
    let llm = from_config(&command(&["--orphan"], 1)).unwrap();
    let started = std::time::Instant::now();
    let result = llm.complete(&request("", "x"));
    assert!(matches!(result, Err(LlmError::Timeout)), "{result:?}");
    assert!(started.elapsed().as_secs() < 20);
}

#[test]
fn claude_cli_never_puts_prompt_text_in_argv() {
    let program = Path::new(ECHO);
    let args = claude_args("sonnet", Some(Path::new("sys.txt")), &[]);
    assert!(check_argv(program, &args).is_ok());
    assert!(args.iter().all(|a| !a.contains('\n')));
}

#[test]
fn invocation_placeholders() {
    let args = |list: &[&str]| list.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    let plain = build_invocation(&args(&["-x"]), "S", "U");
    assert_eq!(plain.args, args(&["-x"]));
    assert!(plain.stdin.starts_with('S') && plain.stdin.ends_with('U'));
    let system = build_invocation(&args(&["--s={system}"]), "S", "U");
    assert_eq!(
        (system.args, system.stdin),
        (args(&["--s=S"]), "U".to_string())
    );
    let prompt = build_invocation(&args(&["-p", "{prompt}"]), "S", "U");
    assert!(
        prompt.stdin.is_empty() && prompt.args[1].contains('S') && prompt.args[1].contains('U')
    );
    assert_eq!(
        build_invocation(&args(&["{prompt}"]), "", "U").args,
        args(&["U"])
    );
}

#[test]
fn extract_json_finds_the_value_among_prose_and_fences() {
    let cases = [
        (r#"{"a": 1}"#, Some(r#"{"a": 1}"#)),
        (
            "Sure! Here you go:\n```json\n{\"a\": [1, 2]}\n```\nAnything else?",
            Some("{\"a\": [1, 2]}"),
        ),
        (
            "result: [1, {\"b\": \"}\"}] done",
            Some("[1, {\"b\": \"}\"}]"),
        ),
        (
            "I said {not json} then {\"ok\": true}",
            Some("{\"ok\": true}"),
        ),
        ("{\"a\": 1} and {\"b\": 2}", Some("{\"a\": 1}")),
        ("no json here", None),
        ("{\"cut\": ", None),
        ("", None),
    ];
    for (text, expected) in cases {
        assert_eq!(extract_json(text), expected, "{text:?}");
    }
}

#[test]
fn fake_answers_in_order_and_records_requests() {
    let fake = FakeLlm::new(["one", "two"]);
    let spy = fake.clone();
    let llm: Box<dyn Llm> = Box::new(fake);
    assert_eq!(llm.complete(&request("s", "a")).unwrap(), "one");
    assert_eq!(llm.complete(&request("s", "b")).unwrap(), "two");
    assert!(matches!(
        llm.complete(&request("s", "c")),
        Err(LlmError::BadResponse(_))
    ));
    let users: Vec<_> = spy.requests().into_iter().map(|r| r.user).collect();
    assert_eq!(users, ["a", "b", "c"]);

    let cfg = LlmConfig {
        provider: "fake".into(),
        args: vec!["scripted".into()],
        ..LlmConfig::default()
    };
    assert_eq!(
        from_config(&cfg)
            .unwrap()
            .complete(&request("", "q"))
            .unwrap(),
        "scripted"
    );
}
