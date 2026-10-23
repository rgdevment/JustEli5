//! Terminal UI for live capture: a pure model (`App`, `update`), a pure view
//! (`view`) and a thin terminal loop. Everything but `run` is testable with
//! ratatui's `TestBackend`.

pub mod app;
pub mod panel;
pub mod term;
pub mod view;

pub use app::{App, Effect, EndState, Modal, Msg, Phase, TistyTarget};
pub use term::{Actions, Outcome, run};
pub use view::view;
