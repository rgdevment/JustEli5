//! Words for comparing Teams captions with ASR text.

/// A word as written, and the form it is compared in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    pub surface: String,
    pub key: String,
}

pub use mc_core::fold::{fold, fold_words};

pub fn tokenize(text: &str) -> Vec<Word> {
    text.split_whitespace()
        .map(|surface| {
            let key = fold(surface);
            Word {
                // A token with no letters at all ("-", "…") compares as itself.
                key: if key.is_empty() {
                    surface.to_string()
                } else {
                    key
                },
                surface: surface.to_string(),
            }
        })
        .collect()
}

/// Leading punctuation, the core, trailing punctuation.
pub fn split_punct(surface: &str) -> (&str, &str, &str) {
    let start = surface
        .char_indices()
        .find(|(_, c)| c.is_alphanumeric())
        .map_or(surface.len(), |(i, _)| i);
    let end = surface
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_alphanumeric())
        .map_or(start, |(i, c)| i + c.len_utf8());
    (&surface[..start], &surface[start..end], &surface[end..])
}

/// `core` carrying the punctuation and the leading capital of `like`.
pub fn dress_like(core: &str, like: &str) -> String {
    let (lead, like_core, trail) = split_punct(like);
    let mut chars = core.chars();
    let cored = match (like_core.chars().next(), chars.next()) {
        (Some(l), Some(first)) if l.is_uppercase() && first.is_lowercase() => {
            first.to_uppercase().chain(chars).collect::<String>()
        }
        (_, Some(first)) => std::iter::once(first).chain(chars).collect::<String>(),
        _ => String::new(),
    };
    format!("{lead}{cored}{trail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_keep_their_surface() {
        let words = tokenize("¿Quedamos mañana? — ok");
        let keys: Vec<_> = words.iter().map(|w| w.key.as_str()).collect();
        assert_eq!(keys, ["quedamos", "manana", "—", "ok"]);
        assert_eq!(words[0].surface, "¿Quedamos");
    }

    #[test]
    fn punctuation_split_and_dressing() {
        assert_eq!(split_punct("¿hola,"), ("¿", "hola", ","));
        assert_eq!(split_punct("..."), ("...", "", ""));
        assert_eq!(dress_like("deploy", "Pus,"), "Deploy,");
        assert_eq!(dress_like("deploy", "pus"), "deploy");
        assert_eq!(dress_like("API", "pus"), "API");
        assert_eq!(dress_like("", "x"), "");
    }
}
