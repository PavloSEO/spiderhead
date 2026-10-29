//! Validate a temporary technical-audit PDF before publishing its final path.
//!
//! Checks output structure, model conservation, extracted text and hard limits.
//! A caller publishes the destination only for `status == "ok"` and deletes the
//! temporary file otherwise.
//!
//! Page counting and text extraction use `lopdf`, a pure-Rust parser, so no optional
//! dependency can be missing and the `skipped` branch is never reached.

use std::path::Path;

use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use unicode_normalization::UnicodeNormalization;

use crate::py::repr;

pub const MODEL_SCHEMA: &str = "spiderhead.technical-audit-pdf/1";
pub const DEFAULT_PAGE_LIMIT: i64 = 200;
pub const DEFAULT_BYTE_LIMIT: i64 = 25 * 1024 * 1024;

fn normalized(value: &str) -> String {
    value
        .nfkc()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_int(v: &Value) -> bool {
    v.as_i64().is_some() && !v.is_boolean()
}

fn as_mapping(v: &Value) -> Option<&Map<String, Value>> {
    v.as_object()
}

fn projected_count_errors(model: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    if model.get("schema") != Some(&Value::from(MODEL_SCHEMA)) {
        return vec![format!(
            "unsupported or missing PDF model schema {}",
            repr(model.get("schema").unwrap_or(&Value::Null))
        )];
    }
    let counts = model
        .get("summary")
        .and_then(as_mapping)
        .and_then(|s| s.get("counts"))
        .and_then(as_mapping);
    let counts = match counts {
        Some(c) => c,
        None => return vec!["PDF model has no summary.counts object".to_owned()],
    };

    let collections: Vec<(&str, Option<&Value>)> = vec![
        ("findings", model.get("findings")),
        ("pages", model.get("pages")),
        (
            "backlog",
            model
                .get("backlog")
                .and_then(as_mapping)
                .and_then(|b| b.get("items")),
        ),
    ];
    for (name, rows) in collections {
        let count_block = counts.get(name).and_then(as_mapping);
        let rows = match rows.and_then(Value::as_array) {
            Some(r) => r,
            None => {
                errors.push(format!("PDF model {name} must be a list"));
                continue;
            }
        };
        let count_block = match count_block {
            Some(b) => b,
            None => {
                errors.push(format!("PDF model has no summary.counts.{name} object"));
                continue;
            }
        };
        let projected = count_block.get("projected_count");
        if !projected.is_some_and(is_int)
            || projected.and_then(Value::as_i64) != Some(rows.len() as i64)
        {
            errors.push(format!(
                "PDF model {name} count mismatch: projected {}, rows {}",
                repr(projected.unwrap_or(&Value::Null)),
                rows.len()
            ));
        }
        let source_count = count_block.get("source_count");
        if let Some(sc) = source_count.filter(|v| !v.is_null()) {
            if !is_int(sc) || sc.as_i64().is_some_and(|n| n < rows.len() as i64) {
                errors.push(format!(
                    "PDF model {name} source count {} is below its projected rows",
                    repr(sc)
                ));
            }
        }
        if source_count.is_some_and(is_int)
            && source_count.and_then(Value::as_i64) > Some(rows.len() as i64)
            && !model
                .get("omissions")
                .and_then(Value::as_array)
                .is_some_and(|o| !o.is_empty())
        {
            errors.push(format!(
                "PDF model {name} omits source rows without an omission record"
            ));
        }
    }

    let checks = model
        .get("coverage")
        .and_then(as_mapping)
        .and_then(|c| c.get("checks"))
        .and_then(Value::as_array);
    let check_counts = counts.get("checks").and_then(as_mapping);
    match (checks, check_counts) {
        (Some(checks), Some(check_counts)) => {
            let mut projected_total = 0i64;
            for name in [
                "ran",
                "failed",
                "page_tools_failed",
                "skipped",
                "disabled",
                "capabilities",
                "silent",
            ] {
                let Some(group) = check_counts.get(name) else {
                    continue;
                };
                let projected = group.get("projected_count");
                if !projected.is_some_and(is_int)
                    || projected.and_then(Value::as_i64).is_some_and(|n| n < 0)
                {
                    errors.push(format!(
                        "PDF model checks.{name}.projected_count is invalid"
                    ));
                } else {
                    projected_total += projected.and_then(Value::as_i64).unwrap_or(0);
                }
            }
            if projected_total != checks.len() as i64 {
                errors.push(format!(
                    "PDF model check count mismatch: projected {projected_total}, rows {}",
                    checks.len()
                ));
            }
        }
        _ => errors.push("PDF model has no check coverage rows or counts".to_owned()),
    }
    errors
}

fn expected_text(model: &Value) -> IndexMap<String, u32> {
    let mut expected: IndexMap<String, u32> = IndexMap::new();
    let mut add = |value: Option<&Value>| {
        if let Some(s) = value.and_then(Value::as_str) {
            let token = normalized(s);
            if token.chars().count() > 1 {
                *expected.entry(token).or_insert(0) += 1;
            }
        }
    };

    for finding in model
        .get("findings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let Some(finding) = finding.as_object() else {
            continue;
        };
        let display = finding.get("display").and_then(as_mapping);
        let record = finding.get("record").and_then(as_mapping);
        add(display.and_then(|d| d.get("observation")));
        if let Some(record) = record {
            add(record.get("url"));
            add(record.get("check"));
        }
    }
    for page in model
        .get("pages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let record = page.get("record").and_then(as_mapping);
        if let Some(record) = record {
            add(record.get("url"));
            add(record.get("title"));
        }
    }
    for item in model
        .get("backlog")
        .and_then(as_mapping)
        .and_then(|b| b.get("items"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let record = item.get("record").and_then(as_mapping);
        if let Some(record) = record {
            add(record.get("title"));
        }
    }
    for reason in model
        .get("run")
        .and_then(as_mapping)
        .and_then(|r| r.get("reasons"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        add(Some(&reason));
    }
    for check in model
        .get("coverage")
        .and_then(as_mapping)
        .and_then(|c| c.get("checks"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        if let Some(check) = check.as_object() {
            add(check.get("id"));
            add(check.get("reason"));
        }
    }
    for omission in model
        .get("omissions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        if let Some(obj) = omission.as_object() {
            for field in ["reason", "message", "notice"] {
                add(obj.get(field));
            }
        } else {
            add(Some(&omission));
        }
    }
    expected
}

fn result(status: &str, page_count: Value, size_bytes: Value, errors: Vec<String>) -> Value {
    json!({
        "status": status,
        "page_count": page_count,
        "size_bytes": size_bytes,
        "errors": errors,
    })
}

/// Check output structure, model conservation, extracted text and hard limits.
///
/// `page_limit` and `byte_limit` must be positive; otherwise returns `Err`.
pub fn validate_pdf_output(
    pdf_path: &Path,
    model: &Value,
    page_limit: i64,
    byte_limit: i64,
) -> Result<Value, String> {
    if page_limit < 1 {
        return Err("page_limit must be a positive integer".to_owned());
    }
    if byte_limit < 1 {
        return Err("byte_limit must be a positive integer".to_owned());
    }
    if !model.is_object() {
        return Ok(result(
            "failed",
            Value::Null,
            Value::Null,
            vec!["PDF model must be an object".to_owned()],
        ));
    }

    let mut errors = projected_count_errors(model);
    if !pdf_path.is_file() {
        errors.push("temporary PDF is missing".to_owned());
        return Ok(result("failed", Value::Null, Value::Null, errors));
    }
    let size_bytes = match std::fs::metadata(pdf_path) {
        Ok(m) => m.len() as i64,
        Err(e) => {
            errors.push(format!("temporary PDF cannot be read: {e}"));
            return Ok(result("failed", Value::Null, Value::Null, errors));
        }
    };
    if size_bytes == 0 {
        errors.push("temporary PDF is empty".to_owned());
    }
    if size_bytes > byte_limit {
        errors.push(format!(
            "PDF byte limit exceeded: {size_bytes} > {byte_limit}"
        ));
    }
    if errors.is_empty() {
        use std::io::Read;
        let signature = std::fs::File::open(pdf_path)
            .and_then(|mut f| {
                let mut buf = [0u8; 5];
                f.read_exact(&mut buf).map(|_| buf)
            })
            .unwrap_or([0u8; 5]);
        if &signature != b"%PDF-" {
            errors.push("temporary output does not start with a PDF signature".to_owned());
        }
    }
    if !errors.is_empty() {
        return Ok(result(
            "failed",
            Value::Null,
            Value::from(size_bytes),
            errors,
        ));
    }

    // Parsing is bundled through lopdf, so no optional dependency can be missing.
    let mut extracted: Vec<String> = Vec::new();
    let page_count = match lopdf::Document::load(pdf_path) {
        Ok(doc) => {
            let pages = doc.get_pages();
            let count = pages.len() as i64;
            if count == 0 {
                errors.push("PDF contains no pages".to_owned());
            }
            if count > page_limit {
                errors.push(format!("PDF page limit exceeded: {count} > {page_limit}"));
                return Ok(result(
                    "failed",
                    Value::from(count),
                    Value::from(size_bytes),
                    errors,
                ));
            }
            for (index, page_number) in pages.keys().enumerate() {
                let text = doc
                    .extract_text(&[*page_number])
                    .unwrap_or_else(|_| String::new());
                if text.trim().is_empty() {
                    errors.push(format!("PDF page {} has no extractable text", index + 1));
                }
                extracted.push(text);
            }
            Some(count)
        }
        Err(e) => {
            return Ok(result(
                "failed",
                Value::Null,
                Value::from(size_bytes),
                vec![format!("PDF could not be parsed or text extracted: {e}")],
            ));
        }
    };

    if errors.is_empty() {
        let searchable = normalized(&extracted.join(" "));
        for (token, expected_count) in expected_text(model).iter() {
            let actual = searchable.matches(token.as_str()).count() as u32;
            if actual < *expected_count {
                errors.push(format!(
                    "PDF text is missing model content: expected {expected_count} occurrence(s) of {}, found {actual}",
                    crate::py::repr_str(token)
                ));
                if errors.len() >= 25 {
                    errors.push(
                        "additional missing model text was omitted from diagnostics".to_owned(),
                    );
                    break;
                }
            }
        }
    }

    Ok(result(
        if errors.is_empty() { "ok" } else { "failed" },
        page_count.map(Value::from).unwrap_or(Value::Null),
        Value::from(size_bytes),
        errors,
    ))
}
