//! Writer-side resource-graph lanes for a native scan artifact.
//!
//! Link and form observation rows plus the v2 resource and discovery-ledger
//! declarations stored beside each committed document. Read-only graph queries
//! over a saved scan live in `scan_graph`; this module only writes evidence.

#![allow(clippy::too_many_arguments, clippy::type_complexity)]

use crate::crawl_capture::header_pairs;
use crate::discovery_ledger::store_document_relations;
use crate::error::StoreError;
use crate::resource_graph;
use crate::scan_artifact::{dump, insert, url as intern_url, ScanError};
use crate::scan_lease::Lease;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

/// Keys of one `LinkEdge` input, sorted for a stable order.
pub const LINK_KEYS: &[&str] = &[
    "anchor",
    "anchor_source",
    "destination",
    "nofollow",
    "position",
    "raw_href",
    "rel",
    "source",
    "target",
];

/// Keys of one `FormEdge` input, sorted for a stable order.
pub const FORM_KEYS: &[&str] = &["action", "has_password", "method", "page"];

fn scan_err(message: &str) -> StoreError {
    StoreError::ScanError(ScanError {
        message: message.into(),
    })
}

fn html_of(content_capture: Option<&Map<String, Value>>) -> Option<&str> {
    content_capture
        .and_then(|c| c.get("html"))
        .and_then(Value::as_str)
}

/// Store v2 resource declarations alongside the document that emitted them.
pub fn put_resource_graph(
    con: &Connection,
    page_url_id: i64,
    source_document_id: Option<i64>,
    representation: &str,
    content_capture: Option<&Map<String, Value>>,
) -> Result<(), StoreError> {
    let (Some(_), Some(document_id)) = (content_capture, source_document_id) else {
        return Ok(());
    };
    resource_graph::store_document(
        con,
        page_url_id,
        document_id,
        representation,
        html_of(content_capture),
    )?;
    Ok(())
}

/// Persist v2-only discovery relations after their frontier outcomes settle.
pub fn put_discovery_ledger(
    con: &Connection,
    page_url_id: i64,
    source_document_id: Option<i64>,
    representation: &str,
    source_url: &str,
    depth: i64,
    content_capture: Option<&Map<String, Value>>,
    links: &[Map<String, Value>],
    candidates: &[Map<String, Value>],
    decisions: &[Map<String, Value>],
    forms: &[Map<String, Value>],
) -> Result<(), StoreError> {
    let Some(document_id) = source_document_id else {
        return Ok(());
    };
    let version: String = con.query_row(
        "SELECT format_version FROM scan WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    if version != "scan.v2" {
        return Ok(());
    }
    let header: Option<(Option<i64>, Option<String>)> = con
        .query_row(
            "SELECT d.source_response_id,r.effective_headers_redacted_json FROM documents d \
             LEFT JOIN responses r ON r.response_id=d.source_response_id WHERE d.document_id=?",
            params![document_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let mut headers = Map::new();
    if let Some((_, Some(text))) = &header {
        if !text.is_empty() {
            // A corrupt stored header is an error; only a
            // non-list payload collapses to an empty header map.
            let parsed: Value = serde_json::from_str(text)
                .map_err(|e| StoreError::Invalid(format!("stored response headers: {e}")))?;
            if let Value::Array(items) = parsed {
                for (name, value) in header_pairs(&Value::Array(items)) {
                    headers.insert(name, Value::String(value));
                }
            }
        }
    }
    store_document_relations(
        con,
        page_url_id,
        Some(document_id),
        header.as_ref().and_then(|h| h.0),
        representation,
        source_url,
        depth,
        html_of(content_capture),
        Some(&headers),
        links,
        candidates,
        decisions,
        forms,
    )
}

fn is_string(map: &Map<String, Value>, key: &str) -> bool {
    map.get(key).is_some_and(Value::is_string)
}

/// Mixin of `NativeScan` for graph writes: implement `con` and get `write_observations`.
pub trait NativeScanGraph {
    /// The writer's open connection.
    fn con(&self) -> &Connection;

    /// Validate and store one page's link and form observations.
    fn write_observations(
        &self,
        lease: &Lease,
        document_id: Option<i64>,
        representation: &str,
        links: &[Map<String, Value>],
        forms: &[Map<String, Value>],
    ) -> Result<(), StoreError> {
        let con = self.con();
        let link_keys: BTreeSet<&str> = LINK_KEYS.iter().copied().collect();
        for (ordinal, item) in links.iter().enumerate() {
            // anchor_source may be absent (#1518); every other key stays mandatory.
            let keys: BTreeSet<&str> = item.keys().map(String::as_str).collect();
            let required: BTreeSet<&str> = link_keys
                .iter()
                .copied()
                .filter(|k| *k != "anchor_source")
                .collect();
            if !keys.is_subset(&link_keys) || !keys.is_superset(&required) {
                return Err(scan_err("link input must have exactly the LinkEdge fields"));
            }
            if item["source"] != json!(lease.url) {
                return Err(scan_err("link source differs from page lease"));
            }
            let anchor_source = item.get("anchor_source").cloned().unwrap_or(json!(""));
            let rel_ok = item
                .get("rel")
                .and_then(Value::as_array)
                .is_some_and(|r| r.iter().all(Value::is_string));
            let destination = item["destination"].as_str().unwrap_or("");
            if [
                "source",
                "destination",
                "anchor",
                "position",
                "target",
                "raw_href",
            ]
            .iter()
            .any(|k| !is_string(item, k))
                || !matches!(anchor_source.as_str(), Some("") | Some("alt"))
                || destination.is_empty()
                || !item["nofollow"].is_boolean()
                || !rel_ok
            {
                return Err(scan_err("link input has invalid scalar types"));
            }
            let mut row = Map::new();
            row.insert("source_url_id".into(), json!(lease.url_id));
            row.insert(
                "destination_url_id".into(),
                json!(intern_url(con, destination)?),
            );
            row.insert("source_document_id".into(), json!(document_id));
            row.insert("evidence_representation".into(), json!(representation));
            row.insert("ordinal".into(), json!(ordinal as i64));
            row.insert("anchor".into(), item["anchor"].clone());
            row.insert("anchor_source".into(), anchor_source);
            row.insert(
                "nofollow".into(),
                json!(item["nofollow"].as_bool().unwrap_or(false) as i64),
            );
            row.insert("position".into(), item["position"].clone());
            row.insert("rel_json".into(), json!(dump(&item["rel"])));
            row.insert("target".into(), item["target"].clone());
            row.insert("raw_href".into(), item["raw_href"].clone());
            insert(con, "links", &row)?;
        }
        for (ordinal, item) in forms.iter().enumerate() {
            let keys: BTreeSet<&str> = item.keys().map(String::as_str).collect();
            if keys != FORM_KEYS.iter().copied().collect::<BTreeSet<_>>() {
                return Err(scan_err("form input must have exactly the FormEdge fields"));
            }
            if item["page"] != json!(lease.url) {
                return Err(scan_err("form page differs from page lease"));
            }
            if ["page", "method", "action"]
                .iter()
                .any(|k| !is_string(item, k))
                || !item["has_password"].is_boolean()
            {
                return Err(scan_err("form input has invalid scalar types"));
            }
            let mut row = Map::new();
            row.insert("page_url_id".into(), json!(lease.url_id));
            row.insert("ordinal".into(), json!(ordinal as i64));
            row.insert("source_document_id".into(), json!(document_id));
            row.insert("evidence_representation".into(), json!(representation));
            row.insert("method".into(), item["method"].clone());
            row.insert("action".into(), item["action"].clone());
            row.insert(
                "has_password".into(),
                json!(item["has_password"].as_bool().unwrap_or(false) as i64),
            );
            insert(con, "forms", &row)?;
        }
        Ok(())
    }
}

impl NativeScanGraph for crate::native_scan::NativeScan {
    fn con(&self) -> &Connection {
        crate::native_scan::NativeScan::con(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Host(Connection);
    impl NativeScanGraph for Host {
        fn con(&self) -> &Connection {
            &self.0
        }
    }

    fn lease() -> Lease {
        Lease {
            url_id: 1,
            url: "https://e.test/".into(),
            depth: 0,
            queue_ordinal: 0,
        }
    }

    fn link(extra: &[(&str, Value)]) -> Map<String, Value> {
        let mut m = Map::new();
        for (k, v) in [
            ("source", json!("https://e.test/")),
            ("destination", json!("https://e.test/a")),
            ("anchor", json!("a")),
            ("nofollow", json!(false)),
            ("position", json!("")),
            ("rel", json!([])),
            ("target", json!("")),
            ("raw_href", json!("/a")),
        ] {
            m.insert(k.into(), v);
        }
        for (k, v) in extra {
            m.insert((*k).into(), v.clone());
        }
        m
    }

    #[test]
    fn foreign_link_keys_are_refused() {
        let host = Host(Connection::open_in_memory().unwrap());
        let err = host
            .write_observations(&lease(), None, "static", &[link(&[("zzz", json!(1))])], &[])
            .unwrap_err();
        assert!(err.to_string().contains("exactly the LinkEdge fields"));
    }

    #[test]
    fn link_source_must_match_lease() {
        let host = Host(Connection::open_in_memory().unwrap());
        let err = host
            .write_observations(
                &lease(),
                None,
                "static",
                &[link(&[("source", json!("https://other.test/"))])],
                &[],
            )
            .unwrap_err();
        assert!(err.to_string().contains("differs from page lease"));
    }

    #[test]
    fn bad_anchor_source_is_refused() {
        let host = Host(Connection::open_in_memory().unwrap());
        let err = host
            .write_observations(
                &lease(),
                None,
                "static",
                &[link(&[("anchor_source", json!("x"))])],
                &[],
            )
            .unwrap_err();
        assert!(err.to_string().contains("invalid scalar types"));
    }

    #[test]
    fn form_keys_are_checked() {
        let host = Host(Connection::open_in_memory().unwrap());
        let mut form = Map::new();
        form.insert("page".into(), json!("https://e.test/"));
        let err = host
            .write_observations(&lease(), None, "static", &[], &[form])
            .unwrap_err();
        assert!(err.to_string().contains("FormEdge"));
    }
}
