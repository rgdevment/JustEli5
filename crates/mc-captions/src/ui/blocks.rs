//! Caption regions of web meeting UIs (Meet, Slack), read tolerantly: the
//! region is found by its accessible name and read as rows of texts, a speaker's
//! name first and then what they said. Chromium drops role-less wrappers, so
//! the rows may be flat; then a name is told from speech by its shape and by
//! the speakers already seen. Nothing is invented: a text that is not clearly
//! a name stays speech, with or without a speaker.

use std::collections::HashSet;

use mc_core::RawObservation;

use super::node::{Role, UiNode};
use super::teams::{find_first, py_strip};

/// Names that have been a speaker before; keeps a flattened region read the same on every poll.
pub type KnownSpeakers = HashSet<String>;

/// Roles a caption region can have; never an interactive control that happens to carry the name.
fn is_container(node: &UiNode) -> bool {
    match &node.role {
        Role::Group | Role::Pane | Role::List => true,
        Role::Other(name) => matches!(name.as_str(), "Region" | "Custom" | "Landmark"),
        _ => false,
    }
}

/// Path of the first container (pre-order) named like one of `names`.
pub fn find_region(window: &UiNode, names: &[String], max_depth: usize) -> Option<Vec<usize>> {
    let wanted: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
    let is_region =
        |n: &UiNode| is_container(n) && wanted.contains(&py_strip(&n.name).to_lowercase());
    find_first(window, max_depth, &is_region)
}

/// What tells a speaker's name from a line of speech when the tree is flat.
#[derive(Debug, Clone, Copy)]
pub struct NameRule<'a> {
    pub max_words: usize,
    /// Any of these characters makes a text speech, not a name.
    pub punctuation: &'a str,
}

impl NameRule<'_> {
    fn is_name(&self, text: &str) -> bool {
        let words = text.split_whitespace().count();
        (1..=self.max_words).contains(&words)
            && !text.chars().any(|c| self.punctuation.contains(c))
            && text.chars().next().is_some_and(|c| !c.is_lowercase())
    }
}

enum Item {
    /// An avatar.
    Image,
    Text(String),
}

struct Reader {
    /// Lowercased texts that are interface (the region's own label, noise).
    skip: Vec<String>,
    max_depth: usize,
}

impl Reader {
    fn keep(&self, node: &UiNode) -> bool {
        let text = py_strip(&node.name);
        node.role == Role::Text && !text.is_empty() && !self.skip.contains(&text.to_lowercase())
    }

    /// Texts and avatars below `node` in document order.
    fn items(&self, node: &UiNode, depth: usize, out: &mut Vec<Item>) {
        if depth > self.max_depth {
            return;
        }
        for child in &node.children {
            if self.keep(child) {
                out.push(Item::Text(py_strip(&child.name).to_string()));
            } else if child.role == Role::Other("Image".to_string()) {
                out.push(Item::Image);
            }
            self.items(child, depth + 1, out);
        }
    }

    fn runs(&self, node: &UiNode, depth: usize) -> Vec<String> {
        let mut items = Vec::new();
        self.items(node, depth, &mut items);
        items
            .into_iter()
            .filter_map(|i| match i {
                Item::Text(text) => Some(text),
                Item::Image => None,
            })
            .collect()
    }

    /// The rows of the region. Wrappers with a single text-bearing child are
    /// looked through; each child below is a row. Texts directly in a node that
    /// also has structure are notices of the interface, not captions.
    fn rows(&self, region: &UiNode) -> Vec<Vec<String>> {
        let mut node = region;
        let mut depth = 1;
        loop {
            let bears = |c: &UiNode| c.role != Role::Text && !self.runs(c, depth + 1).is_empty();
            let structural = node.children.iter().filter(|c| bears(c)).count();
            let direct = node.children.iter().any(|c| self.keep(c));
            if !direct
                && structural == 1
                && let Some(only) = node.children.iter().find(|c| bears(c))
            {
                node = only;
                depth += 1;
                continue;
            }
            let mut rows = Vec::new();
            for child in &node.children {
                if self.keep(child) {
                    if structural == 0 {
                        rows.push(vec![py_strip(&child.name).to_string()]);
                    }
                } else if child.role != Role::Text {
                    let runs = self.runs(child, depth + 1);
                    if !runs.is_empty() {
                        rows.push(runs);
                    }
                }
            }
            return rows;
        }
    }

    /// Rows cut at avatars, when avatars really start the blocks: at least two,
    /// each followed by a name. Otherwise the images mean nothing.
    fn rows_by_avatars(
        &self,
        region: &UiNode,
        rule: &NameRule,
        known: &KnownSpeakers,
    ) -> Option<Vec<Vec<String>>> {
        let mut items = Vec::new();
        self.items(region, 1, &mut items);
        let images = items.iter().filter(|i| matches!(i, Item::Image)).count();
        let each_followed_by_a_name = items.windows(2).all(|pair| match pair {
            [Item::Image, Item::Text(t)] => rule.is_name(t) || known.contains(t),
            [Item::Image, Item::Image] => false,
            _ => true,
        }) && !matches!(items.last(), Some(Item::Image));
        if images < 2 || !each_followed_by_a_name {
            return None;
        }
        let mut rows: Vec<Vec<String>> = vec![Vec::new()];
        for item in items {
            match item {
                Item::Image => rows.push(Vec::new()),
                Item::Text(text) => {
                    if let Some(row) = rows.last_mut() {
                        row.push(text);
                    }
                }
            }
        }
        Some(rows.into_iter().filter(|r| !r.is_empty()).collect())
    }
}

enum Entry {
    /// A row that says who spoke.
    Pair(String, String),
    /// A row of one text, or of several that do not start with a name.
    Run(String),
}

impl Entry {
    fn text(&self) -> &str {
        match self {
            Self::Pair(_, text) | Self::Run(text) => text,
        }
    }
}

fn entry(row: &[String], rule: &NameRule, known: &KnownSpeakers) -> Option<Entry> {
    match row {
        [] => None,
        [only] => Some(Entry::Run(only.clone())),
        [first, rest @ ..] if rule.is_name(first) || known.contains(first) => {
            Some(Entry::Pair(first.clone(), rest.join(" ")))
        }
        _ => Some(Entry::Run(row.join(" "))),
    }
}

fn ends_a_sentence(text: &str) -> bool {
    text.trim_end().ends_with(['.', '?', '!', '\u{2026}'])
}

/// Whether the `Run` at `i` opens a speaker. A name-like text is taken for a
/// name only with evidence: it was a speaker before, or sentence-like speech
/// follows it and the region shows names and speech alternating.
fn opens_a_speaker(
    entries: &[Entry],
    i: usize,
    is_name: &[bool],
    rule: &NameRule,
    known: &KnownSpeakers,
) -> bool {
    let Entry::Run(text) = &entries[i] else {
        return false;
    };
    if known.contains(text) {
        return true;
    }
    let Some(Entry::Run(next)) = entries.get(i + 1) else {
        return false;
    };
    if !rule.is_name(text) {
        return false;
    }
    let named_before =
        is_name[..i].contains(&true) || entries[..i].iter().any(|e| matches!(e, Entry::Pair(..)));
    let speech_follows = !rule.is_name(next);
    if !named_before {
        // The first speaker: speech after it, or the same shape repeating after one row.
        let repeats = matches!(entries.get(i + 2), Some(Entry::Run(t)) if rule.is_name(t));
        return speech_follows || repeats;
    }
    let previous_done = i == 0 || ends_a_sentence(entries[i - 1].text());
    let alternates =
        i >= 2 && is_name[i - 2] && (previous_done || rule.is_name(entries[i - 1].text()));
    (speech_follows && previous_done) || alternates
}

/// Live captions often lack punctuation, so a region that alternates name-shaped and
/// speech-shaped rows for at least three speakers is taken as names and speech: where the
/// names start (0, or 1 when the first speaker scrolled off), or `None`.
fn strict_alternation(entries: &[Entry], rule: &NameRule) -> Option<usize> {
    let start = entries
        .iter()
        .take(2)
        .position(|e| rule.is_name(e.text()))?;
    let rows = &entries[start..];
    let alternates = rows
        .iter()
        .enumerate()
        .all(|(k, e)| matches!(e, Entry::Run(_)) && rule.is_name(e.text()) == (k % 2 == 0));
    (alternates && rows.len() >= 5).then_some(start)
}

/// `(speaker, text)` for each caption of the rows.
fn pair_up(
    entries: &[Entry],
    rule: &NameRule,
    known: &mut KnownSpeakers,
) -> Vec<(Option<String>, String)> {
    let mut is_name = vec![false; entries.len()];
    let mut out: Vec<(Option<String>, String)> = Vec::new();
    let mut waiting: Option<String> = None;
    let alternation = strict_alternation(entries, rule);
    for (i, e) in entries.iter().enumerate() {
        match e {
            Entry::Pair(name, text) => {
                waiting = None;
                known.insert(name.clone());
                out.push((Some(name.clone()), text.clone()));
            }
            Entry::Run(text) => {
                if let Some(name) = waiting.take() {
                    known.insert(name.clone());
                    out.push((Some(name), text.clone()));
                } else if alternation.is_some_and(|s| i >= s && (i - s) % 2 == 0)
                    || opens_a_speaker(entries, i, &is_name, rule, known)
                {
                    is_name[i] = true;
                    waiting = Some(text.clone());
                } else if let Some((_, said)) = out.last_mut() {
                    said.push(' ');
                    said.push_str(text);
                } else {
                    out.push((None, text.clone()));
                }
            }
        }
    }
    out
}

/// The captions in `region`, in document order. `walk_depth` bounds how deep
/// below the region texts are looked for.
pub fn captions(
    region: &UiNode,
    region_names: &[String],
    noise: &[String],
    walk_depth: usize,
    known: &mut KnownSpeakers,
    rule: &NameRule,
) -> Vec<RawObservation> {
    let skip = region_names
        .iter()
        .chain(noise)
        .map(|s| py_strip(s).to_lowercase())
        .collect();
    let reader = Reader {
        skip,
        max_depth: walk_depth,
    };
    let rows = reader
        .rows_by_avatars(region, rule, known)
        .unwrap_or_else(|| reader.rows(region));
    let entries: Vec<Entry> = rows
        .iter()
        .filter_map(|row| entry(row, rule, known))
        .collect();
    // One name-shaped row after another and nobody known yet: a name and the first words of speech
    // cannot be told apart, so wait for the next poll rather than guess.
    let undecided = entries.len() == 2
        && known.is_empty()
        && entries
            .iter()
            .all(|e| matches!(e, Entry::Run(t) if rule.is_name(t)));
    if undecided {
        return Vec::new();
    }
    pair_up(&entries, rule, known)
        .into_iter()
        .map(|(speaker, text)| RawObservation {
            speaker,
            text: Some(text),
            node_id: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(name: &str) -> UiNode {
        UiNode::new(Role::Text, name)
    }

    fn group(children: Vec<UiNode>) -> UiNode {
        UiNode::new(Role::Group, "").with_children(children)
    }

    fn item(name: &str) -> UiNode {
        group(vec![text(name)])
    }

    fn image() -> UiNode {
        UiNode::new(Role::Other("Image".into()), "")
    }

    type Said = Vec<(Option<String>, String)>;

    fn read_with(region: &UiNode, known: &mut KnownSpeakers) -> Said {
        let rule = NameRule {
            max_words: 5,
            punctuation: ".?!,;:",
        };
        let noise = ["Only you see this".to_string()];
        captions(region, &["Captions".to_string()], &noise, 10, known, &rule)
            .into_iter()
            .map(|o| (o.speaker, o.text.unwrap_or_default()))
            .collect()
    }

    fn read(region: &UiNode) -> Said {
        read_with(region, &mut KnownSpeakers::new())
    }

    fn said(speaker: &str, text: &str) -> (Option<String>, String) {
        (Some(speaker.to_string()), text.to_string())
    }

    #[test]
    fn one_block_inside_wrappers_is_one_caption() {
        let region = group(vec![group(vec![group(vec![text("Ana"), text("Hola.")])])]);
        assert_eq!(read(&region), [said("Ana", "Hola.")]);
    }

    #[test]
    fn blocks_split_per_speaker_and_skip_the_region_label() {
        let region = group(vec![group(vec![
            text("Captions"),
            group(vec![text("Ana"), text("uno")]),
            group(vec![text("Luis"), text("dos"), text("tres")]),
        ])]);
        assert_eq!(
            read(&region),
            [said("Ana", "uno"), said("Luis", "dos tres")]
        );
    }

    #[test]
    fn a_flat_alternating_list_pairs_names_with_the_text_after_them() {
        let region = group(vec![
            item("Ana Pérez"),
            item("Hola equipo, empezamos la daily."),
            item("Luis"),
            item("Hoy reviso el backlog"),
            item("y cierro el ticket."),
        ]);
        assert_eq!(
            read(&region),
            [
                said("Ana Pérez", "Hola equipo, empezamos la daily."),
                said("Luis", "Hoy reviso el backlog y cierro el ticket.")
            ]
        );
    }

    #[test]
    fn exact_alternation_is_evidence_even_when_the_speech_looks_like_a_name() {
        let region = group(vec![
            item("Ana"),
            item("Hola equipo"),
            item("Luis"),
            item("Si"),
        ]);
        assert_eq!(
            read(&region),
            [said("Ana", "Hola equipo"), said("Luis", "Si")]
        );
    }

    #[test]
    fn a_name_and_the_first_words_of_speech_wait_for_the_next_poll() {
        let mut known = KnownSpeakers::new();
        let early = group(vec![item("Ana Pérez"), item("Buenos días a")]);
        assert_eq!(read_with(&early, &mut known), []);
        let later = group(vec![
            item("Ana Pérez"),
            item("Buenos días a todos, empezamos."),
        ]);
        assert_eq!(
            read_with(&later, &mut known),
            [said("Ana Pérez", "Buenos días a todos, empezamos.")]
        );
        assert_eq!(
            read_with(&early, &mut known),
            [said("Ana Pérez", "Buenos días a")]
        );
    }

    #[test]
    fn unpunctuated_live_speech_alternating_with_three_names_pairs_up() {
        let region = group(vec![
            item("hoy revisamos el backlog"),
            item("Luis"),
            item("ayer cerré el ticket"),
            item("María José"),
            item("yo sigo con el deploy"),
            item("Pedro"),
            item("nada más por hoy"),
        ]);
        assert_eq!(
            read(&region),
            [
                (None, "hoy revisamos el backlog".to_string()),
                said("Luis", "ayer cerré el ticket"),
                said("María José", "yo sigo con el deploy"),
                said("Pedro", "nada más por hoy")
            ]
        );
    }

    #[test]
    fn a_new_speaker_after_several_rows_of_speech_is_recognised() {
        let region = group(vec![
            item("Ana"),
            item("Hola."),
            item("Seguimos con eso."),
            item("Luis"),
            item("Vale, adelante."),
        ]);
        assert_eq!(
            read(&region),
            [
                said("Ana", "Hola. Seguimos con eso."),
                said("Luis", "Vale, adelante.")
            ]
        );
    }

    #[test]
    fn a_capitalised_line_in_the_middle_of_speech_stays_speech() {
        let region = group(vec![
            item("Ana"),
            item("Hola, empezamos con el presupuesto de"),
            item("Gracias a todos"),
            item("por venir."),
        ]);
        assert_eq!(
            read(&region),
            [said(
                "Ana",
                "Hola, empezamos con el presupuesto de Gracias a todos por venir."
            )]
        );
    }

    #[test]
    fn a_trailing_name_like_row_is_speech_unless_it_is_a_known_speaker() {
        let region = group(vec![item("Ana"), item("Vale."), item("Luis")]);
        assert_eq!(read(&region), [said("Ana", "Vale. Luis")]);
        let mut known = KnownSpeakers::from(["Luis".to_string()]);
        assert_eq!(read_with(&region, &mut known), [said("Ana", "Vale.")]);
    }

    #[test]
    fn speakers_seen_once_are_remembered_across_reads() {
        let mut known = KnownSpeakers::new();
        let first = group(vec![item("Ana"), item("Vale."), item("Luis"), item("Si.")]);
        assert_eq!(read_with(&first, &mut known).len(), 2);
        let later = group(vec![item("Ana"), item("Vale."), item("Luis")]);
        assert_eq!(read_with(&later, &mut known), [said("Ana", "Vale.")]);
    }

    #[test]
    fn texts_before_any_name_keep_an_empty_speaker() {
        let region = group(vec![
            item("seguimos con lo anterior"),
            item("Ana"),
            item("Vale."),
        ]);
        assert_eq!(
            read(&region),
            [
                (None, "seguimos con lo anterior".to_string()),
                said("Ana", "Vale.")
            ]
        );
    }

    #[test]
    fn a_row_with_several_texts_is_name_and_text_only_when_it_starts_with_a_name() {
        let region = group(vec![
            group(vec![text("Ana"), text("Hola.")]),
            group(vec![text("lo que decía antes"), text("y sigue.")]),
        ]);
        assert_eq!(
            read(&region),
            [said("Ana", "Hola. lo que decía antes y sigue.")]
        );
    }

    #[test]
    fn a_mixed_region_decides_per_row() {
        let region = group(vec![
            item("Hello without a name."),
            group(vec![text("Luis"), text("Hi.")]),
        ]);
        assert_eq!(
            read(&region),
            [
                (None, "Hello without a name.".to_string()),
                said("Luis", "Hi.")
            ]
        );
    }

    #[test]
    fn direct_notices_beside_structure_are_dropped_and_noise_is_skipped() {
        let region = group(vec![
            text("Captions are shown here"),
            text("Only you see this"),
            group(vec![text("Ana"), text("Hola.")]),
            group(vec![text("Luis"), text("Adiós.")]),
        ]);
        assert_eq!(
            read(&region),
            [said("Ana", "Hola."), said("Luis", "Adiós.")]
        );
    }

    #[test]
    fn avatars_start_the_blocks_when_they_come_before_names() {
        let region = group(vec![
            image(),
            item("Ana Pérez"),
            item("hola"),
            image(),
            item("Luis"),
            item("adiós"),
        ]);
        assert_eq!(
            read(&region),
            [said("Ana Pérez", "hola"), said("Luis", "adiós")]
        );
    }

    #[test]
    fn avatars_that_do_not_start_blocks_are_ignored() {
        let region = group(vec![
            item("Ana"),
            item("Hola, mira esto."),
            image(),
            item("de nuevo."),
            image(),
            item("Luis"),
            item("Vale, adelante."),
        ]);
        let all = read(&region);
        assert_eq!(all[0], said("Ana", "Hola, mira esto. de nuevo."));
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn nothing_to_read_gives_nothing() {
        assert!(read(&group(vec![group(vec![]), text("  ")])).is_empty());
    }

    #[test]
    fn region_is_found_by_name_but_only_among_containers() {
        let window = UiNode::new(Role::Window, "").with_children(vec![
            UiNode::new(Role::Button, "Captions"),
            UiNode::new(Role::Other("TabItem".into()), "Captions"),
            UiNode::new(Role::Other("MenuItem".into()), "Captions"),
            text("Captions"),
            UiNode::new(Role::Group, " captions "),
        ]);
        assert_eq!(
            find_region(&window, &["Captions".to_string()], 5),
            Some(vec![4])
        );
    }
}
