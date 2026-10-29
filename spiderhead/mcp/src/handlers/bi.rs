//! Handlers of the BI delivery group: typed package export, selected-view
//! filtering, destination plans, cohort and progress packages. Everything runs
//! on local files; destination writes go only through an authorized host client.

use std::path::Path;

use serde_json::{json, Map, Value};

use spiderhead_core::host::SystemHost;
use spiderhead_reports::bi::project::ExportLimits;
use spiderhead_reports::bi_destinations as destinations;

use crate::error::ToolError;

use super::{kwargs, missing_option, need_str, opt, opt_bool, opt_int, opt_str};

/// A reports-domain failure with the plain message the reference `ValueError` carries.
fn failed(error: impl std::fmt::Display) -> ToolError {
    ToolError::Failed(error.to_string())
}

fn limit(args: &Map<String, Value>, name: &str, default: i64) -> i64 {
    opt_int(args, name).unwrap_or(default)
}

/// `bi_filter`: a bounded local CSV view of declared fields, plus optional XLSX.
pub fn bi_filter(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 10] = [
        "package",
        "dataset",
        "out_dir",
        "where",
        "columns",
        "max_rows_per_file",
        "max_bytes_per_file",
        "max_output_bytes",
        "xlsx_out",
        "xlsx_max_rows_per_sheet",
    ];
    let package = need_str(&args, "package", "bi-filter", &ACCEPTED)?;
    let dataset = need_str(&args, "dataset", "bi-filter", &ACCEPTED)?;
    let out_dir = need_str(&args, "out_dir", "bi-filter", &ACCEPTED)?;
    let where_ = opt(&args, "where").and_then(Value::as_object);
    let columns: Option<Vec<String>> =
        opt(&args, "columns").and_then(Value::as_array).map(|list| {
            list.iter()
                .map(|item| item.as_str().unwrap_or_default().to_owned())
                .collect()
        });
    let max_output_bytes = limit(&args, "max_output_bytes", 4 * 1024 * 1024 * 1024);
    let mut result = destinations::filter_package(
        Path::new(package),
        dataset,
        Path::new(out_dir),
        where_,
        columns.as_deref(),
        limit(&args, "max_rows_per_file", 250_000) as usize,
        limit(&args, "max_bytes_per_file", 8 * 1024 * 1024) as u64,
        max_output_bytes as u64,
    )
    .map_err(failed)?;
    if let Some(xlsx_out) = opt_str(&args, "xlsx_out") {
        let xlsx = destinations::export_bi_xlsx(
            Path::new(out_dir),
            dataset,
            Path::new(xlsx_out),
            limit(&args, "xlsx_max_rows_per_sheet", 1_048_575),
            max_output_bytes as u64,
        )
        .map_err(failed)?;
        if let Some(object) = result.as_object_mut() {
            object.insert("xlsx".into(), xlsx);
        }
    }
    Ok(result)
}

/// `bi_export`: typed, partitioned BI datasets from saved local crawl evidence.
pub fn bi_export(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let provider_joins: Vec<String> = opt(&args, "provider_joins")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|item| item.as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default();
    let limits = ExportLimits {
        max_rows_per_file: limit(&args, "max_rows_per_file", 25_000) as usize,
        max_bytes_per_file: limit(&args, "max_bytes_per_file", 8 * 1024 * 1024) as u64,
        max_output_bytes: limit(&args, "max_output_bytes", 4 * 1024 * 1024 * 1024) as u64,
        max_scan_bytes: limit(&args, "max_scan_bytes", 8 * 1024 * 1024 * 1024) as u64,
        search_metric: opt_str(&args, "search_metric").map(str::to_owned),
    };
    let result = spiderhead_reports::bi::export_bi(
        opt_str(&args, "scan").map(Path::new),
        opt(&args, "audit"),
        &provider_joins,
        opt_str(&args, "out_dir").map(Path::new),
        limits,
    )
    .map_err(failed)?;
    let xlsx_out = opt_str(&args, "xlsx_out");
    let xlsx_dataset = opt_str(&args, "xlsx_dataset");
    if xlsx_out.is_some() != xlsx_dataset.is_some() {
        return Err(ToolError::Failed(
            "xlsx_out and xlsx_dataset must be supplied together".to_owned(),
        ));
    }
    let xlsx = match (xlsx_out, xlsx_dataset) {
        (Some(out), Some(dataset)) => destinations::export_bi_xlsx(
            Path::new(result["output_directory"].as_str().unwrap_or_default()),
            dataset,
            Path::new(out),
            limit(&args, "xlsx_max_rows_per_sheet", 1_048_575),
            4 * 1024 * 1024 * 1024,
        )
        .map_err(failed)?,
        _ => Value::Null,
    };
    let mut document = json!({"ok": true});
    merge(&mut document, &result);
    document["xlsx"] = xlsx;
    Ok(document)
}

/// `bi_sheets_plan`: a credential-free Sheets preflight of a complete package.
pub fn bi_sheets_plan(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let package = need_str(
        &args,
        "package",
        "bi-sheets-plan",
        &["package", "max_cells"],
    )?;
    let plan = destinations::sheets_plan(Path::new(package), limit(&args, "max_cells", 10_000_000))
        .map_err(failed)?;
    Ok(with_ok(&plan))
}

/// `bi_bigquery_plan`: a load description with no project, credential or write.
pub fn bi_bigquery_plan(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 3] = ["package", "dataset", "operation"];
    let package = need_str(&args, "package", "bi-bigquery-plan", &ACCEPTED)?;
    let dataset = need_str(&args, "dataset", "bi-bigquery-plan", &ACCEPTED)?;
    let plan = destinations::bigquery_plan(
        Path::new(package),
        dataset,
        opt_str(&args, "operation").unwrap_or("replace"),
    )
    .map_err(failed)?;
    Ok(with_ok(&plan))
}

/// `bi_destination_apply`: preview, or apply through the authorized host client.
pub fn bi_destination_apply(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 6] = [
        "package",
        "target",
        "destination",
        "operation",
        "apply",
        "reconcile",
    ];
    let package = need_str(&args, "package", "bi-destination-apply", &ACCEPTED)?;
    let target = need_str(&args, "target", "bi-destination-apply", &ACCEPTED)?;
    let destination = need_str(&args, "destination", "bi-destination-apply", &ACCEPTED)?;
    let Some(operation) = opt_str(&args, "operation") else {
        return Err(missing_option(
            "bi-destination-apply",
            "operation",
            &ACCEPTED,
        ));
    };
    if !matches!(destination, "sheets" | "bigquery") {
        return Err(ToolError::Failed(
            "destination must be 'sheets' or 'bigquery'".to_owned(),
        ));
    }
    let apply = opt_bool(&args, "apply").unwrap_or(false);
    let reconcile = opt_bool(&args, "reconcile").unwrap_or(false);
    if reconcile && !apply {
        return Err(ToolError::Failed(
            "reconcile requires apply=true for the reviewed destination target".to_owned(),
        ));
    }
    let package = Path::new(package);
    if !apply {
        let preview =
            destinations::destination_preview(&SystemHost, package, target, destination, operation)
                .map_err(failed)?;
        return Ok(with_ok(&preview));
    }
    let client =
        destinations::resolve_host_client(&SystemHost, destination, target).map_err(failed)?;
    let applied = destinations::apply_with_client(
        package,
        target,
        operation,
        client.as_ref(),
        apply,
        reconcile,
    )
    .map_err(failed)?;
    let mut document = json!({"ok": true, "destination": destination});
    merge(&mut document, &applied);
    Ok(document)
}

/// `publication_cohorts`: an offline publication-cohort package from saved evidence.
pub fn publication_cohorts(args: &Value) -> Result<Value, ToolError> {
    cohort(args, spiderhead_reports::cohorts::publication_cohorts)
}

/// `gsc_progress`: an offline branded/non-branded progress package.
pub fn gsc_progress(args: &Value) -> Result<Value, ToolError> {
    cohort(args, spiderhead_reports::cohorts::gsc_progress)
}

type CohortFn = fn(
    Option<&Value>,
    Option<&str>,
    Option<&str>,
) -> Result<Value, spiderhead_reports::cohorts::CohortProjectionError>;

fn cohort(args: &Value, build: CohortFn) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let result = build(
        opt(&args, "document"),
        opt_str(&args, "file"),
        opt_str(&args, "out_dir"),
    )
    .map_err(failed)?;
    Ok(with_ok(&result))
}

/// `{"ok": true, **document}` — the reference `{"ok": True, **result}`.
fn with_ok(document: &Value) -> Value {
    let mut out = json!({"ok": true});
    merge(&mut out, document);
    out
}

fn merge(into: &mut Value, from: &Value) {
    if let (Some(out), Some(object)) = (into.as_object_mut(), from.as_object()) {
        for (key, value) in object {
            out.insert(key.clone(), value.clone());
        }
    }
}
