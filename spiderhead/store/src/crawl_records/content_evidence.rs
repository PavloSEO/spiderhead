//! Bridge captured parser output to versioned corpus/structured/i18n contexts.

#![allow(
    unused_variables,
    unused_mut,
    dead_code,
    clippy::too_many_arguments,
    clippy::new_without_default,
    clippy::type_complexity
)] // remove once the module is filled

use serde_json::Value;
use std::collections::BTreeMap;

/// Return atomic context items for corpus, structured, language and rules evidence.
pub fn capture(
    page_url_id: i64,
    source_document_id: i64,
    representation: &str,
    html: Option<&str>,
    parsed: Option<&BTreeMap<String, Value>>,
    settings: &BTreeMap<String, Value>,
    indexable: Option<bool>,
    canonical_target: &str,
    unavailable_reason: &str,
    extraction_rules: Option<&Value>,
) -> Vec<BTreeMap<String, Value>> {
    use crate::content_evidence as evidence;
    use crate::head_links;
    use crate::markup_evidence;
    use crate::structured_evidence;

    let evidence_settings = settings.get("evidence").filter(|v| v.is_object());
    let content_area = evidence_settings
        .and_then(|e| e.get("content_area"))
        .or_else(|| settings.get("content_area"));
    let configured_rules = evidence_settings.and_then(|e| e.get("extraction_rules"));
    let rules_to_apply = extraction_rules.or(configured_rules);
    let parsed_value = parsed.map(|p| Value::Object(p.clone().into_iter().collect()));
    let parsed_map = parsed.map(|p| {
        p.iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<serde_json::Map<String, Value>>()
    });

    let content = evidence::capture_document(
        page_url_id,
        source_document_id,
        representation,
        html,
        parsed_value.as_ref(),
        content_area,
        indexable,
        canonical_target,
        unavailable_reason,
    );
    let mut items: Vec<BTreeMap<String, Value>> = Vec::new();
    if let Ok(content) = content {
        if let Ok(row) = evidence::context_item(&Value::Object(content)) {
            items.push(row.into_iter().collect());
        }
    }
    let empty = BTreeMap::new();
    let head_source = Value::Object(
        parsed
            .unwrap_or(&empty)
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    );
    items.push(
        head_links::context_item(
            page_url_id,
            source_document_id,
            representation,
            &head_source,
        )
        .as_object()
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default(),
    );
    if let (Ok(structured), Ok(language)) = (
        structured_evidence::structured_payload(
            page_url_id,
            source_document_id,
            representation,
            parsed_map.as_ref(),
            html,
        ),
        structured_evidence::language_payload(
            page_url_id,
            source_document_id,
            representation,
            parsed_map.as_ref(),
            html,
        ),
    ) {
        for row in structured_evidence::context_items(&structured, &language) {
            items.push(row.into_iter().collect());
        }
    }
    items.push(
        markup_evidence::context_item(
            page_url_id,
            source_document_id,
            representation,
            parsed_map.as_ref(),
            unavailable_reason,
        )
        .into_iter()
        .collect(),
    );
    // Extraction-rule evaluation lives in the checks crate (a higher layer):
    // rules configured here are appended by the caller, which owns that edge.
    // The capture config leaves `evidence.extraction_rules` empty.
    let _ = rules_to_apply;
    items
}
