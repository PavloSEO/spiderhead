//! Preset views and facet groups of the URL table, plus the measured-evidence
//! checks the facets need. Port of the reference suite.

use rusqlite::Connection;
use serde_json::{json, Value};

use super::filters::QueryError;

/// Return a fixed SQL expression for a scheme-and-authority URL prefix.
fn origin_expression(value: &str) -> String {
    let scheme = format!("instr({value},'://')");
    let remainder = format!("substr({value},{scheme}+3)");
    let slash = format!("instr({remainder},'/')");
    let query = format!("instr({remainder},'?')");
    let fragment = format!("instr({remainder},'#')");
    let authority_end = format!(
        "CASE WHEN {slash}>0 THEN {scheme}+1+{slash} \
         WHEN {query}>0 THEN {scheme}+1+{query} \
         WHEN {fragment}>0 THEN {scheme}+1+{fragment} \
         ELSE length({value}) END"
    );
    format!("lower(substr({value},1,{authority_end}))")
}

fn page_origin() -> String {
    origin_expression("u.url")
}

fn start_url() -> &'static str {
    "(SELECT start_url FROM scan WHERE singleton=1)"
}

fn start_origin() -> String {
    origin_expression(start_url())
}

/// Named filter sets. A preset is AND-combined with caller filters and reported back.
/// `(english_label, [filter objects])` — UI labels live in `i18n`, keyed by preset id.
pub fn preset_filters(preset: &str) -> Result<Vec<Value>, QueryError> {
    let filters: &[Value] = match preset {
        "status_3xx" => &[json!({"column":"status_class","op":"eq","value":"3xx"})],
        "status_4xx" => &[json!({"column":"status_class","op":"eq","value":"4xx"})],
        "status_5xx" => &[json!({"column":"status_class","op":"eq","value":"5xx"})],
        "no_response" => &[json!({"column":"status_class","op":"eq","value":"none"})],
        "title_missing" => &[json!({"column":"title","op":"empty"})],
        "title_over_60" => &[json!({"column":"title_length","op":"gt","value":60})],
        "meta_description_missing" => &[json!({"column":"meta_description","op":"empty"})],
        "meta_description_over_160" => {
            &[json!({"column":"meta_description_length","op":"gt","value":160})]
        }
        "h1_missing" => &[json!({"column":"h1","op":"empty"})],
        "h1_multiple" => &[json!({"column":"h1_2","op":"not_empty"})],
        "canonical_missing" => &[json!({"column":"canonical","op":"empty"})],
        "images_missing_alt" => &[json!({"column":"images_missing_alt_attr","op":"gt","value":0})],
        "noindex_meta" => &[json!({"column":"meta_robots","op":"contains","value":"noindex"})],
        "noindex_header" => &[json!({"column":"x_robots","op":"contains","value":"noindex"})],
        "no_outlinks" => &[json!({"column":"outlinks","op":"eq","value":0})],
        "hreflang" => &[json!({"column":"has_hreflang","op":"eq","value":true})],
        "duplicate_titles" => &[json!({"column":"title_duplicate","op":"eq","value":true})],
        "in_sitemap" => &[json!({"column":"in_sitemap","op":"eq","value":true})],
        "inlinks" => &[json!({"column":"inlinks","op":"gt","value":0})],
        "robots_blocked" => &[json!({"column":"robots_blocked","op":"eq","value":true})],
        "description_duplicate" => {
            &[json!({"column":"description_duplicate","op":"eq","value":true})]
        }
        "canonical_non_indexable" => {
            &[json!({"column":"canonical_non_indexable","op":"eq","value":true})]
        }
        _ => {
            return Err(QueryError::invalid(
                "unknown_preset",
                format!("preset is not one of: {}", PRESET_IDS.join(", ")),
            ))
        }
    };
    Ok(filters.to_vec())
}

/// Preset ids in declaration order (stable for menus).
pub static PRESET_IDS: &[&str] = &[
    "status_3xx",
    "status_4xx",
    "status_5xx",
    "no_response",
    "title_missing",
    "title_over_60",
    "meta_description_missing",
    "meta_description_over_160",
    "h1_missing",
    "h1_multiple",
    "canonical_missing",
    "images_missing_alt",
    "noindex_meta",
    "noindex_header",
    "no_outlinks",
    "hreflang",
    "duplicate_titles",
    "in_sitemap",
    "inlinks",
    "robots_blocked",
    "description_duplicate",
    "canonical_non_indexable",
];

/// Facet id → fixed SQL expression over `pages p` (+ `urls u` where needed).
/// Resource-graph facets exist in the vocabulary but always report `unavailable`
/// on scan.v1, like reference does when coverage is not `complete`.
pub fn facet_groups() -> Vec<(String, String)> {
    let po = page_origin();
    let so = start_origin();
    let resource = |extra: &str| {
        format!(
            "EXISTS (SELECT 1 FROM resource_graph_occurrences r WHERE r.page_url_id=p.url_id \
             AND r.source_document_id=p.document_id{extra})"
        )
    };
    vec![
        ("all".into(), "1".into()),
        (
            "status_2xx".into(),
            "p.status_code>=200 AND p.status_code<300".into(),
        ),
        (
            "status_3xx".into(),
            "p.status_code>=300 AND p.status_code<400".into(),
        ),
        (
            "status_4xx".into(),
            "p.status_code>=400 AND p.status_code<500".into(),
        ),
        (
            "status_5xx".into(),
            "p.status_code>=500 AND p.status_code<600".into(),
        ),
        (
            "title_missing".into(),
            "(p.title IS NULL OR p.title='')".into(),
        ),
        (
            "canonical_missing".into(),
            "(p.canonical IS NULL OR p.canonical='')".into(),
        ),
        (
            "directives_noindex".into(),
            "instr(lower(p.meta_robots||','||p.x_robots),'noindex')>0".into(),
        ),
        (
            "directives_nofollow".into(),
            "instr(lower(p.meta_robots||','||p.x_robots),'nofollow')>0".into(),
        ),
        ("with_outlinks".into(), "p.outlinks>0".into()),
        ("internal".into(), format!("{po}={so}")),
        ("external".into(), format!("{po}<>{so}")),
        (
            "title_duplicate".into(),
            "EXISTS (SELECT 1 FROM pages t WHERE t.url_id<>p.url_id \
             AND trim(t.title)<>'' AND lower(t.title)=lower(p.title))"
                .into(),
        ),
        ("title_long".into(), "length(p.title)>60".into()),
        (
            "title_short".into(),
            "(p.title<>'' AND length(p.title)<30)".into(),
        ),
        (
            "canonical_other".into(),
            "(p.canonical<>'' AND p.canonical<>u.url)".into(),
        ),
        (
            "canonical_self".into(),
            "(p.canonical<>'' AND p.canonical=u.url)".into(),
        ),
        (
            "in_sitemap".into(),
            "EXISTS (SELECT 1 FROM context_items s WHERE s.kind='sitemap_declared_url' \
             AND CAST(json_extract(s.payload_json,'$.url_id') AS INTEGER)=p.url_id)"
                .into(),
        ),
        (
            "with_inlinks".into(),
            "EXISTS (SELECT 1 FROM links l WHERE l.destination_url_id=p.url_id)".into(),
        ),
        ("with_resources".into(), resource("")),
        ("with_images".into(), resource(" AND r.kind='image'")),
        (
            "with_scripts".into(),
            resource(" AND r.kind IN ('script','module')"),
        ),
        (
            "with_stylesheets".into(),
            resource(" AND r.kind='stylesheet'"),
        ),
    ]
}

/// Facets whose counts need a complete retained resource graph (scan.v2 only).
pub static RESOURCE_FACETS: [&str; 4] = [
    "with_resources",
    "with_images",
    "with_scripts",
    "with_stylesheets",
];

/// Requested facet ids in order; `Null`/empty list means no facets.
pub fn facet_ids(facets: &Value) -> Result<Vec<String>, QueryError> {
    let groups = facet_groups();
    match facets {
        Value::Null => Ok(vec![]),
        Value::String(s) if s == "all" => Ok(groups.into_iter().map(|(id, _)| id).collect()),
        Value::Array(list) if list.iter().all(|v| v.is_string()) => {
            let ids: Vec<String> = list
                .iter()
                .map(|v| v.as_str().unwrap_or_default().to_string())
                .collect();
            let known: std::collections::BTreeSet<&str> =
                groups.iter().map(|(id, _)| id.as_str()).collect();
            let unknown: Vec<&str> = ids
                .iter()
                .filter(|id| !known.contains(id.as_str()))
                .map(String::as_str)
                .collect();
            if !unknown.is_empty() {
                return Err(QueryError::invalid(
                    "invalid_facet",
                    format!("unknown facet group: {}", unknown.join(", ")),
                ));
            }
            let mut dedup = ids.clone();
            dedup.sort();
            dedup.dedup();
            if dedup.len() != ids.len() {
                return Err(QueryError::invalid(
                    "invalid_facet",
                    "facets repeats a group",
                ));
            }
            Ok(ids)
        }
        _ => Err(QueryError::invalid(
            "invalid_facet",
            "facets must be a list of group ids or 'all'",
        )),
    }
}

/// Sitemap membership is measured only when every declared root has a complete
/// fetch summary (reference `_sitemap_membership_measured`).
pub fn sitemap_membership_measured(con: &Connection) -> bool {
    let roots: i64 = con
        .query_row(
            "SELECT COUNT(*) FROM context_items WHERE kind='sitemap_declaration'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if roots == 0 {
        return false;
    }
    let complete: i64 = con
        .query_row(
            "SELECT COUNT(*) FROM context_items WHERE kind='sitemap_fetch_summary' \
             AND completeness='complete'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    complete == roots
}

/// Link observations are measured when `capabilities.links.state` is complete/empty.
pub fn links_measured(con: &Connection) -> bool {
    let text: String = con
        .query_row(
            "SELECT capabilities_json FROM scan WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .unwrap_or_default();
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get("links")?.get("state")?.as_str().map(str::to_owned))
        .is_some_and(|s| s == "complete" || s == "empty")
}

/// Whether a non-empty `start_url` exists (internal/external facets need it).
pub fn has_start_url(con: &Connection) -> bool {
    con.query_row(
        "SELECT 1 FROM scan WHERE singleton=1 AND start_url IS NOT NULL AND start_url<>''",
        [],
        |_| Ok(()),
    )
    .optional()
    .unwrap_or_default()
    .is_some()
}

use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_is_allowed_filter_syntax() {
        for id in PRESET_IDS {
            let filters = preset_filters(id).expect(id);
            for f in &filters {
                super::super::filters::filter_clause(f).expect(id);
            }
        }
    }

    #[test]
    fn unknown_preset_names_allowed_ones() {
        let e = preset_filters("nope").unwrap_err();
        assert_eq!(e.reason_code, "unknown_preset");
        assert!(e.message.contains("status_3xx"));
    }

    #[test]
    fn facet_id_validation() {
        assert_eq!(facet_ids(&Value::Null).unwrap().len(), 0);
        assert_eq!(facet_ids(&json!("all")).unwrap().len(), 23);
        assert!(facet_ids(&json!(["status_2xx", "status_2xx"])).is_err());
        assert!(facet_ids(&json!(["bogus"])).is_err());
    }
}
