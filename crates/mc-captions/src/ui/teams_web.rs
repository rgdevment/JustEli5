//! Teams in a browser tab. The page is the same web app as the desktop
//! WebView2, so the tree is read with the Teams parser; only the window choice
//! differs: a window of a browser process (never the desktop app or another Electron app) titled Teams.

use super::browser::is_browser;
use super::node::WindowInfo;
use super::selectors::Selectors;
use super::teams::{Tier, pick_by};

pub fn is_window(window: &WindowInfo, selectors: &Selectors) -> bool {
    let title = window.title.to_lowercase();
    is_browser(window, &selectors.browser)
        && selectors
            .teams_web
            .title_hints
            .iter()
            .any(|h| title.contains(&h.to_lowercase()))
}

pub fn pick_window_tier(windows: &[WindowInfo], selectors: &Selectors) -> Option<(usize, Tier)> {
    let eligible = |w: &WindowInfo| is_browser(w, &selectors.browser);
    pick_by(
        windows,
        &selectors.teams,
        &eligible,
        &selectors.teams_web.title_hints,
    )
}
