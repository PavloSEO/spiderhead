//! The `seo_url_check_batch` catalog documents its accepted check names: the
//! tool description and the schema's `checks` property enumerate exactly the
//! handler's supported set, so callers can pick valid names without guessing.

use std::collections::BTreeSet;

use serde_json::Value;
use spiderhead_mcp::catalog::Catalog;
use spiderhead_mcp::handlers::url_checks::SUPPORTED_CHECKS;

fn spec() -> Value {
    Catalog::global()
        .specs()
        .find(|spec| spec["name"] == "seo_url_check_batch")
        .cloned()
        .expect("seo_url_check_batch in the catalog")
}

/// Check-like words in `text`: lowercased tokens that are a supported name's
/// shape (`parse`, or something ending in `-check`).
fn check_words(text: &str) -> BTreeSet<&str> {
    text.split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'))
        .filter(|token| *token == "parse" || token.ends_with("-check"))
        .collect()
}

/// Every supported name appears in both descriptions, and no stale name does.
#[test]
fn the_batch_document_names_exactly_the_supported_checks() {
    let supported: BTreeSet<&str> = SUPPORTED_CHECKS.iter().copied().collect();

    let spec = spec();
    let tool_description = spec["description"].as_str().unwrap_or_default();
    assert_eq!(
        check_words(tool_description),
        supported,
        "tool description must list exactly the supported checks: {tool_description}"
    );

    let property = &spec["inputSchema"]["properties"]["checks"];
    let property_description = property["description"].as_str().unwrap_or_default();
    assert_eq!(
        check_words(property_description),
        supported,
        "checks property description must list exactly the supported checks: {property_description}"
    );
}
