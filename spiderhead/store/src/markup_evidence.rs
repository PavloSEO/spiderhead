//! Document-bound static AMP and accessibility observations, without SQL changes.

use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Map, Value};

use crate::error::StoreError;

/// Context kind stored in `context_items`.
pub const KIND: &str = "markup_evidence";
/// Payload schema version.
pub const VERSION: &str = "markup_evidence.v1";
/// Sorted for a stable output order.
pub const REPRESENTATIONS: &[&str] = &["legacy_fragment", "rendered", "static"];

fn invalid(message: &str) -> StoreError {
    StoreError::Invalid(message.to_string())
}

fn is_int(value: &Value) -> bool {
    value.is_i64() || value.is_u64()
}

/// Keep measured absence distinct from an unparsed document.
pub fn context_item(
    page_url_id: i64,
    source_document_id: i64,
    representation: &str,
    parsed: Option<&Map<String, Value>>,
    unavailable_reason: &str,
) -> Map<String, Value> {
    let facts = parsed
        .and_then(|p| p.get("markup"))
        .filter(|f| f.is_object());
    let available = facts.is_some() && unavailable_reason.is_empty();
    let state = if available { "complete" } else { "unavailable" };
    let reason = if available {
        String::new()
    } else if unavailable_reason.is_empty() {
        "document markup was not parsed".to_string()
    } else {
        unavailable_reason.to_string()
    };
    let payload = json!({
        "schema_version": VERSION,
        "page_url_id": page_url_id,
        "source_document_id": source_document_id,
        "representation": representation,
        "state": state,
        "reason": reason,
        "facts": if available { facts.cloned().unwrap_or(Value::Null) } else { Value::Null },
    });
    let mut item = Map::new();
    item.insert("kind".into(), json!(KIND));
    item.insert(
        "item_key".into(),
        json!(format!(
            "page:{page_url_id}:document:{source_document_id}:representation:{representation}"
        )),
    );
    item.insert("payload_version".into(), json!("scan_context.v1"));
    // serde_json's default map is key-sorted, so serialized keys come out in sorted order.
    item.insert("payload_json".into(), json!(payload.to_string()));
    item.insert("completeness".into(), json!(state));
    item.insert("reason".into(), json!(reason));
    item
}

/// Validate one `amp` object of the markup facts.
fn check_amp(amp: &Value, amp_codes: &[&str]) -> Result<(), StoreError> {
    let Some(obj) = amp.as_object() else {
        return Err(invalid("markup evidence measurements are invalid"));
    };
    let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    let mut expected = vec![
        "declared",
        "errors",
        "method",
        "validator_state",
        "validator_reason",
        "unavailable",
    ];
    expected.sort_unstable();
    let mut got = keys.clone();
    got.sort_unstable();
    if got != expected
        || amp.get("method").and_then(Value::as_str) != Some("static_structure")
        || !amp.get("declared").is_some_and(Value::is_boolean)
        || !amp.get("errors").is_some_and(Value::is_array)
        || amp.get("validator_state").and_then(Value::as_str) != Some("unavailable")
        || amp
            .get("validator_reason")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
    {
        return Err(invalid("markup evidence measurements are invalid"));
    }
    for error in amp["errors"].as_array().into_iter().flatten() {
        let code = error.get("code").and_then(Value::as_str);
        if !error.is_object() || !code.is_some_and(|c| amp_codes.contains(&c)) {
            return Err(invalid("markup evidence measurements are invalid"));
        }
    }
    Ok(())
}

/// Validate the accessibility object and each issue entry.
fn check_accessibility(accessibility: &Value, a11y_codes: &[&str]) -> Result<(), StoreError> {
    let Some(obj) = accessibility.as_object() else {
        return Err(invalid("markup evidence measurements are invalid"));
    };
    let mut got: Vec<&str> = obj.keys().map(String::as_str).collect();
    got.sort_unstable();
    if got != ["issues", "method", "sample_limit", "unavailable"]
        || obj.get("sample_limit").and_then(Value::as_i64) != Some(20)
        || !is_int(&obj["sample_limit"])
        || obj.get("method").and_then(Value::as_str) != Some("static_html")
        || !obj.get("issues").is_some_and(Value::is_array)
        || !obj.get("unavailable").is_some_and(Value::is_array)
    {
        return Err(invalid("markup evidence measurements are invalid"));
    }
    for issue in obj["issues"].as_array().into_iter().flatten() {
        let Some(issue_obj) = issue.as_object() else {
            return Err(invalid("markup evidence issue is invalid"));
        };
        let mut keys: Vec<&str> = issue_obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        let code = issue.get("code").and_then(Value::as_str);
        let count = issue.get("count").and_then(Value::as_i64);
        let samples = issue.get("samples").and_then(Value::as_array);
        let truncated = issue.get("samples_truncated").and_then(Value::as_bool);
        let valid = keys == ["code", "count", "samples", "samples_truncated"]
            && code.is_some_and(|c| a11y_codes.contains(&c))
            && count.is_some_and(|c| c >= 1)
            && samples.is_some_and(|s| count.is_some_and(|c| (s.len() as i64) <= c.min(20)))
            && truncated.is_some_and(|t| {
                samples.is_some_and(|s| t == (count.unwrap_or(0) > s.len() as i64))
            });
        if !valid {
            return Err(invalid("markup evidence issue is invalid"));
        }
    }
    Ok(())
}

/// Reject malformed facts and mismatched source-document identities.
/// `amp_codes` and `a11y_codes` are the closed code registries of the markup checks
/// supplied by the caller.
pub fn validate_context(
    con: &Connection,
    item: &Map<String, Value>,
    payload: &Value,
    amp_codes: &[&str],
    a11y_codes: &[&str],
) -> Result<(), StoreError> {
    let Some(obj) = payload.as_object() else {
        return Err(invalid("markup evidence payload is invalid"));
    };
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut expected = vec![
        "schema_version",
        "page_url_id",
        "source_document_id",
        "representation",
        "state",
        "reason",
        "facts",
    ];
    expected.sort_unstable();
    let page_url_id = obj.get("page_url_id").and_then(Value::as_i64);
    let source_document_id = obj.get("source_document_id").and_then(Value::as_i64);
    let representation = obj.get("representation").and_then(Value::as_str);
    let state = obj.get("state").and_then(Value::as_str);
    let reason = obj.get("reason").and_then(Value::as_str);
    let well_formed = keys == expected
        && obj.get("schema_version").and_then(Value::as_str) == Some(VERSION)
        && page_url_id.is_some_and(|v| v >= 1)
        && source_document_id.is_some_and(|v| v >= 1)
        && representation.is_some_and(|r| REPRESENTATIONS.contains(&r))
        && state.is_some_and(|s| s == "complete" || s == "unavailable")
        && reason.is_some()
        && state == item.get("completeness").and_then(Value::as_str)
        && reason == item.get("reason").and_then(Value::as_str);
    if !well_formed {
        return Err(invalid("markup evidence payload is invalid"));
    }
    let facts = &obj["facts"];
    if state == Some("unavailable") {
        if !facts.is_null() || reason.unwrap_or("").is_empty() {
            return Err(invalid(
                "unavailable markup evidence requires a reason and no facts",
            ));
        }
    } else {
        let Some(facts_obj) = facts.as_object() else {
            return Err(invalid("markup evidence facts are invalid"));
        };
        if facts_obj.len() != 2
            || !facts_obj.contains_key("amp")
            || !facts_obj.contains_key("accessibility")
        {
            return Err(invalid("markup evidence facts are invalid"));
        }
        check_amp(&facts["amp"], amp_codes)?;
        check_accessibility(&facts["accessibility"], a11y_codes)?;
    }
    let key = format!(
        "page:{}:document:{}:representation:{}",
        page_url_id.unwrap_or(0),
        source_document_id.unwrap_or(0),
        representation.unwrap_or("")
    );
    let bound: Option<i64> = con
        .query_row(
            "SELECT 1 FROM documents WHERE document_id=?1 AND url_id=?2 AND representation=?3",
            rusqlite::params![source_document_id, page_url_id, representation],
            |r| r.get(0),
        )
        .optional()?;
    if item.get("item_key").and_then(Value::as_str) != Some(key.as_str()) || bound.is_none() {
        return Err(invalid("markup evidence binds the wrong document"));
    }
    Ok(())
}
