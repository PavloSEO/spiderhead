//! Metrika-traffic dashboard: oracle parity and ported renderer tests.

use std::path::PathBuf;

use serde_json::{json, Value};
use spiderhead_reports::svg_charts::{bar_chart, donut_chart, line_chart, ChartOpts, Series};
use spiderhead_reports::traffic_dashboard::{load_brand, render_html};

fn fixtures() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reports_oracle.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("fixture file"))
        .expect("fixture json")
}

fn traffic() -> Value {
    fixtures()["traffic"].clone()
}

fn document(case: &str) -> Value {
    traffic()[case]["document"].clone()
}

fn render(document: &Value, brand: Option<&Value>, lang: Option<&str>) -> String {
    render_html(document, brand, lang).expect("render")
}

// from gen: traffic_cases() rendered dashboard HTML
#[test]
fn traffic_dashboard_parity() {
    let cases = traffic();
    for (name, case) in cases.as_object().unwrap() {
        let document = &case["document"];
        for key in ["html_en", "html_ru", "html_brand"] {
            let brand = (key == "html_brand").then(|| &case["brand"]);
            let lang = (key == "html_ru").then_some("ru");
            match case.get(key) {
                Some(want) => {
                    let got = render_html(document, brand, lang)
                        .unwrap_or_else(|e| panic!("{name}:{key}: {e}"));
                    assert_eq!(want.as_str().unwrap(), got, "{name}:{key}");
                }
                None => {
                    let want_err = case
                        .get(format!("{key}_error"))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    match (want_err, render_html(document, brand, lang)) {
                        (Some(want), Err(got)) => assert_eq!(want, got, "{name}:{key} error"),
                        (Some(want), Ok(_)) => panic!("{name}:{key}: wanted error {want}"),
                        (None, Err(e)) => panic!("{name}:{key}: {e}"),
                        (None, Ok(_)) => {}
                    }
                }
            }
        }
    }
}

/// `f"{value:,.0f}"` for a non-negative integer value.
fn grouped(value: f64, sep: char) -> String {
    let digits = format!("{value:.0}");
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(sep);
        }
        out.push(c);
    }
    out
}

#[test]
fn rendered_html_is_static_and_self_contained() {
    let html = render(&document("full"), None, None);
    let lowered = html.to_lowercase();
    assert!(!lowered.contains("<script"));
    assert!(!lowered.contains(" src=") && !lowered.contains("<link"));
    assert!(!lowered.contains("@import") && !lowered.contains("url("));
    assert!(html.contains("@page { size: 297mm 210mm"));
    assert!(html.matches("<section class=\"page").count() >= 12);
    assert!(html.contains("Summary") && html.contains("Traffic channels"));
    assert!(html.contains("<svg"));
}

#[test]
fn rendered_html_formats_document_values() {
    let doc = document("full");
    let html = render(&doc, None, None);
    let visits = &doc["blocks"]["summary"]["metrics"]["visits"];
    let value = visits["value"].as_f64().unwrap();
    let delta = visits["previous_delta_pct"].as_f64().unwrap();
    assert!(html.contains(&grouped(value, ',')));
    assert!(html.contains(&format!("+{delta:.1}%")));
    assert!(html.contains("example.com/guide/choosing-a-plan/"));
}

#[test]
fn unavailable_blocks_render_as_explicit_panels() {
    let html = render(&document("unavailable"), None, None);
    assert!(html.contains("Data unavailable"));
    assert!(html.contains("synthetic rejection"));
}

#[test]
fn russian_labels_and_number_format() {
    let doc = document("full");
    let html = render(&doc, None, Some("ru"));
    assert!(html.contains("Итоги") && html.contains("Каналы трафика"));
    assert!(html.contains("lang=\"ru\""));
    let visits = doc["blocks"]["summary"]["metrics"]["visits"]["value"]
        .as_f64()
        .unwrap();
    assert!(html.contains(&grouped(visits, '\u{a0}')));
}

#[test]
fn brand_colours_and_name_are_applied() {
    let doc = document("full");
    let brand = json!({
        "name": "Example Co",
        "accent": "#B71C1C",
        "ink": "#0E2247",
        "logo_text": "EXAMPLE",
    });
    let html = render(&doc, Some(&brand), None);
    assert!(html.contains("#B71C1C") && html.contains("#0E2247") && html.contains("EXAMPLE"));
    let dir = std::env::temp_dir().join(format!("reports-brand-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("brand.json");
    std::fs::write(&path, serde_json::to_string(&brand).unwrap()).unwrap();
    let from_path = load_brand(Some(&json!(path.to_str().unwrap()))).unwrap();
    assert_eq!(from_path.accent, "#B71C1C");
    let from_text = load_brand(Some(&json!(serde_json::to_string(&brand).unwrap()))).unwrap();
    assert_eq!(from_text.name, "Example Co");
    assert!(render(&doc, None, None).contains("#2F5BD3"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn invalid_brand_is_refused() {
    let cases: Vec<(Value, &str)> = vec![
        (json!({"accent": "red"}), "#RRGGBB"),
        (json!({"accent": "#B71C1C;}body{"}), "#RRGGBB"),
        (
            json!({"font_stack": "Arial; } * { color: red"}),
            "font_stack",
        ),
        (json!({"colour": "#B71C1C"}), "unknown brand keys"),
        (json!("missing-brand-file.json"), "not found"),
        (json!({"name": "x".repeat(81)}), "80"),
    ];
    for (brand, message) in cases {
        let err = load_brand(Some(&brand)).expect_err("brand must fail");
        assert!(err.contains(message), "{brand}: {err} missing {message}");
    }
}

#[test]
fn renderer_refuses_other_documents() {
    let err = render_html(&json!({"schema": "spiderhead.site-audit/1"}), None, None)
        .expect_err("must refuse");
    assert!(err.contains("not a spiderhead.metrika-traffic/1"), "{err}");
}

#[test]
fn search_console_rows_paginate_into_their_own_pages() {
    let html = render(&document("gsc-pages"), None, None);
    assert_eq!(2, html.matches("Search Console queries</h2>").count());
    assert!(html.contains("example query 29"));
}

#[test]
fn bar_labels_thin_out_instead_of_overlapping() {
    let labels: Vec<String> = (0..90).map(|i| i.to_string()).collect();
    let values: Vec<Option<f64>> = (0..90).map(|i| Some(10_000.0 + i as f64 * 37.0)).collect();
    let svg = bar_chart(
        &labels,
        &[Series::new("v", values, "#2F5BD3")],
        &ChartOpts::new(600.0, 200.0, |v| grouped(v, ',')),
    );
    let value_labels = svg.matches("font-weight=\"600\"").count();
    assert!(value_labels > 0 && value_labels < 90);
    assert!(svg.contains("rect"));
}

#[test]
fn donut_skips_labels_on_slivers() {
    let svg = donut_chart(
        &[97.0, 2.0, 1.0],
        &[
            "#111111".to_string(),
            "#222222".to_string(),
            "#333333".to_string(),
        ],
        &["97%".to_string(), "2%".to_string(), "1%".to_string()],
        200.0,
        None,
        None,
        "#334155",
        11.0,
        0.045,
        None,
    );
    assert!(svg.contains("97%") && !svg.contains("2%") && !svg.contains("1%"));
}

#[test]
fn line_chart_labels_peak_and_last_point() {
    let labels: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
    let svg = line_chart(
        &labels,
        &[Series::new(
            "v",
            vec![Some(5.0), Some(40.0), Some(12.0), Some(20.0)],
            "#2F5BD3",
        )],
        &ChartOpts::new(500.0, 200.0, |v| format!("{v:.0}")),
    );
    assert!(svg.contains(">40<") && svg.contains(">20<") && svg.contains(">5<"));
}
