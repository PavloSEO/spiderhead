//! Charset resolution and decoding for `decode_entity`.
//!
//! The label goes through the same steps as MIME content-charset parsing and codec lookup;
//! decoding replaces malformed bytes as `bytes.decode(name, errors="replace")` does. One-byte codecs
//! use generated tables (`charset_data.rs`), so `latin-1` is a true identity map and
//! `cp1252` leaves its five undefined bytes as U+FFFD (unlike the WHATWG labels of `encoding_rs`).

use super::charset_data::{ALIASES, SINGLE_BYTE};
use crate::error::StoreError;
use regex::Regex;
use std::sync::OnceLock;

/// Whitespace test: Unicode White_Space plus U+001C..U+001F.
pub(crate) fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

fn py_rstrip(s: &str) -> &str {
    s.trim_end_matches(py_isspace)
}

fn py_lstrip(s: &str) -> &str {
    s.trim_start_matches(py_isspace)
}

/// Count of `sub` in `text[start..end]`, with clamped indexes (byte offsets; all callers
/// search ASCII needles, so UTF-8 offsets and character offsets agree for the matched substrings).
fn count_range(s: &str, sub: &str, start: i64, end: i64) -> i64 {
    let len = s.len() as i64;
    let norm = |i: i64| (if i < 0 { i + len } else { i }).clamp(0, len);
    let (start, end) = (norm(start), norm(end));
    if start > end {
        return 0;
    }
    let hay = &s.as_bytes()[start as usize..end as usize];
    let needle = sub.as_bytes();
    let mut count = 0;
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if &hay[i..i + needle.len()] == needle {
            count += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    count
}

fn find_from(s: &str, ch: char, from: i64) -> i64 {
    let from = from.clamp(0, s.len() as i64) as usize;
    s[from..].find(ch).map_or(-1, |i| (i + from) as i64)
}

/// Split a header value into stripped `name=value` pieces.
fn parse_param(value: &str) -> Vec<String> {
    let s = format!(";{value}");
    let mut list = Vec::new();
    let mut start: i64 = 0;
    while find_from(&s, ';', start) == start {
        start += 1;
        let mut end = find_from(&s, ';', start);
        let mut ind = start;
        let mut diff = 0;
        while end > 0 {
            diff += count_range(&s, "\"", ind, end) - count_range(&s, "\\\"", ind, end);
            if diff % 2 == 0 {
                break;
            }
            let next = find_from(&s, ';', end + 1);
            end = ind;
            ind = next;
        }
        if end < 0 {
            end = s.len() as i64;
        }
        let (a, b) = (start as usize, end as usize);
        let found = s[a..b].find('=');
        let piece = match found {
            None => s[a..b].to_owned(),
            Some(i) => format!(
                "{}={}",
                py_rstrip(&s[a..a + i]).to_lowercase(),
                py_lstrip(&s[a + i + 1..b])
            ),
        };
        list.push(py_strip(&piece).to_owned());
        start = end;
    }
    list
}

/// Remove surrounding quotes from a header parameter value.
fn unquote(s: &str) -> String {
    let n = s.chars().count();
    if n > 1 {
        if s.starts_with('"') && s.ends_with('"') {
            return s[1..s.len() - 1]
                .replace("\\\\", "\\")
                .replace("\\\"", "\"");
        }
        if s.starts_with('<') && s.ends_with('>') {
            return s[1..s.len() - 1].to_owned();
        }
    }
    s.to_owned()
}

fn quote(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// One RFC 2231 piece: continuation number, text, whether it is percent-encoded.
type Continuation = (Option<i64>, String, bool);

/// A parameter value: plain text, or the `(charset, language, value)` triple of RFC 2231.
enum Param {
    Plain(String),
    Extended(Option<String>, Option<String>, String),
}

fn continuation() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(?P<name>\w+)\*((?P<num>[0-9]+)\*?)?$").expect("static regex"))
}

/// Percent-decode `s` as Latin-1 text.
fn percent_latin1(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && b[i + 1].is_ascii_hexdigit()
            && b[i + 2].is_ascii_hexdigit()
        {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v as char);
                i += 3;
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Collect content-type parameters, decode RFC 2231 values, then read `charset`.
fn charset_param(content_type: &str) -> Option<Param> {
    let mut params: Vec<(String, String)> = parse_param(content_type)
        .into_iter()
        .map(|p| match p.split_once('=') {
            Some((n, v)) => (py_strip(n).to_owned(), py_strip(v).to_owned()),
            None => (py_strip(&p).to_owned(), String::new()),
        })
        .collect();
    let mut decoded: Vec<(String, Param)> =
        vec![(params[0].0.clone(), Param::Plain(params[0].1.clone()))];
    let mut groups: Vec<(String, Vec<Continuation>)> = Vec::new();
    for (name, value) in params.drain(1..) {
        let encoded = name.ends_with('*');
        let value = unquote(&value);
        if let Some(m) = continuation().captures(&name) {
            let base = m["name"].to_owned();
            let num = m.name("num").and_then(|n| n.as_str().parse::<i64>().ok());
            match groups.iter_mut().find(|g| g.0 == base) {
                Some(g) => g.1.push((num, value, encoded)),
                None => groups.push((base, vec![(num, value, encoded)])),
            }
        } else {
            decoded.push((name, Param::Plain(format!("\"{}\"", quote(&value)))));
        }
    }
    for (name, mut conts) in groups {
        let has_zero = conts.iter().any(|c| c.0 == Some(0));
        if has_zero {
            conts.retain(|c| c.0.is_some());
        } else {
            for c in &mut conts {
                c.0 = Some(c.0.unwrap_or(0));
            }
        }
        conts.sort_by_key(|c| c.0);
        let mut extended = false;
        let mut joined = String::new();
        for (_, s, encoded) in conts {
            if encoded {
                joined.push_str(&percent_latin1(&s));
                extended = true;
            } else {
                joined.push_str(&s);
            }
        }
        let value = quote(&joined);
        if extended {
            let parts: Vec<&str> = value.splitn(3, '\'').collect();
            let triple = if parts.len() <= 2 {
                Param::Extended(None, None, format!("\"{value}\""))
            } else {
                Param::Extended(
                    Some(parts[0].to_owned()),
                    Some(parts[1].to_owned()),
                    format!("\"{}\"", parts[2]),
                )
            };
            decoded.push((name, triple));
        } else {
            decoded.push((name, Param::Plain(format!("\"{value}\""))));
        }
    }
    decoded
        .into_iter()
        .find(|(k, _)| k.to_lowercase() == "charset")
        .map(|(_, v)| match v {
            Param::Plain(s) => Param::Plain(unquote(&s)),
            Param::Extended(c, l, s) => Param::Extended(c, l, unquote(&s)),
        })
}

/// Charset of a bare content-type string.
pub(crate) fn content_charset(content_type: &str) -> Option<String> {
    let charset = match charset_param(content_type)? {
        Param::Plain(s) => s,
        Param::Extended(charset, _language, text) => {
            let wanted = charset
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| "us-ascii".into());
            // `text.encode('raw-unicode-escape')` then decode strictly with the declared charset.
            let mut bytes = Vec::new();
            for c in text.chars() {
                if (c as u32) < 256 {
                    bytes.push(c as u8);
                } else if (c as u32) < 0x10000 {
                    bytes.extend(format!("\\u{:04x}", c as u32).bytes());
                } else {
                    bytes.extend(format!("\\U{:08x}", c as u32).bytes());
                }
            }
            lookup(&wanted)
                .and_then(|name| decode_strict(name, &bytes))
                .unwrap_or(text)
        }
    };
    if !charset.is_ascii() {
        return None;
    }
    Some(charset.to_lowercase())
}

/// Lower-cased label with non-alphanumeric characters replaced by `_`.
fn normalize_encoding(label: &str) -> String {
    let mut out = String::new();
    let mut punct = false;
    for c in label.to_ascii_lowercase().chars() {
        if c.is_alphanumeric() || c == '.' {
            if punct && !out.is_empty() {
                out.push('_');
            }
            if c.is_ascii() {
                out.push(c);
            }
            punct = false;
        } else {
            punct = true;
        }
    }
    out
}

/// Canonical codec name for a text codec; `None` when the label is unknown or the
/// codec is a binary transform (the caller falls back to UTF-8 in both cases).
pub(crate) fn lookup(label: &str) -> Option<&'static str> {
    let norm = normalize_encoding(label);
    let find = |key: &str| {
        ALIASES
            .binary_search_by(|(k, _, _)| (*k).cmp(key))
            .ok()
            .map(|i| ALIASES[i])
    };
    find(&norm)
        .or_else(|| find(&norm.replace('.', "_")).filter(|entry| entry.2))
        .map(|entry| entry.1)
}

/// Strict decoding for the few codecs RFC 2231 values need.
fn decode_strict(name: &str, bytes: &[u8]) -> Option<String> {
    match name {
        "utf-8" => String::from_utf8(bytes.to_vec()).ok(),
        "ascii" => bytes
            .is_ascii()
            .then(|| String::from_utf8_lossy(bytes).into_owned()),
        _ => {
            let table = SINGLE_BYTE.iter().find(|(n, _)| *n == name)?.1;
            bytes
                .iter()
                .map(|&b| match table[b as usize] {
                    0xFFFD => None,
                    c => char::from_u32(u32::from(c)),
                })
                .collect()
        }
    }
}

fn utf16(raw: &[u8], big_endian: bool) -> String {
    let (units, odd) = (raw.len() / 2, raw.len() % 2 == 1);
    let words: Vec<u16> = (0..units)
        .map(|i| {
            let pair = [raw[2 * i], raw[2 * i + 1]];
            if big_endian {
                u16::from_be_bytes(pair)
            } else {
                u16::from_le_bytes(pair)
            }
        })
        .collect();
    let mut out: String = char::decode_utf16(words.iter().copied())
        .map(|r| r.unwrap_or('\u{fffd}'))
        .collect();
    // A dangling high surrogate and a trailing odd byte are one truncation error.
    let dangling = words.last().is_some_and(|w| (0xD800..0xDC00).contains(w));
    if odd && !dangling {
        out.push('\u{fffd}');
    }
    out
}

fn utf32(raw: &[u8], big_endian: bool) -> String {
    let mut out = String::new();
    for chunk in raw.chunks(4) {
        if chunk.len() < 4 {
            out.push('\u{fffd}');
            break;
        }
        let v = [chunk[0], chunk[1], chunk[2], chunk[3]];
        let n = if big_endian {
            u32::from_be_bytes(v)
        } else {
            u32::from_le_bytes(v)
        };
        out.push(char::from_u32(n).unwrap_or('\u{fffd}'));
    }
    out
}

fn multibyte(name: &str) -> Option<&'static encoding_rs::Encoding> {
    Some(match name {
        "big5" | "big5hkscs" | "cp950" => encoding_rs::BIG5,
        "cp932" | "shift_jis" => encoding_rs::SHIFT_JIS,
        "euc_jp" => encoding_rs::EUC_JP,
        "euc_kr" | "cp949" => encoding_rs::EUC_KR,
        "gbk" | "gb2312" => encoding_rs::GBK,
        "gb18030" => encoding_rs::GB18030,
        _ => return None,
    })
}

/// `raw.decode(name, errors="replace")`.
pub(crate) fn decode(name: &str, raw: &[u8]) -> Result<String, StoreError> {
    if let Some((_, table)) = SINGLE_BYTE.iter().find(|(n, _)| *n == name) {
        return Ok(raw
            .iter()
            .map(|&b| char::from_u32(u32::from(table[b as usize])).unwrap_or('\u{fffd}'))
            .collect());
    }
    let text = match name {
        "utf-8" => String::from_utf8_lossy(raw).into_owned(),
        "utf-8-sig" => {
            String::from_utf8_lossy(raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(raw)).into_owned()
        }
        "utf-16" => match raw {
            [0xff, 0xfe, rest @ ..] => utf16(rest, false),
            [0xfe, 0xff, rest @ ..] => utf16(rest, true),
            _ => utf16(raw, false),
        },
        "utf-16-le" => utf16(raw, false),
        "utf-16-be" => utf16(raw, true),
        "utf-32" => match raw {
            [0xff, 0xfe, 0, 0, rest @ ..] => utf32(rest, false),
            [0, 0, 0xfe, 0xff, rest @ ..] => utf32(rest, true),
            _ => utf32(raw, false),
        },
        "utf-32-le" => utf32(raw, false),
        "utf-32-be" => utf32(raw, true),
        _ => match multibyte(name) {
            Some(enc) => enc.decode_without_bom_handling(raw).0.into_owned(),
            None => {
                return Err(StoreError::Invalid(format!(
                    "charset {name} is not supported by the Rust decoder"
                )))
            }
        },
    };
    Ok(text)
}
