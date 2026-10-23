//! Zoom: window choice, caption list and caption items.

use std::sync::LazyLock;

use mc_core::RawObservation;
use regex::Regex;

use super::node::{Role, UiNode, WindowInfo};
use super::selectors::ZoomSel;
use super::teams::{find_first, py_strip};

/// `Speaker 2 (Room), text` or `Some Name, text`. The text part is group 1.
/// Python's `\s` also matches U+001C..U+001F, which Rust's does not, hence the
/// explicit range.
static ITEM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:Speaker \d+[\s\x1C-\x1F]*\([^)]*\)|[^,]{2,40}),[\s\x1C-\x1F]*(.+)")
        .expect("static regex")
});

/// Index of the meeting window: a known meeting class first, then a title with
/// a meeting hint (never the Zoom home window).
pub fn pick_window(windows: &[WindowInfo], sel: &ZoomSel) -> Option<usize> {
    for class in &sel.meeting_classes {
        if let Some(index) = windows.iter().position(|w| &w.class_name == class) {
            return Some(index);
        }
    }
    windows.iter().position(|w| {
        let title = w.title.to_lowercase();
        sel.title_hints
            .iter()
            .any(|h| title.contains(&h.to_lowercase()))
            && w.class_name != sel.home_class
            && !sel.exclude_classes.contains(&w.class_name)
    })
}

pub fn meeting_name(title: &str) -> Option<String> {
    (!title.is_empty()).then(|| title.to_string())
}

/// Path of the caption list inside the meeting window.
pub fn find_container(window: &UiNode, sel: &ZoomSel) -> Option<Vec<usize>> {
    let caption_window =
        |n: &UiNode| n.role == Role::Window && n.class_name == sel.caption_window_class;
    if let Some(mut path) = find_first(window, sel.caption_window_depth, &caption_window)
        && let Some(node) = window.at(&path)
        && let Some(sub) = find_first(node, sel.list_depth, &|n: &UiNode| n.role == Role::List)
    {
        path.extend(sub);
        return Some(path);
    }
    // Fallback: any list whose name mentions captions, pre-order walk.
    fn walk(node: &UiNode, path: &mut Vec<usize>, depth: usize, max: usize, sel: &ZoomSel) -> bool {
        for (index, child) in node.children.iter().enumerate() {
            if depth > max {
                return false;
            }
            path.push(index);
            let name = child.name.to_lowercase();
            if is_list(child)
                && sel
                    .list_name_hints
                    .iter()
                    .any(|h| name.contains(&h.to_lowercase()))
            {
                return true;
            }
            if walk(child, path, depth + 1, max, sel) {
                return true;
            }
            path.pop();
        }
        false
    }
    let mut path = Vec::new();
    walk(window, &mut path, 1, sel.fallback_depth, sel).then_some(path)
}

/// A list-like node: `AXList` is `List`, while `AXTable`/`AXOutline` (macOS) map to `Table`/`Tree`.
fn is_list(node: &UiNode) -> bool {
    node.role == Role::List || matches!(&node.role, Role::Other(n) if n == "Table" || n == "Tree")
}

/// macOS row shape (assumed, unverified): `AXRow` holding `AXStaticText` directly or inside `AXCell`s; two texts are
/// speaker and caption, one is "Speaker, caption" or just the caption.
fn row_texts(row: &UiNode) -> Option<(Option<String>, String)> {
    let mut texts = Vec::new();
    for child in &row.children {
        match &child.role {
            Role::Text => texts.push(child),
            Role::Other(n) if n == "DataItem" => {
                texts.extend(child.children.iter().filter(|c| c.role == Role::Text));
            }
            _ => {}
        }
    }
    let mut names = texts
        .iter()
        .map(|t| t.name.as_str())
        .filter(|n| !n.is_empty());
    let first = names.next()?;
    match names.max_by_key(|n| n.chars().count()) {
        Some(caption) => Some((Some(py_strip(first).to_string()), caption.to_string())),
        None => match ITEM.captures(first).and_then(|c| c.get(1)) {
            Some(group) => {
                let head = first[..group.start()].trim_end_matches([' ', ',']);
                Some((Some(py_strip(head).to_string()), group.as_str().to_string()))
            }
            None => Some((None, first.to_string())),
        },
    }
}

/// `(speaker, text)` of one caption item, or `None` when there is no usable text.
pub fn parse_item(item: &UiNode) -> Option<(Option<String>, String)> {
    let mut text = String::new();
    for pane in item.children.iter().filter(|c| c.role == Role::Pane) {
        for edit in pane.children.iter().filter(|c| c.role == Role::Edit) {
            if edit.name.chars().count() > text.chars().count() {
                text.clone_from(&edit.name);
            }
        }
    }
    if text.is_empty() {
        text.clone_from(&item.name);
    }
    if text.is_empty()
        && let Some((speaker, text)) = row_texts(item)
    {
        let text = py_strip(&text);
        return (text.chars().count() > 2).then(|| (speaker, text.to_string()));
    }
    let mut speaker = None;
    if let Some(captures) = ITEM.captures(&item.name)
        && let Some(group) = captures.get(1)
    {
        if text.is_empty() {
            text = group.as_str().to_string();
        }
        // Python slices `name[: start(1) - 2]` in code points, assuming ", ".
        let start = item.name[..group.start()].chars().count();
        let head: String = item.name.chars().take(start.saturating_sub(2)).collect();
        speaker = Some(py_strip(&head).trim_end_matches(',').to_string());
    }
    let text = py_strip(&text);
    (text.chars().count() > 2).then(|| (speaker, text.to_string()))
}

pub fn captions(list: &UiNode) -> Vec<RawObservation> {
    list.children
        .iter()
        .filter_map(parse_item)
        .map(|(speaker, text)| RawObservation {
            speaker,
            text: Some(text),
            node_id: None,
        })
        .collect()
}

#[cfg(test)]
mod mac_tests {
    use super::*;

    fn text(name: &str) -> UiNode {
        UiNode::new(Role::Text, name)
    }

    fn row(children: Vec<UiNode>) -> UiNode {
        UiNode::new(Role::ListItem, "").with_children(children)
    }

    fn cell(children: Vec<UiNode>) -> UiNode {
        UiNode::new(Role::Other("DataItem".into()), "").with_children(children)
    }

    #[test]
    fn finds_a_macos_caption_table_by_its_name() {
        let table = UiNode::new(Role::Other("Table".into()), "Closed Captions");
        let window = UiNode::new(Role::Window, "Zoom Meeting").with_children(vec![
            UiNode::new(Role::Group, "").with_children(vec![table]),
        ]);
        let path = find_container(&window, &ZoomSel::default());
        assert_eq!(path, Some(vec![0, 0]));
    }

    #[test]
    fn rows_of_cells_with_speaker_and_text() {
        let item = row(vec![
            cell(vec![text("Ana")]),
            cell(vec![text("hello everyone")]),
        ]);
        assert_eq!(
            parse_item(&item),
            Some((Some("Ana".into()), "hello everyone".into()))
        );
    }

    #[test]
    fn a_row_with_one_text_splits_speaker_and_caption() {
        let item = row(vec![text("Ana Gomez, let us begin")]);
        assert_eq!(
            parse_item(&item),
            Some((Some("Ana Gomez".into()), "let us begin".into()))
        );
        assert_eq!(
            parse_item(&row(vec![text("no speaker here")])).unwrap().0,
            None
        );
    }

    #[test]
    fn empty_rows_give_nothing() {
        assert_eq!(parse_item(&row(vec![cell(vec![text("")])])), None);
        let list = UiNode::new(Role::List, "").with_children(vec![row(vec![text("Bo, hi there")])]);
        assert_eq!(captions(&list).len(), 1);
    }
}
