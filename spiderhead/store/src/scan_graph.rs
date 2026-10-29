//! Bounded, read-only saved-scan graph pages and SQL edge comparison.

#![allow(clippy::too_many_arguments, clippy::type_complexity)]

use crate::crawl_records::spider::canonical_key;
use crate::error::StoreError;
use crate::frontier::defrag;
use crate::link_context::cell_value;
use crate::link_positions::position_summary;
use crate::link_queries::{
    representation as checked_representation, spellings, url as checked_url,
};
use crate::scan_artifact::ScanError;
use crate::url_links::{header_parts, ids, open_light, LinksError, FRAGMENT_VARIANTS};
use rusqlite::functions::FunctionFlags;
use rusqlite::types::Value as Sql;
use rusqlite::{params_from_iter, Connection};
use serde_json::{json, Map, Value};
use spiderhead_core::url_normalize::norm_url;
use spiderhead_parse::parser::meta::robots_directives;
use spiderhead_parse::py_url::{urljoin, urlsplit, urlunsplit};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::time::{Duration, Instant};

/// Format label of the result.
pub const FORMAT: &str = "spiderhead.scan-graph.v1";
/// Most nodes one page may describe.
pub const MAX_NODES: i64 = 5000;
/// Most edges one page may describe.
pub const MAX_EDGES: i64 = 20000;
/// Rows a section aggregation may examine.
pub const AGGREGATE_SCAN_CAP: i64 = 20000;
/// Link positions a filter may name, sorted for a stable order.
pub const POSITION_NAMES: &[&str] = &[
    "breadcrumbs",
    "content",
    "footer",
    "header",
    "nav",
    "not_classified",
    "other",
    "sidebar",
];

type Row = Map<String, Value>;

/// Robots directive tokens of `(meta robots, X-Robots-Tag)`; production uses the parser's.
pub type RobotsFn<'a> = &'a dyn Fn(Option<&str>, Option<&str>) -> Vec<String>;

fn parser_robots(meta: Option<&str>, x: Option<&str>) -> Vec<String> {
    robots_directives(&[meta, x])
}

fn scan_err(message: impl Into<String>) -> StoreError {
    StoreError::ScanError(ScanError {
        message: message.into(),
    })
}

fn fetch(con: &Connection, sql: &str, params: &[Sql]) -> Result<Vec<Row>, StoreError> {
    let mut stmt = con.prepare(sql)?;
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt.query(params_from_iter(params.iter()))?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let mut map = Map::new();
        for (index, name) in names.iter().enumerate() {
            map.insert(name.clone(), cell_value(row.get_ref(index)?));
        }
        out.push(map);
    }
    Ok(out)
}

fn text(sql: impl Into<String>) -> Sql {
    Sql::Text(sql.into())
}

/// Validate a position filter (comma text or a list) and return it sorted and unique.
pub fn positions(value: Option<&Value>) -> Result<Vec<String>, StoreError> {
    let bad = || {
        scan_err(
            "positions must contain header, content, footer, sidebar, breadcrumbs, nav, other, or not_classified",
        )
    };
    let values: Vec<Value> = match value {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::String(s)) => s.split(',').map(|p| json!(p)).collect(),
        Some(Value::Array(items)) => items.clone(),
        Some(_) => return Err(bad()),
    };
    if values.is_empty() {
        return Err(bad());
    }
    let mut names = BTreeSet::new();
    for item in &values {
        match item.as_str() {
            Some(name) if POSITION_NAMES.contains(&name) => {
                names.insert(name.to_string());
            }
            _ => return Err(bad()),
        }
    }
    Ok(names.into_iter().collect())
}

/// SQL predicate and parameters for the position/representation filters.
/// The link alias defaults to `l`.
pub fn link_filter(positions: &[String], representation: &str, alias: &str) -> (String, Vec<Sql>) {
    let (mut clauses, mut params) = (Vec::new(), Vec::new());
    if !positions.is_empty() {
        clauses.push(format!(
            "{alias}.position IN ({})",
            vec!["?"; positions.len()].join(",")
        ));
        params.extend(positions.iter().map(|p| {
            text(if p == "not_classified" {
                ""
            } else {
                p.as_str()
            })
        }));
    }
    if representation != "all" {
        clauses.push(format!("{alias}.evidence_representation=?"));
        params.push(text(representation));
    }
    (
        if clauses.is_empty() {
            "1".into()
        } else {
            clauses.join(" AND ")
        },
        params,
    )
}

/// Range predicate for a section prefix of the scan's start origin.
pub fn section(value: Option<&str>, start: Option<&str>) -> Result<(String, Vec<Sql>), StoreError> {
    let Some(value) = value else {
        return Ok(("1".into(), Vec::new()));
    };
    if value.chars().count() > 8192
        || !value.starts_with('/')
        || value.contains(['?', '#', '\\', '\0'])
    {
        return Err(scan_err(
            "section must be an absolute URL path prefix without query or fragment",
        ));
    }
    let start = start.filter(|s| !s.is_empty()).ok_or_else(|| {
        StoreError::LinksError(LinksError {
            message: "section queries require a recorded start URL".into(),
            reason_code: "section_origin_unavailable".into(),
        })
    })?;
    let parts = urlsplit(start).map_err(|e| StoreError::Invalid(e.to_string()))?;
    let prefix = urlunsplit(&parts.scheme, &parts.netloc, value, "", "");
    // A range lookup uses urls.url's unique index; LIKE may scan case-sensitively.
    Ok((
        "u.url>=? AND u.url<?".into(),
        vec![text(prefix.clone()), text(format!("{prefix}\u{10ffff}"))],
    ))
}

/// Section bucket of a URL: its first folder below `section`.
pub fn cluster(url: &str, section: Option<&str>) -> String {
    let Ok(parts) = urlsplit(url) else {
        return url.to_string();
    };
    let path = if parts.path.is_empty() {
        "/"
    } else {
        parts.path.as_str()
    };
    let mut base = section.filter(|s| !s.is_empty()).unwrap_or("/").to_string();
    if !base.ends_with('/') {
        base.push('/');
    }
    let remainder = match path.strip_prefix(base.as_str()) {
        Some(rest) => rest,
        None => path.trim_start_matches('/'),
    };
    let folder = remainder.split_once('/').map_or("", |(first, _)| first);
    let path = if folder.is_empty() {
        base
    } else {
        format!("{base}{folder}/")
    };
    urlunsplit(&parts.scheme, &parts.netloc, &path, "", "")
}

/// Resolve an occurrence to one retained page using indexed URL lookups.
///
/// Fragment stripping is SQL-local; only an absent exact page needs the root slash alias. The
/// original destination remains an occurrence observation.
/// The schema name defaults to `main`.
pub fn target_join(schema: &str) -> String {
    let base =
        "CASE WHEN instr(d.url,'#')>0 THEN substr(d.url,1,instr(d.url,'#')-1) ELSE d.url END";
    let lookup = |url: &str| {
        format!(
            "SELECT resolved.url_id FROM {schema}.urls resolved CROSS JOIN {schema}.pages present \
             ON present.url_id=resolved.url_id WHERE resolved.url={url}"
        )
    };
    format!(
        "JOIN {schema}.pages target ON target.url_id=COALESCE(({}),({})) ",
        lookup(base),
        lookup(&format!("graph_root_alias({base})"))
    )
}

/// The alias spelling of a root URL (the other of `https://h` and `https://h/`).
pub fn root_alias(url: &str) -> String {
    let key = canonical_key(url);
    if key != url {
        key
    } else {
        spellings(&key).1
    }
}

fn node(
    con: &Connection,
    row: &Row,
    inlink_cap: i64,
    robots_of: RobotsFn<'_>,
) -> Result<Row, StoreError> {
    let url = row["url"].as_str().unwrap_or("");
    let variants = ids(con, &canonical_key(url), true)?;
    let shown: Vec<i64> = variants
        .iter()
        .copied()
        .take(FRAGMENT_VARIANTS as usize)
        .collect();
    let count = fetch(
        con,
        &format!(
            "SELECT COUNT(*) AS n FROM (SELECT 1 FROM json_each(?) variants \
             CROSS JOIN links l INDEXED BY links_destination_source_position \
             ON l.destination_url_id=variants.value JOIN urls d ON d.url_id=l.destination_url_id \
             {}WHERE target.url_id=? LIMIT ?)",
            target_join("main")
        ),
        &[
            text(serde_json::to_string(&shown)?),
            Sql::Integer(row["url_id"].as_i64().unwrap_or(0)),
            Sql::Integer(inlink_cap + 1),
        ],
    )?[0]["n"]
        .as_i64()
        .unwrap_or(0);
    let robots = fetch(
        con,
        "SELECT payload_json AS p FROM context_items WHERE kind='robots_blocked_url' AND item_key=?",
        &[text(format!("url:{}", row["url_id"].as_i64().unwrap_or(0)))],
    )?;
    let robots: Option<Value> = match robots.first() {
        Some(r) => Some(serde_json::from_str(r["p"].as_str().unwrap_or("null"))?),
        None => None,
    };
    let status = row["status_code"].as_i64();
    let norm = |u: &str| norm_url(Some(u)).map_err(|e| StoreError::Invalid(e.to_string()));
    let reason = match status {
        None => "HTTP status unmeasured".to_string(),
        Some(s) if !(200..300).contains(&s) => format!("HTTP {s}"),
        Some(_) => {
            let directives = robots_of(row["meta_robots"].as_str(), row["x_robots"].as_str());
            let canonical = row["canonical"].as_str().filter(|c| !c.is_empty());
            if directives.iter().any(|d| d == "noindex") {
                "noindex directive".into()
            } else if canonical.is_some_and(|c| {
                let joined = urljoin(url, c).unwrap_or_else(|_| c.to_string());
                norm(&joined).ok() != norm(url).ok()
            }) {
                "canonicalized to another URL".into()
            } else if robots.is_some() {
                "blocked by recorded robots policy".into()
            } else {
                String::new()
            }
        }
    };
    let pick = |key: &str| {
        robots
            .as_ref()
            .and_then(|r| r.get(key))
            .cloned()
            .unwrap_or(Value::Null)
    };
    let mut out = Map::new();
    out.insert("id".into(), row["url_id"].clone());
    out.insert("url".into(), row["url"].clone());
    out.insert("status_code".into(), row["status_code"].clone());
    out.insert("depth".into(), row["crawl_depth"].clone());
    out.insert("indexable".into(), json!(reason.is_empty()));
    out.insert(
        "indexability_state".into(),
        json!(if status.is_none() {
            "unmeasured"
        } else if !reason.is_empty() {
            "non_indexable"
        } else {
            "indexable_candidate"
        }),
    );
    out.insert("indexability_reason".into(), json!(reason));
    out.insert("robots_blocked".into(), json!(robots.is_some()));
    out.insert("robots_policy".into(), pick("policy"));
    out.insert("robots_token".into(), pick("token"));
    out.insert("word_count".into(), row["word_count"].clone());
    out.insert("inlinks".into(), json!(count.min(inlink_cap)));
    out.insert(
        "inlinks_capped".into(),
        json!(count > inlink_cap || variants.len() as i64 > FRAGMENT_VARIANTS),
    );
    out.insert("weight".into(), Value::Null);
    out.insert("weight_state".into(), json!("not_measured"));
    Ok(out)
}

fn edge(row: &Row) -> Result<Row, StoreError> {
    let position = row["position"].as_str().unwrap_or("");
    let mut out = Map::new();
    out.insert("id".into(), row["link_id"].clone());
    out.insert("source".into(), row["source_url_id"].clone());
    out.insert("target".into(), row["destination_page_id"].clone());
    out.insert(
        "destination_url_id".into(),
        row["destination_url_id"].clone(),
    );
    out.insert("source_url".into(), row["source_url"].clone());
    out.insert("target_url".into(), row["target_url"].clone());
    out.insert("anchor".into(), row["anchor"].clone());
    out.insert(
        "nofollow".into(),
        json!(row["nofollow"].as_i64().unwrap_or(0) != 0),
    );
    out.insert(
        "rel".into(),
        serde_json::from_str(row["rel_json"].as_str().unwrap_or("[]"))?,
    );
    out.insert(
        "position".into(),
        if position.is_empty() {
            Value::Null
        } else {
            json!(position)
        },
    );
    out.insert(
        "position_status".into(),
        json!(if position.is_empty() {
            "not_classified"
        } else {
            "classified"
        }),
    );
    out.insert(
        "representation".into(),
        row["evidence_representation"].clone(),
    );
    Ok(out)
}

/// Orphans and weakly connected components of the selected induced graph.
fn stats(page_ids: &[i64], edges: &[Row]) -> Value {
    let mut parent: BTreeMap<i64, i64> = page_ids.iter().map(|i| (*i, *i)).collect();
    fn find(parent: &mut BTreeMap<i64, i64>, mut node: i64) -> i64 {
        while parent[&node] != node {
            let grand = parent[&parent[&node]];
            parent.insert(node, grand);
            node = grand;
        }
        node
    }
    let mut incoming = BTreeSet::new();
    for edge in edges {
        let source = edge["source_url_id"].as_i64().unwrap_or(0);
        let target = edge["destination_page_id"].as_i64().unwrap_or(0);
        if source != target {
            incoming.insert(target);
        }
        parent.entry(source).or_insert(source);
        parent.entry(target).or_insert(target);
        let (rs, rt) = (find(&mut parent, source), find(&mut parent, target));
        parent.insert(rs, rt);
    }
    let all: BTreeSet<i64> = page_ids.iter().copied().collect();
    let orphans = all.difference(&incoming).count();
    let roots: BTreeSet<i64> = page_ids.iter().map(|n| find(&mut parent, *n)).collect();
    json!({
        "state": "measured",
        "scope": "selected retained-page induced graph",
        "orphans": orphans,
        "components": roots.len(),
        "component_kind": "weakly_connected",
        "global_reachability": "not_measured",
    })
}

/// Page ids within `depth` hops of `key`: `(sorted ids, capped, examined edges)`.
pub fn around(
    con: &Connection,
    key: &str,
    depth: i64,
    node_budget: i64,
    edge_budget: i64,
    link_where: &str,
    link_params: &[Sql],
    section_where: &str,
    section_params: &[Sql],
) -> Result<(Vec<i64>, bool, i64), StoreError> {
    checked_url(key, "around")?;
    let key = defrag(key).to_string();
    let seed = fetch(
        con,
        "SELECT p.url_id AS id FROM urls u CROSS JOIN pages p ON p.url_id=u.url_id \
         WHERE u.url IN (?,?) ORDER BY u.url=? DESC LIMIT 1",
        &[text(key.clone()), text(root_alias(&key)), text(key.clone())],
    )?;
    let Some(seed) = seed.first().and_then(|r| r["id"].as_i64()) else {
        return Err(StoreError::LinksError(LinksError {
            message: "around URL is not a retained page".into(),
            reason_code: "url_not_found".into(),
        }));
    };
    let mut seen: BTreeSet<i64> = BTreeSet::from([seed]);
    let mut queue: VecDeque<(i64, i64)> = VecDeque::from([(seed, 0)]);
    let mut examined = 0i64;
    let (mut capped, mut variant_capped) = (false, false);
    while let Some((node, level)) = queue.pop_front() {
        if level >= depth {
            continue;
        }
        let node_url = fetch(
            con,
            "SELECT url AS u FROM urls WHERE url_id=?",
            &[Sql::Integer(node)],
        )?
        .first()
        .and_then(|r| r["u"].as_str().map(String::from))
        .unwrap_or_default();
        let variants = ids(con, &canonical_key(&node_url), true)?;
        variant_capped |= variants.len() as i64 > FRAGMENT_VARIANTS;
        let shown: Vec<i64> = variants
            .iter()
            .copied()
            .take(FRAGMENT_VARIANTS as usize)
            .collect();
        // Separate indexed directions avoid SQLite choosing a full-table OR scan.
        let join = target_join("main");
        let queries: [(String, Vec<Sql>); 2] = [
            (
                format!(
                    "SELECT target.url_id AS t FROM links l JOIN urls d ON d.url_id=l.destination_url_id \
                     {join}JOIN urls u ON u.url_id=target.url_id \
                     WHERE l.source_url_id=? AND {link_where} AND {section_where}"
                ),
                [vec![Sql::Integer(node)], link_params.to_vec(), section_params.to_vec()].concat(),
            ),
            (
                format!(
                    "SELECT l.source_url_id AS t FROM json_each(?) variants CROSS JOIN links l \
                     INDEXED BY links_destination_source_position ON l.destination_url_id=variants.value \
                     JOIN urls d ON d.url_id=l.destination_url_id \
                     {join}JOIN pages p ON p.url_id=l.source_url_id JOIN urls u ON u.url_id=p.url_id \
                     WHERE target.url_id=? AND {link_where} AND {section_where}"
                ),
                [
                    vec![text(serde_json::to_string(&shown)?), Sql::Integer(node)],
                    link_params.to_vec(),
                    section_params.to_vec(),
                ]
                .concat(),
            ),
        ];
        for (sql, mut params) in queries {
            params.push(Sql::Integer(edge_budget - examined + 1));
            for row in fetch(con, &format!("{sql} ORDER BY l.link_id LIMIT ?"), &params)? {
                examined += 1;
                if examined > edge_budget {
                    capped = true;
                    break;
                }
                let target = row["t"].as_i64().unwrap_or(0);
                if !seen.contains(&target) {
                    if seen.len() as i64 >= node_budget {
                        capped = true;
                        break;
                    }
                    seen.insert(target);
                    queue.push_back((target, level + 1));
                }
            }
            if capped {
                break;
            }
        }
        if capped {
            break;
        }
    }
    Ok((
        seen.into_iter().collect(),
        capped || variant_capped,
        examined,
    ))
}

/// Paging window and filters shared by the section aggregation.
pub struct ClusterQuery<'a> {
    /// Section predicate SQL.
    pub where_: &'a str,
    /// Section predicate parameters.
    pub params: &'a [Sql],
    /// Link predicate SQL.
    pub link_where: &'a str,
    /// Link predicate parameters.
    pub link_params: &'a [Sql],
    /// Section prefix, when filtered.
    pub section: Option<&'a str>,
    /// Node page offset.
    pub node_offset: i64,
    /// Edge page offset.
    pub edge_offset: i64,
    /// Node page size.
    pub node_limit: i64,
    /// Edge page size.
    pub edge_limit: i64,
}

/// Fill `result` with section nodes and section-to-section edge counts.
pub fn clusters(
    con: &Connection,
    result: &mut Row,
    q: &ClusterQuery<'_>,
) -> Result<(), StoreError> {
    let owned_section = q.section.map(String::from);
    con.create_scalar_function(
        "graph_section",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        move |ctx| {
            let url: String = ctx.get(0)?;
            Ok(cluster(&url, owned_section.as_deref()))
        },
    )?;
    let mut node_params = q.params.to_vec();
    node_params.extend([
        Sql::Integer(AGGREGATE_SCAN_CAP + 1),
        Sql::Integer(q.node_limit + 1),
        Sql::Integer(q.node_offset),
    ]);
    let nodes = fetch(
        con,
        &format!(
            "SELECT graph_section(url) AS section,COUNT(*) AS pages FROM \
             (SELECT u.url FROM urls u CROSS JOIN pages p ON p.url_id=u.url_id WHERE {} LIMIT ?) \
             GROUP BY section ORDER BY section LIMIT ? OFFSET ?",
            q.where_
        ),
        &node_params,
    )?;
    let shown: Vec<Value> = nodes
        .iter()
        .take(q.node_limit as usize)
        .map(|row| {
            let name = row["section"].as_str().unwrap_or("");
            let path = urlsplit(name).map(|p| p.path).unwrap_or_default();
            json!({
                "id": name, "url": name, "kind": "section", "pages": row["pages"],
                "expand_section": path,
            })
        })
        .collect();
    result.insert("nodes".into(), Value::Array(shown));
    result.insert(
        "node_has_more".into(),
        json!(nodes.len() as i64 > q.node_limit),
    );
    result.insert("aggregate".into(), json!("sections"));
    let pages_sum: i64 = nodes.iter().map(|r| r["pages"].as_i64().unwrap_or(0)).sum();
    let mut aggregation = Map::new();
    aggregation.insert("state".into(), json!("bounded_sample"));
    aggregation.insert("page_scan_cap".into(), json!(AGGREGATE_SCAN_CAP));
    aggregation.insert("edge_scan_cap".into(), json!(AGGREGATE_SCAN_CAP));
    aggregation.insert(
        "examined_pages".into(),
        json!(pages_sum.min(AGGREGATE_SCAN_CAP)),
    );
    aggregation.insert(
        "examined_pages_state".into(),
        json!(if q.node_offset > 0 || nodes.len() as i64 > q.node_limit {
            "lower_bound"
        } else {
            "bounded_input"
        }),
    );
    aggregation.insert(
        "reason".into(),
        json!("section totals describe the bounded SQL input, not the whole scan"),
    );
    let range = if q.params.is_empty() {
        "1 "
    } else {
        "t.url>=? AND t.url<? "
    };
    let mut edge_params: Vec<Sql> = q.params.to_vec();
    edge_params.extend(q.link_params.iter().cloned());
    edge_params.extend(q.params.iter().cloned());
    edge_params.extend([
        Sql::Integer(AGGREGATE_SCAN_CAP + 1),
        Sql::Integer(q.edge_limit + 1),
        Sql::Integer(q.edge_offset),
    ]);
    let rows = fetch(
        con,
        &format!(
            "SELECT graph_section(source_url) AS s,graph_section(target_url) AS t,COUNT(*) AS n FROM \
             (SELECT u.url AS source_url,t.url AS target_url FROM links l \
             JOIN urls u ON u.url_id=l.source_url_id JOIN urls d ON d.url_id=l.destination_url_id \
             JOIN pages p ON p.url_id=l.source_url_id {}JOIN urls t ON t.url_id=target.url_id \
             WHERE {} AND {} AND {range}LIMIT ?) GROUP BY graph_section(source_url),graph_section(target_url) \
             ORDER BY 1,2 LIMIT ? OFFSET ?",
            target_join("main"),
            q.where_,
            q.link_where
        ),
        &edge_params,
    )?;
    let edges: Vec<Value> = rows
        .iter()
        .take(q.edge_limit as usize)
        .map(|r| json!({"source": r["s"], "target": r["t"], "occurrences": r["n"]}))
        .collect();
    result.insert("edges".into(), Value::Array(edges));
    let occurrences: i64 = rows.iter().map(|r| r["n"].as_i64().unwrap_or(0)).sum();
    aggregation.insert(
        "examined_edges".into(),
        json!(occurrences.min(AGGREGATE_SCAN_CAP)),
    );
    aggregation.insert(
        "examined_edges_state".into(),
        json!(if q.edge_offset > 0 || rows.len() as i64 > q.edge_limit {
            "lower_bound"
        } else {
            "bounded_input"
        }),
    );
    result.insert("aggregation".into(), Value::Object(aggregation));
    result.insert(
        "edge_has_more".into(),
        json!(rows.len() as i64 > q.edge_limit),
    );
    result.insert("state".into(), json!("partial"));
    result.insert("reason_code".into(), json!("section_aggregation_bounded"));
    Ok(())
}

/// Arguments of [`scan_graph`]; defaults come from [`GraphQuery::new`].
#[derive(Debug, Clone)]
pub struct GraphQuery<'a> {
    /// Position filter (comma text or a list).
    pub positions: Option<Value>,
    /// Section path prefix.
    pub section: Option<&'a str>,
    /// Center the graph on this URL.
    pub around: Option<&'a str>,
    /// Hops around the center.
    pub depth: i64,
    /// `none` or `sections`.
    pub aggregate: &'a str,
    /// Representation filter.
    pub representation: &'a str,
    /// Node page offset.
    pub node_offset: i64,
    /// Edge page offset.
    pub edge_offset: i64,
    /// Node page size.
    pub node_limit: i64,
    /// Edge page size.
    pub edge_limit: i64,
    /// Node budget.
    pub max_nodes: i64,
    /// Edge budget.
    pub max_edges: i64,
    /// Another scan to compare edges with.
    pub compare_to: Option<&'a Path>,
    /// Time budget in seconds.
    pub timeout_seconds: f64,
}

impl Default for GraphQuery<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphQuery<'_> {
    /// Query with no filters, the default page sizes and budgets.
    pub fn new() -> Self {
        GraphQuery {
            positions: None,
            section: None,
            around: None,
            depth: 2,
            aggregate: "none",
            representation: "all",
            node_offset: 0,
            edge_offset: 0,
            node_limit: 1000,
            edge_limit: 2000,
            max_nodes: MAX_NODES,
            max_edges: MAX_EDGES,
            compare_to: None,
            timeout_seconds: 1.0,
        }
    }
}

const NODE_COLUMNS: &str =
    "p.url_id,p.status_code,p.crawl_depth,p.meta_robots,p.x_robots,p.canonical,p.word_count,u.url";

fn is_interrupt(error: &StoreError) -> bool {
    matches!(error, StoreError::Sqlite(e) if e.to_string().to_lowercase().contains("interrupted"))
}

/// Return independent node/edge chunks without loading a whole scan into memory.
pub fn scan_graph(scan_path: &Path, q: &GraphQuery<'_>) -> Result<Row, StoreError> {
    scan_graph_with(scan_path, q, &parser_robots)
}

/// [`scan_graph`] over an injected robots-directive tokenizer.
pub fn scan_graph_with(
    scan_path: &Path,
    q: &GraphQuery<'_>,
    robots_of: RobotsFn<'_>,
) -> Result<Row, StoreError> {
    let positions = positions(q.positions.as_ref())?;
    let representation = checked_representation(q.representation)?;
    if q.node_offset < 0 || q.edge_offset < 0 || q.depth < 0 || q.depth > 100 {
        return Err(scan_err(
            "graph offsets must be nonnegative and depth must be 0..100",
        ));
    }
    if !(1..=MAX_NODES).contains(&q.max_nodes)
        || !(1..=MAX_EDGES).contains(&q.max_edges)
        || !(1..=q.max_nodes).contains(&q.node_limit)
        || !(1..=q.max_edges).contains(&q.edge_limit)
        || !(q.timeout_seconds > 0.0 && q.timeout_seconds <= 30.0)
    {
        return Err(scan_err(
            "graph budgets require nodes <=5000, edges <=20000, page limits within budgets, timeout 0..30s",
        ));
    }
    if !matches!(q.aggregate, "none" | "sections")
        || (q.aggregate == "sections" && q.around.is_some())
    {
        return Err(scan_err(
            "aggregate must be none or sections; around cannot be combined with sections",
        ));
    }
    let con = open_light(scan_path)?;
    con.create_scalar_function(
        "graph_root_alias",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| Ok(root_alias(&ctx.get::<String>(0)?)),
    )?;
    con.create_scalar_function(
        "graph_document_key",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| Ok(defrag(&ctx.get::<String>(0)?).to_string()),
    )?;
    let (head, _, _, start) = header_parts(&con)?;
    let (where_, params) = section(q.section, start.as_deref())?;
    let (link_where, link_params) = link_filter(&positions, &representation, "l");
    let deadline = Instant::now() + Duration::from_secs_f64(q.timeout_seconds);
    con.progress_handler(1000, Some(move || Instant::now() >= deadline));
    let mut result = Map::new();
    result.insert("ok".into(), json!(true));
    result.insert("format".into(), json!(FORMAT));
    for (k, v) in &head {
        result.insert(k.clone(), v.clone());
    }
    result.insert("validation".into(), json!("light"));
    result.insert("state".into(), json!("complete"));
    result.insert("reason_code".into(), Value::Null);
    result.insert(
        "filters".into(),
        json!({
            "positions": positions, "section": q.section, "around": q.around,
            "depth": q.depth, "representation": q.representation,
        }),
    );
    result.insert("aggregate".into(), json!(q.aggregate));
    result.insert("nodes".into(), json!([]));
    result.insert("edges".into(), json!([]));
    for (k, v) in [
        ("node_offset", q.node_offset),
        ("edge_offset", q.edge_offset),
        ("node_limit", q.node_limit),
        ("edge_limit", q.edge_limit),
    ] {
        result.insert(k.into(), json!(v));
    }
    result.insert("node_has_more".into(), json!(false));
    result.insert("edge_has_more".into(), json!(false));
    result.insert("max_nodes".into(), json!(q.max_nodes));
    result.insert("max_edges".into(), json!(q.max_edges));
    result.insert("total".into(), Value::Null);
    result.insert("total_unit".into(), json!("retained_pages"));
    result.insert("total_capped".into(), json!(false));
    result.insert("filtered_total".into(), Value::Null);
    result.insert("filtered_total_capped".into(), json!(false));
    result.insert("position_coverage".into(), json!({"state": "not_measured"}));
    result.insert(
        "statistics".into(),
        json!({
            "state": "not_measured",
            "reason": "complete selected graph has not been measured",
            "orphans": null, "components": null,
        }),
    );
    if head["coverage"]["state"] == json!("unavailable") {
        result.insert("state".into(), json!("unavailable"));
        result.insert("reason_code".into(), json!("links_not_retained"));
        return Ok(result);
    }
    match build(
        &con,
        q,
        robots_of,
        &positions,
        &representation,
        &where_,
        &params,
        &link_where,
        &link_params,
        start.as_deref(),
        &mut result,
    ) {
        Err(error) if is_interrupt(&error) => {
            result.insert("state".into(), json!("partial"));
            result.insert("reason_code".into(), json!("time_budget_exhausted"));
        }
        other => other?,
    }
    let more = |flag: &str| result[flag] == json!(true);
    let next = |flag: &str, offset: i64, key: &str| {
        if more(flag) {
            json!(offset + result[key].as_array().map_or(0, Vec::len) as i64)
        } else {
            Value::Null
        }
    };
    let next_nodes = next("node_has_more", q.node_offset, "nodes");
    let next_edges = next("edge_has_more", q.edge_offset, "edges");
    result.insert("next_node_offset".into(), next_nodes);
    result.insert("next_edge_offset".into(), next_edges);
    let state_of = |value: &Value, capped: &Value| {
        if value.is_null() {
            "unknown"
        } else if capped == &json!(true) {
            "capped"
        } else {
            "exact"
        }
    };
    let total_state = state_of(&result["total"], &result["total_capped"]);
    let filtered_state = state_of(&result["filtered_total"], &result["filtered_total_capped"]);
    result.insert("total_state".into(), json!(total_state));
    result.insert("filtered_total_state".into(), json!(filtered_state));
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn build(
    con: &Connection,
    q: &GraphQuery<'_>,
    robots_of: RobotsFn<'_>,
    positions: &[String],
    representation: &str,
    where_: &str,
    params: &[Sql],
    link_where: &str,
    link_params: &[Sql],
    start: Option<&str>,
    result: &mut Row,
) -> Result<(), StoreError> {
    let mut local_capped = false;
    let node_window_offset = if q.section.is_some() && q.around.is_none() {
        (q.node_offset / q.max_nodes) * q.max_nodes
    } else {
        0
    };
    let node_window_index = q.node_offset - node_window_offset;
    result.insert("node_window_offset".into(), json!(node_window_offset));
    let rows: Vec<Row> = if let Some(center) = q.around {
        let (page_ids, capped, examined) = around(
            con,
            center,
            q.depth,
            q.max_nodes,
            q.max_edges,
            link_where,
            link_params,
            where_,
            params,
        )?;
        local_capped = capped;
        result.insert("examined_edges".into(), json!(examined));
        fetch(
            con,
            &format!(
                "SELECT {NODE_COLUMNS} FROM pages p JOIN urls u USING(url_id) \
                 WHERE p.url_id IN ({}) ORDER BY p.url_id",
                vec!["?"; page_ids.len()].join(",")
            ),
            &page_ids
                .iter()
                .map(|i| Sql::Integer(*i))
                .collect::<Vec<_>>(),
        )?
    } else {
        let mut args = params.to_vec();
        args.extend([
            Sql::Integer(q.max_nodes + 1),
            Sql::Integer(node_window_offset),
        ]);
        fetch(
            con,
            &format!(
                "SELECT {NODE_COLUMNS} FROM urls u CROSS JOIN pages p ON p.url_id=u.url_id \
                 WHERE {where_} ORDER BY u.url LIMIT ? OFFSET ?"
            ),
            &args,
        )?
    };
    result.insert(
        "filtered_total".into(),
        json!((rows.len() as i64).min(q.max_nodes)),
    );
    result.insert(
        "filtered_total_capped".into(),
        json!(rows.len() as i64 > q.max_nodes || local_capped),
    );
    if q.aggregate == "sections"
        || (rows.len() as i64 > q.max_nodes && q.around.is_none() && q.section.is_none())
    {
        clusters(
            con,
            result,
            &ClusterQuery {
                where_,
                params,
                link_where,
                link_params,
                section: q.section,
                node_offset: q.node_offset,
                edge_offset: q.edge_offset,
                node_limit: q.node_limit,
                edge_limit: q.edge_limit,
            },
        )?;
    } else {
        let kept: Vec<&Row> = rows.iter().take(q.max_nodes as usize).collect();
        let page_ids: Vec<i64> = kept
            .iter()
            .map(|r| r["url_id"].as_i64().unwrap_or(0))
            .collect();
        let window_end = ((node_window_index + q.node_limit).min(q.max_nodes)) as usize;
        let window_start = (node_window_index as usize).min(rows.len());
        let mut nodes = Vec::new();
        for row in rows[window_start..window_end.min(rows.len()).max(window_start)].iter() {
            nodes.push(Value::Object(node(con, row, q.max_edges, robots_of)?));
        }
        result.insert(
            "node_has_more".into(),
            json!(node_window_index + (nodes.len() as i64) < rows.len() as i64),
        );
        result.insert("nodes".into(), Value::Array(nodes));
        let mut edges: Vec<Row> = Vec::new();
        let edge_select = format!(
            "SELECT l.*,target.url_id AS destination_page_id,s.url AS source_url,d.url AS target_url FROM \
             (SELECT value AS url_id FROM json_each(?)) picked \
             CROSS JOIN links l ON l.source_url_id=picked.url_id \
             JOIN urls s ON s.url_id=l.source_url_id JOIN urls d ON d.url_id=l.destination_url_id \
             {}WHERE target.url_id IN ({}) AND {link_where} \
             ORDER BY l.source_url_id,l.evidence_representation,l.ordinal,l.link_id ",
            target_join("main"),
            vec!["?"; page_ids.len()].join(",")
        );
        let mut edge_params: Vec<Sql> = vec![text(serde_json::to_string(&page_ids)?)];
        edge_params.extend(page_ids.iter().map(|i| Sql::Integer(*i)));
        edge_params.extend(link_params.iter().cloned());
        if !page_ids.is_empty() {
            let mut args = edge_params.clone();
            args.push(Sql::Integer(q.max_edges + 1));
            edges = fetch(con, &format!("{edge_select}LIMIT ?"), &args)?;
        }
        result.insert(
            "edge_total".into(),
            json!((edges.len() as i64).min(q.max_edges)),
        );
        result.insert(
            "edge_total_capped".into(),
            json!(edges.len() as i64 > q.max_edges),
        );
        if !page_ids.is_empty() && edges.len() as i64 > q.max_edges {
            let mut args = edge_params.clone();
            args.extend([Sql::Integer(q.edge_limit + 1), Sql::Integer(q.edge_offset)]);
            let page = fetch(con, &format!("{edge_select}LIMIT ? OFFSET ?"), &args)?;
            let shown: Result<Vec<Value>, StoreError> = page
                .iter()
                .take(q.edge_limit as usize)
                .map(|r| edge(r).map(Value::Object))
                .collect();
            result.insert("edges".into(), Value::Array(shown?));
            result.insert(
                "edge_has_more".into(),
                json!(page.len() as i64 > q.edge_limit),
            );
        } else {
            let start_at = (q.edge_offset as usize).min(edges.len());
            let shown: Result<Vec<Value>, StoreError> = edges[start_at..]
                .iter()
                .take(q.edge_limit as usize)
                .map(|r| edge(r).map(Value::Object))
                .collect();
            let shown = shown?;
            result.insert(
                "edge_has_more".into(),
                json!((q.edge_offset + shown.len() as i64) < edges.len() as i64),
            );
            result.insert("edges".into(), Value::Array(shown));
        }
        if local_capped || edges.len() as i64 > q.max_edges || rows.len() as i64 > q.max_nodes {
            result.insert("state".into(), json!("partial"));
            result.insert("reason_code".into(), json!("graph_budget_exhausted"));
        } else {
            result.insert("statistics".into(), stats(&page_ids, &edges));
        }
    }
    // Counts run last; the first page remains useful if its count budget expires.
    let count = fetch(
        con,
        "SELECT COUNT(*) AS n FROM (SELECT 1 FROM pages LIMIT ?)",
        &[Sql::Integer(q.max_nodes + 1)],
    )?[0]["n"]
        .as_i64()
        .unwrap_or(0);
    result.insert("total".into(), json!(count.min(q.max_nodes)));
    result.insert("total_capped".into(), json!(count > q.max_nodes));
    let coverage = position_summary(con, 100_000)?;
    result.insert("position_coverage".into(), Value::Object(coverage.clone()));
    if !positions.is_empty() && !positions.iter().any(|p| p == "not_classified") {
        let truthy = |k: &str| {
            coverage
                .get(k)
                .is_some_and(|v| v.as_i64().is_some_and(|n| n != 0) || v == &json!(true))
        };
        if truthy("not_classified") {
            let reason = if !truthy("classified") {
                "position_not_classified"
            } else {
                "positions_partially_classified"
            };
            let partial = truthy("classified") || truthy("capped");
            result.insert(
                "state".into(),
                json!(if partial { "partial" } else { "unavailable" }),
            );
            result.insert("reason_code".into(), json!(reason));
            result.insert(
                "statistics".into(),
                json!({"state": "not_measured", "reason": reason, "orphans": null, "components": null}),
            );
        }
    }
    if let Some(other) = q.compare_to {
        let shown_edges = result["edges"].as_array().map_or(0, Vec::len) as i64;
        let diff_limit = q.edge_limit.min((q.max_edges - shown_edges) / 2);
        let comparison = if diff_limit < 1 {
            json!({"state": "not_measured", "reason_code": "edge_budget_exhausted"})
        } else {
            Value::Object(diff(
                con,
                other,
                positions,
                representation,
                q.section,
                start,
                q.edge_offset,
                diff_limit,
                q.max_edges,
            )?)
        };
        result.insert("comparison".into(), comparison);
    }
    Ok(())
}

/// Compare this scan's edges with another scan by SQL `EXCEPT`.
#[allow(clippy::too_many_arguments)]
pub fn diff(
    con: &Connection,
    path: &Path,
    positions: &[String],
    representation: &str,
    section_prefix: Option<&str>,
    start: Option<&str>,
    offset: i64,
    limit: i64,
    cap: i64,
) -> Result<Row, StoreError> {
    let (other_head, other_start) = {
        let other = open_light(path)?;
        let (head, _, _, other_start) = header_parts(&other)?;
        (head, other_start)
    };
    if other_head["coverage"]["state"] == json!("unavailable") {
        return Ok(json!({
            "state": "unavailable", "reason_code": "links_not_retained",
            "scan_uuid": other_head["scan_uuid"],
        })
        .as_object()
        .cloned()
        .unwrap_or_default());
    }
    let resolved = std::fs::canonicalize(path)?;
    // The attach inherits the connection's read-only flags.
    con.execute(
        "ATTACH DATABASE ? AS previous",
        [resolved.to_string_lossy().into_owned()],
    )?;
    let pairs = |schema: &str, origin: Option<&str>| -> Result<(String, Vec<Sql>), StoreError> {
        let (where_, params) = section(section_prefix, origin)?;
        let (link_where, link_params) = link_filter(positions, representation, "l");
        let range = if params.is_empty() {
            "1"
        } else {
            "t.url>=? AND t.url<?"
        };
        let sql = format!(
            "SELECT graph_document_key(u.url) AS source,graph_document_key(t.url) AS target FROM {schema}.links l \
             JOIN {schema}.urls u ON u.url_id=l.source_url_id JOIN {schema}.urls d ON d.url_id=l.destination_url_id \
             JOIN {schema}.pages p ON p.url_id=l.source_url_id {}JOIN {schema}.urls t ON t.url_id=target.url_id \
             WHERE {where_} AND {link_where} AND {range}",
            target_join(schema)
        );
        let args = [params.clone(), link_params, params].concat();
        Ok((sql, args))
    };
    let (current, cp) = pairs("main", start)?;
    let (previous, pp) = pairs("previous", other_start.as_deref())?;
    let mut result = Map::new();
    result.insert("state".into(), json!("complete"));
    result.insert("scan_uuid".into(), other_head["scan_uuid"].clone());
    result.insert(
        "identity".into(),
        json!("source/destination exact retained document URL pair; fragment targets resolved"),
    );
    let page_cap = limit.min(cap) + 1;
    for (label, left, lp, right, rp) in [
        ("added", &current, &cp, &previous, &pp),
        ("removed", &previous, &pp, &current, &cp),
    ] {
        let args = [
            lp.clone(),
            rp.clone(),
            vec![Sql::Integer(page_cap), Sql::Integer(offset)],
        ]
        .concat();
        let rows = fetch(
            con,
            &format!(
                "SELECT source,target FROM ({left} EXCEPT {right}) ORDER BY source,target LIMIT ? OFFSET ?"
            ),
            &args,
        )?;
        let shown: Vec<Value> = rows
            .iter()
            .take(limit as usize)
            .map(|r| json!({"source_url": r["source"], "target_url": r["target"]}))
            .collect();
        let more = rows.len() as i64 > limit;
        result.insert(label.into(), Value::Array(shown));
        result.insert(format!("{label}_has_more"), json!(more));
        result.insert(
            format!("{label}_next_offset"),
            if more {
                json!(offset + limit.min(rows.len() as i64))
            } else {
                Value::Null
            },
        );
    }
    // New orphans are retained current pages with zero observed incoming non-self pairs, minus
    // the previous scan's orphan URLs; SQL EXCEPT owns the comparison.
    let orphans = |schema: &str,
                   pairs_sql: &str,
                   origin: Option<&str>|
     -> Result<(String, Vec<Sql>), StoreError> {
        let (where_, params) = section(section_prefix, origin)?;
        Ok((
            format!(
                "SELECT graph_document_key(u.url) AS url FROM {schema}.urls u CROSS JOIN {schema}.pages p ON p.url_id=u.url_id \
                 WHERE {where_} AND NOT EXISTS(SELECT 1 FROM ({pairs_sql}) e WHERE e.target=graph_document_key(u.url) AND e.source!=e.target)"
            ),
            params,
        ))
    };
    let (now, np) = orphans("main", &current, start)?;
    let (before, bp) = orphans("previous", &previous, other_start.as_deref())?;
    let args = [
        np,
        cp,
        bp,
        pp,
        vec![Sql::Integer(limit + 1), Sql::Integer(offset)],
    ]
    .concat();
    let rows = fetch(
        con,
        &format!("SELECT url FROM ({now} EXCEPT {before}) ORDER BY url LIMIT ? OFFSET ?"),
        &args,
    )?;
    result.insert(
        "new_orphans".into(),
        Value::Array(
            rows.iter()
                .take(limit as usize)
                .map(|r| r["url"].clone())
                .collect(),
        ),
    );
    result.insert(
        "new_orphans_has_more".into(),
        json!(rows.len() as i64 > limit),
    );
    result.insert(
        "scope".into(),
        json!("selected retained-page graph; no global orphan claim"),
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_are_validated_sorted_and_unique() {
        assert!(positions(None).unwrap().is_empty());
        assert_eq!(
            positions(Some(&json!("nav,content,nav"))).unwrap(),
            vec!["content", "nav"]
        );
        assert_eq!(
            positions(Some(&json!(["footer", "header"]))).unwrap(),
            vec!["footer", "header"]
        );
        assert!(positions(Some(&json!("bogus"))).is_err());
        assert!(positions(Some(&json!([]))).is_err());
        assert!(positions(Some(&json!([1]))).is_err());
    }

    #[test]
    fn link_filter_maps_unclassified_to_blank() {
        let (sql, params) =
            link_filter(&["content".into(), "not_classified".into()], "static", "l");
        assert_eq!(sql, "l.position IN (?,?) AND l.evidence_representation=?");
        assert_eq!(params.len(), 3);
        assert_eq!(params[1], Sql::Text(String::new()));
        assert_eq!(link_filter(&[], "all", "l").0, "1");
    }

    #[test]
    fn section_builds_a_range_on_the_start_origin() {
        let (sql, params) = section(Some("/blog/"), Some("https://e.test/start")).unwrap();
        assert_eq!(sql, "u.url>=? AND u.url<?");
        assert_eq!(params[0], Sql::Text("https://e.test/blog/".into()));
        assert_eq!(section(None, None).unwrap().0, "1");
        assert!(section(Some("blog"), Some("https://e.test/")).is_err());
        assert!(section(Some("/a?b"), Some("https://e.test/")).is_err());
        let err = section(Some("/a"), None).unwrap_err();
        assert!(
            matches!(err, StoreError::LinksError(ref e) if e.reason_code == "section_origin_unavailable")
        );
    }

    #[test]
    fn cluster_buckets_by_first_folder() {
        assert_eq!(
            cluster("https://e.test/blog/a/b", None),
            "https://e.test/blog/"
        );
        assert_eq!(cluster("https://e.test/", None), "https://e.test/");
        assert_eq!(
            cluster("https://e.test/blog/x/y", Some("/blog")),
            "https://e.test/blog/x/"
        );
        assert_eq!(
            cluster("https://e.test/other", Some("/blog")),
            "https://e.test/blog/"
        );
    }

    #[test]
    fn stats_count_orphans_and_components() {
        let edge = |s: i64, t: i64| {
            json!({"source_url_id": s, "destination_page_id": t})
                .as_object()
                .cloned()
                .unwrap_or_default()
        };
        let s = stats(&[1, 2, 3, 4], &[edge(1, 2), edge(2, 2), edge(3, 3)]);
        assert_eq!(s["orphans"], json!(3)); // 1, 3 (self-loop only), 4
        assert_eq!(s["components"], json!(3)); // {1,2} {3} {4}
        assert_eq!(s["component_kind"], json!("weakly_connected"));
    }

    #[test]
    fn target_join_names_the_schema() {
        let sql = target_join("previous");
        assert!(sql.contains("previous.pages target"));
        assert!(sql.contains("graph_root_alias(CASE WHEN instr(d.url,'#')>0"));
    }

    #[test]
    fn budgets_are_validated() {
        let q = GraphQuery {
            depth: 101,
            ..GraphQuery::new()
        };
        assert!(scan_graph(Path::new("/x"), &q).is_err());
        let q = GraphQuery {
            node_limit: 6000,
            ..GraphQuery::new()
        };
        assert!(scan_graph(Path::new("/x"), &q).is_err());
        let q = GraphQuery {
            aggregate: "sections",
            around: Some("https://e.test/"),
            ..GraphQuery::new()
        };
        assert!(scan_graph(Path::new("/x"), &q).is_err());
        let q = GraphQuery {
            timeout_seconds: 31.0,
            ..GraphQuery::new()
        };
        assert!(scan_graph(Path::new("/x"), &q).is_err());
    }
}

#[cfg(test)]
mod oracle_tests {
    use super::*;
    use crate::status::oracle_support::*;

    fn tokens(meta: Option<&str>, x: Option<&str>) -> Vec<String> {
        [meta, x]
            .into_iter()
            .flatten()
            .flat_map(|v| v.split([',', ' ']))
            .filter(|t| !t.is_empty())
            .map(str::to_lowercase)
            .collect()
    }

    // Default arguments; the output is compared against a recorded graph.
    #[test]
    fn scan_graph_matches_the_reference_baseline() {
        let mut divergences = Vec::new();
        for site in SITES {
            let q = GraphQuery::new();
            let actual = scan_graph_with(&raw_scan(site), &q, &tokens).unwrap();
            let actual = normalise(&Value::Object(actual));
            let expected = baseline(site, "scan-graph.json");
            if actual != expected {
                let a = serde_json::to_string_pretty(&actual).unwrap();
                let e = serde_json::to_string_pretty(&expected).unwrap();
                let line = a
                    .lines()
                    .zip(e.lines())
                    .position(|(x, y)| x != y)
                    .unwrap_or(0);
                divergences.push(format!(
                    "{site}: first difference at line {line}:\n  expected {}\n  actual   {}",
                    e.lines().nth(line).unwrap_or(""),
                    a.lines().nth(line).unwrap_or("")
                ));
            }
        }
        assert!(divergences.is_empty(), "{}", divergences.join("\n"));
    }
}
