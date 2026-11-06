//! `cluster-google`: free, offline SERP clustering of Google phrases. Two
//! phrases of the same language and location are linked when their organic
//! top results share at least `min_shared` normalized URLs; links are
//! transitive (union-find), the same rule the Yandex `cluster` stage applies.
//! `clusters-google` pages the stored result.

use std::collections::{BTreeMap, HashMap};

use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::config;
use crate::store::Store;
use crate::Error;

/// A URL that ranks for more phrases than this is a hub (directory, wiki):
/// it would add millions of pair checks and links unrelated phrases.
const HUB_POSTING: usize = 1000;
/// Domains listed per cluster.
const TOP_DOMAINS: usize = 5;
/// Members returned per cluster by `clusters-google`.
const MEMBER_CAP: usize = 100;

/// URL identity for overlap: no scheme, `www.`, query, fragment or trailing slash.
pub fn url_key(url: &str) -> Option<String> {
    let u = url.trim();
    let u = u.split_once("://").map_or(u, |x| x.1);
    let u = u.split(['?', '#']).next().unwrap_or("");
    let (host, path) = u.split_once('/').unwrap_or((u, ""));
    let host = host.to_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let path = path.trim_end_matches('/');
    if host.is_empty() {
        None
    } else if path.is_empty() {
        Some(host.to_owned())
    } else {
        Some(format!("{host}/{path}"))
    }
}

struct Serp {
    norm: String,
    at: (String, String),
    /// (url key, domain, rank), one entry per URL.
    urls: Vec<(String, String, i64)>,
}

#[derive(Default)]
struct Meta {
    volume: i64,
    intent: String,
    page: String,
    verdict: String,
}

fn find(p: &mut [usize], mut x: usize) -> usize {
    while p[x] != x {
        p[x] = p[p[x]];
        x = p[x];
    }
    x
}

/// Most frequent non-empty value; ties go to the alphabetically first.
fn majority<'a>(vals: impl Iterator<Item = &'a str>) -> String {
    let mut m: BTreeMap<&str, usize> = BTreeMap::new();
    for v in vals.filter(|v| !v.is_empty()) {
        *m.entry(v).or_default() += 1;
    }
    let mut best = ("", 0);
    for (k, n) in m {
        if n > best.1 {
            best = (k, n);
        }
    }
    best.0.to_owned()
}

/// Host of the project's own site from `project.json` (`site.host`, else the
/// host of `site.target`), looked up next to or above the semantic directory.
fn target_host(project: &str) -> Option<String> {
    let dir = config::data_dir(project).ok()?;
    let candidates = [
        Some(dir.join("project.json")),
        dir.parent().map(|p| p.join("project.json")),
    ];
    for path in candidates.into_iter().flatten() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(doc) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let site = &doc["site"];
        let host = site["host"].as_str().map(str::to_owned).or_else(|| {
            let t = site["target"].as_str()?;
            url_key(t)?.split('/').next().map(str::to_owned)
        });
        if let Some(h) = host.filter(|h| !h.is_empty()) {
            return Some(h.to_lowercase().trim_start_matches("www.").to_owned());
        }
    }
    None
}

fn is_ours(domain: &str, host: &str) -> bool {
    let d = domain.to_lowercase();
    let d = d.strip_prefix("www.").unwrap_or(&d);
    d == host || d.ends_with(&format!(".{host}"))
}

/// Latest organic top-`top` snapshot per phrase for one scope.
fn load_serps(conn: &Connection, lang: &str, loc: i64, top: i64) -> Result<Vec<Serp>, Error> {
    let mut stmt = conn
        .prepare(
            "SELECT norm, fetched_at, device, COALESCE(rank_group,pos), COALESCE(domain,''), url \
             FROM g_serp WHERE language=?1 AND location_code=?2 AND type='organic' \
             AND url IS NOT NULL AND url<>'' ORDER BY norm, COALESCE(rank_group,pos)",
        )
        .map_err(Error::Db)?;
    let mut rows = stmt.query(params![lang, loc]).map_err(Error::Db)?;
    let mut by_norm: HashMap<String, Serp> = HashMap::new();
    while let Some(r) = rows.next().map_err(Error::Db)? {
        let norm: String = r.get(0).map_err(Error::Db)?;
        let at: (String, String) = (r.get(1).map_err(Error::Db)?, r.get(2).map_err(Error::Db)?);
        let rank: i64 = r.get::<_, Option<i64>>(3).map_err(Error::Db)?.unwrap_or(0);
        let domain: String = r.get(4).map_err(Error::Db)?;
        let url: String = r.get(5).map_err(Error::Db)?;
        if rank < 1 || rank > top {
            continue;
        }
        let Some(key) = url_key(&url) else { continue };
        let entry = by_norm.entry(norm.clone()).or_insert_with(|| Serp {
            norm,
            at: at.clone(),
            urls: Vec::new(),
        });
        if at > entry.at {
            entry.at = at;
            entry.urls.clear();
        } else if at < entry.at {
            continue;
        }
        if !entry.urls.iter().any(|u| u.0 == key) {
            entry.urls.push((key, domain, rank));
        }
    }
    let mut out: Vec<Serp> = by_norm
        .into_values()
        .filter(|s| !s.urls.is_empty())
        .collect();
    out.sort_by(|a, b| a.norm.cmp(&b.norm));
    Ok(out)
}

fn load_meta(conn: &Connection, lang: &str, loc: i64) -> Result<HashMap<String, Meta>, Error> {
    let mut stmt = conn
        .prepare(
            "SELECT norm, COALESCE(volume,0), COALESCE(intent,''), extra_json FROM g_phrases \
             WHERE language=?1 AND location_code=?2",
        )
        .map_err(Error::Db)?;
    let mut rows = stmt.query(params![lang, loc]).map_err(Error::Db)?;
    let mut out: HashMap<String, Meta> = HashMap::new();
    while let Some(r) = rows.next().map_err(Error::Db)? {
        let norm: String = r.get(0).map_err(Error::Db)?;
        let extra: Option<String> = r.get(3).map_err(Error::Db)?;
        let extra: Value = extra
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        let m = out.entry(norm).or_default();
        m.volume = m.volume.max(r.get(1).map_err(Error::Db)?);
        if m.intent.is_empty() {
            m.intent = r.get(2).map_err(Error::Db)?;
        }
        if m.page.is_empty() {
            m.page = extra["jev_page"].as_str().unwrap_or("").to_owned();
        }
        if m.verdict.is_empty() {
            m.verdict = extra["jev_verdict"].as_str().unwrap_or("").to_owned();
        }
    }
    Ok(out)
}

/// Whether the latest snapshot of each phrase showed an AI Overview.
fn load_ai(conn: &Connection, lang: &str, loc: i64) -> Result<HashMap<String, bool>, Error> {
    let mut stmt = conn
        .prepare(
            "SELECT norm, ai_overview FROM g_serp_features WHERE language=?1 AND location_code=?2 \
             ORDER BY fetched_at",
        )
        .map_err(Error::Db)?;
    let rows = stmt
        .query_map(params![lang, loc], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? > 0))
        })
        .map_err(Error::Db)?;
    // Ascending by time: the last snapshot wins.
    Ok(rows.flatten().collect())
}

fn load_topics(conn: &Connection, lang: &str) -> Result<HashMap<String, Vec<String>>, Error> {
    let mut stmt = conn
        .prepare(
            "SELECT norm, topic_id FROM topic_members WHERE engine='google' AND language=?1 \
             ORDER BY topic_id",
        )
        .map_err(Error::Db)?;
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for (norm, id) in stmt
        .query_map([lang], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(Error::Db)?
        .flatten()
    {
        out.entry(norm).or_default().push(id);
    }
    Ok(out)
}

fn scopes(
    conn: &Connection,
    lang: Option<&str>,
    loc: Option<i64>,
) -> Result<Vec<(String, i64)>, Error> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT language, location_code FROM g_serp WHERE type='organic' \
             AND (?1 IS NULL OR language=?1) AND (?2 IS NULL OR location_code=?2) \
             ORDER BY language, location_code",
        )
        .map_err(Error::Db)?;
    let rows = stmt
        .query_map(params![lang, loc], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })
        .map_err(Error::Db)?;
    Ok(rows.flatten().collect())
}

/// Cluster one scope and replace its stored clusters.
fn cluster_scope(
    conn: &Connection,
    lang: &str,
    loc: i64,
    min_shared: i64,
    top: i64,
    host: Option<&str>,
) -> Result<Value, Error> {
    let serps = load_serps(conn, lang, loc, top)?;
    let meta = load_meta(conn, lang, loc)?;
    let ai = load_ai(conn, lang, loc)?;
    let topics = load_topics(conn, lang)?;

    // url -> phrases ranking for it; shared-URL counts per phrase pair.
    let mut index: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, s) in serps.iter().enumerate() {
        for u in &s.urls {
            index.entry(u.0.as_str()).or_default().push(i);
        }
    }
    let mut pairs: HashMap<(usize, usize), i64> = HashMap::new();
    for list in index
        .values()
        .filter(|l| l.len() > 1 && l.len() <= HUB_POSTING)
    {
        for (k, &a) in list.iter().enumerate() {
            for &b in &list[k + 1..] {
                *pairs.entry((a, b)).or_default() += 1;
            }
        }
    }
    let mut parent: Vec<usize> = (0..serps.len()).collect();
    for (&(a, b), &n) in &pairs {
        if n >= min_shared {
            let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
            if ra != rb {
                parent[ra.max(rb)] = ra.min(rb);
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..serps.len() {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }

    let volume = |i: usize| meta.get(&serps[i].norm).map_or(0, |m| m.volume);
    let mut groups: Vec<Vec<usize>> = groups.into_values().collect();
    for g in &mut groups {
        g.sort_by(|&a, &b| {
            volume(b)
                .cmp(&volume(a))
                .then_with(|| serps[a].norm.cmp(&serps[b].norm))
        });
    }
    groups.sort_by(|a, b| {
        let (va, vb): (i64, i64) = (
            a.iter().map(|&i| volume(i)).sum(),
            b.iter().map(|&i| volume(i)).sum(),
        );
        vb.cmp(&va)
            .then(b.len().cmp(&a.len()))
            .then_with(|| serps[a[0]].norm.cmp(&serps[b[0]].norm))
    });

    conn.execute(
        "DELETE FROM g_cluster_members WHERE cluster_id IN \
         (SELECT id FROM g_clusters WHERE language=?1 AND location_code=?2)",
        params![lang, loc],
    )
    .map_err(Error::Db)?;
    conn.execute(
        "DELETE FROM g_clusters WHERE language=?1 AND location_code=?2",
        params![lang, loc],
    )
    .map_err(Error::Db)?;

    let mut singletons = 0;
    for g in &groups {
        if g.len() == 1 {
            singletons += 1;
        }
        let lead = &serps[g[0]].norm;
        let mut doms: BTreeMap<&str, i64> = BTreeMap::new();
        let mut ours: Option<(i64, &str)> = None;
        for &i in g {
            let mut seen: Vec<&str> = Vec::new();
            for (key, d, rank) in &serps[i].urls {
                let d = d.strip_prefix("www.").unwrap_or(d);
                if !d.is_empty() && !seen.contains(&d) {
                    seen.push(d);
                    *doms.entry(d).or_default() += 1;
                }
                if host.is_some_and(|h| is_ours(d, h)) && ours.is_none_or(|o| *rank < o.0) {
                    ours = Some((*rank, key));
                }
            }
        }
        let mut doms: Vec<(&str, i64)> = doms.into_iter().collect();
        doms.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let top_domains: Vec<Value> = doms
            .iter()
            .take(TOP_DOMAINS)
            .map(|(d, n)| json!({"domain": d, "n": n}))
            .collect();
        let flags: Vec<bool> = g
            .iter()
            .filter_map(|&i| ai.get(&serps[i].norm).copied())
            .collect();
        let ai_share = (!flags.is_empty()).then(|| {
            (flags.iter().filter(|f| **f).count() as f64 / flags.len() as f64 * 1000.0).round()
                / 1000.0
        });
        let metas: Vec<&Meta> = g.iter().filter_map(|&i| meta.get(&serps[i].norm)).collect();
        let mut ids: Vec<&str> = g
            .iter()
            .filter_map(|&i| topics.get(&serps[i].norm))
            .flatten()
            .map(String::as_str)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        conn.execute(
            "INSERT INTO g_clusters(language,location_code,name,marker_norm,size,volume_sum,min_shared,\
             ai_share,top_domains_json,intent,page_type,verdict,our_pos,our_url,topics) \
             VALUES(?1,?2,?3,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![
                lang,
                loc,
                lead,
                g.len() as i64,
                g.iter().map(|&i| volume(i)).sum::<i64>(),
                min_shared,
                ai_share,
                Value::Array(top_domains).to_string(),
                majority(metas.iter().map(|m| m.intent.as_str())),
                majority(metas.iter().map(|m| m.page.as_str())),
                majority(metas.iter().map(|m| m.verdict.as_str())),
                ours.map(|o| o.0),
                ours.map(|o| o.1.to_owned()),
                ids.join(","),
            ],
        )
        .map_err(Error::Db)?;
        let id = conn.last_insert_rowid();
        for &i in g {
            conn.execute(
                "INSERT INTO g_cluster_members(cluster_id,norm) VALUES(?,?)",
                params![id, serps[i].norm],
            )
            .map_err(Error::Db)?;
        }
    }
    Ok(json!({
        "language": lang,
        "location_code": loc,
        "phrases": serps.len(),
        "clusters": groups.len(),
        "singletons": singletons,
        "largest": groups.iter().map(Vec::len).max().unwrap_or(0),
        "hub_urls_skipped": index.values().filter(|l| l.len() > HUB_POSTING).count(),
    }))
}

/// Cluster every (or the requested) language and location that has SERP data.
pub fn cluster_google(
    store: &Store,
    project: &str,
    language: Option<&str>,
    location: Option<i64>,
    min_shared: i64,
    top: i64,
) -> Result<Value, Error> {
    if min_shared < 1 || top < 1 {
        return Err(Error::Config(
            "--min-shared and --top must be at least 1".into(),
        ));
    }
    super::ensure_schema(&store.conn)?;
    let host = target_host(project);
    let tx = store.conn.unchecked_transaction().map_err(Error::Db)?;
    let mut done = Vec::new();
    for (lang, loc) in scopes(&tx, language, location)? {
        done.push(cluster_scope(
            &tx,
            &lang,
            loc,
            min_shared,
            top,
            host.as_deref(),
        )?);
    }
    tx.commit().map_err(Error::Db)?;
    Ok(json!({
        "contract": "spiderhead.semantics.operation.v1",
        "stage": "cluster-google",
        "min_shared": min_shared,
        "top": top,
        "our_domain": host,
        "scopes": done,
    }))
}

/// One page of stored clusters, largest demand first.
pub fn clusters_google(
    store: &Store,
    language: Option<&str>,
    location: Option<i64>,
    offset: i64,
    limit: i64,
) -> Result<Value, Error> {
    let conn = &store.conn;
    let has: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='g_clusters'",
            [],
            |r| r.get(0),
        )
        .map_err(Error::Db)?;
    let (offset, limit) = (offset.max(0), if limit > 0 { limit } else { 50 });
    let mut out = Vec::new();
    let mut total = 0;
    if has > 0 {
        let filter = "(?1 IS NULL OR language=?1) AND (?2 IS NULL OR location_code=?2)";
        total = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM g_clusters WHERE {filter}"),
                params![language, location],
                |r| r.get(0),
            )
            .map_err(Error::Db)?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT id,language,location_code,name,size,volume_sum,min_shared,ai_share,\
                 top_domains_json,intent,page_type,verdict,our_pos,our_url,topics \
                 FROM g_clusters WHERE {filter} ORDER BY volume_sum DESC, size DESC, id LIMIT ?3 OFFSET ?4"
            ))
            .map_err(Error::Db)?;
        let mut members = conn
            .prepare(
                "SELECT m.norm FROM g_cluster_members m LEFT JOIN g_phrases p ON p.norm=m.norm \
                 AND p.language=?2 AND p.location_code=?3 WHERE m.cluster_id=?1 \
                 GROUP BY m.norm ORDER BY MAX(COALESCE(p.volume,0)) DESC, m.norm",
            )
            .map_err(Error::Db)?;
        let mut topic_name = conn
            .prepare("SELECT name FROM topics WHERE id=?")
            .map_err(Error::Db)?;
        let mut rows = stmt
            .query(params![language, location, limit, offset])
            .map_err(Error::Db)?;
        while let Some(r) = rows.next().map_err(Error::Db)? {
            let id: i64 = r.get(0).map_err(Error::Db)?;
            let lang: String = r.get(1).map_err(Error::Db)?;
            let loc: i64 = r.get(2).map_err(Error::Db)?;
            let all: Vec<String> = members
                .query_map(params![id, lang, loc], |m| m.get(0))
                .map_err(Error::Db)?
                .flatten()
                .collect();
            let topics: Vec<Value> = r
                .get::<_, Option<String>>(14)
                .map_err(Error::Db)?
                .unwrap_or_default()
                .split(',')
                .filter(|t| !t.is_empty())
                .map(|t| {
                    let name: Option<String> = topic_name.query_row([t], |x| x.get(0)).ok();
                    json!({"id": t, "name": name})
                })
                .collect();
            let doms: Value = r
                .get::<_, Option<String>>(8)
                .map_err(Error::Db)?
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(Value::Null);
            out.push(json!({
                "id": id, "language": lang, "location_code": loc,
                "name": r.get::<_, String>(3).map_err(Error::Db)?,
                "size": r.get::<_, i64>(4).map_err(Error::Db)?,
                "volume_sum": r.get::<_, Option<i64>>(5).map_err(Error::Db)?,
                "min_shared": r.get::<_, i64>(6).map_err(Error::Db)?,
                "ai_overview_share": r.get::<_, Option<f64>>(7).map_err(Error::Db)?,
                "top_domains": doms,
                "intent": r.get::<_, Option<String>>(9).map_err(Error::Db)?,
                "page_type": r.get::<_, Option<String>>(10).map_err(Error::Db)?,
                "verdict": r.get::<_, Option<String>>(11).map_err(Error::Db)?,
                "our_position": r.get::<_, Option<i64>>(12).map_err(Error::Db)?,
                "our_url": r.get::<_, Option<String>>(13).map_err(Error::Db)?,
                "topics": topics,
                "members_total": all.len(),
                "members": all.iter().take(MEMBER_CAP).collect::<Vec<_>>(),
            }));
        }
    }
    let next = offset + out.len() as i64;
    Ok(json!({
        "contract": "spiderhead.semantics.operation.v1",
        "stage": "clusters-google",
        "total": total,
        "offset": offset,
        "limit": limit,
        "next_offset": (next < total).then_some(next),
        "clusters": out,
    }))
}

/// Per-language/location cluster counts for `status`.
pub fn counts(conn: &Connection) -> Value {
    let mut out = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT language, location_code, COUNT(*), SUM(size=1), SUM(size) FROM g_clusters \
         GROUP BY 1,2 ORDER BY 5 DESC",
    ) {
        let _ = stmt
            .query_map([], |r| {
                Ok(json!({
                    "language": r.get::<_, String>(0)?,
                    "location_code": r.get::<_, i64>(1)?,
                    "clusters": r.get::<_, i64>(2)?,
                    "singletons": r.get::<_, i64>(3)?,
                    "phrases": r.get::<_, i64>(4)?,
                }))
            })
            .map(|it| out.extend(it.flatten()));
    }
    Value::Array(out)
}
