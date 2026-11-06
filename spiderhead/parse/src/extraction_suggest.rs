//! Suggest bounded extraction-rule selectors for one element found by its text.
//!
//! Given one sample document and the visible text of the element a user would
//! otherwise locate by hand, find the matching element(s) and propose candidate
//! selectors in stability order — ids and semantic attributes first, then the
//! shortest class-signature path anchored at a stable ancestor, then a
//! positional `nth-of-type` fallback only when no unique candidate exists.
//! Every candidate's match count is measured against the whole document with
//! the exact selector engine extraction rules use, so an emitted rule evaluates
//! identically inside `extraction_rules::evaluate`.

use std::collections::HashSet;

use serde_json::{json, Map, Value};

use crate::dom::{ordered_attrs, parse_document, visible_strings, Selector, Soup};
use crate::extraction_rules::{validate_rules, MAX_MATCHES};
use crate::parser::shared::collapse_whitespace;
use scraper::ElementRef;

/// Format identifier of a suggestion document.
pub const VERSION: &str = "extraction_suggest.v1";
/// Largest accepted sample document, in bytes.
pub const MAX_HTML_BYTES: usize = 8 * 1024 * 1024;
/// Largest accepted target text.
pub const MAX_TEXT_LENGTH: usize = 2048;
/// Element count reported as considered; the note explains the rest.
pub const MAX_TARGETS: usize = 5;
/// Most candidates in one answer.
pub const MAX_CANDIDATES: usize = 12;
/// Most sample matches per candidate.
pub const MAX_SAMPLES: usize = 3;
/// Most characters of one sample match.
pub const MAX_SAMPLE_CHARS: usize = 200;
/// Longest class-signature chain tried.
pub const MAX_PATH_DEPTH: usize = 4;
/// Longest accepted selector.
pub const MAX_SELECTOR_CHARS: usize = 512;
/// Elements whose text is code or placeholder, never rendered page copy.
const SKIP_TAGS: [&str; 4] = ["script", "style", "template", "noscript"];
/// Strategy rungs in stability order.
const STRATEGY_RANK: [(&str, u8); 4] = [("id", 0), ("semantic", 1), ("path", 2), ("nth", 3)];

/// Python `str.strip()` (Unicode whitespace both ends).
fn py_strip(text: &str) -> String {
    text.trim_matches(crate::py_str::is_py_space).to_owned()
}

/// Normalize an element's text exactly like extraction rules do.
fn norm(tag: &ElementRef<'_>) -> String {
    collapse_whitespace(&visible_strings(**tag, &[]).join(" "))
}

/// Soupsieve's identifier escape (the CSS ident rules): digits escape as a hex
/// code with a trailing space when they would start the identifier, control
/// characters as hex, everything outside `[A-Za-z0-9_-]` and non-ASCII with a
/// literal backslash.
pub fn css_escape(ident: &str) -> String {
    let mut out = String::new();
    let first_is_digit = ident.chars().next().is_some_and(|c| c.is_ascii_digit());
    for (index, ch) in ident.chars().enumerate() {
        let code = ch as u32;
        match code {
            0 => out.push('\u{FFFD}'),
            0x01..=0x1F | 0x7F => {
                out.push('\\');
                out.push_str(&format!("{code:x} "));
            }
            0x30..=0x39 if index == 0 || (index == 1 && first_is_digit) => {
                out.push('\\');
                out.push_str(&format!("{code:x} "));
            }
            _ if ch == '-' || ch == '_' || ch.is_alphanumeric() => out.push(ch),
            _ => {
                out.push('\\');
                out.push(ch);
            }
        }
    }
    out
}

/// One class-signature path step: `tag.class1.class2`.
fn step(tag: &ElementRef<'_>) -> String {
    let classes = tag
        .value()
        .attr("class")
        .map(|raw| {
            raw.split(crate::py_str::is_py_space)
                .map(py_strip)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut out = tag.value().name().to_owned();
    for name in classes.iter().filter(|name| !name.is_empty()) {
        out.push('.');
        out.push_str(&css_escape(name));
    }
    out
}

/// One positional path step: `tag:nth-of-type(index)` (1-based).
fn nth_step(tag: &ElementRef<'_>) -> String {
    let name = tag.value().name();
    let mut index = 1;
    let mut sibling = tag.prev_sibling();
    while let Some(node) = sibling {
        if let Some(el) = ElementRef::wrap(node) {
            if el.value().name() == name {
                index += 1;
            }
        }
        sibling = node.prev_sibling();
    }
    format!("{name}:nth-of-type({index})")
}

/// Root-first tag chain from the document down to `target`.
fn chain<'a>(target: &ElementRef<'a>) -> Vec<ElementRef<'a>> {
    let mut out: Vec<ElementRef<'_>> = vec![*target];
    let mut ancestor = target.parent();
    while let Some(node) = ancestor {
        if let Some(el) = ElementRef::wrap(node) {
            out.push(el);
        }
        ancestor = node.parent();
    }
    out.reverse();
    out
}

/// Innermost elements whose normalized text equals `wanted`.
fn visible_targets<'a>(soup: &'a Soup, wanted: &str) -> Vec<ElementRef<'a>> {
    let hits: Vec<ElementRef<'_>> = soup
        .tree
        .nodes()
        .filter_map(ElementRef::wrap)
        .filter(|tag| {
            !SKIP_TAGS.contains(&tag.value().name())
                && !has_skip_ancestor(tag)
                && norm(tag) == wanted
        })
        .collect();
    let ancestors: HashSet<ego_tree::NodeId> = hits
        .iter()
        .flat_map(|tag| tag.ancestors().map(|node| node.id()))
        .collect();
    hits.into_iter()
        .filter(|tag| !ancestors.contains(&tag.id()))
        .collect()
}

/// Whether any ancestor of `tag` is a skipped container.
fn has_skip_ancestor(tag: &ElementRef<'_>) -> bool {
    let mut ancestor = tag.parent();
    while let Some(node) = ancestor {
        if let Some(el) = ElementRef::wrap(node) {
            if SKIP_TAGS.contains(&el.value().name()) {
                return true;
            }
        }
        ancestor = node.parent();
    }
    false
}

/// Attribute selectors over stable, human-chosen attribute values.
fn semantic_selectors(target: &ElementRef<'_>) -> Vec<String> {
    let attrs = ordered_attrs(target.value());
    let get = |name: &str| -> Option<String> {
        attrs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_owned())
    };
    let mut names: Vec<String> = Vec::new();
    if get("itemprop").is_some() {
        names.push("itemprop".to_owned());
    }
    let mut data: Vec<&str> = attrs
        .iter()
        .map(|(key, _)| *key)
        .filter(|key| key.starts_with("data-"))
        .collect();
    data.sort_unstable();
    names.extend(data.iter().map(|key| (*key).to_owned()));
    if get("name").is_some() {
        names.push("name".to_owned());
    }
    let mut selectors = Vec::new();
    for attr in names {
        let Some(value) = get(&attr) else { continue };
        if !value_ok(&value) || autogenerated(&value) {
            continue;
        }
        selectors.push(format!(
            "{}[{attr}=\"{}\"]",
            target.value().name(),
            css_escape(&value)
        ));
        if selectors.len() == 3 {
            break;
        }
    }
    selectors
}

/// `[A-Za-z][A-Za-z0-9_-]{0,63}` exactly.
fn value_ok(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    let rest = chars.count();
    rest <= 63
        && value[1..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A four-digit run or longer (machine-generated).
fn autogenerated(value: &str) -> bool {
    let mut run = 0;
    for ch in value.chars() {
        run = if ch.is_ascii_digit() { run + 1 } else { 0 };
        if run >= 4 {
            return true;
        }
    }
    false
}

/// Measure every proposed selector with the extraction engine; drop dead ones.
fn verify(soup: &Soup, found: &[(&str, &str)]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for (strategy, selector) in found {
        if seen.contains(selector) || selector.chars().count() > MAX_SELECTOR_CHARS {
            continue;
        }
        let Ok(parsed) = Selector::parse(selector) else {
            continue;
        };
        seen.insert(selector);
        let elements: Vec<ElementRef<'_>> = soup.select(&parsed).collect();
        out.push(json!({
            "selector": selector,
            "strategy": strategy,
            "matches": elements.len(),
            "sample_matches": elements
                .iter()
                .take(MAX_SAMPLES)
                .map(|el| char_prefix(&norm(el), MAX_SAMPLE_CHARS))
                .collect::<Vec<_>>(),
        }));
    }
    out
}

/// First `n` characters of `text` (Python slice semantics).
fn char_prefix(text: &str, n: usize) -> String {
    text.char_indices()
        .nth(n)
        .map_or(text, |(at, _)| &text[..at])
        .to_owned()
}

/// Candidate selectors for one target, in generation order (stability ladder).
fn candidates_for(soup: &Soup, target: &ElementRef<'_>) -> Vec<Value> {
    let chain = chain(target);
    let mut found: Vec<(&str, String)> = Vec::new();
    let own_id = target.value().attr("id").unwrap_or_default();
    if !own_id.is_empty() {
        found.push(("id", format!("#{}", css_escape(own_id))));
    }
    let depths = MAX_PATH_DEPTH.min(chain.len().saturating_sub(1));
    for depth in 1..=depths {
        let ancestor = chain[chain.len() - 1 - depth];
        let Some(anchor) = ancestor.value().attr("id").filter(|id| !id.is_empty()) else {
            continue;
        };
        let steps = chain[chain.len() - depth..]
            .iter()
            .map(step)
            .collect::<Vec<_>>()
            .join(" ");
        found.push(("id", format!("#{} {steps}", css_escape(anchor))));
        break;
    }
    found.extend(
        semantic_selectors(target)
            .into_iter()
            .map(|selector| ("semantic", selector)),
    );
    let signature = step(target);
    let signature_unique = select_count(soup, &signature) == 1;
    found.push(("path", signature.clone()));
    if !signature_unique {
        for depth in 1..=depths {
            let selector = chain[chain.len() - 1 - depth..]
                .iter()
                .map(step)
                .collect::<Vec<_>>()
                .join(" ");
            if select_count(soup, &selector) == 1 {
                found.push(("path", selector));
                break;
            }
        }
    }
    let found: Vec<(&str, &str)> = found
        .iter()
        .map(|(strategy, selector)| (*strategy, selector.as_str()))
        .collect();
    let mut verified = verify(soup, &found);
    if !verified
        .iter()
        .any(|candidate| candidate["matches"] == json!(1))
    {
        for depth in 1..=depths {
            let selector = chain[chain.len() - 1 - depth..]
                .iter()
                .map(nth_step)
                .collect::<Vec<_>>()
                .join(" > ");
            if select_count(soup, &selector) == 1 {
                verified.extend(verify(soup, &[("nth", selector.as_str())]));
                break;
            }
        }
    }
    verified
}

/// Document-wide match count of one selector; unparsable selectors never match.
fn select_count(soup: &Soup, selector: &str) -> usize {
    Selector::parse(selector)
        .map(|parsed| soup.select(&parsed).count())
        .unwrap_or(0)
}

/// Strategy rank of one candidate.
fn rank(strategy: &str) -> u8 {
    STRATEGY_RANK
        .iter()
        .find(|(name, _)| *name == strategy)
        .map_or(u8::MAX, |(_, rank)| *rank)
}

/// A rule id derived from the target text; `element` when nothing survives.
fn slug(text: &str) -> String {
    let mut out = String::new();
    for ch in text.to_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_owned();
    let mut slug: String = trimmed.chars().take(48).collect();
    slug = slug.trim_matches('-').to_owned();
    if slug.is_empty() {
        return "element".to_owned();
    }
    if !slug.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        slug = format!("element-{slug}").chars().take(48).collect();
    }
    slug
}

/// Build the validated `extraction_rules.v1` rule for one candidate.
pub fn emit_rule_for(
    candidate: &Value,
    text: &str,
    rule_id: Option<&str>,
) -> Result<Value, String> {
    let matches = candidate["matches"].as_u64().unwrap_or(1).max(1);
    let id = rule_id.map_or_else(|| slug(text), str::to_owned);
    let rule = json!({
        "id": id,
        "kind": "text",
        "selector": candidate["selector"].as_str().unwrap_or_default(),
        "operator": "equals",
        "value": text,
        "max_matches": MAX_MATCHES.min(matches as u32),
    });
    // The closed grammar validates a rule list; one rule goes in as a list of one.
    validate_rules(&Value::Array(vec![rule.clone()]))
        .map(|_| rule)
        .map_err(|error| error.to_string())
}

/// Suggest extraction-rule selectors for the element holding `text`.
///
/// Pure and offline: one parsed document in, candidates with measured
/// document-wide match counts out. `emit_rule` attaches a ready
/// `extraction_rules.v1` rule for the best candidate, validated with the same
/// closed grammar `scan-extract` applies.
pub fn suggest(
    html: &str,
    text: &str,
    emit_rule: bool,
    rule_id: Option<&str>,
) -> Result<Value, String> {
    if html.trim_matches(crate::py_str::is_py_space).is_empty() {
        return Err("html must be a non-empty document string".to_owned());
    }
    if text.trim_matches(crate::py_str::is_py_space).is_empty() {
        return Err("text must be the non-empty visible text of the element".to_owned());
    }
    let wanted = collapse_whitespace(text);
    if wanted.chars().count() > MAX_TEXT_LENGTH {
        return Err(format!(
            "target text exceeds the {MAX_TEXT_LENGTH}-character extraction rule value limit"
        ));
    }
    if html.len() > MAX_HTML_BYTES {
        return Err("document exceeds the 8 MiB extraction suggest budget".to_owned());
    }
    let soup = parse_document(html);
    let targets = visible_targets(&soup, &wanted);
    let mut collected: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for target in &targets {
        for candidate in candidates_for(&soup, target) {
            let key = candidate["selector"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            if seen.insert(key) {
                collected.push(candidate);
            }
        }
    }
    collected.sort_by(|a, b| {
        let key = |candidate: &Value| {
            let selector = candidate["selector"].as_str().unwrap_or_default();
            (
                rank(candidate["strategy"].as_str().unwrap_or_default()),
                if bare_id(selector) { 0u8 } else { 1 },
                selector.chars().count(),
                selector.to_owned(),
            )
        };
        key(a).cmp(&key(b))
    });
    collected.truncate(MAX_CANDIDATES);
    let ordered: Vec<Value> = collected
        .into_iter()
        .enumerate()
        .map(|(index, mut candidate)| {
            if let Some(object) = candidate.as_object_mut() {
                object.insert("best".into(), json!(index == 0));
            }
            candidate
        })
        .collect();
    let mut result = Map::new();
    result.insert("ok".into(), json!(true));
    result.insert("format".into(), json!(VERSION));
    result.insert("text".into(), json!(wanted));
    result.insert("elements_matched".into(), json!(targets.len()));
    result.insert(
        "targets_considered".into(),
        json!(targets.len().min(MAX_TARGETS)),
    );
    result.insert("candidates".into(), Value::Array(ordered.clone()));
    if ordered.is_empty() {
        result.insert(
            "note".into(),
            json!("no element with the requested text was found in the document"),
        );
    } else if targets.len() > MAX_TARGETS {
        result.insert(
            "note".into(),
            json!(format!(
                "{} elements match the text; only the innermost {MAX_TARGETS} produced candidates",
                targets.len()
            )),
        );
    }
    if emit_rule {
        let Some(best) = ordered.first() else {
            return Err("cannot emit a rule: no candidate selectors were found".to_owned());
        };
        let rule = emit_rule_for(best, &wanted, rule_id)?;
        result.insert("rule".into(), rule);
    }
    Ok(Value::Object(result))
}

/// A bare `#id` selector: the element's own id, not an id-anchored chain.
fn bare_id(selector: &str) -> bool {
    let mut chars = selector.chars();
    if chars.next() != Some('#') {
        return false;
    }
    let body = chars.as_str();
    !body.is_empty()
        && body
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_escape_leaves_plain_idents_and_escapes_the_rest() {
        assert_eq!(css_escape("lead"), "lead");
        assert_eq!(css_escape("a-b_c9"), "a-b_c9");
        // A leading digit escapes as a hex code with the terminating space.
        assert_eq!(css_escape("2col"), r"\32 col");
        assert_eq!(css_escape("a.b/c"), r"a\.b\/c");
        assert_eq!(css_escape("a b"), r"a\ b");
    }

    #[test]
    fn an_own_id_is_the_best_candidate_and_counts_are_document_wide() {
        let html = "<html><body><div class=\"wrap\">\
                    <p id=\"lead\">Hello world</p><p>Hello world</p>\
                    </div></body></html>";
        let result = suggest(html, "Hello world", false, None).unwrap();
        assert_eq!(result["ok"], json!(true));
        assert_eq!(result["format"], json!(VERSION));
        assert_eq!(result["elements_matched"], json!(2));
        let candidates = result["candidates"].as_array().unwrap();
        assert_eq!(candidates[0]["selector"], json!("#lead"));
        assert_eq!(candidates[0]["strategy"], json!("id"));
        assert_eq!(candidates[0]["best"], json!(true));
        assert_eq!(candidates[0]["matches"], json!(1));
        // The bare class-less selector stays a measured candidate, both matches counted.
        let shared = candidates
            .iter()
            .find(|c| c["selector"] == json!("p"))
            .unwrap();
        assert_eq!(shared["matches"], json!(2));
        // Identical selectors from both targets appear once.
        let selectors: Vec<&str> = candidates
            .iter()
            .filter_map(|c| c["selector"].as_str())
            .collect();
        assert_eq!(
            selectors.len(),
            selectors.iter().collect::<HashSet<_>>().len()
        );
    }

    #[test]
    fn a_positional_fallback_appears_only_when_nothing_unique_exists() {
        let html = "<html><body><p>Price</p><p>Price</p></body></html>";
        let result = suggest(html, "Price", false, None).unwrap();
        let candidates = result["candidates"].as_array().unwrap();
        // Nothing unique in the ladder, so the shortest shared selector leads.
        assert_eq!(candidates[0]["selector"], json!("p"));
        assert_eq!(candidates[0]["matches"], json!(2));
        assert!(candidates.iter().any(|c| c["selector"]
            == json!("body:nth-of-type(1) > p:nth-of-type(2)")
            && c["matches"] == json!(1)));
    }

    #[test]
    fn text_inside_code_containers_is_never_a_target() {
        let html = "<html><body><script>var price = 1;</script><p>Price</p></body></html>";
        let result = suggest(html, "var price = 1;", false, None).unwrap();
        assert_eq!(result["elements_matched"], json!(0));
        assert_eq!(result["candidates"], json!([]));
        assert!(result["note"]
            .as_str()
            .unwrap()
            .contains("no element with the requested text"));
    }

    #[test]
    fn missing_text_reports_no_candidates() {
        let result = suggest("<p>Other</p>", "Missing", false, None).unwrap();
        assert_eq!(result["candidates"], json!([]));
        assert_eq!(result["elements_matched"], json!(0));
    }

    #[test]
    fn an_emitted_rule_validates_against_the_closed_grammar() {
        let html = "<html><body><p id=\"lead\">Hello world</p></body></html>";
        let result = suggest(html, "Hello world", true, None).unwrap();
        assert_eq!(
            result["rule"],
            json!({
                "id": "hello-world",
                "kind": "text",
                "selector": "#lead",
                "operator": "equals",
                "value": "Hello world",
                "max_matches": 1,
            })
        );
        // An explicit rule id replaces the slug.
        let named = suggest(html, "Hello world", true, Some("lead-copy")).unwrap();
        assert_eq!(named["rule"]["id"], json!("lead-copy"));
    }

    #[test]
    fn a_slug_derives_from_the_target_text() {
        assert_eq!(slug("Hello, world!"), "hello-world");
        assert_eq!(slug("  --  "), "element");
        assert_eq!(slug("42%"), "element-42");
        // The prefixed form stays within the 48-character budget.
        let long: String = "9".repeat(60);
        assert_eq!(slug(&long).chars().count(), 48);
        assert!(slug(&long).starts_with("element-"));
    }

    #[test]
    fn empty_inputs_and_oversize_text_are_refused() {
        assert_eq!(
            suggest("  ", "Price", false, None).unwrap_err(),
            "html must be a non-empty document string"
        );
        assert_eq!(
            suggest("<p>Price</p>", " ", false, None).unwrap_err(),
            "text must be the non-empty visible text of the element"
        );
        let long = "x".repeat(MAX_TEXT_LENGTH + 1);
        assert_eq!(
            suggest("<p>x</p>", &long, false, None).unwrap_err(),
            "target text exceeds the 2048-character extraction rule value limit"
        );
    }
}
