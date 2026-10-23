//! Word-level alignment of two texts (LCS), grouped into edit blocks.

use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Equal {
        a: Range<usize>,
        b: Range<usize>,
    },
    /// Both sides changed.
    Replace {
        a: Range<usize>,
        b: Range<usize>,
    },
    /// Only `a` has words here.
    Delete {
        a: Range<usize>,
    },
    /// Only `b` has words here.
    Insert {
        b: Range<usize>,
    },
}

/// Length of the longest common subsequence (two rows of memory).
pub fn lcs_len(a: &[String], b: &[String]) -> usize {
    let mut below = vec![0usize; b.len() + 1];
    let mut row = vec![0usize; b.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            row[j] = if a[i] == b[j] {
                below[j + 1] + 1
            } else {
                below[j].max(row[j + 1])
            };
        }
        std::mem::swap(&mut below, &mut row);
    }
    below[0]
}

fn table(a: &[String], b: &[String]) -> Vec<Vec<usize>> {
    let mut t = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            t[i][j] = if a[i] == b[j] {
                t[i + 1][j + 1] + 1
            } else {
                t[i + 1][j].max(t[i][j + 1])
            };
        }
    }
    t
}

/// Edit script from `a` to `b`. Ties go to deleting from `a` first, so the
/// result is the same on every run.
pub fn diff(a: &[String], b: &[String]) -> Vec<Op> {
    let t = table(a, b);
    let (mut i, mut j) = (0, 0);
    let mut ops: Vec<Op> = Vec::new();
    // The current run of differences, closed by the next equal word.
    let (mut run_a, mut run_b): (Option<Range<usize>>, Option<Range<usize>>) = (None, None);
    let close =
        |ops: &mut Vec<Op>, run_a: &mut Option<Range<usize>>, run_b: &mut Option<Range<usize>>| {
            match (run_a.take(), run_b.take()) {
                (Some(a), Some(b)) => ops.push(Op::Replace { a, b }),
                (Some(a), None) => ops.push(Op::Delete { a }),
                (None, Some(b)) => ops.push(Op::Insert { b }),
                (None, None) => {}
            }
        };
    let push_equal = |ops: &mut Vec<Op>, i: usize, j: usize| match ops.last_mut() {
        Some(Op::Equal { a, b }) if a.end == i && b.end == j => {
            a.end += 1;
            b.end += 1;
        }
        _ => ops.push(Op::Equal {
            a: i..i + 1,
            b: j..j + 1,
        }),
    };
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            close(&mut ops, &mut run_a, &mut run_b);
            push_equal(&mut ops, i, j);
            i += 1;
            j += 1;
        } else if i < a.len() && (j == b.len() || t[i + 1][j] >= t[i][j + 1]) {
            let r = run_a.get_or_insert(i..i);
            r.end = i + 1;
            i += 1;
        } else {
            let r = run_b.get_or_insert(j..j);
            r.end = j + 1;
            j += 1;
        }
    }
    close(&mut ops, &mut run_a, &mut run_b);
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn identical_texts_are_one_equal_block() {
        let a = words("uno dos tres");
        assert_eq!(diff(&a, &a), [Op::Equal { a: 0..3, b: 0..3 }]);
        assert_eq!(lcs_len(&a, &a), 3);
        assert!(diff(&[], &[]).is_empty());
    }

    #[test]
    fn a_split_word_becomes_one_replace_block() {
        let ops = diff(&words("hice de ploy ayer"), &words("hice deploy ayer"));
        assert_eq!(
            ops,
            [
                Op::Equal { a: 0..1, b: 0..1 },
                Op::Replace { a: 1..3, b: 1..2 },
                Op::Equal { a: 3..4, b: 2..3 },
            ]
        );
    }

    #[test]
    fn inserts_and_deletes_are_told_apart() {
        let ops = diff(&words("a b c d"), &words("a x b c"));
        assert_eq!(
            ops,
            [
                Op::Equal { a: 0..1, b: 0..1 },
                Op::Insert { b: 1..2 },
                Op::Equal { a: 1..3, b: 2..4 },
                Op::Delete { a: 3..4 },
            ]
        );
    }

    #[test]
    fn the_script_always_rebuilds_both_sides() {
        let pairs = [
            (
                "el bug de estaging falló",
                "el bug de staging fallo otra vez",
            ),
            ("", "hola mundo"),
            ("hola mundo", ""),
            ("a a a b", "a b b b"),
        ];
        for (x, y) in pairs {
            let (a, b) = (words(x), words(y));
            let (mut ra, mut rb) = (Vec::new(), Vec::new());
            for op in diff(&a, &b) {
                match op {
                    Op::Equal { a: r, b: s } => {
                        assert_eq!(a[r.clone()], b[s.clone()]);
                        ra.extend(r);
                        rb.extend(s);
                    }
                    Op::Replace { a: r, b: s } => {
                        ra.extend(r);
                        rb.extend(s);
                    }
                    Op::Delete { a: r } => ra.extend(r),
                    Op::Insert { b: s } => rb.extend(s),
                }
            }
            assert_eq!(ra, (0..a.len()).collect::<Vec<_>>(), "{x:?} -> {y:?}");
            assert_eq!(rb, (0..b.len()).collect::<Vec<_>>(), "{x:?} -> {y:?}");
        }
    }
}
