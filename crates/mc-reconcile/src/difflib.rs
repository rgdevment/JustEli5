//! Port of CPython's `difflib.SequenceMatcher(None, a, b)` over `char`s.
//!
//! The reconciler decides whether two caption frames are the same line using
//! `SequenceMatcher(...).ratio()`, so this must reproduce Python exactly,
//! including `autojunk` (characters that make up more than 1% of `b` are
//! ignored as match seeds once `len(b) >= 200`, which real monologues exceed)
//! and the tie-breaking of `find_longest_match` (smallest `i`, then smallest
//! `j`), including the loops that re-extend a match over those ignored
//! characters.

use std::collections::HashMap;

/// A matching block: `a[a_start..a_start+size] == b[b_start..b_start+size]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Match {
    pub a_start: usize,
    pub b_start: usize,
    pub size: usize,
}

/// Python's `SequenceMatcher(None, a, b)`, with `autojunk` selectable.
pub struct SequenceMatcher {
    a: Vec<char>,
    b: Vec<char>,
    b2j: HashMap<char, Vec<usize>>,
}

/// `len(b)` from which `autojunk` starts discarding popular characters.
const AUTOJUNK_MIN_LEN: usize = 200;

impl SequenceMatcher {
    /// `SequenceMatcher(None, a, b)` with Python's default `autojunk=True`.
    pub fn new(a: &str, b: &str) -> Self {
        Self::with_autojunk(a, b, true)
    }

    pub fn with_autojunk(a: &str, b: &str, autojunk: bool) -> Self {
        let a: Vec<char> = a.chars().collect();
        let b: Vec<char> = b.chars().collect();
        let mut b2j: HashMap<char, Vec<usize>> = HashMap::new();
        for (i, c) in b.iter().enumerate() {
            b2j.entry(*c).or_default().push(i);
        }
        let n = b.len();
        if autojunk && n >= AUTOJUNK_MIN_LEN {
            let ntest = n / 100 + 1;
            b2j.retain(|_, idxs| idxs.len() <= ntest);
        }
        Self { a, b, b2j }
    }

    /// `find_longest_match(alo, ahi, blo, bhi)`: the longest block inside the
    /// ranges, earliest in `a` and then in `b` on ties.
    pub fn find_longest_match(&self, alo: usize, ahi: usize, blo: usize, bhi: usize) -> Match {
        let (a, b) = (&self.a, &self.b);
        let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);

        let mut j2len: HashMap<usize, usize> = HashMap::new();
        for (i, ch) in a.iter().enumerate().take(ahi).skip(alo) {
            let mut newj2len: HashMap<usize, usize> = HashMap::new();
            if let Some(js) = self.b2j.get(ch) {
                for &j in js {
                    if j < blo {
                        continue;
                    }
                    if j >= bhi {
                        break;
                    }
                    let prev = j.checked_sub(1).and_then(|p| j2len.get(&p)).copied();
                    let k = prev.unwrap_or(0) + 1;
                    newj2len.insert(j, k);
                    if k > bestsize {
                        besti = i + 1 - k;
                        bestj = j + 1 - k;
                        bestsize = k;
                    }
                }
            }
            j2len = newj2len;
        }

        // Re-extend over characters autojunk removed from `b2j` (there are no
        // user-supplied junk characters, so every element may extend).
        while besti > alo && bestj > blo && a[besti - 1] == b[bestj - 1] {
            besti -= 1;
            bestj -= 1;
            bestsize += 1;
        }
        while besti + bestsize < ahi
            && bestj + bestsize < bhi
            && a[besti + bestsize] == b[bestj + bestsize]
        {
            bestsize += 1;
        }

        Match {
            a_start: besti,
            b_start: bestj,
            size: bestsize,
        }
    }

    /// `get_matching_blocks()` without the trailing sentinel: sorted, with
    /// adjacent blocks merged (Python's `non_adjacent` pass).
    pub fn matching_blocks(&self) -> Vec<Match> {
        let mut queue = vec![(0usize, self.a.len(), 0usize, self.b.len())];
        let mut blocks = Vec::new();
        while let Some((alo, ahi, blo, bhi)) = queue.pop() {
            let m = self.find_longest_match(alo, ahi, blo, bhi);
            if m.size > 0 {
                blocks.push(m);
                if alo < m.a_start && blo < m.b_start {
                    queue.push((alo, m.a_start, blo, m.b_start));
                }
                if m.a_start + m.size < ahi && m.b_start + m.size < bhi {
                    queue.push((m.a_start + m.size, ahi, m.b_start + m.size, bhi));
                }
            }
        }
        blocks.sort();

        let mut merged: Vec<Match> = Vec::with_capacity(blocks.len());
        for block in blocks {
            match merged.last_mut() {
                Some(last)
                    if last.a_start + last.size == block.a_start
                        && last.b_start + last.size == block.b_start =>
                {
                    last.size += block.size;
                }
                _ => merged.push(block),
            }
        }
        merged
    }

    /// `ratio()`: `2.0 * matches / (len(a) + len(b))`, `1.0` for two empty
    /// sequences.
    pub fn ratio(&self) -> f64 {
        let matches: usize = self.matching_blocks().iter().map(|m| m.size).sum();
        let length = self.a.len() + self.b.len();
        if length > 0 {
            2.0 * matches as f64 / length as f64
        } else {
            1.0
        }
    }
}

/// `SequenceMatcher(None, a, b).ratio()`.
pub fn ratio(a: &str, b: &str) -> f64 {
    SequenceMatcher::new(a, b).ratio()
}

/// `SequenceMatcher(None, a, b, autojunk=False).ratio()`.
pub fn ratio_nojunk(a: &str, b: &str) -> f64 {
    SequenceMatcher::with_autojunk(a, b, false).ratio()
}
