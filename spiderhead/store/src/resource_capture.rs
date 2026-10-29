//! Fetch pass over pending scan.v2 resource declarations, under graph-specific budgets.
//!
//! The network sits behind [`ResourceFetcher`]; this module owns budgets, redirects, scope
//! checks and what is persisted.

use crate::crawl_records::spider::Scope;
use crate::error::StoreError;
use crate::ledger::urlparts::urlsplit;
use crate::resource_graph::{
    css_children_text, ensure_schema, fetch_integrity_state, image_dimensions,
    set_occurrence_state, store_css_children,
};
use indexmap::IndexMap;
use rusqlite::Connection;
use serde_json::Value;
use spiderhead_parse::py_url::urljoin;
use std::collections::{BTreeMap, BTreeSet};

/// One HTTP exchange as the resource graph needs it.
#[derive(Debug, Clone, Default)]
pub struct FetchedResource {
    /// Response status, `None` when no response arrived.
    pub status_code: Option<i64>,
    /// `Location` of a redirect, empty otherwise.
    pub redirect_url: String,
    /// `Content-Type` header value.
    pub content_type: String,
    /// Transport error text, empty on success.
    pub error: String,
    /// Cache disposition reported by the transport.
    pub cache_status: String,
    /// `Content-Encoding` of the entity, empty for identity.
    pub content_encoding: String,
    /// Entity bytes retained for this exchange (bounded by the request cap).
    pub body: Option<Vec<u8>>,
    /// Whether `body` is the whole entity.
    pub body_complete: bool,
    /// Response headers, in arrival order.
    pub headers: Vec<(String, String)>,
}

/// Why a fetch produced no exchange.
#[derive(Debug)]
pub enum FetchFailure {
    /// The scan's total request budget is spent; the pass stops.
    RequestBudgetExhausted,
    /// Any other failure of this one URL.
    Failed(String),
}

/// Network and delivery-analysis seam, implemented by the crawler.
pub trait ResourceFetcher {
    /// GET `url` without following redirects, retaining at most `max_bytes` of entity.
    fn fetch(&mut self, url: &str, max_bytes: i64) -> Result<FetchedResource, FetchFailure>;
    /// JSON text describing transfer and caching of a fetched body of the given declared kind.
    fn delivery_json(&self, body: &[u8], kind: &str, headers: &[(String, String)]) -> String;
}

/// Counters of one pass.
pub type Totals = IndexMap<String, i64>;

fn zero_totals() -> Totals {
    ["stored", "fetched", "excluded", "failed", "budget"]
        .iter()
        .map(|k| ((*k).to_owned(), 0))
        .collect()
}

/// One pending declaration.
struct Pending {
    values: IndexMap<String, Value>,
    kind: String,
    url: String,
    depth: i64,
}

fn next_pending(con: &Connection) -> Result<Option<Pending>, StoreError> {
    let mut stmt = con.prepare(
        "SELECT occurrence_id,page_url_id,source_document_id,representation,kind,resolved_url,nesting_depth \
         FROM resource_graph_occurrences WHERE state='disabled' ORDER BY occurrence_id LIMIT 1",
    )?;
    let mut rows = stmt.query([])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let mut values = IndexMap::new();
    for (i, name) in ["occurrence_id", "page_url_id", "source_document_id"]
        .iter()
        .enumerate()
    {
        values.insert((*name).to_owned(), Value::from(row.get::<_, i64>(i)?));
    }
    values.insert(
        "representation".into(),
        Value::from(row.get::<_, String>(3)?),
    );
    let kind: String = row.get(4)?;
    let url: String = row.get(5)?;
    let depth: i64 = row.get(6)?;
    values.insert("resolved_url".into(), Value::from(url.clone()));
    values.insert("nesting_depth".into(), Value::from(depth));
    Ok(Some(Pending {
        values,
        kind,
        url,
        depth,
    }))
}

fn host_of(url: &str) -> String {
    urlsplit(url).hostname().unwrap_or_default().to_lowercase()
}

/// Integer limit `name` under `resources.graph`.
fn limit(graph: &Value, name: &str) -> i64 {
    graph.get(name).and_then(Value::as_i64).unwrap_or(0)
}

struct Budget {
    count: i64,
    bytes: i64,
    origins: BTreeSet<String>,
}

/// Reason the next request may not start, if any.
fn exhausted(graph: &Value, used: &Budget, host: &str, elapsed: f64) -> Option<&'static str> {
    if used.count >= limit(graph, "max_requests") {
        Some("resource graph request budget exhausted")
    } else if used.bytes >= limit(graph, "max_bytes") {
        Some("resource graph byte budget exhausted")
    } else if limit(graph, "max_seconds") != 0 && elapsed >= limit(graph, "max_seconds") as f64 {
        Some("resource graph wall-clock budget exhausted")
    } else if !used.origins.contains(host)
        && used.origins.len() as i64 >= limit(graph, "max_origins")
    {
        Some("resource graph origin budget exhausted")
    } else {
        None
    }
}

/// Everything learned from following one resource to its last hop.
struct Followed {
    last: Option<FetchedResource>,
    current: String,
    redirects: i64,
    received: i64,
    budget_reason: &'static str,
    scope_reason: String,
}

/// Settings shared by every URL of one pass.
struct Pass<'a> {
    graph: &'a Value,
    scope: &'a Scope,
    start_host: &'a str,
    started: f64,
}

fn follow(
    fetcher: &mut dyn ResourceFetcher,
    pass: &Pass<'_>,
    url: &str,
    used: &mut Budget,
    clock: &mut dyn FnMut() -> f64,
) -> Result<Followed, FetchFailure> {
    let mut f = Followed {
        last: None,
        current: url.to_owned(),
        redirects: 0,
        received: 0,
        budget_reason: "",
        scope_reason: String::new(),
    };
    loop {
        let host = host_of(&f.current);
        let elapsed = clock() - pass.started;
        if let Some(reason) = exhausted(pass.graph, used, &host, elapsed) {
            f.budget_reason = reason;
            break;
        }
        let cap = (limit(pass.graph, "max_bytes_per_resource") - f.received)
            .min(limit(pass.graph, "max_bytes") - used.bytes);
        if cap < 1 {
            f.budget_reason = "resource graph byte budget exhausted";
            break;
        }
        let got = fetcher.fetch(&f.current, cap)?;
        used.count += 1;
        let hop_bytes = got.body.as_ref().map_or(0, |b| b.len() as i64);
        f.received += hop_bytes;
        used.bytes += hop_bytes;
        used.origins.insert(host);
        let redirecting =
            !got.redirect_url.is_empty() && (300..400).contains(&got.status_code.unwrap_or(0));
        let location = got.redirect_url.clone();
        f.last = Some(got);
        if !redirecting {
            break;
        }
        let next = urljoin(&f.current, &location).unwrap_or(location);
        let reason = pass.scope.rejection(&next, pass.start_host);
        if !reason.is_empty() {
            f.scope_reason = reason;
            break;
        }
        if f.redirects >= limit(pass.graph, "max_redirects") {
            break;
        }
        f.redirects += 1;
        f.current = next;
    }
    Ok(f)
}

/// Fetch deduplicated v2 declarations under resource-graph-specific budgets.
///
/// Returns counters `stored`, `fetched`, `excluded`, `failed`, `budget`. `settings` is the
/// crawl settings document (`resources.fetch`, `resources.graph`, `scope`). `clock` is
/// monotonic seconds. Nothing happens outside a v2 artifact or when fetching is off.
pub fn capture(
    con: &Connection,
    settings: &Value,
    fetcher: &mut dyn ResourceFetcher,
    clock: &mut dyn FnMut() -> f64,
) -> Result<Totals, StoreError> {
    let mut totals = zero_totals();
    let version: i64 = con.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != 2 {
        return Ok(totals);
    }
    ensure_schema(con)?;
    let resources = settings.get("resources").cloned().unwrap_or(Value::Null);
    if !resources
        .get("fetch")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(totals);
    }
    let graph = resources.get("graph").cloned().unwrap_or(Value::Null);
    let start_url: Option<String> =
        con.query_row("SELECT start_url FROM scan WHERE singleton=1", [], |r| {
            r.get(0)
        })?;
    let start_host = host_of(&start_url.unwrap_or_default());
    let scope_map: Option<BTreeMap<String, Value>> = settings
        .get("scope")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
    let scope = Scope::from_config(scope_map.as_ref());
    let started = clock();
    let mut used = Budget {
        count: 0,
        bytes: 0,
        origins: BTreeSet::new(),
    };
    let bump = |totals: &mut Totals, key: &str| *totals.entry(key.to_owned()).or_insert(0) += 1;
    while let Some(row) = next_pending(con)? {
        let url = row.url.clone();
        let host = host_of(&url);
        let reason = scope.rejection(&url, &start_host);
        if !reason.is_empty() {
            set_occurrence_state(con, &url, "excluded", &reason)?;
            bump(&mut totals, "excluded");
            continue;
        }
        let elapsed = clock() - started;
        if exhausted(&graph, &used, &host, elapsed).is_some()
            || row.depth > limit(&graph, "max_nesting")
        {
            set_occurrence_state(con, &url, "budget", "resource graph budget exhausted")?;
            bump(&mut totals, "budget");
            continue;
        }
        let existing: Option<(String, String)> = {
            let mut stmt = con
                .prepare("SELECT state,reason FROM resource_graph_fetches WHERE resolved_url=?")?;
            let mut rows = stmt.query([&url])?;
            match rows.next()? {
                Some(r) => Some((r.get(0)?, r.get(1)?)),
                None => None,
            }
        };
        if let Some((state, reason)) = existing {
            set_occurrence_state(con, &url, &state, &reason)?;
            bump(&mut totals, "stored");
            continue;
        }
        let before = clock();
        let pass = Pass {
            graph: &graph,
            scope: &scope,
            start_host: &start_host,
            started,
        };
        let followed = follow(fetcher, &pass, &url, &mut used, clock);
        let followed = match followed {
            Ok(f) => f,
            Err(FetchFailure::RequestBudgetExhausted) => {
                set_occurrence_state(con, &url, "budget", "total HTTP request budget exhausted")?;
                bump(&mut totals, "budget");
                break;
            }
            Err(FetchFailure::Failed(message)) => {
                used.count += 1;
                let truncated: String = message.chars().take(500).collect();
                con.execute(
                    "INSERT INTO resource_graph_fetches(resolved_url,state,reason,status_code,content_type,bytes_received,elapsed_seconds,origin_host,redirects,nesting_depth,final_url,compression,cache_state,integrity_state,width,height,body_state) \
                     VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                    rusqlite::params![
                        url, "failed", truncated, None::<i64>, "", 0, None::<f64>, host, 0,
                        row.depth, "", "unknown", "unknown", fetch_integrity_state(con, &url)?,
                        None::<i64>, None::<i64>, "unavailable",
                    ],
                )?;
                set_occurrence_state(con, &url, "failed", "resource fetch failed")?;
                bump(&mut totals, "failed");
                continue;
            }
        };
        let record = followed.last.as_ref();
        let status = record.and_then(|r| r.status_code);
        let content_type = record.map(|r| r.content_type.clone()).unwrap_or_default();
        let body = record.and_then(|r| r.body.as_deref());
        let still_redirecting = record.is_some_and(|r| {
            !r.redirect_url.is_empty() && (300..400).contains(&r.status_code.unwrap_or(0))
        });
        let (state, reason) = if !followed.budget_reason.is_empty() {
            ("budget", followed.budget_reason.to_owned())
        } else if !followed.scope_reason.is_empty() {
            ("excluded", followed.scope_reason.clone())
        } else if still_redirecting {
            ("failed", "resource redirect budget exhausted".to_owned())
        } else if !status.is_some_and(|s| (200..300).contains(&s)) {
            let error = record.map(|r| r.error.clone()).unwrap_or_default();
            let text = if error.is_empty() {
                "resource response was not successful".to_owned()
            } else {
                error
            };
            ("failed", text)
        } else if status == Some(206) {
            (
                "partial",
                "HTTP 206 response contains only a requested byte range; whole resource was not measured".to_owned(),
            )
        } else if body.is_none() || !record.is_some_and(|r| r.body_complete) {
            (
                "partial",
                "resource response body was not retained completely".to_owned(),
            )
        } else {
            ("complete", String::new())
        };
        let (width, height) = image_dimensions(body)?;
        let compression = record
            .map(|r| r.content_encoding.as_str())
            .filter(|c| !c.is_empty())
            .unwrap_or("identity");
        let cache_state = record
            .map(|r| r.cache_status.as_str())
            .filter(|c| !c.is_empty())
            .unwrap_or("unknown");
        con.execute(
            "INSERT INTO resource_graph_fetches(resolved_url,state,reason,status_code,content_type,bytes_received,elapsed_seconds,origin_host,redirects,nesting_depth,final_url,compression,cache_state,integrity_state,width,height,body_state) \
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            rusqlite::params![
                url, state, reason, status, content_type, followed.received,
                Some(clock() - before), host, followed.redirects, row.depth, followed.current,
                compression, cache_state, fetch_integrity_state(con, &url)?, width, height,
                if state == "complete" { "complete" } else { "partial" },
            ],
        )?;
        set_occurrence_state(con, &url, state, &reason)?;
        if let (true, Some(r), Some(body)) = (state == "complete", record, body) {
            let delivery = fetcher.delivery_json(body, &row.kind, &r.headers);
            con.execute(
                "UPDATE resource_graph_fetches SET delivery_json=? WHERE resolved_url=?",
                (delivery, &url),
            )?;
        }
        bump(
            &mut totals,
            match state {
                "complete" => "fetched",
                "excluded" => "excluded",
                "budget" => "budget",
                _ => "failed",
            },
        );
        let is_css = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("text/css");
        if state == "complete" && is_css {
            let text = css_children_text(body.unwrap_or(&[]));
            store_css_children(con, &row.values, &text, limit(&graph, "max_nesting"))?;
        }
    }
    Ok(totals)
}
