//! Platform-neutral view of an application's accessibility tree, the pure
//! parsers that turn it into captions, and a source that ties both to a
//! [`UiBackend`] (Windows UI Automation, macOS Accessibility, or a fake).

pub mod anon;
pub mod ax_map;
pub mod ax_tree;
pub mod backend;
pub mod blocks;
pub mod browser;
pub mod dump;
pub mod fake;
pub mod meet;
pub mod mirror;
pub mod node;
pub mod platform;
pub mod report;
pub mod selectors;
pub mod slack;
pub mod source;
pub mod teams;
pub mod teams_web;
pub mod uia_map;
pub mod zoom;

pub use backend::{Budget, NodeIdent, Target, UiBackend, UiError};
pub use fake::{FakeBackend, Fixture, World};
pub use node::{Role, UiNode, WindowInfo};
pub use platform::Platform;
pub use selectors::Selectors;
pub use source::{NodeMode, UiaSource};
