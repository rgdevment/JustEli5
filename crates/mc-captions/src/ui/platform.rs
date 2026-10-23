//! The meeting platforms and what differs between them: how a window is
//! chosen, where its captions live and how they are read. One place maps a
//! platform to its parser, so the source, the doctor report and the
//! anonymiser stay free of per-platform branches.

use mc_core::RawObservation;

use super::blocks::KnownSpeakers;
use super::browser::page_title;
use super::node::{UiNode, WindowInfo};
use super::selectors::Selectors;
use super::{meet, slack, teams, teams_web, zoom};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Teams,
    Zoom,
    /// Google Meet in a browser.
    Meet,
    /// Teams in a browser.
    TeamsWeb,
    /// Slack huddles (desktop app or browser).
    Slack,
}

impl Platform {
    pub const ALL: [Self; 5] = [
        Self::Teams,
        Self::Zoom,
        Self::Meet,
        Self::TeamsWeb,
        Self::Slack,
    ];

    /// The name used on the command line and in fixtures.
    pub fn name(self) -> &'static str {
        match self {
            Self::Teams => "teams",
            Self::Zoom => "zoom",
            Self::Meet => "meet",
            Self::TeamsWeb => "teams-web",
            Self::Slack => "slack",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    /// The capture source's name.
    pub fn label(self) -> &'static str {
        match self {
            Self::Teams => "TeamsUia",
            Self::Zoom => "ZoomUia",
            Self::Meet => "MeetUia",
            Self::TeamsWeb => "TeamsWebUia",
            Self::Slack => "SlackUia",
        }
    }

    /// The application, for dump files.
    pub fn app(self) -> &'static str {
        match self {
            Self::Teams => "Microsoft Teams",
            Self::Zoom => "Zoom",
            Self::Meet => "Google Meet",
            Self::TeamsWeb => "Microsoft Teams (web)",
            Self::Slack => "Slack",
        }
    }

    /// Whether the window is attached before its captions are found. A meeting
    /// of the others is told by its window; a Slack window says nothing about a huddle.
    pub fn attaches_without_captions(self) -> bool {
        self != Self::Slack
    }

    /// Whether `window` can be (part of) this platform's meeting.
    pub fn is_window(self, window: &WindowInfo, sel: &Selectors) -> bool {
        match self {
            Self::Teams => sel.teams.classes.contains(&window.class_name),
            Self::Zoom => {
                let z = &sel.zoom;
                let title = window.title.to_lowercase();
                z.meeting_classes.contains(&window.class_name)
                    || window.class_name == z.caption_window_class
                    || window.class_name == z.home_class
                    || z.title_hints
                        .iter()
                        .any(|h| title.contains(&h.to_lowercase()))
            }
            Self::Meet => meet::is_window(window, &sel.browser, &sel.meet),
            Self::TeamsWeb => teams_web::is_window(window, sel),
            Self::Slack => slack::is_window(window, &sel.browser, &sel.slack),
        }
    }

    /// Index of the window to read and why it was chosen.
    pub fn pick(self, windows: &[WindowInfo], sel: &Selectors) -> Option<(usize, String)> {
        self.candidates(windows, sel).into_iter().next()
    }

    /// The windows worth reading, best first. Only Slack has several: its
    /// windows say nothing about a huddle, so the one with captions is found by trying them.
    pub fn candidates(self, windows: &[WindowInfo], sel: &Selectors) -> Vec<(usize, String)> {
        if self == Self::Slack {
            return slack::candidates(windows, &sel.browser, &sel.slack)
                .into_iter()
                .map(|(i, huddle)| (i, if huddle { "Huddle" } else { "Window" }.into()))
                .collect();
        }
        self.pick_one(windows, sel).into_iter().collect()
    }

    fn pick_one(self, windows: &[WindowInfo], sel: &Selectors) -> Option<(usize, String)> {
        match self {
            Self::Teams => teams::pick_window_tier(windows, &sel.teams)
                .map(|(i, tier)| (i, format!("{tier:?}"))),
            Self::Zoom => zoom::pick_window(windows, &sel.zoom).map(|i| (i, "Meeting".into())),
            Self::Meet => {
                meet::pick_window(windows, &sel.browser, &sel.meet).map(|i| (i, "Meeting".into()))
            }
            Self::TeamsWeb => {
                teams_web::pick_window_tier(windows, sel).map(|(i, tier)| (i, format!("{tier:?}")))
            }
            Self::Slack => None,
        }
    }

    /// Path of the captions container inside the window. For the Teams family
    /// an empty path (the whole window) is the fallback when none is found.
    pub fn container(self, window: &UiNode, sel: &Selectors) -> Option<Vec<usize>> {
        match self {
            Self::Teams | Self::TeamsWeb => Some(teams::find_container(window, &sel.teams)),
            Self::Zoom => zoom::find_container(window, &sel.zoom),
            Self::Meet => meet::find_container(window, &sel.meet),
            Self::Slack => slack::find_container(window, &sel.slack),
        }
    }

    /// `known` remembers the speakers seen so far, so a flattened region reads the same on every poll.
    pub fn captions(
        self,
        container: &UiNode,
        sel: &Selectors,
        known: &mut KnownSpeakers,
    ) -> Vec<RawObservation> {
        match self {
            Self::Teams | Self::TeamsWeb => teams::captions(container, &sel.teams),
            Self::Zoom => zoom::captions(container),
            Self::Meet => meet::captions(container, &sel.meet, known),
            Self::Slack => slack::captions(container, &sel.slack, known),
        }
    }

    pub fn meeting_name(self, title: &str, sel: &Selectors) -> Option<String> {
        match self {
            Self::Teams => teams::meeting_name(title, &sel.teams),
            Self::TeamsWeb => teams::meeting_name(page_title(title, &sel.browser), &sel.teams),
            Self::Zoom => zoom::meeting_name(title),
            Self::Meet => meet::meeting_name(title, &sel.browser, &sel.meet),
            Self::Slack => slack::meeting_name(title, &sel.browser),
        }
    }

    pub fn cache_ttl_s(self, sel: &Selectors) -> f64 {
        match self {
            Self::Teams => sel.teams.cache_ttl_s,
            Self::Zoom => sel.zoom.cache_ttl_s,
            Self::Meet => sel.meet.cache_ttl_s,
            Self::TeamsWeb => sel.teams_web.cache_ttl_s,
            Self::Slack => sel.slack.cache_ttl_s,
        }
    }

    /// Levels below the window that discovery must read: Python searches for
    /// the Teams container within some depth and then walks below it, and its
    /// `GetChildren()` on the deepest group reads one more level.
    pub fn discovery_depth(self, sel: &Selectors) -> usize {
        match self {
            Self::Teams | Self::TeamsWeb => {
                let t = &sel.teams;
                let root = t.root_depths.iter().copied().max().unwrap_or(20);
                let walk = t.walk_depths.iter().copied().max().unwrap_or(25);
                root + walk + 1
            }
            Self::Zoom => {
                let z = &sel.zoom;
                (z.caption_window_depth + z.list_depth).max(z.fallback_depth) + 1
            }
            Self::Meet => sel.meet.search_depth,
            Self::Slack => sel.slack.search_depth,
        }
    }

    /// Levels below the container that a poll reads.
    pub fn poll_depth(self, sel: &Selectors) -> usize {
        match self {
            Self::Teams | Self::TeamsWeb => {
                sel.teams.walk_depths.iter().copied().max().unwrap_or(25) + 1
            }
            // list -> item -> pane -> edit
            Self::Zoom => 3,
            Self::Meet => sel.meet.walk_depth,
            Self::Slack => sel.slack.walk_depth,
        }
    }

    /// Whether the tree is Chromium's, which is tiny while web accessibility is off.
    pub fn is_chromium(self) -> bool {
        self != Self::Zoom
    }
}
