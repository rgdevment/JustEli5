//! Microsoft Teams: window choice, caption container and caption parsing.
//! Pure functions over [`UiNode`] trees; the rules are the Python
//! implementation's, with the strings moved into [`TeamsSel`].

use mc_core::RawObservation;
use mc_core::pytext::py_isspace;

use super::node::{Role, UiNode, WindowInfo};
use super::selectors::TeamsSel;

fn lower(text: &str) -> String {
    text.to_lowercase()
}

/// Python's `str.strip()`.
pub fn py_strip(text: &str) -> &str {
    text.trim_matches(py_isspace)
}

/// Index of the window to read, or `None`. Priority: caption pop-out (the last
/// one), then a meeting window (the last one), then the first other Teams
/// window; chat-like windows never qualify.
pub fn pick_window(windows: &[WindowInfo], sel: &TeamsSel) -> Option<usize> {
    pick_window_tier(windows, sel).map(|(index, _)| index)
}

/// Why a Teams window was chosen (for diagnostics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    CaptionPopout,
    Meeting,
    Fallback,
}

/// [`pick_window`] with the reason.
pub fn pick_window_tier(windows: &[WindowInfo], sel: &TeamsSel) -> Option<(usize, Tier)> {
    let is_teams = |w: &WindowInfo| sel.classes.contains(&w.class_name);
    pick_by(
        windows,
        sel,
        &is_teams,
        std::slice::from_ref(&sel.required_title),
    )
}

/// The tiered choice over the windows `eligible` accepts whose title has one of
/// `required` (Teams in a browser reuses it with browser windows).
pub fn pick_by(
    windows: &[WindowInfo],
    sel: &TeamsSel,
    eligible: &dyn Fn(&WindowInfo) -> bool,
    required: &[String],
) -> Option<(usize, Tier)> {
    let (mut caption, mut meeting, mut fallback) = (None, None, None);
    for (index, window) in windows.iter().enumerate() {
        if !eligible(window) {
            continue;
        }
        let title = lower(&window.title);
        // Selector strings are compared lowercased, whatever the user wrote.
        let has = |keys: &[String]| keys.iter().any(|k| title.contains(&lower(k)));
        if title.is_empty() || !has(required) {
            continue;
        }
        if has(&sel.reject_keywords) {
            continue;
        }
        if has(&sel.caption_keywords) {
            caption = Some(index);
        } else if has(&sel.meeting_keywords) {
            meeting = Some(index);
        } else if fallback.is_none() {
            fallback = Some(index);
        }
    }
    caption
        .map(|i| (i, Tier::CaptionPopout))
        .or(meeting.map(|i| (i, Tier::Meeting)))
        .or(fallback.map(|i| (i, Tier::Fallback)))
}

/// The meeting's name from the window title, without Teams' own decorations.
pub fn meeting_name(title: &str, sel: &TeamsSel) -> Option<String> {
    let mut name = title;
    for prefix in &sel.title_prefixes {
        if let Some(rest) = name.strip_prefix(prefix.as_str()) {
            name = rest;
        }
    }
    for suffix in &sel.title_suffixes {
        if let Some(rest) = name.strip_suffix(suffix.as_str()) {
            name = rest;
        }
    }
    let name = py_strip(name);
    (!name.is_empty()).then(|| name.to_string())
}

/// Whether `name` can be a participant's name. A profile card hovered during the
/// meeting is also a group of two texts, but its labels and bios are not people.
/// An empty name is an anonymous guest, which the captions keep.
pub fn is_speaker_name(name: &str, sel: &TeamsSel) -> bool {
    let name = py_strip(name);
    let words = name.split_whitespace().count();
    words <= sel.max_name_words
        && name.chars().count() <= sel.max_name_chars
        && !name
            .chars()
            .any(|c| c.is_ascii_digit() || ",:;?!".contains(c))
        && !sel.card_labels.iter().any(|l| lower(l) == lower(name))
}

/// `(speaker, caption)` of a caption group, or `None`.
fn parse_group(node: &UiNode, sel: &TeamsSel) -> Option<(String, String)> {
    if node.role != Role::Group || node.children.len() < 2 {
        return None;
    }
    let mut texts = node.children.iter().filter(|c| c.role == Role::Text);
    let speaker = texts.next()?;
    let caption = texts.next()?;
    if caption.name.is_empty()
        || sel.noise.iter().any(|n| caption.name.contains(n.as_str()))
        || !is_speaker_name(&speaker.name, sel)
    {
        return None;
    }
    Some((speaker.name.clone(), caption.name.clone()))
}

fn collect(
    node: &UiNode,
    depth: usize,
    max_depth: usize,
    sel: &TeamsSel,
    out: &mut Vec<(String, String)>,
) {
    for child in &node.children {
        if depth > max_depth {
            return;
        }
        if let Some(caption) = parse_group(child, sel) {
            out.push(caption);
        }
        collect(child, depth + 1, max_depth, sel, out);
    }
}

/// The captions under `root`, in document order. Deeper searches are tried
/// only when a shallower one finds nothing.
pub fn captions(root: &UiNode, sel: &TeamsSel) -> Vec<RawObservation> {
    for &max_depth in &sel.walk_depths {
        let mut found = Vec::new();
        collect(root, 1, max_depth, sel, &mut found);
        if !found.is_empty() {
            return found
                .into_iter()
                .map(|(speaker, text)| RawObservation {
                    speaker: Some(speaker),
                    text: Some(text),
                    node_id: None,
                })
                .collect();
        }
    }
    Vec::new()
}

/// Depth-first pre-order search below `root` (the root itself excluded) down to
/// `max_depth` levels, like `uiautomation`'s `FindControl`: children of `root`
/// are at depth 1 and the first match in document order wins. Returns the
/// child-index path of the match.
pub fn find_first(
    root: &UiNode,
    max_depth: usize,
    pred: &dyn Fn(&UiNode) -> bool,
) -> Option<Vec<usize>> {
    fn descend(
        node: &UiNode,
        depth: usize,
        max_depth: usize,
        pred: &dyn Fn(&UiNode) -> bool,
        path: &mut Vec<usize>,
    ) -> bool {
        for (index, child) in node.children.iter().enumerate() {
            if depth > max_depth {
                return false;
            }
            path.push(index);
            if pred(child) || descend(child, depth + 1, max_depth, pred, path) {
                return true;
            }
            path.pop();
        }
        false
    }
    let mut path = Vec::new();
    descend(root, 1, max_depth, pred, &mut path).then_some(path)
}

/// Where the captions live inside a window: the `RootWebArea` document that
/// really has captions, else the window itself (empty path).
pub fn find_container(window: &UiNode, sel: &TeamsSel) -> Vec<usize> {
    for &depth in &sel.root_depths {
        let is_root =
            |n: &UiNode| n.role == Role::Document && n.automation_id == sel.root_automation_id;
        if let Some(path) = find_first(window, depth, &is_root)
            && let Some(node) = window.at(&path)
            && !captions(node, sel).is_empty()
        {
            return path;
        }
    }
    Vec::new()
}
