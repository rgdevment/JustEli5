//! What the sources need from a platform's accessibility API.

use std::fmt;
use std::time::Instant;

use super::node::{Role, UiNode, WindowInfo};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiError {
    /// The window (or the whole app) is gone.
    WindowGone,
    /// A node we remembered no longer exists; look again.
    Stale,
    /// The walk ran out of nodes or time before finishing.
    Budget,
    Backend(String),
    /// The API could not be initialised (no COM, no permission): give up.
    Init(String),
}

impl fmt::Display for UiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WindowGone => f.write_str("window is gone"),
            Self::Stale => f.write_str("a remembered element no longer exists"),
            Self::Budget => f.write_str("the tree walk exceeded its node or time budget"),
            Self::Backend(m) => write!(f, "accessibility error: {m}"),
            Self::Init(m) => write!(f, "accessibility API unavailable: {m}"),
        }
    }
}

impl std::error::Error for UiError {}

/// What a remembered node looked like when it was found. A backend answers
/// `Stale` when the node now at the path is a different one (a banner inserted
/// before it moved the child indexes), instead of silently reading it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeIdent {
    pub role: Role,
    pub automation_id: String,
    pub class_name: String,
}

impl NodeIdent {
    pub fn of(node: &UiNode) -> Self {
        Self {
            role: node.role.clone(),
            automation_id: node.automation_id.clone(),
            class_name: node.class_name.clone(),
        }
    }

    pub fn matches(&self, node: &UiNode) -> bool {
        node.role == self.role
            && node.automation_id == self.automation_id
            && node.class_name == self.class_name
    }
}

/// Where to look: a window and a child-index path below it (empty: the window).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub window: u64,
    pub path: Vec<usize>,
    /// The identity the node at `path` must still have.
    pub expect: Option<NodeIdent>,
}

/// Limits for one snapshot, so a huge or slow tree cannot stall a poll.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    /// Depth below the snapshot root that is read (the root is depth 0).
    pub max_depth: usize,
    pub max_nodes: usize,
    pub deadline: Option<Instant>,
}

impl Budget {
    pub fn new(max_depth: usize, max_nodes: usize) -> Self {
        Self {
            max_depth,
            max_nodes,
            deadline: None,
        }
    }

    pub fn expired(&self) -> bool {
        self.deadline.is_some_and(|d| Instant::now() >= d)
    }
}

/// Not `Send`: native handles belong to the thread that created them.
///
/// Notes for the Windows implementation (`win_uia`):
/// * Use UI Automation's RAW view everywhere, as the Python `uiautomation`
///   package does (its `RawViewWalker`); with the default control view, depths
///   and child indexes differ from the recorded fixtures.
/// * One cross-process call per snapshot: a single `BuildUpdatedCache` /
///   `FindAllBuildCache(TreeScope_Subtree)` with a raw-view tree filter and only
///   ControlType, Name, AutomationId, ClassName and RuntimeId cached; clip the
///   depth on the client. Walking node by node costs one call per node.
/// * A deadline cannot interrupt a single COM call: bound it with
///   `IUIAutomation2` `TransactionTimeout` / `ConnectionTimeout`.
/// * Decode strings with `from_utf16_lossy` (a cut emoji leaves a lone
///   surrogate).
pub trait UiBackend {
    /// Top-level windows in the OS's order.
    fn windows(&mut self) -> Result<Vec<WindowInfo>, UiError>;
    /// A snapshot of the subtree at `target`, down to `budget.max_depth`.
    /// `WindowGone` when the window closed, `Stale` when the path no longer
    /// exists, `Budget` when the walk did not finish.
    fn snapshot(&mut self, target: &Target, budget: &Budget) -> Result<UiNode, UiError>;
    /// Whether a window still exists (cheap).
    fn alive(&mut self, window: u64) -> bool;
    /// Whether the last snapshot was cut short by its deadline or node budget.
    fn truncated(&self) -> bool {
        false
    }
    /// Called once after every poll. A replaying fake moves to its next world
    /// here; real backends have nothing to do.
    fn after_poll(&mut self) {}
}
