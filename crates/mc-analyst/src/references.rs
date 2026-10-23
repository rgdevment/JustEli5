//! Links and ticket keys said in the meeting or written in the notes, found without a model.

/// One reference: what it is and the text that names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub kind: &'static str,
    pub text: String,
}

fn link_kind(url: &str) -> &'static str {
    let lower = url.to_lowercase();
    if lower.contains("/wiki/") || lower.contains("confluence") {
        "Confluence"
    } else if lower.contains("/browse/") || lower.contains("jira") {
        "Jira"
    } else if lower.contains("github.com") {
        "GitHub"
    } else if lower.contains("figma.com") {
        "Figma"
    } else if lower.contains("docs.google.com") || lower.contains("drive.google.com") {
        "Google Drive"
    } else {
        "Enlace"
    }
}

/// `ABC-123`: two to ten capitals (digits after the first allowed), a dash and up to six digits.
fn is_ticket(word: &str) -> bool {
    let Some((key, number)) = word.split_once('-') else {
        return false;
    };
    let mut chars = key.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && (2..=10).contains(&key.len())
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && (1..=6).contains(&number.len())
        && number.chars().all(|c| c.is_ascii_digit())
}

/// Every link and ticket key in `texts`, in the order first seen, each once.
pub fn references<'a>(texts: impl IntoIterator<Item = &'a str>) -> Vec<Reference> {
    let mut found: Vec<Reference> = Vec::new();
    for text in texts {
        for raw in text.split_whitespace() {
            let word = raw.trim_matches(|c: char| "()[]<>«»\"'.,;:!?".contains(c));
            let reference = if word.starts_with("http://") || word.starts_with("https://") {
                Reference {
                    kind: link_kind(word),
                    text: word.to_string(),
                }
            } else if is_ticket(word) {
                Reference {
                    kind: "Ticket",
                    text: word.to_string(),
                }
            } else {
                continue;
            };
            if !found.iter().any(|f| f.text == reference.text) {
                found.push(reference);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_and_tickets_are_found_once_with_their_kind() {
        let found = references([
            "Revisa PROJ-5231 y https://acme.atlassian.net/wiki/spaces/X/pages/1.",
            "otra vez PROJ-5231, el PR https://github.com/a/b/pull/3 y COVID-19 no",
            "MVP-1 tampoco es ticket? sí: lo es; abc-12 no",
        ]);
        let pairs: Vec<(&str, &str)> = found.iter().map(|r| (r.kind, r.text.as_str())).collect();
        assert_eq!(
            pairs,
            [
                ("Ticket", "PROJ-5231"),
                (
                    "Confluence",
                    "https://acme.atlassian.net/wiki/spaces/X/pages/1"
                ),
                ("GitHub", "https://github.com/a/b/pull/3"),
                ("Ticket", "COVID-19"),
                ("Ticket", "MVP-1"),
            ]
        );
    }
}
