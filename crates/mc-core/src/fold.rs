//! Text as it is compared: lower case, without accents or punctuation.

fn fold_char(c: char) -> Option<char> {
    let c = c.to_lowercase().next().unwrap_or(c);
    Some(match c {
        'á' | 'à' | 'ä' | 'â' => 'a',
        'é' | 'è' | 'ë' | 'ê' => 'e',
        'í' | 'ì' | 'ï' | 'î' => 'i',
        'ó' | 'ò' | 'ö' | 'ô' => 'o',
        'ú' | 'ù' | 'ü' | 'û' => 'u',
        'ñ' => 'n',
        c if c.is_alphanumeric() => c,
        _ => return None,
    })
}

/// Lower case, accents removed, only letters and digits.
pub fn fold(text: &str) -> String {
    text.chars().filter_map(fold_char).collect()
}

/// Lower-case, accent-free words separated by single spaces (for ratios).
pub fn fold_words(text: &str) -> String {
    text.split_whitespace()
        .map(fold)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_ignores_case_accents_and_punctuation() {
        assert_eq!(fold("¿Está?"), "esta");
        assert_eq!(fold("Mañana,"), "manana");
        assert_eq!(fold("QA-2"), "qa2");
        assert_eq!(fold_words("  El  Hot-fix, ya "), "el hotfix ya");
    }
}
