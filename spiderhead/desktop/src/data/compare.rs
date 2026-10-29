//! Scan comparison for the "Сравнение" screen: sibling scan files of the open
//! one, a two-scan page-field diff, and one URL observed in both.
//!
//! `compare.v2` (core findings diff + offline verification) is not ported yet;
//! this module reads the retained `scan.v1` files directly — page fields,
//! link counts and outlink sets — so every number is real evidence, and the
//! screen degrades to the canvas empty/unavailable states when data is thin.
//!
//! Rows stay bounded: summaries are aggregate SQL, row lists are paged with
//! `LIMIT/OFFSET`, link diffs cap at [`LINK_CAP`].

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};

use super::filters::QueryError;
use super::session::file_uri;

/// Added/removed outlinks of `compare.url` at most.
pub const LINK_CAP: i64 = 200;
/// Sibling scan files the picker offers at most.
pub const SCAN_CAP: usize = 200;
/// Row pages of the diff at most.
pub const PAGE_CAP: i64 = 500;

/// Comparison tabs of the canvas (`compare.tab`).
pub const TABS: &[&str] = &["fixed", "better", "worse", "gone", "new"];
/// Metric ids for the better/worse tabs (`compare.metric`).
pub const METRICS: &[&str] = &["words", "inlinks", "response"];

fn qe(e: rusqlite::Error) -> String {
    format!("cannot read scan: {e}")
}

/// Open a scan file read-only; `None` on anything that is not a readable scan.
fn open_ro(path: &Path) -> Result<Connection, String> {
    let uri = format!("{}?mode=ro", file_uri(path));
    Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_URI | rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
    )
    .map_err(qe)
}

/// Scan-table header fields (`scan_uuid`, `finished_at`, `start_url`, fingerprint).
type ScanHeader = (String, Option<String>, Option<String>, Option<String>);

/// Header fields of one scan file (`None` → not a readable scan).
fn scan_header(con: &Connection) -> Option<ScanHeader> {
    con.query_row(
        "SELECT scan_uuid, finished_at, start_url, config_fingerprint
         FROM scan WHERE singleton=1",
        [],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        },
    )
    .optional()
    .ok()
    .flatten()
}

/// Short human name of a scan file: its folder for `scan.sqlite`, else stem.
fn display_name(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let folder = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if stem == "scan" && !folder.is_empty() {
        folder.to_string()
    } else {
        stem.to_string()
    }
}

/// Sibling `scan.v1` files beside `open_scan`, newest first — the same pool
/// the details history tab reads. `number` counts down like `url_history`.
pub fn sibling_scans(open_scan: &Path) -> Result<Value, QueryError> {
    let dir = open_scan
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    name.ends_with(".sqlite") && !name.contains(".audit-v2")
                })
                .collect()
        })
        .unwrap_or_default();
    paths.sort();
    let mut scans = Vec::new();
    for path in paths.iter().take(SCAN_CAP) {
        let Ok(con) = open_ro(path) else {
            continue;
        };
        let Some((uuid, finished_at, start_url, _)) = scan_header(&con) else {
            continue;
        };
        let pages: i64 = con
            .query_row("SELECT COUNT(*) FROM pages", [], |r| r.get(0))
            .unwrap_or(0);
        scans.push(json!({
            "path": path.display().to_string(),
            "uuid": uuid,
            "label": display_name(path),
            "finished_at": finished_at,
            "start_url": start_url,
            "pages": pages,
        }));
    }
    scans.sort_by(|a, b| {
        let key = |s: &Value| {
            s.get("finished_at")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        key(b).cmp(&key(a))
    });
    let total = scans.len() as i64;
    for (i, s) in scans.iter_mut().enumerate() {
        s["number"] = json!(total - i as i64);
    }
    Ok(json!({
        "state": "available",
        "scans": scans,
        "total": total,
        "open": open_scan.display().to_string(),
    }))
}

/// Status rank of a page's HTTP code: 2xx healthy, 3xx redirect, else broken.
const STATUS_RANK: &str = "CASE WHEN {}.status_code BETWEEN 200 AND 299 THEN 3 \
     WHEN {}.status_code BETWEEN 300 AND 399 THEN 2 \
     WHEN {}.status_code IS NULL THEN 0 ELSE 1 END";

fn rank(alias: &str) -> String {
    STATUS_RANK
        .replacen("{}", alias, 1)
        .replacen("{}", alias, 1)
        .replacen("{}", alias, 1)
}

/// The `indexable` expression of `data::columns` for one `pages` alias.
fn indexable(alias: &str) -> String {
    format!(
        "COALESCE({a}.status_code BETWEEN 200 AND 299 AND \
         instr(lower({a}.meta_robots||','||{a}.x_robots),'noindex')=0, 0)",
        a = alias
    )
}

/// A `(before, after)` pair of scans joined in one connection (`o` = after).
pub struct Pair {
    con: Connection,
    /// Path of the `after` scan (the `o` attachment).
    pub after_path: PathBuf,
    /// Path of the `before` scan (the primary connection).
    pub before_path: PathBuf,
}

impl Pair {
    /// Attach `after` to a read-only `before` connection.
    pub fn open(before: &Path, after: &Path) -> Result<Self, String> {
        let con = open_ro(before)?;
        let uri = format!("{}?mode=ro", file_uri(after));
        con.execute("ATTACH DATABASE ?1 AS o", params![uri])
            .map_err(qe)?;
        Ok(Self {
            con,
            after_path: after.to_path_buf(),
            before_path: before.to_path_buf(),
        })
    }

    fn count(&self, sql: &str) -> i64 {
        self.con.query_row(sql, [], |r| r.get(0)).unwrap_or(0)
    }

    /// Median of a `pages` column in one schema (`""` = before, `"o."` = after).
    fn median(&self, schema: &str, column: &str) -> Option<f64> {
        self.con
            .query_row(
                &format!(
                    "SELECT AVG(x) FROM (SELECT p.{c} AS x FROM {s}pages p \
                     WHERE p.{c} IS NOT NULL ORDER BY x \
                     LIMIT 2 - (SELECT COUNT(*)%2 FROM {s}pages p2 WHERE p2.{c} IS NOT NULL) \
                     OFFSET (SELECT (COUNT(*)-1)/2 FROM {s}pages p3 WHERE p3.{c} IS NOT NULL))",
                    s = schema,
                    c = column
                ),
                [],
                |r| r.get::<_, Option<f64>>(0),
            )
            .ok()
            .flatten()
    }

    /// Median inbound-link count per page of one schema.
    fn median_inlinks(&self, schema: &str) -> Option<f64> {
        self.con
            .query_row(
                &format!(
                    "SELECT AVG(x) FROM (SELECT n AS x FROM \
                     (SELECT (SELECT COUNT(*) FROM {s}links l \
                       WHERE l.destination_url_id=p.url_id) AS n \
                      FROM {s}pages p ORDER BY n \
                     LIMIT 2 - (SELECT COUNT(*)%2 FROM {s}pages) \
                     OFFSET (SELECT (COUNT(*)-1)/2 FROM {s}pages)))",
                    s = schema
                ),
                [],
                |r| r.get::<_, Option<f64>>(0),
            )
            .ok()
            .flatten()
    }

    /// Aggregate summary: populations, partitions, KPI medians and deltas.
    pub fn summary(&self) -> Value {
        let pages_b = self.count("SELECT COUNT(*) FROM pages");
        let pages_a = self.count("SELECT COUNT(*) FROM o.pages");
        let common = self.count(
            "SELECT COUNT(*) FROM pages p JOIN urls u ON u.url_id=p.url_id \
             WHERE EXISTS(SELECT 1 FROM o.pages ap JOIN o.urls au \
               ON au.url_id=ap.url_id WHERE au.url=u.url)",
        );
        let status_changed = self.count(
            "SELECT COUNT(*) FROM pages p JOIN urls u ON u.url_id=p.url_id \
             JOIN o.urls au ON au.url=u.url JOIN o.pages ap ON ap.url_id=au.url_id \
             WHERE COALESCE(p.status_code,-1)<>COALESCE(ap.status_code,-1)",
        );
        let canonical_changed = self.count(
            "SELECT COUNT(*) FROM pages p JOIN urls u ON u.url_id=p.url_id \
             JOIN o.urls au ON au.url=u.url JOIN o.pages ap ON ap.url_id=au.url_id \
             WHERE COALESCE(p.canonical,'')<>COALESCE(ap.canonical,'')",
        );
        let long_title = |s: &str| {
            self.count(&format!(
                "SELECT COUNT(*) FROM {s}pages WHERE length(title)>60"
            ))
        };
        let kpis = |schema: &str| {
            json!({
                "words": self.median(schema, "word_count"),
                "inlinks": self.median_inlinks(schema),
                "response": self.median(schema, "response_time"),
                "title_over_60": long_title(schema),
            })
        };
        let (uuid_b, fin_b, start_b, fp_b) = scan_header(&self.con).unwrap_or_default();
        let mut con_a = open_ro(&self.after_path).ok();
        let (uuid_a, fin_a, start_a, fp_a) = con_a
            .as_mut()
            .and_then(|c| scan_header(c))
            .unwrap_or_default();
        let config_warning = matches!((&fp_b, &fp_a), (Some(b), Some(a)) if b != a);
        json!({
            "before": {"path": self.before_path.display().to_string(),
                       "label": display_name(&self.before_path), "uuid": uuid_b,
                       "finished_at": fin_b, "start_url": start_b, "pages": pages_b},
            "after": {"path": self.after_path.display().to_string(),
                      "label": display_name(&self.after_path), "uuid": uuid_a,
                      "finished_at": fin_a, "start_url": start_a, "pages": pages_a},
            "common": common,
            "only_before": pages_b - common,
            "only_after": pages_a - common,
            "status_changed": status_changed,
            "canonical_changed": canonical_changed,
            "config_warning": config_warning,
            "kpi_before": kpis(""),
            "kpi_after": kpis("o."),
        })
    }

    /// Paged rows of one tab (`fixed`/`better`/`worse`/`gone`/`new`).
    pub fn page(&self, tab: &str, metric: &str, offset: i64, limit: i64) -> Result<Value, String> {
        let limit = limit.clamp(1, PAGE_CAP);
        let offset = offset.max(0);
        let (sql, count_sql): (String, String) = match tab {
            "gone" => (
                format!(
                    "SELECT u.url, p.status_code, NULL, NULL FROM pages p \
                     JOIN urls u ON u.url_id=p.url_id \
                     WHERE NOT EXISTS(SELECT 1 FROM o.pages ap JOIN o.urls au \
                       ON au.url_id=ap.url_id WHERE au.url=u.url) \
                     ORDER BY p.page_ordinal LIMIT {limit} OFFSET {offset}"
                ),
                "SELECT COUNT(*) FROM pages p JOIN urls u ON u.url_id=p.url_id \
                 WHERE NOT EXISTS(SELECT 1 FROM o.pages ap JOIN o.urls au \
                   ON au.url_id=ap.url_id WHERE au.url=u.url)"
                    .into(),
            ),
            "new" => (
                format!(
                    "SELECT au.url, NULL, NULL, ap.status_code, NULL FROM o.pages ap \
                     JOIN o.urls au ON au.url_id=ap.url_id \
                     WHERE NOT EXISTS(SELECT 1 FROM pages p JOIN urls u \
                       ON u.url_id=p.url_id WHERE u.url=au.url) \
                     ORDER BY ap.page_ordinal LIMIT {limit} OFFSET {offset}"
                ),
                "SELECT COUNT(*) FROM o.pages ap JOIN o.urls au ON au.url_id=ap.url_id \
                 WHERE NOT EXISTS(SELECT 1 FROM pages p JOIN urls u \
                   ON u.url_id=p.url_id WHERE u.url=au.url)"
                    .into(),
            ),
            "fixed" => {
                let rb = rank("p");
                let ra = rank("ap");
                (
                    format!(
                        "SELECT u.url, p.status_code, NULL, ap.status_code, NULL \
                         FROM pages p JOIN urls u ON u.url_id=p.url_id \
                         JOIN o.urls au ON au.url=u.url JOIN o.pages ap ON ap.url_id=au.url_id \
                         WHERE {ra} > {rb} \
                         ORDER BY p.page_ordinal LIMIT {limit} OFFSET {offset}"
                    ),
                    format!(
                        "SELECT COUNT(*) FROM pages p JOIN urls u ON u.url_id=p.url_id \
                         JOIN o.urls au ON au.url=u.url JOIN o.pages ap ON ap.url_id=au.url_id \
                         WHERE {ra} > {rb}"
                    ),
                )
            }
            "better" | "worse" => {
                let (col, extra): (String, String) = match metric {
                    "inlinks" => (
                        "(SELECT COUNT(*) FROM links l WHERE l.destination_url_id=p.url_id)".into(),
                        "(SELECT COUNT(*) FROM o.links l WHERE l.destination_url_id=ap.url_id)"
                            .into(),
                    ),
                    "response" => ("p.response_time".into(), "ap.response_time".into()),
                    _ => ("p.word_count".into(), "ap.word_count".into()),
                };
                // "better" direction: words/inlinks grow, response time shrinks.
                let sign = match (tab, metric) {
                    ("better", "response") | ("worse", "words") | ("worse", "inlinks") => "<",
                    _ => ">",
                };
                (
                    format!(
                        "SELECT u.url, {col}, NULL, {extra}, NULL \
                         FROM pages p JOIN urls u ON u.url_id=p.url_id \
                         JOIN o.urls au ON au.url=u.url JOIN o.pages ap ON ap.url_id=au.url_id \
                         WHERE {col} IS NOT NULL AND {extra} IS NOT NULL \
                           AND {extra} {sign} {col} \
                         ORDER BY ABS({extra}-{col}) DESC LIMIT {limit} OFFSET {offset}"
                    ),
                    format!(
                        "SELECT COUNT(*) FROM pages p JOIN urls u ON u.url_id=p.url_id \
                         JOIN o.urls au ON au.url=u.url JOIN o.pages ap ON ap.url_id=au.url_id \
                         WHERE {col} IS NOT NULL AND {extra} IS NOT NULL \
                           AND {extra} {sign} {col}"
                    ),
                )
            }
            other => return Err(format!("unknown compare tab {other:?}")),
        };
        let total = self.count(&count_sql);
        let mut stmt = self.con.prepare(&sql).map_err(qe)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, rusqlite::types::Value>(1)?,
                    r.get::<_, rusqlite::types::Value>(3)?,
                ))
            })
            .map_err(qe)?;
        let mut out = Vec::new();
        for row in rows {
            let (url, before_raw, after_raw) = row.map_err(qe)?;
            let before = super::query::sql_to_json(before_raw);
            let after = super::query::sql_to_json(after_raw);
            let (before, after) = match tab {
                "gone" => (before, Value::Null),
                "new" => (Value::Null, after),
                _ => (before, after),
            };
            let delta = match (tab, before.as_f64(), after.as_f64()) {
                ("better" | "worse", Some(b), Some(a)) => json!(a - b),
                _ => Value::Null,
            };
            out.push(json!({
                "url": url,
                "before": before,
                "after": after,
                "delta": delta,
            }));
        }
        Ok(json!({
            "tab": tab,
            "metric": metric,
            "rows": out,
            "total": total,
            "offset": offset,
            "has_more": offset + limit < total,
            "next_offset": if offset + limit < total { offset + limit } else { total },
        }))
    }
}

/// A `pages` row of one URL in one scan (all fields `null` when absent).
fn observe(con: &Connection, url: &str) -> Result<Option<Map<String, Value>>, String> {
    let row = con
        .query_row(
            &format!(
                "SELECT p.status_code, p.title, p.meta_description, p.h1, \
                        p.word_count, p.response_time, p.size_bytes, p.canonical, \
                        {indexable}, \
                        (SELECT COUNT(*) FROM links l WHERE l.destination_url_id=u.url_id), \
                        (SELECT COUNT(*) FROM links l WHERE l.source_url_id=u.url_id) \
                 FROM urls u JOIN pages p ON p.url_id=u.url_id WHERE u.url=?1",
                indexable = indexable("p")
            ),
            params![url],
            |r| {
                let mut m = Map::new();
                for (i, name) in [
                    "status_code",
                    "title",
                    "meta_description",
                    "h1",
                    "word_count",
                    "response_time",
                    "size_bytes",
                    "canonical",
                    "indexable",
                    "inlinks",
                    "outlinks",
                ]
                .iter()
                .enumerate()
                {
                    m.insert(
                        (*name).to_string(),
                        super::query::sql_to_json(r.get::<_, rusqlite::types::Value>(i)?),
                    );
                }
                Ok(m)
            },
        )
        .optional()
        .map_err(qe)?;
    Ok(row)
}

/// One URL in `before` vs `after`: per-field rows and the outlink set diff.
pub fn url_compare(before: &Path, after: &Path, url: &str) -> Result<Value, String> {
    let con_b = open_ro(before)?;
    let con_a = open_ro(after)?;
    let b = observe(&con_b, url)?;
    let a = observe(&con_a, url)?;
    if b.is_none() && a.is_none() {
        return Ok(json!({
            "state": "absent",
            "url": url,
            "fields": [],
            "links_added": [],
            "links_removed": [],
        }));
    }
    let fields = [
        "status_code",
        "title",
        "meta_description",
        "h1",
        "word_count",
        "inlinks",
        "outlinks",
        "canonical",
        "indexable",
        "response_time",
        "size_bytes",
    ];
    let mut rows = Vec::new();
    for f in fields {
        let vb = b
            .as_ref()
            .and_then(|m| m.get(f))
            .cloned()
            .unwrap_or(Value::Null);
        let va = a
            .as_ref()
            .and_then(|m| m.get(f))
            .cloned()
            .unwrap_or(Value::Null);
        let delta = match (vb.as_f64(), va.as_f64()) {
            (Some(x), Some(y)) => json!(y - x),
            _ => Value::Null,
        };
        let state = match (vb.is_null(), va.is_null()) {
            (true, false) => "added",
            (false, true) => "removed",
            _ if vb == va => "same",
            _ => "changed",
        };
        rows.push(json!({
            "field": f,
            "before": vb,
            "after": va,
            "delta": delta,
            "state": state,
        }));
    }
    let links = |con: &Connection| -> Result<std::collections::BTreeSet<String>, String> {
        let mut stmt = con
            .prepare(
                "SELECT du.url FROM links l JOIN urls du ON du.url_id=l.destination_url_id \
                 JOIN urls su ON su.url_id=l.source_url_id WHERE su.url=?1 LIMIT ?2",
            )
            .map_err(qe)?;
        let rows = stmt
            .query_map(params![url, LINK_CAP * 4], |r| r.get::<_, String>(0))
            .map_err(qe)?;
        let mut set = std::collections::BTreeSet::new();
        for u in rows {
            set.insert(u.map_err(qe)?);
        }
        Ok(set)
    };
    let (mut added, mut removed) = (Vec::new(), Vec::new());
    if let (Ok(lb), Ok(la)) = (links(&con_b), links(&con_a)) {
        added = la
            .difference(&lb)
            .take(LINK_CAP as usize)
            .cloned()
            .collect();
        removed = lb
            .difference(&la)
            .take(LINK_CAP as usize)
            .cloned()
            .collect();
    }
    Ok(json!({
        "state": "available",
        "url": url,
        "fields": rows,
        "links_added": added,
        "links_removed": removed,
        "present_before": b.is_some(),
        "present_after": a.is_some(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(site: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../oracle/raw-scans")
            .join(site)
            .join("scan.sqlite")
    }

    /// Two sibling scans in one temp dir: basic copied under two names.
    fn pair_dir() -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("desk-d3-cmp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let b = dir.join("scan-a.sqlite");
        let a = dir.join("scan-b.sqlite");
        std::fs::copy(raw("basic"), &b).unwrap();
        std::fs::copy(raw("basic"), &a).unwrap();
        (b, a)
    }

    #[test]
    fn siblings_list_scans_newest_first() {
        let (b, _a) = pair_dir();
        let v = sibling_scans(&b).unwrap();
        assert_eq!(v["total"], 2);
        assert!(v["scans"][0]["pages"].as_i64().unwrap() >= 10);
        let _ = std::fs::remove_dir_all(b.parent().unwrap());
    }

    #[test]
    fn identical_scans_diff_to_zero() {
        let (b, a) = pair_dir();
        let pair = Pair::open(&b, &a).unwrap();
        let s = pair.summary();
        assert_eq!(s["common"], 10, "{s}");
        assert_eq!(s["only_before"], 0);
        assert_eq!(s["only_after"], 0);
        assert_eq!(s["status_changed"], 0);
        // The attached `o` schema's medians resolve like the primary's —
        // a dotted alias (`o.p`) would silently turn them into NULLs.
        for side in ["kpi_before", "kpi_after"] {
            assert!(s[side]["words"].is_number(), "{side}: {s}");
            assert!(s[side]["inlinks"].is_number(), "{side}: {s}");
        }
        assert_eq!(s["kpi_before"]["words"], s["kpi_after"]["words"]);
        let gone = pair.page("gone", "words", 0, 50).unwrap();
        assert_eq!(gone["total"], 0);
        let _ = std::fs::remove_dir_all(b.parent().unwrap());
    }

    #[test]
    fn different_scans_show_gone_and_new() {
        // basic vs redirects: different URL populations.
        let b = raw("basic");
        let a = raw("redirects");
        let pair = Pair::open(&b, &a).unwrap();
        let s = pair.summary();
        assert!(s["only_before"].as_i64().unwrap() > 0, "{s}");
        assert!(s["only_after"].as_i64().unwrap() > 0, "{s}");
        let new = pair.page("new", "words", 0, 10).unwrap();
        assert!(new["total"].as_i64().unwrap() > 0);
        let row = &new["rows"][0];
        assert!(row["url"].as_str().unwrap().starts_with("http"));
        assert_eq!(row["before"], Value::Null);
    }

    #[test]
    fn url_compare_reports_fields_and_state() {
        let b = raw("basic");
        let a = raw("redirects");
        let v = url_compare(&b, &a, "http://127.0.0.1:18701/").unwrap();
        assert_eq!(v["state"], "available");
        assert_eq!(v["present_before"], true);
        let fields = v["fields"].as_array().unwrap();
        assert_eq!(fields.len(), 11);
        assert!(fields.iter().any(|f| f["field"] == "status_code"));
        let miss = url_compare(&b, &a, "http://absent.example/").unwrap();
        assert_eq!(miss["state"], "absent");
    }
}
