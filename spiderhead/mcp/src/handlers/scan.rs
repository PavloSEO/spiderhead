//! Handlers over retained local scan artifacts (the reference suite,
//! the reference suite) and their storage cores.

use std::path::Path;

use serde_json::{json, Map, Value};

use rusqlite::OptionalExtension;
use spiderhead_store::corpus_inputs;
use spiderhead_store::directory_tree as tree_core;
use spiderhead_store::history::inspect_scan;
use spiderhead_store::scan_export::export_scan_data;
use spiderhead_store::scan_graph::{scan_graph as graph_core, GraphQuery};
use spiderhead_store::status::scan_status as status_core;
use spiderhead_store::url_query::{
    export_scan_query, scan_url_counts as counts_core, scan_url_query as query_core, ExportArgs,
    QueryArgs, EXPORT_MAX_ROWS,
};

use crate::error::ToolError;

use super::{failed, missing_option, need_str, opt, opt_bool, opt_float, opt_int, opt_str};

/// `scan_status`: summarize one scan snapshot offline.
pub fn scan_status(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let input_path = need_str(
        &args,
        "input_path",
        "scan-status",
        &["input_path", "full_validation"],
    )?;
    let full_validation = opt_bool(&args, "full_validation").unwrap_or(false);
    status_core(input_path, full_validation)
        .map(Value::Object)
        .map_err(failed)
}

/// `scan_url_query`: filter, sort and paginate one scan's page table.
pub fn scan_url_query(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let input_path = need_str(&args, "input_path", "scan-url-query", &QUERY_PARAMS)?;
    if let Some(export) = opt(&args, "export") {
        let export_args = ExportArgs {
            preset: opt(&args, "preset").cloned().unwrap_or(Value::Null),
            filters: opt(&args, "filters").cloned().unwrap_or(Value::Null),
            columns: opt(&args, "columns").cloned().unwrap_or(Value::Null),
            fmt: opt(&args, "export_format")
                .cloned()
                .unwrap_or_else(|| json!("csv")),
            max_rows: opt(&args, "export_max_rows")
                .cloned()
                .unwrap_or_else(|| json!(EXPORT_MAX_ROWS)),
        };
        return Ok(Value::Object(export_scan_query(
            input_path,
            export,
            &export_args,
        )));
    }
    let query_args = QueryArgs::from_kwargs(&args);
    Ok(Value::Object(query_core(input_path, &query_args)))
}

const QUERY_PARAMS: [&str; 16] = [
    "input_path",
    "filters",
    "sort",
    "direction",
    "columns",
    "offset",
    "limit",
    "count_timeout_seconds",
    "max_bytes",
    "facets",
    "preset",
    "export",
    "export_format",
    "export_max_rows",
    "issue_check",
    "issue_severity",
];

/// `scan_url_counts`: exact filtered counts of many filter sets in one call.
pub fn scan_url_counts(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let input_path = need_str(
        &args,
        "input_path",
        "scan-url-counts",
        &["input_path", "filter_sets", "count_timeout_seconds"],
    )?;
    let Some(filter_sets) = opt(&args, "filter_sets").cloned() else {
        return Err(missing_option(
            "scan-url-counts",
            "filter_sets",
            &["input_path", "filter_sets", "count_timeout_seconds"],
        ));
    };
    let timeout = opt_float(&args, "count_timeout_seconds").unwrap_or(30.0);
    Ok(Value::Object(counts_core(
        input_path,
        &filter_sets,
        timeout,
    )))
}

/// `scan_graph`: bounded internal graph chunks, never fetching.
pub fn scan_graph(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let Some(input_path) = opt_str(&args, "input_path").map(str::to_owned) else {
        return Err(missing_option("scan-graph", "input_path", &GRAPH_PARAMS));
    };
    let max_bytes = opt_int(&args, "max_bytes").unwrap_or(8 * 1024 * 1024);
    let guard = |error: String| -> Value {
        json!({
            "ok": false,
            "state": "unavailable",
            "error": error,
            "reason_code": "invalid_graph_request",
        })
    };
    if !(4096..=8 * 1024 * 1024).contains(&max_bytes) {
        return Ok(guard("max_bytes must be 4096..8388608".to_owned()));
    }
    let section = opt_str(&args, "section").map(str::to_owned);
    let around = opt_str(&args, "around").map(str::to_owned);
    let compare_to = opt_str(&args, "compare_to")
        .map(Path::new)
        .map(Path::to_path_buf);
    let query = GraphQuery {
        positions: opt(&args, "positions").cloned(),
        section: section.as_deref(),
        around: around.as_deref(),
        depth: opt_int(&args, "depth").unwrap_or(2),
        aggregate: opt_str(&args, "aggregate").unwrap_or("none"),
        representation: opt_str(&args, "representation").unwrap_or("all"),
        node_offset: opt_int(&args, "node_offset").unwrap_or(0),
        edge_offset: opt_int(&args, "edge_offset").unwrap_or(0),
        node_limit: opt_int(&args, "node_limit").unwrap_or(1000),
        edge_limit: opt_int(&args, "edge_limit").unwrap_or(2000),
        max_nodes: opt_int(&args, "max_nodes").unwrap_or(5000),
        max_edges: opt_int(&args, "max_edges").unwrap_or(20000),
        compare_to: compare_to.as_deref(),
        timeout_seconds: opt_float(&args, "timeout_seconds").unwrap_or(1.0),
    };
    match graph_core(Path::new(&input_path), &query) {
        Ok(result) => {
            let size = spiderhead_core::common::python_json(&Value::Object(result.clone())).len();
            if size > max_bytes as usize {
                return Ok(json!({
                    "ok": false,
                    "format": result.get("format").cloned().unwrap_or(Value::Null),
                    "state": "limit_reached",
                    "reason_code": "output_byte_limit_exceeded",
                    "max_bytes": max_bytes,
                }));
            }
            Ok(Value::Object(result))
        }
        Err(error) => Ok(guard(super::plain(&error))),
    }
}

const GRAPH_PARAMS: [&str; 16] = [
    "input_path",
    "positions",
    "section",
    "around",
    "depth",
    "aggregate",
    "representation",
    "node_offset",
    "edge_offset",
    "node_limit",
    "edge_limit",
    "max_nodes",
    "max_edges",
    "compare_to",
    "timeout_seconds",
    "max_bytes",
];

/// `scan_directory_tree`: bounded URL-path directory summary.
pub fn scan_directory_tree(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let Some(input_path) = opt_str(&args, "input_path").map(str::to_owned) else {
        return Err(missing_option(
            "scan-directory-tree",
            "input_path",
            &TREE_PARAMS,
        ));
    };
    let built = tree_core::scan_directory_tree(
        Path::new(&input_path),
        opt_int(&args, "max_depth").unwrap_or(3),
        opt_int(&args, "max_children").unwrap_or(50),
        opt_int(&args, "max_nodes").unwrap_or(10_000),
        opt_int(&args, "max_bytes").unwrap_or(1_048_576),
        opt_float(&args, "timeout_seconds").unwrap_or(15.0),
    );
    match built {
        Ok(core) => {
            let mut out = Map::new();
            out.insert("ok".into(), json!(true));
            out.extend(core);
            Ok(Value::Object(out))
        }
        Err(error) => Ok(json!({
            "ok": false,
            "state": "invalid",
            "reason_code": "invalid_directory_tree",
            "error": super::plain(&error),
        })),
    }
}

const TREE_PARAMS: [&str; 6] = [
    "input_path",
    "max_depth",
    "max_children",
    "max_nodes",
    "max_bytes",
    "timeout_seconds",
];

/// `scan_inspect`: read any allowed scan table with bounded rows.
pub fn scan_inspect(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let input_path = need_str(
        &args,
        "input_path",
        "scan-inspect",
        &[
            "input_path",
            "table",
            "offset",
            "limit",
            "max_bytes",
            "columns",
            "total",
        ],
    )?;
    let columns: Option<Vec<String>> = opt(&args, "columns").and_then(|value| {
        value.as_array().map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
    });
    inspect_scan(
        Path::new(input_path),
        opt_str(&args, "table").unwrap_or("pages"),
        opt_int(&args, "offset").unwrap_or(0),
        opt_int(&args, "limit").unwrap_or(100),
        opt_int(&args, "max_bytes").unwrap_or(1_048_576),
        columns.as_deref(),
        opt_bool(&args, "total").unwrap_or(false),
    )
    .map(Value::Object)
    .map_err(failed)
}

/// `scan_list`: the retained-scan catalogue of one directory — or of a
/// project's `scans/` subdirectory when `project` names a workspace.
pub fn scan_list(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let mut directory = opt_str(&args, "directory").map(str::to_owned);
    if let Some(project) = opt_str(&args, "project") {
        let opened = spiderhead_projects::workspace::open_project(Path::new(project), None)
            .map_err(|error| ToolError::Failed(error.to_string()))?;
        // `directory or <project>/scans` — an empty directory falls through too.
        if directory.as_deref().unwrap_or_default().is_empty() {
            directory = opened
                .get("path")
                .and_then(Value::as_str)
                .map(|root| format!("{root}/scans"));
        }
    }
    let Some(directory) = directory else {
        return Err(ToolError::Failed(
            "directory or project is required".to_owned(),
        ));
    };
    let offset = opt_int(&args, "offset").unwrap_or(0);
    let limit = opt_int(&args, "limit").unwrap_or(100);
    spiderhead_store::history::list_scans(Path::new(&directory), offset, limit)
        .map(Value::Object)
        .map_err(failed)
}

/// `scan_body_read`: one bounded retained body window by exact URL.
pub fn scan_body_read(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let input_path = need_str(
        &args,
        "input_path",
        "scan-body-read",
        &[
            "input_path",
            "url",
            "representation",
            "variant_key",
            "offset",
            "length",
            "max_decoded_bytes",
        ],
    )?;
    let Some(url) = opt_str(&args, "url").map(str::to_owned) else {
        return Err(missing_option(
            "scan-body-read",
            "url",
            &[
                "input_path",
                "url",
                "representation",
                "variant_key",
                "offset",
                "length",
                "max_decoded_bytes",
            ],
        ));
    };
    spiderhead_store::body_read::scan_body_read(
        input_path,
        &url,
        opt_str(&args, "representation").unwrap_or("static"),
        opt_str(&args, "variant_key"),
        opt_int(&args, "offset").unwrap_or(0),
        opt_int(&args, "length").unwrap_or(4096),
        opt_int(&args, "max_decoded_bytes").unwrap_or(8 * 1024 * 1024),
    )
    .map(Value::Object)
    .map_err(failed)
}

/// `scan_export`: export retained scan data under the `scan_export.v1` contract.
pub fn scan_export(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    const EXPORT_PARAMS: [&str; 5] = ["input_path", "out", "format", "records", "fields"];
    let input_path = need_str(&args, "input_path", "scan-export", &EXPORT_PARAMS)?;
    let out = opt_str(&args, "out")
        .map(Path::new)
        .map(Path::to_path_buf)
        .ok_or_else(|| missing_option("scan-export", "out", &EXPORT_PARAMS))?;
    let fmt = opt_str(&args, "format").unwrap_or("json");
    let records = opt(&args, "records").cloned().unwrap_or(Value::Null);
    let fields = opt(&args, "fields").cloned().unwrap_or(Value::Null);
    Ok(Value::Object(export_scan_data(
        &json!(input_path),
        Some(out.as_path()),
        fmt,
        &records,
        &fields,
    )))
}

/// `scan_findings_page`: browse one retained scan's findings through a project.
pub fn scan_findings_page(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let Some(project) = opt_str(&args, "project").map(str::to_owned) else {
        return Err(missing_option(
            "scan-findings-page",
            "project",
            &[
                "project",
                "scan_uuid",
                "offset",
                "limit",
                "query",
                "url",
                "sort",
                "descending",
            ],
        ));
    };
    spiderhead_store::finding_page::findings_page(
        &project,
        opt_str(&args, "scan_uuid"),
        opt_int(&args, "offset").unwrap_or(0),
        opt_int(&args, "limit").unwrap_or(50),
        opt_str(&args, "query").unwrap_or(""),
        opt_str(&args, "url"),
        opt_str(&args, "sort").unwrap_or("severity"),
        opt_bool(&args, "descending").unwrap_or(false),
    )
    .map(Value::Object)
    .map_err(failed)
}

/// `duplicate_check`: exact and near-duplicate groups, from `items[]` or a scan's corpus.
pub fn duplicate_check(args: &Value) -> Result<Value, ToolError> {
    let args = super::kwargs(args);
    let scan = opt_str(&args, "scan").map(str::to_owned);
    let items = opt(&args, "items").cloned().unwrap_or(Value::Null);
    if items.is_array() && scan.is_some() {
        return Err(ToolError::Failed(
            "items[] and scan are mutually exclusive".to_owned(),
        ));
    }
    let threshold = opt_float(&args, "threshold").unwrap_or(0.92);
    let with_fingerprints = opt_bool(&args, "with_fingerprints").unwrap_or(false);
    let only_indexable = opt_bool(&args, "only_indexable").unwrap_or(true);
    if let Some(scan) = scan {
        let corpus = corpus_inputs::scan_corpus(&scan, "duplicate").map_err(failed)?;
        let coverage = corpus
            .get("coverage")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if coverage.get("state").and_then(Value::as_str) == Some("unavailable") {
            let mut out = Map::new();
            out.insert("ok".into(), json!(false));
            out.extend(corpus_inputs::corpus_public(&corpus, 0, ""));
            return Ok(Value::Object(out));
        }
        let items: Vec<spiderhead_parse::duplicate::DuplicateItem> =
            serde_json::from_value(corpus.get("items").cloned().unwrap_or(Value::Null))
                .map_err(|error| ToolError::Failed(format!("corpus items are invalid: {error}")))?;
        let options = spiderhead_parse::duplicate::DuplicateOptions {
            threshold,
            k: 3,
            with_fingerprints,
            only_indexable,
            max_shingles: Some(corpus_inputs::MAX_DUPLICATE_SHINGLES as usize),
            max_candidate_comparisons: Some(
                corpus_inputs::MAX_DUPLICATE_CANDIDATE_COMPARISONS as usize,
            ),
        };
        let report = spiderhead_parse::duplicate::find_duplicates(&items, &options)
            .map_err(|error| ToolError::Failed(error.to_string()))?;
        let analyzed = report.count as i64;
        let mut out = python_report(&report, with_fingerprints, false);
        out.extend(corpus_inputs::corpus_public(&corpus, analyzed, ""));
        return Ok(Value::Object(out));
    }
    if !items.is_array() || items.as_array().is_some_and(Vec::is_empty) {
        return Err(ToolError::Failed(
            "items[] required (list of {id, text})".to_owned(),
        ));
    }
    let parsed: Vec<spiderhead_parse::duplicate::DuplicateItem> = serde_json::from_value(items)
        .map_err(|error| ToolError::Failed(format!("items are invalid: {error}")))?;
    let options = spiderhead_parse::duplicate::DuplicateOptions {
        threshold,
        k: 3,
        with_fingerprints,
        only_indexable,
        max_shingles: None,
        max_candidate_comparisons: None,
    };
    let report = spiderhead_parse::duplicate::find_duplicates(&parsed, &options)
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    Ok(Value::Object(python_report(
        &report,
        with_fingerprints,
        parsed.is_empty(),
    )))
}

/// The duplicate report in the reference `find_duplicates` result key order.
///
/// A non-empty input always builds the measured-branch literal (with
/// `measured`/`reason` appended by value); only a truly empty input takes the
/// early-return shape that names `measured` before the counters.
fn python_report(
    report: &spiderhead_parse::duplicate::DuplicateReport,
    with_fingerprints: bool,
    empty_input: bool,
) -> Map<String, Value> {
    let mut out = Map::new();
    let clusters = serde_json::to_value(&report.clusters).unwrap_or(Value::Null);
    let exact = serde_json::to_value(&report.exact_duplicates).unwrap_or(Value::Null);
    if empty_input {
        out.insert("ok".into(), json!(true));
        out.insert("measured".into(), json!(false));
        if let Some(reason) = &report.reason {
            out.insert("reason".into(), json!(reason));
        }
        out.insert("count".into(), json!(report.count));
        out.insert(
            "candidate_pairs_checked".into(),
            json!(report.candidate_pairs_checked),
        );
        out.insert("clusters".into(), clusters);
        out.insert("exact_duplicates".into(), exact);
        out.insert(
            "excluded_non_indexable".into(),
            json!(report.excluded_non_indexable),
        );
        out.insert("excluded_no_id".into(), json!(report.excluded_no_id));
        out.insert("excluded_no_text".into(), json!(report.excluded_no_text));
        out.insert(
            "excluded_duplicate_id".into(),
            json!(report.excluded_duplicate_id),
        );
    } else {
        out.insert("ok".into(), json!(report.ok));
        out.insert("count".into(), json!(report.count));
        if let Some(threshold) = report.threshold {
            out.insert("threshold".into(), json!(threshold));
        }
        out.insert("clusters".into(), clusters);
        out.insert("exact_duplicates".into(), exact);
        out.insert(
            "candidate_pairs_checked".into(),
            json!(report.candidate_pairs_checked),
        );
        out.insert(
            "excluded_non_indexable".into(),
            json!(report.excluded_non_indexable),
        );
        out.insert("excluded_no_id".into(), json!(report.excluded_no_id));
        out.insert("excluded_no_text".into(), json!(report.excluded_no_text));
        out.insert(
            "excluded_duplicate_id".into(),
            json!(report.excluded_duplicate_id),
        );
        out.insert("measured".into(), json!(report.measured));
        if !report.measured {
            if let Some(reason) = &report.reason {
                out.insert("reason".into(), json!(reason));
            }
        }
    }
    if with_fingerprints {
        if let Some(fingerprints) = &report.fingerprints {
            out.insert(
                "fingerprints".into(),
                serde_json::to_value(fingerprints).unwrap_or(Value::Null),
            );
        } else {
            out.insert("fingerprints".into(), json!({}));
        }
    }
    out
}

// ── Offline reanalysis ───────────────────────────────────────────────────

/// `scan_reanalyze`: derive a fresh scan by re-parsing retained evidence
/// without any network work, then audit the derived artifact.
pub fn scan_reanalyze(args: &Value) -> Result<Value, ToolError> {
    use spiderhead_crawl::reanalysis::iterate_reparsed_pages;
    use spiderhead_store::reanalysis::{derived_scan, replace_reparsed_page};

    let args = super::kwargs(args);
    const ACCEPTED: [&str; 5] = [
        "input_path",
        "out",
        "producer_build",
        "step",
        "max_documents",
    ];
    let input_path = super::need_str(&args, "input_path", "scan-reanalyze", &ACCEPTED)?;
    let out = super::need_str(&args, "out", "scan-reanalyze", &ACCEPTED)?;
    let step = super::opt_str(&args, "step").unwrap_or("all");
    if !["all", "links.position"].contains(&step) {
        return Err(ToolError::Failed(
            "step must be all or links.position".to_owned(),
        ));
    }
    let max_documents = super::opt_int(&args, "max_documents");
    if let Some(max) = max_documents {
        if step != "links.position" || max < 1 {
            return Err(ToolError::Failed(
                "max_documents is a positive batch budget for step=links.position".to_owned(),
            ));
        }
    }
    const VERSION: &str = "3.2.0";
    let revision = match super::opt_str(&args, "producer_build") {
        Some(build) => {
            if build.len() != 40 || !build.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()) {
                return Err(ToolError::Failed(
                    "producer_build must be a full lowercase Git commit SHA".to_owned(),
                ));
            }
            build.to_owned()
        }
        None => {
            return Err(ToolError::Failed(
                "no verified source checkout or packaged producer metadata; pass producer_build explicitly"
                    .to_owned(),
            ))
        }
    };
    let mut runtime = Map::new();
    runtime.insert("python".into(), json!("-"));
    runtime.insert("sqlite".into(), json!(rusqlite::version()));
    runtime.insert("httpx".into(), json!("-"));
    runtime.insert("lxml".into(), json!("-"));
    runtime.insert("beautifulsoup4".into(), json!("-"));

    let source_path = Path::new(input_path);
    let mut derived = derived_scan(source_path, Path::new(out), VERSION, &revision, &runtime)
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    let (writer, source) = derived.split();
    let settings = spiderhead_store::scan_schema::read_effective_config(source)
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    let captured_settings = settings.clone();
    let list_mode: bool = source
        .query_row(
            "SELECT 1 FROM context_items WHERE kind='list_mode' AND item_key='run'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|error| ToolError::Failed(error.to_string()))?
        .is_some();
    let parent: Map<String, Value> = source
        .query_row(
            "SELECT scan_uuid,writer_version,writer_revision,crawl_partial,corpus_partial \
             FROM scan WHERE singleton=1",
            [],
            |row| {
                Ok(Map::from_iter([
                    ("scan_uuid".into(), json!(row.get::<_, String>(0)?)),
                    ("writer_version".into(), json!(row.get::<_, String>(1)?)),
                    ("writer_revision".into(), json!(row.get::<_, String>(2)?)),
                    ("crawl_partial".into(), json!(row.get::<_, i64>(3)?)),
                    ("corpus_partial".into(), json!(row.get::<_, i64>(4)?)),
                ]))
            },
        )
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    let pages: i64 = source
        .query_row("SELECT COUNT(*) FROM pages", [], |row| row.get(0))
        .map_err(|error| ToolError::Failed(error.to_string()))?;

    let mut start_gate: Option<Map<String, Value>> = None;
    if step == "all" {
        let replay = iterate_reparsed_pages(source, &settings)
            .map_err(|error| ToolError::Failed(error.to_string()))?;
        for page in &replay.pages {
            replace_reparsed_page(writer, page)
                .map_err(|error| ToolError::Failed(error.to_string()))?;
        }
        start_gate = replay.start_page_gate.clone();
        for page in &replay.pages {
            replace_reparsed_page(writer, page)
                .map_err(|error| ToolError::Failed(error.to_string()))?;
        }
    }
    let body_limit = settings
        .get("storage")
        .and_then(|storage| storage.get("max_body_bytes"))
        .and_then(Value::as_i64)
        .unwrap_or(8 * 1024 * 1024);
    let connection = writer
        .try_con()
        .map_err(|e| ToolError::Failed(e.to_string()))?;
    let link_positions = spiderhead_store::link_positions::backfill_positions(
        connection,
        max_documents,
        body_limit,
        step != "all",
    )
    .map_err(|error| ToolError::Failed(error.to_string()))?;
    let _ = connection;
    if step == "links.position" && !list_mode {
        // The start-page gate rebuilds from the source's active static document.
        if let Some(gate) = start_gate_from_source(source, body_limit) {
            start_gate = Some(gate);
        }
    }
    let mut settings = settings;
    if let Some(link_position) = settings
        .get_mut("link_position")
        .and_then(Value::as_object_mut)
    {
        link_position.insert("classify".into(), json!(true));
    }

    let mut reason = String::new();
    if start_gate.is_none() && !list_mode {
        reason = "reanalysis unavailable: start-page raw evidence is not_in_corpus".to_owned();
    } else {
        let scan_path = writer.path().to_path_buf();
        let audit =
            spiderhead_checks::audit::audit_v2_document(&scan_path).map_err(ToolError::Failed);
        match audit {
            Ok((mut header, collections)) => {
                let run = header
                    .get("run")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                let mut run = run;
                let host = spiderhead_core::host::SystemHost;
                let manifest = spiderhead_core::crawl_settings::manifest(
                    &Value::Object(captured_settings.clone()),
                    &host,
                )
                .map_err(|error| ToolError::Failed(error.to_string()))?;
                run.insert("crawl_config".into(), json!(manifest));
                let provenance_payload: String = writer
                    .try_con()
                    .map_err(|error| ToolError::Failed(error.to_string()))?
                    .query_row(
                        "SELECT payload_json FROM context_items \
                         WHERE kind='reanalysis_provenance' AND item_key='run'",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|error| ToolError::Failed(error.to_string()))?;
                let provenance: Map<String, Value> = serde_json::from_str(&provenance_payload)
                    .map_err(|error| ToolError::Failed(error.to_string()))?;
                run.insert(
                    "reanalysis".into(),
                    json!({
                        "parent_scan_uuid": provenance.get("parent_scan_uuid"),
                        "parent_writer_version": parent.get("writer_version"),
                        "parent_writer_revision": parent.get("writer_revision"),
                        "analyzer_revision": revision,
                        "network": "disabled",
                        "step": step,
                        "link_positions": link_positions,
                        "analysis_policy": {"link_position.classify": true},
                    }),
                );
                header.insert("run".into(), Value::Object(run));
                match writer.save_audit_v2(&header, &collections) {
                    Ok(()) => {}
                    Err(spiderhead_store::error::StoreError::AuditSizeError(error)) => {
                        reason = error.to_string();
                    }
                    Err(error) => return Err(ToolError::Failed(error.to_string())),
                }
            }
            Err(error) => return Err(error),
        }
    }
    if !reason.is_empty() {
        writer
            .note_audit_unavailable(&reason)
            .map_err(|error| ToolError::Failed(error.to_string()))?;
    }
    let header: Map<String, Value> = writer
        .try_con()
        .map_err(|error| ToolError::Failed(error.to_string()))?
        .query_row(
            "SELECT scan_uuid,crawl_partial,corpus_partial,capabilities_json FROM scan WHERE singleton=1",
            [],
            |row| {
                Ok(Map::from_iter([
                    ("scan_uuid".into(), json!(row.get::<_, String>(0)?)),
                    ("crawl_partial".into(), json!(row.get::<_, i64>(1)?)),
                    ("corpus_partial".into(), json!(row.get::<_, i64>(2)?)),
                    (
                        "capabilities_json".into(),
                        json!(row.get::<_, String>(3)?),
                    ),
                ]))
            },
        )
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    let scan_path = derived
        .publish()
        .map_err(|error| ToolError::Failed(error.to_string()))?;
    let capabilities: Value = serde_json::from_str(
        header
            .get("capabilities_json")
            .and_then(Value::as_str)
            .unwrap_or("{}"),
    )
    .unwrap_or(json!({}));
    Ok(json!({
        "ok": true,
        "scan": scan_path.to_string_lossy(),
        "scan_uuid": header.get("scan_uuid"),
        "parent_scan_uuid": parent.get("scan_uuid"),
        "source_kind": "reanalysis",
        "analyzer_revision": revision,
        "pages": pages,
        "link_positions": link_positions,
        "audit_available": reason.is_empty(),
        "audit_reason": reason,
        "crawl_partial": header.get("crawl_partial").and_then(Value::as_i64).unwrap_or(0) != 0,
        "corpus_partial": header.get("corpus_partial").and_then(Value::as_i64).unwrap_or(0) != 0,
        "capabilities": capabilities,
    }))
}

/// The start-page evidence gate for a positions-only pass, rebuilt from the
/// source's active static document.
fn start_gate_from_source(
    source: &rusqlite::Connection,
    body_limit: i64,
) -> Option<Map<String, Value>> {
    let start_page = source
        .query_row(
            "SELECT p.*,u.url FROM pages p JOIN urls u USING(url_id) WHERE u.url=\
             (SELECT start_url FROM scan WHERE singleton=1)",
            [],
            |row| {
                let mut map = Map::new();
                for column in [
                    "outlinks",
                    "external_outlinks",
                    "url_id",
                    "representation",
                    "document_id",
                ] {
                    let value: Value = match row.get_ref(column) {
                        Ok(rusqlite::types::ValueRef::Integer(n)) => json!(n),
                        Ok(rusqlite::types::ValueRef::Text(text)) => {
                            json!(String::from_utf8_lossy(text).into_owned())
                        }
                        _ => Value::Null,
                    };
                    map.insert(column.to_string(), value);
                }
                Ok(map)
            },
        )
        .optional()
        .ok()
        .flatten()?;
    let document_id = match start_page.get("representation").and_then(Value::as_str) {
        Some("static") => start_page.get("document_id").and_then(Value::as_i64),
        _ => None,
    }?;
    let html = spiderhead_store::bodies::read_document(source, document_id, body_limit).ok()?;
    Some(Map::from_iter([
        ("html".into(), json!(html)),
        (
            "outlinks".into(),
            start_page.get("outlinks").cloned().unwrap_or(json!(0)),
        ),
        (
            "external_outlinks".into(),
            start_page
                .get("external_outlinks")
                .cloned()
                .unwrap_or(json!(0)),
        ),
    ]))
}
