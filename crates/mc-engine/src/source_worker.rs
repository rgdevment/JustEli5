//! Runs a capture source on its own thread so a stuck native call never blocks the driver.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use mc_captions::{Discovery, SourceError};
use mc_core::RawObservation;

use crate::live::SharedFactory;

enum Cmd {
    Discover,
    Poll { want_meeting: bool },
    Close,
}

/// One poll with what the driver needs from it.
pub struct Polled {
    pub result: Result<Vec<RawObservation>, SourceError>,
    pub read_ms: f64,
    pub meeting: Option<String>,
}

pub enum Reply {
    Built(Result<String, SourceError>),
    Discovered(Result<Discovery, SourceError>, Option<u32>),
    Polled(Polled),
    Closed,
}

/// Why waiting for the worker ended without a reply.
#[derive(Debug, PartialEq, Eq)]
pub enum Gone {
    /// The worker thread ended (the source panicked).
    Dead,
    /// Still busy when the caller's deadline passed.
    Timeout,
}

pub struct SourceWorker {
    cmds: Sender<Cmd>,
    replies: Receiver<Reply>,
}

impl SourceWorker {
    /// Starts the thread, which builds the source first (reply: `Built`).
    pub fn spawn(factory: SharedFactory) -> Self {
        let (cmds, cmd_rx) = channel();
        let (reply_tx, replies) = channel();
        std::thread::spawn(move || work(&factory, &cmd_rx, &reply_tx));
        Self { cmds, replies }
    }

    pub fn discover(&self) -> Result<(), Gone> {
        self.send(Cmd::Discover)
    }

    pub fn poll(&self, want_meeting: bool) -> Result<(), Gone> {
        self.send(Cmd::Poll { want_meeting })
    }

    fn send(&self, cmd: Cmd) -> Result<(), Gone> {
        self.cmds.send(cmd).map_err(|_| Gone::Dead)
    }

    /// Waits up to `wait` for the next reply.
    pub fn recv(&self, wait: Duration) -> Result<Reply, Gone> {
        match self.replies.recv_timeout(wait) {
            Ok(reply) => Ok(reply),
            Err(RecvTimeoutError::Timeout) => Err(Gone::Timeout),
            Err(RecvTimeoutError::Disconnected) => Err(Gone::Dead),
        }
    }

    /// Asks the source to close and waits at most `wait`; a stuck source is
    /// left detached with its thread.
    pub fn close(self, wait: Duration) {
        if self.cmds.send(Cmd::Close).is_err() {
            return;
        }
        let deadline = Instant::now() + wait;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            // Replies to an abandoned request may still come before the ack.
            if matches!(self.recv(left), Ok(Reply::Closed) | Err(Gone::Dead)) {
                return;
            }
        }
    }
}

fn work(factory: &SharedFactory, cmds: &Receiver<Cmd>, replies: &Sender<Reply>) {
    let built = match factory.lock() {
        Ok(mut build) => build(),
        Err(_) => Err(SourceError::Fatal("source factory poisoned".to_string())),
    };
    let mut source = match built {
        Ok(source) => {
            let _ = replies.send(Reply::Built(Ok(source.name().to_string())));
            source
        }
        Err(error) => {
            let _ = replies.send(Reply::Built(Err(error)));
            return;
        }
    };
    for cmd in cmds {
        // A panic ends the thread (the driver sees it gone) but still closes the source.
        let reply = catch_unwind(AssertUnwindSafe(|| match cmd {
            Cmd::Discover => {
                let found = source.discover();
                Some(Reply::Discovered(found, source.window_pid()))
            }
            Cmd::Poll { want_meeting } => {
                let started = Instant::now();
                let result = source.poll();
                let measured = started.elapsed().as_secs_f64() * 1000.0;
                Some(Reply::Polled(Polled {
                    result,
                    read_ms: source.read_ms_override().unwrap_or(measured),
                    meeting: if want_meeting {
                        source.meeting_name()
                    } else {
                        None
                    },
                }))
            }
            Cmd::Close => None,
        }));
        match reply {
            Ok(Some(reply)) => {
                if replies.send(reply).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
    let _ = catch_unwind(AssertUnwindSafe(|| source.close()));
    let _ = replies.send(Reply::Closed);
}
