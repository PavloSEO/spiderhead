//! Preset views and facet groups for scan-url-query.
//!
//!
//! Named audit-style filter sets (`presets`) and the fixed facet SQL expressions
//! (`FACET_GROUPS`), plus the one-pass facet counter and its measured-evidence checks.

use std::sync::OnceLock;

use indexmap::IndexMap;
use rusqlite::types::Value as SqlValue;
use rusqlite::{params_from_iter, Connection};
use serde_json::{json, Value};

use crate::url_query::{is_interrupt, Budget};
use crate::url_query_schema::QueryError;

/// A preset: its label and the filters it stands for.
pub type Preset = (&'static str, Vec<Value>);

/// Audit-style named views as filter sets over the column allow-list, in definition order.
///
/// A preset is a shorthand: it is AND-combined with the caller's filters and reported back.
pub fn presets() -> &'static IndexMap<&'static str, Preset> {
    static MAP: OnceLock<IndexMap<&'static str, Preset>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m: IndexMap<&'static str, Preset> = IndexMap::new();
        let mut put = |id: &'static str,
                       label: &'static str,
                       column: &str,
                       op: &str,
                       value: Option<Value>| {
            let mut f = json!({"column": column, "op": op});
            if let Some(v) = value {
                f["value"] = v;
            }
            m.insert(id, (label, vec![f]));
        };
        put(
            "status_3xx",
            "Response codes: 3xx",
            "status_class",
            "eq",
            Some(json!("3xx")),
        );
        put(
            "status_4xx",
            "Response codes: 4xx",
            "status_class",
            "eq",
            Some(json!("4xx")),
        );
        put(
            "status_5xx",
            "Response codes: 5xx",
            "status_class",
            "eq",
            Some(json!("5xx")),
        );
        put(
            "no_response",
            "Response codes: none",
            "status_class",
            "eq",
            Some(json!("none")),
        );
        put("title_missing", "Titles: missing", "title", "empty", None);
        put(
            "title_over_60",
            "Titles: over 60 characters",
            "title_length",
            "gt",
            Some(json!(60)),
        );
        put(
            "meta_description_missing",
            "Meta description: missing",
            "meta_description",
            "empty",
            None,
        );
        put(
            "meta_description_over_160",
            "Meta description: over 160 characters",
            "meta_description_length",
            "gt",
            Some(json!(160)),
        );
        put("h1_missing", "H1: missing", "h1", "empty", None);
        put(
            "h1_multiple",
            "H1: more than one",
            "h1_2",
            "not_empty",
            None,
        );
        put(
            "canonical_missing",
            "Canonicals: missing",
            "canonical",
            "empty",
            None,
        );
        put(
            "images_missing_alt",
            "Images: missing alt attribute",
            "images_missing_alt_attr",
            "gt",
            Some(json!(0)),
        );
        put(
            "noindex_meta",
            "Directives: noindex in meta robots",
            "meta_robots",
            "contains",
            Some(json!("noindex")),
        );
        put(
            "noindex_header",
            "Directives: noindex in X-Robots-Tag",
            "x_robots",
            "contains",
            Some(json!("noindex")),
        );
        put(
            "no_outlinks",
            "Links: no outlinks",
            "outlinks",
            "eq",
            Some(json!(0)),
        );
        put(
            "hreflang",
            "Hreflang: present",
            "has_hreflang",
            "eq",
            Some(json!(true)),
        );
        put(
            "duplicate_titles",
            "Titles: duplicate",
            "title_duplicate",
            "eq",
            Some(json!(true)),
        );
        put(
            "in_sitemap",
            "Sitemap: included",
            "in_sitemap",
            "eq",
            Some(json!(true)),
        );
        put(
            "inlinks",
            "Links: has inlinks",
            "inlinks",
            "gt",
            Some(json!(0)),
        );
        put(
            "robots_blocked",
            "Robots: blocked by robots.txt",
            "robots_blocked",
            "eq",
            Some(json!(true)),
        );
        put(
            "description_duplicate",
            "Descriptions: duplicate",
            "description_duplicate",
            "eq",
            Some(json!(true)),
        );
        put(
            "canonical_non_indexable",
            "Canonicals: target is not indexable",
            "canonical_non_indexable",
            "eq",
            Some(json!(true)),
        );
        m
    })
}

/// Fixed facet SQL expressions in definition order; no caller-provided text reaches the query.
/// Facet group id to its SQL predicate.
pub const FACET_GROUPS: &[(&str, &str)] = &[("all", "1"), ("status_2xx", "p.status_code>=200 AND p.status_code<300"), ("status_3xx", "p.status_code>=300 AND p.status_code<400"), ("status_4xx", "p.status_code>=400 AND p.status_code<500"), ("status_5xx", "p.status_code>=500 AND p.status_code<600"), ("title_missing", "(p.title IS NULL OR p.title='')"), ("canonical_missing", "(p.canonical IS NULL OR p.canonical='')"), ("directives_noindex", "instr(lower(p.meta_robots||','||p.x_robots),'noindex')>0"), ("directives_nofollow", "instr(lower(p.meta_robots||','||p.x_robots),'nofollow')>0"), ("with_outlinks", "p.outlinks>0"), ("internal", "lower(substr(u.url,1,CASE WHEN instr(substr(u.url,instr(u.url,'://')+3),'/')>0 THEN instr(u.url,'://')+1+instr(substr(u.url,instr(u.url,'://')+3),'/') WHEN instr(substr(u.url,instr(u.url,'://')+3),'?')>0 THEN instr(u.url,'://')+1+instr(substr(u.url,instr(u.url,'://')+3),'?') WHEN instr(substr(u.url,instr(u.url,'://')+3),'#')>0 THEN instr(u.url,'://')+1+instr(substr(u.url,instr(u.url,'://')+3),'#') ELSE length(u.url) END))=lower(substr((SELECT start_url FROM scan WHERE singleton=1),1,CASE WHEN instr(substr((SELECT start_url FROM scan WHERE singleton=1),instr((SELECT start_url FROM scan WHERE singleton=1),'://')+3),'/')>0 THEN instr((SELECT start_url FROM scan WHERE singleton=1),'://')+1+instr(substr((SELECT start_url FROM scan WHERE singleton=1),instr((SELECT start_url FROM scan WHERE singleton=1),'://')+3),'/') WHEN instr(substr((SELECT start_url FROM scan WHERE singleton=1),instr((SELECT start_url FROM scan WHERE singleton=1),'://')+3),'?')>0 THEN instr((SELECT start_url FROM scan WHERE singleton=1),'://')+1+instr(substr((SELECT start_url FROM scan WHERE singleton=1),instr((SELECT start_url FROM scan WHERE singleton=1),'://')+3),'?') WHEN instr(substr((SELECT start_url FROM scan WHERE singleton=1),instr((SELECT start_url FROM scan WHERE singleton=1),'://')+3),'#')>0 THEN instr((SELECT start_url FROM scan WHERE singleton=1),'://')+1+instr(substr((SELECT start_url FROM scan WHERE singleton=1),instr((SELECT start_url FROM scan WHERE singleton=1),'://')+3),'#') ELSE length((SELECT start_url FROM scan WHERE singleton=1)) END))"), ("external", "lower(substr(u.url,1,CASE WHEN instr(substr(u.url,instr(u.url,'://')+3),'/')>0 THEN instr(u.url,'://')+1+instr(substr(u.url,instr(u.url,'://')+3),'/') WHEN instr(substr(u.url,instr(u.url,'://')+3),'?')>0 THEN instr(u.url,'://')+1+instr(substr(u.url,instr(u.url,'://')+3),'?') WHEN instr(substr(u.url,instr(u.url,'://')+3),'#')>0 THEN instr(u.url,'://')+1+instr(substr(u.url,instr(u.url,'://')+3),'#') ELSE length(u.url) END))<>lower(substr((SELECT start_url FROM scan WHERE singleton=1),1,CASE WHEN instr(substr((SELECT start_url FROM scan WHERE singleton=1),instr((SELECT start_url FROM scan WHERE singleton=1),'://')+3),'/')>0 THEN instr((SELECT start_url FROM scan WHERE singleton=1),'://')+1+instr(substr((SELECT start_url FROM scan WHERE singleton=1),instr((SELECT start_url FROM scan WHERE singleton=1),'://')+3),'/') WHEN instr(substr((SELECT start_url FROM scan WHERE singleton=1),instr((SELECT start_url FROM scan WHERE singleton=1),'://')+3),'?')>0 THEN instr((SELECT start_url FROM scan WHERE singleton=1),'://')+1+instr(substr((SELECT start_url FROM scan WHERE singleton=1),instr((SELECT start_url FROM scan WHERE singleton=1),'://')+3),'?') WHEN instr(substr((SELECT start_url FROM scan WHERE singleton=1),instr((SELECT start_url FROM scan WHERE singleton=1),'://')+3),'#')>0 THEN instr((SELECT start_url FROM scan WHERE singleton=1),'://')+1+instr(substr((SELECT start_url FROM scan WHERE singleton=1),instr((SELECT start_url FROM scan WHERE singleton=1),'://')+3),'#') ELSE length((SELECT start_url FROM scan WHERE singleton=1)) END))"), ("title_duplicate", "EXISTS (SELECT 1 FROM pages t WHERE t.url_id<>p.url_id AND trim(t.title)<>'' AND lower(t.title)=lower(p.title))"), ("title_long", "length(p.title)>60"), ("title_short", "(p.title<>'' AND length(p.title)<30)"), ("canonical_other", "(p.canonical<>'' AND p.canonical<>u.url)"), ("canonical_self", "(p.canonical<>'' AND p.canonical=u.url)"), ("in_sitemap", "EXISTS (SELECT 1 FROM context_items s WHERE s.kind='sitemap_declared_url' AND CAST(json_extract(s.payload_json,'$.url_id') AS INTEGER)=p.url_id)"), ("with_inlinks", "EXISTS (SELECT 1 FROM links l WHERE l.destination_url_id=p.url_id)"), ("with_resources", "EXISTS (SELECT 1 FROM resource_graph_occurrences r WHERE r.page_url_id=p.url_id AND r.source_document_id=p.document_id)"), ("with_images", "EXISTS (SELECT 1 FROM resource_graph_occurrences r WHERE r.page_url_id=p.url_id AND r.source_document_id=p.document_id AND r.kind='image')"), ("with_scripts", "EXISTS (SELECT 1 FROM resource_graph_occurrences r WHERE r.page_url_id=p.url_id AND r.source_document_id=p.document_id AND r.kind IN ('script','module'))"), ("with_stylesheets", "EXISTS (SELECT 1 FROM resource_graph_occurrences r WHERE r.page_url_id=p.url_id AND r.source_document_id=p.document_id AND r.kind='stylesheet')")];

fn invalid_facet(message: impl Into<String>) -> QueryError {
    QueryError::new("invalid_facet", message)
}

/// The filters a preset stands for.
pub fn preset_filters(preset: &Value) -> Result<Vec<Value>, QueryError> {
    if let Some(p) = preset.as_str() {
        if let Some((_, filters)) = presets().get(p) {
            return Ok(filters.clone());
        }
    }
    let mut ids: Vec<&str> = presets().keys().copied().collect();
    ids.sort_unstable();
    Err(QueryError::new(
        "unknown_preset",
        format!("preset is not one of: {}", ids.join(", ")),
    ))
}

/// Requested facet group ids in order; None or [] means no facets are requested.
pub fn facet_ids(facets: &Value) -> Result<Vec<String>, QueryError> {
    match facets {
        Value::Null => return Ok(Vec::new()),
        Value::String(s) if s == "all" => {
            return Ok(FACET_GROUPS.iter().map(|(k, _)| k.to_string()).collect())
        }
        _ => {}
    }
    let Value::Array(items) = facets else {
        return Err(invalid_facet("facets must be a list of group ids or 'all'"));
    };
    if !items.iter().all(Value::is_string) {
        return Err(invalid_facet("facets must be a list of group ids or 'all'"));
    }
    let ids: Vec<String> = items
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    let unknown: Vec<&str> = ids
        .iter()
        .map(String::as_str)
        .filter(|f| !FACET_GROUPS.iter().any(|(k, _)| k == f))
        .collect();
    if !unknown.is_empty() {
        return Err(invalid_facet(format!(
            "unknown facet group: {}",
            unknown.join(", ")
        )));
    }
    let mut seen = std::collections::BTreeSet::new();
    if !ids.iter().all(|f| seen.insert(f.as_str())) {
        return Err(invalid_facet("facets repeats a group"));
    }
    Ok(ids)
}

/// Counts and per-group states of the requested facet groups.
pub type FacetCounts = (IndexMap<String, Option<i64>>, IndexMap<String, String>);

/// Count requested groups in one pass and retain an explicit state per group.
#[allow(clippy::too_many_arguments)]
pub fn facet_counts(
    con: &Connection,
    budget: &Budget<'_>,
    ids: &[String],
    join: &str,
    where_: &str,
    params: &[SqlValue],
    timeout: f64,
    resource_graph_state: Option<&str>,
    facet_groups: Option<&IndexMap<String, String>>,
) -> Result<FacetCounts, QueryError> {
    let mut unavailable: Vec<&str> = Vec::new();
    let mut capped: Vec<&str> = Vec::new();
    let has = |name: &str| ids.iter().any(|g| g == name);
    if (has("internal") || has("external"))
        && !crate::url_query::exists(
            con,
            "SELECT 1 FROM scan WHERE singleton=1 AND start_url IS NOT NULL AND start_url<>''",
        )?
    {
        unavailable.extend(["internal", "external"].into_iter().filter(|g| has(g)));
    }
    if has("in_sitemap") && !sitemap_membership_measured(con)? {
        unavailable.push("in_sitemap");
    }
    if has("with_inlinks") && !links_measured(con)? {
        unavailable.push("with_inlinks");
    }
    let resource: Vec<&str> = [
        "with_resources",
        "with_images",
        "with_scripts",
        "with_stylesheets",
    ]
    .into_iter()
    .filter(|g| has(g))
    .collect();
    if !resource.is_empty() && resource_graph_state != Some("complete") {
        if resource_graph_state == Some("capped") {
            capped.extend(resource);
        } else {
            unavailable.extend(resource);
        }
    }
    let active: Vec<&String> = ids
        .iter()
        .filter(|g| !unavailable.contains(&g.as_str()) && !capped.contains(&g.as_str()))
        .collect();
    let mut counts: IndexMap<String, Option<i64>> = IndexMap::new();
    let mut states: IndexMap<String, String> = IndexMap::new();
    for g in ids {
        if unavailable.contains(&g.as_str()) {
            counts.insert(g.clone(), None);
            states.insert(g.clone(), "unavailable".into());
        } else if capped.contains(&g.as_str()) {
            counts.insert(g.clone(), None);
            states.insert(g.clone(), "capped".into());
        }
    }
    if active.is_empty() {
        return Ok((counts, states));
    }
    let lookup = |g: &str| -> String {
        facet_groups
            .and_then(|m| m.get(g).cloned())
            .or_else(|| {
                FACET_GROUPS
                    .iter()
                    .find(|(k, _)| *k == g)
                    .map(|(_, v)| v.to_string())
            })
            .unwrap_or_default()
    };
    let exprs = active
        .iter()
        .map(|g| {
            format!(
                "COALESCE(SUM(CASE WHEN ({}) THEN 1 ELSE 0 END),0)",
                lookup(g)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    budget.start(timeout);
    let source_join = if join.is_empty() {
        " JOIN urls u ON u.url_id=p.url_id"
    } else {
        join
    };
    let sql = format!("SELECT {exprs} FROM pages p{source_join}{where_}");
    let row: Result<Vec<i64>, rusqlite::Error> =
        con.query_row(&sql, params_from_iter(params.iter()), |r| {
            (0..active.len()).map(|i| r.get::<_, i64>(i)).collect()
        });
    match row {
        Ok(values) => {
            for (g, v) in active.iter().zip(values) {
                counts.insert((*g).clone(), Some(v));
                states.insert((*g).clone(), "exact".into());
            }
        }
        Err(e) if is_interrupt(&e) => {
            for g in &active {
                counts.insert((*g).clone(), None);
                states.insert((*g).clone(), "capped".into());
            }
        }
        Err(e) => {
            return Err(QueryError::new(
                "cannot_read",
                format!("cannot read scan: {e}"),
            ))
        }
    }
    Ok((counts, states))
}

/// True when every declared sitemap root was fetched completely.
pub fn sitemap_membership_measured(con: &Connection) -> Result<bool, QueryError> {
    let count = |sql: &str| crate::url_query::count(con, sql);
    let roots = count("SELECT COUNT(*) FROM context_items WHERE kind='sitemap_declaration'")?;
    if roots == 0 {
        return Ok(false);
    }
    let complete = count(
        "SELECT COUNT(*) FROM context_items WHERE kind='sitemap_fetch_summary' AND completeness='complete'",
    )?;
    Ok(complete == roots)
}

/// True when link observations were retained (`links.state` is complete or empty).
pub fn links_measured(con: &Connection) -> Result<bool, QueryError> {
    let text: Option<String> = con
        .query_row(
            "SELECT capabilities_json FROM scan WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .map_err(crate::url_query::sql_error)?;
    let state = text
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("links").cloned())
        .and_then(|l| l.get("state").and_then(Value::as_str).map(str::to_string));
    Ok(matches!(state.as_deref(), Some("complete" | "empty")))
}
