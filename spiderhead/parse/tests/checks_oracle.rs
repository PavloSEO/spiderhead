//! Differential tests of the slice w2-p3 functions against the reference checks package.
//!
//! Fixture `fixtures/checks_oracle.json` (generator the reference suite) holds the outputs
//! of `text_normalize`, `navigation`, `link_context` and the pure `asset_weight` helpers. Every
//! record is compared field by field; a record that raised in reference must be an error here with
//! the same message. `CHECKS_FIXTURE` overrides the fixture path, `CHECKS_SHOW` the number of
//! reported divergences per module.

use std::collections::HashMap;

use serde_json::{json, Value};
use spiderhead_parse::asset_evidence::{
    check_cache_lifetime, find_missing_font_display, looks_legacy_transpiled, looks_minified,
    whitespace_ratio,
};
use spiderhead_parse::dom::parse_document;
use spiderhead_parse::link_context::extract_occurrences;
use spiderhead_parse::navigation::{
    expand_navigation, retain_navigation, validate_navigation, CaptureSignal, NavigationCapture,
};
use spiderhead_parse::py_str::is_py_space;
use spiderhead_parse::text_normalize::{
    declared_language, manifest_entry, normalization_policy, normalize_document, normalize_text,
    prepare_items, py_is_word, py_isalpha, script_evidence,
};
use spiderhead_parse::types::ContentAreaConfig;

const FIXTURE: &str = include_str!("fixtures/checks_oracle.json");

/// Collect every difference between `expected` and `actual` as a readable path.
fn walk(expected: &Value, actual: &Value, path: &str, out: &mut Vec<String>) {
    match (expected, actual) {
        (Value::Object(x), Value::Object(y)) => {
            for (key, ev) in x {
                match y.get(key) {
                    Some(av) => walk(ev, av, &format!("{path}/{key}"), out),
                    None => out.push(format!("{path}/{key}: missing, want {}", short(ev))),
                }
            }
            for (key, av) in y {
                if !x.contains_key(key) {
                    out.push(format!("{path}/{key}: extra {}", short(av)));
                }
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                out.push(format!(
                    "{path}: length want {} got {}\n    want {}\n    got  {}",
                    x.len(),
                    y.len(),
                    short(expected),
                    short(actual)
                ));
                return;
            }
            for (i, (ev, av)) in x.iter().zip(y).enumerate() {
                walk(ev, av, &format!("{path}/{i}"), out);
            }
        }
        _ if expected == actual => {}
        _ => out.push(format!(
            "{path}: want {} got {}",
            short(expected),
            short(actual)
        )),
    }
}

fn short(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() > 240 {
        format!("{}...", text.chars().take(240).collect::<String>())
    } else {
        text
    }
}

/// the reference `ValueError: message` / `KeyError: 'k'` text, with the type prefix removed.
fn raised_message(text: &str) -> &str {
    text.split_once(": ").map_or(text, |(_, message)| message)
}

fn fixture() -> Value {
    let text = match std::env::var("CHECKS_FIXTURE") {
        Ok(path) => std::fs::read_to_string(path).unwrap_or_default(),
        Err(_) => FIXTURE.to_owned(),
    };
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

fn records<'a>(section: &'a Value, name: &str) -> &'a [Value] {
    section[name].as_array().map_or(&[], Vec::as_slice)
}

fn report(module: &str, mismatches: &[String], checked: usize) {
    let limit = std::env::var("CHECKS_SHOW")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25);
    assert!(
        mismatches.is_empty(),
        "{module}: {} divergences in {checked} records:\n{}",
        mismatches.len(),
        mismatches
            .iter()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Parse a `content_area` record value (`null` = no configuration).
fn config_of(value: &Value) -> Option<ContentAreaConfig> {
    if value.is_null() {
        return None;
    }
    serde_json::from_value(value.clone()).ok()
}

/// Code-point ranges where `pred` holds, in the fixture's shape (`[first, last]` pairs).
fn ranges(pred: impl Fn(char) -> bool) -> Vec<Value> {
    let mut out = Vec::new();
    let mut start: Option<u32> = None;
    for cp in 0..=0x10FFFF_u32 {
        let ok = char::from_u32(cp).is_some_and(&pred);
        match (ok, start) {
            (true, None) => start = Some(cp),
            (false, Some(first)) => {
                out.push(json!([first, cp - 1]));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(first) = start {
        out.push(json!([first, 0x10FFFF]));
    }
    out
}

#[test]
fn text_normalize_matches_the_reference_checks() {
    let fixture = fixture();
    let section = &fixture["text_normalize"];
    let mut bad = Vec::new();
    let mut checked = 0;

    for case in records(section, "normalize_text") {
        checked += 1;
        let text = case["input"].as_str().unwrap_or_default();
        let got = normalize_text(text);
        let mut diffs = Vec::new();
        walk(&case["output"], &json!(got), "", &mut diffs);
        bad.extend(
            diffs
                .into_iter()
                .map(|d| format!("normalize_text {text:?}{d}")),
        );
    }
    for case in records(section, "script_evidence") {
        checked += 1;
        let got = serde_json::to_value(script_evidence(case["input"].as_str().unwrap_or_default()));
        let mut diffs = Vec::new();
        walk(&case["output"], &got.unwrap_or(Value::Null), "", &mut diffs);
        bad.extend(diffs.into_iter().map(|d| format!("script_evidence{d}")));
    }
    for case in records(section, "normalize_document") {
        checked += 1;
        let html = case["html"].as_str().unwrap_or_default();
        let config = config_of(&case["content_area"]);
        let got = normalize_document(html, config.as_ref());
        let name = case["name"].as_str().unwrap_or_default();
        match (case.get("raised"), got) {
            (Some(_), Err(_)) => {}
            (Some(raised), Ok(_)) => bad.push(format!(
                "normalize_document {name}: reference raised {raised}, Rust returned"
            )),
            (None, Err(err)) => bad.push(format!("normalize_document {name}: Rust error {err}")),
            (None, Ok(doc)) => {
                let mut diffs = Vec::new();
                walk(
                    &case["output"],
                    &serde_json::to_value(doc).unwrap_or(Value::Null),
                    "",
                    &mut diffs,
                );
                bad.extend(
                    diffs
                        .into_iter()
                        .map(|d| format!("normalize_document {name}{d}")),
                );
            }
        }
    }
    for case in records(section, "declared_language") {
        checked += 1;
        let html = case["html"].as_str().unwrap_or_default();
        let got = serde_json::to_value(declared_language(&parse_document(html)));
        let mut diffs = Vec::new();
        walk(&case["output"], &got.unwrap_or(Value::Null), "", &mut diffs);
        bad.extend(
            diffs
                .into_iter()
                .map(|d| format!("declared_language {}{d}", case["name"])),
        );
    }
    for case in records(section, "normalization_policy") {
        checked += 1;
        let config = config_of(&case["content_area"]);
        let mut diffs = Vec::new();
        walk(
            &case["output"],
            &normalization_policy(config.as_ref()),
            "",
            &mut diffs,
        );
        bad.extend(
            diffs
                .into_iter()
                .map(|d| format!("normalization_policy{d}")),
        );
    }
    for case in records(section, "manifest_entry") {
        checked += 1;
        let item = case["item"].as_object().cloned().unwrap_or_default();
        let mut diffs = Vec::new();
        walk(&case["output"], &manifest_entry(&item), "", &mut diffs);
        bad.extend(diffs.into_iter().map(|d| format!("manifest_entry{d}")));
    }
    for case in records(section, "prepare_items") {
        checked += 1;
        let items = case["items"].as_array().cloned().unwrap_or_default();
        let config = config_of(&case["content_area"]);
        match (case.get("raised"), prepare_items(&items, config.as_ref())) {
            (Some(_), Err(_)) => {}
            (Some(raised), Ok(_)) => bad.push(format!(
                "prepare_items: reference raised {raised}, Rust returned"
            )),
            (None, Err(err)) => bad.push(format!("prepare_items: Rust error {err}")),
            (None, Ok(rows)) => {
                let mut diffs = Vec::new();
                walk(&case["output"], &Value::Array(rows), "", &mut diffs);
                bad.extend(diffs.into_iter().map(|d| format!("prepare_items{d}")));
            }
        }
    }

    // Code-point predicates over the whole Unicode space; the reference str predicates are the oracle.
    let predicates = &section["predicates"];
    let sweeps: [(&str, Value, Value); 3] = [
        (
            "alpha (str.isalpha)",
            predicates["alpha"].clone(),
            json!(ranges(py_isalpha)),
        ),
        (
            "word (\\w)",
            predicates["word"].clone(),
            json!(ranges(py_is_word)),
        ),
        (
            "space (str.isspace)",
            predicates["space"].clone(),
            json!(ranges(is_py_space)),
        ),
    ];
    for (name, expected, actual) in sweeps {
        checked += 1;
        let mut diffs = Vec::new();
        walk(&expected, &actual, "", &mut diffs);
        bad.extend(diffs.into_iter().map(|d| format!("predicate {name}{d}")));
    }

    // Every code point through the whole pipeline: NFC, casefold and the removal rules. the reference
    // single-code-point results are recorded where they differ from the identity-minus-removals rule.
    let mut expected_points: HashMap<u32, String> = HashMap::new();
    for entry in records(section, "normalize_text_codepoints") {
        let (Some(cp), Some(text)) = (entry[0].as_u64(), entry[1].as_str()) else {
            continue;
        };
        expected_points.insert(cp as u32, text.to_owned());
    }
    for cp in 0..=0x10FFFF_u32 {
        let Some(c) = char::from_u32(cp) else {
            continue;
        };
        checked += 1;
        let removed = matches!(
            unicode_general_category::get_general_category(c),
            unicode_general_category::GeneralCategory::Control
                | unicode_general_category::GeneralCategory::Format
                | unicode_general_category::GeneralCategory::Surrogate
        );
        let default = if is_py_space(c) || removed {
            String::new()
        } else {
            c.to_string()
        };
        let expected = expected_points.get(&cp).cloned().unwrap_or(default);
        let got = normalize_text(&c.to_string());
        if got != expected {
            bad.push(format!(
                "normalize_text U+{cp:04X}: want {expected:?} got {got:?}"
            ));
        }
    }

    report("text_normalize", &bad, checked);
}

#[test]
fn normalize_text_folds_unicode_variants_to_one_form() {
    // Decomposed and composed forms, a zero-width space, a soft hyphen and a bidi mark.
    assert_eq!(normalize_text("Cafe\u{301}"), normalize_text("Caf\u{e9}"));
    assert_eq!(normalize_text("ab\u{200b}cd\u{ad}ef\u{202e}"), "abcdef");
    assert_eq!(normalize_text("  Hello\t\r\n World  "), "hello world");
    assert_eq!(normalize_text("Straße"), "strasse");
}

#[test]
fn script_evidence_names_latin_cyrillic_mixed_and_undetermined() {
    let latin = script_evidence(&"abcdefghijklmnopqrstuvwxyz".repeat(2));
    assert_eq!(latin.script, "latin");
    let cyrillic = script_evidence(&"привет мир тест".repeat(3));
    assert_eq!(cyrillic.script, "cyrillic");
    let mixed = script_evidence(&format!("{}{}", "ab".repeat(15), "жз".repeat(15)));
    assert_eq!(mixed.script, "mixed");
    let short = script_evidence("abc");
    assert_eq!(short.script, "undetermined");
    assert_eq!(short.script_letters, 3);
}

#[test]
fn prepare_items_marks_missing_bodies_unavailable() {
    let items = vec![
        json!("not an object"),
        json!({"url": "https://a.example/", "html": "   "}),
        json!({"url": "https://b.example/", "html": "<main><h1>Title</h1><p>Body text here</p></main>", "indexable": true}),
    ];
    let rows = prepare_items(&items, None).unwrap_or_default();
    assert_eq!(rows[0]["state"], "unavailable");
    assert_eq!(rows[1]["reason"], "item supplies no html body");
    assert_eq!(rows[2]["state"], Value::Null);
    assert_eq!(rows[2]["indexable"], true);
}

#[test]
fn navigation_matches_the_reference_checks() {
    let fixture = fixture();
    let section = &fixture["navigation"];
    let mut bad = Vec::new();
    let mut checked = 0;

    for case in records(section, "validate") {
        checked += 1;
        let stored = case["stored"].as_bool().unwrap_or(false);
        let table: HashMap<u64, String> = case["resolve"]
            .as_object()
            .map(|map| {
                map.iter()
                    .filter_map(|(k, v)| Some((k.parse().ok()?, v.as_str()?.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        let resolve = |id: u64| table.get(&id).cloned();
        let resolver: Option<&dyn Fn(u64) -> Option<String>> = if case["resolve"].is_null() {
            None
        } else {
            Some(&resolve)
        };
        let got = validate_navigation(&case["value"], stored, resolver);
        match (case.get("raised"), got) {
            (Some(raised), Err(err))
                if err.to_string() == raised_message(raised.as_str().unwrap_or_default()) => {}
            (Some(raised), other) => bad.push(format!(
                "validate: reference raised {raised}, Rust {other:?}"
            )),
            (None, Err(err)) => bad.push(format!("validate: Rust error {err}")),
            (None, Ok(())) => {}
        }
    }

    for case in records(section, "retain") {
        checked += 1;
        let mut order: Vec<String> = Vec::new();
        let mut ids: HashMap<String, u64> = HashMap::new();
        let mut intern = |url: &str| {
            *ids.entry(url.to_owned()).or_insert_with(|| {
                order.push(url.to_owned());
                order.len() as u64
            })
        };
        let got = retain_navigation(&case["raw"], &mut intern);
        drop(intern);
        match (case.get("raised"), got) {
            (Some(raised), Err(err))
                if err.to_string() == raised_message(raised.as_str().unwrap_or_default()) => {}
            (Some(raised), other) => {
                bad.push(format!("retain: reference raised {raised}, Rust {other:?}"))
            }
            (None, Err(err)) => bad.push(format!("retain: Rust error {err}")),
            (None, Ok(value)) => {
                let expected_order: Vec<String> = case["interned"]
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                if expected_order != order {
                    bad.push(format!(
                        "retain: interned order want {expected_order:?} got {order:?}"
                    ));
                }
                let mut diffs = Vec::new();
                walk(&case["output"], &value, "", &mut diffs);
                bad.extend(diffs.into_iter().map(|d| format!("retain{d}")));
            }
        }
    }

    for case in records(section, "expand") {
        checked += 1;
        let table: HashMap<u64, String> = case["resolve"]
            .as_object()
            .map(|map| {
                map.iter()
                    .filter_map(|(k, v)| Some((k.parse().ok()?, v.as_str()?.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        let resolve = |id: u64| table.get(&id).cloned();
        match (
            case.get("raised"),
            expand_navigation(&case["stored"], &resolve),
        ) {
            (Some(raised), Err(err))
                if err.to_string() == raised_message(raised.as_str().unwrap_or_default()) => {}
            (Some(raised), other) => {
                bad.push(format!("expand: reference raised {raised}, Rust {other:?}"))
            }
            (None, Err(err)) => bad.push(format!("expand: Rust error {err}")),
            (None, Ok(value)) => {
                let mut diffs = Vec::new();
                walk(&case["output"], &value, "", &mut diffs);
                bad.extend(diffs.into_iter().map(|d| format!("expand{d}")));
            }
        }
    }

    for case in records(section, "capture") {
        checked += 1;
        let target = case["target"].as_str().unwrap_or_default();
        let wait = case["wait"].as_str().unwrap_or_default();
        let timeout = case["timeout"].as_f64().unwrap_or(15.0);
        let cdp = case["cdp"].as_bool().unwrap_or(false);
        let mut capture = NavigationCapture::new(target, wait, timeout, cdp);
        for step in records(case, "steps") {
            let elapsed = step["elapsed_ms"].as_u64().unwrap_or(0);
            let url = step["url"].as_str().unwrap_or_default().to_owned();
            let signal = match step["op"].as_str().unwrap_or_default() {
                "requested" => CaptureSignal::Requested {
                    url,
                    reason: step["reason"].as_str().unwrap_or_default().to_owned(),
                },
                "document" => CaptureSignal::Document { url },
                "same" => CaptureSignal::SameDocument {
                    url,
                    navigation_type: step["navigation_type"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                },
                "frame" => CaptureSignal::PlaywrightFrame { url },
                _ => CaptureSignal::Response {
                    request_url: step["request_url"].as_str().unwrap_or_default().to_owned(),
                    status: step["status"].as_u64().unwrap_or(0) as u16,
                    location: step["location"].as_str().map(str::to_owned),
                },
            };
            capture.signal(signal, elapsed);
        }
        capture.finish(case["success"].as_bool().unwrap_or(false));
        let got = serde_json::to_value(&capture.data).unwrap_or(Value::Null);
        let mut diffs = Vec::new();
        walk(&case["output"], &got, "", &mut diffs);
        bad.extend(diffs.into_iter().map(|d| format!("capture {target}{d}")));
    }

    report("navigation", &bad, checked);
}

#[test]
fn link_context_matches_the_reference_checks() {
    let fixture = fixture();
    let mut bad = Vec::new();
    let cases = records(&fixture, "link_context");
    for case in cases {
        let name = case["name"].as_str().unwrap_or_default();
        let config = config_of(&case["content_area"]);
        let rules = if case["position_rules"].is_null() {
            None
        } else {
            Some(case["position_rules"].clone())
        };
        let cap = case["cap"].as_u64().unwrap_or(0) as usize;
        let got = extract_occurrences(
            case["html"].as_str().unwrap_or_default(),
            case["final_url"].as_str().unwrap_or_default(),
            config.as_ref(),
            rules.as_ref(),
            cap,
        );
        match (case.get("raised"), got) {
            (Some(_), Err(_)) => {}
            (Some(raised), Ok(_)) => bad.push(format!(
                "link_context {name}: reference raised {raised}, Rust returned"
            )),
            (None, Err(err)) => bad.push(format!("link_context {name}: Rust error {err}")),
            (None, Ok(context)) => {
                let got = serde_json::to_value(context).unwrap_or(Value::Null);
                let mut diffs = Vec::new();
                walk(&case["output"], &got, "", &mut diffs);
                bad.extend(diffs.into_iter().map(|d| format!("link_context {name}{d}")));
            }
        }
    }
    report("link_context", &bad, cases.len());
}

#[test]
fn asset_evidence_matches_the_reference_checks() {
    let fixture = fixture();
    let section = &fixture["asset_evidence"];
    let mut bad = Vec::new();
    let mut checked = 0;
    for case in records(section, "whitespace_ratio") {
        checked += 1;
        let got = json!(whitespace_ratio(case["input"].as_str().unwrap_or_default()));
        let mut diffs = Vec::new();
        walk(&case["output"], &got, "", &mut diffs);
        bad.extend(diffs.into_iter().map(|d| format!("whitespace_ratio{d}")));
    }
    for case in records(section, "looks_minified") {
        checked += 1;
        let got = json!(looks_minified(case["input"].as_str().unwrap_or_default()));
        let mut diffs = Vec::new();
        walk(&case["output"], &got, "", &mut diffs);
        bad.extend(diffs.into_iter().map(|d| format!("looks_minified{d}")));
    }
    for case in records(section, "find_missing_font_display") {
        checked += 1;
        let got: Vec<Value> = find_missing_font_display(case["input"].as_str().unwrap_or_default())
            .into_iter()
            .map(|item| json!({"excerpt": item.excerpt}))
            .collect();
        let mut diffs = Vec::new();
        walk(&case["output"], &Value::Array(got), "", &mut diffs);
        bad.extend(
            diffs
                .into_iter()
                .map(|d| format!("find_missing_font_display{d}")),
        );
    }
    for case in records(section, "looks_legacy_transpiled") {
        checked += 1;
        let got = json!(looks_legacy_transpiled(
            case["input"].as_str().unwrap_or_default()
        ));
        let mut diffs = Vec::new();
        walk(&case["output"], &got, "", &mut diffs);
        bad.extend(
            diffs
                .into_iter()
                .map(|d| format!("looks_legacy_transpiled{d}")),
        );
    }
    for case in records(section, "check_cache_lifetime") {
        checked += 1;
        let got = serde_json::to_value(check_cache_lifetime(case["input"].as_str()));
        let mut diffs = Vec::new();
        walk(&case["output"], &got.unwrap_or(Value::Null), "", &mut diffs);
        bad.extend(
            diffs
                .into_iter()
                .map(|d| format!("check_cache_lifetime{d}")),
        );
    }
    report("asset_evidence", &bad, checked);
}

#[test]
fn looks_minified_classifies_small_files_by_shape() {
    assert!(looks_minified(".a{color:red}"));
    assert!(!looks_minified("body { color: red; }"));
}

#[test]
fn font_display_swap_is_compliant() {
    let css = "@font-face { font-family: A; src: url(a.woff2); font-display: swap; }";
    assert!(find_missing_font_display(css).is_empty());
}

#[test]
fn font_display_missing_is_flagged_with_excerpt() {
    let css = "@font-face { font-family: A; src: url(a.woff2); }";
    let findings = find_missing_font_display(css);
    assert_eq!(findings.len(), 1);
    assert!(findings[0].excerpt.contains("font-family: A"));
}

#[test]
fn font_display_auto_is_still_flagged() {
    let css = "@font-face { font-family: A; font-display: auto; }";
    assert_eq!(find_missing_font_display(css).len(), 1);
}

#[test]
fn every_font_face_block_is_checked() {
    let css = "@font-face { font-family: A; font-display: swap; }@font-face { font-family: B; }";
    assert_eq!(find_missing_font_display(css).len(), 1);
}

#[test]
fn legacy_transpile_markers_are_detected() {
    assert!(looks_legacy_transpiled(
        "require('core-js/modules/es.promise')"
    ));
    assert!(looks_legacy_transpiled("var regeneratorRuntime = {}"));
    assert!(!looks_legacy_transpiled("const x = () => 1;"));
}

#[test]
fn cache_control_missing_is_not_ok() {
    let result = check_cache_lifetime(None);
    assert!(!result.ok);
    assert_eq!(result.max_age, None);
    assert_eq!(result.reason.as_deref(), Some("no Cache-Control max-age"));
}

#[test]
fn cache_control_lifetime_verdicts() {
    let short = check_cache_lifetime(Some("public, max-age=60"));
    assert!(!short.ok);
    assert_eq!(short.max_age, Some(60));
    assert!(check_cache_lifetime(Some("public, max-age=31536000, immutable")).ok);
    assert!(!check_cache_lifetime(Some("no-store")).ok);
}

#[test]
fn normalize_text_collapses_space_and_removes_invisibles() {
    assert_eq!(
        normalize_text("  Multiple   Spaces\tAnd\nNewlines "),
        "multiple spaces and newlines"
    );
    assert_eq!(normalize_text("ÄBC"), "äbc");
    assert_eq!(normalize_text("to\u{200b}ken"), "token");
    assert_eq!(normalize_text(""), "");
}

#[test]
fn script_evidence_shares_match_the_reference_profile() {
    let latin = script_evidence("the quick brown fox jumps over the lazy dog");
    assert_eq!(latin.script, "latin");
    assert_eq!(latin.script_shares["latin"], 1.0);
    assert_eq!(latin.script_shares["cyrillic"], 0.0);
    let cyrillic = script_evidence("быстрая рыжая лиса перепрыгивает через ленивого пса");
    assert_eq!(cyrillic.script, "cyrillic");
    assert_eq!(cyrillic.script_shares["cyrillic"], 1.0);
    let blended = script_evidence("the quick brown fox рыжая лиса и серая собака");
    assert_eq!(blended.script, "mixed");
    assert!(blended.script_shares["latin"] > 0.0);
    assert!(blended.script_shares["cyrillic"] > 0.0);
}

#[test]
fn cdp_signals_separate_script_anchor_history_fragment_and_unknown() {
    let target = "https://example.test/";
    let mut capture = NavigationCapture::new(target, "load", 15.0, true);
    capture.signal(
        CaptureSignal::Document {
            url: target.to_owned(),
        },
        0,
    );
    for (reason, path) in [
        ("scriptInitiated", "script"),
        ("anchorClick", "anchor"),
        ("", "unknown"),
    ] {
        let url = format!("https://example.test/{path}");
        capture.signal(
            CaptureSignal::Requested {
                url: url.clone(),
                reason: reason.to_owned(),
            },
            1,
        );
        capture.signal(CaptureSignal::Document { url }, 2);
    }
    capture.signal(
        CaptureSignal::SameDocument {
            url: "https://example.test/unknown?q=1".to_owned(),
            navigation_type: "historyApi".to_owned(),
        },
        3,
    );
    capture.signal(
        CaptureSignal::SameDocument {
            url: "https://example.test/unknown?q=1#part".to_owned(),
            navigation_type: "fragment".to_owned(),
        },
        4,
    );
    capture.finish(true);
    let kinds: Vec<&str> = capture
        .data
        .events
        .iter()
        .map(|e| e.kind.as_str())
        .collect();
    assert_eq!(
        kinds,
        [
            "initial_http_navigation",
            "script_navigation",
            "anchor_navigation",
            "document_navigation",
            "spa_history_change",
            "fragment_navigation",
        ]
    );
    assert!(capture.data.events.iter().all(|e| e.user_click.is_none()));
    let value = serde_json::to_value(&capture.data).unwrap_or(Value::Null);
    assert!(validate_navigation(&value, false, None).is_ok());
}

#[test]
fn failed_capture_keeps_bounded_events_and_names_the_budget() {
    let mut capture = NavigationCapture::new("https://example.test/", "load", 15.0, true);
    for n in 0..(spiderhead_parse::navigation::EVENT_CAP + 5) {
        let url = format!("https://example.test/{}", n % 2);
        capture.add(
            &url,
            "script_navigation",
            "cdp_requested",
            None,
            None,
            n as u64,
        );
    }
    capture.finish(false);
    assert_eq!(
        capture.data.events.len(),
        spiderhead_parse::navigation::EVENT_CAP
    );
    assert_eq!(capture.data.events_omitted, 5);
    assert_eq!(capture.data.state, "partial");
    assert_eq!(capture.data.reason, "event_budget_exhausted");
}

#[test]
fn legacy_evidence_is_unavailable_and_a_forged_click_is_rejected() {
    let legacy = retain_navigation(
        &json!({"events": [{"kind": "script_navigation"}]}),
        &mut |_| 1,
    );
    let legacy = legacy.unwrap_or(Value::Null);
    assert_eq!(legacy["state"], "unavailable");
    assert_eq!(legacy["events"], json!([]));
    let mut capture = NavigationCapture::new("https://example.test/", "load", 15.0, true);
    capture.add(
        "https://example.test/new",
        "document_navigation",
        "cdp_frame",
        None,
        None,
        0,
    );
    capture.finish(false);
    capture.data.events[0].user_click = Some(false);
    let value = serde_json::to_value(&capture.data).unwrap_or(Value::Null);
    let err = validate_navigation(&value, false, None)
        .err()
        .map(|e| e.to_string());
    assert_eq!(
        err.as_deref(),
        Some("navigation user-click cause was not observed")
    );
}

#[test]
fn unmapped_requested_reasons_stay_valid_observations() {
    let mut capture = NavigationCapture::new("https://example.test/", "load", 15.0, true);
    capture.signal(
        CaptureSignal::Document {
            url: "https://example.test/".to_owned(),
        },
        0,
    );
    for (reason, path) in [("pageBlockInterstitial", "blocked"), ("reload", "reloaded")] {
        let url = format!("https://example.test/{path}");
        capture.signal(
            CaptureSignal::Requested {
                url: url.clone(),
                reason: reason.to_owned(),
            },
            1,
        );
        capture.signal(CaptureSignal::Document { url }, 2);
    }
    capture.finish(true);
    let kinds: Vec<&str> = capture
        .data
        .events
        .iter()
        .map(|e| e.kind.as_str())
        .collect();
    assert_eq!(
        kinds,
        [
            "initial_http_navigation",
            "document_navigation",
            "refresh_navigation"
        ]
    );
    let value = serde_json::to_value(&capture.data).unwrap_or(Value::Null);
    assert!(validate_navigation(&value, false, None).is_ok());
}

#[test]
fn heading_precedence_and_cap_are_explained() {
    let html = "<body><main><h2>First</h2><a href='/a'>A</a>\
        <section><a href='/b'>B</a><h3>Section</h3><a href='/c'>C</a></section>\
        <footer><a href='/d'>D</a></footer></main></body>";
    let capped = extract_occurrences(html, "https://example.test/", None, None, 2);
    let capped = capped.unwrap();
    assert_eq!(capped.eligible_total, 4);
    assert_eq!(capped.eligible_omitted, 2);
    let texts: Vec<Option<&str>> = capped
        .occurrences
        .iter()
        .map(|o| o.heading.as_ref().map(|h| h.text.as_str()))
        .collect();
    assert_eq!(texts, [Some("First"), Some("First")]);
    let full = extract_occurrences(html, "https://example.test/", None, None, 20_000);
    let full = full.unwrap();
    let relations: Vec<&str> = full
        .occurrences
        .iter()
        .map(|o| o.heading_relation.as_str())
        .collect();
    assert_eq!(
        relations,
        [
            "preceding_content",
            "preceding_content",
            "same_section",
            "not_content_link"
        ]
    );
    assert_eq!(
        full.occurrences[2]
            .heading
            .as_ref()
            .map(|h| h.text.as_str()),
        Some("Section")
    );
    assert!(full.occurrences[3].heading.is_none());
}

#[test]
fn custom_selector_and_default_body_are_explained() {
    let rules = json!([{"position": "nav", "selector": ".promo"}]);
    let custom = extract_occurrences(
        "<body><main><h2>Heading</h2><div class='promo'><a href='/x'>X</a></div></main></body>",
        "https://example.test/",
        None,
        Some(&rules),
        20_000,
    );
    let custom = custom.unwrap();
    let first = &custom.occurrences[0];
    assert_eq!(first.position, "nav");
    assert_eq!(first.matched_selector.as_deref(), Some(".promo"));
    assert!(first.heading.is_none());
    let fallback = extract_occurrences(
        "<body><h2>Body heading</h2><a href='/x'>X</a></body>",
        "https://example.test/",
        None,
        None,
        20_000,
    );
    let fallback = fallback.unwrap();
    let link = &fallback.occurrences[0];
    assert_eq!(link.content_root_strategy, "default_body");
    assert_eq!(link.position, "content");
    assert_eq!(
        link.heading.as_ref().map(|h| h.text.as_str()),
        Some("Body heading")
    );
}

#[test]
fn many_siblings_keep_bounded_deterministic_dom_paths() {
    let links: String = (0..1000)
        .map(|i| format!("<a href='/p{i}'>P{i}</a>"))
        .collect();
    let html = format!("<body><main>{links}</main></body>");
    let result = extract_occurrences(&html, "https://example.test/", None, None, 20_000);
    let result = result.unwrap();
    assert_eq!(result.eligible_total, 1000);
    let last = &result.occurrences[999];
    assert!(last.dom_path.ends_with("a:nth-of-type(1000)"));
    assert!(last.dom_path.chars().count() <= 512);
}
