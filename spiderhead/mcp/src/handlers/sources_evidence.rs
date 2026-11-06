//! Offline evidence and backlink handlers (the reference suite,
//! the reference suite, the reference suite): evidence normalization, the PageSpeed artifact
//! reader and the backlink donor checks.

use std::path::Path;

use serde_json::{json, Value};

use spiderhead_recon::backlink_profile::{build_backlink_profile, export_disavow};
use spiderhead_recon::backlinks::check_backlinks;
use spiderhead_sources::backlink_history::{save_snapshot, snapshot_history};
use spiderhead_sources::evidence_import::{normalize_file, normalize_inline, public_provenance};
use spiderhead_sources::provider_artifacts::read_pagespeed_artifact;
use spiderhead_sources::providers::save_local_artifact;

use crate::error::ToolError;

use super::sources_net::{fail, present, str_list};
use super::{kwargs, opt, opt_bool, opt_float, opt_int, opt_str};

const MAX_JSON_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// One argument that accepts inline JSON text or a bounded JSON file path
/// (`_json_or_path`).
pub(super) fn json_or_path(value: Option<&Value>, label: &str) -> Result<Option<Value>, ToolError> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(None),
        Value::Object(_) | Value::Array(_) => Ok(Some(value.clone())),
        Value::String(text) => {
            let text = text.trim();
            if text.starts_with('{') || text.starts_with('[') {
                return serde_json::from_str(text)
                    .map(Some)
                    .map_err(|error| fail(format!("{label} is not valid JSON: {error}")));
            }
            let path = Path::new(text);
            let bounded = path
                .symlink_metadata()
                .map(|meta| meta.is_file() && meta.len() <= MAX_JSON_FILE_BYTES)
                .unwrap_or(false);
            if !bounded {
                return Err(fail(format!(
                    "{label} must be inline JSON or a bounded readable JSON file"
                )));
            }
            let raw = std::fs::read_to_string(path).map_err(fail)?;
            serde_json::from_str(&raw)
                .map(Some)
                .map_err(|error| fail(format!("{label} is not valid JSON: {error}")))
        }
        _ => Err(fail(format!("{label} must be JSON data or a file path"))),
    }
}

/// `evidence_normalize`: normalize one supplied CSV/XLSX/JSON file, offline.
pub fn evidence_normalize(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(file) = present(&args, "file") else {
        return Err(fail("file required"));
    };
    let manifest = json_or_path(opt(&args, "mapping"), "mapping")?;
    let document = normalize_file(
        file,
        manifest.as_ref(),
        opt_str(&args, "sheet"),
        opt_str(&args, "site_origin"),
    )
    .map_err(fail)?;
    let privacy = document["mapping"]["source"]["privacy"]
        .as_str()
        .filter(|text| !text.is_empty())
        .unwrap_or("supplied")
        .to_owned();
    let artifact = match present(&args, "out_dir") {
        Some(directory) => Some(save_local_artifact(directory, &document).map_err(fail)?),
        None => None,
    };
    let mut result = json!({
        "ok": true,
        "format": "spiderhead.evidence-normalize.v1",
        "privacy": privacy,
        "summary": document["summary"],
        "artifact_reference": artifact,
    });
    if privacy == "restricted" {
        result["provenance"] = public_provenance(&document["provenance"]);
        result["rows_redacted"] = json!(true);
    } else {
        result["document"] = document;
    }
    Ok(result)
}

/// `provider_artifact_read`: a bounded, allowlisted PageSpeed summary page.
pub fn provider_artifact_read(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let null = Value::Null;
    read_pagespeed_artifact(
        opt(&args, "artifact_ref").unwrap_or(&null),
        opt(&args, "artifact_dir").unwrap_or(&null),
        opt(&args, "offset"),
        opt(&args, "limit"),
    )
    .map_err(fail)
}

/// `backlinks_check`: do the donor pages link to the target, and how.
pub fn backlinks_check(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(target) = present(&args, "target") else {
        return Err(fail("target required"));
    };
    let donors = str_list(&args, "donors").unwrap_or_default();
    if donors.is_empty() {
        return Err(fail("donors[] required"));
    }
    let concurrency = opt_int(&args, "concurrency").unwrap_or(3);
    Ok(check_backlinks(target, &donors, concurrency, 15.0))
}

/// `backlinks_disavow`: the explicit disavow selection, never submitted.
pub fn backlinks_disavow(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    export_disavow(
        str_list(&args, "domains").as_deref(),
        str_list(&args, "urls").as_deref(),
        opt_str(&args, "output"),
    )
    .map_err(fail)
}

/// `backlinks_profile`: merge explicit backlink evidence into one profile and
/// optionally retain a private history snapshot. Provider `sources` entries are
/// not collected here; evidence and imports supplied by the caller are.
pub fn backlinks_profile(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(domain) = present(&args, "domain") else {
        return Err(fail("domain is required"));
    };
    let normalized = spiderhead_recon::net_client::normalize_domain(domain);
    if normalized.is_empty() {
        return Err(fail("domain must be a valid hostname or HTTP(S) URL"));
    }
    if opt_bool(&args, "history_only").unwrap_or(false) {
        return snapshot_history(
            opt(&args, "db"),
            opt(&args, "project"),
            Some(&json!(normalized)),
            opt(&args, "limit"),
            opt(&args, "offset"),
        )
        .map_err(fail);
    }
    if opt_str(&args, "db").is_some() && opt_str(&args, "project").is_some() {
        return Err(fail("pass db or project, not both"));
    }
    if opt(&args, "sources").is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty())) {
        return Err(fail(
            "backlinks-profile sources[] collection is not available in this build",
        ));
    }
    let list = |name: &str| opt(&args, name).and_then(Value::as_array).cloned();
    let imports = list("imports");
    let evidence = list("evidence");
    let failure: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);
    let sink = |host: &str,
                rows: &[Value],
                coverage: &[Value],
                edges: &[Value],
                db: Option<&str>,
                project: Option<&str>,
                observed_at: &str| {
        let saved = save_snapshot(
            &json!(host),
            &json!(rows),
            &json!(coverage),
            &json!(edges),
            db.map(|value| json!(value)).as_ref(),
            project.map(|value| json!(value)).as_ref(),
            &json!(observed_at),
        );
        saved.unwrap_or_else(|error| {
            *failure.borrow_mut() = Some(error.to_string());
            Value::Null
        })
    };
    let profile = build_backlink_profile(
        domain,
        imports.as_deref(),
        evidence.as_deref(),
        opt_str(&args, "db"),
        opt_str(&args, "project"),
        opt_str(&args, "observed_at"),
        opt_str(&args, "state"),
        opt_str(&args, "follow"),
        opt_str(&args, "target_prefix"),
        opt_float(&args, "min_spam_score"),
        opt_float(&args, "max_spam_score"),
        opt_str(&args, "first_seen_from"),
        opt_str(&args, "first_seen_to"),
        opt_bool(&args, "one_per_domain").unwrap_or(false),
        opt(&args, "target_status"),
        opt_int(&args, "limit").unwrap_or(1000),
        opt_int(&args, "offset").unwrap_or(0),
        Some(&sink),
    )
    .map_err(fail)?;
    if let Some(message) = failure.into_inner() {
        return Err(fail(message));
    }
    Ok(profile)
}

/// Python truthiness of one JSON value (empty containers and zeros are falsy).
fn py_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().map(|n| n != 0.0).unwrap_or(false),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// `_evidence_document`: one evidence input resolved into a normalized evidence
/// document — as-is, inline rows, JSON text, or a bounded file normalized
/// through the mapping path.
fn evidence_document(
    value: &Value,
    mapping: Option<&Value>,
    sheet: Option<&str>,
    site_origin: Option<&str>,
) -> Result<Value, ToolError> {
    let manifest = json_or_path(mapping, "mapping")?;
    match value {
        Value::Object(map) => {
            if map.get("format")
                == Some(&json!(
                    spiderhead_sources::evidence_import::NORMALIZED_FORMAT
                ))
            {
                return Ok(value.clone());
            }
            if let Some(rows) = map.get("rows").and_then(Value::as_array) {
                let inline_manifest = match manifest.as_ref().filter(|m| py_truthy(m)) {
                    Some(resolved) => Some(resolved),
                    None => map.get("mapping").filter(|m| py_truthy(m)),
                };
                return normalize_inline(rows, inline_manifest).map_err(fail);
            }
            Err(ToolError::Failed(
                "evidence must be a normalized document, a rows object, or a file path".to_owned(),
            ))
        }
        Value::String(text) => {
            let text = text.trim();
            if text.starts_with('{') || text.starts_with('[') {
                let parsed: Value = serde_json::from_str(text).map_err(fail)?;
                return evidence_document(&parsed, mapping, sheet, site_origin);
            }
            normalize_file(text, manifest.as_ref(), sheet, site_origin).map_err(fail)
        }
        _ => Err(ToolError::Failed(
            "evidence must be a normalized document, a rows object, or a file path".to_owned(),
        )),
    }
}

/// `evidence_join`: join normalized analytics/search evidence to crawl pages,
/// offline; one crawl side (`pages`, `scan`, `audit`), an optional compare
/// source, and an optional durable store for large scans.
pub fn evidence_join(args: &Value) -> Result<Value, ToolError> {
    use rusqlite::OptionalExtension;
    use sha2::Digest;
    use spiderhead_sources::cloudflare::CfError;
    use spiderhead_sources::evidence_join::{evidence_compatibility, join_evidence};
    use spiderhead_sources::evidence_join_store;
    use spiderhead_store::inputs::load_audit_document;

    let args = kwargs(args);
    let document = evidence_document(
        opt(&args, "evidence")
            .ok_or_else(|| ToolError::Failed("evidence source required".to_owned()))?,
        opt(&args, "mapping"),
        opt_str(&args, "sheet"),
        opt_str(&args, "site_origin"),
    )?;
    let compare_document = match opt(&args, "compare") {
        Some(value) => Some(evidence_document(
            value,
            opt(&args, "compare_mapping"),
            opt_str(&args, "compare_sheet"),
            opt_str(&args, "compare_site_origin"),
        )?),
        None => None,
    };

    let pages_present = opt(&args, "pages").is_some();
    let scan_present = args.get("scan").is_some();
    let audit_present = opt(&args, "audit").is_some();
    if [pages_present, scan_present, audit_present]
        .into_iter()
        .filter(|flag| *flag)
        .count()
        > 1
    {
        return Err(ToolError::Failed(
            "pages, scan and audit are alternative crawl inputs, not a set".to_owned(),
        ));
    }

    const LARGE_SCAN_PAGES: i64 = 100_000;
    let mut page_rows: Option<Vec<Value>> = None;
    let mut large_scan = false;
    let mut crawl_context = json!({});
    if pages_present {
        let resolved = json_or_path(opt(&args, "pages"), "pages")?.ok_or_else(|| {
            ToolError::Failed("pages must be a bounded list of page objects".to_owned())
        })?;
        let rows = resolved
            .as_array()
            .filter(|rows| {
                rows.len() as i64 <= LARGE_SCAN_PAGES && rows.iter().all(Value::is_object)
            })
            .ok_or_else(|| {
                ToolError::Failed("pages must be a bounded list of page objects".to_owned())
            })?;
        page_rows = Some(rows.clone());
        crawl_context = json!({"source": "pages"});
    } else if let Some(scan) = present(&args, "scan") {
        let con = spiderhead_store::scan_artifact::open_scan(Path::new(scan), false, None)
            .map_err(super::failed)?;
        large_scan = con
            .query_row("SELECT COUNT(*) FROM pages", [], |row| row.get::<_, i64>(0))
            .map_err(|e| ToolError::Failed(e.to_string()))?
            > LARGE_SCAN_PAGES;
        if !large_scan {
            let mut stmt = con
                .prepare("SELECT u.url,p.status_code FROM pages p JOIN urls u USING(url_id) ORDER BY p.url_id")
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(json!({"url": row.get::<_, String>(0)?, "status_code": row.get::<_, Option<i64>>(1)?}))
                })
                .map_err(|e| ToolError::Failed(e.to_string()))?;
            let mut collected = Vec::new();
            for row in rows {
                collected.push(row.map_err(|e| ToolError::Failed(e.to_string()))?);
            }
            page_rows = Some(collected);
        }
        let scan_uuid: String = con
            .query_row("SELECT scan_uuid FROM scan", [], |row| row.get(0))
            .map_err(|e| ToolError::Failed(e.to_string()))?;
        let partial = con
            .query_row(
                "SELECT document_json FROM audit WHERE singleton=1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| ToolError::Failed(e.to_string()))?
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|document| {
                document
                    .get("run")
                    .cloned()
                    .unwrap_or(Value::Null)
                    .get("crawl_partial")
                    .cloned()
            })
            .and_then(|value| value.as_bool());
        crawl_context = json!({"source": "scan", "scan_uuid": scan_uuid, "partial": partial});
    } else if audit_present {
        let mut diagnostics: Vec<serde_json::Map<String, Value>> = Vec::new();
        let audit_document = load_audit_document(
            opt(&args, "audit").expect("checked above"),
            "audit",
            Some(&mut diagnostics),
        )
        .map_err(super::failed)?;
        let rows = audit_document
            .get("pages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if rows.len() as i64 > LARGE_SCAN_PAGES {
            return Err(ToolError::Failed(
                "audit pages exceed the bounded join limit".to_owned(),
            ));
        }
        page_rows = Some(rows);
        crawl_context = json!({
            "source": "audit",
            "partial": audit_document["run"]["crawl_partial"].as_bool().unwrap_or(false),
        });
        if !diagnostics.is_empty() {
            crawl_context["input_diagnostics"] =
                Value::Array(diagnostics.into_iter().map(Value::Object).collect());
        }
    }
    if page_rows.is_none() && !large_scan && compare_document.is_none() {
        return Err(ToolError::Failed(
            "evidence_join needs a crawl input (pages, scan or audit) to join, \
             or a compare source for a compatibility-only decision"
                .to_owned(),
        ));
    }
    let url_policy = json!({
        "ignore_query": opt_bool(&args, "ignore_query").unwrap_or(false),
        "ignore_scheme": opt_bool(&args, "ignore_scheme").unwrap_or(false),
        "casefold_path": opt_bool(&args, "casefold_path").unwrap_or(false),
    });

    let mut store_result: Option<Value> = None;
    let mut join_result: Option<Value> = None;
    if large_scan {
        let Some(out_dir) = present(&args, "out_dir") else {
            return Err(ToolError::Failed(format!(
                "a scan over {LARGE_SCAN_PAGES} pages requires --out-dir \
                 for its durable evidence-join store"
            )));
        };
        let root = Path::new(out_dir);
        if root
            .symlink_metadata()
            .map(|meta| meta.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err(ToolError::Failed(
                "private join output directory must not be a symlink".to_owned(),
            ));
        }
        std::fs::create_dir_all(root).map_err(fail)?;
        let digest =
            sha2::Sha256::digest(spiderhead_core::common::canonical_json(&document).as_bytes());
        let short: String = format!("{digest:x}").chars().take(16).collect();
        let destination = root.join(format!("evidence-join-{short}.sqlite"));
        if destination.symlink_metadata().is_ok() {
            return Err(ToolError::Failed(
                "durable evidence-join artifact already exists; choose a new output directory"
                    .to_owned(),
            ));
        }
        let scan_path = present(&args, "scan").unwrap_or_default().to_owned();
        let scan_rows = |path: &str| -> Result<(String, Vec<(i64, String)>), CfError> {
            let target = if path.is_empty() {
                scan_path.as_str()
            } else {
                path
            };
            let con = spiderhead_store::scan_artifact::open_scan(Path::new(target), false, None)
                .map_err(|e| CfError::Os(e.to_string()))?;
            let scan_uuid: String = con
                .query_row("SELECT scan_uuid FROM scan", [], |row| row.get(0))
                .map_err(|e| CfError::Os(e.to_string()))?;
            let mut stmt = con
                .prepare(
                    "SELECT p.page_ordinal,u.url FROM pages p JOIN urls u USING(url_id) \
                     ORDER BY p.page_ordinal",
                )
                .map_err(|e| CfError::Os(e.to_string()))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| CfError::Os(e.to_string()))?;
            let mut pairs = Vec::new();
            for row in rows {
                pairs.push(row.map_err(|e| CfError::Os(e.to_string()))?);
            }
            Ok((scan_uuid, pairs))
        };
        store_result = Some(
            evidence_join_store::write(
                &scan_rows,
                &scan_path,
                &document,
                &destination.to_string_lossy(),
                Some(&url_policy),
            )
            .map_err(fail)?,
        );
        spiderhead_core::filesystem::set_permissions_mode(root, 0o700).map_err(fail)?;
    } else if let Some(rows) = page_rows.as_deref() {
        join_result = Some(
            join_evidence(rows, &document, Some(&url_policy), Some(&crawl_context))
                .map_err(fail)?,
        );
    }
    let compatibility = match &compare_document {
        Some(compare) => {
            Some(evidence_compatibility(&document, compare, opt(&args, "policy")).map_err(fail)?)
        }
        None => None,
    };
    let restricted = [Some(&document), compare_document.as_ref()]
        .into_iter()
        .flatten()
        .any(|doc| doc["mapping"]["source"]["privacy"] == json!("restricted"));
    let artifact = match (&store_result, present(&args, "out_dir")) {
        (Some(store), _) => Some(format!(
            "local-artifact:{}",
            store["content_sha256"].as_str().unwrap_or_default()
        )),
        (None, Some(out_dir)) => Some(
            save_local_artifact(
                out_dir,
                &json!({
                    "join": join_result.clone().unwrap_or(Value::Null),
                    "compatibility": compatibility.clone().unwrap_or(Value::Null),
                }),
            )
            .map_err(fail)?,
        ),
        (None, None) => None,
    };
    let mut response = json!({
        "ok": true,
        "format": "spiderhead.evidence-join-result.v1",
        "privacy": if restricted { "restricted" } else { "supplied" },
        "artifact_reference": artifact,
    });
    if let Some(compatibility) = compatibility {
        response["compatibility"] = if restricted {
            spiderhead_sources::evidence_join::public_compatibility(&compatibility)
        } else {
            compatibility
        };
    }
    if let Some(store) = store_result {
        response["join_store"] = json!({
            "format": store["format"],
            "file_name": Path::new(store["path"].as_str().unwrap_or_default())
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or_default(),
            "url_policy": store["url_policy"],
            "allocation_policy": store["allocation_policy"],
            "crawl": store["crawl"],
            "summary": store["summary"],
            "content_sha256": store["content_sha256"],
        });
        response["populations_redacted"] = json!(restricted);
    } else if restricted {
        response["populations_redacted"] = json!(true);
        if let Some(join) = &join_result {
            response["join"] = json!({
                "format": join["format"],
                "url_policy": join["url_policy"],
                "crawl": join["crawl"],
                "summary": join["summary"],
            });
        }
    } else if let Some(join) = join_result {
        response["join"] = join;
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    // from tests/test_evidence_import.py::_manifest
    fn manifest(provider: &str) -> Value {
        let mut source = json!({
            "provider": provider,
            "timezone": "America/Los_Angeles",
            "reporting_identity": "fixture-property",
            "attribution": "data-driven",
        });
        if provider == "gsc" {
            source["search_engine"] = json!("google");
        }
        json!({
            "format": "spiderhead.evidence-mapping.v1",
            "period": {"start_date": "2026-01-01", "end_date": "2026-01-31"},
            "metrics": [{"name": "clicks", "type": "number", "unit": "count"}],
            "source": source,
            "row_shape": "flat",
        })
    }

    // from tests/test_evidence_import.py::_write_csv
    fn write_csv(path: &std::path::Path, header: &[&str], row: &[&str]) -> String {
        let mut text = format!("{}\n", header.join(","));
        text.push_str(&format!("{}\n", row.join(",")));
        std::fs::write(path, text).expect("write csv");
        path.to_string_lossy().into_owned()
    }

    // from tests/test_evidence_import.py::test_evidence_join_handler_joins_and_compares_offline
    #[test]
    fn joins_and_compares_offline() {
        let directory = tempfile::tempdir().expect("tempdir");
        let evidence = write_csv(
            &directory.path().join("rows.csv"),
            &["url", "clicks"],
            &["https://a.test/one", "4"],
        );
        let compare = write_csv(
            &directory.path().join("ga4.csv"),
            &["url", "sessions"],
            &["https://a.test/one", "9"],
        );
        let mut compare_mapping = manifest("ga4");
        compare_mapping["metrics"] = json!([{"name": "sessions"}]);
        let result = evidence_join(&json!({
            "audit": {"run": {"crawl_partial": false}, "pages": [{"url": "https://a.test/one"}]},
            "evidence": evidence,
            "compare": compare,
            "mapping": manifest("gsc"),
            "compare_mapping": compare_mapping,
            "policy": {"cross_source": "juxtapose"},
        }))
        .expect("ok");
        assert_eq!(result["ok"], json!(true));
        assert_eq!(result["join"]["summary"]["matched"], json!(1));
        assert_eq!(result["join"]["crawl"]["partial"], json!(false));
        let compatibility = &result["compatibility"];
        assert_eq!(compatibility["verdict"], json!("compatible"));
        assert_eq!(compatibility["juxtaposed"], json!(true));
        // GSC clicks and GA4 sessions remain labeled, distinct source metrics.
        assert_eq!(compatibility["axes"]["left"]["metrics"], json!(["clicks"]));
        assert_eq!(
            compatibility["axes"]["right"]["metrics"],
            json!(["sessions"])
        );
    }

    // from tests/test_evidence_import.py::test_evidence_join_handler_joins_and_compares_offline
    // (refusal branch: a valid evidence file but no crawl input and no compare source)
    #[test]
    fn refuses_without_a_crawl_side() {
        let directory = tempfile::tempdir().expect("tempdir");
        let evidence = write_csv(
            &directory.path().join("rows.csv"),
            &["url", "clicks"],
            &["https://a.test/one", "4"],
        );
        let error = evidence_join(&json!({
            "evidence": evidence,
            "mapping": manifest("gsc"),
        }))
        .unwrap_err();
        assert!(error.to_string().contains("needs a crawl input"));
    }

    // from tests/test_evidence_import.py::test_evidence_join_handler_joins_and_compares_offline
    // (restricted provenance keeps the join to counts only)
    #[test]
    fn restricted_evidence_redacts_the_join() {
        let directory = tempfile::tempdir().expect("tempdir");
        let evidence = write_csv(
            &directory.path().join("rows.csv"),
            &["url", "clicks"],
            &["https://a.test/one", "4"],
        );
        let mut mapping = manifest("gsc");
        mapping["source"]["privacy"] = json!("restricted");
        let result = evidence_join(&json!({
            "pages": [{"url": "https://a.test/one"}],
            "evidence": evidence,
            "mapping": mapping,
        }))
        .expect("ok");
        assert_eq!(result["privacy"], json!("restricted"));
        assert_eq!(result["populations_redacted"], json!(true));
        assert!(result["join"].get("rows").is_none());
        assert!(result["join"].get("format").is_some());
    }
}
