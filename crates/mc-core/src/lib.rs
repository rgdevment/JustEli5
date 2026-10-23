//! Core types shared by every crate: stamps, observations, records, clocks, and
//! the Python text primitives the reconciler must reproduce exactly.

pub mod clock;
pub mod config;
pub mod fold;
pub mod pytext;
pub mod speakers;
pub mod time;
pub mod types;

pub use clock::{Clock, FakeClock, ScriptedClock, SessionClock, Stamp};
pub use types::{Decision, FrameRef, RawObservation, Record, Segment};
