//! Port of the reference `textwrap.wrap` / `textwrap.fill` with the defaults argparse relies on.
//!
//! argparse wraps help text with `break_long_words=True`, `break_on_hyphens=True`,
//! `drop_whitespace=True`; lengths are counted in code points, exactly as the reference `len`.

use std::sync::LazyLock;

use fancy_regex::Regex;

/// The chunking regex of `textwrap.TextWrapper.wordsep_re` (whitespace runs, em-dashes, words
/// split after hyphens that sit inside letter sequences).
static WORDSEP: LazyLock<Option<Regex>> = LazyLock::new(|| {
    let ws = r"[\t\n\x0b\x0c\r ]";
    let nws = r"[^\t\n\x0b\x0c\r ]";
    let wp = r#"[\w!"'&.,?]"#;
    let lt = r"[^\d\W]";
    let pattern = format!(
        r"{ws}+|(?<={wp})-{{2,}}(?=\w)|{nws}+?(?:-(?:(?<={lt}{{2}}-)|(?<={lt}-{lt}-))(?={lt}-?{lt})|(?={ws}|\z)|(?<={wp})(?=-{{2,}}\w))"
    );
    Regex::new(&pattern).ok()
});

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn is_blank(s: &str) -> bool {
    s.chars()
        .all(|c| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r'))
}

fn split_chunks(text: &str) -> Vec<String> {
    let text: String = text
        .chars()
        .map(|c| {
            if matches!(c, '\t' | '\n' | '\x0b' | '\x0c' | '\r') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let Some(re) = WORDSEP.as_ref() else {
        return text.split(' ').map(str::to_owned).collect();
    };
    let mut chunks = Vec::new();
    let mut last = 0;
    for m in re.find_iter(&text).flatten() {
        if m.start() > last {
            chunks.push(text[last..m.start()].to_owned());
        }
        if m.end() > m.start() {
            chunks.push(m.as_str().to_owned());
        }
        last = m.end();
    }
    if last < text.len() {
        chunks.push(text[last..].to_owned());
    }
    chunks
}

fn take_chars(s: &str, n: usize) -> (&str, &str) {
    let idx = s.char_indices().nth(n).map_or(s.len(), |(i, _)| i);
    s.split_at(idx)
}

fn handle_long_word(
    chunks: &mut Vec<String>,
    cur_line: &mut Vec<String>,
    cur_len: usize,
    width: usize,
) {
    let space_left = if width < 1 {
        1
    } else {
        width.saturating_sub(cur_len)
    };
    if space_left > 0 {
        let Some(chunk) = chunks.last_mut() else {
            return;
        };
        let mut end = space_left;
        if char_len(chunk) > space_left {
            let head: Vec<char> = chunk.chars().take(space_left).collect();
            if let Some(h) = head.iter().rposition(|&c| c == '-') {
                if h > 0 && head[..h].iter().any(|&c| c != '-') {
                    end = h + 1;
                }
            }
        }
        let (a, b) = take_chars(chunk, end);
        let (a, b) = (a.to_owned(), b.to_owned());
        cur_line.push(a);
        *chunk = b;
    } else if cur_line.is_empty() {
        if let Some(c) = chunks.pop() {
            cur_line.push(c);
        }
    }
}

/// Wrap `text` to `width` columns; every line starts with `initial` (first) or `subsequent`.
pub fn wrap(text: &str, width: usize, initial: &str, subsequent: &str) -> Vec<String> {
    let mut chunks = split_chunks(text);
    chunks.reverse();
    let mut lines: Vec<String> = Vec::new();
    while !chunks.is_empty() {
        let mut cur_line: Vec<String> = Vec::new();
        let mut cur_len = 0usize;
        let indent = if lines.is_empty() {
            initial
        } else {
            subsequent
        };
        let width = width.saturating_sub(char_len(indent));
        if !lines.is_empty() && chunks.last().is_some_and(|c| is_blank(c)) {
            chunks.pop();
        }
        while let Some(last) = chunks.last() {
            let l = char_len(last);
            if cur_len + l > width {
                break;
            }
            cur_len += l;
            if let Some(c) = chunks.pop() {
                cur_line.push(c);
            }
        }
        if chunks.last().is_some_and(|c| char_len(c) > width) {
            handle_long_word(&mut chunks, &mut cur_line, cur_len, width);
        }
        if cur_line.last().is_some_and(|c| is_blank(c)) {
            cur_line.pop();
        }
        if !cur_line.is_empty() {
            lines.push(format!("{indent}{}", cur_line.concat()));
        }
    }
    lines
}

/// `textwrap.fill`: [`wrap`] joined with newlines, same indent on every line.
pub fn fill(text: &str, width: usize, indent: &str) -> String {
    wrap(text, width, indent, indent).join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyphen_split_and_long_words() {
        // from textwrap docs: goof-ball splits after the hyphen
        let lines = wrap("Look, goof-ball -- use the -b option!", 10, "", "");
        assert_eq!(lines, ["Look,", "goof-ball", "-- use the", "-b option!"]);
        assert_eq!(wrap("abcdefghij", 4, "", ""), ["abcd", "efgh", "ij"]);
        assert!(wrap("", 10, "", "").is_empty());
    }
}
