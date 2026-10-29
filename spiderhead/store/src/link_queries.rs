//! Bounded, read-only operator queries over retained hyperlink occurrences.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;
use std::time::{Duration, Instant};

use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::crawl_records::spider::{canonical_key, Scope};
use crate::error::StoreError;
use crate::link_context::{arm_deadline, open_light, row_map};

/// Representation filters accepted by the queries, in sorted order.
pub const REPRESENTATIONS: &[&str] = &[
    "all",
    "legacy_fragment",
    "legacy_unknown",
    "rendered",
    "static",
];
/// Regex source for cursor tokens: ASCII letters, digits, `_` and `-`, up to 2048 characters.
pub const CURSOR_CHARS: &str = r"[A-Za-z0-9_-]{1,2048}\Z";
/// Stable ordering of occurrences: static, rendered, legacy fragment, then the rest.
pub const ORDER: &str = "CASE l.evidence_representation WHEN 'static' THEN 0 WHEN 'rendered' THEN 1 WHEN 'legacy_fragment' THEN 2 ELSE 3 END, l.ordinal, l.link_id";

const CURSOR_VERSION: &str = "scan_inlinks.v1";
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::Invalid(message.into())
}

/// Validate an absolute HTTP(S) URL without credentials and return its canonical key.
pub fn url(value: &str, label: &str) -> Result<String, StoreError> {
    let message = format!("{label} must be an absolute HTTP(S) URL without credentials");
    if value.is_empty() || value.len() > 8192 {
        return Err(invalid(format!(
            "{label} must be an absolute HTTP(S) URL of at most 8192 characters"
        )));
    }
    let (scheme, rest) = value
        .split_once("://")
        .ok_or_else(|| invalid(message.clone()))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority;
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https")
        || authority.is_empty()
        || authority.contains('@')
        || value.chars().any(char::is_whitespace)
    {
        return Err(invalid(message));
    }
    let host = host_port
        .rsplit_once(':')
        .map(|(h, p)| (h, Some(p)))
        .unwrap_or((host_port, None));
    if host.0.is_empty() {
        return Err(invalid(message));
    }
    if let Some(port) = host.1 {
        let ok = port
            .parse::<u32>()
            .map(|p| (1..=65535).contains(&p))
            .unwrap_or(false);
        if !ok {
            return Err(invalid(message));
        }
    }
    Ok(canonical_key(value))
}

/// Validate a representation filter.
pub fn representation(value: &str) -> Result<String, StoreError> {
    if !REPRESENTATIONS.contains(&value) {
        return Err(invalid(
            "representation must be all, static, rendered, legacy_fragment, or legacy_unknown",
        ));
    }
    Ok(value.to_string())
}

/// Scan header and coverage for a link query, including `config` and `start_url`.
pub fn header(con: &Connection, representation: &str) -> Result<Map<String, Value>, StoreError> {
    let row = con.query_row(
        "SELECT scan_uuid,evidence_revision,lifecycle,finish_reason,crawl_partial,\
         capabilities_json,limitations_json,config_json,start_url FROM scan WHERE singleton=1",
        [],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, Option<String>>(8)?,
            ))
        },
    )?;
    let (
        scan_uuid,
        evidence_revision,
        lifecycle,
        _finish,
        crawl_partial,
        capabilities,
        limitations,
        config,
        start_url,
    ) = row;
    let capabilities: Value = serde_json::from_str(&capabilities)?;
    let links = capabilities.get("links").cloned().unwrap_or_else(
        || json!({"state": "unavailable", "reason": "link capability was not recorded"}),
    );
    let link_state = links
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("unavailable")
        .to_string();
    let mut reasons: Vec<String> = Vec::new();
    if lifecycle != "finished" {
        reasons.push(format!("scan lifecycle is {lifecycle}"));
    }
    if crawl_partial != 0 {
        reasons.push("crawl is partial".into());
    }
    if link_state != "complete" {
        let reason = links
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("link capability is {link_state}"));
        reasons.push(reason);
    }
    let limitations: Vec<String> = serde_json::from_str(&limitations)?;
    reasons.extend(
        limitations
            .into_iter()
            .filter(|reason| reason.starts_with("link_observations_omitted")),
    );
    if representation != "all" {
        let measured: Option<i64> = con
            .query_row(
                "SELECT 1 FROM links WHERE evidence_representation=?1 LIMIT 1",
                params![representation],
                |r| r.get(0),
            )
            .optional()?;
        let mut captured = measured.is_some();
        if !captured && representation != "legacy_unknown" {
            let page: Option<i64> = con
                .query_row(
                    "SELECT 1 FROM pages WHERE representation=?1 LIMIT 1",
                    params![representation],
                    |r| r.get(0),
                )
                .optional()?;
            captured = page.is_some();
        }
        if !captured {
            reasons.push(format!("{representation} representation was not captured"));
        }
    }
    let state = if link_state == "unavailable"
        || reasons
            .iter()
            .any(|reason| reason.ends_with("representation was not captured"))
    {
        "unavailable"
    } else if !reasons.is_empty() {
        "partial"
    } else {
        "observed_scope_only"
    };
    let config: Value = serde_json::from_str(&config)?;
    let mut coverage = Map::new();
    coverage.insert("state".into(), json!(state));
    coverage.insert("reasons".into(), json!(reasons));
    coverage.insert("global_reachability".into(), json!("unknown"));
    coverage.insert(
        "scope".into(),
        json!("retained links in this scan and selected representation"),
    );
    let mut out = Map::new();
    out.insert("scan_uuid".into(), json!(scan_uuid));
    out.insert("evidence_revision".into(), json!(evidence_revision));
    out.insert("coverage".into(), Value::Object(coverage));
    out.insert("config".into(), config);
    out.insert("start_url".into(), json!(start_url));
    Ok(out)
}

/// The crawler treats an empty root path and slash as one page identity.
pub fn spellings(key: &str) -> (String, String) {
    let (base, query) = match key.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (key, None),
    };
    let path_start = base.find("://").map(|i| i + 3).unwrap_or(0);
    let is_root = base[path_start..].find('/').is_some_and(|i| {
        let path = &base[path_start + i..];
        path == "/"
    });
    if !is_root {
        return (key.to_string(), key.to_string());
    }
    let netloc_end = path_start + base[path_start..].find('/').unwrap_or(0);
    let scheme_netloc = &base[..netloc_end];
    let alternate = match query {
        Some(q) => format!("{scheme_netloc}?{q}"),
        None => scheme_netloc.to_string(),
    };
    (key.to_string(), alternate)
}

/// Find fetched source pages without loading a site-wide URL map.
pub fn source_ids(con: &Connection, key: &str) -> Result<Vec<i64>, StoreError> {
    let (first, second) = spellings(key);
    let mut statement = con.prepare(
        "SELECT p.url_id FROM pages p JOIN urls u USING(url_id) \
         WHERE u.url IN (?1,?2) ORDER BY p.page_ordinal,p.url_id",
    )?;
    let rows = statement.query_map(params![first, second], |r| r.get::<_, i64>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// One stored occurrence with its URLs and classification status.
pub fn edge(con: &Connection, link_id: i64) -> Result<Map<String, Value>, StoreError> {
    let columns = [
        "link_id",
        "source_url_id",
        "destination_url_id",
        "source_document_id",
        "evidence_representation",
        "ordinal",
        "anchor",
        "nofollow",
        "position",
        "source_url",
        "destination_url",
    ];
    let row = con
        .query_row(
            "SELECT l.link_id,l.source_url_id,l.destination_url_id,l.source_document_id,\
             l.evidence_representation,l.ordinal,l.anchor,l.nofollow,l.position,\
             s.url AS source_url,d.url AS destination_url \
             FROM links l JOIN urls s ON s.url_id=l.source_url_id \
             JOIN urls d ON d.url_id=l.destination_url_id WHERE l.link_id=?1",
            params![link_id],
            |r| row_map(r, &columns),
        )
        .optional()?;
    let Some(mut item) = row else {
        return Err(invalid("observed path refers to a missing link record"));
    };
    let nofollow = item.get("nofollow").and_then(Value::as_i64).unwrap_or(0) != 0;
    let position = item
        .get("position")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
        .map(str::to_owned);
    item.insert("nofollow".into(), json!(nofollow));
    item.insert(
        "position".into(),
        position.as_ref().map_or(Value::Null, |p| json!(p)),
    );
    item.insert(
        "position_status".into(),
        json!(if position.is_some() {
            "classified"
        } else {
            "not_classified"
        }),
    );
    Ok(item)
}

/// Predecessor record of one visited URL during the search.
struct Parent {
    source: Option<String>,
    link_id: Option<i64>,
    depth: i64,
}

/// Find one shortest retained path; no result asserts global orphanhood.
///
/// FIFO source expansion and source-local representation/ordinal/link-id order
/// choose the first parent for equal-length routes. A source is looked up in
/// indexed ``urls``/``pages`` and expanded through the existing links source
/// index, so the whole graph is never reconstructed for a single query.
/// Defaults: representation `all`, 10000 nodes, 200000 edges, depth 20, 15 s timeout.
#[allow(clippy::too_many_arguments)]
pub fn shortest_observed_path(
    scan_path: &Path,
    seed: &str,
    target: &str,
    representation: &str,
    max_nodes: i64,
    max_edges: i64,
    max_depth: i64,
    timeout_seconds: f64,
) -> Result<Map<String, Value>, StoreError> {
    let seed_key = url(seed, "seed")?;
    let target_key = url(target, "target")?;
    let representation = self::representation(representation)?;
    if !(1..=100_000).contains(&max_nodes)
        || !(1..=2_000_000).contains(&max_edges)
        || !(0..=100).contains(&max_depth)
        || !(timeout_seconds > 0.0 && timeout_seconds <= 30.0)
    {
        return Err(invalid(
            "invalid path budget; nodes 1..100000, edges 1..2000000, depth 0..100, timeout 0..30s",
        ));
    }
    let deadline = Instant::now() + Duration::from_secs_f64(timeout_seconds);
    let (con, validation) = open_light(scan_path)?;
    let mut header = header(&con, &representation)?;
    let config = header.remove("config").unwrap_or(Value::Null);
    let scope_map: Option<BTreeMap<String, Value>> = config
        .get("scope")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
    let scope = Scope::from_config(scope_map.as_ref());
    let scan_start = header
        .remove("start_url")
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| seed_key.clone());
    let start_host = host_of(&scan_start);
    let start_key = canonical_key(&scan_start);
    let coverage_state = header["coverage"]["state"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let mut result = header.clone();
    result.insert("seed".into(), json!(seed_key));
    result.insert("target".into(), json!(target_key));
    result.insert("representation".into(), json!(representation));
    result.insert("identity".into(), json!("fragmentless exact crawler URL"));
    result.insert(
        "policy".into(),
        json!("internal followed hyperlinks; static then rendered then legacy_fragment then legacy_unknown"),
    );
    result.insert(
        "limits".into(),
        json!({"max_nodes": max_nodes, "max_edges": max_edges, "max_depth": max_depth}),
    );
    result.insert("visited_nodes".into(), json!(0));
    result.insert("examined_edges".into(), json!(0));
    result.insert("hops".into(), json!([]));
    result.insert("validation".into(), json!(validation));
    if source_ids(&con, &seed_key)?.is_empty() {
        result.insert("state".into(), json!("seed_unobserved"));
        result.insert(
            "reason".into(),
            json!("seed is not a retained page in this scan"),
        );
        return Ok(result);
    }
    if coverage_state == "unavailable" && seed_key != target_key {
        result.insert("state".into(), json!("unavailable"));
        result.insert(
            "reason".into(),
            json!("selected link evidence is unavailable"),
        );
        return Ok(result);
    }
    let mut parents: HashMap<String, Parent> = HashMap::new();
    parents.insert(
        seed_key.clone(),
        Parent {
            source: None,
            link_id: None,
            depth: 0,
        },
    );
    let mut queue: VecDeque<String> = VecDeque::from([seed_key.clone()]);
    let mut examined: i64 = 0;
    let mut depth_limited = false;
    let mut limit_reason = String::new();
    let mut state = if seed_key == target_key {
        "found".to_string()
    } else {
        "unreachable_in_observed_graph".to_string()
    };
    'search: while let Some(source) = queue.pop_front() {
        if state == "found" {
            break;
        }
        if Instant::now() >= deadline {
            state = "limit_reached".into();
            limit_reason = "time_budget_exhausted".into();
            break;
        }
        let depth = parents[&source].depth;
        if depth >= max_depth {
            depth_limited = true;
            continue;
        }
        for source_id in source_ids(&con, &source)? {
            let (where_clause, mut bindings): (&str, Vec<SqlValue>) = if representation != "all" {
                (
                    "AND l.evidence_representation=?2",
                    vec![
                        SqlValue::Integer(source_id),
                        SqlValue::Text(representation.clone()),
                    ],
                )
            } else {
                ("", vec![SqlValue::Integer(source_id)])
            };
            let sql = format!(
                "SELECT l.link_id,l.nofollow,d.url AS destination_url \
                 FROM links l JOIN urls d ON d.url_id=l.destination_url_id \
                 WHERE l.source_url_id=?1 {where_clause} ORDER BY {ORDER}"
            );
            let mut statement = con.prepare(&sql)?;
            let rows: Vec<(i64, i64, String)> = statement
                .query_map(params_from_iter(bindings.drain(..)), |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?
                .collect::<Result<_, _>>()?;
            for (link_id, nofollow, destination_url) in rows {
                if Instant::now() >= deadline {
                    state = "limit_reached".into();
                    limit_reason = "time_budget_exhausted".into();
                    break 'search;
                }
                if examined >= max_edges {
                    state = "limit_reached".into();
                    limit_reason = "edge_budget_exhausted".into();
                    break 'search;
                }
                examined += 1;
                let destination = canonical_key(&destination_url);
                if nofollow != 0
                    || (destination != start_key
                        && !scope.rejection(&destination, &start_host).is_empty())
                {
                    continue;
                }
                if parents.contains_key(&destination) {
                    continue;
                }
                if parents.len() as i64 >= max_nodes {
                    state = "limit_reached".into();
                    limit_reason = "node_budget_exhausted".into();
                    break 'search;
                }
                parents.insert(
                    destination.clone(),
                    Parent {
                        source: Some(source.clone()),
                        link_id: Some(link_id),
                        depth: depth + 1,
                    },
                );
                queue.push_back(destination.clone());
                if destination == target_key {
                    state = "found".into();
                    break 'search;
                }
            }
        }
    }
    if state == "unreachable_in_observed_graph" && depth_limited {
        state = "limit_reached".into();
        limit_reason = "depth_budget_exhausted".into();
    }
    result.insert("state".into(), json!(state));
    if !limit_reason.is_empty() {
        result.insert("reason".into(), json!(limit_reason));
    }
    result.insert("visited_nodes".into(), json!(parents.len()));
    result.insert("examined_edges".into(), json!(examined));
    if state == "found" {
        let mut path = Vec::new();
        let mut node = target_key.clone();
        while node != seed_key {
            let parent = &parents[&node];
            let (Some(source), Some(link_id)) = (parent.source.clone(), parent.link_id) else {
                return Err(invalid("observed path predecessor is invalid"));
            };
            path.push(Value::Object(edge(&con, link_id)?));
            node = source;
        }
        path.reverse();
        result.insert("hops".into(), Value::Array(path));
    }
    Ok(result)
}

/// Host part of an absolute URL, lowercased; empty when absent.
fn host_of(url: &str) -> String {
    let Some((_, rest)) = url.split_once("://") else {
        return String::new();
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    host_port.split(':').next().unwrap_or("").to_lowercase()
}

fn b64url_encode(raw: &[u8]) -> String {
    let mut out = String::new();
    for chunk in raw.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let symbols = [(n >> 18) & 63, (n >> 12) & 63, (n >> 6) & 63, n & 63];
        let take = chunk.len() + 1;
        for symbol in symbols.iter().take(take) {
            out.push(ALPHABET[*symbol as usize] as char);
        }
    }
    out
}

fn b64url_decode(text: &str) -> Option<Vec<u8>> {
    if text.len() % 4 == 1 {
        return None;
    }
    let mut bits: u32 = 0;
    let mut count = 0;
    let mut out = Vec::new();
    for ch in text.bytes() {
        let value = ALPHABET.iter().position(|&c| c == ch)? as u32;
        bits = (bits << 6) | value;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push(((bits >> count) & 0xff) as u8);
        }
    }
    Some(out)
}

fn target_digest(target: &str) -> String {
    format!("{:x}", Sha256::digest(target.as_bytes()))
}

/// Encode an opaque cursor naming the last link ID returned for this scan and target.
pub fn encode_cursor(
    header: &Map<String, Value>,
    target: &str,
    representation: &str,
    link_id: i64,
) -> String {
    let payload = json!({
        "version": CURSOR_VERSION,
        "scan_uuid": header.get("scan_uuid").cloned().unwrap_or(Value::Null),
        "evidence_revision": header.get("evidence_revision").cloned().unwrap_or(Value::Null),
        "target_sha256": target_digest(target),
        "representation": representation,
        "last_link_id": link_id,
    });
    b64url_encode(payload.to_string().as_bytes())
}

/// Decode a cursor, returning the last link ID, or 0 when no cursor was given.
pub fn decode_cursor(
    cursor: Option<&str>,
    header: &Map<String, Value>,
    target: &str,
    representation: &str,
) -> Result<i64, StoreError> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    let well_formed = !cursor.is_empty()
        && cursor.len() <= 2048
        && cursor
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !well_formed {
        return Err(invalid("inlink cursor is invalid"));
    }
    let raw = b64url_decode(cursor).ok_or_else(|| invalid("inlink cursor is invalid"))?;
    let value: Value =
        serde_json::from_slice(&raw).map_err(|_| invalid("inlink cursor is invalid"))?;
    let Some(obj) = value.as_object() else {
        return Err(invalid("inlink cursor is invalid"));
    };
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let last = obj.get("last_link_id").and_then(Value::as_i64);
    if keys
        != [
            "evidence_revision",
            "last_link_id",
            "representation",
            "scan_uuid",
            "target_sha256",
            "version",
        ]
        || obj.get("version").and_then(Value::as_str) != Some(CURSOR_VERSION)
        || last.is_none_or(|v| v < 1)
        || !obj.get("evidence_revision").is_some_and(Value::is_i64)
    {
        return Err(invalid("inlink cursor is invalid"));
    }
    let binds = obj.get("scan_uuid") == header.get("scan_uuid")
        && obj.get("evidence_revision") == header.get("evidence_revision")
        && obj.get("target_sha256").and_then(Value::as_str) == Some(target_digest(target).as_str())
        && obj.get("representation").and_then(Value::as_str) == Some(representation);
    if !binds {
        return Err(invalid(
            "inlink cursor belongs to a different scan, revision, target, or representation",
        ));
    }
    Ok(last.unwrap_or(0))
}

/// Page retained inlinks by stable link ID without an unbounded count scan.
/// Defaults: representation `all`, first page, limit 100, 1 MiB byte cap, 15 s timeout.
#[allow(clippy::too_many_arguments)]
pub fn reverse_inlinks(
    scan_path: &Path,
    target: &str,
    representation: &str,
    cursor: Option<&str>,
    limit: i64,
    max_bytes: i64,
    timeout_seconds: f64,
) -> Result<Map<String, Value>, StoreError> {
    let target_key = url(target, "target")?;
    let representation = self::representation(representation)?;
    if !(1..=500).contains(&limit)
        || !(4096..=8_388_608).contains(&max_bytes)
        || !(timeout_seconds > 0.0 && timeout_seconds <= 30.0)
    {
        return Err(invalid(
            "invalid inlink page; limit 1..500, max_bytes 4096..8388608, timeout 0..30s",
        ));
    }
    let (con, validation) = open_light(scan_path)?;
    let mut header = header(&con, &representation)?;
    header.remove("config");
    header.remove("start_url");
    let after_link_id = decode_cursor(cursor, &header, &target_key, &representation)?;
    let where_clause = if representation != "all" {
        "AND l.evidence_representation=?8"
    } else {
        ""
    };
    let (first, second) = spellings(&target_key);
    let mut bindings: Vec<SqlValue> = vec![
        SqlValue::Text(first.clone()),
        SqlValue::Text(second.clone()),
        SqlValue::Text(format!("{first}#")),
        SqlValue::Text(format!("{first}$")),
        SqlValue::Text(format!("{second}#")),
        SqlValue::Text(format!("{second}$")),
        SqlValue::Integer(after_link_id),
    ];
    if !where_clause.is_empty() {
        bindings.push(SqlValue::Text(representation.clone()));
    }
    bindings.push(SqlValue::Integer(limit + 1));
    let _deadline = arm_deadline(&con, Duration::from_secs_f64(timeout_seconds));
    let mut items: Vec<Value> = Vec::new();
    let mut used: i64 = 0;
    let mut has_more = false;
    let mut state = "complete".to_string();
    let sql = format!(
        "SELECT l.link_id FROM (SELECT url_id FROM urls WHERE url IN (?1,?2) \
         OR (url>=?3 AND url<?4) OR (url>=?5 AND url<?6)) d \
         CROSS JOIN links l INDEXED BY links_destination_source_position \
         WHERE l.destination_url_id=d.url_id AND l.link_id>?7 \
         {where_clause} ORDER BY l.link_id LIMIT ?{}",
        bindings.len()
    );
    let link_ids: Result<Vec<i64>, rusqlite::Error> = (|| {
        let mut statement = con.prepare(&sql)?;
        let collected = statement
            .query_map(params_from_iter(bindings.iter()), |r| r.get::<_, i64>(0))?
            .collect();
        collected
    })();
    match link_ids {
        Ok(ids) => {
            for link_id in ids {
                if items.len() as i64 == limit {
                    has_more = true;
                    break;
                }
                let item = Value::Object(edge(&con, link_id)?);
                let size = spiderhead_core::common::python_json(&item).len() as i64;
                if used + size > max_bytes {
                    if items.is_empty() {
                        return Err(invalid("one inlink record exceeds max_bytes"));
                    }
                    has_more = true;
                    break;
                }
                items.push(item);
                used += size;
            }
        }
        Err(error) => {
            if !error.to_string().to_lowercase().contains("interrupted") {
                return Err(error.into());
            }
            state = "limit_reached".into();
            has_more = true;
        }
    }
    let next_cursor = if has_more {
        match items
            .last()
            .and_then(|i| i.get("link_id"))
            .and_then(Value::as_i64)
        {
            Some(last) => Some(encode_cursor(&header, &target_key, &representation, last)),
            None => cursor.map(str::to_owned),
        }
    } else {
        None
    };
    let mut out = header.clone();
    out.insert("target".into(), json!(target_key));
    out.insert("representation".into(), json!(representation));
    out.insert(
        "identity".into(),
        json!("exact destination URL plus its fragment occurrences"),
    );
    out.insert("cursor".into(), json!(cursor));
    out.insert("limit".into(), json!(limit));
    out.insert("max_bytes".into(), json!(max_bytes));
    out.insert("bytes".into(), json!(used));
    out.insert("state".into(), json!(state));
    out.insert(
        "reason".into(),
        json!(if state == "limit_reached" {
            "time_budget_exhausted"
        } else {
            ""
        }),
    );
    let returned = items.len();
    out.insert("items".into(), Value::Array(items));
    out.insert("returned".into(), json!(returned));
    out.insert("has_more".into(), json!(has_more));
    out.insert(
        "next_cursor".into(),
        next_cursor.map_or(Value::Null, |c| json!(c)),
    );
    out.insert("validation".into(), json!(validation));
    Ok(out)
}

/// Compile-time check that the cursor alphabet constant matches the documented regex.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trip_and_alphabet() {
        assert_eq!(CURSOR_CHARS, r"[A-Za-z0-9_-]{1,2048}\Z");
        let raw = b"{\"a\":1}";
        assert_eq!(
            b64url_decode(&b64url_encode(raw)).as_deref(),
            Some(&raw[..])
        );
    }
}
