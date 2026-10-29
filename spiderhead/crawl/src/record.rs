//! `PageRecord` and the projection from a parsed page into a record.
//! Field names are the scan's stored keys; the writer checks this exact key set.

use serde_json::{json, Map, Value};
use spiderhead_parse::models::ParsedPage;

/// `uses_ajax_crawling_scheme`: a `#!` fragment or an `?_escaped_fragment_=` query.
pub fn uses_ajax_crawling_scheme(href: &str) -> bool {
    href.contains("#!") || href.contains("_escaped_fragment_=")
}

fn text_of(value: Option<&str>) -> String {
    value.unwrap_or("").to_string()
}

fn first_heading(parsed: &ParsedPage, level: &str, index: usize) -> String {
    parsed
        .headings
        .get(level)
        .and_then(|items| items.get(index))
        .cloned()
        .unwrap_or_default()
}

/// Matches script and style blocks, including self-closing ones, and JSON-LD / application/json blocks.
/// Uses fancy-regex for its backreference support, which the plain `regex` crate lacks.
fn script_style_blocks_re() -> fancy_regex::Regex {
    fancy_regex::Regex::new(r"(?is)<(script|style)\b[^>]*/>|<(script|style)\b[^>]*>.*?</\2\s*>")
        .expect("static regex")
}

/// Length of the HTML with script and style blocks removed.
/// `text_ratio` divides visible text by this, so the measurement must match exactly.
pub fn markup_only_bytes(html: &str) -> usize {
    let re = script_style_blocks_re();
    let stripped = re.replace_all(html, "");
    stripped.len()
}

/// Body-derived fields, built the same way for every parsed page.
pub fn from_parsed(parsed: &ParsedPage) -> Map<String, Value> {
    let og = &parsed.og;
    let links = &parsed.links;
    let position = &parsed.position;
    let framed: Vec<_> = parsed.frames.iter().filter(|f| f.in_content_area).collect();
    let images_with_alt: Vec<_> = parsed.images.iter().filter(|i| i.has_alt).collect();
    let readability = parsed.readability.clone();
    let mut record = Map::new();
    record.insert(
        "title".into(),
        json!(parsed.title.clone().unwrap_or_default()),
    );
    record.insert(
        "meta_description".into(),
        json!(parsed.meta_description.clone().unwrap_or_default()),
    );
    record.insert(
        "meta_description_count".into(),
        json!(parsed.meta_description_count),
    );
    record.insert("h1".into(), json!(first_heading(parsed, "h1", 0)));
    record.insert("h1_2".into(), json!(first_heading(parsed, "h1", 1)));
    record.insert(
        "h1_alt_text".into(),
        json!(parsed.h1_alt_only_text.clone().unwrap_or_default()),
    );
    record.insert("h2".into(), json!(first_heading(parsed, "h2", 0)));
    record.insert(
        "canonical".into(),
        json!(parsed.canonical.clone().unwrap_or_default()),
    );
    record.insert("lorem_ipsum_count".into(), json!(parsed.lorem_ipsum_count));
    record.insert("images_total".into(), json!(parsed.images.len() as u64));
    record.insert(
        "images_missing_alt_attr".into(),
        json!((parsed.images.len() - images_with_alt.len()) as u64),
    );
    record.insert(
        "images_max_alt_length".into(),
        json!(images_with_alt
            .iter()
            .map(|i| i.alt_length)
            .max()
            .unwrap_or(0)),
    );
    record.insert(
        "plugin_elements".into(),
        json!(parsed.plugin_elements_count),
    );
    record.insert(
        "mobile_alternate_broken".into(),
        json!(parsed.mobile_alternate_broken),
    );
    record.insert("meta_fragment".into(), json!(parsed.meta_fragment));
    record.insert(
        "ajax_scheme_outlinks".into(),
        json!(links
            .iter()
            .filter(|l| uses_ajax_crawling_scheme(&l.href))
            .count() as u64),
    );
    record.insert(
        "meta_robots".into(),
        json!(parsed.robots_meta_scoped.join(", ")),
    );
    record.insert(
        "og_title".into(),
        json!(text_of(og.get("og:title").map(String::as_str))),
    );
    record.insert(
        "og_description".into(),
        json!(text_of(og.get("og:description").map(String::as_str))),
    );
    record.insert(
        "og_image".into(),
        json!(text_of(og.get("og:image").map(String::as_str))),
    );
    record.insert(
        "og_url".into(),
        json!(text_of(og.get("og:url").map(String::as_str))),
    );
    record.insert("word_count".into(), json!(parsed.word_count));
    record.insert("content_frames".into(), json!(framed.len() as u64));
    record.insert(
        "content_frames_same_origin".into(),
        json!(framed.iter().filter(|f| f.same_origin).count() as u64),
    );
    record.insert("outlinks".into(), json!(links.len() as u64));
    record.insert(
        "external_outlinks".into(),
        json!(links.iter().filter(|l| l.external).count() as u64),
    );
    record.insert(
        "charset".into(),
        json!(parsed.charset.clone().unwrap_or_default()),
    );
    record.insert(
        "doctype".into(),
        json!(parsed.doctype.clone().unwrap_or_default()),
    );
    record.insert(
        "viewport".into(),
        json!(parsed.viewport.clone().unwrap_or_default()),
    );
    record.insert("meta_refresh".into(), json!(parsed.meta_refresh));
    record.insert(
        "title_outside_head".into(),
        opt_bool(position.title_outside_head),
    );
    record.insert(
        "meta_description_outside_head".into(),
        opt_bool(position.meta_description_outside_head),
    );
    record.insert(
        "canonical_outside_head".into(),
        opt_bool(position.canonical_outside_head),
    );
    record.insert(
        "directives_outside_head".into(),
        opt_bool(position.directives_outside_head),
    );
    record.insert(
        "hreflang_outside_head".into(),
        opt_bool(position.hreflang_outside_head),
    );
    record.insert(
        "hreflang".into(),
        json!(parsed
            .hreflang
            .iter()
            .map(|h| json!({"lang": h.lang, "raw_href": h.raw_href, "url": h.url}))
            .collect::<Vec<Value>>()),
    );
    record.insert("amphtml".into(), json!(parsed.amphtml));
    record.insert(
        "heading_outline".into(),
        json!(parsed
            .heading_outline
            .iter()
            .map(|h| json!({"level": h.level, "text": h.text, "region": h.region}))
            .collect::<Vec<Value>>()),
    );
    record.insert(
        "sentence_count".into(),
        opt_u64(readability.as_ref().map(|r| r.sentence_count)),
    );
    record.insert(
        "avg_words_per_sentence".into(),
        readability
            .as_ref()
            .map(|r| json!(r.avg_words_per_sentence))
            .unwrap_or(Value::Null),
    );
    record.insert(
        "flesch".into(),
        readability
            .as_ref()
            .map(|r| json!(r.flesch))
            .unwrap_or(Value::Null),
    );
    record.insert(
        "readability".into(),
        json!(readability
            .as_ref()
            .map(|r| r.readability.clone())
            .unwrap_or_default()),
    );
    record.insert(
        "link_placement".into(),
        parsed
            .link_placement
            .clone()
            .map(|p| serde_json::to_value(&p).unwrap_or(Value::Null))
            .unwrap_or(
                serde_json::to_value(spiderhead_parse::models::LinkPlacement::default())
                    .unwrap_or(Value::Null),
            ),
    );
    record.insert(
        "trust_signals".into(),
        serde_json::to_value(&parsed.trust_signals).unwrap_or(Value::Null),
    );
    record.insert(
        "duplicate_ids".into(),
        serde_json::to_value(&parsed.duplicate_ids).unwrap_or(Value::Null),
    );
    record.insert("head_count".into(), json!(position.head_count));
    record.insert("body_count".into(), json!(position.body_count));
    record.insert("head_not_first".into(), json!(position.head_not_first));
    record.insert(
        "invalid_head_elements".into(),
        json!(position.invalid_head_elements.join(", ")),
    );
    record
}

fn opt_bool(value: Option<bool>) -> Value {
    match value {
        Some(flag) => json!(flag),
        None => Value::Null,
    }
}

fn opt_u64(value: Option<u64>) -> Value {
    match value {
        Some(number) => json!(number),
        None => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ajax_scheme_counts_hash_bang_and_escaped() {
        assert!(uses_ajax_crawling_scheme("http://x/#!page"));
        assert!(uses_ajax_crawling_scheme("http://x/?_escaped_fragment_=p"));
        assert!(!uses_ajax_crawling_scheme("http://x/page"));
    }

    #[test]
    fn markup_only_bytes_strips_script_and_style() {
        let html = "<html><head><style>a{}</style></head><body>hi<script>1</script></body></html>";
        let stripped = markup_only_bytes(html);
        assert!(stripped < html.len());
        assert_eq!(stripped, "<html><head></head><body>hi</body></html>".len());
    }
}
