//! Build a validated PDF report without exposing intermediate files.
//!
//! Renders localized HTML, prints it through headless Chromium inside a temporary
//! directory, rewrites fixed metadata, validates, then atomically moves the result into place.

use std::path::Path;

use serde_json::{json, Value};

const INSTALL: &str = "pip install 'seohead-seotools[pdf]' and install Chrome, Edge or Chromium";

/// Rewrite the PDF with only fixed, non-source metadata values.
fn sanitize_metadata(path: &Path, lang: &str) -> Result<(), String> {
    let mut doc = lopdf::Document::load(path).map_err(|e| e.to_string())?;
    let title = if lang == "ru" {
        "Технический SEO-аудит"
    } else {
        "Technical SEO audit"
    };
    let info: Vec<(Vec<u8>, lopdf::Object)> = [
        ("Title", title),
        ("Author", "SpiderHead Tools"),
        ("Subject", "Technical SEO audit report"),
        ("Creator", "SpiderHead Tools"),
        ("Producer", "SpiderHead Tools"),
    ]
    .iter()
    .map(|(key, text)| {
        (
            key.as_bytes().to_vec(),
            lopdf::Object::string_literal(text.as_bytes().to_vec()),
        )
    })
    .collect();
    let info_dict = doc.add_object(lopdf::Dictionary::from_iter(info));
    doc.trailer.set("Info", info_dict);
    let cleaned = path.with_file_name(format!(
        ".{}.metadata.pdf",
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    let outcome = doc.save(&cleaned).map_err(|e| e.to_string());
    match outcome {
        Ok(_) => std::fs::rename(&cleaned, path).map_err(|e| e.to_string()),
        Err(e) => {
            let _ = std::fs::remove_file(&cleaned);
            Err(e)
        }
    }
}

/// Render, sanitize, validate and atomically publish one technical-audit PDF.
///
/// Returns a result object (`ok`/`status`/`error`), never an exception: `Err` is
/// reserved for model and brand contract violations.
pub fn write_pdf_report(model: &Value, path: &Path, lang: &str) -> Value {
    let html = match crate::audit_pdf::render_audit_pdf_html(model, lang, None) {
        Ok(html) => html,
        Err(e) => {
            return json!({
                "ok": false,
                "format": "pdf",
                "status": "failed",
                "error": format!("ValueError: {e}"),
            })
        }
    };
    let temp_dir = std::env::temp_dir().join(format!(
        ".spiderhead-pdf-{}-{}",
        std::process::id(),
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    let _ = std::fs::remove_dir_all(&temp_dir);
    if std::fs::create_dir_all(&temp_dir).is_err() {
        return json!({
            "ok": false,
            "format": "pdf",
            "status": "failed",
            "error": "could not create the temporary PDF directory",
        });
    }
    let cleanup = |dir: &Path| {
        let _ = std::fs::remove_dir_all(dir);
    };
    let html_path = temp_dir.join("report.html");
    let pdf_path = temp_dir.join("report.pdf");
    if let Err(e) = std::fs::write(&html_path, html.as_bytes()) {
        cleanup(&temp_dir);
        return json!({
            "ok": false,
            "format": "pdf",
            "status": "failed",
            "error": format!("OSError: {e}"),
        });
    }
    let rendered = crate::chromium_pdf::print_to_pdf(
        &html_path,
        &pdf_path,
        None,
        crate::chromium_pdf::DEFAULT_TIMEOUT,
    );
    if rendered.get("status").and_then(Value::as_str) != Some("ok") {
        let status = rendered
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("failed");
        let error = if status == "skipped" {
            "no local Chrome, Edge, or Chromium executable was found".to_owned()
        } else {
            rendered
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("Chromium did not produce a PDF")
                .to_owned()
        };
        cleanup(&temp_dir);
        return json!({
            "ok": false,
            "format": "pdf",
            "status": status,
            "error": error,
            "install": INSTALL,
        });
    }

    if let Err(e) = sanitize_metadata(&pdf_path, lang) {
        cleanup(&temp_dir);
        return json!({
            "ok": false,
            "format": "pdf",
            "status": "failed",
            "error": format!("metadata rewrite failed: {e}"),
        });
    }
    let validation = match crate::pdf_validation::validate_pdf_output(
        &pdf_path,
        model,
        crate::pdf_validation::DEFAULT_PAGE_LIMIT,
        crate::pdf_validation::DEFAULT_BYTE_LIMIT,
    ) {
        Ok(v) => v,
        Err(e) => {
            cleanup(&temp_dir);
            return json!({
                "ok": false,
                "format": "pdf",
                "status": "failed",
                "error": format!("ValueError: {e}"),
            });
        }
    };
    if validation.get("status").and_then(Value::as_str) != Some("ok") {
        cleanup(&temp_dir);
        let mut failure = json!({
            "ok": false,
            "format": "pdf",
            "status": validation.get("status").cloned().unwrap_or(json!("failed")),
            "error": "PDF validation failed",
            "validation": validation,
        });
        if let Some(install) = validation.get("install").filter(|v| !v.is_null()) {
            failure
                .as_object_mut()
                .map(|m| m.insert("install".to_owned(), install.clone()));
        }
        return failure;
    }

    if let Err(e) =
        std::fs::rename(&pdf_path, path).or_else(|_| std::fs::copy(&pdf_path, path).map(|_| ()))
    {
        cleanup(&temp_dir);
        return json!({
            "ok": false,
            "format": "pdf",
            "status": "failed",
            "error": format!("OSError: {e}"),
        });
    }
    cleanup(&temp_dir);

    json!({
        "ok": true,
        "format": "pdf",
        "path": path.to_string_lossy(),
        "bytes": std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        "findings": model.pointer("/summary/counts/findings/projected_count").cloned().unwrap_or(Value::Null),
        "pages": model.pointer("/summary/counts/pages/projected_count").cloned().unwrap_or(Value::Null),
        "lang": lang,
        "validation": validation,
    })
}
