//! Google-side storage of a semantic core, next to the Yandex `phrases`
//! table: provider evidence per language and location, SERP snapshots, cost
//! ledger, and the topic tables that link both engines.
//!
//! Yandex tables are never altered; everything here is additive and created
//! with `IF NOT EXISTS` by the first Google import, so databases that never
//! see Google data keep their exact shape.

use rusqlite::Connection;
use rust_xlsxwriter::{Format, Workbook};
use serde_json::{json, Value};

use crate::store::Store;
use crate::Error;

mod cluster;
mod csv_core;
mod dfs;
mod topics;

pub use cluster::{cluster_google, clusters_google, url_key};
pub use csv_core::import_google_csv;
pub use dfs::import_dataforseo;
pub use topics::import_topics;

/// Additive schema for Google data and the cross-engine topic links.
pub const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS g_phrases(
  id INTEGER PRIMARY KEY,
  norm TEXT NOT NULL,               -- lowercased, whitespace-collapsed keyword (any script)
  keyword TEXT,                     -- keyword as the provider returned it
  engine TEXT NOT NULL DEFAULT 'google',
  language TEXT NOT NULL DEFAULT '',
  location_code INTEGER NOT NULL DEFAULT 0,
  country TEXT,
  device TEXT NOT NULL DEFAULT 'desktop',
  volume INTEGER, volume_monthly_json TEXT,
  kd INTEGER, cpc REAL, low_bid REAL, high_bid REAL,
  competition REAL, competition_level TEXT,
  intent TEXT,
  source TEXT,                      -- first source: labs_ranked|labs_suggestions|labs_ideas|labs_related|ads_volume|ads_ideas
  source_detail TEXT,               -- seed, domain or URL the source was run for
  extra_json TEXT,                  -- categories, trend, backlinks, SERP item types, ...
  first_seen TEXT, last_update TEXT,
  UNIQUE(norm, language, location_code, device)
);
CREATE INDEX IF NOT EXISTS ix_g_phrases_lang ON g_phrases(language, location_code);
CREATE TABLE IF NOT EXISTS g_sources(
  norm TEXT NOT NULL, language TEXT NOT NULL, location_code INTEGER NOT NULL,
  source TEXT NOT NULL, source_detail TEXT NOT NULL DEFAULT '',
  PRIMARY KEY(norm, language, location_code, source, source_detail)
);
CREATE TABLE IF NOT EXISTS g_rankings(
  norm TEXT NOT NULL, language TEXT NOT NULL, location_code INTEGER NOT NULL,
  domain TEXT NOT NULL, url TEXT NOT NULL, target TEXT,
  position INTEGER, rank_group INTEGER, serp_type TEXT,
  etv REAL, paid_cost REAL, fetched_at TEXT, source TEXT,
  PRIMARY KEY(norm, language, location_code, domain, url)
);
CREATE INDEX IF NOT EXISTS ix_g_rankings_domain ON g_rankings(domain);
CREATE TABLE IF NOT EXISTS g_competitors(
  target TEXT NOT NULL, language TEXT NOT NULL, location_code INTEGER NOT NULL,
  domain TEXT NOT NULL, avg_position REAL, intersections INTEGER,
  etv REAL, kw_count INTEGER, domain_etv REAL, domain_kw_count INTEGER,
  PRIMARY KEY(target, language, location_code, domain)
);
CREATE TABLE IF NOT EXISTS g_serp(
  norm TEXT NOT NULL, language TEXT NOT NULL, location_code INTEGER NOT NULL,
  device TEXT NOT NULL, fetched_at TEXT NOT NULL,
  pos INTEGER, rank_group INTEGER, type TEXT, domain TEXT, url TEXT, title TEXT
);
CREATE INDEX IF NOT EXISTS ix_g_serp_key ON g_serp(norm, language, location_code, device, fetched_at);
CREATE INDEX IF NOT EXISTS ix_g_serp_domain ON g_serp(domain);
CREATE TABLE IF NOT EXISTS g_serp_features(
  norm TEXT NOT NULL, language TEXT NOT NULL, location_code INTEGER NOT NULL,
  device TEXT NOT NULL, fetched_at TEXT NOT NULL,
  ai_overview INTEGER NOT NULL DEFAULT 0, ai_overview_markdown TEXT,
  cited_json TEXT, paa_json TEXT, related_searches_json TEXT,
  item_types_json TEXT, se_results_count INTEGER, check_url TEXT,
  PRIMARY KEY(norm, language, location_code, device, fetched_at)
);
-- SERP clusters of Google phrases, one set per language and location.
CREATE TABLE IF NOT EXISTS g_clusters(
  id INTEGER PRIMARY KEY,
  language TEXT NOT NULL, location_code INTEGER NOT NULL,
  name TEXT NOT NULL, marker_norm TEXT NOT NULL,
  size INTEGER NOT NULL, volume_sum INTEGER, min_shared INTEGER NOT NULL,
  ai_share REAL, top_domains_json TEXT, intent TEXT, page_type TEXT, verdict TEXT,
  our_pos INTEGER, our_url TEXT, topics TEXT
);
CREATE INDEX IF NOT EXISTS ix_g_clusters_scope ON g_clusters(language, location_code);
CREATE TABLE IF NOT EXISTS g_cluster_members(
  cluster_id INTEGER NOT NULL, norm TEXT NOT NULL,
  PRIMARY KEY(cluster_id, norm)
);
CREATE INDEX IF NOT EXISTS ix_g_cluster_members_norm ON g_cluster_members(norm);
CREATE TABLE IF NOT EXISTS provider_runs(
  id INTEGER PRIMARY KEY,
  provider TEXT NOT NULL, endpoint TEXT, name TEXT,
  task_id TEXT UNIQUE,              -- provider task id; makes re-imports idempotent
  cost_usd REAL, rows INTEGER, ts TEXT, raw_path TEXT,
  language TEXT, location_code INTEGER, target TEXT
);
CREATE TABLE IF NOT EXISTS topics(
  id TEXT PRIMARY KEY, name TEXT NOT NULL, page_type TEXT
);
CREATE TABLE IF NOT EXISTS topic_members(
  topic_id TEXT NOT NULL, engine TEXT NOT NULL, norm TEXT NOT NULL,
  language TEXT NOT NULL DEFAULT '', location_code INTEGER NOT NULL DEFAULT 0,
  demand INTEGER,
  PRIMARY KEY(topic_id, engine, norm, language, location_code)
);
CREATE TABLE IF NOT EXISTS topic_landings(
  topic_id TEXT NOT NULL, locale TEXT NOT NULL, url TEXT NOT NULL DEFAULT '',
  PRIMARY KEY(topic_id, locale)
);
-- Where each normalized phrase comes from: google, yandex or both.
CREATE VIEW IF NOT EXISTS v_origin AS
  SELECT norm,
         CASE WHEN SUM(y)>0 AND SUM(g)>0 THEN 'both' WHEN SUM(y)>0 THEN 'yandex' ELSE 'google' END AS origin
  FROM (SELECT norm, 1 AS y, 0 AS g FROM phrases
        UNION ALL SELECT norm, 0, 1 FROM g_phrases
        UNION ALL SELECT replace(norm,'-',' '), 0, 1 FROM g_phrases WHERE norm LIKE '%-%')
  GROUP BY norm;
-- Both engines' phrases per topic; origin flags phrases present in both.
CREATE VIEW IF NOT EXISTS v_topic_phrases AS
  SELECT m.topic_id, t.name AS topic, m.engine, m.norm, m.language, m.location_code,
         COALESCE(g.volume, p.base) AS volume,
         COALESCE((SELECT origin FROM v_origin o WHERE o.norm=m.norm), m.engine) AS origin
  FROM topic_members m
  JOIN topics t ON t.id=m.topic_id
  LEFT JOIN g_phrases g ON m.engine='google' AND g.norm=m.norm AND g.language=m.language
       AND (m.location_code=0 OR g.location_code=m.location_code)
  LEFT JOIN phrases p ON m.engine='yandex' AND p.norm=m.norm;
-- Topics with an English landing but no landing in some locale (hreflang gaps).
CREATE VIEW IF NOT EXISTS v_topic_gaps AS
  SELECT t.id AS topic_id, t.name AS topic, t.page_type, l.locale
  FROM topics t JOIN topic_landings l ON l.topic_id=t.id AND l.url=''
  WHERE EXISTS(SELECT 1 FROM topic_landings e WHERE e.topic_id=t.id AND e.locale='en' AND e.url<>'');
";

/// Create the Google/topic tables on an open connection (idempotent).
pub fn ensure_schema(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(SCHEMA).map_err(Error::Db)
}

/// Provider-neutral key for a keyword: lowercase with whitespace collapsed.
/// Unlike the Yandex normalizer it keeps every script (Arabic, CJK, ...).
pub fn gnorm(keyword: &str) -> String {
    keyword
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// ISO country for the location codes DataForSEO uses most; `None` otherwise.
pub fn country(location_code: i64) -> Option<&'static str> {
    Some(match location_code {
        2840 => "US",
        2826 => "GB",
        2356 => "IN",
        2276 => "DE",
        2040 => "AT",
        2756 => "CH",
        2250 => "FR",
        2724 => "ES",
        2484 => "MX",
        2076 => "BR",
        2392 => "JP",
        2410 => "KR",
        2792 => "TR",
        2784 => "AE",
        2682 => "SA",
        2818 => "EG",
        _ => return None,
    })
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0)
}

/// Google-side counts for `status`: empty object when nothing was imported.
pub fn status(store: &Store) -> Value {
    let conn = &store.conn;
    if count(conn, "SELECT COUNT(*) FROM g_phrases") == 0
        && count(conn, "SELECT COUNT(*) FROM provider_runs") == 0
    {
        return json!({});
    }
    let rows = |sql: &str| -> Value {
        let mut out = Vec::new();
        if let Ok(mut stmt) = conn.prepare(sql) {
            let _ = stmt
                .query_map([], |r| {
                    Ok(json!({
                        "language": r.get::<_, String>(0)?,
                        "location_code": r.get::<_, i64>(1)?,
                        "n": r.get::<_, i64>(2)?,
                    }))
                })
                .map(|it| out.extend(it.flatten()));
        }
        Value::Array(out)
    };
    json!({
        "phrases": count(conn, "SELECT COUNT(*) FROM g_phrases"),
        "by_locale": rows("SELECT language, location_code, COUNT(*) FROM g_phrases GROUP BY 1,2 ORDER BY 3 DESC"),
        "rankings": count(conn, "SELECT COUNT(*) FROM g_rankings"),
        "serp_snapshots": count(conn, "SELECT COUNT(*) FROM g_serp_features"),
        "competitors": count(conn, "SELECT COUNT(*) FROM g_competitors"),
        "clusters": if count(conn, "SELECT COUNT(*) FROM sqlite_master WHERE name='g_clusters'") > 0 {
            cluster::counts(conn)
        } else {
            json!([])
        },
        "topics": count(conn, "SELECT COUNT(*) FROM topics"),
        "topic_gaps": count(conn, "SELECT COUNT(*) FROM v_topic_gaps"),
        "origin": {
            "both": count(conn, "SELECT COUNT(*) FROM v_origin WHERE origin='both'"),
        },
        "cost_usd": conn
            .query_row("SELECT ROUND(COALESCE(SUM(cost_usd),0),5) FROM provider_runs", [], |r| r.get::<_, f64>(0))
            .unwrap_or(0.0),
    })
}

fn xerr(e: rust_xlsxwriter::XlsxError) -> Error {
    Error::Io(e.to_string())
}

fn sheet(
    wb: &mut Workbook,
    conn: &Connection,
    name: &str,
    sql: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<(), Error> {
    let head = Format::new().set_bold();
    let ws = wb.add_worksheet().set_name(name).map_err(xerr)?;
    ws.set_freeze_panes(1, 0).map_err(xerr)?;
    let mut stmt = conn.prepare(sql).map_err(Error::Db)?;
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    for (c, n) in names.iter().enumerate() {
        ws.write_string_with_format(0, c as u16, n, &head)
            .map_err(xerr)?;
    }
    let mut rows = stmt.query(args).map_err(Error::Db)?;
    let mut r = 1u32;
    while let Some(row) = rows.next().map_err(Error::Db)? {
        for c in 0..names.len() {
            let v = row.get_ref(c).map_err(Error::Db)?;
            use rusqlite::types::ValueRef::*;
            match v {
                Integer(i) => ws.write_number(r, c as u16, i as f64).map(|_| ()),
                Real(f) => ws.write_number(r, c as u16, f).map(|_| ()),
                Text(t) => ws
                    .write_string(r, c as u16, String::from_utf8_lossy(t).as_ref())
                    .map(|_| ()),
                _ => Ok(()),
            }
            .map_err(xerr)?;
        }
        r += 1;
        if r > 1_000_000 {
            break;
        }
    }
    ws.set_column_width(0, 42.0).map_err(xerr)?;
    Ok(())
}

/// Add the Google sheets to the final workbook: one `g-<lang>` sheet per
/// language (phrases with origin) and a topics matrix. No-op without data.
pub fn excel_sheets(store: &Store, wb: &mut Workbook) -> Result<(), Error> {
    let conn = &store.conn;
    // Tables exist only after the first Google import.
    if count(
        conn,
        "SELECT COUNT(*) FROM sqlite_master WHERE name='g_phrases'",
    ) == 0
    {
        return Ok(());
    }
    let langs: Vec<String> = conn
        .prepare("SELECT DISTINCT language FROM g_phrases WHERE language<>'' ORDER BY language")
        .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect())
        .unwrap_or_default();
    for lang in &langs {
        sheet(
            wb,
            conn,
            &format!("g-{lang}"),
            "SELECT g.keyword, g.volume, g.kd, g.cpc, g.intent, g.country, g.device, \
                    COALESCE(o.origin,'google') AS origin, g.source, g.source_detail \
             FROM g_phrases g LEFT JOIN v_origin o ON o.norm=g.norm \
             WHERE g.language=?1 ORDER BY g.volume DESC NULLS LAST",
            &[lang],
        )?;
    }
    for lang in &langs {
        if count(
            conn,
            &format!(
                "SELECT COUNT(*) FROM g_clusters WHERE language='{}'",
                lang.replace('\'', "''")
            ),
        ) == 0
        {
            continue;
        }
        sheet(
            wb,
            conn,
            &format!("gc-{lang}"),
            "SELECT c.name AS cluster, c.size, c.volume_sum, c.location_code, c.ai_share, \
                    (SELECT group_concat(json_extract(value,'$.domain')||' '||json_extract(value,'$.n'), ', ') \
                       FROM json_each(c.top_domains_json)) AS top_domains, \
                    c.intent, c.page_type, c.verdict, c.our_pos, c.our_url, c.topics, \
                    (SELECT group_concat(norm, ' | ') FROM g_cluster_members m WHERE m.cluster_id=c.id) AS members \
             FROM g_clusters c WHERE c.language=?1 ORDER BY c.volume_sum DESC, c.size DESC, c.id",
            &[lang],
        )?;
    }
    let has_topics = count(conn, "SELECT COUNT(*) FROM topics") > 0;
    if has_topics {
        sheet(
            wb,
            conn,
            "topics",
            "SELECT t.id, t.name AS topic, t.page_type, \
                    (SELECT group_concat(l.locale||': '||CASE WHEN l.url='' THEN '-' ELSE l.url END, ' | ') \
                       FROM topic_landings l WHERE l.topic_id=t.id) AS landings, \
                    (SELECT COUNT(*) FROM topic_members m WHERE m.topic_id=t.id) AS members \
             FROM topics t ORDER BY t.id",
            &[],
        )?;
    }
    Ok(())
}
