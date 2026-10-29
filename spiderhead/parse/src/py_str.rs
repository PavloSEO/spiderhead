//! String behaviour that the hreflang check and its messages depend on.
//! Covers strip (a wider whitespace set than `char::is_whitespace`), quoted display in messages,
//! and capitalize.

/// Whitespace test for one code point: the Unicode white-space set plus U+001C..U+001F.
pub fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Strip leading and trailing characters that count as whitespace.
pub fn py_strip(text: &str) -> &str {
    text.trim_matches(is_py_space)
}

/// Printable test for one code point.
///
/// Covers Cc, Cf, Co, Zs, Zl and Zp by range. Unassigned code points are not tabled, so a
/// message quoting one of them can differ from the standard `repr` output.
fn is_printable(c: char) -> bool {
    let code = c as u32;
    if c == ' ' {
        return true;
    }
    let non_printable = matches!(code,
        0x00..=0x1f
        | 0x7f..=0xa0
        | 0xad
        | 0x600..=0x605
        | 0x61c
        | 0x6dd
        | 0x70f
        | 0x8e2
        | 0x1680
        | 0x180e
        | 0x2000..=0x200f
        | 0x2028..=0x202f
        | 0x205f..=0x206f
        | 0x3000
        | 0xd800..=0xf8ff
        | 0xfeff
        | 0xfff9..=0xfffb
        | 0x13430..=0x13438
        | 0x1bca0..=0x1bca3
        | 0x1d173..=0x1d17a
        | 0xe0001
        | 0xe0020..=0xe007f
        | 0xf0000..=0xffffd
        | 0x100000..=0x10fffd
    );
    !non_printable
}

/// Quoted display of a string, as used in the hreflang messages.
///
/// Uses single quotes unless the text holds a single quote and no double quote. Backslash,
/// the chosen quote, tab, newline and carriage return are escaped; other non-printable code
/// points become `\xhh`, `\uhhhh` or `\Uhhhhhhhh`.
pub fn py_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if is_printable(c) => out.push(c),
            c => push_escape(&mut out, c as u32),
        }
    }
    out.push(quote);
    out
}

/// Append the \x, \u or \U escape for a code point.
fn push_escape(out: &mut String, code: u32) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let (prefix, width) = if code <= 0xff {
        ('x', 2)
    } else if code <= 0xffff {
        ('u', 4)
    } else {
        ('U', 8)
    };
    out.push('\\');
    out.push(prefix);
    for shift in (0..width).rev() {
        let nibble = ((code >> (shift * 4)) & 0xf) as usize;
        out.push(HEX[nibble] as char);
    }
}

/// Capitalize: first code point in title case, the rest lower case.
///
/// The first code point uses the upper-case mapping, which matches the title-case mapping for
/// every code point that can appear in a script code after the ASCII comparison.
pub fn py_capitalize(text: &str) -> String {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut out = String::with_capacity(text.len());
    if first.is_ascii() {
        out.push(first.to_ascii_uppercase());
    } else {
        out.extend(first.to_uppercase());
    }
    for c in chars {
        out.extend(c.to_lowercase());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Behaviour is checked against fixed reference rows.
    #[test]
    fn strip_follows_python_isspace() {
        assert_eq!(py_strip("\u{1c}\u{3000}en\u{a0}"), "en");
        assert_eq!(py_strip("\u{200b}en"), "\u{200b}en");
    }

    #[test]
    fn repr_chooses_quotes_and_escapes_like_cpython() {
        assert_eq!(py_repr("en"), "'en'");
        assert_eq!(py_repr("it's"), "\"it's\"");
        assert_eq!(py_repr("a'b\"c"), "'a\\'b\"c'");
        assert_eq!(py_repr("a\tb\\c\n"), "'a\\tb\\\\c\\n'");
        assert_eq!(py_repr("\u{7f}"), "'\\x7f'");
        assert_eq!(py_repr("\u{200b}"), "'\\u200b'");
        assert_eq!(py_repr("\u{e000}"), "'\\ue000'");
        assert_eq!(py_repr("\u{1f600}"), "'\u{1f600}'");
    }

    #[test]
    fn capitalize_title_cases_the_first_code_point() {
        assert_eq!(py_capitalize("hANS"), "Hans");
        assert_eq!(py_capitalize("zzzz"), "Zzzz");
        assert_eq!(py_capitalize(""), "");
    }
}
