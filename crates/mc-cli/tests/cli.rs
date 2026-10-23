//! The `mc replay` command: exit codes and the options it accepts.

use std::path::PathBuf;
use std::process::Command;

fn mc(args: &[&str]) -> Option<i32> {
    Command::new(env!("CARGO_BIN_EXE_mc"))
        .args(args)
        .output()
        .expect("mc runs")
        .status
        .code()
}

fn corpus(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/corpus")
        .join(name)
        .display()
        .to_string()
}

#[test]
fn skew_divergent_non_strict_exits_0() {
    assert_eq!(
        mc(&["replay", &corpus("synthetic_skew_divergent"), "--verify"]),
        Some(0)
    );
}

#[test]
fn skew_divergent_strict_exits_1() {
    assert_eq!(
        mc(&[
            "replay",
            &corpus("synthetic_skew_divergent"),
            "--verify",
            "--strict"
        ]),
        Some(1)
    );
}

#[test]
fn normal_corpus_strict_exits_0() {
    assert_eq!(
        mc(&["replay", &corpus("synthetic_basic"), "--verify", "--strict"]),
        Some(0)
    );
}

#[test]
fn unknown_flag_exits_2() {
    assert_eq!(
        mc(&["replay", &corpus("synthetic_basic"), "--verify", "--strcit"]),
        Some(2)
    );
}

#[test]
fn missing_corpus_dir_exits_3() {
    assert_eq!(
        mc(&["replay", &corpus("no_such_corpus"), "--verify"]),
        Some(3)
    );
}

#[test]
fn two_positionals_exit_2() {
    assert_eq!(
        mc(&[
            "replay",
            &corpus("synthetic_basic"),
            &corpus("synthetic_basic"),
            "--verify",
        ]),
        Some(2)
    );
}
