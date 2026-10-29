//! Comparison source adapters and delta row builders: reading collections from
//! an audit.v2 reader or a plain mapping, segment resolution, job identity,
//! and pair/fact row construction.

use std::collections::{BTreeSet, HashMap};

use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::Digest as _;
use spiderhead_parse::py_str::py_repr;
use spiderhead_store::evidence_contract::Obj;

use crate::compare::base::{header_obj, host_changed, origin_of, AuditSource, CompareError};
use crate::compare::facts::page_facts;
use crate::pyurl::urlsplit;
use crate::pyvalue::truthy;
use crate::segments::{rule_matches, Segment, UNSEGMENTED};

use super::shared::{hex, py_json};

/// The row count a source claims for one collection, when it can be known.
pub(crate) fn collection_count(source: &AuditSource<'_>, name: &str) -> Option<i64> {
    match source {
        AuditSource::Reader(reader) => reader.collections.get(&format!("/{name}")).copied(),
        AuditSource::Document(document) => document
            .get(name)
            .and_then(Value::as_array)
            .map(|list| list.len() as i64),
    }
}

/// The segment definitions a run recorded, normalized to the engine's shape.
/// Unusable definitions degrade to "no segments" — a broken declaration must
/// not fail the whole comparison, it only costs segment coverage.
pub(crate) fn segment_definitions(source: &AuditSource<'_>) -> Vec<Value> {
    let Some(config) = header_obj(source)
        .get("run")
        .and_then(Value::as_object)
        .and_then(|run| run.get("crawl_config"))
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };
    let analysis = config
        .get("analysis.segments")
        .filter(|value| truthy(value));
    if let Some(list) = analysis
        .and_then(Value::as_array)
        .filter(|list| !list.is_empty())
    {
        if list.iter().any(|item| item.as_object().is_none()) {
            return Vec::new();
        }
        return list.clone();
    }
    let Some(scope) = config
        .get("scope.segments")
        .filter(|value| truthy(value))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut definitions = Vec::new();
    for item in scope {
        let Some(item) = item.as_object() else {
            return Vec::new();
        };
        let Some(name) = item.get("name").and_then(Value::as_str) else {
            return Vec::new();
        };
        let mut rules = Vec::new();
        if truthy(&item.get("prefix").cloned().unwrap_or(Value::Null)) {
            rules.push(json!({
                "op": "prefix",
                "field": "path",
                "value": item.get("prefix").cloned().unwrap_or(Value::Null),
            }));
        }
        if truthy(&item.get("host").cloned().unwrap_or(Value::Null)) {
            rules.push(json!({
                "op": "eq",
                "field": "host",
                "value": item.get("host").cloned().unwrap_or(Value::Null),
            }));
        }
        let pattern = item.get("pattern").filter(|value| truthy(value));
        if let Some(pattern) = pattern {
            rules.push(json!({
                "op": "regex",
                "field": "url",
                "value": pattern.clone(),
            }));
        }
        definitions.push(json!({"name": name, "rules": rules}));
    }
    definitions
}

/// The one segment a page is reported under: the first declared segment it
/// matches, or the shared unsegmented bucket.
pub(crate) fn primary_segment(page: &Obj, ordered_segments: &[Segment]) -> String {
    if ordered_segments.is_empty() {
        return UNSEGMENTED.to_owned();
    }
    let Some(url) = page.get("url").and_then(Value::as_str) else {
        // Rows without a URL cannot be placed; pages are validated earlier, so
        // this only guards a porting bug.
        return UNSEGMENTED.to_owned();
    };
    let parts = urlsplit(url);
    let mut record: Obj = page.clone();
    record.insert("path".into(), json!(parts.path));
    record.insert("host".into(), json!(parts.hostname().unwrap_or_default()));
    let mut resolved: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut primary: Option<String> = None;
    for segment in ordered_segments {
        let matched = segment
            .rules
            .iter()
            .any(|rule| rule_matches(rule, &record, &resolved));
        resolved.insert(
            segment.name.clone(),
            if matched {
                BTreeSet::from([url.to_owned()])
            } else {
                BTreeSet::new()
            },
        );
        if matched && primary.is_none() {
            primary = Some(segment.name.clone());
        }
    }
    match primary {
        Some(name) if name != UNSEGMENTED => name,
        _ => UNSEGMENTED.to_owned(),
    }
}

/// The stable job identity: everything a resume must agree on — source
/// identities, force, the exact correspondence declaration and compression —
/// hashed into 32 hex characters.
pub(crate) fn job_id(
    identities: &Obj,
    force: bool,
    declaration: Option<&crate::compare::base::Declaration>,
    compression: &str,
) -> String {
    let declaration_hash = declaration.map(|decl| {
        let rendered = py_json(&json!({
            "origin_map": decl.origin_map,
            "pairs": decl.pairs,
        }));
        hex(&sha2::Sha256::digest(rendered.as_bytes()))
    });
    let key = py_json(&json!({
        "identities": identities,
        "force": force,
        "correspondence": declaration_hash,
        "compression": compression,
    }));
    let digest = hex(&sha2::Sha256::digest(key.as_bytes()));
    digest[..32].to_owned()
}

/// One row of a source collection: the indexed document when it was retained,
/// otherwise a direct read from the audit.v2 store.
pub(crate) fn row(
    source: &AuditSource<'_>,
    name: &str,
    ordinal: i64,
    document: Option<&str>,
) -> Result<Value, CompareError> {
    if let Some(document) = document {
        return serde_json::from_str(document).map_err(|e| CompareError(e.to_string()));
    }
    match source {
        AuditSource::Reader(reader) => reader
            .get_item(&format!("/{name}"), ordinal)
            .map_err(|e| CompareError(e.to_string())),
        AuditSource::Document(_) => Err(CompareError(format!(
            "comparison source is missing the retained document for {} row {ordinal}",
            py_repr(name)
        ))),
    }
}

/// One declared correspondence pair, with the origins and host verdict a
/// reviewer needs to judge the mapping.
pub(crate) fn pair(before_url: &str, after_url: &str, kind: &str, state: &str) -> Obj {
    let mut row = Obj::new();
    row.insert("before_url".into(), json!(before_url));
    row.insert("after_url".into(), json!(after_url));
    row.insert("correspondence_key".into(), json!(after_url));
    row.insert("kind".into(), json!(kind));
    row.insert("state".into(), json!(state));
    row.insert("before_origin".into(), json!(origin_of(before_url)));
    row.insert("after_origin".into(), json!(origin_of(after_url)));
    row.insert(
        "host_changed".into(),
        json!(host_changed(before_url, after_url)),
    );
    row
}

/// Matched pairs with page facts on both sides, in before-URL order. The full
/// list is materialized before streaming into the facts export.
pub(crate) fn facts_rows(
    con: &Connection,
    before_source: &AuditSource<'_>,
    after_source: &AuditSource<'_>,
) -> Result<Vec<Obj>, CompareError> {
    let mut statement = con
        .prepare(
            "SELECT pairs.document, b.ordinal, b.document, a.ordinal, a.document FROM pairs
             JOIN pages b ON b.side=0 AND b.url=pairs.before_url
             JOIN pages a ON a.side=1 AND a.url=pairs.after_url
             WHERE pairs.state='matched' ORDER BY pairs.before_url",
        )
        .map_err(|e| CompareError(e.to_string()))?;
    let mut rows = statement
        .query([])
        .map_err(|e| CompareError(e.to_string()))?;
    let mut result = Vec::new();
    while let Some(row) = rows.next().map_err(|e| CompareError(e.to_string()))? {
        let pair_document: Option<String> = row.get(0).map_err(|e| CompareError(e.to_string()))?;
        let pair: Obj = serde_json::from_str(
            &pair_document
                .ok_or_else(|| CompareError("correspondence pair row is missing".into()))?,
        )
        .map_err(|e| CompareError(e.to_string()))?;
        let before_ordinal: i64 = row.get(1).map_err(|e| CompareError(e.to_string()))?;
        let before_document: Option<String> =
            row.get(2).map_err(|e| CompareError(e.to_string()))?;
        let after_ordinal: i64 = row.get(3).map_err(|e| CompareError(e.to_string()))?;
        let after_document: Option<String> = row.get(4).map_err(|e| CompareError(e.to_string()))?;
        let before = page_facts(row_to_obj(&row_value(
            before_source,
            before_ordinal,
            before_document,
        )?)?);
        let after = page_facts(row_to_obj(&row_value(
            after_source,
            after_ordinal,
            after_document,
        )?)?);
        let mut changed: Vec<String> = before
            .keys()
            .filter(|name| {
                let before_value = before[name.as_str()].clone();
                let after_value = after.get(name.as_str()).cloned().unwrap_or(Value::Null);
                !crate::pyvalue::py_eq(&before_value, &after_value)
            })
            .cloned()
            .collect();
        changed.sort();
        let mut out = Obj::new();
        for key in [
            "before_url",
            "after_url",
            "correspondence_key",
            "kind",
            "host_changed",
        ] {
            out.insert(key.into(), pair.get(key).cloned().unwrap_or(Value::Null));
        }
        out.insert("before".into(), Value::Object(before));
        out.insert("after".into(), Value::Object(after));
        out.insert(
            "changed".into(),
            Value::Array(changed.into_iter().map(Value::String).collect()),
        );
        result.push(out);
    }
    Ok(result)
}

fn row_value(
    source: &AuditSource<'_>,
    ordinal: i64,
    document: Option<String>,
) -> Result<Value, CompareError> {
    row(source, "pages", ordinal, document.as_deref())
}

fn row_to_obj(value: &Value) -> Result<&Obj, CompareError> {
    value
        .as_object()
        .ok_or_else(|| CompareError("comparison page row is not an object".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_segment_first_match_wins() {
        let definitions = vec![
            json!({"name": "docs", "rules": [{"op": "prefix", "field": "path", "value": "/docs"}]}),
            json!({"name": "site", "rules": [{"op": "prefix", "field": "path", "value": "/"}]}),
        ];
        let ordered = crate::segments::resolve_order(&definitions).expect("resolve");
        let page: Obj =
            serde_json::from_str(r#"{"url": "https://example.com/docs/a", "status_code": 200}"#)
                .expect("page");
        assert_eq!(primary_segment(&page, &ordered), "docs");
        let root: Obj =
            serde_json::from_str(r#"{"url": "https://example.com/other"}"#).expect("page");
        assert_eq!(primary_segment(&root, &ordered), "site");
        let empty: Obj =
            serde_json::from_str(r#"{"url": "https://example.com/other"}"#).expect("page");
        assert_eq!(primary_segment(&empty, &[]), "unsegmented");
    }

    #[test]
    fn job_id_depends_on_every_option() {
        let identities: Obj = serde_json::from_str(
            r#"{"before": {"audit_sha256": "a"}, "after": {"audit_sha256": "b"}}"#,
        )
        .expect("identities");
        let base = job_id(&identities, false, None, "none");
        assert_eq!(base.len(), 32);
        assert_ne!(base, job_id(&identities, true, None, "none"));
        assert_ne!(base, job_id(&identities, false, None, "gzip"));
        let declaration = crate::compare::base::Declaration {
            origin_map: Obj::new(),
            pairs: [("https://a/".to_owned(), "https://b/".to_owned())]
                .into_iter()
                .collect(),
        };
        assert_ne!(base, job_id(&identities, false, Some(&declaration), "none"));
    }
}
