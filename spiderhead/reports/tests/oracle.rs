//! Differential tests against the reference baseline fixtures.
//!
//! the reference suite (run with the oracle venv) emits
//! `tests/fixtures/reports_oracle.json`; every assertion here compares Rust
//! output against that captured reference output — zero diffs.

use std::path::PathBuf;

use serde_json::{json, Value};
use spiderhead_reports::client_findings::{
    check_title, finding_exclusion_report, project_document,
};
use spiderhead_reports::document::{detect_kind, normalize_sf_audit, safe_segment, ReportDoc};
use spiderhead_reports::evidence_summary;
use spiderhead_reports::facts::build_facts_export;
use spiderhead_reports::svg_charts::{bar_chart, donut_chart, line_chart, ChartOpts, Series};

fn fixtures() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reports_oracle.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture file"))
        .expect("fixture json")
}

fn documents() -> Vec<Value> {
    fixtures()
        .get("documents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn assert_json_eq(context: &str, expected: &Value, actual: &Value) {
    assert_eq!(
        expected,
        actual,
        "{context}\nEXPECTED: {}\nACTUAL: {}",
        serde_json::to_string_pretty(expected).unwrap_or_default(),
        serde_json::to_string_pretty(actual).unwrap_or_default()
    );
}

// from gen: detect() per document
#[test]
fn detect_parity() {
    for doc in documents() {
        let name = doc["name"].as_str().unwrap_or_default();
        let detect = &doc["detect"];
        // The fixture stores only outputs; the input document is embedded
        // through `run_doc` — rebuild inputs via the same report_docs module on
        // the reference side. Here inputs ride along in `input`.
        let input = doc.get("input").expect("fixture has input");
        let (kind, error) = detect_kind(input);
        let expected = detect["kind"].as_array().expect("kind pair");
        let want_kind = expected[0].as_str();
        let want_error = expected[1].as_str();
        assert_eq!(want_kind, kind, "{name}: kind");
        assert_eq!(want_error, error.as_deref(), "{name}: detection error");
    }
}

// from gen: normalize() per sf-audit document
#[test]
fn normalize_parity() {
    for doc in documents() {
        let name = doc["name"].as_str().unwrap_or_default();
        let input = doc.get("input").expect("fixture has input");
        if let Some(expected) = doc.get("normalized") {
            assert_json_eq(
                &format!("{name}: normalized"),
                expected,
                &normalize_sf_audit(input),
            );
        }
    }
}

// from gen: project() per document
#[test]
fn project_parity() {
    for doc in documents() {
        let name = doc["name"].as_str().unwrap_or_default();
        let input = doc.get("input").expect("fixture has input");
        let (kind, _) = detect_kind(input);
        if kind.is_none() {
            continue;
        }
        let normalized = if kind == Some("sf-audit") {
            normalize_sf_audit(input)
        } else {
            input.clone()
        };
        assert_json_eq(
            &format!("{name}: projected"),
            doc.get("projected").unwrap_or(&Value::Null),
            &project_document(&normalized),
        );
    }
}

// from gen: md_output() per document
#[test]
fn md_parity() {
    for doc in documents() {
        let Some(expected) = doc.get("md").and_then(Value::as_str) else {
            continue;
        };
        let name = doc["name"].as_str().unwrap_or_default();
        let projected = doc.get("projected").cloned().unwrap_or(Value::Null);
        let mut buffer = Vec::new();
        spiderhead_reports::md::render(&projected as &dyn ReportDoc, &mut buffer)
            .expect("md render");
        assert_eq!(expected, String::from_utf8(buffer).unwrap(), "{name}: md");
    }
}

// from gen: csv_outputs() per document
#[test]
fn csv_parity() {
    let dir = std::env::temp_dir().join(format!("reports-oracle-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for doc in documents() {
        let Some(expected) = doc.get("csv") else {
            continue;
        };
        let name = doc["name"].as_str().unwrap_or_default();
        let projected = doc.get("projected").cloned().unwrap_or(Value::Null);
        let target = dir.join(format!("{name}.csv"));
        spiderhead_reports::csvfile::write(&projected as &dyn ReportDoc, &target)
            .expect("csv write");
        for (suffix, want) in expected.as_object().unwrap() {
            let produced = target.with_extension(suffix.trim_start_matches('.'));
            let bytes =
                std::fs::read(&produced).unwrap_or_else(|e| panic!("{name}: {produced:?}: {e}"));
            let bom = bytes.starts_with(b"\xef\xbb\xbf");
            assert_eq!(want["bom"].as_bool().unwrap(), bom, "{name}{suffix}: bom");
            let text = String::from_utf8(if bom { bytes[3..].to_vec() } else { bytes }).unwrap();
            assert_eq!(want["text"].as_str().unwrap(), text, "{name}{suffix}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// from gen: docx_blocks() per document (extracted structure, not bytes)
#[test]
fn docx_parity() {
    let dir = std::env::temp_dir().join(format!("reports-docx-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for doc in documents() {
        let Some(expected) = doc.get("docx") else {
            continue;
        };
        let name = doc["name"].as_str().unwrap_or_default();
        let projected = doc.get("projected").cloned().unwrap_or(Value::Null);
        let target = dir.join(format!("{name}.docx"));
        spiderhead_reports::docx::write(&projected as &dyn ReportDoc, &target).expect("docx write");
        let actual = extract_docx_blocks(&target);
        assert_json_eq(&format!("{name}: docx"), expected, &actual);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Minimal OOXML block extractor: paragraphs (styleId + text) and tables.
fn extract_docx_blocks(path: &std::path::Path) -> Value {
    let file = std::fs::File::open(path).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut zip.by_name("word/document.xml").unwrap(), &mut xml)
        .unwrap();
    let mut blocks = Vec::new();
    let mut pos = 0;
    while pos < xml.len() {
        if xml[pos..].starts_with("<w:p>") || xml[pos..].starts_with("<w:p ") {
            let end = xml[pos..].find("</w:p>").map(|i| pos + i + 6).unwrap();
            blocks.push(parse_paragraph(&xml[pos..end]));
            pos = end;
        } else if xml[pos..].starts_with("<w:tbl>") {
            let end = xml[pos..].find("</w:tbl>").map(|i| pos + i + 8).unwrap();
            blocks.push(parse_table(&xml[pos..end]));
            pos = end;
        } else {
            pos += 1;
        }
    }
    json!(blocks)
}

fn parse_paragraph(xml: &str) -> Value {
    let style = xml
        .find("w:pStyle w:val=\"")
        .and_then(|i| {
            let rest = &xml[i + 16..];
            rest.find('"').map(|e| rest[..e].to_string())
        })
        .unwrap_or_else(|| "Normal".to_owned());
    let mut text = String::new();
    let mut pos = 0;
    while pos < xml.len() {
        if xml[pos..].starts_with("<w:t ") || xml[pos..].starts_with("<w:t>") {
            let open = xml[pos..].find('>').map(|i| pos + i + 1).unwrap();
            let close = xml[open..].find("</w:t>").map(|i| open + i).unwrap();
            text.push_str(&unescape(&xml[open..close]));
            pos = close + 6;
        } else if xml[pos..].starts_with("<w:br/>") {
            text.push('\n');
            pos += 7;
        } else {
            pos += 1;
        }
    }
    json!({"type": "p", "style": json!(style), "text": text})
}

fn parse_table(xml: &str) -> Value {
    let mut rows = Vec::new();
    let mut pos = 0;
    while let Some(start) = xml[pos..].find("<w:tr>") {
        let tr = pos + start + 6;
        let tr_end = xml[tr..].find("</w:tr>").map(|i| tr + i).unwrap();
        let mut cells = Vec::new();
        let mut cpos = tr;
        while let Some(cs) = xml[cpos..tr_end].find("<w:tc>") {
            let cell_start = cpos + cs + 6;
            let cell_end = xml[cell_start..tr_end]
                .find("</w:tc>")
                .map(|i| cell_start + i)
                .unwrap_or(tr_end);
            cells.push(json!(cell_text(&xml[cell_start..cell_end])));
            cpos = cell_end + 6;
        }
        rows.push(json!(cells));
        pos = tr_end + 7;
    }
    json!({"type": "table", "rows": rows})
}

fn cell_text(xml: &str) -> String {
    // `cell.text` joins paragraph texts with '\n'.
    let mut parts = Vec::new();
    let mut pos = 0;
    while let Some(start) = find_text_open(&xml[pos..]) {
        let open = pos + start;
        let open_end = xml[open..].find('>').map(|i| open + i + 1).unwrap();
        let close = xml[open_end..]
            .find("</w:t>")
            .map(|i| open_end + i)
            .unwrap();
        parts.push(unescape(&xml[open_end..close]));
        pos = close + 6;
    }
    parts.join("")
}

/// Position of the next `<w:t>`/`<w:t ` open tag (never `<w:tcW`).
fn find_text_open(xml: &str) -> Option<usize> {
    let mut pos = 0;
    loop {
        let at = xml[pos..].find("<w:t")?;
        let abs = pos + at;
        if xml[abs..].starts_with("<w:t ") || xml[abs..].starts_with("<w:t>") {
            return Some(abs);
        }
        pos = abs + 4;
    }
}

fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&")
}

// from gen: evidence_rows() per document
#[test]
fn evidence_rows_parity() {
    for doc in documents() {
        let Some(expected) = doc.get("evidence_rows") else {
            continue;
        };
        let name = doc["name"].as_str().unwrap_or_default();
        let projected = doc.get("projected").cloned().unwrap_or(Value::Null);
        let summary = projected.get("summary").cloned().unwrap_or(json!({}));
        let actual: Vec<Vec<String>> = evidence_summary::rows(&summary);
        assert_json_eq(&format!("{name}: evidence"), expected, &json!(actual));
    }
}

// from gen: exclusions() per document
#[test]
fn exclusions_parity() {
    for doc in documents() {
        let Some(expected) = doc.get("exclusions") else {
            continue;
        };
        let name = doc["name"].as_str().unwrap_or_default();
        let projected = doc.get("projected").cloned().unwrap_or(Value::Null);
        let summary = projected.get("summary").cloned().unwrap_or(json!({}));
        let suppressed = projected
            .get("suppressed_issues")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let actual = finding_exclusion_report(&summary, Some(&suppressed)).unwrap_or(Value::Null);
        assert_json_eq(&format!("{name}: exclusions"), expected, &actual);
    }
}

// from gen: facts_export() per sf-audit document
#[test]
fn facts_parity() {
    for doc in documents() {
        let Some(expected) = doc.get("facts") else {
            continue;
        };
        let name = doc["name"].as_str().unwrap_or_default();
        let input = doc.get("input").expect("fixture has input");
        let (kind, _) = detect_kind(input);
        let sites = match kind {
            Some("site-audit") => json!([{
                "label": "example.com",
                "crawl_audit": Value::Null,
                "site_audit": input,
            }]),
            _ => json!([{
                "label": "example.com",
                "crawl_audit": input,
                "site_audit": Value::Null,
            }]),
        };
        match build_facts_export(sites.as_array().unwrap()) {
            Ok(export) => assert_json_eq(&format!("{name}: facts"), &expected["export"], &export),
            Err(err) => assert_eq!(
                expected["error"].as_str().unwrap_or_default(),
                err.to_string(),
                "{name}: facts error"
            ),
        }
    }
}

// from gen: check_titles()
#[test]
fn check_title_parity() {
    let expected = &fixtures()["check_titles"];
    for (input, want) in expected.as_object().unwrap() {
        assert_eq!(
            want.as_str().unwrap(),
            check_title(&json!(input)),
            "check_title({input})"
        );
    }
}

// from gen: safe_segments()
#[test]
fn safe_segment_parity() {
    let expected = fixtures()["safe_segments"].as_array().unwrap().clone();
    let inputs = [
        json!("example.com"),
        json!("a/b\\c:d*e?f\"g<h>i|j"),
        json!("with  spaces\ttab"),
        json!(""),
        Value::Null,
        json!("x".repeat(200)),
    ];
    for (want, input) in expected.iter().zip(inputs.iter()) {
        assert_eq!(
            want.as_str().unwrap(),
            safe_segment(input),
            "safe_segment({input})"
        );
    }
}

// from gen: svg_charts() four chart kinds
#[test]
fn svg_parity() {
    let expected = &fixtures()["svg"];
    let labels = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let fmt = |v: f64| format!("{v:.0}");
    let mut opts = ChartOpts::new(320.0, 180.0, fmt);
    let actual = json!({
        "bar": bar_chart(&labels, &[Series::new("s1", vec![Some(3.0), Some(1.0), Some(4.0)], "#3366aa")], &opts),
        "line": line_chart(
            &labels,
            &[
                Series::new("s1", vec![Some(3.0), None, Some(4.0)], "#3366aa"),
                { let mut s = Series::new("s2", vec![Some(1.0), Some(2.0), Some(0.0)], "#aa3366"); s.dashed = true; s },
            ],
            &opts,
        ),
        "donut": donut_chart(
            &[3.0, 1.0, 0.0],
            &["#111".to_string(), "#222".to_string(), "#333".to_string()],
            &["60%".to_string(), "20%".to_string(), String::new()],
            160.0,
            Some("4"),
            Some("findings"),
            "#334155",
            11.0,
            0.045,
            None,
        ),
        "donut-empty": donut_chart(&[0.0], &["#111".to_string()], &[String::new()], 160.0, None, None, "#334155", 11.0, 0.045, None),
    });
    opts.title = Some("grouped".to_string());
    let grouped = bar_chart(
        &labels,
        &[
            Series::new("s1", vec![Some(3.0), Some(1.0), Some(4.0)], "#3366aa"),
            Series::new("s2", vec![Some(2.0), Some(5.0), Some(1.0)], "#aa3366"),
        ],
        &opts,
    );
    for (key, want) in expected.as_object().unwrap() {
        let got = if key == "bar-grouped" {
            grouped.clone()
        } else {
            actual[key].as_str().unwrap_or_default().to_string()
        };
        assert_eq!(want.as_str().unwrap(), got, "svg:{key}");
    }
}

// from gen: xlsx_output() per document (extracted cells + index, not bytes)
#[test]
fn xlsx_parity() {
    let dir = std::env::temp_dir().join(format!("reports-xlsx-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for doc in documents() {
        let Some(expected) = doc.get("xlsx") else {
            continue;
        };
        let name = doc["name"].as_str().unwrap_or_default();
        let projected = doc.get("projected").cloned().unwrap_or(Value::Null);
        let target = dir.join("report.xlsx");
        spiderhead_reports::xlsx::write(&projected as &dyn ReportDoc, &target)
            .unwrap_or_else(|e| panic!("{name}: xlsx write: {e}"));
        let actual = extract_xlsx(&target);
        assert_json_eq(
            &format!("{name}: xlsx sheets"),
            &expected["sheets"],
            &actual,
        );
        // Index sidecar minus volatile fields.
        let index_path = target.with_file_name("report.xlsx.index.json");
        let mut index: Value =
            serde_json::from_str(&std::fs::read_to_string(&index_path).expect("index.json"))
                .unwrap();
        index.as_object_mut().unwrap().remove("workbook_sha256");
        index.as_object_mut().unwrap().remove("workbook_bytes");
        assert_json_eq(&format!("{name}: xlsx index"), &expected["index"], &index);
        let _ = std::fs::remove_file(&index_path);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// from gen: cohorts_cases() — publication + gsc projections, files and errors
#[test]
fn cohorts_parity() {
    let cases = fixtures()["cohorts"].as_object().unwrap().clone();
    let dir = std::env::temp_dir().join(format!("reports-cohorts-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, case) in &cases {
        let input = &case["input"];
        let out = dir.join(name.replace(':', "-"));
        let _ = std::fs::remove_dir_all(&out);
        let result = if name.starts_with("publication:") {
            spiderhead_reports::cohorts::publication_cohorts(
                Some(input),
                None,
                Some(out.to_str().unwrap()),
            )
        } else {
            spiderhead_reports::cohorts::gsc_progress(
                Some(input),
                None,
                Some(out.to_str().unwrap()),
            )
        };
        match (case.get("error"), result) {
            (Some(want), Err(got)) => {
                assert_eq!(want.as_str().unwrap(), got.to_string(), "{name}: error");
            }
            (None, Ok(result)) => {
                let mut result = result.as_object().unwrap().clone();
                result.remove("output_directory");
                result.remove("manifest");
                assert_json_eq(
                    &format!("{name}: result"),
                    &case["result"],
                    &Value::Object(result),
                );
                let mut files = serde_json::Map::new();
                for entry in std::fs::read_dir(&out).unwrap() {
                    let path = entry.unwrap().path();
                    let fname = path.file_name().unwrap().to_str().unwrap().to_string();
                    let bytes = std::fs::read(&path).unwrap();
                    if fname == "manifest.json" {
                        files.insert(
                            fname,
                            serde_json::from_slice(&bytes).expect("manifest json"),
                        );
                    } else {
                        files.insert(
                            fname,
                            Value::String(String::from_utf8(bytes).expect("utf8")),
                        );
                    }
                }
                assert_json_eq(
                    &format!("{name}: files"),
                    &case["files"],
                    &Value::Object(files),
                );
            }
            (Some(want), Ok(got)) => panic!("{name}: wanted error {want}, got {got}"),
            (None, Err(e)) => panic!("{name}: {e}"),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// from gen: looker_cases() — copy-link builder results and contract errors
#[test]
fn looker_parity() {
    let cases = fixtures()["looker"].as_object().unwrap().clone();
    for (name, case) in &cases {
        let input = &case["input"];
        let result = spiderhead_reports::looker_link::build_looker_copy_link(
            &input["blueprint"],
            input
                .get("original_report_id")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            input
                .get("original_report_confirmed")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            input
                .get("report_name")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            input.get("data_sources").unwrap_or(&Value::Null),
        );
        match (case.get("error"), result) {
            (Some(want), Err(got)) => {
                assert_eq!(want.as_str().unwrap(), got.to_string(), "{name}: error");
            }
            (None, Ok(got)) => assert_json_eq(&format!("{name}: result"), &case["result"], &got),
            (Some(want), Ok(got)) => panic!("{name}: wanted error {want}, got {got}"),
            (None, Err(e)) => panic!("{name}: {e}"),
        }
    }
}

/// Extract every worksheet's cell matrix via calamine, normalized to JSON.
fn extract_xlsx(path: &std::path::Path) -> Value {
    use calamine::{Data, Reader, Xlsx};
    let mut wb: Xlsx<std::io::BufReader<std::fs::File>> =
        calamine::open_workbook(path).expect("open xlsx");
    let mut sheets = serde_json::Map::new();
    for name in wb.sheet_names().to_vec() {
        let range = wb.worksheet_range(&name).expect("sheet range");
        let mut rows: Vec<Value> = Vec::new();
        for row in range.rows() {
            rows.push(Value::Array(
                row.iter()
                    .map(|cell| match cell {
                        Data::Empty => Value::Null,
                        Data::String(s) => Value::String(s.to_string()),
                        Data::Int(i) => json!(i),
                        Data::Float(f) => {
                            if f.fract() == 0.0 && f.abs() < 1e15 {
                                json!(*f as i64)
                            } else {
                                json!(f)
                            }
                        }
                        Data::Bool(b) => Value::Bool(*b),
                        Data::DateTime(d) => Value::String(d.to_string()),
                        Data::DateTimeIso(s) => Value::String(s.to_string()),
                        Data::DurationIso(s) => Value::String(s.to_string()),
                        Data::Error(e) => Value::String(format!("{e:?}")),
                    })
                    .collect(),
            ));
        }
        while rows.last().is_some_and(|r| {
            r.as_array()
                .is_some_and(|cells| cells.iter().all(Value::is_null))
        }) {
            rows.pop();
        }
        sheets.insert(name, Value::Array(rows));
    }
    Value::Object(sheets)
}

// from gen: pdf_case() per document — pdf_model + audit_pdf + pdf_validation
#[test]
fn pdf_parity() {
    use spiderhead_reports::audit_pdf::render_audit_pdf_html;
    use spiderhead_reports::pdf_model::build_pdf_model;
    use spiderhead_reports::pdf_validation::{
        validate_pdf_output, DEFAULT_BYTE_LIMIT, DEFAULT_PAGE_LIMIT,
    };

    let dir = std::env::temp_dir().join(format!("reports-pdf-oracle-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let empty = dir.join("empty.pdf");
    std::fs::write(&empty, b"").expect("empty pdf");
    let notpdf = dir.join("notpdf.pdf");
    std::fs::write(&notpdf, b"this is not a pdf signature").expect("not-pdf file");

    for doc in documents() {
        let name = doc["name"].as_str().unwrap_or_default();
        let Some(pdf) = doc.get("pdf") else { continue };
        let input = doc.get("input").expect("fixture has input");
        match pdf.get("model_error") {
            Some(want) => match build_pdf_model(input, &[], None, None) {
                Err(e) => assert_eq!(want.as_str().unwrap(), e.0, "{name}: model error"),
                Ok(model) => panic!(
                    "{name}: wanted model error {want}, got {}",
                    serde_json::to_string(&model).unwrap_or_default()
                ),
            },
            None => {
                let model = build_pdf_model(input, &[], None, None)
                    .unwrap_or_else(|e| panic!("{name}: model failed: {}", e.0));
                assert_json_eq(&format!("{name}: pdf model"), &pdf["model"], &model);
                for lang in ["en", "ru"] {
                    let key = format!("html_{lang}");
                    let Some(want) = pdf.get(&key) else { continue };
                    let html = render_audit_pdf_html(&model, lang, None)
                        .unwrap_or_else(|e| panic!("{name}: render {lang}: {e}"));
                    if want.as_str() != Some(html.as_str()) {
                        std::fs::write(
                            dir.join(format!("{name}-{lang}.expected.html")),
                            want.as_str().unwrap_or_default(),
                        )
                        .ok();
                        std::fs::write(dir.join(format!("{name}-{lang}.actual.html")), &html).ok();
                        panic!("{name}: html_{lang} differs (wrote to {dir:?})");
                    }
                }
                let missing = validate_pdf_output(
                    std::path::Path::new("/nonexistent-report-path.pdf"),
                    &model,
                    DEFAULT_PAGE_LIMIT,
                    DEFAULT_BYTE_LIMIT,
                )
                .expect("missing pdf result");
                assert_json_eq(
                    &format!("{name}: validation_missing_pdf"),
                    &pdf["validation_missing_pdf"],
                    &missing,
                );
                let empty_v =
                    validate_pdf_output(&empty, &model, DEFAULT_PAGE_LIMIT, DEFAULT_BYTE_LIMIT)
                        .expect("empty pdf result");
                assert_json_eq(
                    &format!("{name}: validation_empty_pdf"),
                    &pdf["validation_empty_pdf"],
                    &empty_v,
                );
                let bad =
                    validate_pdf_output(&notpdf, &model, DEFAULT_PAGE_LIMIT, DEFAULT_BYTE_LIMIT)
                        .expect("bad signature result");
                assert_json_eq(
                    &format!("{name}: validation_bad_signature"),
                    &pdf["validation_bad_signature"],
                    &bad,
                );
                let err = validate_pdf_output(
                    std::path::Path::new("/nonexistent.pdf"),
                    &model,
                    0,
                    DEFAULT_BYTE_LIMIT,
                )
                .expect_err("page_limit must fail");
                assert_eq!(
                    pdf["validation_page_limit_error"].as_str().unwrap(),
                    err,
                    "{name}: page_limit error"
                );
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
