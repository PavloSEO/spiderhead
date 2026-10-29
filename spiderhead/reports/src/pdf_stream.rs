//! Explicit bounded PDF overview with complete, atomic audit.v2 companions.
//!
//! The PDF displays an ordered prefix only; every record stays in the JSON and CSV
//! companions, published atomically with it or not at all.

use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use spiderhead_core::artifacts::AUDIT_V2;
use spiderhead_store::audit_v2::AuditV2Reader;

use crate::document::AuditV2ReportView;
use crate::pdf_model::{build_pdf_model, collection, coverage_record};

pub const POLICY: &str = "overview-v1";
pub const MAX_DISPLAY_ROWS: usize = 32;
pub const MAX_DISPLAY_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_METADATA_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_OUTPUT_BYTES: u64 = 64 * 1024 * 1024 * 1024;
pub const DISK_RESERVE_BYTES: u64 = 1024 * 1024 * 1024;
pub const ARTIFACT_LINK_PREFIX: &str = "https://report-artifact.invalid/";

const COVERAGE_COLLECTIONS: [&str; 6] = [
    "/run/checks_skipped",
    "/run/checks_disabled",
    "/summary/check_coverage/checks_silent_ids",
    "/summary/check_coverage/checks_disabled_ids",
    "/summary/evidence_contract/capability_rows",
    "/summary/project_coverage/status/items",
];

/// Compact JSON text: no whitespace, no NaN or infinity, non-ASCII not escaped.
fn dumps(value: &Value) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| format!("JSON serialization failed: {e}"))
}

/// Percent-encode a string as one URI path segment.
fn quote_segment(text: &str) -> String {
    const SAFE: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_.-~";
    let mut out = String::new();
    for byte in text.as_bytes() {
        let ch = *byte as char;
        if SAFE.contains(ch) {
            out.push(ch);
        } else {
            // UTF-8 percent-encoding of the raw byte.
            out.push_str(&format!("%{:02X}", byte));
        }
    }
    out
}

fn pointer_escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// Project ordered prefixes, keeping complete source counts and coverage.
pub fn build_overview_model(
    reader: &AuditV2Reader,
    project_coverage: Option<&Value>,
) -> Result<Value, String> {
    let mut projection = Map::new();
    let mut omissions: Vec<Value> = Vec::new();
    let mut header = reader.header.clone();
    // Large nested summary populations are complete companion data, not
    // display metadata: reference them before checking the metadata budget.
    let required_summary = [
        "totals",
        "by_severity",
        "by_check",
        "check_coverage",
        "evidence_contract",
        "project_coverage",
    ];
    let summary_block = reader
        .header
        .get("summary")
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut projected_summary = Map::new();
    for (key, value) in summary_block.as_object().cloned().unwrap_or_default() {
        let value = if !required_summary.contains(&key.as_str())
            && (value.is_object() || value.is_array())
        {
            json!({
                "state": "retained_in_companion",
                "reference": format!("audit.json#/summary/{}", pointer_escape(&key)),
            })
        } else {
            value
        };
        projected_summary.insert(key, value);
    }
    if let Some(coverage) = project_coverage {
        projected_summary.insert("project_coverage".to_owned(), coverage.clone());
    }
    header
        .as_object_mut()
        .ok_or("audit.v2 header must be an object")?
        .insert("summary".to_owned(), Value::Object(projected_summary));
    let mut metadata_bytes = dumps(&header)?.len();
    if metadata_bytes > MAX_METADATA_BYTES {
        return Err(format!(
            "PDF overview metadata exceeds {MAX_METADATA_BYTES} bytes"
        ));
    }

    fn restore(
        value: &Value,
        reader: &AuditV2Reader,
        projection: &mut Map<String, Value>,
        omissions: &mut Vec<Value>,
        metadata_bytes: &mut usize,
    ) -> Result<Value, String> {
        if let Some(pointer) = value
            .as_object()
            .and_then(|m| m.get("$audit_v2_collection"))
            .and_then(Value::as_str)
        {
            let pointer = pointer.to_owned();
            let total = reader.count(&pointer).map_err(|e| e.to_string())?;
            if pointer == "/issues" || pointer == "/pages" {
                let mut rows = Vec::new();
                let mut size = 0usize;
                for row in reader
                    .iter_collection(&pointer)
                    .map_err(|e| e.to_string())?
                {
                    let row_bytes = dumps(&row)?.len();
                    if rows.len() == MAX_DISPLAY_ROWS || size + row_bytes > MAX_DISPLAY_BYTES {
                        break;
                    }
                    rows.push(row);
                    size += row_bytes;
                }
                let key = if pointer == "/issues" {
                    "findings"
                } else {
                    "pages"
                };
                projection.insert(
                    key.to_owned(),
                    json!({
                        "source": total,
                        "displayed": rows.len(),
                        "omitted": total - rows.len() as i64,
                        "first_ordinal": 0,
                        "end_ordinal_exclusive": rows.len(),
                        "display_bytes": size,
                        "max_rows": MAX_DISPLAY_ROWS,
                        "max_bytes": MAX_DISPLAY_BYTES,
                    }),
                );
                if total > rows.len() as i64 {
                    omissions.push(json!({
                        "collection": pointer,
                        "count": total - rows.len() as i64,
                        "reason": "Outside the ordered PDF display prefix; complete records remain in audit.json and CSV companions.",
                    }));
                }
                return Ok(Value::Array(rows));
            }
            if COVERAGE_COLLECTIONS.contains(&pointer.as_str()) {
                let mut rows = Vec::new();
                for row in reader
                    .iter_collection(&pointer)
                    .map_err(|e| e.to_string())?
                {
                    *metadata_bytes += dumps(&row)?.len();
                    if *metadata_bytes > MAX_METADATA_BYTES {
                        return Err(format!(
                            "PDF overview coverage metadata exceeds {MAX_METADATA_BYTES} bytes"
                        ));
                    }
                    rows.push(row);
                }
                return Ok(Value::Array(rows));
            }
            if total > 0 {
                omissions.push(json!({
                    "collection": pointer,
                    "count": total,
                    "reason": "This retained collection is available in full in audit.json; it is not expanded in the PDF overview.",
                }));
            }
            if pointer == "/groups" || pointer == "/suppressed_issues" {
                return Ok(json!([]));
            }
            return Ok(json!({
                "state": "retained_in_companion",
                "source_count": total,
                "reference": format!("audit.json#{pointer}"),
            }));
        }
        if let Some(map) = value.as_object() {
            let mut out = Map::new();
            for (key, child) in map {
                out.insert(
                    key.clone(),
                    restore(child, reader, projection, omissions, metadata_bytes)?,
                );
            }
            return Ok(Value::Object(out));
        }
        if let Some(list) = value.as_array() {
            let mut out = Vec::new();
            for child in list {
                out.push(restore(
                    child,
                    reader,
                    projection,
                    omissions,
                    metadata_bytes,
                )?);
            }
            return Ok(Value::Array(out));
        }
        Ok(value.clone())
    }

    let mut document = restore(
        &header,
        reader,
        &mut projection,
        &mut omissions,
        &mut metadata_bytes,
    )?;
    document
        .as_object_mut()
        .map(|m| m.entry("groups".to_owned()).or_insert(json!([])));
    let mut model = build_pdf_model(&document, &[], None, None).map_err(|e| e.0)?;
    // Native audits retain per-check capability records in source evidence;
    // project each one explicitly so the overview shows every state/reason.
    let capabilities = document
        .get("summary")
        .and_then(|s| s.get("evidence_contract"))
        .and_then(|e| e.get("capability_rows"))
        .and_then(Value::as_array)
        .cloned();
    if let Some(capabilities) = capabilities {
        let groups_has = model
            .get("coverage")
            .and_then(|c| c.get("groups"))
            .and_then(Value::as_object)
            .is_some_and(|g| g.contains_key("capabilities"));
        if !groups_has {
            let group = collection(
                Some(&Value::Array(capabilities.clone())),
                "summary.evidence_contract.capability_rows",
                "mapping",
            )
            .map_err(|e| e.0)?;
            if let Some(groups) = model
                .get_mut("coverage")
                .and_then(|c| c.get_mut("groups"))
                .and_then(Value::as_object_mut)
            {
                groups.insert("capabilities".to_owned(), group.clone());
            }
            if let Some(checks) = model
                .get_mut("coverage")
                .and_then(|c| c.get_mut("checks"))
                .and_then(Value::as_array_mut)
            {
                for (index, record) in capabilities.iter().enumerate() {
                    checks.push(coverage_record(
                        "summary.evidence_contract.capability_rows",
                        index,
                        record,
                        record
                            .get("state")
                            .and_then(Value::as_str)
                            .unwrap_or("unreported"),
                        record.get("check"),
                        record.get("reason"),
                    ));
                }
            }
            if let Some(checks) = model
                .get_mut("summary")
                .and_then(|s| s.get_mut("counts"))
                .and_then(|c| c.get_mut("checks"))
                .and_then(Value::as_object_mut)
            {
                checks.insert(
                    "capabilities".to_owned(),
                    json!({
                        "state": group.get("state").cloned().unwrap_or(Value::Null),
                        "source_count": group.get("source_count").cloned().unwrap_or(Value::Null),
                        "projected_count": group.get("projected_count").cloned().unwrap_or(Value::Null),
                    }),
                );
            }
        }
    }
    if let Some(m) = model.as_object_mut() {
        m.insert(
            "projection".to_owned(),
            json!({
                "policy": POLICY,
                "selection": "ordered-prefix-not-representative",
                "summary_display": "scalars and complete-artifact references for nested metadata",
                "coverage_display": "all saved check states and reasons; nested descriptors in audit.json",
                "collections": projection,
            }),
        );
        m.insert("omissions".to_owned(), Value::Array(omissions));
    }
    for (key, counts) in &projection {
        if let Some(block) = model
            .get_mut("summary")
            .and_then(|s| s.get_mut("counts"))
            .and_then(|c| c.get_mut(key))
            .and_then(Value::as_object_mut)
        {
            block.insert(
                "source_count".to_owned(),
                counts.get("source").cloned().unwrap_or(Value::Null),
            );
        }
    }
    Ok(model)
}

/// Free bytes on the filesystem holding `directory`.
fn free_bytes(directory: &Path) -> u64 {
    spiderhead_core::filesystem::free_space(directory).unwrap_or(u64::MAX)
}

/// Output budget: byte cap plus a 1 GiB disk reserve, polled every 8 MiB.
struct OutputBudget {
    directory: PathBuf,
    used: u64,
    next_disk_check: u64,
    /// file name -> {"bytes", "sha256", "rows"?}
    files: Map<String, Value>,
}

impl OutputBudget {
    fn new(directory: &Path) -> Self {
        Self {
            directory: directory.to_path_buf(),
            used: 0,
            next_disk_check: 0,
            files: Map::new(),
        }
    }

    fn reserve(&mut self, amount: u64) -> Result<(), String> {
        if self.used + amount > MAX_OUTPUT_BYTES {
            return Err(format!(
                "PDF package exceeds {MAX_OUTPUT_BYTES} output bytes; no package published"
            ));
        }
        if self.used + amount >= self.next_disk_check {
            if free_bytes(&self.directory) < DISK_RESERVE_BYTES + amount {
                return Err(
                    "PDF package would consume the 1 GiB disk reserve; no package published"
                        .to_owned(),
                );
            }
            self.next_disk_check = self.used + amount + 8 * 1024 * 1024;
        }
        self.used += amount;
        Ok(())
    }
}

/// A budgeted sink: reserves bytes before writing, tracks size/digest and
/// records `{bytes, sha256}` into the budget on drop. Shared through
/// `Rc<RefCell>` so it can be boxed as `dyn Write` inside `csvfile`.
struct BudgetSink {
    budget: Rc<RefCell<OutputBudget>>,
    name: String,
    file: std::fs::File,
    digest: Sha256,
    size: u64,
    error: Option<String>,
}

impl Write for BudgetSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(e) = &self.error {
            return Err(std::io::Error::other(e.clone()));
        }
        if let Err(e) = self.budget.borrow_mut().reserve(buf.len() as u64) {
            self.error = Some(e.clone());
            return Err(std::io::Error::other(e));
        }
        self.file.write_all(buf)?;
        self.digest.update(buf);
        self.size += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl Drop for BudgetSink {
    fn drop(&mut self) {
        let _ = self.file.sync_all();
        let digest = std::mem::take(&mut self.digest).finalize();
        if let Ok(mut budget) = self.budget.try_borrow_mut() {
            budget.files.insert(
                self.name.clone(),
                json!({"bytes": self.size, "sha256": format!("{:x}", digest)}),
            );
        }
    }
}

/// Replace staging-independent placeholder URIs with portable relative links.
fn portable_links(path: &Path, bundle_name: &str, artifacts: &[String]) -> Result<(), String> {
    use lopdf::{Document, Object, StringFormat};

    let mut doc = Document::load(path).map_err(|e| e.to_string())?;
    let links: Map<String, Value> = artifacts
        .iter()
        .map(|name| {
            (
                format!("{ARTIFACT_LINK_PREFIX}{name}"),
                json!(format!(
                    "{}/{}",
                    quote_segment(bundle_name),
                    quote_segment(name)
                )),
            )
        })
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    for (_num, page_id) in doc.get_pages() {
        let annots = doc
            .get_dictionary(page_id)
            .ok()
            .and_then(|d| d.get(b"Annots").ok().cloned());
        let Some(annots) = annots else { continue };
        let refs: Vec<lopdf::ObjectId> = match &annots {
            Object::Array(items) => items.iter().filter_map(|o| o.as_reference().ok()).collect(),
            Object::Reference(r) => vec![*r],
            _ => Vec::new(),
        };
        for r in refs {
            let action_uri = doc
                .get_object(r)
                .ok()
                .and_then(|o| o.as_dict().ok())
                .and_then(|d| d.get(b"A").ok())
                .and_then(|a| doc.dereference(a).ok().map(|(_, o)| o.clone()))
                .and_then(|a| a.as_dict().ok().cloned())
                .and_then(|d| {
                    d.get(b"URI")
                        .ok()
                        .and_then(|u| u.as_str().ok())
                        .map(|bytes| String::from_utf8_lossy(bytes).to_string())
                });
            let Some(uri) = action_uri else { continue };
            let Some(target) = links.get(&uri).and_then(Value::as_str) else {
                continue;
            };
            if let Ok(Object::Dictionary(dict)) = doc.get_object_mut(r) {
                if let Ok(Object::Dictionary(action)) = dict.get_mut(b"A") {
                    action.set(
                        "URI",
                        Object::String(target.as_bytes().to_vec(), StringFormat::Literal),
                    );
                    seen.insert(uri);
                }
            }
        }
    }
    if seen.len() != links.len() {
        return Err("PDF overview did not retain every complete-companion link".to_owned());
    }
    let replacement = path.with_extension("linked.pdf");
    doc.save(&replacement).map_err(|e| e.to_string())?;
    std::fs::rename(&replacement, path).map_err(|e| e.to_string())
}

/// Publish a new PDF and complete evidence directory, or publish neither.
pub fn write_overview(
    reader: &AuditV2Reader,
    view: &AuditV2ReportView,
    target: &Path,
    lang: &str,
) -> Result<Value, String> {
    let bundle = target.with_file_name(format!(
        "{}.files",
        target
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    if target.exists()
        || target.is_symlink()
        || bundle.exists()
        || bundle
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    {
        return Ok(json!({
            "ok": false,
            "error": "PDF overview requires a new output path and companion directory",
        }));
    }
    let project_coverage = view
        .get("summary")
        .and_then(|s| s.get("project_coverage").cloned());
    let model = build_overview_model(reader, project_coverage.as_ref())?;
    let names = [
        "audit.json".to_owned(),
        "findings.csv".to_owned(),
        "findings.pages.csv".to_owned(),
        "findings.scope.csv".to_owned(),
        "manifest.json".to_owned(),
    ];
    let model = {
        let mut m = model;
        m.as_object_mut().map(|o| {
            o.insert(
                "artifacts".to_owned(),
                Value::Array(names.iter().map(|n| json!(n)).collect()),
            )
        });
        m
    };

    let parent = target.parent().unwrap_or(Path::new("."));
    let temporary = std::env::temp_dir().join(format!(
        ".spiderhead-overview-{}-{}",
        std::process::id(),
        bundle
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    let _ = std::fs::remove_dir_all(&temporary);
    std::fs::create_dir_all(&temporary).map_err(|e| e.to_string())?;
    let cleanup = |temporary: &Path| {
        let _ = std::fs::remove_dir_all(temporary);
    };
    let stage = temporary.join(bundle.file_name().unwrap_or_default());
    std::fs::create_dir(&stage).map_err(|e| e.to_string())?;
    let mut budget = OutputBudget::new(&stage);
    if let Err(e) = budget.reserve(0) {
        cleanup(&temporary);
        return Err(e);
    }
    // Render first: a missing Chromium/PDF dependency must not trigger a
    // potentially large export that cannot produce the requested package.
    let pdf = stage.join("report.pdf");
    let rendered = crate::pdf_output::write_pdf_report(&model, &pdf, lang);
    if !rendered.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        cleanup(&temporary);
        return Ok(rendered);
    }
    if let Err(e) = portable_links(
        &pdf,
        &bundle
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        &names,
    ) {
        cleanup(&temporary);
        return Err(e);
    }
    let validation = crate::pdf_validation::validate_pdf_output(
        &pdf,
        &model,
        crate::pdf_validation::DEFAULT_PAGE_LIMIT,
        crate::pdf_validation::DEFAULT_BYTE_LIMIT,
    )?;
    if validation.get("status").and_then(Value::as_str) != Some("ok") {
        cleanup(&temporary);
        return Ok(json!({
            "ok": false,
            "error": "PDF overview validation failed",
            "validation": validation,
        }));
    }
    let pdf_size = std::fs::metadata(&pdf).map(|m| m.len()).unwrap_or(0);
    if let Err(e) = budget.reserve(pdf_size) {
        cleanup(&temporary);
        return Err(e);
    }
    let pdf_bytes = std::fs::read(&pdf).map_err(|e| e.to_string())?;
    let pdf_hash = format!("{:x}", Sha256::digest(&pdf_bytes));

    // audit.json — the complete source document, chunked as stored.
    {
        let path = stage.join("audit.json");
        let chunks = reader.document_chunks(None).map_err(|e| e.to_string())?;
        let mut file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
        let mut digest = Sha256::new();
        let mut size = 0u64;
        for chunk in &chunks {
            let encoded = chunk.as_bytes();
            if let Err(e) = budget.reserve(encoded.len() as u64) {
                cleanup(&temporary);
                return Err(e);
            }
            file.write_all(encoded).map_err(|e| e.to_string())?;
            digest.update(encoded);
            size += encoded.len() as u64;
        }
        file.sync_all().map_err(|e| e.to_string())?;
        budget.files.insert(
            "audit.json".to_owned(),
            json!({"bytes": size, "sha256": format!("{:x}", digest.finalize())}),
        );
    }

    // CSV companions through the budget sink, with explicit row counts.
    let shared = Rc::new(RefCell::new(budget));
    let csv_rows = {
        let shared = Rc::clone(&shared);
        let mut sink = move |path: &Path| -> Result<Box<dyn Write>, crate::csvfile::CsvError> {
            // Exclusive create with private permissions, so an existing file is never overwritten.
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(crate::csvfile::CsvError::Io)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = file.set_permissions(std::fs::Permissions::from_mode(0o600));
            }
            Ok(Box::new(BudgetSink {
                budget: Rc::clone(&shared),
                name: path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                file,
                digest: Sha256::new(),
                size: 0,
                error: None,
            }))
        };
        crate::csvfile::write_with(view, &stage.join("findings.csv"), &mut sink)
    };
    let mut budget = match Rc::try_unwrap(shared) {
        Ok(cell) => cell.into_inner(),
        Err(_) => unreachable!("budget sinks are dropped before this point"),
    };
    let csv_rows = match csv_rows {
        Ok(rows) => rows,
        Err(e) => {
            cleanup(&temporary);
            return Err(e.to_string());
        }
    };
    for (name, count) in [
        ("findings.csv", Some(csv_rows.findings)),
        ("findings.scope.csv", Some(csv_rows.scope)),
        ("findings.pages.csv", Some(csv_rows.pages)),
        ("findings.coverage.csv", csv_rows.coverage),
    ] {
        if let Some(count) = count {
            budget.files.entry(name.to_owned()).and_modify(|e| {
                if let Some(m) = e.as_object_mut() {
                    m.insert("rows".to_owned(), json!(count));
                }
            });
        }
    }
    for (name, expected) in [
        ("findings.csv", view.finding_count),
        ("findings.pages.csv", view.page_count),
    ] {
        let actual = if name == "findings.csv" {
            csv_rows.findings
        } else {
            csv_rows.pages
        };
        if actual as i64 != expected {
            cleanup(&temporary);
            return Err(format!(
                "PDF companion row count differs for {name}; no package published"
            ));
        }
    }

    let manifest = json!({
        "schema": "spiderhead.pdf-overview-package/1",
        "pdf_policy": POLICY,
        "source": {
            "format": AUDIT_V2,
            "sha256": reader.sha256,
            "binding": reader.binding,
            "collections": reader.collections,
        },
        "projection": model.get("projection").cloned().unwrap_or(Value::Null),
        "omissions": model.get("omissions").cloned().unwrap_or_else(|| json!([])),
        "complete_machine_export": true,
        "files": {
            // budget.files merged below
        },
        "limits": {
            "max_output_bytes": MAX_OUTPUT_BYTES,
            "disk_reserve_bytes": DISK_RESERVE_BYTES,
            "pdf_pages": crate::pdf_validation::DEFAULT_PAGE_LIMIT,
            "pdf_bytes": crate::pdf_validation::DEFAULT_BYTE_LIMIT,
            "metadata_bytes": MAX_METADATA_BYTES,
        },
    });
    let mut manifest = manifest;
    {
        let mut files = budget.files.clone();
        files.insert(
            format!(
                "../{}",
                target
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ),
            json!({"bytes": pdf_size, "sha256": pdf_hash}),
        );
        manifest
            .as_object_mut()
            .map(|m| m.insert("files".to_owned(), Value::Object(files)));
    }
    {
        let path = stage.join("manifest.json");
        let mut file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
        let payload = dumps(&manifest)? + "\n";
        let encoded = payload.as_bytes();
        if let Err(e) = budget.reserve(encoded.len() as u64) {
            cleanup(&temporary);
            return Err(e);
        }
        file.write_all(encoded).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        let mut digest = Sha256::new();
        digest.update(encoded);
        budget.files.insert(
            "manifest.json".to_owned(),
            json!({"bytes": encoded.len(), "sha256": format!("{:x}", digest.finalize())}),
        );
    }

    // Publish: bundle directory first, then hard-link the PDF out.
    let mut published = false;
    let mut linked = false;
    let outcome = (|| -> Result<(), String> {
        if bundle.exists() || bundle.symlink_metadata().is_ok() {
            return Err("PDF companion destination appeared during export".to_owned());
        }
        std::fs::rename(&stage, &bundle).map_err(|e| e.to_string())?;
        published = true;
        std::fs::hard_link(bundle.join("report.pdf"), target).map_err(|e| e.to_string())?;
        linked = true;
        std::fs::remove_file(bundle.join("report.pdf")).map_err(|e| e.to_string())?;
        let _ = spiderhead_store::scan_lease::fsync_directory(&bundle);
        let _ = spiderhead_store::scan_lease::fsync_directory(parent);
        Ok(())
    })();
    if let Err(e) = outcome {
        if linked {
            let _ = std::fs::remove_file(target);
        }
        if published {
            let _ = std::fs::remove_dir_all(&bundle);
        }
        cleanup(&temporary);
        return Err(e);
    }
    cleanup(&temporary);

    let mut result = rendered.clone();
    if let Some(m) = result.as_object_mut() {
        m.insert("path".to_owned(), json!(target.to_string_lossy()));
        m.insert(
            "bytes".to_owned(),
            json!(std::fs::metadata(target).map(|md| md.len()).unwrap_or(0)),
        );
        m.insert("findings".to_owned(), json!(view.finding_count));
        m.insert("pages".to_owned(), json!(view.page_count));
        m.insert("pdf_policy".to_owned(), json!(POLICY));
        m.insert(
            "projection".to_owned(),
            model.get("projection").cloned().unwrap_or(Value::Null),
        );
        m.insert(
            "manifest".to_owned(),
            json!(bundle.join("manifest.json").to_string_lossy()),
        );
        m.insert(
            "manifest_sha256".to_owned(),
            budget
                .files
                .get("manifest.json")
                .and_then(|f| f.get("sha256"))
                .cloned()
                .unwrap_or(Value::Null),
        );
        m.insert(
            "outputs".to_owned(),
            Value::Array(
                std::iter::once(json!(target.to_string_lossy()))
                    .chain(
                        names
                            .iter()
                            .map(|n| json!(bundle.join(n).to_string_lossy())),
                    )
                    .collect(),
            ),
        );
        m.insert("validation".to_owned(), validation);
    }
    Ok(result)
}
