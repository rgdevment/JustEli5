//! A scripted backend over serialisable trees, for tests and `fake-uia:` runs.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::backend::{Budget, Target, UiBackend, UiError};
use super::node::{UiNode, WindowInfo};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FakeWindow {
    #[serde(flatten)]
    pub info: WindowInfo,
    pub tree: UiNode,
}

/// The windows that exist at one moment.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct World {
    pub windows: Vec<FakeWindow>,
}

/// The fixture file: the first world plus the worlds that follow it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fixture {
    pub format: String,
    pub version: u32,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub app: String,
    #[serde(default)]
    pub anonymized: bool,
    #[serde(default)]
    pub note: String,
    pub windows: Vec<FakeWindow>,
    /// Worlds that replace `windows` one by one as the backend advances.
    #[serde(default)]
    pub script: Vec<World>,
}

pub const FIXTURE_FORMAT: &str = "mc-uia-tree";

#[derive(Debug, Default)]
pub struct FakeBackend {
    worlds: Vec<World>,
    index: usize,
    killed: HashSet<u64>,
    auto_advance: bool,
    pub windows_calls: usize,
    pub snapshot_calls: usize,
    pub nodes_read: usize,
}

impl FakeBackend {
    pub fn new(world: World) -> Self {
        Self {
            worlds: vec![world],
            ..Self::default()
        }
    }

    pub fn scripted(worlds: Vec<World>) -> Self {
        Self {
            worlds,
            ..Self::default()
        }
    }

    pub fn from_fixture(fixture: Fixture) -> Self {
        let mut worlds = vec![World {
            windows: fixture.windows,
        }];
        worlds.extend(fixture.script);
        Self::scripted(worlds)
    }

    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        Ok(Self::from_fixture(serde_json::from_str(text)?))
    }

    /// Moves to the next world after every snapshot (a replayed meeting).
    pub fn auto_advance(mut self, on: bool) -> Self {
        self.auto_advance = on;
        self
    }

    pub fn advance(&mut self) {
        if self.index + 1 < self.worlds.len() {
            self.index += 1;
        }
    }

    /// Makes a window's handles stale, like the user closing it.
    pub fn kill(&mut self, handle: u64) {
        self.killed.insert(handle);
    }

    fn world(&self) -> Option<&World> {
        self.worlds.get(self.index)
    }

    fn window(&self, handle: u64) -> Option<&FakeWindow> {
        if self.killed.contains(&handle) {
            return None;
        }
        self.world()?
            .windows
            .iter()
            .find(|w| w.info.handle == handle)
    }
}

fn clip(
    node: &UiNode,
    depth: usize,
    budget: &Budget,
    count: &mut usize,
) -> Result<UiNode, UiError> {
    *count += 1;
    if *count > budget.max_nodes || budget.expired() {
        return Err(UiError::Budget);
    }
    let mut copy = UiNode {
        children: Vec::new(),
        ..node.clone()
    };
    if depth < budget.max_depth {
        for child in &node.children {
            copy.children.push(clip(child, depth + 1, budget, count)?);
        }
    }
    Ok(copy)
}

impl UiBackend for FakeBackend {
    fn windows(&mut self) -> Result<Vec<WindowInfo>, UiError> {
        self.windows_calls += 1;
        let killed = &self.killed;
        Ok(self
            .world()
            .map(|w| {
                w.windows
                    .iter()
                    .filter(|w| !killed.contains(&w.info.handle))
                    .map(|w| w.info.clone())
                    .collect()
            })
            .unwrap_or_default())
    }

    fn snapshot(&mut self, target: &Target, budget: &Budget) -> Result<UiNode, UiError> {
        self.snapshot_calls += 1;
        let window = self.window(target.window).ok_or(UiError::WindowGone)?;
        let node = window.tree.at(&target.path).ok_or(UiError::Stale)?;
        if target
            .expect
            .as_ref()
            .is_some_and(|ident| !ident.matches(node))
        {
            return Err(UiError::Stale);
        }
        let mut count = 0;
        let result = clip(node, 0, budget, &mut count);
        self.nodes_read += count;
        result
    }

    fn after_poll(&mut self) {
        if self.auto_advance {
            self.advance();
        }
    }

    fn alive(&mut self, window: u64) -> bool {
        self.window(window).is_some()
    }
}
