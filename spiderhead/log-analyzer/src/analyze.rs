//! One-shot analysis of a single access log: how crawlers use the site, by family, status,
//! section and path, with optional per-IP authenticity checks.

use std::path::Path;

use indexmap::IndexMap;
use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};

use crate::bots::detect_bot;
use crate::clock::{iso_date, iso_utc};
use crate::dns::Verifier;
use crate::error::{os_message, LogsError, Result};
use crate::format::{detect_format, section, Format, LineParser};
use crate::net::{parse_ip, Network};
use crate::reader::{open_log, Lines};
use crate::text::strip;

/// Distinct paths tracked per family before new ones are refused.
pub const MAX_TRACKED_PATHS: usize = 5000;

/// Knobs of [`analyze_log`]; every field is bounds-checked.
#[derive(Debug, Clone)]
pub struct AnalyzeOptions {
    /// Run reverse-DNS verification on the returned IP page (network access).
    pub verify_bots: bool,
    /// Stop reading after this many non-blank lines (1..=100_000_000).
    pub max_lines: i64,
    /// Lines inspected to detect the format (1..=1000).
    pub sample_size: i64,
    /// First IP row of the page (0..=100_000_000).
    pub offset: i64,
    /// IP rows per page (1..=500).
    pub limit: i64,
    /// Fresh DNS lookups allowed for this page (0..=100).
    pub max_dns: i64,
    /// Restrict IP rows to a bot name or family.
    pub bot: Option<String>,
    /// Restrict IP rows to a CIDR network.
    pub ip: Option<String>,
    /// Cap on tracked distinct paths per family.
    pub max_tracked_paths: usize,
}

impl Default for AnalyzeOptions {
    fn default() -> Self {
        Self {
            verify_bots: false,
            max_lines: 5_000_000,
            sample_size: 50,
            offset: 0,
            limit: 100,
            max_dns: 20,
            bot: None,
            ip: None,
            max_tracked_paths: MAX_TRACKED_PATHS,
        }
    }
}

type Counter = IndexMap<String, u64>;

/// Entries by descending count; ties keep first-seen order.
pub fn most_common(counter: &Counter, n: usize) -> Vec<(&String, &u64)> {
    let mut items: Vec<(&String, &u64)> = counter.iter().collect();
    items.sort_by(|a, b| b.1.cmp(a.1));
    items.truncate(n);
    items
}

fn bump(counter: &mut Counter, key: &str, by: u64) {
    if let Some(v) = counter.get_mut(key) {
        *v += by;
    } else {
        counter.insert(key.to_string(), by);
    }
}

fn counter_json(counter: &Counter, n: usize) -> Value {
    Value::Object(
        most_common(counter, n)
            .into_iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect(),
    )
}

/// Bounds check shared by the paged entry points.
pub fn check_range(name: &str, value: i64, min: i64, max: i64) -> Result<()> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(LogsError::value(format!(
            "{name} must be an integer in {min}..{max}"
        )))
    }
}

fn failure(path: &str, error: impl std::fmt::Display) -> Value {
    json!({"ok": false, "path": path, "error": error.to_string()})
}

/// Parse a log file and compute aggregates; only bot verification uses the network.
///
/// Read problems come back as `{"ok": false, "error": ...}` data; an out-of-range option or
/// an unreadable request-time token is an error.
pub fn analyze_log(path: &str, options: &AnalyzeOptions, verifier: &Verifier) -> Result<Value> {
    validate(options)?;
    let network = options
        .ip
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(Network::parse)
        .transpose()?;
    match open_log(Path::new(path)) {
        Ok(handle) => analyze_lines(Lines::new(handle), path, options, network, verifier),
        Err(error) => Ok(failure(path, os_message(&error, Some(path)))),
    }
}

fn validate(o: &AnalyzeOptions) -> Result<()> {
    check_range("max_lines", o.max_lines, 1, 100_000_000)?;
    check_range("sample_size", o.sample_size, 1, 1000)?;
    check_range("offset", o.offset, 0, 100_000_000)?;
    check_range("limit", o.limit, 1, 500)?;
    check_range("max_dns", o.max_dns, 0, 100)
}

/// Analysis over an already-open line source; `path` is only echoed in the report.
pub fn analyze_lines<I>(
    lines: I,
    path: &str,
    o: &AnalyzeOptions,
    network: Option<Network>,
    verifier: &Verifier,
) -> Result<Value>
where
    I: Iterator<Item = Result<String>>,
{
    match run(lines, path, o, network, verifier) {
        Err(LogsError::Io(text) | LogsError::Sqlite(text)) => {
            // A truncated or corrupt archive fails mid-stream; partial counts from it must
            // never be presented as a complete analysis.
            Ok(failure(path, format!("Cannot read log: {text}")))
        }
        other => other,
    }
}

#[derive(Default)]
struct Totals {
    total: u64,
    parsed: u64,
    skipped: u64,
    unknown_agent: u64,
    truncated: bool,
    first: Option<i64>,
    last: Option<i64>,
}

fn run<I>(
    mut lines: I,
    path: &str,
    o: &AnalyzeOptions,
    network: Option<Network>,
    verifier: &Verifier,
) -> Result<Value>
where
    I: Iterator<Item = Result<String>>,
{
    let mut sample: Vec<String> = Vec::new();
    for _ in 0..o.sample_size {
        match lines.next() {
            Some(line) => sample.push(line?),
            None => break,
        }
    }
    let Some(format) = detect_format(sample.iter().map(String::as_str)) else {
        return Ok(failure(
            path,
            "Unrecognized log format: expected Apache Common/Combined or IIS W3C",
        ));
    };
    let ips = Connection::open("")?;
    ips.execute_batch(
        "CREATE TABLE ips (bot TEXT, family TEXT, ip TEXT, visits INTEGER, last_visit TEXT, PRIMARY KEY(bot, ip))",
    )?;
    let mut parser = LineParser::for_format(format);
    let mut t = Totals::default();
    let mut by_family: IndexMap<String, Counter> = IndexMap::new();
    let mut bot_hits = Counter::new();
    let mut bot_bytes: std::collections::HashMap<String, i64> = Default::default();
    let mut status_by_family: IndexMap<String, std::collections::BTreeMap<i64, u64>> =
        IndexMap::new();
    let mut section_by_family: IndexMap<String, Counter> = IndexMap::new();
    let mut paths_by_family: IndexMap<String, Counter> = IndexMap::new();
    let mut paths_truncated: std::collections::BTreeSet<String> = Default::default();
    let mut daily: std::collections::BTreeMap<String, u64> = Default::default();

    let all = sample.into_iter().map(Ok).chain(lines);
    for line in all {
        let line = line?;
        let line = line.strip_suffix('\n').unwrap_or(&line);
        if strip(line).is_empty() {
            continue;
        }
        if t.total >= o.max_lines as u64 {
            t.truncated = true;
            break;
        }
        t.total += 1;
        let Some(row) = parser.parse(line)? else {
            t.skipped += 1;
            continue;
        };
        t.parsed += 1;
        if row.user_agent.is_none() {
            t.unknown_agent += 1;
        }
        let bot = detect_bot(row.user_agent.as_deref());
        let fallback = if row.user_agent.is_some() {
            "human"
        } else {
            "unknown"
        };
        let (family, name) = match &bot {
            Some(b) => (b.family, b.name),
            None => (fallback, fallback),
        };
        bump(by_family.entry(family.into()).or_default(), name, 1);
        *status_by_family
            .entry(family.into())
            .or_default()
            .entry(row.status)
            .or_insert(0) += 1;
        bump(
            section_by_family.entry(family.into()).or_default(),
            &section(&row.path),
            1,
        );
        // A full counter keeps incrementing paths it already tracks; only a brand-new path
        // is refused, so a family's top paths never fall out of sync with its total.
        let family_paths = paths_by_family.entry(family.into()).or_default();
        if family_paths.contains_key(&row.path) || family_paths.len() < o.max_tracked_paths {
            bump(family_paths, &row.path, 1);
        } else {
            paths_truncated.insert(family.to_string());
        }
        let stamp = row.time.map(iso_utc);
        if bot.is_some() {
            bump(&mut bot_hits, name, 1);
            *bot_bytes.entry(name.to_string()).or_insert(0) += row.bytes;
            ips.prepare_cached(
                "INSERT INTO ips VALUES (?, ?, ?, 1, ?) ON CONFLICT(bot, ip) DO UPDATE SET \
                 visits=visits+1, last_visit=CASE WHEN excluded.last_visit > last_visit OR last_visit IS NULL \
                 THEN excluded.last_visit ELSE last_visit END",
            )?
            .execute(params![name, family, row.ip, stamp])?;
        }
        if let Some(when) = row.time {
            bump_day(&mut daily, &iso_date(when));
            t.first = Some(t.first.map_or(when, |f| f.min(when)));
            t.last = Some(t.last.map_or(when, |l| l.max(when)));
        }
    }
    finish(
        &ips,
        path,
        o,
        format,
        network,
        verifier,
        &t,
        &by_family,
        &bot_hits,
        &bot_bytes,
        &status_by_family,
        &section_by_family,
        &paths_by_family,
        &paths_truncated,
        &daily,
    )
}

fn bump_day(daily: &mut std::collections::BTreeMap<String, u64>, day: &str) {
    *daily.entry(day.to_string()).or_insert(0) += 1;
}

#[allow(clippy::too_many_arguments)]
fn finish(
    ips: &Connection,
    path: &str,
    o: &AnalyzeOptions,
    format: Format,
    network: Option<Network>,
    verifier: &Verifier,
    t: &Totals,
    by_family: &IndexMap<String, Counter>,
    bot_hits: &Counter,
    bot_bytes: &std::collections::HashMap<String, i64>,
    status_by_family: &IndexMap<String, std::collections::BTreeMap<i64, u64>>,
    section_by_family: &IndexMap<String, Counter>,
    paths_by_family: &IndexMap<String, Counter>,
    paths_truncated: &std::collections::BTreeSet<String>,
    daily: &std::collections::BTreeMap<String, u64>,
) -> Result<Value> {
    let mut counts: std::collections::HashMap<String, u64> = Default::default();
    {
        let mut stmt = ips.prepare("SELECT bot, count(*) FROM ips GROUP BY bot")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (bot, n) = row?;
            counts.insert(bot, n as u64);
        }
    }
    let total_ips: i64 = ips.query_row("SELECT count(DISTINCT ip) FROM ips", [], |r| r.get(0))?;
    let (offset, limit) = (o.offset as u64, o.limit as u64);
    let mut selected: Vec<Map<String, Value>> = Vec::new();
    let mut matched = 0u64;
    {
        let mut stmt = ips.prepare("SELECT * FROM ips ORDER BY bot, ip")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let (name, family, address): (String, String, String) =
                (r.get(0)?, r.get(1)?, r.get(2)?);
            if let Some(filter) = o.bot.as_deref().filter(|b| !b.is_empty()) {
                if filter != name && filter != family {
                    continue;
                }
            }
            if let Some(net) = &network {
                if !parse_ip(&address).is_some_and(|a| net.contains(a)) {
                    continue;
                }
            }
            if offset <= matched && matched < offset + limit {
                let mut m = Map::new();
                m.insert("bot".into(), json!(name));
                m.insert("family".into(), json!(family));
                m.insert("ip".into(), json!(address));
                m.insert("visits".into(), json!(r.get::<_, i64>(3)?));
                m.insert("last_visit".into(), json!(r.get::<_, Option<String>>(4)?));
                selected.push(m);
            }
            matched += 1;
        }
    }
    let verification = verifier.verify_rows(&selected, o.verify_bots, o.max_dns as usize);
    let shown = selected.len() as u64;
    let more = matched > offset + shown;
    let sorted_status: Map<String, Value> = status_by_family
        .iter()
        .map(|(f, c)| {
            let inner: Map<String, Value> =
                c.iter().map(|(s, n)| (s.to_string(), json!(n))).collect();
            (f.clone(), Value::Object(inner))
        })
        .collect();
    let per_family = |source: &IndexMap<String, Counter>, n: usize| -> Value {
        Value::Object(
            source
                .iter()
                .map(|(f, c)| (f.clone(), counter_json(c, n)))
                .collect(),
        )
    };
    let bots: Vec<Value> = most_common(bot_hits, 40)
        .into_iter()
        .map(|(n, h)| {
            json!({
                "name": n, "hits": h, "bytes": bot_bytes.get(n).copied().unwrap_or(0),
                "unique_ips": counts.get(n).copied().unwrap_or(0), "unique_ips_truncated": false,
            })
        })
        .collect();
    let mut result = json!({
        "ok": true,
        "path": path,
        "format": format.name(),
        "lines": {"total": t.total, "parsed": t.parsed, "skipped": t.skipped, "truncated": t.truncated},
        "bot_detection": {
            "available": t.unknown_agent < t.parsed,
            "unknown_user_agent": t.unknown_agent,
            "reason": if t.unknown_agent > 0 { json!("User-agent fields are missing") } else { Value::Null },
        },
        "period": {"from": t.first.map(iso_utc), "to": t.last.map(iso_utc)},
        "by_family": per_family(by_family, 20),
        "bots": bots,
        "status_by_family": Value::Object(sorted_status),
        "sections_by_family": per_family(section_by_family, 20),
        "top_paths_by_family": per_family(paths_by_family, 15),
        "paths_truncated": paths_truncated.iter().collect::<Vec<_>>(),
        "ips_truncated": [],
        "unique_bot_ips": total_ips,
        "bot_ips": {
            "rows": verification.get("checks").cloned().unwrap_or(json!([])),
            "total": matched, "offset": offset, "limit": limit, "has_more": more,
            "next_offset": if more { json!(offset + shown) } else { Value::Null },
        },
        "daily": daily,
        "verification": Value::Object(verification),
    });
    let notes = findings(&result);
    result["findings"] = json!(notes);
    Ok(result)
}

/// Plain-language findings derived from a finished report.
pub fn findings(report: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let lines = &report["lines"];
    let (parsed, skipped) = (
        lines["parsed"].as_u64().unwrap_or(0),
        lines["skipped"].as_u64().unwrap_or(0),
    );
    if parsed == 0 {
        return vec![
            "No lines were parsed; the detected format is wrong or the file is empty".into(),
        ];
    }
    if skipped as f64 > parsed as f64 * 0.1 {
        out.push(format!(
            "Failed to parse {skipped} of {} lines; the log may mix formats or contain custom fields",
            lines["total"].as_u64().unwrap_or(0)
        ));
    }
    let empty = Map::new();
    let fams = report["by_family"].as_object().unwrap_or(&empty);
    let unknown = report["bot_detection"]["unknown_user_agent"]
        .as_u64()
        .unwrap_or(0);
    if unknown > 0 {
        out.push(
            "Bot classification is unavailable for rows without a user-agent; absent bot labels do not prove absent visits"
                .into(),
        );
    }
    if !fams.contains_key("googlebot") && unknown == 0 {
        out.push(
            "Googlebot does not appear in the log; the site may not be crawled, or the log period may be too short"
                .into(),
        );
    }
    if let Some(ai) = fams.get("ai").and_then(Value::as_object) {
        let total: u64 = ai.values().filter_map(Value::as_u64).sum();
        let names: Vec<&str> = ai.keys().take(4).map(String::as_str).collect();
        out.push(format!(
            "AI crawlers made {total} requests ({})",
            names.join(", ")
        ));
    }
    if let Some(seo) = fams.get("seo-tool").and_then(Value::as_object) {
        let total: u64 = seo.values().filter_map(Value::as_u64).sum();
        out.push(format!(
            "Third-party SEO crawlers made {total} requests, consuming your bandwidth and server resources"
        ));
    }
    if let Some(statuses) = report["status_by_family"].as_object() {
        for (family, by_status) in statuses {
            if family == "human" || family == "unknown" {
                continue;
            }
            let by_status = by_status.as_object().unwrap_or(&empty);
            let errors: u64 = by_status
                .iter()
                .filter(|(s, _)| s.parse::<i64>().is_ok_and(|s| s >= 400))
                .filter_map(|(_, c)| c.as_u64())
                .sum();
            let total = by_status
                .values()
                .filter_map(Value::as_u64)
                .sum::<u64>()
                .max(1);
            if errors as f64 / total as f64 > 0.1 {
                out.push(format!(
                    "{family}: {errors} of {total} requests returned errors ({:.0}%), wasting crawl budget",
                    errors as f64 / total as f64 * 100.0
                ));
            }
        }
    }
    let verification = &report["verification"];
    if verification["checked"].as_bool().unwrap_or(false) {
        let fake: Vec<&str> = verification["checks"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter(|c| c["verified"] == json!(false))
                    .filter_map(|c| c["bot"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        if !fake.is_empty() {
            out.push(format!(
                "Reverse-DNS verification failed for {}; these requests may be impersonating known bots",
                fake.join(", ")
            ));
        }
    } else if verification["dns_available"] == json!(false) {
        out.push("Bot authenticity was not checked because reverse DNS is unavailable".into());
    }
    out
}
