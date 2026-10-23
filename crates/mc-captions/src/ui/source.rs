//! A [`CaptureSource`] over any [`UiBackend`] and a platform's parsers.

use std::time::{Duration, Instant};

use mc_core::RawObservation;

use super::backend::{Budget, NodeIdent, Target, UiBackend, UiError};
use super::blocks::KnownSpeakers;
use super::node::{UiNode, WindowInfo};
pub use super::platform::Platform;
use super::selectors::Selectors;
use crate::source::{CaptureSource, Discovery, SourceError};

/// Consecutive transient failures after which the window is treated as lost.
const MAX_TRANSIENT: u32 = 3;
/// A Zoom window without a caption list is searched again at this pace
/// (Python searches on every poll, which would walk the tree ten times a second).
const NO_LIST_RETRY: Duration = Duration::from_secs(1);

/// Per-snapshot limits. Real UI Automation makes each node a cross-process call,
/// so the Windows backend must keep within these by caching (see `UiBackend`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_nodes: usize,
    pub poll_budget: Duration,
    pub discovery_budget: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_nodes: 5000,
            poll_budget: Duration::from_millis(250),
            discovery_budget: Duration::from_millis(1500),
        }
    }
}

impl Limits {
    /// macOS reads node by node over IPC (no cached subtree), and Chromium trees are big: more nodes and
    /// time to attach (discovery 6 s, 20000 nodes), and 500 ms per poll of the caption container.
    pub fn macos() -> Self {
        Self {
            max_nodes: 20_000,
            poll_budget: Duration::from_millis(500),
            discovery_budget: Duration::from_secs(6),
        }
    }
}

/// `All`: every caption on screen each frame; `Last`: only the newest
/// (`capture_all_nodes = false` in the Python configuration).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeMode {
    All,
    Last,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Stats {
    pub discoveries: u32,
    pub polls: u32,
    pub cache_hits: u32,
    pub revalidations: u32,
    pub whole_window_fallbacks: u32,
    /// Snapshots the backend cut short (deadline or node budget).
    pub truncated_snapshots: u32,
}

struct Cached {
    window: WindowInfo,
    /// The container of the captions; `None`: the window has none (Zoom with
    /// captions off), so there is nothing to read.
    container: Option<(Vec<usize>, NodeIdent)>,
    since: Instant,
}

impl Cached {
    fn target(&self) -> Option<Target> {
        self.container.as_ref().map(|(path, ident)| Target {
            window: self.window.handle,
            path: path.clone(),
            expect: Some(ident.clone()),
        })
    }
}

pub struct UiaSource<B: UiBackend> {
    backend: B,
    platform: Platform,
    selectors: Selectors,
    mode: NodeMode,
    limits: Limits,
    cached: Option<Cached>,
    transient_run: u32,
    /// Speakers seen in this meeting, so a flattened caption region reads the same on every poll.
    known_speakers: KnownSpeakers,
    pub stats: Stats,
}

impl<B: UiBackend> UiaSource<B> {
    pub fn new(backend: B, platform: Platform, selectors: Selectors, mode: NodeMode) -> Self {
        Self {
            backend,
            platform,
            selectors,
            mode,
            limits: Limits::default(),
            cached: None,
            transient_run: 0,
            known_speakers: KnownSpeakers::new(),
            stats: Stats::default(),
        }
    }

    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    fn ttl(&self) -> Duration {
        let seconds = self.platform.cache_ttl_s(&self.selectors);
        // A hostile or mistyped value must not panic the capture thread.
        Duration::try_from_secs_f64(seconds.max(0.0)).unwrap_or(Duration::from_secs(3600))
    }

    fn parse(&mut self, root: &UiNode) -> Vec<RawObservation> {
        let mut observations =
            self.platform
                .captions(root, &self.selectors, &mut self.known_speakers);
        if self.mode == NodeMode::Last && observations.len() > 1 {
            observations.drain(..observations.len() - 1);
        }
        observations
    }

    /// Maps a failure; repeated transient ones (an oversized tree that never
    /// fits, a container that keeps vanishing) end up as `Lost`.
    fn count_failure(&mut self, error: UiError) -> SourceError {
        let mapped = Self::map_error(error);
        if matches!(mapped, SourceError::Transient(_)) {
            self.transient_run += 1;
            if self.transient_run >= MAX_TRANSIENT {
                self.transient_run = 0;
                return SourceError::Lost("repeated failures reading the window".to_string());
            }
        }
        mapped
    }

    fn map_error(error: UiError) -> SourceError {
        match error {
            UiError::WindowGone => SourceError::Lost("window closed".to_string()),
            UiError::Init(message) => SourceError::Fatal(message),
            other => SourceError::Transient(other.to_string()),
        }
    }

    /// Finds the window and the container of the captions inside it.
    fn locate(&mut self) -> Result<Option<Cached>, UiError> {
        self.stats.discoveries += 1;
        let windows = self.backend.windows()?;
        let mut candidates = self.platform.candidates(&windows, &self.selectors);
        if self.platform.attaches_without_captions() {
            candidates.truncate(1);
        }
        for (index, _) in candidates {
            let window = windows[index].clone();
            if let Some(cached) = self.locate_in(window)? {
                return Ok(Some(cached));
            }
        }
        Ok(None)
    }

    /// The container of the captions in one window; `None` when the platform
    /// only counts a window with captions and this one has none.
    fn locate_in(&mut self, window: WindowInfo) -> Result<Option<Cached>, UiError> {
        let mut budget = Budget::new(
            self.platform.discovery_depth(&self.selectors),
            self.limits.max_nodes,
        );
        budget.deadline = Some(Instant::now() + self.limits.discovery_budget);
        let tree = self.backend.snapshot(
            &Target {
                window: window.handle,
                path: Vec::new(),
                expect: None,
            },
            &budget,
        )?;
        self.note_truncation();
        let path = self.platform.container(&tree, &self.selectors);
        if path.as_ref().is_some_and(Vec::is_empty) {
            self.stats.whole_window_fallbacks += 1;
        }
        if path.is_none() && !self.platform.attaches_without_captions() {
            return Ok(None);
        }
        let container = path.and_then(|path| {
            let ident = NodeIdent::of(tree.at(&path)?);
            Some((path, ident))
        });
        Ok(Some(Cached {
            window,
            container,
            since: Instant::now(),
        }))
    }

    fn note_truncation(&mut self) {
        if self.backend.truncated() {
            self.stats.truncated_snapshots += 1;
        }
    }

    fn budget(&self) -> Budget {
        let mut budget = Budget::new(
            self.platform.poll_depth(&self.selectors),
            self.limits.max_nodes,
        );
        budget.deadline = Some(Instant::now() + self.limits.poll_budget);
        budget
    }

    /// Whether the cached container is still the one to read: not past its
    /// time to live (a window without a container is looked at again sooner).
    fn fresh(&self) -> bool {
        self.cached.as_ref().is_some_and(|c| {
            let limit = if c.container.is_some() {
                self.ttl()
            } else {
                NO_LIST_RETRY
            };
            c.since.elapsed() < limit
        })
    }

    /// The cheap alternative to a full search when the time to live ran out:
    /// the window is still there and the remembered node still looks the same.
    /// Never for the whole-window fallback, which exists to be searched again
    /// (captions may have appeared in a proper container since).
    fn revalidate(&mut self) -> Option<Vec<RawObservation>> {
        let cached = self.cached.as_ref()?;
        let target = cached.target()?;
        if target.path.is_empty() || !self.backend.alive(target.window) {
            return None;
        }
        let tree = self.backend.snapshot(&target, &self.budget()).ok()?;
        self.note_truncation();
        self.stats.revalidations += 1;
        if let Some(cached) = self.cached.as_mut() {
            cached.since = Instant::now();
        }
        Some(self.parse(&tree))
    }
}

impl<B: UiBackend> CaptureSource for UiaSource<B> {
    fn name(&self) -> &str {
        self.platform.label()
    }

    fn discover(&mut self) -> Result<Discovery, SourceError> {
        self.known_speakers.clear();
        match self.locate() {
            Ok(Some(cached)) => {
                let title = cached.window.title.clone();
                self.cached = Some(cached);
                self.transient_run = 0;
                Ok(Discovery::Attached { window: title })
            }
            Ok(None) => {
                self.cached = None;
                Ok(Discovery::Searching)
            }
            Err(error) => Err(Self::map_error(error)),
        }
    }

    fn poll(&mut self) -> Result<Vec<RawObservation>, SourceError> {
        self.stats.polls += 1;
        let result = self.poll_inner();
        self.backend.after_poll();
        result
    }

    fn window_pid(&self) -> Option<u32> {
        Some(self.cached.as_ref()?.window.pid).filter(|pid| *pid != 0)
    }

    fn meeting_name(&self) -> Option<String> {
        let title = &self.cached.as_ref()?.window.title;
        self.platform.meeting_name(title, &self.selectors)
    }
}

impl<B: UiBackend> UiaSource<B> {
    fn poll_inner(&mut self) -> Result<Vec<RawObservation>, SourceError> {
        if self.fresh() {
            self.stats.cache_hits += 1;
        } else if let Some(observations) = self.revalidate() {
            self.transient_run = 0;
            return Ok(observations);
        } else {
            match self.locate() {
                Ok(Some(cached)) => self.cached = Some(cached),
                Ok(None) => {
                    self.cached = None;
                    return Err(SourceError::Lost("no meeting window".to_string()));
                }
                Err(error) => return Err(self.count_failure(error)),
            }
        }
        let Some(cached) = self.cached.as_ref() else {
            return Err(SourceError::Lost("no meeting window".to_string()));
        };
        let Some(target) = cached.target() else {
            // A window without a caption list yet: nothing to read.
            return Ok(Vec::new());
        };
        match self.backend.snapshot(&target, &self.budget()) {
            Ok(tree) => {
                self.note_truncation();
                self.transient_run = 0;
                Ok(self.parse(&tree))
            }
            Err(UiError::WindowGone) => {
                self.cached = None;
                Err(Self::map_error(UiError::WindowGone))
            }
            Err(error @ (UiError::Stale | UiError::Budget)) => {
                // Look for the container again next time.
                self.cached = None;
                Err(self.count_failure(error))
            }
            Err(error) => Err(Self::map_error(error)),
        }
    }
}
