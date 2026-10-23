//! Providers that run a local CLI: `claude-cli`, `copilot-cli` and `command`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::process;
use crate::{CallUsage, Llm, LlmError, LlmRequest, Meter, system_text};

pub const CLAUDE_DEFAULT_MODEL: &str = "sonnet";
const CLAUDE_HINT: &str =
    "install the `claude` command-line tool, sign in once, and make sure it is on PATH";
const COPILOT_HINT: &str = "install the GitHub Copilot CLI (npm install -g @github/copilot) and sign in with `copilot` once";
const SEPARATOR: &str = "\n\n--- user ---\n\n";

/// Print mode, no tools, no saved session. Neither prompt travels in argv: the
/// system prompt is a file and the user prompt goes on stdin.
pub fn claude_args(model: &str, system_file: Option<&Path>, extra: &[String]) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--model",
        model,
        "--output-format",
        "json",
        "--tools",
        "",
        "--no-session-persistence",
        "--strict-mcp-config",
        "--disable-slash-commands",
    ]
    .map(String::from)
    .to_vec();
    if let Some(file) = system_file {
        args.push("--system-prompt-file".into());
        args.push(file.display().to_string());
    }
    args.extend_from_slice(extra);
    args
}

/// A file removed when dropped.
struct TempFile(PathBuf);

impl TempFile {
    fn with_text(text: &str) -> Result<Self, LlmError> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path =
            std::env::temp_dir().join(format!("mc-llm-system-{}-{nanos}.txt", std::process::id()));
        std::fs::write(&path, text).map_err(|e| LlmError::Process {
            code: None,
            stderr: format!("{}: {e}", path.display()),
        })?;
        Ok(Self(path))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub struct ClaudeCli {
    program: PathBuf,
    model: String,
    extra_args: Vec<String>,
    timeout: Duration,
    meter: Meter,
}

impl ClaudeCli {
    pub fn new(
        command: Option<&str>,
        model: Option<&str>,
        extra_args: &[String],
        timeout: Duration,
    ) -> Result<Self, LlmError> {
        Ok(Self {
            program: process::resolve(command.unwrap_or("claude"), CLAUDE_HINT)?,
            model: model.unwrap_or(CLAUDE_DEFAULT_MODEL).to_string(),
            extra_args: extra_args.to_vec(),
            timeout,
            meter: Meter::default(),
        })
    }
}

/// Input (cache included), cache read and output tokens of one answer.
pub type Tokens = (Option<u64>, Option<u64>, Option<u64>);

/// The text and token counts of a `--output-format json` answer; anything else is the text.
pub fn claude_answer(stdout: &str) -> Result<(String, Tokens), LlmError> {
    // A notice printed before the JSON would hide it; the answer is its last line.
    let parsed = serde_json::from_str::<Value>(stdout.trim())
        .ok()
        .or_else(|| {
            let last = stdout
                .lines()
                .rev()
                .find(|l| l.trim_start().starts_with('{'))?;
            serde_json::from_str(last.trim()).ok()
        });
    let Some(value) = parsed else {
        return Ok((stdout.to_string(), (None, None, None)));
    };
    let Some(text) = value["result"].as_str() else {
        return Ok((stdout.to_string(), (None, None, None)));
    };
    if value["is_error"] == true {
        return Err(LlmError::BadResponse(format!("claude: {text}")));
    }
    let usage = &value["usage"];
    let cached = |key: &str| usage[key].as_u64().unwrap_or(0);
    let input = usage["input_tokens"]
        .as_u64()
        .map(|n| n + cached("cache_creation_input_tokens") + cached("cache_read_input_tokens"));
    let read = usage["cache_read_input_tokens"].as_u64();
    Ok((
        text.to_string(),
        (
            input,
            read.or(input.map(|_| 0)),
            usage["output_tokens"].as_u64(),
        ),
    ))
}

impl Llm for ClaudeCli {
    fn name(&self) -> String {
        format!("claude-cli ({})", self.model)
    }

    fn complete(&self, req: &LlmRequest) -> Result<String, LlmError> {
        let system = system_text(req);
        let file = (!system.is_empty())
            .then(|| TempFile::with_text(&system))
            .transpose()?;
        let args = claude_args(
            &self.model,
            file.as_ref().map(|f| f.0.as_path()),
            &self.extra_args,
        );
        let started = Instant::now();
        let stdout = process::run(&self.program, &args, &req.user, self.timeout)?;
        let (text, (input, read, output)) = claude_answer(&stdout)?;
        self.meter.record_cached(started, input, read, output);
        Ok(text)
    }

    fn usage(&self) -> Vec<CallUsage> {
        self.meter.calls()
    }
}

/// How the prompt reaches a generic command.
#[derive(Debug, PartialEq, Eq)]
pub struct Invocation {
    pub args: Vec<String>,
    pub stdin: String,
}

/// Replaces every placeholder in one left-to-right pass, so a value that
/// itself contains `{system}` or `{prompt}` is never expanded again.
fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    'scan: while !rest.is_empty() {
        for (name, value) in values {
            if let Some(after) = rest.strip_prefix(name) {
                out.push_str(value);
                rest = after;
                continue 'scan;
            }
        }
        let width = rest.chars().next().map_or(1, char::len_utf8);
        out.push_str(&rest[..width]);
        rest = &rest[width..];
    }
    out
}

/// `{prompt}` (system and user together) and `{system}` go into the arguments;
/// without either, the system prompt is prepended to stdin.
pub fn build_invocation(args: &[String], system: &str, user: &str) -> Invocation {
    let uses = |placeholder: &str| args.iter().any(|a| a.contains(placeholder));
    let combined = if system.is_empty() {
        user.to_string()
    } else {
        format!("{system}{SEPARATOR}{user}")
    };
    if uses("{prompt}") {
        let values = [("{prompt}", combined.as_str()), ("{system}", system)];
        return Invocation {
            args: args.iter().map(|a| fill(a, &values)).collect(),
            stdin: String::new(),
        };
    }
    if uses("{system}") {
        let values = [("{system}", system)];
        return Invocation {
            args: args.iter().map(|a| fill(a, &values)).collect(),
            stdin: user.to_string(),
        };
    }
    Invocation {
        args: args.to_vec(),
        stdin: combined,
    }
}

pub struct CommandLlm {
    label: String,
    program: PathBuf,
    args: Vec<String>,
    timeout: Duration,
    meter: Meter,
}

impl CommandLlm {
    pub fn new(
        label: &str,
        command: &str,
        args: &[String],
        hint: &str,
        timeout: Duration,
    ) -> Result<Self, LlmError> {
        Ok(Self {
            label: label.to_string(),
            program: process::resolve(command, hint)?,
            args: args.to_vec(),
            timeout,
            meter: Meter::default(),
        })
    }

    /// `copilot -p <prompt> --silent`: the non-interactive mode of the Copilot CLI,
    /// which takes its prompt as an argument (`run` checks it fits).
    pub fn copilot(
        command: Option<&str>,
        model: Option<&str>,
        extra_args: &[String],
        timeout: Duration,
    ) -> Result<Self, LlmError> {
        let mut args: Vec<String> = ["-p", "{prompt}", "--silent"].map(String::from).to_vec();
        if let Some(model) = model {
            args.extend(["--model".to_string(), model.to_string()]);
        }
        args.extend_from_slice(extra_args);
        Self::new(
            "copilot-cli",
            command.unwrap_or("copilot"),
            &args,
            COPILOT_HINT,
            timeout,
        )
    }
}

impl Llm for CommandLlm {
    fn name(&self) -> String {
        self.label.clone()
    }

    fn complete(&self, req: &LlmRequest) -> Result<String, LlmError> {
        let invocation = build_invocation(&self.args, &system_text(req), &req.user);
        let started = Instant::now();
        let answer = process::run(
            &self.program,
            &invocation.args,
            &invocation.stdin,
            self.timeout,
        )?;
        self.meter.record(started, None, None);
        Ok(answer)
    }

    fn usage(&self) -> Vec<CallUsage> {
        self.meter.calls()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_json_answer_gives_its_text_and_every_input_token() {
        let out = r#"{"result":"ok","is_error":false,"usage":{"input_tokens":2,"cache_creation_input_tokens":4513,"cache_read_input_tokens":649,"output_tokens":4}}"#;
        assert_eq!(
            claude_answer(out).unwrap(),
            ("ok".to_string(), (Some(5164), Some(649), Some(4)))
        );
        let noted = format!(
            "notice
{out}
"
        );
        assert_eq!(claude_answer(&noted).unwrap().0, "ok");
        assert_eq!(
            claude_answer("plain text").unwrap(),
            ("plain text".to_string(), (None, None, None))
        );
        let failed = r#"{"result":"rate limited","is_error":true}"#;
        assert!(claude_answer(failed).is_err());
    }
}
