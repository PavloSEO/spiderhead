//! Page commit of the native scan writer: one complete fold-back unit or none of it.
//!
//! The frontier, runtime and finalization lanes live in [`crate::scan_lease`]; page commit needs the writer's corpus,
//! observation and event lanes, so it is implemented on [`NativeScan`] itself.

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::error::StoreError;
use crate::native_scan::NativeScan;
use crate::scan_artifact::{
    self as artifact, dump, expected, insert, url as intern_url, ScanError, MAX_RECORD_BYTES,
    PAGE_BOOLS, PAGE_NONNEGATIVE_INTS,
};
use crate::scan_bodies::{content_capture, ContentEvidenceSink, NativeScanBodies};
use crate::scan_lease::{
    bounded_items, digest, json_chunks, CommitReceipt, Lease, NativeScanLease, PageCommit,
    ScanWriterHost, MAX_EDGES_PER_PAGE, MAX_PAGE_COMMIT_ITEMS, OPTIONAL_PAGE_SOURCES,
    PAGE_JSON_SOURCES,
};

fn fail(message: impl Into<String>) -> StoreError {
    ScanError {
        message: message.into(),
    }
    .into()
}

/// Bounded check over a list of maps.
fn bounded_maps(
    items: &[Map<String, Value>],
    label: &str,
    limit: i64,
) -> Result<Vec<Map<String, Value>>, StoreError> {
    let values: Vec<Value> = items.iter().cloned().map(Value::Object).collect();
    Ok(bounded_items(&values, label, limit)?
        .into_iter()
        .filter_map(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .collect())
}

/// Exact integer test: booleans and floats are not integers.
fn is_int(value: &Value) -> bool {
    value.is_i64() || value.is_u64()
}

impl ScanWriterHost for NativeScan {
    fn connection(&self) -> &rusqlite::Connection {
        self.con()
    }

    fn assert_mutable(&self) -> Result<(), StoreError> {
        NativeScan::assert_mutable(self)
    }

    fn begin(&self, enforce_wal_bound: bool) -> Result<(), StoreError> {
        NativeScan::begin(self, enforce_wal_bound)
    }

    fn rollback(&self) {
        NativeScan::rollback(self)
    }

    fn hit(&self, name: &str) {
        NativeScan::hit(self, name)
    }

    fn event(&self, event_type: &str, payload: Value) -> Result<(), StoreError> {
        let payload = match payload {
            Value::Object(map) => map,
            _ => return Err(fail("event payload must be an object")),
        };
        NativeScan::event(self, event_type, &payload, None)
    }

    fn event_coverage(&self) -> Result<(), StoreError> {
        NativeScan::event_coverage(self)
    }
}

impl NativeScanLease for NativeScan {}

impl ContentEvidenceSink for NativeScan {
    fn put_content_evidence(
        &self,
        con: &rusqlite::Connection,
        page_url_id: i64,
        source_document_id: i64,
        representation: &str,
        capture: &Map<String, Value>,
    ) -> Result<(), StoreError> {
        let text = |key: &str| {
            capture
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let parsed = capture.get("parsed").and_then(Value::as_object).map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<std::collections::BTreeMap<_, _>>()
        });
        let settings: std::collections::BTreeMap<String, Value> = capture
            .get("settings")
            .and_then(Value::as_object)
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let items = crate::crawl_records::content_evidence::capture(
            page_url_id,
            source_document_id,
            representation,
            capture.get("html").and_then(Value::as_str),
            parsed.as_ref(),
            &settings,
            capture.get("indexable").and_then(Value::as_bool),
            &text("canonical_target"),
            &text("unavailable_reason"),
            None,
        );
        for item in items {
            let item: Map<String, Value> = item.into_iter().collect();
            crate::native_context::put_context(con, &item, None)?;
        }
        Ok(())
    }
}

impl NativeScan {
    /// Map a claimed page record onto the `pages` columns (`url_id` and `page_ordinal` set,
    /// `document_id` left to the caller).
    pub fn page_row(
        &mut self,
        record: &Map<String, Value>,
        lease: &Lease,
    ) -> Result<Map<String, Value>, StoreError> {
        if record.get("url") != Some(&json!(lease.url)) {
            return Err(fail(
                "page record URL differs from the claimed frontier lease",
            ));
        }
        if record.get("crawl_depth") != Some(&json!(lease.depth)) {
            return Err(fail(
                "page record crawl depth differs from the claimed frontier lease",
            ));
        }
        // Legacy JSONL carries these transient projections; scan.v1 keeps source bodies, and the
        // per-attempt request rows in ``responses``.
        let record: Map<String, Value> = record
            .iter()
            .filter(|(name, _)| !matches!(name.as_str(), "render_blocking" | "attempts"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let schema = expected()?;
        let columns = schema
            .columns
            .get("pages")
            .ok_or_else(|| fail("pages table is missing from the expected schema"))?;
        let internal = |name: &str| matches!(name, "url_id" | "page_ordinal" | "document_id");
        let json_source = |name: &str| {
            PAGE_JSON_SOURCES
                .iter()
                .find(|(column, _)| *column == name)
                .map(|(_, source)| *source)
        };
        let mut source_names: Vec<&str> = vec!["url"];
        for column in columns {
            if !internal(&column.name) {
                source_names.push(json_source(&column.name).unwrap_or(&column.name));
            }
        }
        let mut unknown: Vec<&str> = record
            .keys()
            .map(String::as_str)
            .filter(|k| !source_names.contains(k))
            .collect();
        if !unknown.is_empty() {
            unknown.sort_unstable();
            return Err(fail(format!(
                "page record has unknown fields: {}",
                py_str_list(&unknown)
            )));
        }
        for name in PAGE_NONNEGATIVE_INTS {
            // Nullable measurements: absent or null means unmeasured, never zero.
            match record.get(*name) {
                None | Some(Value::Null) => {}
                Some(v) if v.as_i64().is_some_and(|n| n >= 0) || v.as_u64().is_some() => {}
                Some(_) => {
                    return Err(fail(format!(
                        "pages.{name}: expected a nonnegative integer"
                    )))
                }
            }
        }
        let count = |key: &str| record.get(key).and_then(Value::as_i64).unwrap_or(0);
        if count("content_frames_same_origin") > count("content_frames") {
            return Err(fail(
                "pages.content_frames_same_origin exceeds content_frames",
            ));
        }
        let unavailable = record.get("body_unavailable").and_then(Value::as_str);
        if !matches!(
            unavailable,
            Some(
                "" | "oversized"
                    | "excluded_by_media_type"
                    | "not_included_by_media_type"
                    | "media_type_unavailable"
            )
        ) {
            return Err(fail("pages.body_unavailable has an unknown marker"));
        }
        let ordinal = match self.page_ordinal_next {
            Some(n) => n,
            None => {
                let sql = if self.v2_pages {
                    "SELECT COALESCE(MAX(page_ordinal)+1,0) FROM pages"
                } else {
                    "SELECT COUNT(*) FROM pages"
                };
                let n: i64 = self.con().query_row(sql, [], |r| r.get(0))?;
                self.page_ordinal_next = Some(n);
                n
            }
        };
        let mut row = Map::new();
        row.insert("url_id".into(), json!(lease.url_id));
        row.insert("page_ordinal".into(), json!(ordinal));
        for column in columns {
            let name = column.name.as_str();
            if internal(name) {
                continue;
            }
            let source = json_source(name).unwrap_or(name);
            let Some(found) = record.get(source) else {
                return Err(fail(format!(
                    "pages.{source}: current native record field is missing"
                )));
            };
            let mut value = found.clone();
            if value.is_null() && !OPTIONAL_PAGE_SOURCES.contains(&source) {
                return Err(fail(format!(
                    "pages.{source}: current native record field cannot be null"
                )));
            }
            if json_source(name).is_some() {
                validate_json_source(name, source, &value)?;
                value = Value::String(dump(&value));
            } else if PAGE_BOOLS.contains(&name) && !value.is_null() {
                let Value::Bool(flag) = value else {
                    return Err(fail(format!("pages.{source}: expected bool or null")));
                };
                value = json!(i64::from(flag));
            }
            if value.is_null() && column.required && column.default.is_none() {
                return Err(fail(format!("pages.{source}: required value is missing")));
            }
            if !value.is_null() {
                row.insert(name.to_string(), value);
            }
        }
        for (name, value) in &row {
            let kind = columns
                .iter()
                .find(|c| &c.name == name)
                .map_or("", |c| c.kind.as_str());
            match kind {
                "TEXT" => {
                    let Value::String(text) = value else {
                        return Err(fail(format!("pages.{name}: expected text")));
                    };
                    if text.len() as i64 > MAX_RECORD_BYTES {
                        return Err(fail(format!(
                            "pages.{name}: text exceeds the 8 MiB record limit"
                        )));
                    }
                }
                "INTEGER" if !is_int(value) => {
                    return Err(fail(format!("pages.{name}: expected integer")))
                }
                "REAL" if !value.as_f64().is_some_and(f64::is_finite) => {
                    return Err(fail(format!("pages.{name}: expected finite real")))
                }
                _ => {}
            }
        }
        Ok(row)
    }

    /// Commit one complete fold-back unit or none of it.
    pub fn commit_page(&mut self, commit: &PageCommit) -> Result<CommitReceipt, StoreError> {
        self.assert_mutable()?;
        let lease = commit
            .lease
            .as_ref()
            .ok_or_else(|| fail("page commit requires its claimed lease"))?;
        let prepared = Prepared::new(commit, lease)?;
        self.begin(true)?;
        match self.commit_page_body(commit, lease, &prepared) {
            Ok(receipt) => Ok(receipt),
            Err(error) => {
                self.rollback();
                Err(error)
            }
        }
    }

    fn commit_page_body(
        &mut self,
        commit: &PageCommit,
        lease: &Lease,
        p: &Prepared,
    ) -> Result<CommitReceipt, StoreError> {
        let record = &commit.record;
        let frontier: Option<(i64, String, i64, i64, String)> = self
            .con()
            .query_row(
                "SELECT f.url_id, u.url, f.depth, f.queue_ordinal, f.state FROM frontier f \
                 JOIN urls u USING(url_id) WHERE f.url_id=?",
                [lease.url_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let Some((url_id, url, depth, ordinal, state)) = frontier else {
            return Err(fail("unknown frontier lease"));
        };
        if (url_id, url.as_str(), depth, ordinal)
            != (
                lease.url_id,
                lease.url.as_str(),
                lease.depth,
                lease.queue_ordinal,
            )
        {
            return Err(fail("forged or stale frontier lease"));
        }
        if state == "done" {
            let prior: Option<String> = self
                .con()
                .query_row(
                    "SELECT payload_json FROM context_items WHERE kind='native_commit' AND item_key=?",
                    [lease.queue_ordinal.to_string()],
                    |r| r.get(0),
                )
                .optional()?;
            let same = prior
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                .is_some_and(|v| v.get("digest").and_then(Value::as_str) == Some(&p.digest));
            if !same {
                return Err(fail("committed lease was retried with different evidence"));
            }
            let revision: i64 = self.con().query_row(
                "SELECT evidence_revision FROM scan WHERE singleton=1",
                [],
                |r| r.get(0),
            )?;
            self.con().execute_batch("COMMIT")?;
            return Ok(CommitReceipt {
                evidence_revision: revision,
                already_committed: true,
            });
        }
        if state != "inflight" || ordinal != lease.queue_ordinal {
            return Err(fail("frontier lease is no longer inflight"));
        }
        let first_inflight: Option<i64> = self.con().query_row(
            "SELECT MIN(queue_ordinal) FROM frontier WHERE state='inflight'",
            [],
            |r| r.get(0),
        )?;
        if first_inflight != Some(lease.queue_ordinal) {
            return Err(fail(
                "page commit must fold back the contiguous inflight prefix",
            ));
        }
        let mut document_id: Option<i64> = None;
        if !commit.captures.is_empty() {
            let policy_text: String =
                self.con()
                    .query_row("SELECT retention_json FROM scan", [], |r| r.get(0))?;
            let policy: Value = serde_json::from_str(&policy_text)?;
            let policy_map = policy.as_object().cloned().unwrap_or_default();
            self.check_capture_disk_space(&policy_map, p.body_bytes)?;
            let known: Vec<&str> = ["redirect_chain", "canonical_chain"]
                .iter()
                .flat_map(|field| {
                    record
                        .get(*field)
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|hop| hop.get("url").and_then(Value::as_str))
                })
                .collect();
            let retain = self.retain_no_store_acknowledged()?;
            for event in &commit.captures {
                if event.requested_url != lease.url
                    && !known.contains(&event.requested_url.as_str())
                {
                    return Err(fail(
                        "response does not belong to the page or its observed destination diagnostics",
                    ));
                }
                let (_response_id, observed) = crate::corpus::store_response(
                    self.con(),
                    event,
                    "page",
                    &policy,
                    retain,
                    Some(&event.requested_url),
                    "static",
                    None,
                )?;
                if event.requested_url == lease.url {
                    document_id = observed;
                }
            }
            if document_id.is_none() {
                return Err(fail("page has no response for its frontier lease"));
            }
            let changed: Vec<crate::scan_bodies::CaptureEvent> = commit
                .captures
                .iter()
                .map(|e| crate::scan_bodies::CaptureEvent {
                    session_changed: e.session_changed,
                    entity_bytes: e.entity_bytes.clone(),
                })
                .collect();
            self.record_session_change(&changed)?;
            self.hit("after_bodies");
        }
        let mut page_row = self.page_row(record, lease)?;
        page_row.insert("document_id".into(), json!(document_id));
        insert(self.con(), "pages", &page_row)?;
        self.hit("after_page");
        crate::scan_links::NativeScanGraph::write_observations(
            self,
            lease,
            document_id,
            "static",
            &p.links,
            &p.forms,
        )?;
        crate::scan_bodies::put_content_evidence(
            self.con(),
            self,
            lease.url_id,
            document_id,
            "static",
            p.content_capture.as_ref(),
        )?;
        crate::scan_links::put_resource_graph(
            self.con(),
            lease.url_id,
            document_id,
            "static",
            p.content_capture.as_ref(),
        )?;
        let mut context = p.context.clone();
        if let Some(coverage) = &commit.route_coverage {
            context.extend(crate::rendered_routes::context_items(
                lease.url_id,
                document_id,
                coverage,
                &p.route_observations,
            )?);
        }
        if let Some(state) = &commit.resource_inventory_state {
            let config_text: String =
                self.con()
                    .query_row("SELECT config_json FROM scan", [], |r| r.get(0))?;
            let config: Value = serde_json::from_str(&config_text)?;
            let fetch = crate::discovery_ledger::truthy(config.pointer("/resources/fetch"));
            let declarations: Vec<indexmap::IndexMap<String, Value>> = p
                .resources
                .iter()
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .collect();
            let document =
                document_id.ok_or_else(|| fail("resource declarations need a stored document"))?;
            crate::resources::put_declarations(
                self.con(),
                lease.url_id,
                document,
                "static",
                &declarations,
                fetch,
                state,
                commit.resources_omitted,
            )?;
        }
        self.hit("after_observations");
        for (index, item) in p.decisions.iter().enumerate() {
            let decision = decision_row(item, lease, index)?;
            insert(self.con(), "decisions", &decision)?;
        }
        let query_limit = self.stored_query_limit()?;
        for (index, (path_key, query_key, requested_url)) in p.query_reservations.iter().enumerate()
        {
            if [path_key, query_key, requested_url]
                .iter()
                .any(|v| v.is_empty())
            {
                return Err(fail(
                    "query reservation needs path, query and exact requested URL",
                ));
            }
            let con = self.con();
            let count: i64 = con.query_row(
                "SELECT COUNT(*) FROM query_variants WHERE path_key=?",
                [path_key],
                |r| r.get(0),
            )?;
            let exists = con
                .prepare("SELECT 1 FROM query_variants WHERE path_key=? AND query_key=?")?
                .exists([path_key, query_key])?;
            if !exists && query_limit > 0 && count >= query_limit {
                self.hit("before_query_budget");
                let mut decision = Map::new();
                decision.insert("url".into(), json!(requested_url));
                decision.insert("reason".into(), json!("query_variants_limit"));
                decision.insert("source".into(), json!(lease.url));
                decision.insert("depth".into(), json!(lease.depth + 1));
                decision.insert(
                    "occurrence_key".into(),
                    json!(format!("{}:query_budget:{index}", lease.queue_ordinal)),
                );
                insert(con, "decisions", &decision)?;
                let rejected = intern_url(con, requested_url)?;
                if !con
                    .prepare("SELECT 1 FROM frontier WHERE url_id=?")?
                    .exists([rejected])?
                {
                    let next: i64 = con.query_row(
                        "SELECT COALESCE(MAX(queue_ordinal)+1, 0) FROM frontier",
                        [],
                        |r| r.get(0),
                    )?;
                    con.execute(
                        "INSERT INTO frontier(url_id, queue_ordinal, depth, state) VALUES(?,?,?, 'excluded')",
                        params![rejected, next, lease.depth + 1],
                    )?;
                }
                self.hit("after_query_budget");
                continue;
            }
            con.execute(
                "INSERT OR IGNORE INTO query_variants(path_key, query_key) VALUES(?,?)",
                [path_key, query_key],
            )?;
            self.hit("after_query_budget");
        }
        let con = self.con();
        let mut next_ordinal: i64 = con.query_row(
            "SELECT COALESCE(MAX(queue_ordinal)+1, 0) FROM frontier",
            [],
            |r| r.get(0),
        )?;
        for (url, depth) in &p.discovered {
            if url.is_empty() || *depth < 0 {
                return Err(fail(
                    "discovered frontier entry must be a nonempty URL and nonnegative depth",
                ));
            }
            let url_id = intern_url(con, url)?;
            if !con
                .prepare("SELECT 1 FROM frontier WHERE url_id=?")?
                .exists([url_id])?
            {
                con.execute(
                    "INSERT INTO frontier(url_id, queue_ordinal, depth, state) VALUES(?,?,?, 'queued')",
                    params![url_id, next_ordinal, depth],
                )?;
                next_ordinal += 1;
            }
        }
        crate::frontier::apply_candidates(
            con,
            &p.candidates,
            &lease.url,
            lease.queue_ordinal,
            Some(query_limit),
        )?;
        self.partial_reasons(&p.partial_reasons)?;
        let con = self.con();
        con.execute(
            "UPDATE frontier SET state='done' WHERE url_id=?",
            [lease.url_id],
        )?;
        crate::scan_links::put_discovery_ledger(
            con,
            lease.url_id,
            document_id,
            "static",
            &lease.url,
            lease.depth,
            p.content_capture.as_ref(),
            &p.links,
            &p.candidates,
            &p.decisions,
            &p.forms,
        )?;
        self.hit("after_frontier");
        self.hit("before_runtime");
        self.write_runtime(&p.runtime, lease.depth)?;
        self.hit("after_runtime");
        for item in &context {
            crate::native_context::put_context(self.con(), item, None)?;
        }
        let mut marker = Map::new();
        marker.insert("kind".into(), json!("native_commit"));
        marker.insert("item_key".into(), json!(lease.queue_ordinal.to_string()));
        marker.insert("payload_version".into(), json!("scan_context.v1"));
        marker.insert(
            "payload_json".into(),
            json!(dump(&json!({"digest": p.digest}))),
        );
        marker.insert("completeness".into(), json!("complete"));
        marker.insert("reason".into(), json!("atomic page commit"));
        insert(self.con(), "context_items", &marker)?;
        ScanWriterHost::event(
            self,
            "checkpoint",
            json!({"state": "page_committed", "queued": p.discovered.len(), "inflight": 0}),
        )?;
        self.event_coverage()?;
        self.con().execute_batch(
            "UPDATE scan SET evidence_revision=evidence_revision+1 WHERE singleton=1",
        )?;
        self.sync_corpus(document_id, commit.resource_inventory_state.as_deref())?;
        self.hit("before_commit");
        self.con().execute_batch("COMMIT")?;
        let ordinal = page_row
            .get("page_ordinal")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        self.page_ordinal_next = Some(ordinal + 1);
        let revision: i64 = self.con().query_row(
            "SELECT evidence_revision FROM scan WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        self.hit("after_commit");
        Ok(CommitReceipt {
            evidence_revision: revision,
            already_committed: false,
        })
    }
}

/// Sorted list of unknown field names, for the error message.
fn py_str_list(names: &[&str]) -> String {
    let items: Vec<String> = names.iter().map(|n| py_repr(n)).collect();
    format!("[{}]", items.join(", "))
}

fn py_repr(text: &str) -> String {
    if text.contains('\'') && !text.contains('"') {
        format!("\"{text}\"")
    } else {
        format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

/// The JSON-backed page columns carry shape checks beyond "is JSON".
fn validate_json_source(name: &str, source: &str, value: &Value) -> Result<(), StoreError> {
    match name {
        "hreflang_json" => {
            let ok = value.as_array().is_some_and(|items| {
                items.iter().all(|item| {
                    item.as_object().is_some_and(|m| {
                        m.len() == 3
                            && ["lang", "raw_href", "url"]
                                .iter()
                                .all(|k| m.get(*k).is_some_and(Value::is_string))
                    })
                })
            });
            if !ok {
                return Err(fail(
                    "pages.hreflang must be ordered lang/raw_href/url string objects",
                ));
            }
        }
        "heading_outline_json" => artifact::heading_outline(value)?,
        "invalid_link_targets_json" if !value.is_null() => artifact::invalid_link_targets(value)?,
        "link_placement_json" if !value.is_null() => artifact::link_placement(value)?,
        "trust_signals_json" if !value.is_null() => artifact::trust_signals(value)?,
        "duplicate_ids_json" if !value.is_null() => artifact::duplicate_ids(value)?,
        "redirect_chain_json" | "canonical_chain_json" => {
            let ok = value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_object));
            if !ok {
                return Err(fail(format!(
                    "pages.{source} must be an ordered object list"
                )));
            }
        }
        _ => {}
    }
    Ok(())
}

/// One decision input checked and shaped as a `decisions` row.
fn decision_row(
    item: &Map<String, Value>,
    lease: &Lease,
    index: usize,
) -> Result<Map<String, Value>, StoreError> {
    let allowed = ["url", "reason", "source", "depth", "occurrence_key"];
    if item.keys().any(|k| !allowed.contains(&k.as_str()))
        || !["url", "reason", "source"]
            .iter()
            .all(|k| item.contains_key(*k))
    {
        return Err(fail("decision input has unknown or missing fields"));
    }
    let text = |key: &str| {
        item.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    if ["url", "reason", "source"]
        .iter()
        .any(|k| text(k).is_none())
    {
        return Err(fail("decision input has invalid string fields"));
    }
    let depth = item.get("depth").cloned().unwrap_or(Value::Null);
    if !depth.is_null() && !depth.as_i64().is_some_and(|d| d >= 0) {
        return Err(fail(
            "decision input depth must be a nonnegative integer or null",
        ));
    }
    let occurrence = match item.get("occurrence_key") {
        None => format!("{}:decision:{index}", lease.queue_ordinal),
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        Some(_) => return Err(fail("decision occurrence key must be a nonempty string")),
    };
    let mut row = Map::new();
    row.insert("url".into(), item["url"].clone());
    row.insert("reason".into(), item["reason"].clone());
    row.insert("source".into(), item["source"].clone());
    row.insert("depth".into(), depth);
    row.insert("occurrence_key".into(), json!(occurrence));
    Ok(row)
}

/// The bounded, checked inputs of one page commit and the digest that identifies them.
struct Prepared {
    links: Vec<Map<String, Value>>,
    forms: Vec<Map<String, Value>>,
    decisions: Vec<Map<String, Value>>,
    discovered: Vec<(String, i64)>,
    query_reservations: Vec<(String, String, String)>,
    candidates: Vec<Map<String, Value>>,
    partial_reasons: Vec<String>,
    context: Vec<Map<String, Value>>,
    route_observations: Vec<Map<String, Value>>,
    resources: Vec<Map<String, Value>>,
    content_capture: Option<Map<String, Value>>,
    runtime: Map<String, Value>,
    body_bytes: i64,
    digest: String,
}

impl Prepared {
    fn new(commit: &PageCommit, lease: &Lease) -> Result<Self, StoreError> {
        if commit.captures.len() > 1000 {
            return Err(fail("too many response observations in one page commit"));
        }
        let mut capture_metadata = Vec::new();
        let (mut body_bytes, mut metadata_bytes) = (0_i64, 0_i64);
        for event in &commit.captures {
            let mut item = serde_json::to_value(event)?;
            let data = event.entity_bytes.as_deref();
            body_bytes += data.map_or(0, |d| d.len() as i64);
            if let Some(map) = item.as_object_mut() {
                map.remove("entity_bytes");
                map.insert(
                    "entity_sha256".into(),
                    data.map_or(Value::Null, |d| {
                        Value::String(
                            Sha256::digest(d)
                                .iter()
                                .map(|b| format!("{b:02x}"))
                                .collect(),
                        )
                    }),
                );
            }
            metadata_bytes += json_chunks(&item, MAX_RECORD_BYTES)?
                .iter()
                .map(|c| c.len() as i64)
                .sum::<i64>();
            if body_bytes > 8 * MAX_RECORD_BYTES || metadata_bytes > MAX_RECORD_BYTES {
                return Err(fail(
                    "native page response observations exceed the atomic input budget",
                ));
            }
            capture_metadata.push(item);
        }
        let resources = bounded_maps(
            &commit.resources,
            "resource declarations",
            MAX_EDGES_PER_PAGE,
        )?;
        let candidates = bounded_maps(
            &commit.candidates,
            "ordered discovery candidates",
            MAX_PAGE_COMMIT_ITEMS,
        )?;
        let reasons: Vec<Value> = commit.partial_reasons.iter().map(|r| json!(r)).collect();
        let partial_reasons: Vec<String> = bounded_items(&reasons, "partial reasons", 16)?
            .into_iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        let capture = match &commit.content_capture {
            Some(map) => content_capture(&Value::Object(map.clone()))?,
            None => None,
        };
        json_chunks(&Value::Object(commit.record.clone()), MAX_RECORD_BYTES)?;
        let links = bounded_maps(&commit.links, "links", MAX_EDGES_PER_PAGE)?;
        let forms = bounded_maps(&commit.forms, "forms", MAX_PAGE_COMMIT_ITEMS)?;
        let decisions = bounded_maps(&commit.decisions, "decisions", MAX_PAGE_COMMIT_ITEMS)?;
        let discovered_values: Vec<Value> = commit
            .discovered
            .iter()
            .map(|(url, depth)| json!([url, depth]))
            .collect();
        bounded_items(
            &discovered_values,
            "discovered frontier entries",
            MAX_PAGE_COMMIT_ITEMS,
        )?;
        let reservation_values: Vec<Value> = commit
            .query_reservations
            .iter()
            .map(|(a, b, c)| json!([a, b, c]))
            .collect();
        bounded_items(
            &reservation_values,
            "query reservations",
            MAX_PAGE_COMMIT_ITEMS,
        )?;
        let context = bounded_maps(&commit.context, "context items", MAX_PAGE_COMMIT_ITEMS)?;
        let route_observations = bounded_maps(
            &commit.route_observations,
            "rendered route observations",
            MAX_EDGES_PER_PAGE,
        )?;
        let runtime = commit.runtime.clone().unwrap_or_default();
        let mut payload = Map::new();
        payload.insert(
            "lease".into(),
            json!({"url_id": lease.url_id, "url": lease.url, "depth": lease.depth, "queue_ordinal": lease.queue_ordinal}),
        );
        payload.insert("record".into(), Value::Object(commit.record.clone()));
        let objects = |items: &[Map<String, Value>]| {
            Value::Array(items.iter().cloned().map(Value::Object).collect())
        };
        payload.insert("links".into(), objects(&links));
        payload.insert("forms".into(), objects(&forms));
        payload.insert("decisions".into(), objects(&decisions));
        payload.insert("discovered".into(), Value::Array(discovered_values));
        payload.insert(
            "query_reservations".into(),
            Value::Array(reservation_values),
        );
        payload.insert("runtime".into(), Value::Object(runtime.clone()));
        payload.insert("context".into(), objects(&context));
        payload.insert("route_observations".into(), objects(&route_observations));
        payload.insert(
            "content_capture".into(),
            match &capture {
                Some(map) => Value::String(
                    Sha256::digest(dump(&Value::Object(map.clone())).as_bytes())
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect(),
                ),
                None => Value::Null,
            },
        );
        if !capture_metadata.is_empty() {
            payload.insert("captures".into(), Value::Array(capture_metadata));
        }
        if let Some(state) = &commit.resource_inventory_state {
            payload.insert(
                "resources".into(),
                json!({
                    "declarations": objects(&resources),
                    "state": state,
                    "omitted": commit.resources_omitted,
                }),
            );
        }
        if !candidates.is_empty() || !partial_reasons.is_empty() {
            payload.insert("candidates".into(), objects(&candidates));
            payload.insert("partial_reasons".into(), json!(partial_reasons));
        }
        let digest = digest(&Value::Object(payload))?;
        Ok(Self {
            links,
            forms,
            decisions,
            discovered: commit.discovered.clone(),
            query_reservations: commit.query_reservations.clone(),
            candidates,
            partial_reasons,
            context,
            route_observations,
            resources,
            content_capture: capture,
            runtime,
            body_bytes,
            digest,
        })
    }
}
