//! `cluster`: SERP clustering of the whole core:
//! paid Yandex SERP fetch cached per phrase in `serp` + `serp_fetches`
//! receipts, then union-find on `>= serp_overlap` shared page URLs or Louvain
//! communities on shared-URL edge weights.

use indexmap::IndexMap;
use serde_json::{json, Value};
use std::collections::HashSet;

use crate::config::Cfg;
use crate::louvain::{best_partition, PyGraph};
use crate::providers::SerpProvider;
use crate::pyfmt::{dumps, py_round, py_str};
use crate::pyset::{py_str_hash, PyStrSet};
use crate::store::{zulu_seconds, Row, Store};

/// Hub domains (portals, wikis, socials, marketplaces, gov/edu, stock media)
/// appear in almost any SERP and carry no topic signal.
const HUB_EXACT: &[&str] = &[
    "yandex.ru",
    "ya.ru",
    "google.com",
    "mail.ru",
    "dzen.ru",
    "habr.com",
    "vc.ru",
    "pikabu.ru",
    "vk.com",
    "ok.ru",
    "t.me",
    "youtube.com",
    "rutube.ru",
    "music.yandex.ru",
    "music.apple.com",
    "ozon.ru",
    "aliexpress.ru",
    "wildberries.ru",
    "avito.ru",
    "market.yandex.ru",
    "pult.ru",
    "ru.freepik.com",
    "freepik.com",
    "shutterstock.com",
    "istockphoto.com",
    "gettyimages.com",
    "ru.wikipedia.org",
    "en.wikipedia.org",
    "ru.ruwiki.ru",
    "cyberleninka.ru",
    "moluch.ru",
    "rucont.ru",
    "otvet.mail.ru",
    "hh.ru",
    "superjob.ru",
    "2gis.ru",
    "zoon.ru",
    "tadviser.ru",
    "consultant.ru",
    "konsultant.ru",
    "regulation.gov.ru",
];

/// Hub domain suffixes (whole subtrees).
const HUB_SUFFIX: &[&str] = &[
    ".gov.ru",
    ".gov.by",
    ".gov.kz",
    ".gosuslugi.ru",
    ".mos.ru",
    ".edu.ru",
    ".edu.by",
    "wikipedia.org",
    "ruwiki.ru",
];

/// Whether the host matches an exact hub or a hub suffix subtree.
pub fn is_hub(dom: &str) -> bool {
    let host = dom.to_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let host = host.trim_end_matches('.');
    if host.is_empty() {
        return false;
    }
    if HUB_EXACT.contains(&host) {
        return true;
    }
    HUB_SUFFIX.iter().any(|suffix| {
        let s = suffix.trim_start_matches('.');
        host == s || host.ends_with(&format!(".{s}"))
    })
}

/// Hostname: netloc without userinfo or port, lowercased,
/// IPv6 brackets stripped.
fn hostname_of(netloc: &str) -> String {
    let hostinfo = netloc.rsplit('@').next().unwrap_or(netloc);
    if let Some(rest) = hostinfo.strip_prefix('[') {
        match rest.find(']') {
            Some(i) => rest[..i].to_lowercase(),
            None => rest.trim_end_matches(']').to_lowercase(),
        }
    } else {
        hostinfo
            .split(':')
            .next()
            .unwrap_or(hostinfo)
            .to_lowercase()
    }
}

/// Port: `Err` for a garbage or out-of-range port;
/// otherwise the numeric port.
fn port_of(netloc: &str) -> Result<Option<i64>, crate::Error> {
    let hostinfo = netloc.rsplit('@').next().unwrap_or(netloc);
    let port_text = if hostinfo.starts_with('[') {
        match hostinfo.find(']') {
            Some(i) if hostinfo.as_bytes().get(i + 1) == Some(&b':') => Some(&hostinfo[i + 2..]),
            _ => None,
        }
    } else {
        hostinfo.split_once(':').map(|(_, p)| p)
    };
    match port_text {
        None => Ok(None),
        Some(t) => {
            let p = t.parse::<i64>().map_err(|_| {
                crate::Error::Config(format!("Port could not be cast to integer value as {t:?}"))
            })?;
            if !(0..=65535).contains(&p) {
                return Err(crate::Error::Config(format!(
                    "Port out of range 0-65535 as {t:?}"
                )));
            }
            Ok(Some(p))
        }
    }
}

/// Netloc and path of an absolute URL, split like a standard URL parser
/// (query and fragment dropped) — same split `core::url_variants` uses.
fn split_netloc_and_path(with_scheme: &str) -> (String, String) {
    let rest = with_scheme
        .split_once("://")
        .map_or(with_scheme, |(_, after)| after);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let netloc = &rest[..end];
    let tail = &rest[end..];
    let path_end = tail.find(['?', '#']).unwrap_or(tail.len());
    (netloc.to_owned(), tail[..path_end].to_owned())
}

/// Normalized host + port + case-sensitive path, no trailing
/// slash.
pub fn url_key(u: &str) -> Result<String, crate::Error> {
    let value = u.trim();
    let with_scheme = if value.contains("://") {
        value.to_string()
    } else {
        format!("https://{value}")
    };
    let (netloc, path) = split_netloc_and_path(&with_scheme);
    let host = hostname_of(&netloc);
    let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
    let port = match port_of(&netloc)? {
        Some(p) if p != 80 && p != 443 => format!(":{p}"),
        _ => String::new(),
    };
    Ok(format!("{host}{port}{path}")
        .trim_end_matches('/')
        .to_string())
}

/// Hub check on the URL's hostname (no port).
pub fn is_hub_url(u: &str) -> Result<bool, crate::Error> {
    let with_scheme = if u.contains("://") {
        u.to_string()
    } else {
        format!("https://{u}")
    };
    let (netloc, _) = split_netloc_and_path(&with_scheme);
    Ok(is_hub(&hostname_of(&netloc)))
}

/// Union-find over phrase names (same as `graph::Uf`, kept local).
struct Uf {
    p: std::collections::HashMap<String, String>,
}

impl Uf {
    fn new() -> Self {
        Self {
            p: std::collections::HashMap::new(),
        }
    }

    fn find(&mut self, x: &str) -> String {
        self.p.entry(x.to_string()).or_insert_with(|| x.to_string());
        let mut x = x.to_string();
        while self.p[&x] != x {
            let next = self.p[&self.p[&x]].clone();
            self.p.insert(x.clone(), next.clone());
            x = next;
        }
        x
    }

    fn union(&mut self, a: &str, b: &str) {
        let ra = self.find(a);
        let rb = self.find(b);
        self.p.insert(ra, rb);
    }
}

/// Louvain communities over shared-page weights.
fn louvain_groups(
    keys: &[String],
    shared: &IndexMap<(String, String), i64>,
    resolution: f64,
) -> IndexMap<i64, Vec<String>> {
    let mut graph = PyGraph::new();
    for k in keys {
        graph.add_node(k);
    }
    for ((a, b), count) in shared {
        if *count >= 2 {
            // an edge needs two shared pages; one shared page chains weak links
            graph.add_edge(a, b, *count as f64);
        }
    }
    let partition = best_partition(&graph, resolution, 42);
    let mut groups: IndexMap<i64, Vec<String>> = IndexMap::new();
    for (node, com) in partition {
        groups.entry(com).or_default().push(node);
    }
    groups
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Prefix of `items`: the first `n` items, or all but the last `|n|` when `n` is negative.
fn take_py<T: Clone>(items: &[T], n: i64) -> Vec<T> {
    let len = items.len() as i64;
    let end = if n < 0 { (len + n).max(0) } else { n.min(len) };
    items[..end.max(0) as usize].to_vec()
}

/// Run SERP clustering through the `serp` provider; `method` picks union-find (`serp`) or Louvain (`louvain`), `limit` bounds the phrases and `top` the results per phrase.
pub fn run(
    store: &Store,
    cfg: &Cfg,
    limit: i64,
    top: i64,
    method: &str,
    serp: &mut dyn SerpProvider,
) -> Result<Value, crate::Error> {
    if method != "serp" && method != "louvain" {
        return Err(crate::Error::Config(
            "cluster method must be serp or louvain".into(),
        ));
    }
    let overlap = crate::config::cfg_i64(cfg, "serp_overlap", 3);
    let region = match cfg.get("regions") {
        Some(Value::Array(items)) if !items.is_empty() => py_str(&items[0]),
        _ => "225".to_string(),
    };
    // Every kept phrase: clean already applied the base cut; exact frequency is not required.
    let mut rows = store.phrases(Some("kept"), false, None)?;
    rows.sort_by(|a, b| {
        let key = |r: &Row| {
            let exact = r["exact"].as_i64().filter(|v| *v != 0);
            let base = r["base"].as_i64().filter(|v| *v != 0);
            exact.or(base).unwrap_or(0)
        };
        key(b).cmp(&key(a)) // stable sort like Python's sorted(key=-k)
    });
    if limit != 0 {
        rows.truncate(limit.max(0) as usize);
        println!(
            "warning: cluster --limit {limit} splits clusters; use it for debugging only \
             and run the whole core without --limit."
        );
    }
    if rows.is_empty() {
        println!("cluster: no kept phrases");
        return Ok(json!({"clusters": 0}));
    }

    store.exec(
        "CREATE TABLE IF NOT EXISTS serp_fetches(\
        phrase_norm TEXT, region TEXT, top INTEGER, state TEXT, operation_id TEXT,\
        raw_json TEXT, error TEXT, fetched_at TEXT, PRIMARY KEY(phrase_norm, region, top))",
        &[],
    )?;
    let receipt_columns: HashSet<String> = store
        .q("PRAGMA table_info(serp_fetches)", &[])?
        .into_iter()
        .map(|r| r["name"].as_str().unwrap_or_default().to_string())
        .collect();
    if !receipt_columns.contains("fetched_at") {
        store.exec("ALTER TABLE serp_fetches ADD COLUMN fetched_at TEXT", &[])?;
    }
    let mut n_req = 0i64;
    let mut doms: IndexMap<String, PyStrSet> = IndexMap::new();
    let mut todo: Vec<String> = Vec::new();
    let receipts: IndexMap<String, Row> = store
        .q(
            "SELECT phrase_norm,state,fetched_at FROM serp_fetches WHERE region=? AND top=?",
            &[json!(region), json!(top)],
        )?
        .into_iter()
        .map(|r| (r["phrase_norm"].as_str().unwrap_or_default().to_string(), r))
        .collect();
    let has_receipt: HashSet<String> = store
        .q("SELECT DISTINCT phrase_norm FROM serp_fetches", &[])?
        .into_iter()
        .map(|r| r["phrase_norm"].as_str().unwrap_or_default().to_string())
        .collect();
    for r in &rows {
        let norm = r["norm"].as_str().unwrap_or_default().to_string();
        let receipt = receipts.get(&norm);
        let cached = store.serp_for(&norm)?;
        if let Some(rc) = receipt {
            if rc["state"].as_str() != Some("complete") {
                return Err(crate::Error::Config(format!(
                    "unresolved paid SERP operation for {norm:?}; \
                     recover the provider receipt before retrying"
                )));
            }
            let raw = store
                .q1(
                    "SELECT raw_json FROM serp_fetches WHERE phrase_norm=? AND region=? AND top=?",
                    &[json!(norm), json!(region), json!(top)],
                )?
                .and_then(|row| row["raw_json"].as_str().map(String::from))
                .unwrap_or_else(|| "null".to_string());
            let body: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
            // `json.loads(raw)["docs"]` — a missing/invalid payload raises.
            let docs: Vec<Value> = match body.get("docs").and_then(|d| d.as_array()) {
                Some(a) => take_py(a, top),
                None => {
                    return Err(crate::Error::Config(format!(
                        "corrupt SERP receipt for {norm:?}"
                    )))
                }
            };
            if cached.is_empty() {
                let doc_rows: Vec<Row> = docs
                    .iter()
                    .filter_map(|d| d.as_object())
                    .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                    .collect();
                store.add_serp(&norm, &doc_rows, rc["fetched_at"].as_str())?;
            }
            let keys: Vec<String> = docs
                .iter()
                .filter_map(|d| {
                    let u = d["url"].as_str().filter(|s| !s.is_empty())?;
                    Some(u.to_string())
                })
                .collect();
            let mut set = PyStrSet::new(|s: &String| py_str_hash(s));
            for k in keys {
                set.add(url_key(&k)?);
            }
            doms.insert(norm, set);
        } else if !cached.is_empty() && !has_receipt.contains(&norm) {
            // Legacy SERP has no region receipt; retain it until an explicit new fetch is recorded.
            let mut set = PyStrSet::new(|s: &String| py_str_hash(s));
            for d in take_py(&cached, top).iter() {
                if let Some(u) = d["url"].as_str().filter(|s| !s.is_empty()) {
                    set.add(url_key(u)?);
                }
            }
            doms.insert(norm, set);
        } else {
            todo.push(norm);
        }
    }
    // Reserve before submission: an interrupted paid batch cannot be silently resubmitted.
    for (batch_i, chunk) in todo.chunks(40).enumerate() {
        for p in chunk {
            store.exec(
                "INSERT OR REPLACE INTO serp_fetches(phrase_norm,region,top,state) \
                 VALUES(?,?,?,'pending')",
                &[json!(p), json!(region), json!(top)],
            )?;
        }
        store.commit()?;
        let queries: Vec<String> = chunk.to_vec();
        let res = serp
            .search_batch(&queries, &region, top)
            .map_err(|e| crate::Error::Stage {
                stage: "cluster".into(),
                message: e.message,
            })?;
        let mut unresolved: Vec<String> = Vec::new();
        let mut submitted = 0i64;
        for p in chunk {
            let result = res.get(p).cloned();
            let mut state = "pending";
            let mut paid_docs: Option<Vec<Value>> = None;
            if let Some(obj) = result.as_ref().and_then(|v| v.as_object()) {
                if obj.get("status").and_then(|v| v.as_str()) == Some("rejected") {
                    store.exec(
                        "DELETE FROM serp_fetches WHERE phrase_norm=? AND region=? AND top=?",
                        &[json!(p), json!(region), json!(top)],
                    )?;
                    unresolved.push(p.clone());
                    continue;
                }
                if obj.get("operation_id").is_some_and(truthy) {
                    submitted += 1;
                }
                if !obj.get("error").is_some_and(truthy)
                    && obj.get("docs").is_some_and(|d| d.is_array())
                {
                    state = "complete";
                    let docs: Vec<Value> = obj["docs"]
                        .as_array()
                        .map(|a| take_py(a, top))
                        .unwrap_or_default();
                    let doc_rows: Vec<Row> = docs
                        .iter()
                        .filter_map(|d| d.as_object())
                        .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                        .collect();
                    store.add_serp(p, &doc_rows, None)?;
                    paid_docs = Some(docs);
                }
            }
            let empty = serde_json::Map::new();
            let res_obj = result
                .as_ref()
                .and_then(|v| v.as_object())
                .unwrap_or(&empty);
            let error = match res_obj.get("error") {
                Some(v) if truthy(v) => Some(py_str(v)),
                _ => None,
            };
            store.exec(
                "UPDATE serp_fetches SET state=?,operation_id=?,raw_json=?,error=?,fetched_at=? \
                 WHERE phrase_norm=? AND region=? AND top=?",
                &[
                    json!(state),
                    res_obj.get("operation_id").cloned().unwrap_or(Value::Null),
                    json!(dumps(result.as_ref().unwrap_or(&Value::Null))),
                    json!(error),
                    if state == "complete" {
                        json!(zulu_seconds())
                    } else {
                        Value::Null
                    },
                    json!(p),
                    json!(region),
                    json!(top),
                ],
            )?;
            if state != "complete" {
                unresolved.push(p.clone());
            }
            // The receipt is durable now: a bad URL in the paid payload must
            // abort the run without stranding the receipt 'pending' (a pending
            // row would mask the real error as 'unresolved paid SERP operation'
            // on every rerun).
            if let Some(docs) = paid_docs {
                let mut set = PyStrSet::new(|s: &String| py_str_hash(s));
                for d in &docs {
                    if let Some(u) = d["url"].as_str().filter(|s| !s.is_empty()) {
                        set.add(url_key(u)?);
                    }
                }
                doms.insert(p.clone(), set);
            }
        }
        n_req += submitted;
        store.add_run(
            "cluster",
            submitted,
            0,
            &format!(
                "SERP batch: complete={}, unresolved={}",
                chunk.len() - unresolved.len(),
                unresolved.len()
            ),
        )?;
        if !unresolved.is_empty() {
            return Err(crate::Error::Config(
                "SERP batch incomplete; retained receipts must be recovered before retrying".into(),
            ));
        }
        println!(
            "  SERP batch {}: +{} (total {})",
            batch_i + 1,
            chunk.len(),
            doms.len()
        );
    }

    // Inverted index URL -> phrases; shared-URL counts per pair; union at >= overlap. Hub URLs
    // and ubiquitous pages (> 4% of results) are noise. Cost is O(sum of pairs), not O(n^2).
    let keys: Vec<String> = doms.keys().cloned().collect();
    let mut dom_index: IndexMap<String, Vec<String>> = IndexMap::new();
    for p in &keys {
        for d in doms[p].iter() {
            dom_index.entry(d).or_default().push(p.clone());
        }
    }
    let ubiq = 4.max((0.04 * keys.len() as f64) as i64);
    let mut shared: IndexMap<(String, String), i64> = IndexMap::new();
    for (d, plist) in &dom_index {
        if is_hub_url(d)? || plist.len() as i64 > ubiq {
            continue;
        }
        let mut sorted = plist.clone();
        sorted.sort();
        for i in 0..sorted.len() {
            for j in (i + 1)..sorted.len() {
                *shared
                    .entry((sorted[i].clone(), sorted[j].clone()))
                    .or_insert(0) += 1;
            }
        }
    }
    let groups_vec: Vec<Vec<String>> = if method == "louvain" {
        let resolution = cfg
            .get("louvain_resolution")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.2);
        louvain_groups(&keys, &shared, resolution)
            .into_values()
            .collect()
    } else {
        let mut uf = Uf::new();
        for p in &keys {
            uf.find(p); // every phrase is a node, singletons included
        }
        for ((a, b), c) in &shared {
            if *c >= overlap {
                uf.union(a, b);
            }
        }
        let mut groups: IndexMap<String, Vec<String>> = IndexMap::new();
        for p in &keys {
            let root = uf.find(p);
            groups.entry(root).or_default().push(p.clone());
        }
        groups.into_values().collect()
    };

    let weights: IndexMap<String, i64> = rows
        .iter()
        .map(|r| {
            (
                r["norm"].as_str().unwrap_or_default().to_string(),
                match r["exact"].as_i64() {
                    Some(e) => e,
                    None => r["base"].as_i64().unwrap_or(0),
                },
            )
        })
        .collect();
    let protected: HashSet<String> = store
        .q(
            "SELECT p.norm FROM phrases p JOIN clusters c ON c.id=p.cluster_id \
             WHERE c.source NOT IN ('serp','louvain') OR c.landing_url IS NOT NULL \
             OR (c.source IS NULL AND c.marker_norm IS NULL)",
            &[],
        )?
        .into_iter()
        .map(|r| r["norm"].as_str().unwrap_or_default().to_string())
        .collect();
    let mut generated: IndexMap<String, i64> = store
        .q(
            "SELECT name,id FROM clusters WHERE source=?",
            &[json!(method)],
        )?
        .into_iter()
        .map(|r| {
            (
                r["name"].as_str().unwrap_or_default().to_string(),
                r["id"].as_i64().unwrap_or(0),
            )
        })
        .collect();
    let mut made = 0i64;
    let mut sorted_groups = groups_vec;
    sorted_groups.sort_by_key(|a| std::cmp::Reverse(a.len()));
    for members in sorted_groups {
        // On ties, the first maximum wins.
        let mut marker = members.first().cloned().unwrap_or_default();
        let mut best = weights.get(&marker).copied().unwrap_or(0);
        for m in &members[1..] {
            let w = weights.get(m).copied().unwrap_or(0);
            if w > best {
                best = w;
                marker = m.clone();
            }
        }
        // coherence: mean pairwise Jaccard share of URLs — |A∪B| is
        // |A| + |B| − |A∩B|, so the union set is never materialized and the
        // intersection walks the smaller side.
        let coh = if members.len() > 1 {
            let mut sum = 0.0f64;
            for i in 0..members.len() {
                for j in (i + 1)..members.len() {
                    let (a, b) = (&doms[&members[i]], &doms[&members[j]]);
                    let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
                    let inter = small.iter().filter(|x| large.contains(x)).count();
                    let union = a.len() + b.len() - inter;
                    sum += inter as f64 / union.max(1) as f64;
                }
            }
            py_round(sum / (members.len() * (members.len() - 1) / 2) as f64, 3)
        } else {
            1.0
        };
        let cid = match generated.get(&marker) {
            Some(id) => *id,
            None => {
                store.exec(
                    "INSERT INTO clusters(name,source) VALUES(?,?)",
                    &[json!(marker), json!(method)],
                )?;
                let id = store.conn.last_insert_rowid();
                generated.insert(marker.clone(), id);
                id
            }
        };
        store.exec(
            "UPDATE clusters SET marker_norm=?,coherence=?,is_real=? WHERE id=?",
            &[
                json!(marker),
                json!(coh),
                json!(if coh >= 0.1 { 1 } else { 0 }),
                json!(cid),
            ],
        )?;
        for p in &members {
            if protected.contains(p) {
                continue;
            }
            store.set_fields(p, &[("cluster_id".to_string(), json!(cid))])?;
        }
        made += 1;
    }
    store.commit()?;
    store.add_run(
        "cluster",
        0,
        0,
        &format!("method={method}, clusters={made}, SERP requests={n_req}"),
    )?;
    println!(
        "cluster: method={method}, clusters={made} ({} phrases, SERP requests={n_req})",
        keys.len()
    );
    Ok(json!({"clusters": made, "requests": n_req, "method": method}))
}
