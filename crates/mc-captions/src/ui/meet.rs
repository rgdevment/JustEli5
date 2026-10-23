//! Google Meet in a browser: window choice and captions. Every default here is
//! unverified until a real dump (`mc doctor --dump --platform meet`).

use mc_core::RawObservation;

use super::blocks::{self, KnownSpeakers, NameRule};
use super::browser::{is_browser, page_title};
use super::node::{UiNode, WindowInfo};
use super::selectors::{BrowserSel, MeetSel};
use super::teams::py_strip;

fn prefix_len(title: &str, sel: &MeetSel) -> Option<usize> {
    let title = title.as_bytes();
    sel.title_prefixes.iter().find_map(|p| {
        let p = p.as_bytes();
        title
            .get(..p.len())
            .filter(|head| head.eq_ignore_ascii_case(p))
            .map(|_| p.len())
    })
}

/// A browser window whose page is a Meet meeting ("Meet - abc-defg-hij").
pub fn is_window(window: &WindowInfo, browser: &BrowserSel, sel: &MeetSel) -> bool {
    is_browser(window, browser) && prefix_len(&window.title, sel).is_some()
}

/// The first (topmost) Meet window.
pub fn pick_window(windows: &[WindowInfo], browser: &BrowserSel, sel: &MeetSel) -> Option<usize> {
    windows.iter().position(|w| is_window(w, browser, sel))
}

pub fn meeting_name(title: &str, browser: &BrowserSel, sel: &MeetSel) -> Option<String> {
    let page = page_title(title, browser);
    let name = py_strip(&page[prefix_len(page, sel).unwrap_or(0)..]);
    (!name.is_empty()).then(|| name.to_string())
}

pub fn find_container(window: &UiNode, sel: &MeetSel) -> Option<Vec<usize>> {
    blocks::find_region(window, &sel.region_names, sel.search_depth)
}

pub fn captions(region: &UiNode, sel: &MeetSel, known: &mut KnownSpeakers) -> Vec<RawObservation> {
    blocks::captions(
        region,
        &sel.region_names,
        &sel.noise,
        sel.walk_depth,
        known,
        &NameRule {
            max_words: sel.name_max_words,
            punctuation: &sel.sentence_punctuation,
        },
    )
}
