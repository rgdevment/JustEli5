//! Long-running child processes (a capture, a model download, an installer): every
//! output line becomes a `job-line` event and the end a `job-exit` event.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

/// The stdin of the jobs that take commands, by job id.
#[derive(Default)]
pub struct Jobs(Mutex<HashMap<String, ChildStdin>>);

#[derive(Clone, Serialize)]
struct Line<'a> {
    id: &'a str,
    stream: &'static str,
    line: String,
}

#[derive(Clone, Serialize)]
struct Exit {
    id: String,
    code: Option<i32>,
}

/// Progress bars redraw with `\r`: only the last state of the line is worth showing.
fn last_segment(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    text.split('\r')
        .map(str::trim_end)
        .rfind(|s| !s.trim().is_empty())
        .unwrap_or("")
        .to_string()
}

fn forward(app: &AppHandle, id: &str, stream: &'static str, reader: impl Read) {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = last_segment(&buf);
                if !line.is_empty() {
                    let _ = app.emit("job-line", Line { id, stream, line });
                }
            }
        }
    }
}

pub fn spawn(
    app: &AppHandle,
    id: String,
    mut cmd: Command,
    keep_stdin: bool,
) -> Result<(), String> {
    let jobs = app.state::<Jobs>();
    if jobs.0.lock().map_err(|e| e.to_string())?.contains_key(&id) {
        return Err(format!("job {id} is already running"));
    }
    cmd.stdin(if keep_stdin {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    if let Some(stdin) = child.stdin.take() {
        jobs.0
            .lock()
            .map_err(|e| e.to_string())?
            .insert(id.clone(), stdin);
    }
    let errors = child.stderr.take().map(|stderr| {
        let (app, id) = (app.clone(), id.clone());
        std::thread::spawn(move || forward(&app, &id, "stderr", stderr))
    });
    let output = child.stdout.take();
    let app = app.clone();
    std::thread::spawn(move || {
        if let Some(stdout) = output {
            forward(&app, &id, "stdout", stdout);
        }
        if let Some(thread) = errors {
            let _ = thread.join();
        }
        let code = child.wait().ok().and_then(|status| status.code());
        if let Ok(mut map) = app.state::<Jobs>().0.lock() {
            map.remove(&id);
        }
        let _ = app.emit("job-exit", Exit { id, code });
    });
    Ok(())
}

/// One line to a running job's stdin (`n text`, `m`, `q` for a capture).
pub fn write(app: &AppHandle, id: &str, line: &str) -> Result<(), String> {
    let jobs = app.state::<Jobs>();
    let mut map = jobs.0.lock().map_err(|e| e.to_string())?;
    let stdin = map
        .get_mut(id)
        .ok_or_else(|| format!("job {id} is not running"))?;
    writeln!(stdin, "{line}")
        .and_then(|()| stdin.flush())
        .map_err(|e| e.to_string())
}
