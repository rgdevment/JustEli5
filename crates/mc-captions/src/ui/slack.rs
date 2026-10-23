//! Slack huddles, in the desktop app or a browser tab. Every default here is
//! unverified until a real dump (`mc doctor --dump --platform slack`).

use mc_core::RawObservation;

use super::blocks::{self, KnownSpeakers, NameRule};
use super::browser::{is_browser, page_title};
use super::node::{UiNode, WindowInfo};
use super::selectors::{BrowserSel, SlackSel};
use super::teams::py_strip;

fn has_any(title: &str, hints: &[String]) -> bool {
    let title = title.to_lowercase();
    hints.iter().any(|h| title.contains(&h.to_lowercase()))
}

/// "Slack | ..." or "... | Slack" / "... - Slack": a page named after Slack, not one that mentions it.
fn slack_shaped(page: &str, hints: &[String]) -> bool {
    let page = page.trim().to_lowercase();
    hints.iter().any(|hint| {
        let hint = hint.to_lowercase();
        page == hint
            || ["|", "-", "\u{2013}", "\u{2014}"].iter().any(|sep| {
                page.starts_with(&format!("{hint} {sep} "))
                    || page.ends_with(&format!(" {sep} {hint}"))
            })
    })
}

fn is_app_window(window: &WindowInfo, sel: &SlackSel) -> bool {
    sel.app_classes.contains(&window.class_name)
        && window
            .process
            .as_deref()
            .is_some_and(|p| sel.app_processes.iter().any(|a| a.eq_ignore_ascii_case(p)))
}

/// A window of the Slack app, or a browser tab titled like Slack.
pub fn is_window(window: &WindowInfo, browser: &BrowserSel, sel: &SlackSel) -> bool {
    is_app_window(window, sel)
        || (is_browser(window, browser)
            && slack_shaped(page_title(&window.title, browser), &sel.title_hints))
}

/// Slack windows in the order to try them: huddle windows first. Whether one
/// really holds a huddle is decided by its captions.
pub fn candidates(
    windows: &[WindowInfo],
    browser: &BrowserSel,
    sel: &SlackSel,
) -> Vec<(usize, bool)> {
    let slack: Vec<usize> = (0..windows.len())
        .filter(|i| is_window(&windows[*i], browser, sel))
        .collect();
    let is_huddle = |i: &usize| has_any(&windows[*i].title, &sel.huddle_hints);
    let huddles = slack.iter().filter(|i| is_huddle(i)).map(|i| (*i, true));
    let others = slack.iter().filter(|i| !is_huddle(i)).map(|i| (*i, false));
    huddles.chain(others).collect()
}

pub fn meeting_name(title: &str, browser: &BrowserSel) -> Option<String> {
    let name = py_strip(page_title(title, browser));
    (!name.is_empty()).then(|| name.to_string())
}

pub fn find_container(window: &UiNode, sel: &SlackSel) -> Option<Vec<usize>> {
    blocks::find_region(window, &sel.region_names, sel.search_depth)
}

pub fn captions(region: &UiNode, sel: &SlackSel, known: &mut KnownSpeakers) -> Vec<RawObservation> {
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
