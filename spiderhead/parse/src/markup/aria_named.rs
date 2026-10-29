//! Static accessibility evidence: aria-label naming, accessible text samples and body-level issues.

use super::*;

/// Whether `tag` has an accessible name through `aria-label`/`aria-labelledby` (ids map from
/// `id` to the elements carrying it).
pub(crate) fn aria_named<'a>(
    tag: &ElementRef<'a>,
    ids: &HashMap<String, Vec<ElementRef<'a>>>,
) -> bool {
    !py_strip(attr(tag, "aria-label")).is_empty()
        || attr(tag, "aria-labelledby")
            .split(crate::py_str::is_py_space)
            .filter(|s| !s.is_empty())
            .any(|reference| {
                ids.get(reference)
                    .is_some_and(|found| found.len() == 1 && !name_text(&found[0]).is_empty())
            })
}

pub(super) fn sample(tag: &ElementRef<'_>) -> Map<String, Json> {
    let mut map = Map::new();
    map.insert("tag".into(), json!(tag.value().name()));
    map.insert("id".into(), json!(char_prefix(attr(tag, "id"), 200)));
    map.insert("name".into(), json!(char_prefix(attr(tag, "name"), 200)));
    map.insert("line".into(), Json::Null);
    map
}

pub(super) fn with(mut base: Map<String, Json>, extra: &[(&str, Json)]) -> Json {
    for (key, value) in extra {
        base.insert((*key).to_owned(), value.clone());
    }
    Json::Object(base)
}

/// `tag.get_text(" ", strip=True)`.
pub(super) fn stripped_text(tag: &ElementRef<'_>) -> String {
    let mut parts: Vec<String> = Vec::new();
    for node in tag.descendants() {
        if let scraper::Node::Text(text) = node.value() {
            let owner = node
                .ancestors()
                .filter_map(ElementRef::wrap)
                .find_map(|a| crate::dom::container_kind(a.value().name()));
            if owner == crate::dom::container_kind(tag.value().name()) && !py_strip(text).is_empty()
            {
                parts.push(py_strip(text).to_owned());
            }
        }
    }
    parts.join(" ")
}

#[derive(Default)]
pub(super) struct Issues {
    order: Vec<(String, usize, Vec<Json>)>,
}

impl Issues {
    pub(super) fn add(&mut self, code: &str, sample: Json) {
        if !self.order.iter().any(|(c, _, _)| c == code) {
            self.order.push((code.to_owned(), 0, Vec::new()));
        }
        if let Some((_, count, samples)) = self.order.iter_mut().find(|(c, _, _)| c == code) {
            *count += 1;
            if samples.len() < SAMPLE_LIMIT {
                samples.push(sample);
            }
        }
    }
}

pub(super) fn accessibility_facts(soup: &Soup) -> AccessibilityFacts {
    let mut issues = Issues::default();
    let root = find(soup, "html");
    if root.is_none_or(|r| py_strip(attr(&r, "lang")).is_empty()) {
        issues.add(
            "A11Y_HTML_LANG_MISSING",
            json!({"tag": "html", "lang": root.and_then(|r| r.value().attr("lang"))}),
        );
    }
    if let Some(body) = find(soup, "body") {
        body_issues(soup, &body, &mut issues);
    }
    let mut out = Vec::new();
    for code in A11Y_CODES {
        if let Some((_, count, samples)) = issues.order.iter().find(|(c, _, _)| c == code) {
            out.push(json!({"code": code, "count": count, "samples": samples,
                            "samples_truncated": *count > samples.len()}));
        }
    }
    AccessibilityFacts {
        method: "static_html".to_owned(),
        issues: out,
        sample_limit: SAMPLE_LIMIT as u64,
        unavailable: vec![
            "contrast requires computed styles".to_owned(),
            "axe-core rendered DOM rules not collected".to_owned(),
        ],
    }
}

pub(super) fn body_issues<'a>(soup: &'a Soup, body: &ElementRef<'a>, issues: &mut Issues) {
    let mut ids: HashMap<String, Vec<ElementRef<'a>>> = HashMap::new();
    let mut labels: HashMap<String, Vec<ElementRef<'a>>> = HashMap::new();
    for tag in all_elements(soup) {
        if self_and_ancestors(&tag).any(|n| INERT_TAGS.contains(&n.value().name())) {
            continue;
        }
        let id = attr(&tag, "id");
        if !id.is_empty() {
            ids.entry(id.to_owned()).or_default().push(tag);
        }
        let target = attr(&tag, "for");
        if tag.value().name() == "label" && !target.is_empty() {
            labels.entry(target.to_owned()).or_default().push(tag);
        }
    }
    let mut previous: Option<u8> = None;
    let mut meaningful = false;
    let mut has_main = false;
    for tag in body.descendants().skip(1).filter_map(ElementRef::wrap) {
        if excluded(&tag) {
            continue;
        }
        let name = tag.value().name();
        meaningful |= matches!(
            name,
            "img" | "amp-img" | "input" | "select" | "textarea" | "video" | "audio"
        );
        let roles: Vec<String> = attr(&tag, "role")
            .to_lowercase()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        has_main |= name == "main" || roles.iter().any(|r| r == "main");
        if matches!(name, "img" | "amp-img")
            && tag.value().attr("alt").is_none()
            && !aria_named(&tag, &ids)
            && !roles.iter().any(|r| r == "presentation" || r == "none")
        {
            issues.add(
                "A11Y_IMAGE_ALT_MISSING",
                with(
                    sample(&tag),
                    &[("src", json!(char_prefix(attr(&tag, "src"), 300)))],
                ),
            );
        }
        if matches!(name, "input" | "select" | "textarea") {
            let control_type = tag.value().attr("type").unwrap_or("text").to_lowercase();
            if name == "input"
                && matches!(
                    control_type.as_str(),
                    "hidden" | "button" | "submit" | "reset" | "image"
                )
            {
                continue;
            }
            let identifier = attr(&tag, "id");
            let references: Vec<&str> = attr(&tag, "aria-labelledby")
                .split(crate::py_str::is_py_space)
                .filter(|s| !s.is_empty())
                .collect();
            let mut named = aria_named(&tag, &ids) || !py_strip(attr(&tag, "title")).is_empty();
            named |= !identifier.is_empty()
                && ids
                    .get(identifier)
                    .is_some_and(|f| f.len() == 1 && f[0] == tag)
                && labels
                    .get(identifier)
                    .is_some_and(|ls| ls.iter().any(|l| !name_text(l).is_empty()));
            let wrapper = tag
                .ancestors()
                .filter_map(ElementRef::wrap)
                .find(|a| a.value().name() == "label");
            named |= wrapper
                .is_some_and(|w| w.value().attr("for").is_none() && !name_text(&w).is_empty());
            if !named {
                issues.add(
                    "A11Y_FORM_LABEL_MISSING",
                    with(
                        sample(&tag),
                        &[
                            ("type", json!(control_type)),
                            (
                                "aria_labelledby",
                                json!(char_prefix(&references.join(" "), 300)),
                            ),
                        ],
                    ),
                );
            }
        }
        if let [b'h', digit @ b'1'..=b'6'] = name.as_bytes() {
            let level = digit - b'0';
            if previous.is_some_and(|p| level > p + 1) {
                issues.add(
                    "A11Y_HEADING_ORDER",
                    with(
                        sample(&tag),
                        &[
                            ("previous_level", json!(previous)),
                            ("level", json!(level)),
                            ("text", json!(char_prefix(&stripped_text(&tag), 200))),
                        ],
                    ),
                );
            }
            previous = Some(level);
        }
    }
    meaningful |= body.descendants().any(|node| {
        // A doctype counts as text.
        let text: &str = match node.value() {
            scraper::Node::Text(text) => text,
            scraper::Node::Doctype(doctype) => doctype.name(),
            _ => return false,
        };
        !py_strip(text).is_empty()
            && node
                .parent()
                .and_then(ElementRef::wrap)
                .is_some_and(|p| !excluded(&p))
    });
    if meaningful && !has_main {
        issues.add(
            "A11Y_MAIN_LANDMARK_MISSING",
            json!({"tag": "body", "meaningful_content": true}),
        );
    }
}

/// Bounded evidence with full issue counts; never asserts full validity.
pub fn markup_facts(html: &str, soup: &Soup) -> Result<MarkupFacts> {
    Ok(MarkupFacts {
        amp: amp_facts(html),
        accessibility: accessibility_facts(soup),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::parse_document;

    /// `line` counts '\n' before the tag plus one; `column` counts characters since the
    /// last newline. Samples keep those values when a check reports the tag.
    #[test]
    pub(super) fn head_record_positions_follow_python_offsets() {
        // Line: newlines before the tag, plus one. Column: characters since the last newline.
        let html = "<!doctype html>\n<html amp>\n<head>\n<meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width\">\n\
<script async src=\"https://cdn.ampproject.org/v0.js\"></script>\n\
<style amp-boilerplate>body{}</style>висим\n</head><body></body></html>";
        let soup = parse_document(html);
        let facts = markup_facts(html, &soup).expect("markup_facts");
        let boilerplate = facts.amp["errors"]
            .as_array()
            .expect("amp errors")
            .iter()
            .find(|e| e["code"] == "AMP_INVALID_BOILERPLATE")
            .expect("missing noscript boilerplate is reported");
        let sample = &boilerplate["observed"]["samples"][0];
        // The <style> tag sits on the sixth source line right after a newline.
        assert_eq!(sample["line"], 6);
        assert_eq!(sample["column"], 0);
    }
}
