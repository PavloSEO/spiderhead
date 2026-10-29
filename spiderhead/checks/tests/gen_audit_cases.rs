//! Synthetic-case parity: replay `fixtures/gen/*.json` (produced by
//! the reference suite running the reference baseline) through the ported
//! Rust checks and diff every emitted issue, skip, group and page metric.

use std::path::{Path, PathBuf};

use rusqlite::{params_from_iter, types::Value as SqlValue, Connection};
use serde_json::{Map, Value};
use spiderhead_checks::audit::rules;
use spiderhead_checks::audit::{AuditContext, Page};
use spiderhead_oracle_diff::{diff, Options};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/gen")
}

/// Dispatch from the fixture's reference check name to the ported Rust function.
fn run_check(name: &str, ctx: &mut AuditContext) -> Result<(), String> {
    let bare = name.rsplit('.').next().unwrap_or(name);
    match bare {
        "check_titles" => rules::content_meta::check_titles(ctx),
        "check_descriptions" => rules::content_meta::check_descriptions(ctx),
        "check_headings" => rules::content_meta::check_headings(ctx),
        "check_schema" => rules::content_meta::check_schema(ctx),
        "check_og" => rules::content_meta::check_og(ctx),
        "check_structured_data_missing" => rules::content::check_structured_data_missing(ctx),
        "check_response_codes" => rules::indexation::check_response_codes(ctx),
        "check_indexability" => rules::indexation::check_indexability(ctx),
        "check_canonical_annotations" => rules::indexation::check_canonical_annotations(ctx),
        "check_canonical_directives" => rules::indexation::check_canonical_directives(ctx),
        "check_canonical_chain" => rules::indexation::check_canonical_chain(ctx),
        "check_canonical_extra" => rules::indexation::check_canonical_extra(ctx),
        "check_canonical_policy" => rules::indexation::check_canonical_policy(ctx),
        "check_canonical_to_redirect" => rules::indexation::check_canonical_to_redirect(ctx),
        "check_canonical_target_error" => rules::indexation::check_canonical_target_error(ctx),
        "check_canonical_homepage_group" => rules::indexation::check_canonical_homepage_group(ctx),
        "check_unlinked_canonical" => rules::indexation::check_unlinked_canonical(ctx),
        "check_ajax_crawling_scheme" => rules::indexation::check_ajax_crawling_scheme(ctx),
        "check_redirect_chains" => rules::indexation::check_redirect_chains(ctx),
        "check_directives_extra" => rules::indexation::check_directives_extra(ctx),
        "check_redirect_type" => rules::indexation::check_redirect_type(ctx),
        "check_url_hygiene" => rules::content::check_url_hygiene(ctx),
        "check_url_and_perf" => rules::content::check_url_and_perf(ctx),
        "check_url_extra" => rules::content::check_url_extra(ctx),
        "check_content" => rules::content::check_content(ctx),
        "check_content_quality" => rules::content::check_content_quality(ctx),
        "check_duplicates" => rules::content::check_duplicates(ctx),
        "check_charset" => rules::technical::check_charset(ctx),
        "check_viewport" => rules::technical::check_viewport(ctx),
        "check_compression" => rules::technical::check_compression(ctx),
        "check_document_markup" => rules::markup::check_document_markup(ctx),
        "check_links_extra" => rules::links::check_links_extra(ctx),
        "check_invalid_link_targets" => rules::links::check_invalid_link_targets(ctx),
        "check_link_placement" => rules::links::check_link_placement(ctx),
        "check_link_score" => rules::inlinks_graph::check_link_score(ctx),
        "check_inlink_composition" => rules::inlinks_graph::check_inlink_composition(ctx),
        "check_discovery_path" => rules::inlinks_graph::check_discovery_path(ctx),
        "check_internal_link_graph" => rules::inlinks_graph::check_internal_link_graph(ctx),
        "check_anchor_text" => rules::inlinks_graph::check_anchor_text(ctx),
        "check_insecure_subresources" => rules::inlinks_graph::check_insecure_subresources(ctx),
        "check_hreflang_targets" => rules::hreflang::check_hreflang_targets(ctx),
        "check_hreflang_noindex_targets" => rules::hreflang::check_hreflang_noindex(ctx),
        "check_hreflang_confirmation_consistency" => {
            rules::hreflang::check_hreflang_consistency(ctx)
        }
        "check_retained_links" => rules::retained_links::check_retained_links(ctx),
        "check_localhost_outlinks" => rules::stored::check_localhost_outlinks(ctx),
        "check_security_headers" => rules::stored::check_security_headers(ctx),
        "check_link_predicates" => {
            ctx.link_rel = rules::link_predicates::run_link_predicates(ctx);
        }
        "check_heading_outline" => rules::content_meta::check_heading_outline(ctx),
        "check_placeholder_markers" => rules::content::check_placeholder_markers(ctx),
        "check_render_assets" => rules::asset_checks::check_render_assets(ctx),
        "check_resource_assets" => rules::asset_checks::check_resource_assets(ctx),
        "check_network_payload" => rules::asset_checks::check_network_payload(ctx),
        "check_rendered_elements" => rules::render_checks::check_rendered_elements(ctx),
        "check_mobile_render_layout" => rules::mobile_checks::check_mobile_render_layout(ctx),
        "check_html_weight" => rules::heuristics::check_html_weight(ctx),
        "check_dom" => rules::heuristics::check_dom(ctx),
        "check_content_duplication" => rules::heuristics::check_content_duplication(ctx),
        "check_templated_titles" => rules::heuristics::check_templated_titles(ctx),
        "check_semantic_content" => rules::semantic::check_semantic_content(ctx),
        "check_semantic_lexical" => rules::semantic::check_semantic_lexical(ctx),
        "run_eeat" => rules::eeat::run_eeat(ctx),
        "check_native_page_evidence" => rules::native::check_native_page_evidence(ctx),
        "check_native_image_resources" => rules::native::check_native_image_resources(ctx),
        "check_native_image_size" => rules::native::check_native_image_size(ctx),
        "check_native_export_aggregates" => rules::native::check_native_export_aggregates(ctx),
        "check_native_structured_and_mixed" => {
            rules::content::check_structured_data_missing(ctx);
            rules::content::check_mixed_content(ctx);
        }
        "check_tech_extra" => rules::technical::check_tech_extra(ctx),
        "retained_sitemap" => {
            ctx.sitemap_summary = rules::sitemap::run_retained_sitemap(ctx);
        }
        "check_amp_pairing" => rules::technical::check_amp_pairing(ctx),
        "check_doctype" => rules::technical::check_doctype(ctx),
        "check_mobile_alternate" => rules::technical::check_mobile_alternate(ctx),
        "check_element_position" => rules::technical::check_element_position(ctx),
        "check_document_skeleton" => rules::technical::check_document_skeleton(ctx),
        "check_html_structure" => rules::technical::check_html_structure(ctx),
        "check_pagination" => rules::pagination::check_pagination(ctx),
        "check_pagination_series" => rules::pagination::check_pagination_series(ctx),
        "check_pagination_sequence" => rules::pagination::check_pagination_sequence(ctx),
        "check_pagination_declarations" => rules::pagination::check_pagination_declarations(ctx),
        "check_body_mime" => rules::native::check_body_mime(ctx),
        "check_render_blocking" => rules::render_blocking::check_render_blocking(ctx),
        "check_native_script_redirects" => rules::native::check_native_script_redirects(ctx),
        "check_link_composition" => rules::link_composition::check_link_composition(ctx),
        "check_redirect_hop_hsts" => rules::stored::check_redirect_hop_hsts(ctx),
        "provider_findings" => run_provider_findings(ctx),
        "check_lighthouse_reports" => run_lighthouse_reports(ctx),
        other => return Err(format!("unported check function {other}")),
    }
    Ok(())
}

/// `provider_findings`: build the project-bound seam out of `ctx.provider_fixture`,
/// evaluate `project_provider_findings`, then emit issues the way the audit does.
/// A failed read becomes the same `unavailable` evidence the fixtures record.
fn run_provider_findings(ctx: &mut AuditContext) {
    use spiderhead_sources::provider_findings as pf;

    let spec = ctx.provider_fixture.clone().unwrap_or(Value::Null);
    let bindings_vec = spec["bindings"].as_array().cloned();
    let bindings = if spec["bindings"].is_null() {
        None
    } else {
        bindings_vec.as_deref()
    };
    let origin = spec["origin"].as_str().map(str::to_owned);
    let history_spec = spec["history"].as_array().cloned().unwrap_or_default();
    let history = move |source: &str, resource: &str| -> Result<Value, ()> {
        history_spec
            .iter()
            .find(|h| {
                h["source"].as_str() == Some(source) && h["resource"].as_str() == Some(resource)
            })
            .map(|h| h["doc"].clone())
            .ok_or(())
    };
    let insp_rows = spec["inspection_rows"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let inspection_latest = move |_site: &str| -> Result<Vec<Value>, ()> { Ok(insp_rows.clone()) };
    let cf_rows = spec["cloudflare_rows"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let cloudflare =
        move |_binding: &Value| -> Result<Value, ()> { Ok(serde_json::json!({"rows": cf_rows})) };
    let inspect_gsc = spec["inspect_gsc"].as_bool().unwrap_or(false);
    let reservation_doc = spec["inspection_reservation"].clone();
    let inspect_doc = spec["inspection_result"].clone();
    let settle_doc = spec["inspection_settle"].clone();
    let reserve = move |_site: &str, _wanted: &[String]| reservation_doc.clone();
    let inspect = move |_site: &str, _urls: &[String]| inspect_doc.clone();
    let settle = move |_id: &Value, _rows: &[Value], _made: i64| Ok(settle_doc.clone());
    let collect = inspect_gsc.then_some(pf::InspectionCollect {
        reserve: &reserve,
        inspect: &inspect,
        settle: &settle,
    });
    let today = spec["as_of"]
        .as_str()
        .and_then(spiderhead_sources::pytime::date_fromisoformat)
        .or_else(|| spiderhead_sources::pytime::date_fromisoformat("2026-01-15"))
        .unwrap_or(0);
    let included: Vec<Value> = ctx
        .pages
        .iter()
        .take(pf::MAX_CRAWL_PAGES)
        .map(|p| serde_json::json!({"url": p.url, "indexability": p.indexability}))
        .collect();
    let omitted = ctx.pages.len() - included.len();
    let ops = pf::ProviderOps {
        bindings,
        origin: origin.as_deref(),
        history: &history,
        inspection_latest: &inspection_latest,
        cloudflare_snapshots: &cloudflare,
        crawl_partial: spec["crawl_partial"].as_bool().unwrap_or(omitted > 0),
        crawl_pages_omitted: spec["crawl_pages_omitted"]
            .as_i64()
            .unwrap_or(omitted as i64),
        inspect_gsc,
        inspect_ops: collect,
        today,
    };
    let evidence = pf::project_provider_findings(&included, &ops).unwrap_or_else(|_| {
        serde_json::json!({
            "schema_version": "provider_findings.v1",
            "state": "unavailable",
            "reason": "retained provider history could not be read",
            "sources": {},
        })
    });
    ctx.provider_evidence = Some(evidence);
    let evidence = ctx.provider_evidence.clone().unwrap_or(Value::Null);
    rules::provider::emit_provider_issues(ctx, &evidence);
}

/// `gen.check_lighthouse_reports`: materialize `ctx.lighthouse_fixture` files into
/// a temporary directory, point `config.input.lighthouse_reports` at them and run
/// the supplied-report check — the same seam the audit uses.
fn run_lighthouse_reports(ctx: &mut AuditContext) {
    let spec = ctx.lighthouse_fixture.clone().unwrap_or(Value::Null);
    let dir = tempfile::tempdir().expect("lighthouse fixture dir");
    let mut paths: Map<String, Value> = Map::new();
    if let Some(files) = spec.get("files").and_then(Value::as_object) {
        for (index, (name, content)) in files.iter().enumerate() {
            let path = dir.path().join(format!("report-{index}.json"));
            let text = match content {
                Value::String(s) => s.clone(),
                other => serde_json::to_string(other).unwrap_or_default(),
            };
            if std::fs::write(&path, text).is_ok() {
                paths.insert(
                    name.clone(),
                    Value::from(path.to_string_lossy().into_owned()),
                );
            }
        }
    }
    let missing = dir
        .path()
        .join("missing.json")
        .to_string_lossy()
        .into_owned();
    let reports = match spec.get("reports") {
        Some(Value::Object(map)) => Value::Object(
            map.iter()
                .map(|(url, key)| {
                    let path = key
                        .as_str()
                        .and_then(|k| paths.get(k))
                        .cloned()
                        .unwrap_or_else(|| Value::from(missing.clone()));
                    (url.clone(), path)
                })
                .collect(),
        ),
        other => other.cloned().unwrap_or(Value::Null),
    };
    let input = ctx
        .config
        .entry("input")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Value::Object(map) = input {
        map.insert("lighthouse_reports".into(), reports);
    }
    rules::asset_checks::check_lighthouse_reports(ctx);
    drop(dir);
}

fn page_of(spec: &Map<String, Value>, measured: &std::collections::BTreeSet<String>) -> Page {
    let get = |k: &str| spec.get(k).cloned().unwrap_or(Value::Null);
    let rec = match get("rec") {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    let mut rec = rec;
    if let Value::Object(row) = get("row") {
        for (k, v) in row {
            // The `row` overlay is the stored pages-table projection: a column
            // that shares an analyzer field name carries the table's NOT NULL
            // default (0/"") when the cell was unmeasured — letting it land
            // would fabricate a measurement the oracle's sparse rec lacks.
            // Only stored-only columns (url_id, *_json, …) may fill the rec.
            if measured.contains(k.as_str()) {
                continue;
            }
            rec.entry(k).or_insert(v);
        }
    }
    // The native loader's per-page derivations (`load_pages`): rec keys the
    // scan table cannot carry but the analyzer's record always holds.
    let text = |rec: &Map<String, Value>, k: &str| {
        rec.get(k).and_then(Value::as_str).unwrap_or("").to_owned()
    };
    if !rec.contains_key("title_length") {
        rec.insert(
            "title_length".into(),
            Value::from(text(&rec, "title").chars().count() as i64),
        );
    }
    if !rec.contains_key("desc_length") {
        rec.insert(
            "desc_length".into(),
            Value::from(text(&rec, "meta_description").chars().count() as i64),
        );
    }
    if !rec.contains_key("meta_charset") {
        rec.insert("meta_charset".into(), Value::from(text(&rec, "charset")));
    }
    if !rec.contains_key("trust_signals") {
        let signals = text(&rec, "trust_signals_json");
        rec.insert(
            "trust_signals".into(),
            serde_json::from_str::<Value>(&signals).unwrap_or(Value::Null),
        );
    }
    let code = get("status_code").as_i64();
    let mut page = Page {
        url: get("url").as_str().unwrap_or("").to_owned(),
        status_code: code,
        content_type: get("content_type").as_str().unwrap_or("").to_owned(),
        indexability: get("indexability").as_str().unwrap_or("").to_owned(),
        indexability_status: get("indexability_status").as_str().unwrap_or("").to_owned(),
        rec,
        ..Page::default()
    };
    page.fill_metrics();
    page
}

fn sql_value(v: &Value) -> SqlValue {
    match v {
        Value::Null => SqlValue::Null,
        Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
        Value::Number(n) => n
            .as_i64()
            .map(SqlValue::Integer)
            .unwrap_or_else(|| SqlValue::Real(n.as_f64().unwrap_or(0.0))),
        Value::String(s) => SqlValue::Text(s.clone()),
        // `{"$blob": "<hex>"}` encodes a BLOB the JSON fixture cannot carry.
        Value::Object(m) if m.len() == 1 && m.contains_key("$blob") => m
            .get("$blob")
            .and_then(Value::as_str)
            .and_then(|h| {
                (0..h.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&h[i..i + 2], 16).ok())
                    .collect::<Option<Vec<u8>>>()
            })
            .map(SqlValue::Blob)
            .unwrap_or(SqlValue::Null),
        Value::Array(_) | Value::Object(_) => SqlValue::Text(v.to_string()),
    }
}

/// Recursive object merge (`overlay` wins), matching reference `deep_merge` for
/// the object-only shapes audit configs take.
fn merge_json(base: Map<String, Value>, overlay: Map<String, Value>) -> Map<String, Value> {
    let mut out = base;
    for (key, value) in overlay {
        match (out.get_mut(&key), value) {
            (Some(Value::Object(b)), Value::Object(o)) => {
                *b = merge_json(std::mem::take(b), o);
            }
            (_, v) => {
                out.insert(key, v);
            }
        }
    }
    out
}

fn build_db(case: &Map<String, Value>) -> Option<(tempfile::TempDir, Connection)> {
    let db = case.get("db")?.as_object()?;
    let dir = tempfile::tempdir().ok()?;
    let path = dir.path().join("scan.sqlite");
    let con = Connection::open(&path).ok()?;
    // `open_scan` keeps file-backed temp storage; AnalysisGraph requires it.
    con.execute_batch("PRAGMA temp_store = 1;").ok()?;
    con.execute_batch(include_str!("../../store/sql/scan_v1.sql"))
        .ok()?;
    // `external_checks` is installed additively by `storage.external_checks`
    // (`ensure_schema`), outside the scan_v1 schema.
    if db.contains_key("external_checks") {
        con.execute_batch(
            "CREATE TABLE IF NOT EXISTS external_checks (ordinal INTEGER PRIMARY KEY CHECK (ordinal >= 0), outcome TEXT NOT NULL CHECK (outcome IN ('fetched','failed','blocked','skipped')), payload_json TEXT NOT NULL);\
             CREATE TABLE IF NOT EXISTS external_check_summary (singleton INTEGER PRIMARY KEY CHECK (singleton=1), payload_json TEXT NOT NULL);",
        )
        .ok()?;
    }
    // `storage.resource_graph` ensure_schema, flattened: base columns plus
    // every additive ALTER the real writer applies on top.
    if db.contains_key("resource_graph_occurrences") || db.contains_key("resource_graph_fetches") {
        con.execute_batch(
            "CREATE TABLE IF NOT EXISTS resource_graph_occurrences (occurrence_id INTEGER PRIMARY KEY,page_url_id INTEGER NOT NULL,source_document_id INTEGER NOT NULL,representation TEXT NOT NULL,ordinal INTEGER NOT NULL,kind TEXT NOT NULL,carrier TEXT NOT NULL,raw_url TEXT NOT NULL,resolved_url TEXT NOT NULL,integrity TEXT,integrity_state TEXT NOT NULL DEFAULT 'unknown',nesting_depth INTEGER NOT NULL,state TEXT NOT NULL,reason TEXT NOT NULL,UNIQUE(page_url_id,source_document_id,representation,ordinal));\
             CREATE TABLE IF NOT EXISTS resource_graph_fetches (resolved_url TEXT PRIMARY KEY,state TEXT NOT NULL,reason TEXT NOT NULL,status_code INTEGER,content_type TEXT NOT NULL,bytes_received INTEGER NOT NULL,elapsed_seconds REAL,origin_host TEXT NOT NULL,redirects INTEGER NOT NULL,nesting_depth INTEGER NOT NULL,final_url TEXT NOT NULL DEFAULT '',compression TEXT NOT NULL DEFAULT 'unknown',cache_state TEXT NOT NULL DEFAULT 'unknown',integrity_state TEXT NOT NULL DEFAULT 'unknown',width INTEGER,height INTEGER,body_state TEXT NOT NULL DEFAULT 'unavailable',delivery_json TEXT);",
        )
        .ok()?;
    }
    // The DDL turns foreign_keys ON; generated rows land in fixture (sorted)
    // order, not dependency order.
    con.execute_batch("PRAGMA foreign_keys = OFF;").ok()?;
    for (table, rows) in db {
        let Some(rows) = rows.as_array() else {
            continue;
        };
        for row in rows {
            let Some(row) = row.as_object() else { continue };
            let cols: Vec<&str> = row.keys().map(String::as_str).collect();
            let sql = format!(
                "INSERT INTO {table} ({}) VALUES ({})",
                cols.join(", "),
                cols.iter().map(|_| "?").collect::<Vec<_>>().join(", ")
            );
            let params: Vec<SqlValue> = cols.iter().map(|c| sql_value(&row[*c])).collect();
            con.execute(&sql, params_from_iter(params)).ok()?;
        }
    }
    Some((dir, con))
}

fn actual_json(ctx: &AuditContext) -> Value {
    let mut out = Map::new();
    out.insert(
        "issues".into(),
        Value::Array(
            ctx.issues
                .iter()
                .map(|i| Value::Object(i.to_json()))
                .collect(),
        ),
    );
    out.insert(
        "skipped".into(),
        Value::Array(
            ctx.skipped
                .iter()
                .map(|s| {
                    let mut m = Map::new();
                    m.insert("id".into(), Value::from(s.id.clone()));
                    m.insert("reason".into(), Value::from(s.reason.clone()));
                    Value::Object(m)
                })
                .collect(),
        ),
    );
    out.insert(
        "groups".into(),
        Value::Array(
            ctx.groups
                .iter()
                .map(|g| Value::Object(g.to_json()))
                .collect(),
        ),
    );
    out.insert(
        "asset_measurements".into(),
        Value::Object(
            ctx.asset_measurements
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
    );
    out.insert(
        "internal_linking".into(),
        Value::Object(ctx.internal_linking.clone()),
    );
    out.insert(
        "trust_evidence".into(),
        Value::Object(ctx.trust_evidence.clone()),
    );
    out.insert("link_rel".into(), Value::Object(ctx.link_rel.clone()));
    out.insert(
        "sitemap_summary".into(),
        Value::Object(ctx.sitemap_summary.clone()),
    );
    out.insert(
        "fragment_links".into(),
        Value::Object(ctx.fragment_links.clone()),
    );
    out.insert(
        "link_position".into(),
        Value::Object(ctx.link_position.clone()),
    );
    let mut metrics = Map::new();
    for page in &ctx.pages {
        metrics.insert(page.url.clone(), Value::Object(page.metrics.clone()));
    }
    out.insert("page_metrics".into(), Value::Object(metrics));
    if let Some(evidence) = &ctx.provider_evidence {
        out.insert("provider_evidence".into(), evidence.clone());
    }
    Value::Object(out)
}

fn run_case(case: &Map<String, Value>) -> Vec<spiderhead_oracle_diff::Divergence> {
    let name = case.get("name").and_then(Value::as_str).unwrap_or("?");
    let measured: std::collections::BTreeSet<String> = case
        .get("fields")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let pages: Vec<Page> = case
        .get("pages")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|p| p.as_object())
                .map(|p| page_of(p, &measured))
                .collect()
        })
        .unwrap_or_default();
    let mut ctx = AuditContext::new(pages);
    if let Some(config) = case.get("config").and_then(Value::as_object) {
        ctx.config = config.clone();
        if let Some(t) = config.get("thresholds").and_then(Value::as_object) {
            ctx.thresholds.apply(t);
        }
    }
    ctx.requirements = serde_json::json!({
        "require_canonical": true,
        "require_h2": false,
        "require_hreflang": false,
        "require_structured_data": false,
        "require_og": false,
    })
    .as_object()
    .cloned()
    .unwrap_or_default();
    if let Some(reqs) = ctx
        .config
        .get("requirements")
        .and_then(Value::as_object)
        .cloned()
    {
        for (k, v) in reqs {
            ctx.requirements.insert(k, v);
        }
    }
    if let Some(attrs) = case.get("ctx").and_then(Value::as_object) {
        for (k, v) in attrs {
            match k.as_str() {
                "start_url" => {
                    ctx.start_url = v.as_str().map(str::to_owned).filter(|u| !u.is_empty())
                }
                "native_hreflang" => ctx.native_hreflang = Some(v.clone()),
                "saved_corpus" => ctx.saved_corpus = Some(v.clone()),
                // The corpus owner's self_codes index the confirmation pass
                // reads when a derivation ran (`ctx._saved_corpus` in the
                // reference run).
                "self_codes_fixture" => {
                    if let Value::Object(m) = v {
                        ctx.corpus_self_codes = Some(
                            m.iter()
                                .map(|(k, codes)| {
                                    (
                                        k.clone(),
                                        codes
                                            .as_array()
                                            .map(|a| {
                                                a.iter()
                                                    .filter_map(Value::as_str)
                                                    .map(str::to_owned)
                                                    .collect()
                                            })
                                            .unwrap_or_default(),
                                    )
                                })
                                .collect(),
                        );
                    }
                }
                "target_probes" => {
                    if let Value::Object(m) = v {
                        ctx.target_probes = m.clone();
                    }
                }
                "link_rel" => {
                    if let Value::Object(m) = v {
                        ctx.link_rel = m.clone();
                    }
                }
                "sitemap_summary" => {
                    if let Value::Object(m) = v {
                        ctx.sitemap_summary = m.clone();
                    }
                }
                "provider_fixture" => ctx.provider_fixture = Some(v.clone()),
                "lighthouse_fixture" => ctx.lighthouse_fixture = Some(v.clone()),
                _ => {}
            }
        }
    }
    // `export_records`: the typed projection each extra bulk frame (the
    // fixture's `export_frames`) produced for the reference run — loaded verbatim as
    // `ctx.exports`, a present-but-empty list still a frame `has` answers.
    if let Some(exports) = case.get("export_records").and_then(Value::as_object) {
        for (name, records) in exports {
            let rows = records
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_object)
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            ctx.exports.insert(name.clone(), rows);
        }
    }
    // The frame's effective column set, exactly as `_has_column` resolved it
    // for the oracle — the Rust side's `has_column` reads the same set.
    if let Some(fields) = case.get("fields").and_then(Value::as_array) {
        ctx.fields = fields
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
    }
    // A case with a "db" block gets a materialized scan.v1 file (the fixture's
    // resolved table rows), mirrored exactly as `load_pages`/the rules see it.
    let _db = build_db(case).map(|(dir, con)| {
        ctx.scan = Some(con);
        // `case.graph` asks the oracle for an AnalysisGraph backend; the Rust
        // checks read the same request as `ctx.graph_access` over `ctx.scan`.
        ctx.graph_access = case.get("graph").and_then(Value::as_bool).unwrap_or(false);
        // `hydrate()`: the artifact's run metadata and effective configuration,
        // exactly as the stored audit path. `ctx.config` carries the crawl
        // config underneath the case's audit-side keys, mirroring how the
        // reference handler reads `settings` from the artifact while `ctx.config`
        // stays the audit configuration.
        if let Some(con) = ctx.scan.as_ref() {
            if let Ok(run) = spiderhead_checks::audit::scan_run(con) {
                ctx.start_url = Some(run.start_url.clone()).filter(|u| !u.is_empty());
                ctx.crawl_partial = run.crawl_partial;
            }
            if let Ok(stored) = spiderhead_store::scan_schema::read_effective_config(con) {
                ctx.config = merge_json(stored, ctx.config.clone());
            }
        }
        dir
    });
    let checks: Vec<&str> = case
        .get("checks")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut hreflang_ran = false;
    for check in checks {
        let bare = check.rsplit('.').next().unwrap_or(check);
        // Fixtures list hreflang quality and reciprocity as two checks;
        // one pass covers both — run it once
        // whichever of the two names the fixture lists.
        if matches!(
            bare,
            "check_hreflang_quality" | "check_hreflang_reciprocity"
        ) {
            if !hreflang_ran {
                rules::hreflang::check_hreflang(&mut ctx);
                hreflang_ran = true;
            }
            continue;
        }
        run_check(check, &mut ctx).unwrap_or_else(|e| panic!("case {name}: {e}"));
    }
    let mut expected = case.get("expected").cloned().unwrap_or(Value::Null);
    let mut actual = actual_json(&ctx);
    // The reference page record leaves unmeasured metrics as nulls; the Rust
    // `Page::fill_metrics` derives the same values eagerly. Assert only the
    // keys the oracle actually materialized — the issue details themselves
    // carry every computed value the checks compare.
    if let (Some(Value::Object(exp)), Some(Value::Object(act))) = (
        expected.get_mut("page_metrics"),
        actual.get_mut("page_metrics"),
    ) {
        for (url, em) in exp.iter_mut() {
            let Some(em) = em.as_object_mut() else {
                continue;
            };
            let Some(am) = act.get_mut(url).and_then(Value::as_object_mut) else {
                continue;
            };
            let keep: Vec<String> = em
                .iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, _)| k.clone())
                .collect();
            em.retain(|k, _| keep.contains(k));
            am.retain(|k, _| keep.contains(k));
        }
    }
    diff(
        &expected,
        &actual,
        &Options {
            float_tolerance: 0.0,
            ..Options::default()
        },
    )
}

#[test]
fn gen_audit_cases_match_oracle() {
    let dir = fixture_dir();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("fixtures/gen directory")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no generated fixtures in {dir:?}");
    let mut failures = 0;
    for file in files {
        let doc: Value =
            serde_json::from_slice(&std::fs::read(&file).expect("read fixture")).expect("parse");
        for case in doc
            .get("cases")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let Some(case) = case.as_object() else {
                continue;
            };
            let name = case.get("name").and_then(Value::as_str).unwrap_or("?");
            let divs = run_case(case);
            if !divs.is_empty() {
                failures += divs.len();
                eprintln!(
                    "case {name} ({}):\n{}",
                    file.display(),
                    spiderhead_oracle_diff::report(&divs, 20)
                );
            }
        }
    }
    assert_eq!(failures, 0, "gen audit cases diverged from the oracle");
}
