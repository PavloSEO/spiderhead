//! Ready-made extraction rule sets shipped with the toolkit, usable by name.

use serde_json::{json, Value};

/// Shipped presets as `(file stem, raw JSON)`, ordered by file name.
const PRESETS: &[(&str, &str)] = &[
    (
        "breadcrumbs",
        include_str!("../data/extraction_presets/breadcrumbs.json"),
    ),
    (
        "forms",
        include_str!("../data/extraction_presets/forms.json"),
    ),
    (
        "headings-outline-check",
        include_str!("../data/extraction_presets/headings-outline-check.json"),
    ),
    (
        "headings",
        include_str!("../data/extraction_presets/headings.json"),
    ),
    (
        "images",
        include_str!("../data/extraction_presets/images.json"),
    ),
    (
        "inline-css",
        include_str!("../data/extraction_presets/inline-css.json"),
    ),
    (
        "json-ld",
        include_str!("../data/extraction_presets/json-ld.json"),
    ),
    (
        "links",
        include_str!("../data/extraction_presets/links.json"),
    ),
    (
        "media",
        include_str!("../data/extraction_presets/media.json"),
    ),
    ("meta", include_str!("../data/extraction_presets/meta.json")),
    (
        "sections",
        include_str!("../data/extraction_presets/sections.json"),
    ),
    (
        "social",
        include_str!("../data/extraction_presets/social.json"),
    ),
    (
        "structure",
        include_str!("../data/extraction_presets/structure.json"),
    ),
    (
        "tables",
        include_str!("../data/extraction_presets/tables.json"),
    ),
];

/// Every shipped preset as `{name, description, rules}` (rules is the rule count).
pub fn available() -> Vec<Value> {
    PRESETS
        .iter()
        .filter_map(|(_, raw)| serde_json::from_str::<Value>(raw).ok())
        .map(|payload| {
            json!({
                "name": payload["name"],
                "description": payload["description"],
                "rules": payload["rules"].as_array().map_or(0, Vec::len),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_parses_and_has_rules() {
        let presets = available();
        assert_eq!(presets.len(), PRESETS.len());
        assert!(presets.iter().all(|p| p["rules"].as_u64() > Some(0)));
    }
}
