//! Helpers shared by the in-process command tests: explicit environments,
//! corpus copies in temporary directories, and captured output.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use mc_cli::cli::{Env, Io, Os, run};
use serde_json::Value;
use tempfile::TempDir;

pub const BASIC_SESSION: &str = "a8bf3c69037539ed23407ca2c775546c";
pub const SKEW_SESSION: &str = "69a710787d7e0315337304fe7b5cb3b0";
pub const UNICODE_SESSION: &str = "818dc3fdbfb723c94b7448369ed2e6d0";
pub const JITTER_SESSION: &str = "7f1df7cac9c46cfcf04ca6083ede7511";
pub const SINGLE_NODE_SESSION: &str = "72cc9bcb5c61468e8093d15452cd52fd";
pub const UNCLEAN_SESSION: &str = "dd589b83b094b28c66ed289d5efbb0ad";

/// Captured result of one `cli::run` call.
#[derive(Debug)]
pub struct Out {
    pub code: u8,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    /// Stdout parsed as one JSON document.
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e}):\n{}", self.stdout))
    }

    /// Stderr parsed as one JSON document (the `--json` error contract).
    pub fn err_json(&self) -> Value {
        serde_json::from_str(self.stderr.trim())
            .unwrap_or_else(|e| panic!("stderr is not JSON ({e}):\n{}", self.stderr))
    }

    /// The `run_id` field of a `mc run --json` output.
    pub fn run_id(&self) -> String {
        self.json()["run_id"]
            .as_str()
            .expect("run_id in run output")
            .to_string()
    }
}

/// An environment with explicit variables, the given OS and a fixed cwd.
pub fn env(os: Os, vars: &[(&str, &str)]) -> Env {
    Env {
        vars: vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        os,
        stdin_tty: false,
        stdout_tty: false,
        cwd: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
    }
}

/// Runs `mc` in-process with the given environment.
pub fn mc(env: &Env, args: &[&str]) -> Out {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let mut out: Vec<u8> = Vec::new();
    let mut err: Vec<u8> = Vec::new();
    let code = run(
        &args,
        env,
        &mut Io {
            out: &mut out,
            err: &mut err,
            input: None,
        },
    );
    Out {
        code,
        stdout: String::from_utf8(out).expect("stdout is UTF-8"),
        stderr: String::from_utf8(err).expect("stderr is UTF-8"),
    }
}

/// Path of a corpus in the repository fixtures (read-only use only).
pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/corpus")
        .join(name)
}

/// Copies a directory tree (corpora are flat, but recursion is harmless).
pub fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create copy target");
    for entry in fs::read_dir(src).expect("read corpus") {
        let entry = entry.expect("corpus entry");
        let target = dst.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy corpus file");
        }
    }
}

/// A temporary sandbox: data and output directories, a home directory, and a
/// PATH that holds no `tisty`. Every variable is explicit, so the host
/// environment never leaks into a test.
pub struct Sandbox {
    pub root: TempDir,
    pub env: Env,
}

impl Sandbox {
    pub fn new() -> Self {
        Self::with_os(Os::Linux)
    }

    pub fn with_os(os: Os) -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let base = root.path().to_path_buf();
        let vars = [
            ("MC_DATA_DIR", base.join("data").display().to_string()),
            ("MC_OUTPUT_DIR", base.join("out").display().to_string()),
            ("HOME", base.join("home").display().to_string()),
            ("PATH", base.join("no-bin").display().to_string()),
        ];
        let refs: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let env = env(os, &refs);
        Self { root, env }
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    pub fn data(&self) -> PathBuf {
        self.path("data")
    }

    pub fn out(&self) -> PathBuf {
        self.path("out")
    }

    /// The sandbox environment with some variables set or replaced.
    pub fn env_with(&self, extra: &[(&str, &str)]) -> Env {
        let mut env = self.env.clone();
        for (k, v) in extra {
            env.vars.insert(k.to_string(), v.to_string());
        }
        env
    }

    /// The sandbox environment without the named variables.
    pub fn env_without(&self, names: &[&str]) -> Env {
        let mut env = self.env.clone();
        for name in names {
            env.vars.remove(*name);
        }
        env
    }

    pub fn mc(&self, args: &[&str]) -> Out {
        mc(&self.env, args)
    }

    /// Copies a fixture corpus into `<root>/corpora/<name>` and returns it.
    /// Commands write `runs/` next to the corpus, so fixtures are never touched.
    pub fn corpus(&self, name: &str) -> PathBuf {
        let dst = self.path("corpora").join(name);
        if !dst.exists() {
            copy_tree(&fixture(name), &dst);
        }
        dst
    }

    /// Copies a corpus to an arbitrary directory under the sandbox.
    pub fn corpus_at(&self, name: &str, rel: &str) -> PathBuf {
        let dst = self.path(rel);
        copy_tree(&fixture(name), &dst);
        dst
    }

    /// Imports one corpus and runs it once; returns the corpus copy.
    pub fn imported_and_run(&self, name: &str) -> PathBuf {
        let dir = self.corpus(name);
        assert_eq!(self.mc(&["import", path_str(&dir)]).code, 0);
        assert_eq!(self.mc(&["run", path_str(&dir)]).code, 0);
        dir
    }
}

impl Default for Sandbox {
    fn default() -> Self {
        Self::new()
    }
}

pub fn path_str(path: &Path) -> &str {
    path.to_str().expect("UTF-8 test path")
}

/// `shown` names `path`; Windows ignores case (PATHEXT yields `.EXE` for `.exe`).
pub fn same_path(shown: &str, path: &Path) -> bool {
    if cfg!(windows) {
        shown.eq_ignore_ascii_case(path_str(path))
    } else {
        shown == path_str(path)
    }
}

/// An executable `name` in `dir` as the host's PATH lookup finds it (`.exe` on Windows).
pub fn fake_command(dir: &Path, name: &str) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let file = dir.join(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    });
    fs::write(&file, "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
    }
    file
}

/// Asserts the `--json` error contract: empty stdout, one error object on
/// stderr with the expected code, and the expected exit status.
pub fn assert_json_error(out: &Out, exit: u8, code: &str) {
    assert_eq!(out.code, exit, "exit code; stderr: {}", out.stderr);
    assert!(
        out.stdout.is_empty(),
        "stdout must be empty on error, got {:?}",
        out.stdout
    );
    let value = out.err_json();
    assert_eq!(value["error"]["code"], code, "error code in {}", out.stderr);
    assert!(
        value["error"]["message"].is_string(),
        "error message in {}",
        out.stderr
    );
}

/// Makes every file under `dir` look an hour old, like a session killed earlier
/// (a fresh copy would look like a capture that is still running).
pub fn age_files(dir: &Path) {
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            age_files(&path);
        } else {
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
    }
}
