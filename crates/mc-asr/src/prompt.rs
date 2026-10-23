//! The initial prompt that steers recognition toward the meeting's own words.

use serde_json::Value;

/// whisper.cpp keeps only the last ~223 tokens of a prompt, and names, acronyms
/// and accented words cost 2-3 characters per token; 400 characters stay inside
/// that even for a glossary of that kind.
pub const MAX_PROMPT_CHARS: usize = 400;

/// `"Vocabulario: a, b, c."` from glossary terms: trimmed, without empty
/// entries or duplicates (case-insensitive), in the order given, cut at a whole
/// term when the budget runs out. `None` when there is nothing to say.
pub fn glossary_prompt(terms: &[String], max_chars: usize) -> Option<String> {
    const HEAD: &str = "Vocabulario: ";
    let mut seen = std::collections::HashSet::new();
    let mut out = String::from(HEAD);
    let mut count = 0usize;
    for term in terms {
        let term = term.split_whitespace().collect::<Vec<_>>().join(" ");
        if term.is_empty() || term.contains('\0') || !seen.insert(term.to_lowercase()) {
            continue;
        }
        let extra = term.chars().count() + if count == 0 { 0 } else { 2 };
        // One more for the closing period.
        if out.chars().count() + extra + 1 > max_chars {
            break;
        }
        if count > 0 {
            out.push_str(", ");
        }
        out.push_str(&term);
        count += 1;
    }
    (count > 0).then(|| {
        out.push('.');
        out
    })
}

/// Terms from a glossary file: a JSON object keyed by term (its aliases are
/// misspellings, never vocabulary), a JSON list of terms, or plain text with one
/// term per line where blank lines and `#` comments are ignored. Text that opens
/// with `{` or `[` must be valid JSON of one of those shapes, so a broken or
/// unrelated JSON file is an error rather than a prompt full of braces.
pub fn parse_glossary(text: &str) -> Result<Vec<String>, String> {
    Ok(parse_glossary_entries(text)?
        .into_iter()
        .map(|(term, _)| term)
        .collect())
}

/// Like [`parse_glossary`], and each term comes with the misspellings the file lists for it.
pub fn parse_glossary_entries(text: &str) -> Result<Vec<(String, Vec<String>)>, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let entries: Vec<(String, Vec<String>)> = if text.trim_start().starts_with(['{', '[']) {
        parse_json_entries(text)?
    } else {
        text.lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .map(|l| (l.to_string(), Vec::new()))
            .collect()
    };
    // A NUL cannot be passed to the engine (it would abort the process).
    let clean =
        |s: String| Some(s.trim().to_string()).filter(|t| !t.is_empty() && !t.contains('\0'));
    Ok(entries
        .into_iter()
        .filter_map(|(term, aliases)| {
            let term = clean(term)?;
            Some((term, aliases.into_iter().filter_map(&clean).collect()))
        })
        .collect())
}

fn parse_json_entries(text: &str) -> Result<Vec<(String, Vec<String>)>, String> {
    if text.trim_start().starts_with('[') {
        let terms = serde_json::from_str::<Vec<String>>(text)
            .map_err(|e| format!("invalid JSON glossary: {e}"))?;
        return Ok(terms.into_iter().map(|t| (t, Vec::new())).collect());
    }
    let OrderedEntries(entries) =
        serde_json::from_str(text).map_err(|e| format!("invalid JSON glossary: {e}"))?;
    for (term, value) in &entries {
        let fits = match value {
            Value::Null | Value::Array(_) => true,
            Value::Object(fields) => fields.get("aliases").is_none_or(Value::is_array),
            _ => false,
        };
        if !fits {
            return Err(format!(
                "not a glossary: the value of {term:?} must be an object, a list or null"
            ));
        }
    }
    Ok(entries
        .into_iter()
        .map(|(term, value)| {
            let list = match &value {
                Value::Array(list) => list.as_slice(),
                Value::Object(fields) => fields
                    .get("aliases")
                    .and_then(Value::as_array)
                    .map_or(&[][..], Vec::as_slice),
                _ => &[],
            };
            let aliases = list
                .iter()
                .filter_map(|a| a.as_str().map(str::to_string))
                .collect();
            (term, aliases)
        })
        .collect())
}

/// A JSON object's entries in file order (the prompt budget keeps the first ones).
struct OrderedEntries(Vec<(String, Value)>);

impl<'de> serde::Deserialize<'de> for OrderedEntries {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Entries;
        impl<'de> serde::de::Visitor<'de> for Entries {
            type Value = OrderedEntries;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an object keyed by glossary term")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<OrderedEntries, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry::<String, Value>()? {
                    entries.push(entry);
                }
                Ok(OrderedEntries(entries))
            }
        }
        deserializer.deserialize_map(Entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn builds_a_prompt_from_unique_terms_in_order() {
        let prompt = glossary_prompt(
            &terms(&[
                "Kubernetes",
                "  push ",
                "kubernetes",
                "",
                "carry over",
                "PUSH",
            ]),
            MAX_PROMPT_CHARS,
        );
        assert_eq!(
            prompt.as_deref(),
            Some("Vocabulario: Kubernetes, push, carry over.")
        );
    }

    #[test]
    fn nothing_to_say_gives_no_prompt() {
        assert_eq!(glossary_prompt(&[], 600), None);
        assert_eq!(glossary_prompt(&terms(&["", "  "]), 600), None);
    }

    #[test]
    fn the_budget_cuts_at_a_whole_term() {
        let many: Vec<String> = (0..200).map(|n| format!("termino{n}")).collect();
        let prompt = glossary_prompt(&many, 100).unwrap();
        assert!(prompt.chars().count() <= 100, "{prompt}");
        assert!(prompt.ends_with('.') && !prompt.ends_with(", ."));
        assert!(prompt.contains("termino0, termino1"));
        // A single term longer than the budget cannot fit at all.
        assert_eq!(glossary_prompt(&terms(&[&"x".repeat(80)]), 40), None);
    }

    #[test]
    fn nul_bytes_never_reach_the_engine() {
        assert_eq!(parse_glossary("ok\nma\0l\nfine").unwrap(), ["ok", "fine"]);
        let prompt = glossary_prompt(&terms(&["a\0b", "bien"]), 100).unwrap();
        assert!(!prompt.contains('\0') && prompt.contains("bien"));
    }

    #[test]
    fn glossary_files_skip_comments_and_blanks() {
        let text = "# equipo\nKubernetes\n\n  sprint  \n# fin\n";
        assert_eq!(parse_glossary(text).unwrap(), ["Kubernetes", "sprint"]);
    }

    #[test]
    fn a_json_glossary_gives_its_terms_and_never_the_misspellings() {
        let object = r#"{"Pipeline": {"aliases": ["paylain"], "live_replace": true},
                         "carry over": {"aliases": []}}"#;
        assert_eq!(parse_glossary(object).unwrap(), ["Pipeline", "carry over"]);
        assert_eq!(
            parse_glossary(r#"["CIAM", " SAGA ", ""]"#).unwrap(),
            ["CIAM", "SAGA"]
        );
    }

    #[test]
    fn the_repository_glossary_makes_a_clean_prompt() {
        let text = include_str!("../../../technical_glossary.json");
        let prompt = glossary_prompt(&parse_glossary(text).unwrap(), MAX_PROMPT_CHARS).unwrap();
        assert!(
            prompt.contains("Pipeline") && prompt.contains("Deploy"),
            "{prompt}"
        );
        assert!(!prompt.contains(['{', '"', '[']), "{prompt}");
    }

    #[test]
    fn a_byte_order_mark_is_ignored() {
        assert_eq!(parse_glossary("\u{feff}[\"CIAM\"]").unwrap(), ["CIAM"]);
        assert_eq!(
            parse_glossary("\u{feff}Kubernetes\nsprint").unwrap(),
            ["Kubernetes", "sprint"]
        );
    }

    #[test]
    fn broken_json_is_an_error_not_a_plain_text_glossary() {
        assert!(parse_glossary(r#"["CIAM", "SAGA",]"#).is_err());
        assert!(parse_glossary(r#"{"Pipeline": {"aliases": [],}}"#).is_err());
        assert!(parse_glossary("[unterminated").is_err());
    }

    #[test]
    fn a_json_object_that_is_not_a_glossary_is_rejected() {
        let eval = r#"{"name": "demo", "expected_terms": 3, "reference": "texto"}"#;
        assert!(parse_glossary(eval).unwrap_err().contains("not a glossary"));
        let bad_aliases = r#"{"Pipeline": {"aliases": "paylain"}}"#;
        assert!(parse_glossary(bad_aliases).is_err());
        assert_eq!(
            parse_glossary(r#"{"a": null, "b": ["x"], "c": {}}"#).unwrap(),
            ["a", "b", "c"]
        );
    }
}
