//! Anonymises an accessibility-tree dump so it can be shared and committed as a
//! test fixture, without changing how the parsers read it.
//!
//! What is kept: roles, identifier-like automation ids and class names, window
//! order, the selector, noise and title keywords (so the same window is picked
//! at the same tier), punctuation, digits, whitespace and every string's length
//! in code points. What is replaced: letters (by `x`/`X`), speaker names (by
//! `Speaker A`, `Speaker B`, ... in order of first appearance), handles, pids.

use std::collections::HashMap;

use super::blocks::KnownSpeakers;
use super::fake::{FakeWindow, Fixture};
use super::node::UiNode;
use super::platform::Platform;
use super::selectors::Selectors;

/// Strings that must survive masking, lowercased.
fn protected(selectors: &Selectors) -> Vec<Vec<char>> {
    let t = &selectors.teams;
    let z = &selectors.zoom;
    let mut words: Vec<String> = Vec::new();
    for list in [
        &selectors.meet.title_prefixes,
        &selectors.meet.region_names,
        &selectors.meet.noise,
        &selectors.teams_web.title_hints,
        &selectors.slack.title_hints,
        &selectors.slack.huddle_hints,
        &selectors.slack.region_names,
        &selectors.slack.noise,
        &selectors.browser.names,
        &t.caption_keywords,
        &t.meeting_keywords,
        &t.reject_keywords,
        &t.noise,
        &t.title_prefixes,
        &t.title_suffixes,
        &z.title_hints,
        &z.list_name_hints,
    ] {
        words.extend(list.iter().cloned());
    }
    words.push(t.required_title.clone());
    words.push("zoom".to_string());
    words.push("unknown user".to_string());
    words.push("usuario desconocido".to_string());
    words.push("speaker ".to_string());
    let mut out: Vec<Vec<char>> = words
        .iter()
        .map(|w| w.trim().to_lowercase().chars().collect::<Vec<_>>())
        .filter(|w| !w.is_empty())
        .collect();
    // Longest first, so "unknown user" wins over a shorter overlapping word.
    out.sort_by_key(|w| std::cmp::Reverse(w.len()));
    out
}

fn lower_char(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Replaces letters by `x`/`X`, keeping everything else and the protected words.
fn mask(text: &str, keep: &[Vec<char>]) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let kept = keep.iter().find(|word| {
            if word.len() > chars.len() - i
                || !word
                    .iter()
                    .enumerate()
                    .all(|(k, w)| lower_char(chars[i + k]) == *w)
            {
                return false;
            }
            // A keyword inside a longer word ("Callahan") is not the keyword.
            let first_edge = word.first().is_some_and(|c| c.is_alphanumeric())
                && i > 0
                && chars[i - 1].is_alphanumeric();
            let last_edge = word.last().is_some_and(|c| c.is_alphanumeric())
                && chars
                    .get(i + word.len())
                    .is_some_and(|c| c.is_alphanumeric());
            !first_edge && !last_edge
        });
        if let Some(word) = kept {
            out.extend(&chars[i..i + word.len()]);
            i += word.len();
            continue;
        }
        let c = chars[i];
        out.push(if c.is_uppercase() {
            'X'
        } else if c.is_alphabetic() {
            'x'
        } else if c.is_numeric() {
            // Phone numbers, meeting ids, passcodes, dates: none survive.
            '0'
        } else {
            c
        });
        i += 1;
    }
    out
}

/// Process names that are fine to keep in a shared dump.
const KNOWN_PROCESSES: [&str; 20] = [
    "ms-teams.exe",
    "teams.exe",
    "zoom.exe",
    "msedgewebview2.exe",
    "slack.exe",
    "chrome.exe",
    "msedge.exe",
    "brave.exe",
    "firefox.exe",
    "opera.exe",
    "vivaldi.exe",
    "arc.exe",
    // macOS bundle names.
    "Microsoft Teams",
    "zoom.us",
    "Slack",
    "Google Chrome",
    "Microsoft Edge",
    "Brave Browser",
    "Firefox",
    "Safari",
];

fn known_idents(selectors: &Selectors) -> Vec<String> {
    let t = &selectors.teams;
    let z = &selectors.zoom;
    let mut idents: Vec<String> = t.classes.clone();
    idents.push(t.root_automation_id.clone());
    idents.extend(z.meeting_classes.iter().cloned());
    idents.extend(z.exclude_classes.iter().cloned());
    idents.push(z.home_class.clone());
    idents.push(z.caption_window_class.clone());
    idents.extend(selectors.browser.classes.iter().cloned());
    idents.extend(selectors.slack.app_classes.iter().cloned());
    idents
}

fn identifier_like(text: &str) -> bool {
    text.len() <= 64
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn speaker_names(fixture: &Fixture, selectors: &Selectors) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut add = |name: String| {
        if !name.trim().is_empty() && !names.contains(&name) {
            names.push(name);
        }
    };
    for window in &fixture.windows {
        // Read where the sources read: the caption container, not the window.
        // Only the platforms the window can belong to: another one would read its interface as speakers.
        for platform in Platform::ALL {
            if !platform.is_window(&window.info, selectors) {
                continue;
            }
            let Some(path) = platform.container(&window.tree, selectors) else {
                continue;
            };
            if let Some(root) = window.tree.at(&path) {
                for caption in platform.captions(root, selectors, &mut KnownSpeakers::new()) {
                    add(caption.speaker.unwrap_or_default());
                }
            }
        }
    }
    names
}

/// Replaces whole-word occurrences of `name` (not inside a longer word).
fn replace_word(text: &str, name: &str, marker: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (start, _) in text.match_indices(name) {
        let end = start + name.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        let boundary = before.is_none_or(|c| !c.is_alphanumeric())
            && after.is_none_or(|c| !c.is_alphanumeric());
        if boundary && start >= last {
            out.push_str(&text[last..start]);
            out.push_str(marker);
            last = end;
        }
    }
    out.push_str(&text[last..]);
    out
}

fn pseudonym(index: usize) -> String {
    let letters = (b'A'..=b'Z').map(char::from).collect::<Vec<_>>();
    let (q, r) = (index / letters.len(), index % letters.len());
    if q == 0 {
        format!("Speaker {}", letters[r])
    } else {
        format!("Speaker {}{}", letters[q - 1], letters[r])
    }
}

struct Anonymiser<'a> {
    keep: Vec<Vec<char>>,
    /// Automation ids and class names the parsers (or the picker) read; every
    /// other one is masked, as ids can carry user or thread identifiers.
    idents: Vec<String>,
    speakers: &'a [(String, String)],
}

impl Anonymiser<'_> {
    /// Speaker names first (so they become their pseudonym), then the mask.
    fn text(&self, text: &str) -> String {
        let mut out = text.to_string();
        let mut placeholders: Vec<(String, String)> = Vec::new();
        // Longest names first, so "Ana García" is not split by "Ana".
        let mut order: Vec<usize> = (0..self.speakers.len()).collect();
        order.sort_by_key(|i| std::cmp::Reverse(self.speakers[*i].0.chars().count()));
        for index in order {
            let (name, pseudo) = &self.speakers[index];
            if name.is_empty() {
                continue;
            }
            // Letters-free private-use marker: it survives masking and cannot
            // be matched by a later (possibly one-letter) name.
            let tag =
                char::from_u32(0xF100 + u32::try_from(index).unwrap_or(0)).unwrap_or('\u{F100}');
            let marker = format!("\u{F000}{tag}\u{F001}");
            let replaced = replace_word(&out, name, &marker);
            if replaced != out {
                out = replaced;
                placeholders.push((marker, pseudo.clone()));
            }
        }
        // Keep the markers out of the masking: split around them.
        let mut result = String::new();
        let mut rest = out.as_str();
        while let Some(start) = rest.find('\u{F000}') {
            result.push_str(&mask(&rest[..start], &self.keep));
            let end = rest[start..]
                .find('\u{F001}')
                .map_or(rest.len(), |e| start + e + '\u{F001}'.len_utf8());
            result.push_str(&rest[start..end]);
            rest = &rest[end..];
        }
        result.push_str(&mask(rest, &self.keep));
        for (marker, pseudo) in placeholders {
            result = result.replace(&marker, &pseudo);
        }
        result
    }

    /// Verbatim only when the selectors (or the allowlist) know the value.
    fn ident(&self, value: &str) -> String {
        if value.is_empty() || (identifier_like(value) && self.idents.iter().any(|i| i == value)) {
            value.to_string()
        } else {
            mask(value, &[])
        }
    }

    fn node(&self, node: &UiNode) -> UiNode {
        UiNode {
            role: node.role.clone(),
            name: self.text(&node.name),
            automation_id: self.ident(&node.automation_id),
            class_name: self.ident(&node.class_name),
            value: node.value.as_ref().map(|v| self.text(v)),
            runtime_id: None,
            children: node.children.iter().map(|c| self.node(c)).collect(),
        }
    }
}

/// An anonymised copy of `fixture`; deterministic, and idempotent on its own
/// output.
pub fn anonymise(fixture: &Fixture, selectors: &Selectors) -> Fixture {
    let names = speaker_names(fixture, selectors);
    let speakers: Vec<(String, String)> = names
        .into_iter()
        .enumerate()
        .map(|(index, name)| (name, pseudonym(index)))
        .collect();
    let anon = Anonymiser {
        keep: protected(selectors),
        idents: known_idents(selectors),
        speakers: &speakers,
    };
    let windows = fixture
        .windows
        .iter()
        .enumerate()
        .map(|(index, window)| {
            let mut info = window.info.clone();
            info.handle = index as u64 + 1;
            info.pid = 4000 + index as u32;
            info.title = anon.text(&window.info.title);
            info.class_name = anon.ident(&window.info.class_name);
            info.process = window.info.process.as_ref().map(|p| {
                if KNOWN_PROCESSES.iter().any(|k| k.eq_ignore_ascii_case(p)) {
                    p.clone()
                } else {
                    mask(p, &[])
                }
            });
            FakeWindow {
                info,
                tree: anon.node(&window.tree),
            }
        })
        .collect();
    Fixture {
        format: fixture.format.clone(),
        version: fixture.version,
        platform: fixture.platform.clone(),
        app: fixture.app.clone(),
        anonymized: true,
        note: "anonymised by mc doctor --dump".to_string(),
        windows,
        script: Vec::new(),
    }
}

/// Name -> pseudonym pairs, for tests.
pub fn pseudonyms(fixture: &Fixture, selectors: &Selectors) -> HashMap<String, String> {
    speaker_names(fixture, selectors)
        .into_iter()
        .enumerate()
        .map(|(index, name)| (name, pseudonym(index)))
        .collect()
}

/// A window title with its letters and digits masked but the selector keywords
/// kept: enough to see which window would be picked and why.
pub fn mask_title(title: &str, selectors: &Selectors) -> String {
    mask(title, &protected(selectors))
}

/// What `doctor --uia` shows for a window that cannot be a meeting.
pub fn hidden_title(title: &str) -> String {
    format!("(hidden, {} chars)", title.chars().count())
}
