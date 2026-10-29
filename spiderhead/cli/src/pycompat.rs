//! Small reference behaviours argparse's messages depend on: `repr(str)`, `int()` / `float()`
//! acceptance, and `difflib.get_close_matches`.

/// `repr()` of a reference `str`.
pub fn repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f || ((0x80..0xa0).contains(&(c as u32))) => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

fn digits_with_underscores(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('_')
        && !s.ends_with('_')
        && !s.contains("__")
        && s.chars().all(|c| c.is_ascii_digit() || c == '_')
}

/// Whether the reference `int(text)` accepts `text` (base 10, any size).
pub fn is_py_int(text: &str) -> bool {
    let t = text.trim();
    digits_with_underscores(t.strip_prefix(['+', '-']).unwrap_or(t))
}

/// Whether the reference `float(text)` accepts `text`.
pub fn is_py_float(text: &str) -> bool {
    let t = text.trim();
    let t = t.strip_prefix(['+', '-']).unwrap_or(t);
    let lower = t.to_ascii_lowercase();
    if matches!(lower.as_str(), "inf" | "infinity" | "nan") {
        return true;
    }
    let (mantissa, exponent) = match lower.split_once('e') {
        Some((m, e)) => (m, Some(e)),
        None => (lower.as_str(), None),
    };
    let (int, frac) = mantissa
        .split_once('.')
        .map_or((mantissa, None), |(i, f)| (i, Some(f)));
    let part_ok = |p: &str| p.is_empty() || digits_with_underscores(p);
    if int.is_empty() && frac.is_none_or(str::is_empty)
        || !part_ok(int)
        || !frac.is_none_or(part_ok)
    {
        return false;
    }
    exponent.is_none_or(|e| digits_with_underscores(e.strip_prefix(['+', '-']).unwrap_or(e)))
}

fn longest_match(
    a: &[char],
    b: &[char],
    alo: usize,
    ahi: usize,
    blo: usize,
    bhi: usize,
) -> (usize, usize, usize) {
    let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0);
    let mut j2len = vec![0usize; b.len() + 1];
    for (i, &ai) in a.iter().enumerate().take(ahi).skip(alo) {
        let mut new = vec![0usize; b.len() + 1];
        for j in blo..bhi {
            if ai == b[j] {
                let k = j2len[j] + 1;
                new[j + 1] = k;
                if k > bestsize {
                    (besti, bestj, bestsize) = (i + 1 - k, j + 1 - k, k);
                }
            }
        }
        j2len = new;
    }
    (besti, bestj, bestsize)
}

fn matched(a: &[char], b: &[char], alo: usize, ahi: usize, blo: usize, bhi: usize) -> usize {
    let (i, j, k) = longest_match(a, b, alo, ahi, blo, bhi);
    if k == 0 {
        return 0;
    }
    k + if alo < i && blo < j {
        matched(a, b, alo, i, blo, j)
    } else {
        0
    } + if i + k < ahi && j + k < bhi {
        matched(a, b, i + k, ahi, j + k, bhi)
    } else {
        0
    }
}

/// `difflib.SequenceMatcher(None, a, b).ratio()` (no junk heuristics: inputs are short).
pub fn ratio(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let total = a.len() + b.len();
    if total == 0 {
        return 1.0;
    }
    2.0 * matched(&a, &b, 0, a.len(), 0, b.len()) as f64 / total as f64
}

/// `difflib.get_close_matches(word, possibilities, n, cutoff=0.5)`.
pub fn close_matches<'a>(word: &str, possibilities: &[&'a str], n: usize) -> Vec<&'a str> {
    let mut scored: Vec<(f64, &str)> = possibilities
        .iter()
        .map(|&p| (ratio(p, word), p))
        .filter(|&(s, _)| s >= 0.5)
        .collect();
    scored.sort_by(|x, y| y.0.total_cmp(&x.0).then_with(|| y.1.cmp(x.1)));
    scored.into_iter().take(n).map(|(_, p)| p).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repr_picks_quotes_like_python() {
        assert_eq!(repr("a"), "'a'");
        assert_eq!(repr("it's"), "\"it's\"");
        assert_eq!(repr("a\"b'c"), "'a\"b\\'c'");
        assert_eq!(repr("t\tn\n\\"), "'t\\tn\\n\\\\'");
    }

    #[test]
    fn int_and_float_acceptance() {
        for ok in ["1", " -5 ", "+7", "1_000"] {
            assert!(is_py_int(ok), "{ok}");
        }
        for bad in ["", "1.5", "1__0", "_1", "abc", "0x10", "- 1"] {
            assert!(!is_py_int(bad), "{bad}");
        }
        for ok in ["1.5", ".5", "5.", "1e5", "-inf", "NaN", "1_0.5", " 2 "] {
            assert!(is_py_float(ok), "{ok}");
        }
        for bad in ["", ".", "e5", "1e", "1.5.2", "abc"] {
            assert!(!is_py_float(bad), "{bad}");
        }
    }

    #[test]
    fn close_matches_orders_by_score_then_name() {
        assert_eq!(
            close_matches("crawl-sitee", &["crawl-site", "crawl-control", "parse"], 3),
            ["crawl-site", "crawl-control"]
        );
    }
}
