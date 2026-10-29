//! Compile a pattern with `fancy-regex`, rejecting forms the reference syntax rejects
//! but the engine accepts, and mapping anchors that only exist in the reference syntax.

use fancy_regex::{Regex, RegexBuilder};

/// `{m,n}` with `m > n` is an error.
fn has_inverted_repeat(chars: &[char]) -> bool {
    let mut i = 0;
    let mut in_class = false;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 1,
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            '{' if !in_class => {
                let rest: String = chars[i + 1..].iter().collect();
                if let Some(end) = rest.find('}') {
                    if let Some((lo, hi)) = rest[..end].split_once(',') {
                        if let (Ok(lo), Ok(hi)) = (lo.parse::<u64>(), hi.parse::<u64>()) {
                            if lo > hi {
                                return true;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// `\Z` (end of string) is `\z` for the Rust engine.
fn translate(chars: &[char]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && chars.get(i + 1) == Some(&'Z') {
            out.push_str("\\z");
            i += 2;
        } else if chars[i] == '\\' && i + 1 < chars.len() {
            out.push(chars[i]);
            out.push(chars[i + 1]);
            i += 2;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Build the regex; `Err` carries a message describing the rejection.
pub(crate) fn build(
    pattern: &str,
    ignore_case: bool,
    backtrack_limit: Option<usize>,
) -> Result<Regex, String> {
    let chars: Vec<char> = pattern.chars().collect();
    if has_inverted_repeat(&chars) {
        return Err("min repeat greater than max repeat".to_owned());
    }
    let mut builder = RegexBuilder::new(&translate(&chars));
    builder.case_insensitive(ignore_case);
    if let Some(limit) = backtrack_limit {
        builder.backtrack_limit(limit);
    }
    builder.build().map_err(|e| e.to_string())
}
