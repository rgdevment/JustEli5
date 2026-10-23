//! What a capture source looks like to the driver, and a scripted fake.

use std::collections::VecDeque;
use std::fmt;
use std::time::Duration;

use mc_core::RawObservation;

/// Where a source is in finding its meeting window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Discovery {
    /// Not found yet: the driver keeps retrying and does not step.
    Searching,
    Attached {
        window: String,
    },
    /// Nothing more will come (a fake ran out of frames, the window closed).
    Ended,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceError {
    /// Skip this frame and keep going.
    Transient(String),
    /// The window is gone: go back to discovery.
    Lost(String),
    /// Stop the capture.
    Fatal(String),
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transient(m) => write!(f, "transient source error: {m}"),
            Self::Lost(m) => write!(f, "source lost: {m}"),
            Self::Fatal(m) => write!(f, "fatal source error: {m}"),
        }
    }
}

impl std::error::Error for SourceError {}

/// A producer of caption observations. Not `Send`: native UI automation
/// handles are bound to the thread that created them, so the driver builds the
/// source on its own thread. The driver (not the source) measures `read_ms`.
pub trait CaptureSource {
    fn name(&self) -> &str;
    fn discover(&mut self) -> Result<Discovery, SourceError>;
    fn poll(&mut self) -> Result<Vec<RawObservation>, SourceError>;
    fn meeting_name(&self) -> Option<String> {
        None
    }
    /// The process that owns the window the captions are read from, once attached.
    fn window_pid(&self) -> Option<u32> {
        None
    }
    /// A source that replays recorded frames reports the `read_ms` they had,
    /// instead of the (meaningless) time its own `poll` took.
    fn read_ms_override(&self) -> Option<f64> {
        None
    }
    fn close(&mut self) {}
}

/// One step of a scripted source.
#[derive(Debug, Clone)]
pub enum FakeStep {
    /// What `poll` returns, with the `read_ms` a recorded frame had (the driver
    /// uses it instead of measuring when present, for deterministic replays).
    Frame {
        nodes: Vec<RawObservation>,
        read_ms: Option<f64>,
    },
    Error(SourceError),
    /// Sleeps inside `poll`, to model a slow source.
    Delay(Duration),
    /// Changes what the next `discover` answers.
    Discovery(Discovery),
}

#[derive(Debug, Default)]
pub struct FakeSource {
    name: String,
    pid: Option<u32>,
    meeting: Option<String>,
    script: VecDeque<FakeStep>,
    discovered: bool,
    last_read_ms: Option<f64>,
}

impl FakeSource {
    pub fn new(script: impl IntoIterator<Item = FakeStep>) -> Self {
        Self {
            name: "FakeSource".to_string(),
            script: script.into_iter().collect(),
            ..Self::default()
        }
    }

    /// Frames of `(speaker, text)` pairs, like the Python `FakeSource([...])`.
    pub fn frames(frames: Vec<Vec<(Option<&str>, Option<&str>)>>) -> Self {
        Self::new(frames.into_iter().map(|frame| {
            FakeStep::Frame {
                nodes: frame
                    .into_iter()
                    .map(|(speaker, text)| RawObservation {
                        speaker: speaker.map(str::to_string),
                        text: text.map(str::to_string),
                        node_id: None,
                    })
                    .collect(),
                read_ms: None,
            }
        }))
    }

    pub fn with_meeting_name(mut self, name: impl Into<String>) -> Self {
        self.meeting = Some(name.into());
        self
    }

    /// The pid a real source would report for its window.
    pub fn with_pid(mut self, pid: u32) -> Self {
        self.pid = Some(pid);
        self
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Steps left in the script.
    pub fn remaining(&self) -> usize {
        self.script.len()
    }

    /// The `read_ms` of the frame `poll` returned last (`None` when not scripted).
    pub fn last_read_ms(&self) -> Option<f64> {
        self.last_read_ms
    }
}

impl CaptureSource for FakeSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn discover(&mut self) -> Result<Discovery, SourceError> {
        // Scripted discovery answers come first, wherever they sit in the script.
        if let Some(FakeStep::Discovery(_)) = self.script.front()
            && let Some(FakeStep::Discovery(answer)) = self.script.pop_front()
        {
            self.discovered = matches!(answer, Discovery::Attached { .. });
            return Ok(answer);
        }
        if self.script.is_empty() {
            return Ok(Discovery::Ended);
        }
        self.discovered = true;
        Ok(Discovery::Attached {
            window: self.name.clone(),
        })
    }

    fn poll(&mut self) -> Result<Vec<RawObservation>, SourceError> {
        loop {
            match self.script.pop_front() {
                None => return Err(SourceError::Lost("script exhausted".to_string())),
                Some(FakeStep::Delay(delay)) => std::thread::sleep(delay),
                Some(FakeStep::Frame { nodes, read_ms }) => {
                    self.last_read_ms = read_ms;
                    return Ok(nodes);
                }
                Some(FakeStep::Error(error)) => return Err(error),
                Some(step @ FakeStep::Discovery(_)) => {
                    // A discovery change mid-run: the source was lost.
                    self.script.push_front(step);
                    return Err(SourceError::Lost("window closed".to_string()));
                }
            }
        }
    }

    fn window_pid(&self) -> Option<u32> {
        self.pid
    }

    fn meeting_name(&self) -> Option<String> {
        self.meeting.clone()
    }

    fn read_ms_override(&self) -> Option<f64> {
        self.last_read_ms
    }
}
