//! The meeting's own vocabulary, found inside a run of words.

use crate::text::fold;

#[derive(Debug, Clone)]
pub struct Glossary {
    /// Folded words of each term, and the term as the user wrote it; the
    /// longest first so "pull request" wins over a one-word "pull".
    terms: Vec<(Vec<String>, String)>,
    /// Folded words of a form the user taught for a term ("beri dex"), and that term.
    aliases: Vec<(Vec<String>, String)>,
}

/// A glossary term found in a sequence of word keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    pub start: usize,
    pub len: usize,
    pub term: usize,
}

fn keyed(text: &str) -> Option<(Vec<String>, String)> {
    let canonical = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let keys: Vec<String> = canonical
        .split_whitespace()
        .map(fold)
        .filter(|k| !k.is_empty())
        .collect();
    (!keys.is_empty()).then_some((keys, canonical))
}

fn longest_first(list: &mut Vec<(Vec<String>, String)>) {
    list.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.1.cmp(&b.1)));
    list.dedup_by(|a, b| a.0 == b.0);
}

impl Glossary {
    pub fn new(terms: &[String]) -> Self {
        Self::with_aliases(terms, &[])
    }

    /// `aliases` are (form, term) pairs; a form that is itself a term is left to the term.
    pub fn with_aliases(terms: &[String], aliases: &[(String, String)]) -> Self {
        let mut terms: Vec<_> = terms.iter().filter_map(|t| keyed(t)).collect();
        longest_first(&mut terms);
        let mut aliases: Vec<_> = aliases
            .iter()
            .filter_map(|(form, term)| Some((keyed(form)?.0, keyed(term)?.1)))
            .filter(|(keys, _)| !terms.iter().any(|(t, _)| t == keys))
            .collect();
        longest_first(&mut aliases);
        Self { terms, aliases }
    }

    pub fn canonical(&self, term: usize) -> &str {
        &self.terms[term].1
    }

    /// The term the alias found as `find_aliases`' `term` stands for.
    pub fn alias_term(&self, alias: usize) -> &str {
        &self.aliases[alias].1
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// Non-overlapping occurrences, left to right.
    pub fn find(&self, keys: &[String]) -> Vec<Found> {
        scan(&self.terms, keys)
    }

    /// Non-overlapping occurrences of the taught forms; `term` indexes `alias_term`.
    pub fn find_aliases(&self, keys: &[String]) -> Vec<Found> {
        scan(&self.aliases, keys)
    }
}

fn scan(list: &[(Vec<String>, String)], keys: &[String]) -> Vec<Found> {
    let mut found = Vec::new();
    let mut i = 0;
    while i < keys.len() {
        let hit = list
            .iter()
            .enumerate()
            .find(|(_, (t, _))| keys.len() >= i + t.len() && keys[i..i + t.len()] == t[..]);
        match hit {
            Some((term, (t, _))) => {
                found.push(Found {
                    start: i,
                    len: t.len(),
                    term,
                });
                i += t.len();
            }
            None => i += 1,
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(text: &str) -> Vec<String> {
        text.split_whitespace().map(fold).collect()
    }

    #[test]
    fn finds_single_and_multi_word_terms_longest_first() {
        let g = Glossary::new(&[
            "pull request".into(),
            "pull".into(),
            "Deploy".into(),
            "".into(),
        ]);
        let found = g.find(&keys("hice pull request y el deploy"));
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].start, found[0].len), (1, 2));
        assert_eq!(g.canonical(found[0].term), "pull request");
        assert_eq!((found[1].start, found[1].len), (5, 1));
        assert_eq!(g.canonical(found[1].term), "Deploy");
    }

    #[test]
    fn taught_forms_point_to_their_term_and_never_shadow_a_term() {
        let g = Glossary::with_aliases(
            &["Verídex".into(), "Core".into()],
            &[
                ("beri dex".into(), "Verídex".into()),
                ("core".into(), "Core User".into()),
            ],
        );
        let found = g.find_aliases(&keys("lo valida Beri dex y core"));
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].start, found[0].len), (2, 2));
        assert_eq!(g.alias_term(found[0].term), "Verídex");
    }

    #[test]
    fn accents_and_case_do_not_matter_and_repeats_are_all_found() {
        let g = Glossary::new(&["Producción".into()]);
        assert_eq!(g.find(&keys("produccion y PRODUCCIÓN")).len(), 2);
        assert!(Glossary::new(&[]).is_empty());
        assert!(g.find(&[]).is_empty());
    }
}
