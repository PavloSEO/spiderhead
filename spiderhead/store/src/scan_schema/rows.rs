//! Row-level checks of `validate_native`: runtime state, frontier, pages, links, context items.

use rusqlite::types::ValueRef;
use rusqlite::{Connection, Row};
use serde_json::{Map, Value};

use super::bad;
use super::deps::deps;
use super::validate::{count, exists, finite_number, is_int, text};
use crate::error::StoreError;
use crate::scan_artifact::{
    duplicate_ids, has_negative_page_counts, heading_outline, link_placement, query_maps,
    trust_signals,
};

const BODY_MARKERS: [&str; 5] = [
    "oversized",
    "excluded_by_media_type",
    "not_included_by_media_type",
    "media_type_unavailable",
    "",
];

/// Truthiness helper for `config["limits"]["max_requests"]`; an absent value counts as 0.
fn config_int(config: &Map<String, Value>, section: &str, key: &str) -> Option<i64> {
    config.get(section)?.get(key)?.as_i64()
}

/// Whether the saved throttle state and counters of `resume_state` are inconsistent.
fn runtime_invalid(runtime: &Map<String, Value>, config: &Map<String, Value>) -> bool {
    let Some(throttle) =
        text(runtime, "throttle_state_json").and_then(|t| serde_json::from_str::<Value>(t).ok())
    else {
        return true;
    };
    let Some(keys) = throttle.as_object() else {
        return true;
    };
    let v1 = [
        "schema_version",
        "delay_seconds",
        "concurrency",
        "consecutive_ok",
    ];
    let v2 = [
        "schema_version",
        "delay_seconds",
        "concurrency",
        "consecutive_ok",
        "requests_used",
    ];
    let version = throttle["schema_version"].as_str();
    let expected: &[&str] = if version == Some("scan_throttle.v1") {
        &v1
    } else {
        &v2
    };
    let mut names: Vec<&str> = keys.keys().map(String::as_str).collect();
    names.sort_unstable();
    let mut wanted = expected.to_vec();
    wanted.sort_unstable();
    if names != wanted || !matches!(version, Some("scan_throttle.v1" | "scan_throttle.v2")) {
        return true;
    }
    let (concurrency, consecutive) = (&throttle["concurrency"], &throttle["consecutive_ok"]);
    if !is_int(concurrency) || !is_int(consecutive) {
        return true;
    }
    if version == Some("scan_throttle.v2") {
        let used = &throttle["requests_used"];
        if !is_int(used) || used.as_i64().unwrap_or(-1) < 0 {
            return true;
        }
        let max = config_int(config, "limits", "max_requests").unwrap_or(0);
        if max != 0 && used.as_i64().unwrap_or(0) > max {
            return true;
        }
    }
    if finite_number(&throttle["delay_seconds"]).is_none_or(|d| d < 0.0) {
        return true;
    }
    let ceiling = config_int(config, "speed", "concurrency").unwrap_or(0);
    let concurrency = concurrency.as_i64().unwrap_or(0);
    let consecutive = consecutive.as_i64().unwrap_or(-1);
    if !(1 <= concurrency && concurrency <= ceiling) || !(0..=2).contains(&consecutive) {
        return true;
    }
    for name in [
        "max_depth_reached",
        "circuit_timeout_streak",
        "circuit_server_error_streak",
    ] {
        if !runtime.get(name).is_some_and(is_int) || runtime[name].as_i64().unwrap_or(-1) < 0 {
            return true;
        }
    }
    if finite_number(&runtime["elapsed_seconds"]).is_none_or(|e| e < 0.0) {
        return true;
    }
    let delay = &runtime["crawl_delay_applied"];
    !delay.is_null() && finite_number(delay).is_none_or(|d| d < 0.0)
}

/// Column `name` of `row` as optional text.
fn opt_text(row: &Row<'_>, name: &str) -> rusqlite::Result<Option<String>> {
    match row.get_ref(name)? {
        ValueRef::Null => Ok(None),
        ValueRef::Text(t) => Ok(Some(String::from_utf8_lossy(t).into_owned())),
        ValueRef::Integer(i) => Ok(Some(i.to_string())),
        ValueRef::Real(f) => Ok(Some(f.to_string())),
        ValueRef::Blob(_) => Ok(None),
    }
}

/// Parse a JSON column; NULL or empty text yields the default.
fn json_or(text: Option<String>, default: &str) -> Result<Value, StoreError> {
    let text = text.filter(|t| !t.is_empty());
    Ok(serde_json::from_str(text.as_deref().unwrap_or(default))?)
}

/// Per-page evidence fields against the frontier lease and the PageRecord vocabulary.
fn check_pages(con: &Connection) -> Result<(), StoreError> {
    let mut frontier = con.prepare("SELECT depth FROM frontier WHERE url_id=?")?;
    let mut pages = con.prepare("SELECT * FROM pages")?;
    let mut rows = pages.query([])?;
    while let Some(page) = rows.next()? {
        let url_id: i64 = page.get("url_id")?;
        let depth: Option<i64> = frontier
            .query_row([url_id], |r| r.get(0))
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        let crawl_depth: Option<i64> = page.get("crawl_depth")?;
        if depth.is_none() || crawl_depth != depth {
            return Err(bad(
                "native scan page crawl depth disagrees with its frontier lease",
            ));
        }
        let frames: Option<i64> = page.get("content_frames")?;
        let same_origin: Option<i64> = page.get("content_frames_same_origin")?;
        let hreflang = opt_text(page, "hreflang_json")?;
        let outline_text = opt_text(page, "heading_outline_json")?;
        let unavailable = opt_text(page, "body_unavailable")?;
        if frames.is_none()
            || same_origin.is_none()
            || hreflang.is_none()
            || outline_text.is_none()
            || unavailable.is_none()
        {
            return Err(bad(
                "native scan page is missing a current PageRecord evidence field",
            ));
        }
        if same_origin > frames
            || !unavailable
                .as_deref()
                .is_none_or(|marker| BODY_MARKERS.contains(&marker))
        {
            return Err(bad("native scan page scalar/body marker is invalid"));
        }
        let parsed = (|| -> Result<_, StoreError> {
            Ok((
                json_or(hreflang, "[]")?,
                json_or(outline_text, "[]")?,
                json_or(opt_text(page, "link_placement_json")?, "null")?,
                json_or(opt_text(page, "trust_signals_json")?, "null")?,
                json_or(opt_text(page, "duplicate_ids_json")?, "null")?,
                serde_json::from_str::<Value>(
                    &opt_text(page, "redirect_chain_json")?.unwrap_or_default(),
                )?,
            ))
        })()
        .map_err(|e| match e {
            StoreError::Sqlite(_) => e,
            _ => bad("native scan page JSON is invalid"),
        })?;
        let (alternates, outline, placement, signals, ids, chain) = parsed;
        heading_outline(&outline)?;
        if !placement.is_null() {
            link_placement(&placement)?;
        }
        if !signals.is_null() {
            trust_signals(&signals)?;
        }
        if !ids.is_null() {
            duplicate_ids(&ids)?;
        }
        let alternates_ok = alternates.as_array().is_some_and(|items| {
            items.iter().all(|item| {
                item.as_object().is_some_and(|o| {
                    o.len() == 3
                        && ["lang", "raw_href", "url"]
                            .iter()
                            .all(|k| o.get(*k).is_some_and(Value::is_string))
                })
            })
        });
        let chain_ok = chain
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_object));
        if !alternates_ok || !chain_ok {
            return Err(bad(
                "native scan page hreflang or redirect chain is invalid",
            ));
        }
    }
    Ok(())
}

fn representation_ok(value: &str) -> bool {
    matches!(value, "static" | "rendered" | "legacy_fragment")
}

fn check_links_and_forms(con: &Connection) -> Result<(), StoreError> {
    let mut links = con.prepare("SELECT evidence_representation, rel_json FROM links")?;
    let mut rows = links.query([])?;
    while let Some(row) = rows.next()? {
        let representation: String = row.get(0)?;
        if !representation_ok(&representation) {
            return Err(bad("native link representation is invalid"));
        }
        let rel: Value = serde_json::from_str(&row.get::<_, String>(1)?)?;
        if !rel
            .as_array()
            .is_some_and(|tokens| tokens.iter().all(Value::is_string))
        {
            return Err(bad("native links.rel_json must be an ordered string list"));
        }
    }
    let mut forms = con.prepare("SELECT evidence_representation FROM forms")?;
    let mut rows = forms.query([])?;
    while let Some(row) = rows.next()? {
        if !representation_ok(&row.get::<_, String>(0)?) {
            return Err(bad("native form representation is invalid"));
        }
    }
    if exists(
        con,
        "SELECT 1 FROM links GROUP BY source_url_id, evidence_representation \
         HAVING MIN(ordinal) != 0 OR MAX(ordinal) != COUNT(*) - 1 LIMIT 1",
    )? {
        return Err(bad(
            "native scan link ordinals are not contiguous per source",
        ));
    }
    if exists(
        con,
        "SELECT 1 FROM forms GROUP BY page_url_id, evidence_representation \
         HAVING MIN(ordinal) != 0 OR MAX(ordinal) != COUNT(*) - 1 LIMIT 1",
    )? {
        return Err(bad(
            "native scan form ordinals are not contiguous per source",
        ));
    }
    Ok(())
}

/// Run-wide ordinals of one context kind must form one contiguous 0..n sequence.
fn check_ordinals(con: &Connection) -> Result<(), StoreError> {
    for (kind, expression) in [
        ("resource_commit", "CAST(substr(item_key,10) AS INTEGER)"),
        ("sitemap_declaration", "CAST(substr(item_key,9) AS INTEGER)"),
        (
            "sitemap_declared_url",
            "CAST(substr(item_key,instr(item_key,':ordinal:')+9) AS INTEGER)",
        ),
    ] {
        let (n, unique, low, high): (i64, i64, Option<i64>, Option<i64>) = con.query_row(
            &format!(
                "SELECT COUNT(*),COUNT(DISTINCT {expression}),MIN({expression}),MAX({expression}) \
                 FROM context_items WHERE kind=?"
            ),
            [kind],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        if n != 0 && (unique != n || low != Some(0) || high != Some(n - 1)) {
            return Err(bad(format!(
                "{kind}: source ordinals are not one contiguous run-wide sequence"
            )));
        }
    }
    Ok(())
}

/// Evidence revision against the committed pages, resources and rendered documents.
fn check_revision(
    con: &Connection,
    scan: &Map<String, Value>,
    source_kind: &str,
    page_count: i64,
) -> Result<(), StoreError> {
    let floor = page_count
        + count(
            con,
            "SELECT COUNT(*) FROM context_items WHERE kind='resource_commit'",
        )?
        + count(
            con,
            "SELECT COUNT(*) FROM documents WHERE representation IN ('rendered','legacy_fragment')",
        )?;
    let provenance: Option<String> = con
        .query_row(
            "SELECT payload_json FROM context_items WHERE kind='reanalysis_provenance' AND item_key='run'",
            [],
            |r| r.get(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    let derived = provenance
        .as_deref()
        .and_then(|t| serde_json::from_str::<Value>(t).ok())
        .and_then(|v| v.get("derived_evidence_revision").cloned());
    let revision = scan.get("evidence_revision").and_then(Value::as_i64);
    let revision_invalid = if source_kind == "native" {
        revision != Some(floor)
    } else {
        provenance.is_none()
            || derived.as_ref().and_then(Value::as_i64) != revision
            || revision.is_none_or(|r| r < floor)
    };
    if revision_invalid
        || exists(
            con,
            "SELECT 1 FROM frontier f LEFT JOIN pages p USING(url_id) \
             WHERE (f.state='done') != (p.url_id IS NOT NULL) OR \
             (p.url_id IS NOT NULL AND p.crawl_depth != f.depth) LIMIT 1",
        )?
        || exists(
            con,
            "SELECT 1 FROM pages p LEFT JOIN frontier f USING(url_id) WHERE f.url_id IS NULL LIMIT 1",
        )?
    {
        if source_kind == "reanalysis" {
            return Err(bad("derived reanalysis provenance or revision disagrees"));
        }
        return Err(bad(
            "native page evidence revision or frontier completion/depth disagrees",
        ));
    }
    Ok(())
}

/// Everything after the header: runtime state, frontier, pages, links, forms and context items.
pub(super) fn validate_rows(
    con: &Connection,
    scan: &Map<String, Value>,
    config: &Map<String, Value>,
    source_kind: &str,
) -> Result<(), StoreError> {
    let runtime = query_maps(con, "SELECT * FROM resume_state WHERE singleton=1")?
        .into_iter()
        .next()
        .ok_or_else(|| bad("native scan requires one resume_state"))?;
    if text(&runtime, "throttle_state_json")
        .is_none_or(|t| serde_json::from_str::<Value>(t).is_err())
    {
        return Err(bad("native scan throttle state is invalid JSON"));
    }
    if runtime_invalid(&runtime, config) {
        return Err(bad("native scan runtime state is invalid"));
    }
    if exists(
        con,
        "SELECT 1 FROM frontier WHERE state NOT IN ('queued','inflight','done','excluded') LIMIT 1",
    )? {
        return Err(bad("native scan frontier has an invalid state"));
    }
    let (frontier_count, low, high): (i64, Option<i64>, Option<i64>) = con.query_row(
        "SELECT COUNT(*), MIN(queue_ordinal), MAX(queue_ordinal) FROM frontier",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if frontier_count != 0 && (low != Some(0) || high != Some(frontier_count - 1)) {
        return Err(bad(
            "native scan frontier queue ordinals are not a contiguous sequence",
        ));
    }
    if exists(con, "SELECT 1 FROM frontier WHERE depth < 0 LIMIT 1")? {
        return Err(bad("native scan frontier depth cannot be negative"));
    }
    if text(scan, "lifecycle") == Some("finished")
        && text(scan, "finish_reason") != Some("stopped_by_budget")
        && source_kind == "native"
        && exists(
            con,
            "SELECT 1 FROM frontier WHERE state IN ('queued','inflight') LIMIT 1",
        )?
    {
        return Err(bad("terminal native scan has unfinished frontier work"));
    }
    if has_negative_page_counts(con)? {
        return Err(bad("native scan page count cannot be negative"));
    }
    check_pages(con)?;
    let (page_count, page_low, page_high): (i64, Option<i64>, Option<i64>) = con.query_row(
        "SELECT COUNT(*), MIN(page_ordinal), MAX(page_ordinal) FROM pages",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if page_count != 0 && (page_low != Some(0) || page_high != Some(page_count - 1)) {
        return Err(bad(
            "native scan page ordinals are not a contiguous sequence",
        ));
    }
    check_revision(con, scan, source_kind, page_count)?;
    if count(
        con,
        "SELECT COUNT(*) FROM context_items WHERE kind='native_commit'",
    )? != page_count
    {
        return Err(bad(
            "native scan is missing committed-page idempotency context",
        ));
    }
    check_links_and_forms(con)?;
    let roots = deps().sitemap_root_ids(con)?;
    let mut items = con.prepare("SELECT * FROM context_items")?;
    let names: Vec<String> = items
        .column_names()
        .iter()
        .map(|n| (*n).to_owned())
        .collect();
    let mut cursor = items.query([])?;
    while let Some(row) = cursor.next()? {
        let mut item = Map::new();
        for (i, name) in names.iter().enumerate() {
            item.insert(name.clone(), cell(row.get_ref(i)?));
        }
        deps().validate_context(con, &item, Some(&roots))?;
    }
    deps().validate_ledger(con)?;
    if source_kind == "reanalysis" {
        let marker: Option<String> = con
            .query_row(
                "SELECT payload_json FROM context_items WHERE kind='reanalysis_provenance' AND item_key='run'",
                [],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        let parent = marker
            .as_deref()
            .map(serde_json::from_str::<Value>)
            .transpose()?
            .and_then(|v| v.get("parent_scan_uuid").cloned());
        if parent.is_none() || parent.as_ref() != scan.get("parent_scan_uuid") {
            return Err(bad("derived reanalysis provenance is missing or disagrees"));
        }
    }
    check_ordinals(con)
}

fn cell(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => Value::from(f),
        ValueRef::Text(t) => Value::from(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::from(String::from_utf8_lossy(b).into_owned()),
    }
}
