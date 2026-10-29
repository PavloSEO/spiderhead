//! Text, URL-source, resource and form extraction from a parsed document, with srcset and sandbox helpers.

use super::*;

/// Collect `<form>` elements; returns `(forms, total_seen)`.
pub fn extract_form_observations(
    soup: &Soup,
    base_url: &str,
    final_url: &str,
    cap: Option<usize>,
) -> (Vec<FormInfo>, usize) {
    let mut forms = Vec::new();
    let mut total = 0;
    for tag in find_all(soup, "form") {
        if is_inert_template_content(&tag) {
            continue;
        }
        total += 1;
        if cap.is_none_or(|cap| forms.len() < cap) {
            forms.push(form_info(&tag, base_url, final_url));
        }
    }
    (forms, total)
}

pub(super) fn form_info(tag: &ElementRef<'_>, base_url: &str, final_url: &str) -> FormInfo {
    let method = match py_strip(attr(tag, "method").unwrap_or("")) {
        "" => "get".to_owned(),
        m => m.to_lowercase(),
    };
    let action_raw = py_strip(attr(tag, "action").unwrap_or(""));
    let action = if action_raw.is_empty() {
        final_url.to_owned()
    } else {
        urljoin(base_url, action_raw).unwrap_or_else(|_| final_url.to_owned())
    };
    let has_password = find_in(tag, "input")
        .any(|input| attr(&input, "type").is_some_and(|t| t.to_lowercase() == "password"));
    FormInfo {
        method,
        action,
        has_password,
    }
}

/// Collapsed visible body text minus `TEXT_EXCLUDED_TAGS`.
pub fn extract_text(soup: &Soup) -> String {
    let mut parts = Vec::new();
    match find(soup, "body") {
        Some(body) => parts.extend(crate::dom::visible_strings(*body, &TEXT_EXCLUDED_TAGS)),
        None => {
            for child in soup.tree.root().children() {
                parts.extend(crate::dom::visible_strings(child, &TEXT_EXCLUDED_TAGS));
            }
        }
    }
    collapse_whitespace(&parts.join(" "))
}

/// Extract one URL per srcset entry, discarding density/width descriptors.
pub fn split_srcset(value: &str) -> Vec<String> {
    // `\s*,\s*(?=(?:[^']*$))`: split at commas with no apostrophe anywhere after them.
    // The last apostrophe's position decides that for every comma in one pass.
    let last_quote = value.rfind('\'');
    let mut urls = Vec::new();
    let mut start = 0;
    let mut pieces: Vec<&str> = Vec::new();
    for (at, c) in value.char_indices() {
        if c == ',' && last_quote.is_none_or(|q| q <= at) {
            pieces.push(&value[start..at]);
            start = at + 1;
        }
    }
    pieces.push(&value[start..]);
    for entry in pieces {
        let entry = py_strip(entry);
        if let Some(first) = entry
            .split(crate::py_str::is_py_space)
            .next()
            .filter(|s| !s.is_empty())
        {
            urls.push(first.to_owned());
        }
    }
    urls
}

/// URLs referenced from CSS text, in source order, duplicates kept.
pub fn extract_css_urls(css_text: Option<&str>) -> Vec<String> {
    CSS_URL
        .captures_iter(css_text.unwrap_or(""))
        .flatten()
        .filter_map(|c| c.get(2).map(|m| py_strip(m.as_str()).to_owned()))
        .collect()
}

/// URL carriers beyond `a[href]`: media, forms, citations, ping, meta-refresh and itemtype.
pub fn extract_url_sources(soup: &Soup, base_url: &str) -> Vec<BTreeMap<String, String>> {
    let mut out: Vec<BTreeMap<String, String>> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut push = |raw_url: &str, tag_name: &str, attr_name: &str| {
        let raw_url = py_strip(raw_url);
        if raw_url.is_empty() || raw_url.starts_with('#') {
            return;
        }
        let low = raw_url.to_lowercase();
        if ["data:", "javascript:", "mailto:", "tel:"]
            .iter()
            .any(|p| low.starts_with(p))
        {
            return;
        }
        let Ok(absolute) = urljoin(base_url, raw_url) else {
            return;
        };
        if seen.insert(absolute.clone()) {
            out.push(BTreeMap::from([
                ("url".to_owned(), absolute),
                ("tag".to_owned(), tag_name.to_owned()),
                ("attr".to_owned(), attr_name.to_owned()),
            ]));
        }
    };
    for (tag_name, attrs) in URL_SOURCE_ATTRS {
        for tag in find_all(soup, tag_name) {
            if is_inert_template_content(&tag) {
                continue;
            }
            for attr_name in attrs {
                let Some(value) = attr(&tag, attr_name).filter(|v| !v.is_empty()) else {
                    continue;
                };
                match *attr_name {
                    "srcset" => {
                        for sub in split_srcset(value) {
                            push(&sub, tag_name, attr_name);
                        }
                    }
                    "ping" => {
                        for sub in value
                            .split(crate::py_str::is_py_space)
                            .filter(|s| !s.is_empty())
                        {
                            push(sub, tag_name, attr_name);
                        }
                    }
                    _ => push(value, tag_name, attr_name),
                }
            }
        }
    }
    for tag in all_elements(soup) {
        if let Some(style) = attr(&tag, "style") {
            if !is_inert_template_content(&tag) {
                for url in extract_css_urls(Some(style)) {
                    push(&url, tag.value().name(), "style");
                }
            }
        }
    }
    for style_tag in find_all(soup, "style") {
        if !is_inert_template_content(&style_tag) {
            for url in extract_css_urls(Some(&get_text(&style_tag, ""))) {
                push(&url, "style", "css");
            }
        }
    }
    for meta in find_all(soup, "meta") {
        if is_inert_template_content(&meta) {
            continue;
        }
        if py_strip(attr(&meta, "http-equiv").unwrap_or("")).to_lowercase() == "refresh" {
            let content = attr(&meta, "content").unwrap_or("");
            if let Ok(Some(found)) = META_REFRESH_URL.captures(content) {
                if let Some(url) = found.get(1) {
                    push(url.as_str(), "meta", "refresh");
                }
            }
        }
    }
    for tag in all_elements(soup) {
        if let Some(value) = attr(&tag, "itemtype") {
            if !is_inert_template_content(&tag) {
                push(value, tag.value().name(), "itemtype");
            }
        }
    }
    out
}

/// Direct script/stylesheet occurrences and the bounded omission count.
pub fn extract_resource_declarations(
    soup: &Soup,
    base_url: &str,
    max_declarations: Option<usize>,
) -> (Vec<ResourceDeclaration>, usize) {
    let mut declarations = Vec::new();
    let mut omitted = 0;
    for tag in all_elements(soup) {
        let name = tag.value().name();
        if !matches!(name, "script" | "link") || is_inert_template_content(&tag) {
            continue;
        }
        let (kind, attr_name) = if name == "script" {
            (ResourceKind::Script, "src")
        } else {
            let stylesheet = attr(&tag, "rel")
                .unwrap_or("")
                .split_whitespace()
                .any(|r| r.to_lowercase() == "stylesheet");
            if !stylesheet {
                continue;
            }
            (ResourceKind::Stylesheet, "href")
        };
        let Some(raw_url) = attr(&tag, attr_name).filter(|u| !py_strip(u).is_empty()) else {
            continue;
        };
        let Ok(resolved) = urljoin(base_url, py_strip(raw_url)) else {
            continue;
        };
        if max_declarations.is_some_and(|cap| declarations.len() >= cap) {
            omitted += 1;
            continue;
        }
        declarations.push(ResourceDeclaration {
            kind,
            url: resolved,
            raw_url: raw_url.to_owned(),
        });
    }
    (declarations, omitted)
}

/// Filter the URL-source output down to image entries.
pub fn image_url_sources(
    url_sources: &[BTreeMap<String, String>],
) -> Vec<BTreeMap<String, String>> {
    url_sources
        .iter()
        .filter(|s| {
            matches!(s.get("tag").map(String::as_str), Some("img" | "source"))
                || matches!(s.get("attr").map(String::as_str), Some("style" | "css"))
        })
        .cloned()
        .collect()
}

/// Collapsed `sandbox` value. The attribute is a token list, rendered as a bracketed list such as
/// `['allow-forms', 'allow-scripts']`; an empty list counts as absent.
pub(super) fn sandbox_text(value: &str) -> String {
    let tokens: Vec<String> = value
        .split_whitespace()
        .map(crate::py_str::py_repr)
        .collect();
    if tokens.is_empty() {
        String::new()
    } else {
        format!("[{}]", tokens.join(", "))
    }
}

/// Every `<iframe>` and where it sits. `in_content_area` holds the DOM-order indexes of frames
/// inside the resolved content root, tracked by document-order index so the tree is not mutated.
pub fn extract_frames(
    soup: &Soup,
    base_url: &str,
    final_url: &str,
    in_content_area: &BTreeSet<usize>,
) -> Result<Vec<FrameInfo>> {
    let mut frames = Vec::new();
    for (index, tag) in find_all(soup, "iframe").enumerate() {
        if is_inert_template_content(&tag) {
            continue;
        }
        let raw_src = py_strip(attr(&tag, "src").unwrap_or("")).to_owned();
        let src = if raw_src.is_empty() {
            String::new()
        } else {
            urljoin(base_url, &raw_src)?
        };
        frames.push(FrameInfo {
            same_origin: src.is_empty() || !is_external(&src, final_url),
            src,
            raw_src,
            in_content_area: in_content_area.contains(&index),
            title: collapse_whitespace(attr(&tag, "title").unwrap_or("")),
            loading: py_strip(attr(&tag, "loading").unwrap_or("")).to_lowercase(),
            sandbox: sandbox_text(attr(&tag, "sandbox").unwrap_or("")),
        });
    }
    Ok(frames)
}
