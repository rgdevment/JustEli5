//! What is known between meetings: the terms of the team and the people in it, each apart from
//! the forms it is written or heard in, with the meetings it came up in as its context.

pub mod error;
pub mod learn;
pub mod model;
pub mod resolve;
pub mod store;

pub use error::MemoryError;
pub use learn::{Learnt, Lesson, Met};
pub use model::{Concept, Form, Kind, Role, Sighting, State};
pub use resolve::{Candidate, Context, Resolution};
pub use store::{Adopted, Memory};
