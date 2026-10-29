//! Ported tests for `reports/pdf_validation.rs` (reference
//! the reference suite): structural, size, text, and
//! model-conservation checks for temporary audit PDFs.
//!
//! `test_missing_pypdf_is_skipped_with_an_install_command` is not ported:
//! `lopdf` is bundled in this port, so there is no optional-extra path to skip.
//! The boolean limit-rejection params are reference type checks; the Rust
//! signature takes `i64`, so only the non-positive cases apply.

use std::path::{Path, PathBuf};

use lopdf::dictionary;
use serde_json::{json, Value};
use spiderhead_reports::pdf_validation::{
    validate_pdf_output, DEFAULT_BYTE_LIMIT, DEFAULT_PAGE_LIMIT,
};

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pdf-validation-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `_write_text_pdf(path, pages)`: one page per string, Helvetica text op.
fn write_text_pdf(path: &Path, pages: &[&str]) {
    let mut doc = lopdf::Document::with_version("1.4");
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let pages_id = doc.new_object_id();
    let mut kids = Vec::new();
    for text in pages {
        let escaped = text
            .replace('\\', "\\\\")
            .replace('(', "\\(")
            .replace(')', "\\)");
        let stream = lopdf::Stream::new(
            dictionary! {},
            format!("BT /F1 12 Tf 36 720 Td ({escaped}) Tj ET").into_bytes(),
        );
        let content_id = doc.add_object(lopdf::Object::Stream(stream));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => font_id },
            },
            "Contents" => content_id,
        });
        kids.push(page_id.into());
    }
    let count = kids.len() as i64;
    doc.objects.insert(
        pages_id,
        lopdf::Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => count,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);
    doc.save(path).expect("save text pdf");
}

/// `_model()`: the sentinel model with a single finding, page, check,
/// backlog item, and no omissions.
fn model() -> Value {
    json!({
        "schema": "spiderhead.technical-audit-pdf/1",
        "run": {"state": "partial", "reasons": ["Synthetic run reason"]},
        "summary": {
            "counts": {
                "findings": {"source_count": 1, "projected_count": 1},
                "pages": {"source_count": 1, "projected_count": 1},
                "checks": {
                    "ran": {"source_count": 1, "projected_count": 1},
                    "failed": {"source_count": 0, "projected_count": 0},
                    "skipped": {"source_count": 0, "projected_count": 0},
                    "disabled": {"source_count": 0, "projected_count": 0},
                    "capabilities": {"source_count": 0, "projected_count": 0},
                    "silent": {"source_count": 0, "projected_count": 0},
                },
                "backlog": {
                    "source_count": 1,
                    "projected_count": 1,
                    "state": "recorded",
                },
            },
        },
        "findings": [{
            "record": {"url": "https://example.invalid/finding-0001/"},
            "display": {
                "title": "Finding sentinel 0001",
                "observation": "Synthetic observation sentinel 0001",
                "reproduction": "Synthetic reproduction sentinel 0001",
            },
        }],
        "pages": [{
            "record": {
                "url": "https://example.invalid/page-0001/",
                "title": "Page sentinel 0001",
            },
        }],
        "coverage": {"checks": [{"id": "CHECK_SENTINEL_0001", "reason": "Check reason sentinel"}]},
        "backlog": {"items": [{"record": {"title": "Backlog sentinel 0001"}}]},
        "omissions": [],
    })
}

fn sentinels() -> Vec<&'static str> {
    vec![
        "Finding sentinel 0001",
        "Synthetic observation sentinel 0001",
        "Synthetic reproduction sentinel 0001",
        "https://example.invalid/finding-0001/",
        "https://example.invalid/page-0001/",
        "Page sentinel 0001",
        "Backlog sentinel 0001",
        "Synthetic run reason",
        "CHECK_SENTINEL_0001",
        "Check reason sentinel",
    ]
}

/// `_text_pdf(path)`: the valid one-page PDF holding every sentinel.
fn text_pdf(path: &Path) -> PathBuf {
    write_text_pdf(
        path,
        &["Finding sentinel 0001 Synthetic observation sentinel 0001 \
           Synthetic reproduction sentinel 0001 https://example.invalid/finding-0001/ \
           https://example.invalid/page-0001/ Page sentinel 0001 Backlog sentinel 0001 \
           Synthetic run reason CHECK_SENTINEL_0001 Check reason sentinel"],
    );
    path.to_path_buf()
}

fn validate(path: &Path, model: &Value) -> Value {
    validate_pdf_output(path, model, DEFAULT_PAGE_LIMIT, DEFAULT_BYTE_LIMIT)
        .expect("validate_pdf_output")
}

#[test]
fn valid_pdf_checks_signature_text_page_and_model_conservation() {
    let dir = tmp("valid");
    let path = text_pdf(&dir.join("audit.pdf"));

    let result = validate(&path, &model());

    assert_eq!(result["status"], json!("ok"));
    assert_eq!(result["page_count"], json!(1));
    assert_eq!(
        result["size_bytes"].as_i64().unwrap() as u64,
        std::fs::metadata(&path).unwrap().len()
    );
    assert_eq!(result["errors"], json!([]));
}

#[test]
fn pdf_page_and_byte_limits_accept_values_at_the_boundary() {
    let dir = tmp("boundary");
    let path = text_pdf(&dir.join("audit.pdf"));
    let size = std::fs::metadata(&path).unwrap().len() as i64;

    let at_boundary = validate_pdf_output(&path, &model(), 1, size).expect("validate");

    assert_eq!(at_boundary["status"], json!("ok"));
    assert_eq!(at_boundary["page_count"], json!(1));
    assert_eq!(at_boundary["size_bytes"], json!(size));
}

#[test]
// (the boolean params are reference type checks with no `i64` analogue)
fn nonpositive_limits_are_rejected() {
    let dir = tmp("nonpositive");
    let unused = dir.join("unused.pdf");

    for (page_limit, byte_limit, message) in [(0, 1, "page_limit"), (1, 0, "byte_limit")] {
        let err = validate_pdf_output(&unused, &model(), page_limit, byte_limit)
            .expect_err("rejected limit");
        assert!(err.contains(message), "{err} must mention {message}");
    }
}

#[test]
fn pdf_over_the_byte_limit_fails_without_claiming_success() {
    let dir = tmp("byte-limit");
    let path = text_pdf(&dir.join("audit.pdf"));
    let size = std::fs::metadata(&path).unwrap().len() as i64;

    let result =
        validate_pdf_output(&path, &model(), DEFAULT_PAGE_LIMIT, size - 1).expect("validate");

    assert_eq!(result["status"], json!("failed"));
    assert_eq!(result["page_count"], Value::Null);
    assert!(result["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e.as_str().unwrap().contains("byte limit exceeded")));
}

#[test]
fn pdf_over_the_page_limit_fails_without_extracting_a_complete_result() {
    let dir = tmp("page-limit");
    let path = dir.join("two-pages.pdf");
    write_text_pdf(&path, &["first synthetic page", "second synthetic page"]);

    let result = validate_pdf_output(&path, &model(), 1, DEFAULT_BYTE_LIMIT).expect("validate");

    assert_eq!(result["status"], json!("failed"));
    assert_eq!(result["page_count"], json!(2));
    assert_eq!(result["errors"], json!(["PDF page limit exceeded: 2 > 1"]));
}

#[test]
fn invalid_signature_and_text_blank_pages_fail() {
    let dir = tmp("invalid");

    let invalid = dir.join("not-pdf.pdf");
    std::fs::write(&invalid, b"not a pdf").unwrap();
    let invalid_result = validate(&invalid, &model());
    assert_eq!(invalid_result["status"], json!("failed"));
    assert!(invalid_result["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e
            .as_str()
            .unwrap()
            .contains("temporary output does not start with a PDF signature")));

    let blank = dir.join("blank.pdf");
    write_text_pdf(&blank, &[""]);
    let blank_result = validate(&blank, &model());
    assert_eq!(blank_result["status"], json!("failed"));
    assert!(blank_result["errors"].as_array().unwrap().iter().any(|e| e
        .as_str()
        .unwrap()
        .contains("PDF page 1 has no extractable text")));
}

#[test]
fn pdf_validation_fails_when_projected_count_disagrees_with_model_rows() {
    let dir = tmp("projected");
    let mut model = model();
    model["summary"]["counts"]["findings"]["projected_count"] = json!(2);

    let result = validate(&text_pdf(&dir.join("audit.pdf")), &model);

    assert_eq!(result["status"], json!("failed"));
    assert!(result["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e.as_str().unwrap().contains("findings count mismatch")));
}

#[test]
fn pdf_model_check_conservation_includes_page_tool_failures() {
    let dir = tmp("page-tool");
    let mut model = model();
    model["summary"]["counts"]["checks"]["page_tools_failed"] = json!({
        "source_count": 1,
        "projected_count": 1,
    });
    model["coverage"]["checks"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": "PAGE_TOOL_FAILED", "reason": "Synthetic page tool failure"}));
    let mut text: Vec<String> = sentinels().iter().map(|s| s.to_string()).collect();
    text.push("PAGE_TOOL_FAILED".to_owned());
    text.push("Synthetic page tool failure".to_owned());
    let path = dir.join("page-tool-failure.pdf");
    write_text_pdf(&path, &[&text.join(" ")]);

    let result = validate(&path, &model);

    assert_eq!(result["status"], json!("ok"));
    assert_eq!(result["errors"], json!([]));
}

#[test]
fn pdf_validation_fails_when_a_source_row_is_unaccounted_for() {
    let dir = tmp("unaccounted");
    let mut model = model();
    model["summary"]["counts"]["findings"]["source_count"] = json!(2);

    let result = validate(&text_pdf(&dir.join("audit.pdf")), &model);

    assert_eq!(result["status"], json!("failed"));
    assert!(result["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e.as_str().unwrap().contains("omits source rows")));
}

#[test]
fn pdf_text_must_retain_each_expected_model_sentinel() {
    let dir = tmp("missing-row");
    let path = dir.join("missing-row.pdf");
    write_text_pdf(&path, &["Finding sentinel 0001 Page sentinel 0001"]);

    let result = validate(&path, &model());

    assert_eq!(result["status"], json!("failed"));
    assert!(result["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e.as_str().unwrap().contains("Backlog sentinel 0001")));
}

#[test]
fn pdf_text_accepts_localized_generated_finding_title_and_reproduction() {
    let dir = tmp("localized");
    let mut model = model();
    model["findings"][0]["display"]["title"] = json!("Title element is missing");
    model["findings"][0]["display"]["reproduction"] =
        json!("At https://example.invalid/finding-0001/: Synthetic observation sentinel 0001");
    model["findings"][0]["record"]["check"] = json!("TITLE_MISSING");
    let mut source_sentinels: Vec<String> = sentinels()
        .into_iter()
        .filter(|s| *s != "Finding sentinel 0001" && *s != "Synthetic reproduction sentinel 0001")
        .map(str::to_owned)
        .collect();
    source_sentinels.push("TITLE_MISSING".to_owned());
    let path = dir.join("localized.pdf");
    write_text_pdf(&path, &[&source_sentinels.join(" ")]);

    let result = validate(&path, &model);

    assert_eq!(result["status"], json!("ok"));
    assert_eq!(result["errors"], json!([]));
}

#[test]
fn pdf_model_omissions_must_be_recorded_for_source_count_shortfalls() {
    let dir = tmp("omissions");
    let mut model = model();
    model["summary"]["counts"]["findings"]["source_count"] = json!(2);
    model["omissions"] = json!([{"reason": "Synthetic omitted source row 0002"}]);
    let text = format!(
        "Synthetic omitted source row 0002 {}",
        sentinels().join(" ")
    );
    let path = dir.join("omission.pdf");
    write_text_pdf(&path, &[&text]);

    let result = validate(&path, &model);

    assert_eq!(result["status"], json!("ok"));
}
