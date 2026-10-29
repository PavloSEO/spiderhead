//! Charset resolution for HTML bodies: every text field of a scan is stored as UTF-8.

use encoding_rs::{Encoding, UTF_16BE, UTF_16LE, UTF_8, WINDOWS_1252};

/// An HTML body decoded to UTF-8, with the declarations it was decoded from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedHtml {
    /// The body as UTF-8 text; malformed sequences become U+FFFD.
    pub text: String,
    /// `charset` parameter of the `Content-Type` header, lowercased; empty when absent.
    pub http_charset: String,
    /// Charset of the `<meta>` declaration in the first 1024 bytes, lowercased; empty when absent.
    pub meta_charset: String,
    /// Canonical name of the encoding actually used for decoding.
    pub charset_used: &'static str,
}

/// How many leading bytes are scanned for a `<meta>` charset declaration.
const PRESCAN_BYTES: usize = 1024;

/// Decode an HTML body.
///
/// Order: byte-order mark, then the HTTP charset, then the `<meta>` declaration, then a
/// UTF-8 validity check, then windows-1252. A Latin-1/ASCII HTTP charset is a common
/// server default rather than a statement about the page, so a differing `<meta>`
/// declaration wins over it.
pub fn decode_html(raw: &[u8], content_type: &str) -> DecodedHtml {
    let http_charset = http_charset(content_type);
    let meta_charset = meta_charset(raw);
    let http = Encoding::for_label(http_charset.as_bytes());
    let meta = Encoding::for_label(meta_charset.as_bytes()).map(meta_encoding);

    let chosen = if let Some((bom, _)) = Encoding::for_bom(raw) {
        bom
    } else {
        match (http, meta) {
            (Some(h), Some(m)) if h == WINDOWS_1252 && m != h => m,
            (Some(h), _) => h,
            (None, Some(m)) => m,
            (None, None) if std::str::from_utf8(raw).is_ok() => UTF_8,
            (None, None) => WINDOWS_1252,
        }
    };
    let (text, used, _) = chosen.decode(raw);
    DecodedHtml {
        text: text.into_owned(),
        http_charset,
        meta_charset,
        charset_used: used.name(),
    }
}

/// A `<meta>` tag read as ASCII cannot be in UTF-16; browsers treat such a label as UTF-8.
fn meta_encoding(enc: &'static Encoding) -> &'static Encoding {
    if enc == UTF_16LE || enc == UTF_16BE {
        UTF_8
    } else {
        enc
    }
}

fn http_charset(content_type: &str) -> String {
    for part in content_type.split(';').skip(1) {
        let part = part.trim();
        let Some((name, value)) = part.split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("charset") {
            let label = value.trim().trim_matches(|c| c == '"' || c == '\'').trim();
            if !label.is_empty() {
                return label.to_ascii_lowercase();
            }
        }
    }
    String::new()
}

/// Label of the first `<meta charset=…>` or `<meta http-equiv=content-type content="…charset=…">`.
fn meta_charset(raw: &[u8]) -> String {
    let window = &raw[..raw.len().min(PRESCAN_BYTES)];
    let lower = window.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = find(&lower[from..], b"<meta") {
        let start = from + at + 5;
        let end = lower[start..]
            .iter()
            .position(|&b| b == b'>')
            .map_or(lower.len(), |p| start + p);
        let tag = &lower[start..end];
        if let Some(label) = charset_attr(tag) {
            return label;
        }
        from = start;
    }
    String::new()
}

fn charset_attr(tag: &[u8]) -> Option<String> {
    let mut from = 0;
    while let Some(at) = find(&tag[from..], b"charset") {
        let pos = from + at;
        from = pos + 7;
        // `data-charset` and `accept-charset` are other attributes.
        if pos > 0 && (tag[pos - 1].is_ascii_alphanumeric() || tag[pos - 1] == b'-') {
            continue;
        }
        let mut i = from;
        while tag.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        if tag.get(i) != Some(&b'=') {
            continue;
        }
        i += 1;
        while tag.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        if matches!(tag.get(i), Some(b'"' | b'\'')) {
            i += 1;
        }
        let label: String = tag[i..]
            .iter()
            .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b':' | b'.'))
            .map(|&b| b as char)
            .collect();
        if !label.is_empty() {
            return Some(label);
        }
    }
    None
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(s: &str, name: &str) -> Vec<u8> {
        Encoding::for_label(name.as_bytes())
            .unwrap()
            .encode(s)
            .0
            .into_owned()
    }

    fn page(head: &str, body: &str, name: &str) -> Vec<u8> {
        enc(
            &format!("<html><head>{head}</head><body>{body}</body></html>"),
            name,
        )
    }

    #[test]
    fn meta_beats_latin1_http_default() {
        let raw = page(
            "<meta charset=windows-1251><title>Старая коллекция</title>",
            "",
            "windows-1251",
        );
        let d = decode_html(&raw, "text/html; charset=iso-8859-1");
        assert!(d.text.contains("Старая коллекция"));
        assert_eq!(
            (
                d.http_charset.as_str(),
                d.meta_charset.as_str(),
                d.charset_used
            ),
            ("iso-8859-1", "windows-1251", "windows-1251")
        );
    }

    #[test]
    fn http_wins_over_meta_when_not_a_latin_default() {
        let raw = page("<meta charset=windows-1251>", "Привет", "koi8-r");
        let d = decode_html(&raw, "text/html; charset=koi8-r");
        assert!(d.text.contains("Привет"));
        assert_eq!(d.charset_used, "KOI8-R");
    }

    #[test]
    fn koi8r_from_meta_http_equiv() {
        let head = r#"<meta http-equiv="Content-Type" content="text/html; charset=koi8-r">"#;
        let d = decode_html(&page(head, "Привет, мир", "koi8-r"), "text/html");
        assert!(d.text.contains("Привет, мир"));
        assert_eq!(d.meta_charset, "koi8-r");
        assert_eq!(d.http_charset, "");
    }

    #[test]
    fn utf8_bom_beats_declarations() {
        let mut raw = b"\xef\xbb\xbf".to_vec();
        raw.extend(page("<meta charset=windows-1251>", "Привет", "utf-8"));
        let d = decode_html(&raw, "text/html; charset=windows-1251");
        assert!(d.text.contains("Привет"));
        assert_eq!(d.charset_used, "UTF-8");
    }

    #[test]
    fn undeclared_valid_utf8_and_fallback() {
        let d = decode_html(&page("", "Привет", "utf-8"), "text/html");
        assert!(d.text.contains("Привет"));
        assert_eq!(d.charset_used, "UTF-8");
        let d = decode_html(b"caf\xe9", "text/html");
        assert_eq!((d.text.as_str(), d.charset_used), ("café", "windows-1252"));
    }

    #[test]
    fn latin1_header_without_meta_stays_latin1() {
        let d = decode_html(b"caf\xe9", "text/html; charset=iso-8859-1");
        assert_eq!(d.text, "café");
    }

    #[test]
    fn data_charset_attribute_is_not_a_declaration() {
        assert_eq!(meta_charset(b"<meta data-charset=koi8-r>"), "");
        assert_eq!(meta_charset(b"<meta name=x charset='UTF-8'>"), "utf-8");
    }

    #[test]
    fn utf16_meta_label_means_utf8() {
        let d = decode_html("<meta charset=utf-16>Привет".as_bytes(), "text/html");
        assert!(d.text.contains("Привет"));
    }
}
