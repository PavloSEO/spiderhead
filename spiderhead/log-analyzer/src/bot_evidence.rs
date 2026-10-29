//! Paged per-IP bot claims with explicit, cached DNS evidence read from a retained artifact.

use rusqlite::functions::FunctionFlags;
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Map, Value};

use crate::analyze::check_range;
use crate::dns::{Verifier, DNS_CACHE_TTL};
use crate::error::{LogsError, Result};
use crate::net::{parse_ip, Network};
use crate::store::open_log_source;

fn unchecked(reason: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("verified".into(), Value::Null);
    m.insert("hostname".into(), Value::Null);
    m.insert("forward_addresses".into(), json!([]));
    m.insert("forward_match".into(), Value::Null);
    m.insert("verdict".into(), json!("unchecked"));
    m.insert("cached".into(), json!(false));
    m.insert("checked_at".into(), Value::Null);
    m.insert("reason".into(), json!(reason));
    m
}

/// Interpret a persisted DNS payload: fresh, bounded and well-formed evidence becomes a
/// cached result; anything else is "no evidence", never a verdict.
pub(crate) fn cached_evidence(
    payload: Option<&str>,
    checked_at: Option<f64>,
    now: f64,
) -> Map<String, Value> {
    let missing = || unchecked("No fresh DNS evidence; explicit verification is required");
    let (Some(payload), Some(stamp)) = (payload, checked_at) else {
        return missing();
    };
    if payload.chars().count() > 65_536 || !(0.0..DNS_CACHE_TTL).contains(&(now - stamp)) {
        return missing();
    }
    let Ok(Value::Object(result)) = serde_json::from_str::<Value>(payload) else {
        return missing();
    };
    let verified = result.get("verified");
    if !matches!(verified, None | Some(Value::Null | Value::Bool(_))) {
        return missing();
    }
    let mut out = Map::new();
    for key in [
        "verified",
        "hostname",
        "forward_addresses",
        "forward_match",
        "reason",
    ] {
        out.insert(key.into(), result.get(key).cloned().unwrap_or(Value::Null));
    }
    out.insert("cached".into(), json!(true));
    out.insert("checked_at".into(), json!(stamp));
    let verdict = match verified {
        Some(Value::Bool(true)) => "verified",
        Some(Value::Bool(false)) => "fake",
        _ => "unchecked",
    };
    out.insert("verdict".into(), json!(verdict));
    out
}

/// Request for [`bot_verification`].
#[derive(Debug, Clone)]
pub struct BotQuery {
    /// Run reverse DNS for the uncached rows of the page (network access, writes the cache).
    pub verify_bots: bool,
    pub limit: i64,
    pub offset: i64,
    pub bot: String,
    pub family: String,
    pub ip: String,
    /// Only rows whose cached verdict is `verified`, `fake` or `unchecked`.
    pub verdict: String,
    /// Fresh DNS lookups allowed for this page (0..=100).
    pub max_dns: i64,
}

impl Default for BotQuery {
    fn default() -> Self {
        Self {
            verify_bots: false,
            limit: 100,
            offset: 0,
            bot: String::new(),
            family: String::new(),
            ip: String::new(),
            verdict: String::new(),
            max_dns: 20,
        }
    }
}

/// Read exact paged IP records. Network use and cache writes require `verify_bots`.
///
/// Argument errors are errors; unreadable evidence comes back as `{"ok": false}` data.
pub fn bot_verification(
    source: &str,
    q: &BotQuery,
    verifier: &Verifier,
    now: f64,
) -> Result<Value> {
    check_range("limit", q.limit, 1, 500)?;
    check_range("offset", q.offset, 0, 100_000_000)?;
    check_range("max_dns", q.max_dns, 0, 100)?;
    if !["", "verified", "fake", "unchecked"].contains(&q.verdict.as_str()) {
        return Err(LogsError::value(
            "verdict must be verified, fake or unchecked",
        ));
    }
    if q.verify_bots && !q.verdict.is_empty() {
        return Err(LogsError::value(
            "DNS verification with a cached verdict filter is not supported; verify an unfiltered page first",
        ));
    }
    let network = (!q.ip.is_empty())
        .then(|| Network::parse(&q.ip))
        .transpose()?;
    match read(source, q, network, verifier, now) {
        Ok(value) => Ok(value),
        Err(error) => Ok(json!({
            "ok": false, "source": source,
            "error": format!("Log bot evidence unavailable: {error}"),
        })),
    }
}

fn read(
    source: &str,
    q: &BotQuery,
    network: Option<Network>,
    verifier: &Verifier,
    now: f64,
) -> Result<Value> {
    let con = open_log_source(source)?;
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<SqlValue> = Vec::new();
    for (column, value) in [("b.bot", &q.bot), ("b.family", &q.family)] {
        if !value.is_empty() {
            clauses.push(format!("{column}=?"));
            params.push(SqlValue::Text(value.clone()));
        }
    }
    if let Some(net) = network {
        con.create_scalar_function("ip_in_subnet", 1, FunctionFlags::SQLITE_UTF8, move |ctx| {
            let value: String = ctx.get(0)?;
            Ok(i64::from(parse_ip(&value).is_some_and(|a| net.contains(a))))
        })?;
        clauses.push("ip_in_subnet(b.ip)=1".into());
    }
    let mut where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    let joined = " FROM bot_ips b LEFT JOIN dns_cache c ON b.ip=c.ip AND b.bot=c.bot";
    let payload = "CASE WHEN typeof(c.payload)='text' AND length(CAST(c.payload AS BLOB))<=65536 THEN c.payload ELSE NULL END";
    if !q.verdict.is_empty() {
        con.create_scalar_function(
            "cached_verdict",
            2,
            FunctionFlags::SQLITE_UTF8,
            move |ctx| {
                let text: Option<String> = ctx.get(0)?;
                let stamp: Option<f64> = match ctx.get_raw(1) {
                    ValueRef::Integer(n) => Some(n as f64),
                    ValueRef::Real(f) => Some(f),
                    _ => None,
                };
                Ok(cached_evidence(text.as_deref(), stamp, now)["verdict"]
                    .as_str()
                    .unwrap_or("unchecked")
                    .to_string())
            },
        )?;
        where_sql.push_str(if where_sql.is_empty() {
            " WHERE "
        } else {
            " AND "
        });
        where_sql.push_str(&format!("cached_verdict({payload},c.checked_at)=?"));
        params.push(SqlValue::Text(q.verdict.clone()));
    }
    let refs: Vec<&dyn rusqlite::ToSql> =
        params.iter().map(|p| p as &dyn rusqlite::ToSql).collect();
    let unique_ips: i64 = con.query_row(
        &format!("SELECT count(DISTINCT b.ip){joined}{where_sql}"),
        refs.as_slice(),
        |r| r.get(0),
    )?;
    let query = format!(
        "SELECT b.*, {payload} AS payload, c.checked_at{joined}{where_sql} ORDER BY b.bot,b.ip"
    );
    let mut stmt = con.prepare(&query)?;
    let mut records = stmt.query(refs.as_slice())?;
    let (offset, limit) = (q.offset as u64, q.limit as u64);
    let mut rows: Vec<Map<String, Value>> = Vec::new();
    let mut total = 0u64;
    while let Some(r) = records.next()? {
        let text: Option<String> = r.get(5)?;
        let stamp = match r.get_ref(6)? {
            ValueRef::Integer(n) => Some(n as f64),
            ValueRef::Real(f) => Some(f),
            _ => None,
        };
        let cached = cached_evidence(text.as_deref(), stamp, now);
        if !q.verdict.is_empty() && cached["verdict"] != json!(q.verdict) {
            continue;
        }
        if offset <= total && total < offset + limit {
            let mut row = Map::new();
            row.insert("bot".into(), json!(r.get::<_, String>(0)?));
            row.insert("family".into(), json!(r.get::<_, String>(1)?));
            row.insert("ip".into(), json!(r.get::<_, String>(2)?));
            row.insert("visits".into(), json!(r.get::<_, i64>(3)?));
            row.insert("last_visit".into(), json!(r.get::<_, Option<String>>(4)?));
            row.extend(cached);
            rows.push(row);
        }
        total += 1;
    }
    drop(records);
    drop(stmt);
    let mut verification = Map::new();
    verification.insert("checked".into(), json!(false));
    verification.insert("dns_checks".into(), json!(0));
    verification.insert("canary_requests".into(), json!(0));
    if q.verify_bots {
        let pending: Vec<Map<String, Value>> = rows
            .iter()
            .filter(|r| r["cached"] != json!(true))
            .cloned()
            .collect();
        let outcome = verifier.verify_rows(&pending, true, q.max_dns as usize);
        let checks: Vec<Map<String, Value>> = outcome["checks"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_object().cloned()).collect())
            .unwrap_or_default();
        let key = |r: &Map<String, Value>| (r["bot"].clone(), r["ip"].clone());
        for row in rows.iter_mut() {
            if let Some(fresh) = checks.iter().find(|c| key(c) == key(row)) {
                *row = fresh.clone();
            }
        }
        let updates: Vec<&Map<String, Value>> = checks
            .iter()
            .filter(|c| c.get("checked_at").is_some_and(|v| !v.is_null()))
            .collect();
        if !updates.is_empty() {
            drop(con);
            persist(source, &updates)?;
        }
        verification = outcome.into_iter().filter(|(k, _)| k != "checks").collect();
    }
    let shown = rows.len() as u64;
    let more = total > offset + shown;
    Ok(json!({
        "ok": true, "source": source, "rows": rows, "total": total, "unique_ips": unique_ips,
        "offset": q.offset, "limit": q.limit, "has_more": more,
        "next_offset": if more { json!(offset + shown) } else { Value::Null },
        "verification": verification,
        "identity_note": "Bot names are user-agent claims; unchecked is not an authenticity verdict",
    }))
}

/// Store fresh evidence in the artifact's DNS cache. The artifact is revalidated and never
/// created: a missing or foreign file fails instead of being replaced.
pub(crate) fn persist(source: &str, updates: &[&Map<String, Value>]) -> Result<()> {
    drop(open_log_source(source)?);
    let path = std::fs::canonicalize(source)?;
    let writable = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    let mut insert = writable.prepare(
        "INSERT INTO dns_cache VALUES(?,?,?,?) ON CONFLICT(ip,bot) DO UPDATE SET payload=excluded.payload,checked_at=excluded.checked_at",
    )?;
    for row in updates {
        let body: Map<String, Value> = row
            .iter()
            .filter(|(k, _)| !["visits", "last_visit", "bot", "family", "ip"].contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        insert.execute(params![
            row["ip"].as_str(),
            row["bot"].as_str(),
            serde_json::to_string(&body).map_err(|e| LogsError::value(e.to_string()))?,
            row["checked_at"].as_f64(),
        ])?;
    }
    Ok(())
}
