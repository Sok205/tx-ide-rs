//! The slice of CPython's `difflib` that `tx artifact diff` uses: `SequenceMatcher` (no junk
//! predicate, `autojunk=True`) over lines, `unified_diff`, and `str.splitlines(keepends=True)`.

use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tag {
    Replace,
    Delete,
    Insert,
    Equal,
}

type Opcode = (Tag, usize, usize, usize, usize);

struct SequenceMatcher<'a> {
    a: &'a [&'a str],
    b: &'a [&'a str],
    /// `b2j` after the autojunk pass: every non-popular line → its indices in `b`, ascending.
    b2j: HashMap<&'a str, Vec<usize>>,
}

impl<'a> SequenceMatcher<'a> {
    fn new(a: &'a [&'a str], b: &'a [&'a str]) -> Self {
        let mut b2j: HashMap<&str, Vec<usize>> = HashMap::new();
        for (index, line) in b.iter().enumerate() {
            b2j.entry(line).or_default().push(index);
        }
        let n = b.len();
        if n >= 200 {
            let ntest = n / 100 + 1;
            let popular: HashSet<&str> = b2j
                .iter()
                .filter(|(_, indices)| indices.len() > ntest)
                .map(|(line, _)| *line)
                .collect();
            b2j.retain(|line, _| !popular.contains(line));
        }
        Self { a, b, b2j }
    }

    fn find_longest_match(
        &self,
        alo: usize,
        ahi: usize,
        blo: usize,
        bhi: usize,
    ) -> (usize, usize, usize) {
        let (a, b) = (self.a, self.b);
        let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0);
        let mut j2len: HashMap<usize, usize> = HashMap::new();
        for (i, line) in a.iter().enumerate().take(ahi).skip(alo) {
            let mut new_j2len = HashMap::new();
            for &j in self.b2j.get(line).map_or(&[][..], Vec::as_slice) {
                if j < blo {
                    continue;
                }
                if j >= bhi {
                    break;
                }
                let k = j
                    .checked_sub(1)
                    .and_then(|prev| j2len.get(&prev))
                    .copied()
                    .unwrap_or(0)
                    + 1;
                new_j2len.insert(j, k);
                if k > bestsize {
                    (besti, bestj, bestsize) = (i + 1 - k, j + 1 - k, k);
                }
            }
            j2len = new_j2len;
        }
        // No junk predicate, so `isbjunk` is always false: extend over equal (possibly popular)
        // neighbours; the junk-extension passes are no-ops.
        while besti > alo && bestj > blo && a[besti - 1] == b[bestj - 1] {
            (besti, bestj, bestsize) = (besti - 1, bestj - 1, bestsize + 1);
        }
        while besti + bestsize < ahi
            && bestj + bestsize < bhi
            && a[besti + bestsize] == b[bestj + bestsize]
        {
            bestsize += 1;
        }
        (besti, bestj, bestsize)
    }

    fn matching_blocks(&self) -> Vec<(usize, usize, usize)> {
        let (la, lb) = (self.a.len(), self.b.len());
        let mut queue = vec![(0, la, 0, lb)];
        let mut blocks = Vec::new();
        while let Some((alo, ahi, blo, bhi)) = queue.pop() {
            let (i, j, k) = self.find_longest_match(alo, ahi, blo, bhi);
            if k > 0 {
                blocks.push((i, j, k));
                if alo < i && blo < j {
                    queue.push((alo, i, blo, j));
                }
                if i + k < ahi && j + k < bhi {
                    queue.push((i + k, ahi, j + k, bhi));
                }
            }
        }
        blocks.sort_unstable();
        let (mut i1, mut j1, mut k1) = (0, 0, 0);
        let mut collapsed = Vec::new();
        for (i2, j2, k2) in blocks {
            if i1 + k1 == i2 && j1 + k1 == j2 {
                k1 += k2;
            } else {
                if k1 > 0 {
                    collapsed.push((i1, j1, k1));
                }
                (i1, j1, k1) = (i2, j2, k2);
            }
        }
        if k1 > 0 {
            collapsed.push((i1, j1, k1));
        }
        collapsed.push((la, lb, 0));
        collapsed
    }

    fn opcodes(&self) -> Vec<Opcode> {
        let (mut i, mut j) = (0, 0);
        let mut answer = Vec::new();
        for (ai, bj, size) in self.matching_blocks() {
            let tag = if i < ai && j < bj {
                Some(Tag::Replace)
            } else if i < ai {
                Some(Tag::Delete)
            } else if j < bj {
                Some(Tag::Insert)
            } else {
                None
            };
            if let Some(tag) = tag {
                answer.push((tag, i, ai, j, bj));
            }
            (i, j) = (ai + size, bj + size);
            if size > 0 {
                answer.push((Tag::Equal, ai, i, bj, j));
            }
        }
        answer
    }

    fn grouped_opcodes(&self, n: usize) -> Vec<Vec<Opcode>> {
        let mut codes = self.opcodes();
        if codes.is_empty() {
            codes.push((Tag::Equal, 0, 1, 0, 1));
        }
        if let Some(first) = codes.first_mut()
            && first.0 == Tag::Equal
        {
            let (tag, i1, i2, j1, j2) = *first;
            *first = (
                tag,
                i1.max(i2.saturating_sub(n)),
                i2,
                j1.max(j2.saturating_sub(n)),
                j2,
            );
        }
        if let Some(last) = codes.last_mut()
            && last.0 == Tag::Equal
        {
            let (tag, i1, i2, j1, j2) = *last;
            *last = (tag, i1, i2.min(i1 + n), j1, j2.min(j1 + n));
        }
        let mut groups = Vec::new();
        let mut group = Vec::new();
        for (tag, mut i1, i2, mut j1, j2) in codes {
            if tag == Tag::Equal && i2 - i1 > 2 * n {
                group.push((tag, i1, i2.min(i1 + n), j1, j2.min(j1 + n)));
                groups.push(std::mem::take(&mut group));
                i1 = i1.max(i2.saturating_sub(n));
                j1 = j1.max(j2.saturating_sub(n));
            }
            group.push((tag, i1, i2, j1, j2));
        }
        let only_equal = group.len() == 1 && group[0].0 == Tag::Equal;
        if !group.is_empty() && !only_equal {
            groups.push(group);
        }
        groups
    }
}

fn format_range_unified(start: usize, stop: usize) -> String {
    let beginning = start + 1;
    let length = stop - start;
    match length {
        1 => beginning.to_string(),
        0 => format!("{},0", beginning - 1),
        _ => format!("{beginning},{length}"),
    }
}

/// `"".join(difflib.unified_diff(a, b, fromfile, tofile))` with the defaults (`n=3`,
/// `lineterm="\n"`, no dates). Lines keep their own endings.
pub fn unified_diff(a: &[&str], b: &[&str], fromfile: &str, tofile: &str) -> String {
    let matcher = SequenceMatcher::new(a, b);
    let mut out = String::new();
    for (index, group) in matcher.grouped_opcodes(3).into_iter().enumerate() {
        if index == 0 {
            out.push_str(&format!("--- {fromfile}\n+++ {tofile}\n"));
        }
        let (first, last) = (group[0], group[group.len() - 1]);
        out.push_str(&format!(
            "@@ -{} +{} @@\n",
            format_range_unified(first.1, last.2),
            format_range_unified(first.3, last.4)
        ));
        for (tag, i1, i2, j1, j2) in group {
            if tag == Tag::Equal {
                for line in &a[i1..i2] {
                    out.push(' ');
                    out.push_str(line);
                }
                continue;
            }
            if matches!(tag, Tag::Replace | Tag::Delete) {
                for line in &a[i1..i2] {
                    out.push('-');
                    out.push_str(line);
                }
            }
            if matches!(tag, Tag::Replace | Tag::Insert) {
                for line in &b[j1..j2] {
                    out.push('+');
                    out.push_str(line);
                }
            }
        }
    }
    out
}

/// `str.splitlines(keepends=True)`: every Unicode line boundary Python recognises, `\r\n` as one.
pub fn splitlines_keepends(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        let end = match c {
            '\r' => match chars.peek() {
                Some(&(next, '\n')) => {
                    chars.next();
                    next + 1
                }
                _ => index + 1,
            },
            '\n' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}'
            | '\u{2029}' => index + c.len_utf8(),
            _ => continue,
        };
        lines.push(&text[start..end]);
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diff(a: &str, b: &str) -> String {
        unified_diff(
            &splitlines_keepends(a),
            &splitlines_keepends(b),
            "rev0",
            "rev1",
        )
    }

    #[test]
    fn splitlines_matches_python() {
        // "a\r\nb\rc\x0bd e".splitlines(True)
        assert_eq!(
            splitlines_keepends("a\r\nb\rc\x0bd\u{2028}e"),
            ["a\r\n", "b\r", "c\x0b", "d\u{2028}", "e"]
        );
        assert!(splitlines_keepends("").is_empty());
        assert_eq!(splitlines_keepends("x\n"), ["x\n"]);
    }

    #[test]
    fn unified_diff_matches_python() {
        // Expected values from difflib.unified_diff under python3.14.
        assert_eq!(diff("a\nb\n", "a\nb\n"), "");
        assert_eq!(
            diff("a\nb\n", "a\nc\nd\n"),
            "--- rev0\n+++ rev1\n@@ -1,2 +1,3 @@\n a\n-b\n+c\n+d\n"
        );
        assert_eq!(diff("", "x"), "--- rev0\n+++ rev1\n@@ -0,0 +1 @@\n+x");
        assert_eq!(
            diff("body\n", "v1\n"),
            "--- rev0\n+++ rev1\n@@ -1 +1 @@\n-body\n+v1\n"
        );
        let a: String = (0..20).map(|i| format!("{i}\n")).collect();
        let b = a.replace("2\n", "two\n").replace("17\n", "seventeen\n");
        assert_eq!(
            diff(&a, &b),
            "--- rev0\n+++ rev1\n@@ -1,6 +1,6 @@\n 0\n 1\n-2\n+two\n 3\n 4\n 5\n\
             @@ -10,11 +10,11 @@\n 9\n 10\n 11\n-12\n+1two\n 13\n 14\n 15\n 16\n-17\n\
             +seventeen\n 18\n 19\n"
        );
    }

    #[test]
    fn autojunk_popular_lines_are_not_anchors() {
        // 250 blank lines around one change: blanks are popular (> 3 occurrences of 250), so
        // nothing after the change anchors and CPython reports the whole tail as replaced.
        let a = format!("{}x\n{}", "\n".repeat(125), "\n".repeat(125));
        let b = format!("{}y\n{}", "\n".repeat(125), "\n".repeat(125));
        let expected_python = format!(
            "--- rev0\n+++ rev1\n@@ -123,129 +123,129 @@\n \n \n \n-x\n{}+y\n{}",
            "-\n".repeat(125),
            "+\n".repeat(125)
        );
        assert_eq!(diff(&a, &b), expected_python);
    }
}
