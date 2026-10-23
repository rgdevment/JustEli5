//! Session clock: one timeline for captions and audio.
//!
//! `mono_ms` (milliseconds since the session started, from a monotonic
//! counter) is the reference for aligning streams; `wall_ms` is the UTC epoch
//! time read from the system clock and never derived from `mono_ms`.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// A point on the session timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stamp {
    pub wall_ms: i64,
    pub mono_ms: f64,
}

/// Source of timeline stamps. Tests inject fakes; production uses [`SessionClock`].
pub trait Clock: Send + Sync {
    fn now(&self) -> Stamp;
}

fn wall_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Real clock: monotonic `mono_ms` from [`Instant`], `wall_ms` from the system clock.
#[derive(Debug)]
pub struct SessionClock {
    t0_wall_ms: i64,
    t0: Instant,
}

impl SessionClock {
    pub fn new() -> Self {
        Self {
            t0_wall_ms: wall_now_ms(),
            t0: Instant::now(),
        }
    }

    /// Wall-clock time at which the session started.
    pub fn t0_wall_ms(&self) -> i64 {
        self.t0_wall_ms
    }
}

impl Default for SessionClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SessionClock {
    fn now(&self) -> Stamp {
        Stamp {
            wall_ms: wall_now_ms(),
            // Same arithmetic as Python's perf_counter_ns delta / 1e6.
            mono_ms: self.t0.elapsed().as_nanos() as f64 / 1e6,
        }
    }
}

/// Clock whose time is set by the test: `wall_ms = base_wall_ms + round(mono_ms)`
/// (ties to even, like Python's `round`). Unlike the Python fake, which keeps
/// one timeline per thread, this one is global.
#[derive(Debug)]
pub struct FakeClock {
    base_wall_ms: i64,
    mono_ms: Mutex<f64>,
}

impl FakeClock {
    pub fn new(base_wall_ms: i64) -> Self {
        Self {
            base_wall_ms,
            mono_ms: Mutex::new(0.0),
        }
    }

    pub fn set_mono_ms(&self, mono_ms: f64) {
        *self.mono_ms.lock().expect("fake clock lock") = mono_ms;
    }

    pub fn advance_ms(&self, delta_ms: f64) {
        *self.mono_ms.lock().expect("fake clock lock") += delta_ms;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Stamp {
        let mono_ms = *self.mono_ms.lock().expect("fake clock lock");
        Stamp {
            wall_ms: self.base_wall_ms + mono_ms.round_ties_even() as i64,
            mono_ms,
        }
    }
}

/// Clock that replays recorded stamps in order, for parity tests: the
/// reconciler must ask for exactly the stamps the Python code asked for.
#[derive(Debug)]
pub struct ScriptedClock {
    state: Mutex<ScriptState>,
}

#[derive(Debug)]
struct ScriptState {
    queue: VecDeque<Stamp>,
    calls: usize,
}

impl ScriptedClock {
    pub fn new(stamps: impl IntoIterator<Item = Stamp>) -> Self {
        Self {
            state: Mutex::new(ScriptState {
                queue: stamps.into_iter().collect(),
                calls: 0,
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, ScriptState> {
        // A panic while asking for a stamp must not poison the counters.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Number of times `now` was called.
    pub fn calls(&self) -> usize {
        self.state().calls
    }

    /// Stamps that were scripted but never requested.
    pub fn remaining(&self) -> usize {
        self.state().queue.len()
    }
}

impl Clock for ScriptedClock {
    /// Returns the next scripted stamp; panics when the script is exhausted,
    /// which means the code under test asked for more stamps than Python did.
    fn now(&self) -> Stamp {
        let next = {
            let mut state = self.state();
            state.calls += 1;
            state.queue.pop_front()
        };
        next.expect("scripted clock exhausted: more now() calls than recorded")
    }
}
