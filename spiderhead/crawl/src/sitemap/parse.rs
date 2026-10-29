//! Pure document API: classify and parse one sitemap, plus decompression.

use super::urls::*;
use super::xml::*;
use serde_json::{json, Map, Value};
use spiderhead_parse::py_url;

/// Parses a priority value like `float()`; `None` when it is not a number.
pub(super) fn parse_priority(value: Option<String>) -> Value {
    let Some(value) = value else {
        return Value::Null;
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Value::Null;
    }
    let transformed: String = trimmed
        .chars()
        .map(|ch| match unicode_decimal_digit(ch) {
            Some(digit) => char::from(digit + b'0'),
            None if ch.is_whitespace() => ' ',
            None => ch,
        })
        .collect();
    match python_float(transformed.trim()) {
        Some(number) if number.is_nan() => Value::String("NaN".to_owned()),
        Some(number) if number.is_infinite() => Value::String(if number > 0.0 {
            "Infinity".to_owned()
        } else {
            "-Infinity".to_owned()
        }),
        Some(number) => json!(number),
        None => Value::Null,
    }
}

/// Unicode decimal-digit value of a character: each Nd block runs ten digits from its zero.
pub(super) fn unicode_decimal_digit(ch: char) -> Option<u8> {
    if let Some(digit) = ch.to_digit(10) {
        return Some(digit as u8);
    }
    const BLOCKS: [u32; 44] = [
        0x0660, 0x06F0, 0x07C0, 0x0966, 0x09E6, 0x0A66, 0x0AE6, 0x0B66, 0x0BE6, 0x0C66, 0x0CE6,
        0x0D66, 0x0DE6, 0x0E50, 0x0ED0, 0x0F20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0,
        0x1A80, 0x1A90, 0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0,
        0xAA50, 0xABF0, 0xFF10, 0x104A0, 0x1D7CE, 0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6, 0x10D30,
        0x11066,
    ];
    let code = ch as u32;
    BLOCKS.iter().find_map(|base| {
        let offset = code.checked_sub(*base)?;
        (offset < 10).then_some(offset as u8)
    })
}

/// Digits may be separated by single underscores, as in numeric literals.
pub(super) fn python_float(text: &str) -> Option<f64> {
    if let Ok(number) = text.parse::<f64>() {
        return Some(number);
    }
    let mut cleaned = String::with_capacity(text.len());
    let bytes: Vec<char> = text.chars().collect();
    for (index, ch) in bytes.iter().enumerate() {
        if *ch == '_' {
            let before = index > 0 && bytes[index - 1].is_ascii_hexdigit();
            let after = bytes.get(index + 1).is_some_and(|c| c.is_ascii_hexdigit());
            if !(before && after) {
                return None;
            }
            continue;
        }
        cleaned.push(*ch);
    }
    cleaned.parse::<f64>().ok()
}

/// XML 1.0 character validity: the range the recovering parser drops silently.
pub(super) fn is_valid_xml_char(ch: char) -> bool {
    let code = ch as u32;
    matches!(ch, '\t' | '\n' | '\r')
        || (0x20..=0xD7FF).contains(&code)
        || (0xE000..=0xFFFD).contains(&code)
        || (0x10000..=0x10FFFF).contains(&code)
}

/// Image, news, video and alternate-language entries of one `<url>`.
pub(super) fn extension_records(url_el: &Element, base_url: &str) -> Map<String, Value> {
    const IMAGE_FIELDS: [&str; 5] = ["loc", "title", "caption", "geo_location", "license"];
    const NEWS_FIELDS: [&str; 3] = ["name", "publication_date", "title"];
    const VIDEO_FIELDS: [&str; 16] = [
        "thumbnail_loc",
        "title",
        "description",
        "player_loc",
        "duration",
        "expiration_date",
        "rating",
        "view_count",
        "publication_date",
        "family_friendly",
        "requires_subscription",
        "live",
        "tag",
        "category",
        "gallery_loc",
        "uploader",
    ];
    let text_fields = |element: &Element, names: &[&str]| -> Map<String, Value> {
        let mut record = Map::new();
        for name in names {
            if let Some(value) = element.child_text(name) {
                if !value.is_empty() {
                    record.insert((*name).to_owned(), json!(value));
                }
            }
        }
        record
    };
    let mut images: Vec<Value> = Vec::new();
    let mut news: Option<Map<String, Value>> = None;
    let mut videos: Vec<Value> = Vec::new();
    let mut alternates: Vec<Value> = Vec::new();
    for child in &url_el.children {
        match child.name.as_str() {
            "image" => {
                let mut record = text_fields(child, &IMAGE_FIELDS);
                if let Some(loc) = record.get("loc").and_then(Value::as_str).map(str::to_owned) {
                    let joined = py_url::urljoin(base_url, &loc).unwrap_or_else(|_| loc.clone());
                    record.insert("loc".into(), json!(joined));
                    images.push(Value::Object(record));
                }
            }
            "news" => {
                let mut record = text_fields(child, &NEWS_FIELDS);
                if !record.contains_key("name") {
                    for sub in &child.children {
                        if sub.name == "publication" {
                            if let Some(name_text) = sub.child_text("name") {
                                if !name_text.is_empty() {
                                    record.insert("name".into(), json!(name_text));
                                }
                            }
                            break;
                        }
                    }
                }
                if !record.is_empty() {
                    news = Some(record);
                }
            }
            "video" => {
                let record = text_fields(child, &VIDEO_FIELDS);
                if !record.is_empty() {
                    videos.push(Value::Object(record));
                }
            }
            "link" => {
                let rel = child.attr("rel").unwrap_or("").trim().to_lowercase();
                let hreflang = child.attr("hreflang").unwrap_or("").trim().to_owned();
                let href = child.attr("href").unwrap_or("").trim().to_owned();
                if rel == "alternate" && !hreflang.is_empty() && !href.is_empty() {
                    let joined = py_url::urljoin(base_url, &href).unwrap_or_else(|_| href.clone());
                    alternates.push(json!({"hreflang": hreflang, "href": joined}));
                }
            }
            _ => {}
        }
    }
    let mut out = Map::new();
    if !images.is_empty() {
        out.insert("images".into(), Value::Array(images));
    }
    if let Some(news) = news {
        out.insert("news".into(), Value::Object(news));
    }
    if !videos.is_empty() {
        out.insert("videos".into(), Value::Array(videos));
    }
    if !alternates.is_empty() {
        out.insert("alternates".into(), Value::Array(alternates));
    }
    out
}

/// Text sitemap: one URL per line, `#` lines are comments, BOM and UTF-16 input accepted.
pub fn parse_text_sitemap(text_bytes: &[u8], base_url: &str) -> Value {
    let text = if text_bytes.starts_with(&[0xFF, 0xFE]) {
        decode_utf16(text_bytes, false)
    } else if text_bytes.starts_with(&[0xFE, 0xFF]) {
        decode_utf16(text_bytes, true)
    } else {
        let body = text_bytes
            .strip_prefix(&[0xEF, 0xBB, 0xBF][..])
            .unwrap_or(text_bytes);
        String::from_utf8_lossy(body).into_owned()
    };
    let mut urls: Vec<Value> = Vec::new();
    for raw in text.split('\n') {
        let line = raw.trim_end_matches('\r').trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Ok(absolute) = py_url::urljoin(base_url, line) else {
            continue;
        };
        if !absolute.starts_with("http://") && !absolute.starts_with("https://") {
            continue;
        }
        urls.push(json!({
            "loc": absolute,
            "lastmod": Value::Null,
            "changefreq": Value::Null,
            "priority": Value::Null,
        }));
    }
    json!({"ok": true, "type": "text", "sitemaps": [], "urls": urls})
}

/// Classifies and extracts one document. Failures are reported in the returned value, not raised.
pub fn parse_sitemap(xml_bytes: &[u8], base_url: &str) -> Value {
    // Plain-text sitemap: one URL per line. A body without '<' is not XML.
    let head: Vec<u8> = xml_bytes
        .iter()
        .copied()
        .skip_while(|b| b.is_ascii_whitespace())
        .take(512)
        .collect();
    if !head.is_empty() && !head.contains(&b'<') {
        return parse_text_sitemap(xml_bytes, base_url);
    }

    let text = decode_bytes(xml_bytes);
    let root = match parse_tree(&text) {
        Parsed::Root(root) => root,
        Parsed::Empty => {
            return json!({
                "ok": false,
                "error": "XML parse error: empty document",
                "type": "unknown",
                "sitemaps": [],
                "urls": [],
            });
        }
        Parsed::NoDocument => {
            if !xml_bytes.get(..512).unwrap_or(xml_bytes).contains(&b'<') {
                return parse_text_sitemap(xml_bytes, base_url);
            }
            return json!({
                "ok": false,
                "error": "XML parse error: Document is empty, line 1, column 1 (<string>, line 1)",
                "type": "unknown",
                "sitemaps": [],
                "urls": [],
            });
        }
    };

    match root.name.as_str() {
        "sitemapindex" => {
            let mut sitemaps: Vec<Value> = Vec::new();
            for child in &root.children {
                if child.name != "sitemap" {
                    continue;
                }
                let Some(loc) = child.child_text("loc").filter(|loc| !loc.is_empty()) else {
                    continue;
                };
                let joined = py_url::urljoin(base_url, &loc).unwrap_or_else(|_| loc.clone());
                sitemaps.push(json!({
                    "loc": joined,
                    "lastmod": child.child_text("lastmod"),
                }));
            }
            json!({"ok": true, "type": "index", "sitemaps": sitemaps, "urls": []})
        }
        "urlset" => {
            let mut urls: Vec<Value> = Vec::new();
            for child in &root.children {
                if child.name != "url" {
                    continue;
                }
                let Some(loc) = child.child_text("loc").filter(|loc| !loc.is_empty()) else {
                    continue;
                };
                let joined = py_url::urljoin(base_url, &loc).unwrap_or_else(|_| loc.clone());
                let mut entry = json!({
                    "loc": joined,
                    "lastmod": child.child_text("lastmod"),
                    "changefreq": child.child_text("changefreq"),
                    "priority": parse_priority(child.child_text("priority")),
                });
                let extensions = extension_records(child, base_url);
                if let Some(object) = entry.as_object_mut() {
                    for (key, value) in extensions {
                        object.insert(key, value);
                    }
                }
                urls.push(entry);
            }
            json!({"ok": true, "type": "urlset", "sitemaps": [], "urls": urls})
        }
        _ => json!({"ok": true, "type": "unknown", "sitemaps": [], "urls": []}),
    }
}

/// Decompresses a `.gz` payload or any body with the gzip magic header, in bounded chunks.
/// Aborts past `MAX_XML_BYTES`; bytes that are not gzip are returned unchanged.
pub fn maybe_gunzip(url: &str, body: &[u8]) -> Vec<u8> {
    const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
    const CHUNK: usize = 64 * 1024;
    let path = py_url::urlsplit(url)
        .map(|parts| parts.path.to_lowercase())
        .unwrap_or_default();
    let looks_gzip = body.starts_with(&GZIP_MAGIC);
    if !(path.ends_with(".gz") || looks_gzip) {
        return body.to_vec();
    }
    let mut out = Vec::new();
    let mut decoder = flate2::read::MultiGzDecoder::new(body);
    let mut chunk = [0u8; CHUNK];
    loop {
        match std::io::Read::read(&mut decoder, &mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                out.extend_from_slice(&chunk[..read]);
                if out.len() > MAX_XML_BYTES {
                    // The bounded read aborts with the shared too-large message; the fetch records it as the error.
                    return out;
                }
            }
            Err(_) => {
                // Truncated or non-gzip data is returned as raw bytes.
                return body.to_vec();
            }
        }
    }
    out
}
