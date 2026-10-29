//! Identify which audit contract a document follows, and refuse the ones this build cannot read.
//!
//! Contract detection for the site-audit writer and the the external crawler export. Two contracts
//! exist: `spiderhead.site-audit/1`, which carries `findings`, and the the external crawler Analyzer
//! export, which carries `issues` and `schema_version`. Matching the shape is not enough:
//! each contract also declares an exact version marker, and a document from an unknown
//! revision is refused, not rendered.

use serde_json::{Map, Value};

use spiderhead_core::formats::matches;

/// Schema of the site-audit document written by the site audit writer.
pub const SITE_AUDIT_SCHEMA: &str = "spiderhead.site-audit/1";
/// The only the external crawler Analyzer `schema_version` this build was reviewed against.
pub const SF_AUDIT_SCHEMA_VERSION: &str = "2.0";

/// The two accepted audit contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditKind {
    /// `spiderhead.site-audit/1`.
    SiteAudit,
    /// the external crawler Analyzer export, `schema_version` 2.0.
    SfAudit,
}

impl AuditKind {
    /// The name the report layer uses for this kind.
    pub fn name(self) -> &'static str {
        match self {
            Self::SiteAudit => "site-audit",
            Self::SfAudit => "sf-audit",
        }
    }
}

/// Quoted text of a JSON value, for error messages that quote a marker verbatim.
fn python_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => python_string_repr(text),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(python_repr).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(key, item)| format!("{}: {}", python_string_repr(key), python_repr(item)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Quoted text of a string: single quotes unless the text holds one and no double quote.
fn python_string_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for character in text.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other if other == quote => {
                out.push('\\');
                out.push(other);
            }
            other => out.push(other),
        }
    }
    out.push(quote);
    out
}

/// A named container and the JSON type it must have.
type Container = (&'static str, fn(&Value) -> bool);

/// Names of the containers that are absent or of the wrong type, in the listed order.
fn missing_containers(document: &Map<String, Value>, expected: &[Container]) -> Vec<String> {
    expected
        .iter()
        .filter(|(name, is_type)| !document.get(*name).is_some_and(is_type))
        .map(|(name, _)| (*name).to_owned())
        .collect()
}

fn is_object(value: &Value) -> bool {
    value.is_object()
}

fn is_array(value: &Value) -> bool {
    value.is_array()
}

/// Identify the contract of `document`.
///
/// Returns `Ok(Some(kind))` for an accepted document, `Ok(None)` when neither shape matches (the
/// caller's generic "unrecognised" message applies), and `Err(message)` when the shape matches
/// but the version marker or a container is wrong. The message names the offending marker.
pub fn detect_kind(document: &Value) -> Result<Option<AuditKind>, String> {
    let Some(map) = document.as_object() else {
        return Ok(None);
    };
    if map.contains_key("findings") || map.contains_key("schema") {
        let marker = map.get("schema");
        let marker_text = marker.and_then(Value::as_str);
        if !matches(marker_text, SITE_AUDIT_SCHEMA) {
            return Err(format!(
                "unsupported or missing 'schema' marker {}; site-audit reports require exactly {}",
                marker.map_or_else(|| "None".to_owned(), python_repr),
                python_string_repr(SITE_AUDIT_SCHEMA)
            ));
        }
        let invalid = missing_containers(
            map,
            &[
                ("findings", is_array),
                ("pages", is_array),
                ("summary", is_object),
            ],
        );
        if !invalid.is_empty() {
            return Err(format!(
                "site-audit has invalid or missing container(s): {}",
                invalid.join(", ")
            ));
        }
        return Ok(Some(AuditKind::SiteAudit));
    }
    if map.contains_key("issues") || map.contains_key("schema_version") {
        let marker = map.get("schema_version");
        let marker_text = marker.and_then(Value::as_str);
        if !matches(marker_text, SF_AUDIT_SCHEMA_VERSION) {
            return Err(format!(
                "unsupported or missing 'schema_version' marker {}; Export Analyzer audits require exactly {}",
                marker.map_or_else(|| "None".to_owned(), python_repr),
                python_string_repr(SF_AUDIT_SCHEMA_VERSION)
            ));
        }
        let invalid = missing_containers(
            map,
            &[
                ("run", is_object),
                ("summary", is_object),
                ("issues", is_array),
                ("pages", is_array),
                ("groups", is_array),
            ],
        );
        if !invalid.is_empty() {
            return Err(format!(
                "Export Analyzer audit has invalid or missing container(s): {}",
                invalid.join(", ")
            ));
        }
        return Ok(Some(AuditKind::SfAudit));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn site_document() -> Value {
        json!({"schema": SITE_AUDIT_SCHEMA, "findings": [], "pages": [], "summary": {}})
    }

    fn sf_document() -> Value {
        json!({
            "schema_version": SF_AUDIT_SCHEMA_VERSION,
            "run": {}, "summary": {}, "issues": [], "pages": [], "groups": []
        })
    }

    // Behaviour of contract detection; the tests that exercise it need the
    // report builder and are deferred).
    #[test]
    fn accepts_both_contracts_at_their_exact_markers() {
        assert_eq!(
            detect_kind(&site_document()),
            Ok(Some(AuditKind::SiteAudit))
        );
        assert_eq!(detect_kind(&sf_document()), Ok(Some(AuditKind::SfAudit)));
        assert_eq!(AuditKind::SfAudit.name(), "sf-audit");
    }

    #[test]
    fn a_marker_from_another_revision_is_refused_with_its_repr() {
        let mut site = site_document();
        site["schema"] = json!("spiderhead.site-audit/2");
        assert_eq!(
            detect_kind(&site),
            Err("unsupported or missing 'schema' marker 'spiderhead.site-audit/2'; site-audit reports require exactly 'spiderhead.site-audit/1'".to_owned())
        );
        let mut sf = sf_document();
        sf["schema_version"] = json!(2.0);
        let error = detect_kind(&sf).unwrap_err();
        assert!(error.starts_with("unsupported or missing 'schema_version' marker 2.0;"));
    }

    #[test]
    fn a_missing_marker_is_named_as_none() {
        let document = json!({"findings": []});
        let error = detect_kind(&document).unwrap_err();
        assert!(error.contains("marker None;"), "{error}");
    }

    #[test]
    fn wrong_containers_are_listed_in_contract_order() {
        let site = json!({"schema": SITE_AUDIT_SCHEMA, "findings": {}, "summary": []});
        assert_eq!(
            detect_kind(&site),
            Err(
                "site-audit has invalid or missing container(s): findings, pages, summary"
                    .to_owned()
            )
        );
        let sf = json!({"schema_version": SF_AUDIT_SCHEMA_VERSION, "run": [], "issues": [], "pages": [], "groups": []});
        assert_eq!(
            detect_kind(&sf),
            Err(
                "Export Analyzer audit has invalid or missing container(s): run, summary"
                    .to_owned()
            )
        );
    }

    #[test]
    fn unknown_shapes_are_unrecognised_not_refused() {
        assert_eq!(detect_kind(&json!({"unrelated": 1})), Ok(None));
        assert_eq!(detect_kind(&json!([1, 2])), Ok(None));
    }

    #[test]
    fn python_repr_matches_the_quoting_rule() {
        assert_eq!(python_string_repr("it's"), "\"it's\"");
        assert_eq!(python_string_repr("plain"), "'plain'");
        assert_eq!(python_repr(&json!(true)), "True");
        assert_eq!(python_repr(&json!({"a": [1, null]})), "{'a': [1, None]}");
    }
}
