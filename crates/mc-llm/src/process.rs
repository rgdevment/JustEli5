//! Running a CLI as a completion backend: prompt on stdin, answer on stdout.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::error::{LlmError, tail};

const POLL: Duration = Duration::from_millis(20);
/// cmd.exe refuses longer command lines, and batch wrappers (npm's `.cmd`) go through it.
const BATCH_ARGV_LIMIT: usize = 8191;
const ARGV_LIMIT: usize = 30_000;

/// Looks `command` up in `path` (`;` or `:` separated); `pathext` is `Some` on
/// Windows, where a bare name also matches each of its extensions.
pub fn find_on_path(command: &str, path: &str, pathext: Option<&str>) -> Option<PathBuf> {
    let separator = if pathext.is_some() { ';' } else { ':' };
    let extensions: Vec<&str> = match pathext {
        Some(list) => {
            // A command that already carries an extension is tried as written first.
            let mut all = if Path::new(command).extension().is_some() {
                vec![""]
            } else {
                Vec::new()
            };
            all.extend(list.split(';'));
            all
        }
        None => vec![""],
    };
    path.split(separator)
        .filter(|d| !d.is_empty())
        .find_map(|dir| {
            extensions.iter().find_map(|ext| {
                let candidate = PathBuf::from(dir).join(format!("{command}{ext}"));
                candidate.is_file().then_some(candidate)
            })
        })
}

/// A command with a directory part is a path (relative ones to `cwd`); a bare
/// name is looked up on `path`.
pub fn resolve_command(
    command: &str,
    cwd: &Path,
    path: &str,
    pathext: Option<&str>,
) -> Option<PathBuf> {
    if command.contains(['/', '\\']) {
        let full = cwd.join(command);
        return full.is_file().then_some(full);
    }
    find_on_path(command, path, pathext)
}

/// Resolves `command` against the real process environment.
pub(crate) fn resolve(command: &str, hint: &str) -> Result<PathBuf, LlmError> {
    let path = std::env::var("PATH").unwrap_or_default();
    let pathext = cfg!(windows)
        .then(|| std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string()));
    let cwd = std::env::current_dir().unwrap_or_default();
    resolve_command(command, &cwd, &path, pathext.as_deref()).ok_or_else(|| LlmError::NotFound {
        command: command.to_string(),
        hint: hint.to_string(),
    })
}

fn is_batch(program: &Path) -> bool {
    program
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
}

/// Whether `args` can reach `program` intact: batch wrappers take no newlines
/// and a shorter command line than a real executable.
pub fn check_argv(program: &Path, args: &[String]) -> Result<(), LlmError> {
    let batch = is_batch(program);
    if batch && args.iter().any(|a| a.contains(['\n', '\r'])) {
        return Err(LlmError::Config(format!(
            "{} is a batch wrapper and cannot take a multi-line argument; use a command that reads stdin",
            program.display()
        )));
    }
    let limit = if batch { BATCH_ARGV_LIMIT } else { ARGV_LIMIT };
    let length: usize = program.as_os_str().len() + args.iter().map(|a| a.len() + 1).sum::<usize>();
    if length > limit {
        return Err(LlmError::Config(format!(
            "the arguments take {length} bytes but {} accepts {limit}; use a command that reads stdin",
            program.display()
        )));
    }
    Ok(())
}

fn drain(mut pipe: impl Read + Send + 'static) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        let _ = tx.send(String::from_utf8_lossy(&bytes).into_owned());
    });
    rx
}

/// Ends the child and everything it started (a `.cmd` shim leaves a node child behind).
fn kill_tree(child: &mut Child) {
    let pid = child.id().to_string();
    #[cfg(windows)]
    let tree = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid])
        .output();
    #[cfg(unix)]
    let tree = Command::new("kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .output();
    // Fall back to the child alone if the tool is missing.
    let _ = tree;
    let _ = child.kill();
    let _ = child.wait();
}

/// Runs the program to completion and returns its trimmed stdout. A run past
/// `timeout` (pipes included) is killed with its whole process tree.
pub(crate) fn run(
    program: &Path,
    args: &[String],
    stdin: &str,
    timeout: Duration,
) -> Result<String, LlmError> {
    check_argv(program, args)?;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own process group, so the timeout can end the children too.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command.spawn().map_err(|e| LlmError::Process {
        code: None,
        stderr: format!("{}: {e}", program.display()),
    })?;
    let deadline = Instant::now() + timeout;
    let mut input = child.stdin.take().expect("stdin was piped");
    let text = stdin.to_string();
    // A child that exits early closes the pipe; its exit status tells the story.
    std::thread::spawn(move || {
        let _ = input.write_all(text.as_bytes());
    });
    let stdout = drain(child.stdout.take().expect("stdout was piped"));
    let stderr = drain(child.stderr.take().expect("stderr was piped"));

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                kill_tree(&mut child);
                return Err(LlmError::Timeout);
            }
            Ok(None) => std::thread::sleep(POLL),
            Err(e) => {
                return Err(LlmError::Process {
                    code: None,
                    stderr: e.to_string(),
                });
            }
        }
    };
    // A grandchild that kept the pipes open must not outlive the deadline either.
    let wait = |rx: &mpsc::Receiver<String>| {
        rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
    };
    let (Ok(out), Ok(err)) = (wait(&stdout), wait(&stderr)) else {
        kill_tree(&mut child);
        return Err(LlmError::Timeout);
    };
    if !status.success() {
        let detail = if err.trim().is_empty() { &out } else { &err };
        return Err(LlmError::Process {
            code: status.code(),
            stderr: tail(detail),
        });
    }
    let answer = out.trim();
    if answer.is_empty() {
        return Err(LlmError::BadResponse("the command printed nothing".into()));
    }
    Ok(answer.to_string())
}
