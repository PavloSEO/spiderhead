//! Sequence matcher (autojunk on, no `isjunk`) and unified diff text, matching the reference output.
//!
//! Output must stay byte-stable, so the match search, the 200-line "popular element"
//! purge and the opcode grouping follow the reference algorithm step for step.

use std::collections::{HashMap, HashSet};

type Block = (usize, usize, usize);

struct Matcher<'a> {
    a: &'a [&'a str],
    b: &'a [&'a str],
    b2j: HashMap<&'a str, Vec<usize>>,
}

impl<'a> Matcher<'a> {
    fn new(a: &'a [&'a str], b: &'a [&'a str]) -> Self {
        let mut b2j: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, line) in b.iter().enumerate() {
            b2j.entry(line).or_default().push(i);
        }
        let n = b.len();
        if n >= 200 {
            let ntest = n / 100 + 1;
            let popular: HashSet<&str> = b2j
                .iter()
                .filter(|(_, idx)| idx.len() > ntest)
                .map(|(k, _)| *k)
                .collect();
            for line in popular {
                b2j.remove(line);
            }
        }
        Self { a, b, b2j }
    }

    fn find_longest_match(&self, alo: usize, ahi: usize, blo: usize, bhi: usize) -> Block {
        let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
        let mut j2len: HashMap<usize, usize> = HashMap::new();
        for i in alo..ahi {
            let mut newj2len: HashMap<usize, usize> = HashMap::new();
            if let Some(indices) = self.b2j.get(self.a[i]) {
                for &j in indices {
                    if j < blo {
                        continue;
                    }
                    if j >= bhi {
                        break;
                    }
                    let k = if j > 0 {
                        j2len.get(&(j - 1)).copied().unwrap_or(0)
                    } else {
                        0
                    } + 1;
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
        // Extend over "popular" lines that b2j left out (there is no junk set).
        while besti > alo && bestj > blo && self.a[besti - 1] == self.b[bestj - 1] {
            besti -= 1;
            bestj -= 1;
            bestsize += 1;
        }
        while besti + bestsize < ahi
            && bestj + bestsize < bhi
            && self.a[besti + bestsize] == self.b[bestj + bestsize]
        {
            bestsize += 1;
        }
        (besti, bestj, bestsize)
    }

    fn matching_blocks(&self) -> Vec<Block> {
        let (la, lb) = (self.a.len(), self.b.len());
        let mut queue = vec![(0, la, 0, lb)];
        let mut blocks: Vec<Block> = Vec::new();
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
        let mut merged: Vec<Block> = Vec::new();
        for (i2, j2, k2) in blocks {
            if i1 + k1 == i2 && j1 + k1 == j2 {
                k1 += k2;
            } else {
                if k1 > 0 {
                    merged.push((i1, j1, k1));
                }
                (i1, j1, k1) = (i2, j2, k2);
            }
        }
        if k1 > 0 {
            merged.push((i1, j1, k1));
        }
        merged.push((la, lb, 0));
        merged
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tag {
    Replace,
    Delete,
    Insert,
    Equal,
}

type Opcode = (Tag, usize, usize, usize, usize);

fn opcodes(m: &Matcher) -> Vec<Opcode> {
    let (mut i, mut j) = (0, 0);
    let mut answer = Vec::new();
    for (ai, bj, size) in m.matching_blocks() {
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

fn grouped_opcodes(m: &Matcher, n: usize) -> Vec<Vec<Opcode>> {
    let mut codes = opcodes(m);
    if codes.is_empty() {
        codes.push((Tag::Equal, 0, 1, 0, 1));
    }
    if let Some(&(Tag::Equal, i1, i2, j1, j2)) = codes.first() {
        codes[0] = (
            Tag::Equal,
            i1.max(i2.saturating_sub(n)),
            i2,
            j1.max(j2.saturating_sub(n)),
            j2,
        );
    }
    let last = codes.len() - 1;
    if let (Tag::Equal, i1, i2, j1, j2) = codes[last] {
        codes[last] = (Tag::Equal, i1, i2.min(i1 + n), j1, j2.min(j1 + n));
    }
    let nn = n + n;
    let mut groups = Vec::new();
    let mut group: Vec<Opcode> = Vec::new();
    for (tag, mut i1, i2, mut j1, j2) in codes {
        if tag == Tag::Equal && i2 - i1 > nn {
            group.push((tag, i1, i2.min(i1 + n), j1, j2.min(j1 + n)));
            groups.push(std::mem::take(&mut group));
            i1 = i1.max(i2.saturating_sub(n));
            j1 = j1.max(j2.saturating_sub(n));
        }
        group.push((tag, i1, i2, j1, j2));
    }
    if !group.is_empty() && !(group.len() == 1 && group[0].0 == Tag::Equal) {
        groups.push(group);
    }
    groups
}

fn format_range(start: usize, stop: usize) -> String {
    let mut beginning = start as i64 + 1;
    let length = stop - start;
    if length == 1 {
        return beginning.to_string();
    }
    if length == 0 {
        beginning -= 1;
    }
    format!("{beginning},{length}")
}

/// `list(difflib.unified_diff(a, b, fromfile, tofile, lineterm=""))` with the default context of 3.
pub(super) fn unified_diff(a: &[&str], b: &[&str], fromfile: &str, tofile: &str) -> Vec<String> {
    let matcher = Matcher::new(a, b);
    let mut out = Vec::new();
    for (index, group) in grouped_opcodes(&matcher, 3).iter().enumerate() {
        if index == 0 {
            out.push(format!("--- {fromfile}"));
            out.push(format!("+++ {tofile}"));
        }
        let (first, last) = (group[0], group[group.len() - 1]);
        out.push(format!(
            "@@ -{} +{} @@",
            format_range(first.1, last.2),
            format_range(first.3, last.4)
        ));
        for &(tag, i1, i2, j1, j2) in group {
            if tag == Tag::Equal {
                out.extend(a[i1..i2].iter().map(|l| format!(" {l}")));
                continue;
            }
            if matches!(tag, Tag::Replace | Tag::Delete) {
                out.extend(a[i1..i2].iter().map(|l| format!("-{l}")));
            }
            if matches!(tag, Tag::Replace | Tag::Insert) {
                out.extend(b[j1..j2].iter().map(|l| format!("+{l}")));
            }
        }
    }
    out
}
