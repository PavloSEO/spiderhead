//! Content-evidence lane of the native scan writer.
//!
//! Response/render body retention, capture preflight (disk reserves, no-store
//! acknowledgement, anonymous-session tracking) and the bounded collector input
//! validation behind `commit_render`.
//!
//! `NativeScanBodies` is implemented for the concrete `NativeScan` (bottom of this file) and stays
//! a trait so tests can fake the host. `RenderHost` supplies calls into other modules
//! (`store_rendered_document`, `store_response`, `put_declarations`, `apply_candidates`, ...);
//! it is still a test seam only.

#![allow(clippy::too_many_arguments, clippy::type_complexity)]

use crate::error::StoreError;
use crate::scan_artifact::{dump, insert, ScanError, MAX_RECORD_BYTES};
use crate::scan_lease::{Lease, MAX_EDGES_PER_PAGE};
use crate::scan_links::{put_discovery_ledger, put_resource_graph};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::path::Path;

/// Keys a content capture must carry, sorted for a stable order.
pub const CONTENT_CAPTURE_FIELDS: &[&str] = &[
    "canonical_target",
    "extraction_rules",
    "html",
    "indexable",
    "parsed",
    "settings",
    "unavailable_reason",
];

fn scan_err(message: &str) -> StoreError {
    StoreError::ScanError(ScanError {
        message: message.into(),
    })
}

/// Validate the bounded collector input used to create document contexts.
pub fn content_capture(value: &Value) -> Result<Option<Map<String, Value>>, StoreError> {
    if value.is_null() {
        return Ok(None);
    }
    let fields: BTreeSet<&str> = CONTENT_CAPTURE_FIELDS.iter().copied().collect();
    let object = value
        .as_object()
        .filter(|o| o.keys().map(String::as_str).collect::<BTreeSet<_>>() == fields)
        .ok_or_else(|| scan_err("content evidence capture has unsupported fields"))?;
    if !(object["html"].is_null() || object["html"].is_string()) {
        return Err(scan_err(
            "content evidence HTML must be text or unavailable",
        ));
    }
    if !(object["parsed"].is_null() || object["parsed"].is_object()) {
        return Err(scan_err(
            "content evidence parser result must be a mapping or unavailable",
        ));
    }
    if !object["settings"].is_object() {
        return Err(scan_err("content evidence settings must be a mapping"));
    }
    if !(object["indexable"].is_null() || object["indexable"].is_boolean()) {
        return Err(scan_err(
            "content evidence indexability must be boolean or unavailable",
        ));
    }
    if !object["canonical_target"].is_string() || !object["unavailable_reason"].is_string() {
        return Err(scan_err("content evidence text fields must be strings"));
    }
    Ok(Some(object.clone()))
}

/// Seam for the content-evidence capture and the context store
/// (not implemented here yet): derive the body-free evidence items and store them.
pub trait ContentEvidenceSink {
    /// Derive and store the content-evidence context for one captured document.
    fn put_content_evidence(
        &self,
        con: &Connection,
        page_url_id: i64,
        source_document_id: i64,
        representation: &str,
        content_capture: &Map<String, Value>,
    ) -> Result<(), StoreError>;
}

/// Derive body-free evidence only after the bound document exists.
pub fn put_content_evidence(
    con: &Connection,
    sink: &dyn ContentEvidenceSink,
    page_url_id: i64,
    source_document_id: Option<i64>,
    representation: &str,
    content_capture: Option<&Map<String, Value>>,
) -> Result<(), StoreError> {
    let Some(capture) = content_capture else {
        return Ok(());
    };
    let Some(document_id) = source_document_id else {
        return Err(scan_err(
            "content evidence requires a stored source document",
        ));
    };
    sink.put_content_evidence(con, page_url_id, document_id, representation, capture)
}

/// One captured HTTP response observation (the fields `commit_render` reads).
#[derive(Debug, Clone, Default)]
pub struct CaptureEvent {
    /// The cookie jar changed while the response was captured.
    pub session_changed: bool,
    /// Decoded entity bytes, when retained.
    pub entity_bytes: Option<Vec<u8>>,
}

fn context_item(kind: &str, payload: &Value) -> Map<String, Value> {
    let mut item = Map::new();
    item.insert("kind".into(), json!(kind));
    item.insert("item_key".into(), json!("run"));
    item.insert("payload_version".into(), json!("scan_context.v1"));
    item.insert("payload_json".into(), json!(dump(payload)));
    item.insert("completeness".into(), json!("complete"));
    item.insert("reason".into(), json!(""));
    item
}

/// Mixin of `NativeScan`: body retention, capture preflight and render commits.
pub trait NativeScanBodies {
    /// The writer's open connection.
    fn con(&self) -> &Connection;
    /// Path of the scan file (its parent directory is checked for free space).
    fn path(&self) -> &Path;
    /// Free bytes on the volume holding the scan.
    fn free_disk_bytes(&self) -> Result<i64, StoreError>;
    /// Validate and store one context item.
    fn put_context(&self, item: &Map<String, Value>) -> Result<(), StoreError>;
    /// Open the writer transaction.
    fn begin(&self) -> Result<(), StoreError>;
    /// Roll the writer transaction back.
    fn rollback(&self);

    /// Note an anonymous session when any capture changed the cookie jar.
    fn record_session_change(&self, captures: &[CaptureEvent]) -> Result<(), StoreError> {
        if captures.iter().any(|e| e.session_changed) {
            self.put_anonymous_session()?;
        }
        Ok(())
    }

    /// Record that the server set a cookie during an anonymous run.
    fn put_anonymous_session(&self) -> Result<(), StoreError> {
        self.put_context(&context_item(
            "anonymous_session",
            &json!({"server_set_cookie": true}),
        ))
    }

    /// Record a fresh anonymous cookie jar without retaining cookie data.
    /// `inferred` defaults to false.
    fn record_anonymous_session_resume(&self, inferred: bool) -> Result<(), StoreError> {
        self.begin()?;
        let body = || -> Result<(), StoreError> {
            if inferred {
                self.put_anonymous_session()?;
            }
            self.put_context(&context_item(
                "anonymous_session_resume",
                &json!({"fresh_session": true}),
            ))?;
            self.con().execute_batch("COMMIT")?;
            Ok(())
        };
        body().inspect_err(|_| self.rollback())
    }

    /// Whether the operator acknowledged retaining `no-store` responses.
    fn retain_no_store_acknowledged(&self) -> Result<bool, StoreError> {
        let text: String = self
            .con()
            .query_row("SELECT config_json FROM scan", [], |r| r.get(0))?;
        let config: Value = serde_json::from_str(&text)?;
        Ok(config["evidence"]["retain_no_store_acknowledged"] == json!(true))
    }

    /// Check the configured free-space reserve before scheduling a request.
    fn preflight_capture(&self) -> Result<(), StoreError> {
        let text: String = self
            .con()
            .query_row("SELECT retention_json FROM scan", [], |r| r.get(0))?;
        let policy: Value = serde_json::from_str(&text)?;
        let reserve = policy["min_free_bytes"].as_i64().unwrap_or(0);
        let needed = reserve.checked_add(MAX_RECORD_BYTES).unwrap_or(i64::MAX);
        if self.free_disk_bytes()? < needed {
            return Err(scan_err(
                "insufficient free disk for native capture; no request was started",
            ));
        }
        Ok(())
    }

    /// Check room for one atomic response capture (body written up to three times).
    fn check_capture_disk_space(
        &self,
        policy: &Map<String, Value>,
        body_bytes: i64,
    ) -> Result<(), StoreError> {
        let reserve = policy
            .get("min_free_bytes")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let needed = reserve
            .checked_add(MAX_RECORD_BYTES)
            .and_then(|n| body_bytes.checked_mul(3).and_then(|b| n.checked_add(b)))
            .unwrap_or(i64::MAX);
        if self.free_disk_bytes()? < needed {
            return Err(scan_err(
                "insufficient free disk for the next atomic response capture",
            ));
        }
        Ok(())
    }
}

/// Everything `commit_render` needs from other modules and the writer's own lanes.
pub trait RenderHost: NativeScanBodies + ContentEvidenceSink {
    /// Refuse writes on a closed or finished scan.
    fn assert_mutable(&self) -> Result<(), StoreError>;
    /// Crash-injection hook for tests.
    fn hit(&self, name: &str);
    /// Store the rendered DOM as a document and return its id.
    fn store_rendered_document(
        &self,
        logical_url: &str,
        html: &[u8],
        renderer: &Map<String, Value>,
        policy: &Map<String, Value>,
        retain_no_store: bool,
        captured_at: &str,
        body_state: &str,
        body_reason: &str,
    ) -> Result<i64, StoreError>;
    /// Store one captured legacy-fragment response and return its document id.
    fn store_response(
        &self,
        event: &CaptureEvent,
        policy: &Map<String, Value>,
        retain_no_store: bool,
        logical_url: &str,
        renderer: &Map<String, Value>,
    ) -> Result<Option<i64>, StoreError>;
    /// Rebuild the page row for an accepted record, without `url_id`.
    fn page_row(
        &self,
        record: &Map<String, Value>,
        lease: &Lease,
    ) -> Result<Map<String, Value>, StoreError>;
    /// Write the observed links and forms.
    fn write_observations(
        &self,
        lease: &Lease,
        document_id: Option<i64>,
        representation: &str,
        links: &[Map<String, Value>],
        forms: &[Map<String, Value>],
    ) -> Result<(), StoreError>;
    /// Store resource declarations.
    fn put_declarations(
        &self,
        lease: &Lease,
        document_id: Option<i64>,
        representation: &str,
        declarations: &[Map<String, Value>],
        fetch_enabled: bool,
        inventory_state: &str,
        omitted: i64,
    ) -> Result<(), StoreError>;
    /// Queue discovery candidates.
    fn apply_candidates(
        &self,
        candidates: &[Map<String, Value>],
        source: &str,
        queue_ordinal: i64,
        limit: i64,
    ) -> Result<(), StoreError>;
    /// Rendered-route context items.
    fn route_context_items(
        &self,
        page_url_id: i64,
        document_id: Option<i64>,
        coverage: &Map<String, Value>,
        observations: &[Map<String, Value>],
    ) -> Result<Vec<Map<String, Value>>, StoreError>;
    /// Current runtime counters.
    fn runtime(&self) -> Result<Map<String, Value>, StoreError>;
    /// Persist runtime counters.
    fn write_runtime(&self, runtime: &Map<String, Value>, max_depth: i64)
        -> Result<(), StoreError>;
    /// Record partial-capture reasons.
    fn partial_reasons(&self, reasons: &[String]) -> Result<(), StoreError>;
    /// Refresh the corpus-partial flag.
    fn sync_corpus(&self) -> Result<(), StoreError>;
    /// Append a scan event.
    fn event(&self, kind: &str, payload: &Value);
    /// Emit the coverage event.
    fn event_coverage(&self);
    /// Query cap used by stored frontier queries.
    fn stored_query_limit(&self) -> Result<i64, StoreError>;
}

/// Inputs of one `commit_render` call; defaults come from `Default`.
#[derive(Debug, Clone)]
pub struct RenderCommit<'a> {
    /// Logical page URL.
    pub url: &'a str,
    /// Accepted page record, when the render was accepted.
    pub record: Option<&'a Map<String, Value>>,
    /// Serialized DOM or raw bytes.
    pub html: Option<&'a [u8]>,
    /// Renderer description.
    pub renderer: &'a Map<String, Value>,
    /// Capture instant.
    pub captured_at: &'a str,
    /// Observed links.
    pub links: &'a [Map<String, Value>],
    /// Observed forms.
    pub forms: &'a [Map<String, Value>],
    /// `rendered` or `legacy_fragment`.
    pub representation: &'a str,
    /// Document body state.
    pub body_state: &'a str,
    /// Document body reason.
    pub body_reason: &'a str,
    /// Captured responses (legacy fragments only).
    pub captures: &'a [CaptureEvent],
    /// Partial-capture reasons.
    pub partial_reasons: &'a [String],
    /// Resource declarations.
    pub resources: &'a [Map<String, Value>],
    /// Resource inventory state, when one was measured.
    pub resource_inventory_state: Option<&'a str>,
    /// Declarations omitted by the bound.
    pub resources_omitted: i64,
    /// Total elapsed seconds, when measured.
    pub elapsed_seconds: Option<f64>,
    /// Rendered-route observations.
    pub route_observations: &'a [Map<String, Value>],
    /// Route coverage record.
    pub route_coverage: Option<&'a Map<String, Value>>,
    /// Validated content capture.
    pub content_capture: Option<&'a Map<String, Value>>,
    /// Discovery candidates.
    pub candidates: &'a [Map<String, Value>],
    /// Discovery decisions.
    pub decisions: &'a [Map<String, Value>],
}

fn bounded<T: serde::Serialize>(items: &[T], label: &str, limit: usize) -> Result<(), StoreError> {
    let mut size = 0usize;
    for (index, item) in items.iter().enumerate() {
        size += serde_json::to_string(item)?.len();
        if index == limit || size as i64 > MAX_RECORD_BYTES {
            return Err(scan_err(&format!(
                "{label} exceeds the bounded per-page limit ({limit} items / 8 MiB)"
            )));
        }
    }
    Ok(())
}

fn valid_decision(decision: &Map<String, Value>) -> bool {
    let keys: BTreeSet<&str> = decision.keys().map(String::as_str).collect();
    keys == BTreeSet::from(["depth", "reason", "source", "url"])
        && ["url", "reason", "source"]
            .iter()
            .all(|k| decision[*k].as_str().is_some_and(|s| !s.is_empty()))
        && decision["depth"].as_i64().is_some_and(|d| d >= 0)
}

/// Retain one render attempt and its accepted extraction atomically; returns the document id.
pub fn commit_render<H: RenderHost>(
    host: &H,
    commit: &RenderCommit<'_>,
) -> Result<Option<i64>, StoreError> {
    host.assert_mutable()?;
    host.preflight_capture()?;
    if !matches!(commit.representation, "rendered" | "legacy_fragment") {
        return Err(scan_err("unsupported rendered representation"));
    }
    let edge_limit = MAX_EDGES_PER_PAGE as usize;
    bounded(commit.links, "rendered links", edge_limit)?;
    bounded(commit.forms, "rendered forms", 2000)?;
    bounded(commit.partial_reasons, "render partial reasons", 16)?;
    bounded(commit.resources, "render resource declarations", edge_limit)?;
    bounded(
        commit.route_observations,
        "rendered route observations",
        edge_limit,
    )?;
    bounded(
        commit.candidates,
        "rendered discovery candidates",
        edge_limit,
    )?;
    bounded(commit.decisions, "rendered discovery decisions", edge_limit)?;
    let entity_total: usize = commit
        .captures
        .iter()
        .map(|e| e.entity_bytes.as_ref().map_or(0, Vec::len))
        .sum();
    if commit.captures.len() > 1000 || entity_total as i64 > 8 * MAX_RECORD_BYTES {
        return Err(scan_err(
            "render response observations exceed the atomic input budget",
        ));
    }
    if commit
        .html
        .is_some_and(|h| h.len() as i64 > 8 * MAX_RECORD_BYTES)
    {
        return Err(scan_err(
            "rendered document exceeds the atomic input budget",
        ));
    }
    bounded(&[commit.renderer], "renderer", 1)?;
    if let Some(record) = commit.record {
        bounded(&[record], "record", 1)?;
    }
    host.begin()?;
    let result = commit_render_inner(host, commit);
    if result.is_err() {
        host.rollback();
    }
    result
}

fn commit_render_inner<H: RenderHost>(
    host: &H,
    c: &RenderCommit<'_>,
) -> Result<Option<i64>, StoreError> {
    let con = host.con();
    let page = con
        .query_row(
            "SELECT p.url_id,p.crawl_depth,p.page_ordinal,f.queue_ordinal FROM pages p \
             JOIN urls u USING(url_id) JOIN frontier f USING(url_id) WHERE u.url=?",
            params![c.url],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| scan_err("rendered document has no committed static page"))?;
    let (url_id, crawl_depth, page_ordinal, queue_ordinal) = page;
    let policy_text: String = con.query_row("SELECT retention_json FROM scan", [], |r| r.get(0))?;
    let policy: Map<String, Value> = serde_json::from_str(&policy_text)?;
    let body_bytes = if c.representation == "rendered" {
        c.html.map_or(0, <[u8]>::len)
    } else {
        c.captures
            .iter()
            .map(|e| e.entity_bytes.as_ref().map_or(0, Vec::len))
            .sum()
    } as i64;
    host.check_capture_disk_space(&policy, body_bytes)?;
    let retain = host.retain_no_store_acknowledged()?;
    let document_id: Option<i64>;
    if c.representation == "rendered" {
        if !c.captures.is_empty() {
            return Err(scan_err(
                "serialized DOM must not masquerade as an HTTP response",
            ));
        }
        document_id = Some(host.store_rendered_document(
            c.url,
            c.html.unwrap_or_default(),
            c.renderer,
            &policy,
            retain,
            c.captured_at,
            c.body_state,
            c.body_reason,
        )?);
    } else {
        if c.captures.is_empty() {
            return Err(scan_err(
                "legacy fragment requires a captured navigation response",
            ));
        }
        let mut last = None;
        for event in c.captures {
            last = host.store_response(event, &policy, retain, c.url, c.renderer)?;
        }
        document_id = last;
        host.record_session_change(c.captures)?;
    }
    host.hit("after_render_body");
    let lease = Lease {
        url_id,
        url: c.url.to_string(),
        depth: crawl_depth,
        queue_ordinal,
    };
    if let Some(record) = c.record {
        if record.get("representation").and_then(Value::as_str) != Some(c.representation) {
            return Err(scan_err(
                "rendered page representation differs from its document",
            ));
        }
        let mut values = host.page_row(record, &lease)?;
        values.shift_remove("url_id");
        values.insert("page_ordinal".into(), json!(page_ordinal));
        values.insert("document_id".into(), json!(document_id));
        let assignments: Vec<String> = values.keys().map(|k| format!("{k}=?")).collect();
        let args: Vec<rusqlite::types::Value> = values.values().map(json_to_sql).collect();
        con.execute(
            &format!("UPDATE pages SET {} WHERE url_id=?", assignments.join(",")),
            rusqlite::params_from_iter(args.into_iter().chain([url_id.into()])),
        )?;
        con.execute(
            "DELETE FROM links WHERE source_url_id=? AND evidence_representation=?",
            params![url_id, c.representation],
        )?;
        con.execute(
            "DELETE FROM forms WHERE page_url_id=? AND evidence_representation=?",
            params![url_id, c.representation],
        )?;
        host.write_observations(&lease, document_id, c.representation, c.links, c.forms)?;
        if let Some(state) = c.resource_inventory_state {
            let text: String = con.query_row("SELECT config_json FROM scan", [], |r| r.get(0))?;
            let config: Value = serde_json::from_str(&text)?;
            host.put_declarations(
                &lease,
                document_id,
                c.representation,
                c.resources,
                crate::discovery_ledger::truthy(config.get("resources").map(|r| &r["fetch"])),
                state,
                c.resources_omitted,
            )?;
        }
    } else if !c.links.is_empty() || !c.forms.is_empty() {
        return Err(scan_err(
            "unaccepted render cannot replace graph observations",
        ));
    }
    if !c.candidates.is_empty() || !c.decisions.is_empty() {
        for (index, decision) in c.decisions.iter().enumerate() {
            if !valid_decision(decision) {
                return Err(scan_err("rendered discovery decision is invalid"));
            }
            let mut row = decision.clone();
            row.insert(
                "occurrence_key".into(),
                json!(format!("{queue_ordinal}:rendered:{index}")),
            );
            insert(con, "decisions", &row)?;
        }
        host.apply_candidates(
            c.candidates,
            c.url,
            queue_ordinal,
            host.stored_query_limit()?,
        )?;
    }
    if let Some(capture) = c.content_capture {
        put_content_evidence(
            con,
            host,
            url_id,
            document_id,
            c.representation,
            Some(capture),
        )?;
    }
    put_resource_graph(
        con,
        url_id,
        document_id,
        c.representation,
        c.content_capture,
    )?;
    put_discovery_ledger(
        con,
        url_id,
        document_id,
        c.representation,
        c.url,
        crawl_depth,
        c.content_capture,
        c.links,
        c.candidates,
        c.decisions,
        c.forms,
    )?;
    if let Some(coverage) = c.route_coverage {
        for item in host.route_context_items(url_id, document_id, coverage, c.route_observations)? {
            host.put_context(&item)?;
        }
    }
    host.hit("after_render_page");
    if let Some(elapsed) = c.elapsed_seconds {
        let mut runtime = host.runtime()?;
        let before = runtime
            .get("elapsed_seconds")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        if elapsed < before {
            return Err(scan_err("render elapsed time cannot go backwards"));
        }
        runtime.insert("elapsed_seconds".into(), json!(elapsed));
        let depth = runtime
            .get("max_depth_reached")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        host.write_runtime(&runtime, depth)?;
    }
    host.partial_reasons(c.partial_reasons)?;
    let bump = if c.representation == "legacy_fragment" {
        c.captures.len() as i64
    } else {
        1
    };
    con.execute(
        "UPDATE scan SET evidence_revision=evidence_revision+?",
        params![bump],
    )?;
    host.sync_corpus()?;
    host.event(
        "render",
        &json!({"url_id": url_id, "representation": c.representation, "state": "stored"}),
    );
    host.event_coverage();
    host.hit("before_render_commit");
    con.execute_batch("COMMIT")?;
    Ok(document_id)
}

fn json_to_sql(value: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as Sql;
    match value {
        Value::Null => Sql::Null,
        Value::Bool(b) => Sql::Integer(*b as i64),
        Value::Number(n) => n
            .as_i64()
            .map(Sql::Integer)
            .unwrap_or_else(|| Sql::Real(n.as_f64().unwrap_or(0.0))),
        Value::String(s) => Sql::Text(s.clone()),
        other => Sql::Text(dump(other)),
    }
}

/// The real writer: the trait's hooks map onto the concrete `NativeScan` (the trait stays a seam
/// so tests can fake the host).
impl NativeScanBodies for crate::native_scan::NativeScan {
    fn con(&self) -> &Connection {
        crate::native_scan::NativeScan::con(self)
    }

    fn path(&self) -> &Path {
        crate::native_scan::NativeScan::path(self)
    }

    fn free_disk_bytes(&self) -> Result<i64, StoreError> {
        let dir = self.path().parent().unwrap_or_else(|| Path::new("."));
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        let free = spiderhead_core::filesystem::free_space(dir)?;
        Ok(i64::try_from(free).unwrap_or(i64::MAX))
    }

    fn put_context(&self, item: &Map<String, Value>) -> Result<(), StoreError> {
        crate::native_context::put_context(NativeScanBodies::con(self), item, None)
    }

    fn begin(&self) -> Result<(), StoreError> {
        crate::native_scan::NativeScan::begin(self, true)
    }

    fn rollback(&self) {
        crate::native_scan::NativeScan::rollback(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn full_capture() -> Value {
        json!({
            "html": "<p>x</p>", "parsed": null, "settings": {}, "indexable": true,
            "canonical_target": "", "unavailable_reason": "", "extraction_rules": [],
        })
    }

    #[test]
    fn content_capture_accepts_exact_shape_only() {
        assert!(content_capture(&Value::Null).unwrap().is_none());
        assert!(content_capture(&full_capture()).unwrap().is_some());
        let mut extra = full_capture();
        extra["x"] = json!(1);
        assert!(content_capture(&extra).is_err());
        let mut bad = full_capture();
        bad["settings"] = json!([]);
        assert!(content_capture(&bad).is_err());
        let mut bad = full_capture();
        bad["indexable"] = json!("yes");
        assert!(content_capture(&bad).is_err());
        let mut bad = full_capture();
        bad["canonical_target"] = json!(null);
        assert!(content_capture(&bad).is_err());
    }

    struct Host {
        con: Connection,
        free: i64,
        contexts: Cell<usize>,
    }
    impl NativeScanBodies for Host {
        fn con(&self) -> &Connection {
            &self.con
        }
        fn path(&self) -> &Path {
            Path::new("/tmp/x.sqlite")
        }
        fn free_disk_bytes(&self) -> Result<i64, StoreError> {
            Ok(self.free)
        }
        fn put_context(&self, _: &Map<String, Value>) -> Result<(), StoreError> {
            self.contexts.set(self.contexts.get() + 1);
            Ok(())
        }
        fn begin(&self) -> Result<(), StoreError> {
            Ok(())
        }
        fn rollback(&self) {}
    }

    fn host(free: i64) -> Host {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(
            "CREATE TABLE scan(config_json TEXT,retention_json TEXT);
             INSERT INTO scan VALUES('{\"evidence\":{\"retain_no_store_acknowledged\":true}}','{\"min_free_bytes\":1000}');",
        )
        .unwrap();
        Host {
            con,
            free,
            contexts: Cell::new(0),
        }
    }

    #[test]
    fn preflight_checks_reserve() {
        assert!(host(1000 + MAX_RECORD_BYTES).preflight_capture().is_ok());
        assert!(host(999 + MAX_RECORD_BYTES).preflight_capture().is_err());
        assert!(host(0).check_capture_disk_space(&Map::new(), 1).is_err());
    }

    #[test]
    fn no_store_flag_and_session_notes() {
        let h = host(0);
        assert!(h.retain_no_store_acknowledged().unwrap());
        h.record_session_change(&[CaptureEvent::default()]).unwrap();
        assert_eq!(h.contexts.get(), 0);
        h.record_session_change(&[CaptureEvent {
            session_changed: true,
            entity_bytes: None,
        }])
        .unwrap();
        assert_eq!(h.contexts.get(), 1);
    }

    #[test]
    fn put_content_evidence_requires_document() {
        struct Sink;
        impl ContentEvidenceSink for Sink {
            fn put_content_evidence(
                &self,
                _: &Connection,
                _: i64,
                _: i64,
                _: &str,
                _: &Map<String, Value>,
            ) -> Result<(), StoreError> {
                Ok(())
            }
        }
        let con = Connection::open_in_memory().unwrap();
        let cap = content_capture(&full_capture()).unwrap();
        assert!(put_content_evidence(&con, &Sink, 1, None, "static", cap.as_ref()).is_err());
        assert!(put_content_evidence(&con, &Sink, 1, None, "static", None).is_ok());
        assert!(put_content_evidence(&con, &Sink, 1, Some(2), "static", cap.as_ref()).is_ok());
    }
}
