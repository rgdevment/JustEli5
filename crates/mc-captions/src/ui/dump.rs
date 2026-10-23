//! Dumps real accessibility trees in the fixture format (`mc doctor --dump`).

use std::time::{Duration, Instant};

use super::anon::anonymise;
use super::backend::{Budget, Target, UiBackend, UiError};
use super::fake::{FIXTURE_FORMAT, FakeWindow, Fixture};
use super::node::WindowInfo;
use super::platform::Platform;
use super::selectors::Selectors;

#[derive(Debug, Clone)]
pub struct DumpOptions {
    /// A platform name (`teams`, `zoom`, `meet`, `teams-web`, `slack`): what the dump is for.
    pub platform: String,
    /// Indexes into the window list; `None`: every window that could matter.
    pub windows: Option<Vec<usize>>,
    pub max_depth: usize,
    pub max_nodes: usize,
    /// Keep the real text. Local use only: it contains the meeting's speech.
    pub raw: bool,
}

impl Default for DumpOptions {
    fn default() -> Self {
        Self {
            platform: "teams".to_string(),
            windows: None,
            max_depth: 60,
            max_nodes: 20_000,
            raw: false,
        }
    }
}

/// Whether a window can be (part of) a meeting of any platform.
pub fn is_candidate(window: &WindowInfo, selectors: &Selectors) -> bool {
    Platform::ALL.iter().any(|p| p.is_window(window, selectors))
}

/// Reads the chosen windows. Each tree's root is a bare `Window` (title and
/// class live on the window entry), as the original Python app read it.
pub fn dump(
    backend: &mut dyn UiBackend,
    selectors: &Selectors,
    options: &DumpOptions,
) -> Result<Fixture, UiError> {
    let listed = backend.windows()?;
    let chosen: Vec<&WindowInfo> = match &options.windows {
        Some(indexes) => indexes.iter().filter_map(|i| listed.get(*i)).collect(),
        None => listed
            .iter()
            .filter(|w| is_candidate(w, selectors))
            .collect(),
    };
    let mut windows = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for info in chosen {
        let mut budget = Budget::new(options.max_depth, options.max_nodes);
        budget.deadline = Some(Instant::now() + Duration::from_secs(20));
        let target = Target {
            window: info.handle,
            path: Vec::new(),
            expect: None,
        };
        match backend.snapshot(&target, &budget) {
            Ok(mut tree) => {
                tree.name.clear();
                tree.class_name.clear();
                tree.automation_id.clear();
                windows.push(FakeWindow {
                    info: info.clone(),
                    tree,
                });
            }
            Err(UiError::WindowGone) => {}
            // One unreadable window must not lose the others.
            Err(other) => skipped.push(format!("window {}: {other}", info.handle)),
        }
    }
    let fixture = Fixture {
        format: FIXTURE_FORMAT.to_string(),
        version: 1,
        platform: options.platform.clone(),
        app: Platform::parse(&options.platform)
            .unwrap_or(Platform::Teams)
            .app()
            .to_string(),
        anonymized: false,
        note: if skipped.is_empty() {
            "dumped by mc doctor --dump (raw)".to_string()
        } else {
            format!(
                "dumped by mc doctor --dump (raw); skipped: {}",
                skipped.join("; ")
            )
        },
        windows,
        script: Vec::new(),
    };
    Ok(if options.raw {
        fixture
    } else {
        anonymise(&fixture, selectors)
    })
}
