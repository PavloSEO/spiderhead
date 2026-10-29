//! Format dispatcher for audit reports: loads the audit, checks its contract,
//! then hands the normalized projection to the per-format writers.
//!
//! Every path returns the same `{"ok": ..., ...}` object; failures are data,
//! not exceptions.
//!
//! `project` and `view` bind through an injected [`ProjectsHost`]: the projects
//! crate depends on this one, so the workspace reads arrive as callables from
//! the handler layer. Without a host the arguments still return the boundary
//! error below.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use spiderhead_store::audit_v2::AuditV2Reader;
use spiderhead_store::inputs::{protects_scan_input, AuditSource};

use crate::client_findings::project_document;
use crate::csvfile::CsvError;
use crate::document::{
    self, detect_kind, normalize_sf_audit, safe_segment, AuditV2ReportView, SF_AUDIT_SCHEMA_VERSION,
};
use crate::docx::DocxError;
use crate::pdf_model::PdfModelError;
use crate::project_coverage::{
    attach_snapshot, load_snapshot, protected_destination, ProjectsHost,
};
use crate::xlsx::XlsxReportError;
use crate::{csvfile, docx, json_out, md, pdf_model, pdf_output, pdf_stream, py, xlsx};

/// The projects-crate boundary shared by the `project`/`view` arguments: the
/// caller did not inject a [`ProjectsHost`], so binding cannot run.
const PROJECTS_UNAVAILABLE: &str =
    "project-bound reports and saved finding views require an injected projects host";

fn err(message: impl Into<String>) -> Value {
    json!({"ok": false, "error": message.into()})
}

fn ok(result: &Value) -> bool {
    result.get("ok") == Some(&json!(true))
}

/// Type name of a JSON value as shown in error messages (`NoneType`, `dict`, `list`, ...).
fn py_type_name(value: &Value) -> &'static str {
    match value {
        Value::Object(_) => "dict",
        Value::Array(_) => "list",
        Value::String(_) => "str",
        Value::Bool(_) => "bool",
        Value::Null => "NoneType",
        Value::Number(n) => {
            if n.is_f64() {
                "float"
            } else {
                "int"
            }
        }
    }
}

/// Display text for a value: `None` for null, otherwise its string form.
fn str_f(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        other => py::str_value(other),
    }
}

/// Exception class name a writer failure reports: I/O is `OSError`, contract and
/// validation failures are `ValueError`, and third-party engines name their own.
fn exc_text(error: impl std::fmt::Display, name: &str) -> String {
    format!("{name}: {error}")
}

fn csv_exc(error: CsvError) -> String {
    match error {
        CsvError::Io(e) => exc_text(e, "OSError"),
        CsvError::Csv(e) => exc_text(e, "_csv.Error"),
    }
}

fn docx_exc(error: DocxError) -> String {
    match error {
        DocxError::Io(e) => exc_text(e, "OSError"),
        DocxError::Zip(e) => exc_text(e, "BadZipFile"),
    }
}

fn xlsx_exc(error: XlsxReportError) -> String {
    match error {
        XlsxReportError::Io(e) => exc_text(e, "OSError"),
        XlsxReportError::Value(e) => exc_text(e, "ValueError"),
        XlsxReportError::Xlsx(e) => exc_text(e, "XlsxWriterError"),
    }
}

/// Output path: the given path, or `audit-<domain>.<fmt>`; a bare filename's parent is `.`.
fn ensure_parent(target: &Path) -> Result<(), String> {
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| exc_text(e, "OSError"))
}

/// Size in bytes of the written artifact.
fn file_size(target: &Path) -> u64 {
    std::fs::metadata(target).map(|m| m.len()).unwrap_or(0)
}

/// The sidecar list `result["outputs"]` reports for csv/xlsx.
fn existing_targets(targets: &[PathBuf]) -> Vec<Value> {
    targets
        .iter()
        .filter(|t| t.exists())
        .map(|t| json!(t.display().to_string()))
        .collect()
}

/// audit.v2 inputs never build the compatibility document: findings and pages
/// stream through the report view.
#[allow(clippy::too_many_arguments)]
fn build_audit_v2_report(
    reader: &AuditV2Reader,
    fmt: &str,
    path: Option<&str>,
    project: Option<&str>,
    source: &Value,
    lang: &str,
    pdf_policy: Option<&str>,
    host: Option<&ProjectsHost<'_>>,
) -> Value {
    let header = &reader.header;
    if header.get("schema_version") != Some(&json!(SF_AUDIT_SCHEMA_VERSION)) {
        return err(format!(
            "unsupported or missing 'schema_version' marker {}; Export Analyzer audits require exactly '{}'",
            py::repr(header.get("schema_version").unwrap_or(&Value::Null)),
            SF_AUDIT_SCHEMA_VERSION
        ));
    }
    let mut invalid: Vec<String> = ["run", "summary"]
        .iter()
        .filter(|name| !header.get(**name).is_some_and(Value::is_object))
        .map(|name| (*name).to_owned())
        .collect();
    for name in ["issues", "pages"] {
        if !reader.collections.contains_key(&format!("/{name}")) {
            invalid.push(name.to_owned());
        }
    }
    if !invalid.is_empty() {
        return err(format!(
            "Export Analyzer audit.v2 has invalid or missing containers: {}",
            invalid.join(", ")
        ));
    }
    if fmt == "pdf" && pdf_policy.is_none() {
        return err(
            "streamed audit.v2 PDF requires explicit pdf_policy='overview-v1'; this creates a bounded overview and complete JSON/CSV companions",
        );
    }
    if !["json", "csv", "xlsx", "md", "docx", "pdf"].contains(&fmt) {
        return err(format!(
            "{fmt} output is not implemented for streamed audit.v2 inputs; use json, csv, xlsx, md, or docx"
        ));
    }
    let mut view = match AuditV2ReportView::new(reader) {
        Ok(view) => view,
        Err(e) => return err(e.to_string()),
    };
    let name = view
        .document
        .get("domain")
        .filter(|v| py::truthy(Some(*v)))
        .map(py::str_value)
        .unwrap_or_else(|| "site".to_owned());
    let name = if fmt == "pdf" {
        safe_segment(&json!(name))
    } else {
        name
    };
    let target = path
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("audit-{name}.{fmt}")));
    let mut targets = vec![target.clone()];
    if fmt == "csv" {
        targets.push(target.with_extension("pages.csv"));
        targets.push(target.with_extension("scope.csv"));
        // The coverage companion is written whenever a project snapshot attaches,
        // which is the only way one attaches.
        if project.is_some() {
            targets.push(target.with_extension("coverage.csv"));
        }
    }
    if fmt == "xlsx" {
        targets.push(PathBuf::from(format!("{}.index.json", target.display())));
    }
    if project.is_some() && fmt == "json" {
        return err(
            "streamed audit.v2 JSON is written verbatim and cannot carry project coverage; use md, csv, xlsx, docx or pdf with --project, or build the JSON without --project",
        );
    }
    if let Some(project) = project {
        let Some(host) = host else {
            return err(PROJECTS_UNAVAILABLE);
        };
        let (project_root, snapshot) =
            match load_snapshot(project, &reader.header, "sf-audit", host) {
                Ok(bound) => bound,
                Err(e) => return err(e.to_string()),
            };
        if let Some(error) = protected_destination(Path::new(&project_root), &targets) {
            return err(error);
        }
        view.document = attach_snapshot(&view.document, &snapshot);
    }
    if protects_scan_input(source, &targets) {
        return err("report output must not overwrite its source scan");
    }
    if let Err(e) = ensure_parent(&target) {
        return err(e);
    }
    let mut workbook_result = Map::new();
    let write_result: Result<(), String> = match fmt {
        "pdf" => {
            return match pdf_stream::write_overview(reader, &view, &target, lang) {
                Ok(result) => result,
                Err(e) => err(exc_text(e, "OSError")),
            };
        }
        "json" => reader
            .document_chunks(None)
            .map_err(|e| exc_text(e, "ValueError"))
            .and_then(|chunks| {
                std::fs::write(&target, chunks.concat()).map_err(|e| exc_text(e, "OSError"))
            }),
        "csv" => csvfile::write(&view, &target).map_err(csv_exc),
        "md" => md::write(&view, &target).map_err(|e| exc_text(e, "OSError")),
        "docx" => docx::write(&view, &target).map_err(docx_exc),
        _ => match xlsx::write_stream(&view, &target) {
            Ok(result) => {
                if let Value::Object(extra) = result {
                    workbook_result = extra;
                }
                Ok(())
            }
            Err(e) => Err(xlsx_exc(e)),
        },
    };
    if let Err(e) = write_result {
        return err(e);
    }
    let mut result = json!({
        "ok": true,
        "format": fmt,
        "path": target.display().to_string(),
        "bytes": file_size(&target),
        "findings": view.finding_count,
        "pages": view.page_count,
    });
    if let Value::Object(map) = &mut result {
        map.extend(workbook_result);
    }
    if ["csv", "xlsx"].contains(&fmt) {
        result
            .as_object_mut()
            .map(|m| m.insert("outputs".into(), json!(existing_targets(&targets))));
    }
    result
}

/// Render an audit document in the requested report format.
#[allow(clippy::too_many_arguments)]
fn build_report_inner(
    data: &Value,
    fmt: &str,
    path: Option<&str>,
    project: Option<&str>,
    view: Option<&str>,
    offset: i64,
    lang: &str,
    pdf_policy: Option<&str>,
    host: Option<&ProjectsHost<'_>>,
) -> Value {
    let fmt = {
        let raw = if fmt.is_empty() { "xlsx" } else { fmt };
        raw.to_lowercase().trim_start_matches('.').to_owned()
    };
    if !document::FORMATS.contains(&fmt.as_str()) {
        return err(format!(
            "report format {} is not supported; available formats: {}",
            py::repr_str(&fmt),
            document::FORMATS.join(", ")
        ));
    }
    if lang != "en" && lang != "ru" {
        return err("report language must be 'en' or 'ru'");
    }
    if fmt != "pdf" && lang != "en" {
        return err("report language is only configurable for PDF output");
    }
    if pdf_policy.is_some_and(|p| p != "overview-v1" || fmt != "pdf") {
        return err("pdf_policy supports only 'overview-v1' with PDF output");
    }
    let mut input_diagnostics: Vec<Value> = Vec::new();
    let source = match document::load(data, &mut input_diagnostics) {
        Ok(source) => source,
        Err(e) => return err(e.to_string()),
    };
    match source {
        AuditSource::Streaming(reader) => {
            // audit.v2 documents expose `iter_collection`; their reader closes when this scope ends.
            if view.is_some() {
                return err("saved finding views are not implemented for streamed audit.v2 inputs");
            }
            let mut result =
                build_audit_v2_report(&reader, &fmt, path, project, data, lang, pdf_policy, host);
            if ok(&result) && !input_diagnostics.is_empty() {
                result
                    .as_object_mut()
                    .map(|m| m.insert("input_diagnostics".into(), json!(input_diagnostics)));
            }
            result
        }
        AuditSource::Document(document) => {
            if pdf_policy.is_some() {
                return err("pdf_policy='overview-v1' requires a streamed audit.v2 input");
            }
            if !document.is_object() {
                return err(format!(
                    "audit document must be a JSON object, got {}",
                    py_type_name(&document)
                ));
            }
            let (kind, marker_error) = detect_kind(&document);
            let Some(kind) = kind else {
                return err(marker_error.unwrap_or_else(|| {
                    let mut keys: Vec<String> = document
                        .as_object()
                        .map(|m| m.keys().cloned().collect())
                        .unwrap_or_default();
                    keys.sort();
                    format!(
                        "audit document schema not recognized: expected 'findings' (seohead.site-audit/1) or 'issues'+'pages' (Export Analyzer audit.json); got top-level keys {}",
                        py::repr(&json!(keys))
                    )
                }));
            };
            let mut view_result: Option<Value> = None;
            let mut filtered_document = document.clone();
            if let Some(view_name) = view {
                let Some(project_name) = project else {
                    return err("a project is required to apply a saved finding view");
                };
                let Some(host) = host else {
                    return err(PROJECTS_UNAVAILABLE);
                };
                let (applied, selected_rows) =
                    match (host.apply_view_details)(project_name, view_name, &document, offset) {
                        Ok(applied) => applied,
                        Err(e) => return err(e),
                    };
                let rows_key = applied
                    .get("source")
                    .and_then(|s| s.get("rows_key"))
                    .and_then(Value::as_str)
                    .unwrap_or("findings")
                    .to_owned();
                filtered_document[rows_key] = json!(selected_rows);
                view_result = Some(applied);
            }
            let mut rendered = if kind == "site-audit" {
                filtered_document.clone()
            } else {
                normalize_sf_audit(&filtered_document)
            };
            if let Some(view_result) = &view_result {
                let mut view_fields = view_result
                    .get("view")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                for key in ["state", "counts", "pagination", "columns", "sort", "source"] {
                    view_fields.insert(
                        key.to_owned(),
                        view_result.get(key).cloned().unwrap_or(Value::Null),
                    );
                }
                let finding_view = Value::Object(view_fields);
                let mut summary = rendered
                    .get("summary")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                summary.insert("finding_view".to_owned(), finding_view.clone());
                rendered["summary"] = Value::Object(summary);
                let mut filtered_summary = filtered_document
                    .get("summary")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                filtered_summary.insert("finding_view".to_owned(), finding_view);
                filtered_document["summary"] = Value::Object(filtered_summary);
            }
            let default_name = if fmt == "pdf" && path.is_none() {
                format!(
                    "audit-{}.pdf",
                    safe_segment(rendered.get("domain").unwrap_or(&json!("site")))
                )
            } else {
                format!(
                    "audit-{}.{fmt}",
                    rendered
                        .get("domain")
                        .map(str_f)
                        .unwrap_or_else(|| "site".to_owned())
                )
            };
            let target = path
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(&default_name));
            let mut targets = vec![target.clone()];
            if fmt == "csv" {
                targets.push(target.with_extension("pages.csv"));
                targets.push(target.with_extension("scope.csv"));
            }
            if fmt == "xlsx" {
                targets.push(PathBuf::from(format!("{}.index.json", target.display())));
            }
            let mut project_snapshot = Value::Null;
            if let Some(project) = project {
                let Some(host) = host else {
                    return err(PROJECTS_UNAVAILABLE);
                };
                let (project_root, snapshot) = match load_snapshot(project, &document, kind, host) {
                    Ok(bound) => bound,
                    Err(e) => return err(e.to_string()),
                };
                project_snapshot = snapshot;
                if fmt == "csv" {
                    targets.push(target.with_extension("coverage.csv"));
                }
                if let Some(error) = protected_destination(Path::new(&project_root), &targets) {
                    return err(error);
                }
            }
            if protects_scan_input(data, &targets) {
                return err("report output must not overwrite its source scan");
            }
            if let Err(e) = ensure_parent(&target) {
                return err(e);
            }
            // The human writers render the snapshot-bound document; `json` relays
            // the original document and `pdf` binds its own copy inside the model.
            let rendered = if !matches!(fmt.as_str(), "pdf" | "json") && !project_snapshot.is_null()
            {
                attach_snapshot(&rendered, &project_snapshot)
            } else {
                rendered
            };
            let mut workbook_result = Map::new();
            let write_result: Result<(), String> = match fmt.as_str() {
                "pdf" => {
                    let model = match pdf_model::build_pdf_model(
                        &filtered_document,
                        &input_diagnostics,
                        project,
                        host,
                    ) {
                        Ok(model) => model,
                        Err(PdfModelError(e)) => return err(exc_text(e, "ValueError")),
                    };
                    let mut result = pdf_output::write_pdf_report(&model, &target, lang);
                    if ok(&result) {
                        if view_result.is_some() {
                            let view_block = rendered["summary"]["finding_view"].clone();
                            result
                                .as_object_mut()
                                .map(|m| m.insert("finding_view".into(), view_block));
                        }
                        if !input_diagnostics.is_empty() {
                            result.as_object_mut().map(|m| {
                                m.insert("input_diagnostics".into(), json!(input_diagnostics))
                            });
                        }
                    }
                    return result;
                }
                "json" => {
                    let json_document = match &view_result {
                        Some(view_result) => json!({
                            "schema": "spiderhead.finding-view/1",
                            "view": rendered["summary"]["finding_view"],
                            "source": view_result["source"],
                            "rows": view_result["items"],
                        }),
                        None => document.clone(),
                    };
                    std::fs::write(&target, json_out::dumps(&json_document))
                        .map_err(|e| exc_text(e, "OSError"))
                }
                "xlsx" => match xlsx::write(&project_document(&rendered), &target) {
                    Ok(result) => {
                        if let Value::Object(extra) = result {
                            workbook_result = extra;
                        }
                        Ok(())
                    }
                    Err(e) => Err(xlsx_exc(e)),
                },
                "docx" => docx::write(&project_document(&rendered), &target).map_err(docx_exc),
                "csv" => csvfile::write(&project_document(&rendered), &target).map_err(csv_exc),
                _ => md::write(&project_document(&rendered), &target)
                    .map_err(|e| exc_text(e, "OSError")),
            };
            if let Err(e) = write_result {
                return err(e);
            }
            let mut result = json!({
                "ok": true,
                "format": fmt,
                "path": target.display().to_string(),
                "bytes": file_size(&target),
                "findings": py::get_list(&rendered, "findings").len(),
                "pages": py::get_list(&rendered, "pages").len(),
            });
            if let Value::Object(map) = &mut result {
                map.extend(workbook_result);
            }
            if !input_diagnostics.is_empty() {
                result
                    .as_object_mut()
                    .map(|m| m.insert("input_diagnostics".into(), json!(input_diagnostics)));
            }
            if view_result.is_some() {
                let view_block = rendered["summary"]["finding_view"].clone();
                result
                    .as_object_mut()
                    .map(|m| m.insert("finding_view".into(), view_block));
            }
            if ["csv", "xlsx"].contains(&fmt.as_str()) {
                result
                    .as_object_mut()
                    .map(|m| m.insert("outputs".into(), json!(existing_targets(&targets))));
            }
            result
        }
    }
}

/// Render an audit document in the requested report format.
///
/// `data` is the audit object, or the path to its JSON file, native scan, or
/// audit.v2 companion. `fmt` is one of `xlsx, docx, csv, md, json, pdf`; `path` is
/// the output location, derived from the audited domain when omitted.
///
/// `project` binds the report to a live workspace and `view` applies one saved
/// finding view; both read through `host`, which the handler layer injects from
/// the projects crate. Without a host the arguments return the boundary error.
/// Journal, export-history and action-log bookkeeping after a successful bound
/// build belongs to the caller — it is the same projects seam this crate cannot
/// reach.
#[allow(clippy::too_many_arguments)]
pub fn build_report(
    data: &Value,
    fmt: &str,
    path: Option<&str>,
    project: Option<&str>,
    view: Option<&str>,
    offset: i64,
    lang: &str,
    pdf_policy: Option<&str>,
    host: Option<&ProjectsHost<'_>>,
) -> Value {
    build_report_inner(
        data, fmt, path, project, view, offset, lang, pdf_policy, host,
    )
}
