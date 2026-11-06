//! `evidence_join`: join normalized analytics/search evidence to crawl pages,
//! offline only (`mcp/handlers/evidence.py::evidence_join`).

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use sha2::Digest;

use spiderhead_sources::evidence_import::{normalize_file, normalize_inline, NORMALIZED_FORMAT};
use spiderhead_sources::evidence_join::{
    evidence_compatibility, join_evidence, public_compatibility,
};
use spiderhead_sources::evidence_join_store;
use spiderhead_store::scan_artifact::open_scan;

use crate::error::ToolError;

use super::sources_evidence::json_or_path;
use super::sources_net::fail;
use super::{kwargs, opt, opt_bool, opt_str};

/// A scan at or above this page count joins through the durable store, not memory.
const LARGE_SCAN_PAGES: i64 = 100_000;

/// rusqlite failure → a failed handler call with the driver's text.
fn sql_error(error: rusqlite::Error) -> ToolError {
    ToolError::Failed(error.to_string())
}

/// One crawl input page: URL plus status, keyed and ordered like the scan query.
fn page_row(url: String, status_code: Option<i64>) -> Value {
    let mut row = Map::new();
    row.insert("url".into(), json!(url));
    row.insert("status_code".into(), json!(status_code));
    Value::Object(row)
}

/// Resolve one evidence input into a normalized document (`_evidence_document`).
fn evidence_document(
    value: Option<&Value>,
    mapping: Option<&Value>,
    sheet: Option<&str>,
    site_origin: Option<&str>,
) -> Result<Value, ToolError> {
    let Some(value) = value else {
        return Err(fail("evidence source required"));
    };
    let manifest = json_or_path(mapping, "mapping")?;
    match value {
        Value::Object(object) => {
            if spiderhead_core::formats::matches(
                object.get("format").and_then(Value::as_str),
                NORMALIZED_FORMAT,
            ) {
                return Ok(value.clone());
            }
            if let Some(rows) = object.get("rows").filter(|rows| rows.is_array()) {
                // An empty manifest is falsy in Python: the document's own
                // mapping takes over.
                let manifest_value = if manifest.as_ref().is_some_and(|m| {
                    m.as_object().is_some_and(|o| !o.is_empty())
                        || m.as_array().is_some_and(|a| !a.is_empty())
                }) {
                    manifest.as_ref()
                } else {
                    object.get("mapping")
                };
                let rows = rows.as_array().cloned().unwrap_or_default();
                return normalize_inline(&rows, manifest_value).map_err(fail);
            }
            Err(fail(
                "evidence must be a normalized document, a rows object, or a file path",
            ))
        }
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                let parsed: Value =
                    serde_json::from_str(trimmed).map_err(|error| fail(error.to_string()))?;
                return evidence_document(Some(&parsed), mapping, sheet, site_origin);
            }
            normalize_file(text, manifest.as_ref(), sheet, site_origin).map_err(fail)
        }
        _ => Err(fail(
            "evidence must be a normalized document, a rows object, or a file path",
        )),
    }
}

/// The saved pages and identity of one scan, read through a read-only open.
fn scan_identity_and_rows(
    scan: &str,
) -> Result<(String, Vec<(i64, String)>), spiderhead_sources::cloudflare::CfError> {
    use spiderhead_sources::cloudflare::CfError;
    let store_error = |message: String| CfError::Classed("EvidenceJoinStoreError", message);
    let con =
        open_scan(Path::new(scan), false, None).map_err(|error| store_error(error.to_string()))?;
    let uuid: String = con
        .query_row("SELECT scan_uuid FROM scan WHERE singleton=1", [], |r| {
            r.get(0)
        })
        .map_err(|error| store_error(error.to_string()))?;
    let mut statement = con
        .prepare(
            "SELECT p.page_ordinal,u.url FROM pages p JOIN urls u USING(url_id) \
             ORDER BY p.page_ordinal",
        )
        .map_err(|error| store_error(error.to_string()))?;
    let rows = statement
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .map_err(|error| store_error(error.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| store_error(error.to_string()))?;
    Ok((uuid, rows))
}

/// The `crawl_partial` flag of one scan's saved audit, `None` when unreadable.
fn scan_partial(con: &rusqlite::Connection) -> Option<bool> {
    let raw: Option<String> = con
        .query_row(
            "SELECT document_json FROM audit WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .ok()?;
    let document: Value = serde_json::from_str(&raw?).ok()?;
    document
        .get("run")
        .into_iter()
        .flat_map(|run| run.get("crawl_partial"))
        .next()
        .and_then(Value::as_bool)
}

/// `evidence_join`: join normalized evidence to crawl pages, offline only.
pub fn evidence_join(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let document = evidence_document(
        opt(&args, "evidence"),
        opt(&args, "mapping"),
        opt_str(&args, "sheet"),
        opt_str(&args, "site_origin"),
    )?;
    let compare_document = match opt(&args, "compare") {
        Some(value) => Some(evidence_document(
            Some(value),
            opt(&args, "compare_mapping"),
            opt_str(&args, "compare_sheet"),
            opt_str(&args, "compare_site_origin"),
        )?),
        None => None,
    };
    let sources = [
        opt(&args, "pages").is_some(),
        opt(&args, "scan").is_some(),
        opt(&args, "audit").is_some(),
    ];
    if sources.iter().filter(|used| **used).count() > 1 {
        return Err(fail(
            "pages, scan and audit are alternative crawl inputs, not a set",
        ));
    }

    let mut page_rows: Option<Vec<Value>> = None;
    let mut large_scan = false;
    let mut crawl_context = Map::new();
    if let Some(pages) = opt(&args, "pages") {
        let rows = json_or_path(Some(pages), "pages")?.unwrap_or(Value::Null);
        let valid = rows.is_array()
            && rows
                .as_array()
                .is_some_and(|items| items.len() <= 100_000 && items.iter().all(Value::is_object));
        if !valid {
            return Err(fail("pages must be a bounded list of page objects"));
        }
        page_rows = Some(rows.as_array().cloned().unwrap_or_default());
        crawl_context.insert("source".into(), json!("pages"));
    } else if let Some(scan) = opt_str(&args, "scan") {
        let con = open_scan(Path::new(scan), false, None).map_err(super::failed)?;
        let total: i64 = con
            .query_row("SELECT COUNT(*) FROM pages", [], |r| r.get(0))
            .map_err(sql_error)?;
        large_scan = total > LARGE_SCAN_PAGES;
        if !large_scan {
            let mut statement = con
                .prepare(
                    "SELECT u.url,p.status_code FROM pages p JOIN urls u USING(url_id) \
                     ORDER BY p.url_id",
                )
                .map_err(sql_error)?;
            page_rows = Some(
                statement
                    .query_map([], |r| Ok(page_row(r.get(0)?, r.get::<_, Option<i64>>(1)?)))
                    .map_err(sql_error)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(sql_error)?,
            );
        }
        let scan_uuid: String = con
            .query_row("SELECT scan_uuid FROM scan", [], |r| r.get(0))
            .map_err(sql_error)?;
        let partial = scan_partial(&con);
        crawl_context.insert("source".into(), json!("scan"));
        crawl_context.insert("scan_uuid".into(), json!(scan_uuid));
        crawl_context.insert("partial".into(), json!(partial));
    } else if let Some(audit) = opt(&args, "audit") {
        let mut diagnostics: Vec<Map<String, Value>> = Vec::new();
        let document =
            spiderhead_store::inputs::load_audit_document(audit, "audit", Some(&mut diagnostics))
                .map_err(super::failed)?;
        let rows = document
            .get("pages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if rows.len() > 100_000 {
            return Err(fail("audit pages exceed the bounded join limit"));
        }
        page_rows = Some(rows);
        crawl_context.insert("source".into(), json!("audit"));
        crawl_context.insert(
            "partial".into(),
            json!(document
                .get("run")
                .into_iter()
                .flat_map(|run| run.get("crawl_partial"))
                .next()
                .and_then(Value::as_bool)
                .unwrap_or(false)),
        );
        if !diagnostics.is_empty() {
            crawl_context.insert(
                "input_diagnostics".into(),
                Value::Array(diagnostics.into_iter().map(Value::Object).collect()),
            );
        }
    }
    if page_rows.is_none() && !large_scan && compare_document.is_none() {
        return Err(fail(
            "evidence_join needs a crawl input (pages, scan or audit) to join, \
             or a compare source for a compatibility-only decision",
        ));
    }

    let url_policy = json!({
        "ignore_query": opt_bool(&args, "ignore_query").unwrap_or(false),
        "ignore_scheme": opt_bool(&args, "ignore_scheme").unwrap_or(false),
        "casefold_path": opt_bool(&args, "casefold_path").unwrap_or(false),
    });

    let mut join_result: Option<Value> = None;
    let mut store_result: Option<Value> = None;
    if large_scan {
        let Some(out_dir) = opt_str(&args, "out_dir") else {
            return Err(fail(format!(
                "a scan over {LARGE_SCAN_PAGES} pages requires --out-dir \
                 for its durable evidence-join store"
            )));
        };
        store_result = Some(large_scan_store(
            opt_str(&args, "scan").unwrap_or_default(),
            &document,
            &url_policy,
            out_dir,
        )?);
    } else if let Some(rows) = &page_rows {
        join_result = Some(
            join_evidence(
                rows,
                &document,
                Some(&url_policy),
                Some(&Value::Object(crawl_context)),
            )
            .map_err(fail)?,
        );
    }
    let compatibility = match &compare_document {
        Some(compare) => Some(
            evidence_compatibility(
                &document,
                compare,
                json_or_path(opt(&args, "policy"), "policy")?.as_ref(),
            )
            .map_err(fail)?,
        ),
        None => None,
    };

    let restricted = [Some(&document), compare_document.as_ref()]
        .into_iter()
        .flatten()
        .any(|doc| {
            doc.get("mapping")
                .and_then(|mapping| mapping.get("source"))
                .and_then(|source| source.get("privacy"))
                .and_then(Value::as_str)
                == Some("restricted")
        });
    let artifact = match (&store_result, opt_str(&args, "out_dir")) {
        (Some(store), _) => Some(format!(
            "local-artifact:{}",
            store["content_sha256"].as_str().unwrap_or_default()
        )),
        (None, Some(out_dir)) => Some(
            spiderhead_sources::providers::save_local_artifact(
                out_dir,
                &json!({"join": join_result.clone(), "compatibility": compatibility.clone()}),
            )
            .map_err(fail)?,
        ),
        (None, None) => None,
    };

    let mut response = Map::new();
    response.insert("ok".into(), json!(true));
    response.insert("format".into(), json!("spiderhead.evidence-join-result.v1"));
    response.insert(
        "privacy".into(),
        json!(if restricted { "restricted" } else { "supplied" }),
    );
    response.insert("artifact_reference".into(), json!(artifact));
    if let Some(compatibility) = compatibility {
        response.insert(
            "compatibility".into(),
            if restricted {
                public_compatibility(&compatibility)
            } else {
                compatibility
            },
        );
    }
    if let Some(store) = store_result {
        let store = store.as_object().cloned().unwrap_or_default();
        let join_store = json!({
            "format": store.get("format"),
            "file_name": store
                .get("path")
                .and_then(Value::as_str)
                .map(|path| {
                    Path::new(path)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default()
                })
                .unwrap_or_default(),
            "url_policy": store.get("url_policy"),
            "allocation_policy": store.get("allocation_policy"),
            "crawl": store.get("crawl"),
            "summary": store.get("summary"),
            "content_sha256": store.get("content_sha256"),
        });
        response.insert("join_store".into(), join_store);
        response.insert("populations_redacted".into(), json!(restricted));
    } else if restricted {
        response.insert("populations_redacted".into(), json!(true));
        if let Some(join) = join_result {
            response.insert(
                "join".into(),
                json!({
                    "format": join.get("format"),
                    "url_policy": join.get("url_policy"),
                    "crawl": join.get("crawl"),
                    "summary": join.get("summary"),
                }),
            );
        }
    } else if let Some(join) = join_result {
        response.insert("join".into(), join);
    }
    Ok(Value::Object(response))
}

/// The large-scan path: a durable store under a private output directory.
fn large_scan_store(
    scan: &str,
    document: &Value,
    url_policy: &Value,
    out_dir: &str,
) -> Result<Value, ToolError> {
    let root = PathBuf::from(out_dir);
    if root
        .symlink_metadata()
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(fail("private join output directory must not be a symlink"));
    };
    std::fs::create_dir_all(&root).map_err(|error| fail(error.to_string()))?;
    let mut digest = sha2::Sha256::new();
    digest.update(canonical_text(document));
    let digest = hex(&digest.finalize());
    let destination = root.join(format!("evidence-join-{}.sqlite", &digest[..16]));
    if destination.exists() || destination.symlink_metadata().is_ok() {
        return Err(fail(
            "durable evidence-join artifact already exists; choose a new output directory",
        ));
    }
    let destination = destination.to_string_lossy().into_owned();
    let result = evidence_join_store::write(
        &scan_identity_and_rows,
        scan,
        document,
        &destination,
        Some(url_policy),
    )
    .map_err(fail)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| fail(error.to_string()))?;
    }
    Ok(result)
}

/// `json.dumps(..., ensure_ascii=False, sort_keys=True, separators=(",", ":"))`.
fn canonical_text(value: &Value) -> String {
    fn write(out: &mut String, value: &Value) {
        match value {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (index, key) in keys.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(key).unwrap_or_default());
                    out.push(':');
                    write(out, &map[*key]);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write(out, item);
                }
                out.push(']');
            }
            other => {
                out.push_str(&serde_json::to_string(other).unwrap_or_default());
            }
        }
    }
    let mut out = String::new();
    write(&mut out, value);
    out
}

/// Lowercase hex of a digest.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
