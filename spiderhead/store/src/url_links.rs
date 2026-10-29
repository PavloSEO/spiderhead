//! Paged outgoing and incoming hyperlinks of one URL from a saved scan, read-only.
//!
//! The lookup is by URL, not by document id: the URL is normalised the way the crawler keys
//! pages and resolved through the unique `urls` index. The scan is opened by a light read-only
//! path (application id, a supported `user_version`, the `scan.v1`/`scan.v2` header and the
//! three tables this query reads); it never runs a full-artifact validation. Pages are produced
//! by index range scans with `LIMIT/OFFSET`; counts are capped and say so. Nothing is written.

#![allow(clippy::too_many_arguments, clippy::type_complexity)]

use crate::crawl_records::spider::Scope;
use crate::error::StoreError;
use crate::link_context::cell_value;
use crate::link_queries::{spellings, url as checked_url};
use crate::scan_artifact::APPLICATION_ID;
use rusqlite::types::Value as Sql;
use rusqlite::{params_from_iter, Connection};
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

/// Hard ceiling on rows per page.
pub const MAX_LIMIT: i64 = 200;
/// Largest accepted offset.
pub const MAX_OFFSET: i64 = 5000000;
/// A count past this is reported as a lower bound.
pub const COUNT_CAP: i64 = 1000000;
/// Rows a filtered/sorted request may examine before it reports a cap.
pub const SCAN_CAP: i64 = 100000;
/// Fragment spellings of one destination looked up at most.
pub const FRAGMENT_VARIANTS: i64 = 1000;

/// Link-type filter values.
pub const LINK_TYPES: &[&str] = &["all", "internal", "external"];
/// Follow filter values.
pub const FOLLOW: &[&str] = &["all", "follow", "nofollow", "sponsored", "ugc"];
/// Status-class filter values.
pub const STATUS_CLASSES: &[&str] = &[
    "all",
    "2xx",
    "3xx",
    "4xx",
    "5xx",
    "broken",
    "error",
    "unscanned",
];
/// Sort orders.
pub const SORTS: &[&str] = &["order", "url", "status"];
/// Directions.
pub const DIRECTIONS: &[&str] = &["out", "in"];
/// Representation filter values.
pub const REPRESENTATIONS: &[&str] = &[
    "all",
    "static",
    "rendered",
    "legacy_fragment",
    "legacy_unknown",
];

/// Columns each table must expose for this query.
pub const NEEDED: &[(&str, &[&str])] = &[
    (
        "scan",
        &[
            "capabilities_json",
            "config_json",
            "format_version",
            "scan_uuid",
            "start_url",
        ],
    ),
    ("urls", &["url", "url_id"]),
    ("pages", &["status_code", "url_id"]),
    (
        "links",
        &[
            "anchor",
            "destination_url_id",
            "evidence_representation",
            "link_id",
            "nofollow",
            "ordinal",
            "position",
            "raw_href",
            "rel_json",
            "source_url_id",
            "target",
        ],
    ),
];

/// Link column list of the paged selects.
pub const LINK_COLUMNS: &str = "l.link_id,l.source_url_id,l.destination_url_id,l.evidence_representation,l.ordinal,l.anchor,l.nofollow,l.position,l.rel_json,l.target,l.raw_href";

/// A refusal with a machine-readable `reason_code`.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct LinksError {
    /// Human-readable refusal.
    pub message: String,
    /// Machine-readable code.
    pub reason_code: String,
}

/// The time budget ended inside a statement.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct Budget {
    /// Text of the interrupt.
    pub message: String,
}

fn links_error(code: &str, message: impl Into<String>) -> StoreError {
    StoreError::LinksError(LinksError {
        message: message.into(),
        reason_code: code.into(),
    })
}

/// Turn a SQLite interrupt (progress handler fired) into the budget signal.
fn budget_or(error: rusqlite::Error) -> StoreError {
    if let rusqlite::Error::SqliteFailure(failure, _) = &error {
        if failure.code == rusqlite::ErrorCode::OperationInterrupted {
            return StoreError::Budget(Budget {
                message: error.to_string(),
            });
        }
    }
    StoreError::Sqlite(error)
}

/// Lower-cased host of a URL, with userinfo and port removed.
pub(crate) fn hostname_of(url: &str) -> Option<String> {
    let parts = spiderhead_parse::py_url::urlsplit(url).ok()?;
    let netloc = parts
        .netloc
        .rsplit_once('@')
        .map_or(parts.netloc.as_str(), |(_, h)| h);
    let host = if let Some(rest) = netloc.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        netloc.split(':').next().unwrap_or("")
    };
    (!host.is_empty()).then(|| host.to_lowercase())
}

/// The link column list, plus the anchor marker when this scan has it.
///
/// A scan written before #1518 has no `anchor_source` column; selecting it there would fail the
/// whole query, so the marker joins in only when present and the row builders then report
/// `None` -- unmeasured, not text-derived.
pub fn with_anchor_source(con: &Connection) -> Result<String, StoreError> {
    let mut stmt = con.prepare("PRAGMA table_info(links)")?;
    let names = stmt.query_map([], |r| r.get::<_, String>(1))?;
    let mut have = false;
    for name in names {
        have |= name? == "anchor_source";
    }
    Ok(format!(
        "{LINK_COLUMNS}{}",
        if have { ",l.anchor_source" } else { "" }
    ))
}

/// Read-only connection after the cheap identity checks; no `quick_check`, no row scans.
pub fn open_light(path: &Path) -> Result<Connection, StoreError> {
    if !path.is_file() {
        return Err(links_error(
            "scan_not_available",
            "scan file does not exist",
        ));
    }
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let wrap =
        |e: rusqlite::Error| links_error("scan_not_available", format!("cannot read scan: {e}"));
    let con = spiderhead_core::sqlite::open_readonly(
        &resolved,
        Duration::from_secs(5),
        &[
            "PRAGMA trusted_schema=OFF",
            "PRAGMA query_only=ON",
            "PRAGMA cache_size=-65536",
        ],
    )
    .map_err(wrap)?;
    con.execute_batch("BEGIN").map_err(wrap)?;
    let version: i64 = con
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(wrap)?;
    let app: i64 = con
        .query_row("PRAGMA application_id", [], |r| r.get(0))
        .map_err(wrap)?;
    if app != APPLICATION_ID {
        return Err(links_error(
            "scan_not_available",
            "not a SEOHEAD scan (application id differs)",
        ));
    }
    if !matches!(version, 1 | 2) {
        return Err(links_error(
            "scan_not_available",
            format!("unsupported scan user_version {version}"),
        ));
    }
    for (table, columns) in NEEDED {
        let mut have = BTreeSet::new();
        {
            let mut stmt = con
                .prepare(&format!("PRAGMA table_info({table})"))
                .map_err(wrap)?;
            let names = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .map_err(wrap)?;
            for name in names {
                have.insert(name.map_err(wrap)?);
            }
        }
        if !columns.iter().all(|c| have.contains(*c)) {
            return Err(links_error(
                "scan_not_available",
                format!("scan has no usable {table} table"),
            ));
        }
    }
    Ok(con)
}

fn unreadable() -> StoreError {
    links_error("scan_not_available", "scan header metadata is unreadable")
}

/// Header block, scope config, start host and start URL of a scan (no `Scope` built).
#[allow(clippy::type_complexity)]
pub fn header_parts(
    con: &Connection,
) -> Result<
    (
        Map<String, Value>,
        Option<BTreeMap<String, Value>>,
        String,
        Option<String>,
    ),
    StoreError,
> {
    #[allow(clippy::type_complexity)]
    let row: Option<(
        String,
        String,
        i64,
        String,
        i64,
        Option<String>,
        String,
        String,
        String,
    )> = con
        .query_row(
            "SELECT scan_uuid,format_version,evidence_revision,lifecycle,crawl_partial,start_url,\
             config_json,capabilities_json,limitations_json FROM scan WHERE singleton=1",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            },
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    let missing = || {
        links_error(
            "scan_not_available",
            "scan header is missing or has another format",
        )
    };
    let (uuid, format, revision, lifecycle, partial, start_url, config, caps, limits) =
        row.ok_or_else(missing)?;
    if !spiderhead_core::artifacts::SCAN_FORMATS.contains(&format.as_str()) {
        return Err(missing());
    }
    let config: Value = serde_json::from_str(&config).map_err(|_| unreadable())?;
    let caps: Value = serde_json::from_str(&caps).map_err(|_| unreadable())?;
    let limitations: Value = serde_json::from_str(&limits).map_err(|_| unreadable())?;
    let scope_cfg: Option<BTreeMap<String, Value>> = config
        .get("scope")
        .and_then(Value::as_object)
        .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
    let empty = Map::new();
    let links = caps
        .get("links")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let discovery = config
        .get("discovery")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let stores = |key: &str| {
        let policy = discovery.get(key).and_then(Value::as_object);
        policy.is_none_or(|p| p.get("store") != Some(&json!(false)))
    };
    let (internal, external) = (stores("hyperlinks"), stores("external"));
    let mut reasons: Vec<String> = Vec::new();
    if lifecycle != "finished" {
        reasons.push(format!("scan lifecycle is {lifecycle}"));
    }
    if partial != 0 {
        reasons.push("crawl is partial".into());
    }
    if links.get("state") != Some(&json!("complete")) {
        let reason = links
            .get("reason")
            .and_then(Value::as_str)
            .filter(|r| !r.is_empty())
            .unwrap_or("link capability is not complete");
        reasons.push(reason.to_string());
    }
    for item in limitations.as_array().into_iter().flatten() {
        let text = item
            .as_str()
            .map_or_else(|| item.to_string(), str::to_string);
        if text.starts_with("link_observations_omitted") {
            reasons.push(text);
        }
    }
    if !internal {
        reasons.push("internal links were not stored".into());
    }
    if !external {
        reasons.push("external links were not stored".into());
    }
    let unavailable = links.get("state") == Some(&json!("unavailable")) || !(internal || external);
    let start_host = start_url
        .as_deref()
        .and_then(hostname_of)
        .unwrap_or_default();
    let state = if unavailable {
        "unavailable"
    } else if !reasons.is_empty() {
        "partial"
    } else {
        "complete"
    };
    let mut out = Map::new();
    out.insert("scan_uuid".into(), json!(uuid));
    out.insert("evidence_revision".into(), json!(revision));
    out.insert(
        "coverage".into(),
        json!({
            "state": state,
            "reasons": reasons,
            "internal_links_stored": internal,
            "external_links_stored": external,
            "retained_attributes": ["rel", "target", "anchor", "position", "raw_href"],
            "not_retained_attributes": ["hreflang", "download"],
        }),
    );
    Ok((out, scope_cfg, start_host, start_url))
}

/// Header block, scope, start host and start URL of a scan.
pub fn header(
    con: &Connection,
) -> Result<(Map<String, Value>, Scope, String, Option<String>), StoreError> {
    let (out, scope_cfg, start_host, start_url) = header_parts(con)?;
    Ok((
        out,
        Scope::from_config(scope_cfg.as_ref()),
        start_host,
        start_url,
    ))
}

/// Ids of the URL's spellings (`https://h` and `https://h/`, plus fragment spellings when asked).
pub fn ids(con: &Connection, key: &str, fragments: bool) -> Result<Vec<i64>, StoreError> {
    let (first, second) = spellings(key);
    let mut sql = String::from("SELECT url_id FROM urls WHERE url IN (?,?)");
    let mut params = vec![first.clone(), second.clone()];
    if fragments {
        // destination spellings carrying a fragment: url LIKE 'key#%'
        sql.push_str(" OR (url>=? AND url<?) OR (url>=? AND url<?)");
        params.extend([
            format!("{first}#"),
            format!("{first}$"),
            format!("{second}#"),
            format!("{second}$"),
        ]);
    }
    sql.push_str(" LIMIT ?");
    let mut args: Vec<Sql> = params.into_iter().map(Sql::Text).collect();
    args.push(Sql::Integer(FRAGMENT_VARIANTS + 2));
    let mut stmt = con.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(args), |r| r.get::<_, i64>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Status class of a destination: `unscanned`, `error`, or `Nxx`.
pub fn status_class(scanned: bool, status: Option<i64>) -> String {
    if !scanned {
        return "unscanned".into();
    }
    match status {
        Some(s) if (100..600).contains(&s) => format!("{}xx", s / 100),
        _ => "error".into(),
    }
}

/// Whether a status class satisfies the requested filter.
pub fn matches_status(want: &str, cls: &str) -> bool {
    want == "all" || cls == want || (want == "broken" && matches!(cls, "4xx" | "5xx"))
}

/// `rel` / `nofollow` / `sponsored` / `ugc` flags of a stored link.
pub fn flags(rel_json: &str, nofollow: bool) -> Vec<(&'static str, Value)> {
    let tokens: Vec<String> = match serde_json::from_str::<Value>(rel_json) {
        Ok(Value::Array(items)) => items
            .iter()
            .map(|t| {
                t.as_str()
                    .map_or_else(|| t.to_string(), str::to_string)
                    .to_lowercase()
            })
            .collect(),
        _ => Vec::new(),
    };
    let joined = tokens.join(" ");
    vec![
        (
            "rel",
            if joined.is_empty() {
                Value::Null
            } else {
                json!(joined)
            },
        ),
        ("nofollow", json!(nofollow)),
        ("sponsored", json!(tokens.iter().any(|t| t == "sponsored"))),
        ("ugc", json!(tokens.iter().any(|t| t == "ugc"))),
    ]
}

/// Refuse a filter value outside its allowed set.
pub fn check(name: &str, value: &str, allowed: &[&str]) -> Result<(), StoreError> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(links_error(
            "invalid_filter",
            format!("{name} must be one of {}", allowed.join(", ")),
        ))
    }
}

/// Per-request context: connection, scope, start host, deadline and the base-status cache.
pub struct Ctx<'a> {
    con: &'a Connection,
    scope: &'a Scope,
    start_host: String,
    base: RefCell<BTreeMap<String, (bool, Option<i64>)>>,
}

type Row = Map<String, Value>;

impl<'a> Ctx<'a> {
    /// Bind a connection with its scope and start host.
    pub fn new(con: &'a Connection, scope: &'a Scope, start_host: &str) -> Self {
        Self {
            con,
            scope,
            start_host: start_host.to_string(),
            base: RefCell::new(BTreeMap::new()),
        }
    }

    /// Whether `url` is internal to the scan scope; `None` without a start host.
    pub fn internal(&self, url: &str) -> Option<bool> {
        (!self.start_host.is_empty()).then(|| self.scope.is_internal(url, &self.start_host))
    }

    /// Stream rows into `visit` until it returns `false`; a budget interrupt becomes `Budget`.
    pub fn rows(
        &self,
        sql: &str,
        params: &[Sql],
        mut visit: impl FnMut(Row) -> Result<bool, StoreError>,
    ) -> Result<(), StoreError> {
        let mut stmt = self.con.prepare(sql).map_err(budget_or)?;
        let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let mut rows = stmt
            .query(params_from_iter(params.iter()))
            .map_err(budget_or)?;
        while let Some(row) = rows.next().map_err(budget_or)? {
            let mut map = Map::new();
            for (index, name) in names.iter().enumerate() {
                map.insert(
                    name.clone(),
                    cell_value(row.get_ref(index).map_err(budget_or)?),
                );
            }
            if !visit(map)? {
                break;
            }
        }
        Ok(())
    }

    /// All rows of a statement.
    pub fn fetch(&self, sql: &str, params: &[Sql]) -> Result<Vec<Row>, StoreError> {
        let mut out = Vec::new();
        self.rows(sql, params, |row| {
            out.push(row);
            Ok(true)
        })?;
        Ok(out)
    }

    /// Status of the fragmentless page behind a destination that carries a fragment.
    pub fn base_status(&self, url: &str) -> Result<(bool, Option<i64>), StoreError> {
        let key = match url.split('#').next() {
            Some(k) if !k.is_empty() => k.to_string(),
            _ => url.to_string(),
        };
        if let Some(hit) = self.base.borrow().get(&key) {
            return Ok(*hit);
        }
        let (first, second) = spellings(&checked_url(&key, "link")?);
        let rows = self.fetch(
            "SELECT p.status_code FROM urls u JOIN pages p ON p.url_id=u.url_id \
             WHERE u.url IN (?,?) LIMIT 1",
            &[Sql::Text(first), Sql::Text(second)],
        )?;
        let value = (
            !rows.is_empty(),
            rows.first().and_then(|r| r["status_code"].as_i64()),
        );
        self.base.borrow_mut().insert(key, value);
        Ok(value)
    }
}

fn text<'a>(row: &'a Row, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}

fn put_flags(item: &mut Row, row: &Row) {
    for (key, value) in flags(
        text(row, "rel_json"),
        row["nofollow"].as_i64().unwrap_or(0) != 0,
    ) {
        item.insert(key.into(), value);
    }
}

fn position_fields(item: &mut Row, row: &Row) {
    let position = text(row, "position");
    item.insert(
        "position".into(),
        if position.is_empty() {
            Value::Null
        } else {
            json!(position)
        },
    );
    item.insert(
        "position_status".into(),
        json!(if position.is_empty() {
            "not_classified"
        } else {
            "classified"
        }),
    );
    let target = text(row, "target");
    item.insert(
        "attributes".into(),
        json!({"target": if target.is_empty() { Value::Null } else { json!(target) }}),
    );
    let raw = text(row, "raw_href");
    item.insert(
        "raw_href".into(),
        if raw.is_empty() {
            Value::Null
        } else {
            json!(raw)
        },
    );
}

/// Row of an outgoing link, shaped for the result.
pub fn out_item(ctx: &Ctx<'_>, row: &Row, index: i64) -> Result<Row, StoreError> {
    let url = text(row, "dest_url").to_string();
    let mut scanned = !row["page_url_id"].is_null();
    let mut status = row["page_status"].as_i64();
    if url.contains('#') {
        (scanned, status) = ctx.base_status(&url)?;
    }
    let mut item = Map::new();
    item.insert("index".into(), json!(index));
    item.insert("link_id".into(), row["link_id"].clone());
    item.insert("ordinal".into(), row["ordinal"].clone());
    item.insert(
        "representation".into(),
        row["evidence_representation"].clone(),
    );
    item.insert("target_url".into(), json!(url));
    item.insert("anchor".into(), row["anchor"].clone());
    item.insert(
        "anchor_source".into(),
        row.get("anchor_source").cloned().unwrap_or(Value::Null),
    );
    put_flags(&mut item, row);
    item.insert("internal".into(), json!(ctx.internal(&url)));
    item.insert("target_scanned".into(), json!(scanned));
    item.insert(
        "target_status".into(),
        if scanned { json!(status) } else { Value::Null },
    );
    position_fields(&mut item, row);
    // Keys are emitted in this fixed order: position, position_status, attributes, raw_href.
    Ok(item)
}

/// Row of an incoming link, shaped for the result.
pub fn in_item(ctx: &Ctx<'_>, row: &Row, index: i64) -> Result<Row, StoreError> {
    let url = text(row, "src_url").to_string();
    let mut item = Map::new();
    item.insert("index".into(), json!(index));
    item.insert("link_id".into(), row["link_id"].clone());
    item.insert("ordinal".into(), row["ordinal"].clone());
    item.insert(
        "representation".into(),
        row["evidence_representation"].clone(),
    );
    item.insert("source_url".into(), json!(url));
    item.insert("target_url".into(), row["dest_url"].clone());
    item.insert("anchor".into(), row["anchor"].clone());
    item.insert(
        "anchor_source".into(),
        row.get("anchor_source").cloned().unwrap_or(Value::Null),
    );
    put_flags(&mut item, row);
    item.insert("internal".into(), json!(ctx.internal(&url)));
    item.insert("source_status".into(), row["page_status"].clone());
    position_fields(&mut item, row);
    Ok(item)
}

/// Status class of an item in the given direction.
pub fn status_of(item: &Row, direction: &str) -> String {
    if direction == "out" {
        status_class(
            item["target_scanned"] == json!(true),
            item["target_status"].as_i64(),
        )
    } else {
        status_class(true, item["source_status"].as_i64())
    }
}

/// Filter predicate for the in-process filters (link type, status class, substring).
pub fn accepts(
    item: &Row,
    direction: &str,
    link_type: &str,
    status_class_want: &str,
    contains: Option<&str>,
) -> bool {
    if link_type != "all" {
        match item["internal"].as_bool() {
            None => return false,
            Some(internal) if internal != (link_type == "internal") => return false,
            _ => {}
        }
    }
    if !matches_status(status_class_want, &status_of(item, direction)) {
        return false;
    }
    if let Some(needle) = contains.filter(|c| !c.is_empty()) {
        let other = if direction == "out" {
            text(item, "target_url")
        } else {
            text(item, "source_url")
        };
        if !other.to_lowercase().contains(needle)
            && !text(item, "anchor").to_lowercase().contains(needle)
        {
            return false;
        }
    }
    true
}

struct Request<'a> {
    link_type: &'a str,
    follow: &'a str,
    status_class: &'a str,
    needle: Option<String>,
    sort: &'a str,
    count_cap: i64,
    scan_cap: i64,
}

fn qmarks(n: usize) -> String {
    vec!["?"; n].join(",")
}

/// One page of the outgoing (`out`) or incoming (`in`) links of a URL.
pub fn url_links(
    scan_path: &Path,
    url: &str,
    direction: &str,
    representation: &str,
    link_type: &str,
    follow: &str,
    status_class_want: &str,
    contains: Option<&str>,
    sort: &str,
    offset: i64,
    limit: i64,
    timeout_seconds: f64,
    count_cap: i64,
    scan_cap: i64,
) -> Result<Map<String, Value>, StoreError> {
    check("direction", direction, DIRECTIONS)?;
    check("representation", representation, REPRESENTATIONS)?;
    check("link_type", link_type, LINK_TYPES)?;
    check("follow", follow, FOLLOW)?;
    check("status_class", status_class_want, STATUS_CLASSES)?;
    check("sort", sort, SORTS)?;
    if sort != "order" && direction == "in" {
        return Err(links_error(
            "invalid_filter",
            "incoming links support only the default order",
        ));
    }
    if let Some(c) = contains {
        if !(1..=512).contains(&c.chars().count()) {
            return Err(links_error(
                "invalid_filter",
                "contains must be 1..512 characters",
            ));
        }
    }
    if !(1..=MAX_LIMIT).contains(&limit)
        || !(0..=MAX_OFFSET).contains(&offset)
        || !(timeout_seconds > 0.0 && timeout_seconds <= 30.0)
        || !(1..=5_000_000).contains(&count_cap)
        || !(1..=1_000_000).contains(&scan_cap)
    {
        return Err(links_error(
            "invalid_page",
            format!("invalid page; limit 1..{MAX_LIMIT}, offset 0..{MAX_OFFSET}"),
        ));
    }
    let key = checked_url(url, "url")?;
    let needle = contains.map(str::to_lowercase);
    let deadline = Instant::now() + Duration::from_secs_f64(timeout_seconds);
    let con = open_light(scan_path)?;
    let (head, scope, start_host, _) = header(&con)?;
    con.progress_handler(10_000, Some(move || Instant::now() >= deadline));
    let ctx = Ctx::new(&con, &scope, &start_host);
    let mut result = Map::new();
    for (k, v) in &head {
        result.insert(k.clone(), v.clone());
    }
    result.insert("url".into(), json!(key));
    result.insert("direction".into(), json!(direction));
    result.insert("representation".into(), json!(representation));
    result.insert(
        "filters".into(),
        json!({
            "link_type": link_type, "follow": follow, "status_class": status_class_want,
            "contains": contains, "sort": sort,
        }),
    );
    result.insert("offset".into(), json!(offset));
    result.insert("limit".into(), json!(limit));
    result.insert("max_limit".into(), json!(MAX_LIMIT));
    result.insert("state".into(), json!("complete"));
    result.insert("reason_code".into(), Value::Null);
    result.insert("items".into(), json!([]));
    result.insert("returned".into(), json!(0));
    result.insert("has_more".into(), json!(false));
    result.insert("next_offset".into(), Value::Null);
    result.insert("total".into(), Value::Null);
    result.insert("total_capped".into(), json!(false));
    result.insert("filtered_total".into(), Value::Null);
    result.insert("filtered_total_state".into(), json!("unknown"));
    let found = ids(&con, &key, direction == "in")?;
    if found.is_empty() {
        return Err(links_error(
            "url_not_found",
            "URL is not recorded in this scan",
        ));
    }
    if found.len() as i64 > FRAGMENT_VARIANTS {
        return Err(links_error(
            "too_many_variants",
            "URL has too many fragment spellings",
        ));
    }
    let page_status: Option<Option<i64>> = {
        let sql = format!(
            "SELECT status_code FROM pages WHERE url_id IN ({}) LIMIT 1",
            qmarks(found.len())
        );
        let args: Vec<Sql> = found.iter().map(|i| Sql::Integer(*i)).collect();
        let mut stmt = con.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(args))?;
        match rows.next()? {
            Some(r) => Some(r.get::<_, Option<i64>>(0)?),
            None => None,
        }
    };
    result.insert(
        "page".into(),
        json!({"scanned": page_status.is_some(), "status": page_status.flatten()}),
    );
    let unavailable = |mut result: Map<String, Value>, code: &str| {
        result.insert("state".into(), json!("unavailable"));
        result.insert("reason_code".into(), json!(code));
        result
    };
    if direction == "out" && page_status.is_none() {
        return Ok(unavailable(result, "url_not_scanned"));
    }
    if head["coverage"]["state"] == json!("unavailable") {
        return Ok(unavailable(result, "links_not_retained"));
    }
    let request = Request {
        link_type,
        follow,
        status_class: status_class_want,
        needle,
        sort,
        count_cap,
        scan_cap,
    };
    match page(&ctx, &mut result, &found, representation, &request) {
        Err(StoreError::Budget(_)) => {
            result.insert("state".into(), json!("limit_reached"));
            result.insert("reason_code".into(), json!("time_budget_exhausted"));
        }
        other => other?,
    }
    let coverage = &head["coverage"];
    if result["total"] == json!(0)
        && !(coverage["internal_links_stored"] == json!(true)
            && coverage["external_links_stored"] == json!(true))
    {
        result.insert("state".into(), json!("unavailable"));
        result.insert("reason_code".into(), json!("links_not_retained"));
    }
    Ok(result)
}

fn page(
    ctx: &Ctx<'_>,
    result: &mut Map<String, Value>,
    found: &[i64],
    representation: &str,
    req: &Request<'_>,
) -> Result<(), StoreError> {
    let direction = result["direction"].as_str().unwrap_or("out").to_string();
    let offset = result["offset"].as_i64().unwrap_or(0);
    let limit = result["limit"].as_i64().unwrap_or(100);
    let marks = qmarks(found.len());
    let mut where_parts = Vec::new();
    let mut params: Vec<Sql> = Vec::new();
    where_parts.push(if direction == "out" {
        format!("l.source_url_id IN ({marks})")
    } else {
        format!("l.destination_url_id IN ({marks})")
    });
    params.extend(found.iter().map(|i| Sql::Integer(*i)));
    if representation != "all" {
        where_parts.push("l.evidence_representation=?".into());
        params.push(Sql::Text(representation.into()));
    }
    let base_where = where_parts.join(" AND ");
    let base_params = params.clone();
    if matches!(req.follow, "sponsored" | "ugc") {
        // Exact token match on the stored rel list; `json_each` avoids LIKE false hits.
        where_parts.push("EXISTS (SELECT 1 FROM json_each(l.rel_json) WHERE value=?)".into());
        params.push(Sql::Text(req.follow.into()));
    } else if req.follow != "all" {
        where_parts.push("l.nofollow=?".into());
        params.push(Sql::Integer(i64::from(req.follow == "nofollow")));
    }
    let flt_where = where_parts.join(" AND ");
    let py_filtering = req.link_type != "all"
        || req.status_class != "all"
        || req.needle.is_some()
        || req.sort != "order";
    let columns = with_anchor_source(ctx.con)?;
    let (select, order, hint) = if direction == "out" {
        (
            format!(
                "SELECT {columns},d.url AS dest_url,p.url_id AS page_url_id,\
                 p.status_code AS page_status FROM links l \
                 JOIN urls d ON d.url_id=l.destination_url_id \
                 LEFT JOIN pages p ON p.url_id=l.destination_url_id"
            ),
            "l.evidence_representation,l.ordinal,l.link_id",
            "",
        )
    } else {
        (
            format!(
                "SELECT {columns},s.url AS src_url,d.url AS dest_url,\
                 p.status_code AS page_status FROM links l \
                 JOIN urls s ON s.url_id=l.source_url_id JOIN urls d ON d.url_id=l.destination_url_id \
                 LEFT JOIN pages p ON p.url_id=l.source_url_id"
            ),
            "l.destination_url_id,l.source_url_id,l.position,l.link_id",
            " INDEXED BY links_destination_source_position",
        )
    };
    let make = |row: &Row, index: i64| -> Result<Row, StoreError> {
        if direction == "out" {
            out_item(ctx, row, index)
        } else {
            in_item(ctx, row, index)
        }
    };
    let mut items: Vec<Row> = Vec::new();
    let has_more;
    if !py_filtering {
        let sql = format!(
            "{select} WHERE l.link_id IN (SELECT l.link_id FROM links l{hint} \
             WHERE {flt_where} ORDER BY {order} LIMIT ? OFFSET ?) ORDER BY {order}"
        );
        let mut args = params.clone();
        args.push(Sql::Integer(limit + 1));
        args.push(Sql::Integer(offset));
        let rows = ctx.fetch(&sql, &args)?;
        for (number, row) in rows.iter().take(limit as usize).enumerate() {
            items.push(make(row, offset + 1 + number as i64)?);
        }
        has_more = rows.len() as i64 > limit;
    } else {
        // Filters that need the other end's host/status, or a re-sort, examine the URL's own
        // links in order up to scan_cap rows.
        let mut matched: Vec<Row> = Vec::new();
        let mut exhausted = true;
        let mut examined = 0i64;
        ctx.rows(
            &format!("{select} WHERE {flt_where} ORDER BY {order}"),
            &params,
            |row| {
                if examined >= req.scan_cap {
                    exhausted = false;
                    return Ok(false);
                }
                examined += 1;
                let item = make(&row, 0)?;
                if accepts(
                    &item,
                    &direction,
                    req.link_type,
                    req.status_class,
                    req.needle.as_deref(),
                ) {
                    matched.push(item);
                }
                Ok(true)
            },
        )?;
        let id = |i: &Row| i["link_id"].as_i64().unwrap_or(0);
        if req.sort == "url" {
            matched.sort_by(|a, b| {
                (text(a, "target_url"), id(a)).cmp(&(text(b, "target_url"), id(b)))
            });
        } else if req.sort == "status" {
            let key = |i: &Row| {
                let s = i["target_status"].as_i64();
                (s.is_none(), s.unwrap_or(0), id(i))
            };
            matched.sort_by_key(key);
        }
        let start = (offset as usize).min(matched.len());
        let window: Vec<&Row> = matched[start..].iter().take(limit as usize + 1).collect();
        for (number, item) in window.iter().take(limit as usize).enumerate() {
            let mut item = (*item).clone();
            item.insert("index".into(), json!(offset + 1 + number as i64));
            items.push(item);
        }
        has_more = window.len() as i64 > limit || !exhausted;
        result.insert(
            "filtered_total".into(),
            if exhausted {
                json!(matched.len())
            } else {
                Value::Null
            },
        );
        result.insert(
            "filtered_total_state".into(),
            json!(if exhausted { "exact" } else { "scan_capped" }),
        );
        if !exhausted {
            result.insert("reason_code".into(), json!("filter_scan_capped"));
            result.insert("state".into(), json!("partial"));
        }
    }
    let returned = items.len() as i64;
    result.insert(
        "items".into(),
        Value::Array(items.into_iter().map(Value::Object).collect()),
    );
    result.insert("returned".into(), json!(returned));
    result.insert("has_more".into(), json!(has_more));
    result.insert(
        "next_offset".into(),
        if has_more {
            json!(offset + returned)
        } else {
            Value::Null
        },
    );
    // Counts last, on whatever time is left: a page is worth more than its total.
    let counts = |where_sql: &str,
                  where_params: &[Sql],
                  cap: i64|
     -> Result<(i64, bool, Option<i64>), StoreError> {
        let inner = format!("SELECT l.source_url_id FROM links l{hint} WHERE {where_sql} LIMIT ?");
        let mut args = where_params.to_vec();
        args.push(Sql::Integer(cap + 1));
        let rows = ctx.fetch(
            &format!("SELECT COUNT(*),COUNT(DISTINCT source_url_id) FROM ({inner})"),
            &args,
        )?;
        let total = rows[0]["COUNT(*)"].as_i64().unwrap_or(0);
        let unique = rows[0]["COUNT(DISTINCT source_url_id)"]
            .as_i64()
            .unwrap_or(0);
        Ok((
            total.min(cap),
            total > cap,
            (total <= cap).then_some(unique),
        ))
    };
    let counted = (|| -> Result<(), StoreError> {
        let (total, capped, unique) = counts(&base_where, &base_params, req.count_cap)?;
        result.insert("total".into(), json!(total));
        result.insert("total_capped".into(), json!(capped));
        if direction == "in" {
            result.insert("unique_sources".into(), json!(unique));
        }
        if !py_filtering {
            if req.follow == "all" {
                result.insert("filtered_total".into(), json!(total));
                result.insert(
                    "filtered_total_state".into(),
                    json!(if capped { "capped" } else { "exact" }),
                );
            } else {
                let (ft, fcapped, _) = counts(&flt_where, &params, req.count_cap)?;
                result.insert("filtered_total".into(), json!(ft));
                result.insert(
                    "filtered_total_state".into(),
                    json!(if fcapped { "capped" } else { "exact" }),
                );
            }
        }
        if direction == "out" {
            let summary = if capped || total > req.scan_cap {
                Value::Null
            } else {
                out_summary(ctx, &base_where, &base_params, req.scan_cap)?
                    .map_or(Value::Null, Value::Object)
            };
            result.insert("summary".into(), summary);
        }
        Ok(())
    })();
    match counted {
        Err(StoreError::Budget(_)) => {
            result.insert("total_state".into(), json!("time_budget_exhausted"));
            if !py_filtering {
                result.insert(
                    "filtered_total_state".into(),
                    json!("time_budget_exhausted"),
                );
            }
            Ok(())
        }
        other => other,
    }
}

/// Counts of internal/external/nofollow links of the URL, or `None` when too many to scan.
pub fn out_summary(
    ctx: &Ctx<'_>,
    where_: &str,
    params: &[Sql],
    scan_cap: i64,
) -> Result<Option<Map<String, Value>>, StoreError> {
    let (mut total, mut internal, mut external, mut nofollow, mut unknown) = (0i64, 0, 0, 0, 0);
    let mut over = false;
    ctx.rows(
        &format!(
            "SELECT d.url,l.nofollow FROM links l JOIN urls d ON d.url_id=l.destination_url_id \
             WHERE {where_}"
        ),
        params,
        |row| {
            if total >= scan_cap {
                over = true;
                return Ok(false);
            }
            total += 1;
            nofollow += i64::from(row["nofollow"].as_i64().unwrap_or(0) != 0);
            match ctx.internal(text(&row, "url")) {
                None => unknown += 1,
                Some(true) => internal += 1,
                Some(false) => external += 1,
            }
            Ok(true)
        },
    )?;
    if over {
        return Ok(None);
    }
    let mut out = Map::new();
    out.insert("total".into(), json!(total));
    out.insert("internal".into(), json!(internal));
    out.insert("external".into(), json!(external));
    out.insert("unknown_locality".into(), json!(unknown));
    out.insert("nofollow".into(), json!(nofollow));
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_classes() {
        assert_eq!(status_class(false, Some(200)), "unscanned");
        assert_eq!(status_class(true, None), "error");
        assert_eq!(status_class(true, Some(404)), "4xx");
        assert_eq!(status_class(true, Some(99)), "error");
        assert_eq!(status_class(true, Some(600)), "error");
        assert!(matches_status("broken", "5xx"));
        assert!(!matches_status("broken", "3xx"));
        assert!(matches_status("all", "error"));
    }

    #[test]
    fn rel_flags() {
        let f = flags(r#"["Nofollow","UGC"]"#, true);
        assert_eq!(f[0], ("rel", json!("nofollow ugc")));
        assert_eq!(f[2], ("sponsored", json!(false)));
        assert_eq!(f[3], ("ugc", json!(true)));
        assert_eq!(flags("not json", false)[0], ("rel", Value::Null));
        assert_eq!(flags("[]", false)[0], ("rel", Value::Null));
    }

    #[test]
    fn check_names_the_allowed_values() {
        assert!(check("sort", "url", SORTS).is_ok());
        let err = check("sort", "zig", SORTS).unwrap_err();
        assert_eq!(err.to_string(), "sort must be one of order, url, status");
        assert!(matches!(
            err,
            StoreError::LinksError(LinksError { ref reason_code, .. }) if reason_code == "invalid_filter"
        ));
    }

    #[test]
    fn missing_scan_is_not_available() {
        let err = open_light(Path::new("/nonexistent/w2s6.sqlite")).unwrap_err();
        assert!(matches!(
            err,
            StoreError::LinksError(LinksError { ref reason_code, .. }) if reason_code == "scan_not_available"
        ));
    }

    #[test]
    fn page_arguments_are_validated() {
        let err = url_links(
            Path::new("/x"),
            "https://e.test/",
            "out",
            "all",
            "all",
            "all",
            "all",
            None,
            "order",
            0,
            0,
            1.0,
            COUNT_CAP,
            SCAN_CAP,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            StoreError::LinksError(LinksError { ref reason_code, .. }) if reason_code == "invalid_page"
        ));
        let err = url_links(
            Path::new("/x"),
            "https://e.test/",
            "in",
            "all",
            "all",
            "all",
            "all",
            None,
            "url",
            0,
            10,
            1.0,
            COUNT_CAP,
            SCAN_CAP,
        )
        .unwrap_err();
        assert!(err.to_string().contains("incoming links support only"));
    }

    #[test]
    fn hostnames() {
        assert_eq!(
            hostname_of("https://User@Example.COM:8080/x").as_deref(),
            Some("example.com")
        );
        assert_eq!(hostname_of("https://[::1]:80/").as_deref(), Some("::1"));
        assert_eq!(hostname_of("").as_deref(), None);
    }

    #[test]
    fn with_anchor_source_detects_column() {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch("CREATE TABLE links(link_id INTEGER)")
            .unwrap();
        assert_eq!(with_anchor_source(&con).unwrap(), LINK_COLUMNS);
        con.execute_batch("ALTER TABLE links ADD COLUMN anchor_source TEXT")
            .unwrap();
        assert!(with_anchor_source(&con)
            .unwrap()
            .ends_with(",l.anchor_source"));
    }

    #[test]
    fn ids_find_root_spellings() {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(
            "CREATE TABLE urls(url_id INTEGER PRIMARY KEY,url TEXT);
             INSERT INTO urls(url) VALUES('https://e.test'),('https://e.test/'),('https://e.test/#x'),('https://e.test/a');",
        )
        .unwrap();
        assert_eq!(ids(&con, "https://e.test/", false).unwrap().len(), 2);
        assert_eq!(ids(&con, "https://e.test/", true).unwrap().len(), 3);
    }
}
