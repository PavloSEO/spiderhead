//! Body diff tests and a table of unified diff outputs.

use super::*;
use crate::bodies::doc_tests::{body, con, document, insert};
use crate::bodies::encode_body;

const URL: &str = "https://example.test/page";

/// Retained page response and static document for `URL`.
fn append(c: &Connection, raw: &[u8], variant: &str, content_type: &str, state: &str) {
    let url_id: i64 = c
        .query_row("SELECT url_id FROM urls WHERE url=?", [URL], |r| r.get(0))
        .unwrap_or_else(|_| {
            c.execute("INSERT INTO urls(url) VALUES(?)", [URL]).unwrap();
            c.last_insert_rowid()
        });
    let ordinal: i64 = c
        .query_row(
            "SELECT COALESCE(MAX(request_ordinal),0)+1 FROM responses",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let complete = state == "complete";
    let sha = complete.then(|| body(c, raw));
    let mut response = json!({
        "request_url_id": url_id, "request_ordinal": ordinal, "effective_url_id": url_id,
        "redirect_chain_json": "[]", "method": "GET", "purpose": "page", "requested_at": "2026-01-01T00:00:00Z",
        "request_headers_redacted_json": "[]", "credentials_used": 0, "variant_key": variant,
        "response_headers_redacted_json": "[]", "effective_headers_redacted_json": "[]",
        "content_type": content_type, "charset": "", "content_encoding": "", "transport_source": "network",
        "cache_status": "", "body_sha256": sha,
        "body_fidelity": if complete { "entity_bytes" } else { "unavailable" },
        "body_state": state, "body_reason": if complete { "none" } else { state }, "error": "", "error_kind": ""
    });
    if !complete && state != "truncated" {
        response["body_reason"] = json!("not_fetched");
    }
    insert(c, "responses", response.as_object().unwrap());
    let response_id = c.last_insert_rowid();
    let doc_id: i64 = c
        .query_row(
            "SELECT COALESCE(MAX(document_id),0)+1 FROM documents",
            [],
            |r| r.get(0),
        )
        .unwrap();
    document(
        c,
        sha.as_deref(),
        json!({"document_id": doc_id, "url_id": url_id, "source_response_id": response_id,
               "decoder_source": if complete { "content_type_charset" } else { "not_applicable" },
               "decoder_charset": if complete { "utf-8" } else { "unknown" },
               "decoder_errors": if complete { "replace" } else { "not_applicable" },
               "fidelity": if complete { "entity_bytes" } else { "unavailable" },
               "body_state": state, "body_reason": if complete { "none" } else if state == "truncated" { "truncated" } else { "not_fetched" }}),
    );
}

fn scan(raw: &[u8], variant: &str, content_type: &str, state: &str) -> Connection {
    let c = con();
    append(&c, raw, variant, content_type, state);
    c
}

fn page(raw: &[u8]) -> Connection {
    scan(raw, "v1", "text/html", "complete")
}

fn diff(l: &Connection, r: &Connection, text: bool) -> Row {
    body_diff(
        l,
        r,
        URL,
        None,
        "static",
        text,
        5 * 1024 * 1024,
        10_000,
        None,
        None,
    )
    .unwrap()
}

#[test]
fn hash_first_unchanged_changed_and_opt_in_text_diff() {
    let (left, same, right) = (
        page(b"<main>left</main>"),
        page(b"<main>left</main>"),
        page(b"<main>right</main>"),
    );
    assert_eq!(diff(&left, &same, false)["status"], "unchanged");
    let changed = diff(&left, &right, true);
    assert_eq!(changed["status"], "changed");
    let lines: Vec<&str> = changed["text_diff"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(lines.contains(&"-<main>left</main>") && lines.contains(&"+<main>right</main>"));
    assert!(!changed["reason"].as_str().unwrap().contains("SEO"));
}

#[test]
fn variant_ambiguity_is_not_comparable_and_exact_variant_selects_one() {
    let extra = scan(b"a", "first", "text/html", "complete");
    append(&extra, b"b", "second", "text/html", "complete");
    let right = scan(b"a", "first", "text/html", "complete");
    let ambiguous = diff(&extra, &right, false);
    assert_eq!(ambiguous["status"], "not_comparable");
    assert!(ambiguous["reason"]
        .as_str()
        .unwrap()
        .contains("variant_key is required"));
    let exact = body_diff(
        &extra,
        &right,
        URL,
        Some("first"),
        "static",
        false,
        5 << 20,
        10_000,
        None,
        None,
    )
    .unwrap();
    assert_eq!(exact["status"], "unchanged");
}

#[test]
fn different_single_variants_and_right_only_ambiguity() {
    let left = scan(b"a", "left", "text/html", "complete");
    let right = scan(b"a", "right", "text/html", "complete");
    let ambiguous_right = scan(b"a", "one", "text/html", "complete");
    append(&ambiguous_right, b"b", "two", "text/html", "complete");
    assert_eq!(diff(&left, &right, false)["status"], "not_comparable");
    let r = diff(&left, &ambiguous_right, false);
    assert_eq!(r["status"], "not_comparable");
    assert!(r["reason"]
        .as_str()
        .unwrap()
        .contains("variant_key is required"));
}

#[test]
fn missing_representation_does_not_fall_back_to_static() {
    let (left, right) = (page(b"<html>static</html>"), page(b"<html>static</html>"));
    let r = body_diff(
        &left,
        &right,
        URL,
        None,
        "rendered",
        false,
        5 << 20,
        10_000,
        None,
        None,
    )
    .unwrap();
    assert_eq!(r["status"], "missing_evidence");
    assert!(r["left"].is_null() && r["right"].is_null());
}

#[test]
fn truncated_and_binary_text_requests_are_not_clean_matches() {
    let complete = page(b"<html>body</html>");
    let truncated = scan(b"", "v1", "text/html", "truncated");
    let missing = diff(&complete, &truncated, false);
    assert_eq!(missing["status"], "missing_evidence");
    assert!(missing["reason"].as_str().unwrap().contains("truncated"));
    let (bl, br) = (
        scan(b"\x00\x01", "v1", "application/octet-stream", "complete"),
        scan(b"\x00\x02", "v1", "application/octet-stream", "complete"),
    );
    let binary = diff(&bl, &br, true);
    assert_eq!(binary["status"], "not_comparable");
    assert_eq!(binary["reason"], "text diff requires textual body fidelity");
}

#[test]
fn text_limits_are_checked_before_body_materialization() {
    let (left, right) = (page(&b"line\n".repeat(4)), page(&b"other\n".repeat(4)));
    let run = |l: &Connection, r: &Connection, bytes: i64, lines: i64| {
        body_diff(l, r, URL, None, "static", true, bytes, lines, None, None).unwrap()["status"]
            .clone()
    };
    assert_eq!(run(&left, &right, 4, 10_000), "not_comparable");
    assert_eq!(run(&left, &right, 5 << 20, 2), "not_comparable");
    assert_eq!(run(&page(b"a"), &page(b"b"), 1, 10_000), "not_comparable");
}

#[test]
fn line_cap_recognizes_cr_and_unicode_separators() {
    let left = page("one\u{2028}two\rthree".as_bytes());
    let right = page("one\u{2028}two\rfour".as_bytes());
    let r = body_diff(
        &left,
        &right,
        URL,
        None,
        "static",
        true,
        5 << 20,
        2,
        None,
        None,
    )
    .unwrap();
    assert_eq!(r["status"], "not_comparable");
    let ok = body_diff(
        &left,
        &right,
        URL,
        None,
        "static",
        true,
        5 << 20,
        20,
        None,
        None,
    )
    .unwrap();
    assert_eq!(ok["text_diff"].as_array().unwrap().last().unwrap(), "+four");
}

#[test]
fn static_and_rendered_in_one_scan_are_selected_by_representation() {
    let c = page(b"<html>static</html>");
    let (l, _) = observation(&c, URL, "static", None).unwrap();
    assert_eq!(l.unwrap()["purpose"], "page");
    let (none, why) = observation(&c, URL, "rendered", None).unwrap();
    assert!(none.is_none() && why.unwrap().contains("missing"));
}

#[test]
fn static_resource_response_fallback_is_retained_and_variant_safe() {
    let c = con();
    let sha = body(&c, b"body{color:red}");
    c.execute(
        "INSERT INTO urls(url_id,url) VALUES(2,'https://example.test/a.css')",
        [],
    )
    .unwrap();
    for (i, variant) in [(1, "x"), (2, "y")] {
        let mut r = json!({"response_id": i, "request_url_id": 2, "request_ordinal": i, "effective_url_id": 2,
            "redirect_chain_json": "[]", "method": "GET", "purpose": "stylesheet", "requested_at": "t",
            "request_headers_redacted_json": "[]", "credentials_used": 0, "variant_key": variant,
            "response_headers_redacted_json": "[]", "effective_headers_redacted_json": "[]",
            "content_type": "text/css", "charset": "", "content_encoding": "", "transport_source": "network",
            "cache_status": "", "body_sha256": sha, "body_fidelity": "entity_bytes", "body_state": "complete",
            "body_reason": "none", "error": "", "error_kind": ""});
        r["variant_key"] = json!(variant);
        insert(&c, "responses", r.as_object().unwrap());
    }
    let (row, why) = observation(&c, "https://example.test/a.css", "static", None).unwrap();
    assert!(row.is_none() && why.unwrap().contains("variant_key is required"));
    let (row, _) = observation(&c, "https://example.test/a.css", "static", Some("y")).unwrap();
    let row = row.unwrap();
    assert_eq!(
        (&row["purpose"], &row["variant_key"], &row["document_id"]),
        (&json!("stylesheet"), &json!("y"), &Value::Null)
    );
    assert_eq!(summary(Some(&row)).unwrap()["sha256"], sha.as_str());
    let (row, why) = observation(&c, "https://example.test/a.css", "static", Some("z")).unwrap();
    assert!(row.is_none() && why.unwrap().contains("variant is missing"));
}

#[test]
fn renderer_provenance_must_be_complete_and_drops_observed_events() {
    let ok = |renderer: &str| -> Row {
        let mut r = Map::new();
        r.insert("renderer_json".into(), json!(renderer));
        r
    };
    let good = r#"{"engine":"e","engine_version":"1","settings":{"navigation":{},"transforms":{},"navigation_evidence":[1]},"navigation_transform":"direct","navigation_url_id":1,"final_url_id":1}"#;
    let p = renderer_provenance(&ok(good)).unwrap().unwrap();
    assert_eq!(p["settings"], json!({"navigation": {}, "transforms": {}}));
    for bad in [
        "not json",
        "[]",
        r#"{"engine":"","engine_version":"1"}"#,
        &good.replace("direct", "unknown"),
        &good.replace("\"final_url_id\":1", "\"final_url_id\":true"),
    ] {
        assert!(renderer_provenance(&ok(bad)).unwrap().is_none(), "{bad}");
    }
    assert!(renderer_provenance(&Map::new()).unwrap().is_none());
}

#[test]
fn validation_refuses_bad_arguments() {
    let c = page(b"a");
    for (url, rep, bytes, lines) in [
        ("", "static", 1, 1),
        (URL, "bogus", 1, 1),
        (URL, "static", 0, 1),
        (URL, "static", 1, MAX_LINES + 1),
    ] {
        assert!(body_diff(&c, &c, url, None, rep, false, bytes, lines, None, None).is_err());
    }
    assert!(limits(MAX_BYTES, MAX_LINES).is_ok());
}

#[test]
fn textual_and_text_decoding_follow_fidelity() {
    let row = |fid: &str, ct: &str| {
        json!({"fidelity": fid, "content_type": ct})
            .as_object()
            .unwrap()
            .clone()
    };
    assert!(textual(&row("serialized_dom", "")));
    assert!(textual(&row("entity_bytes", "Text/HTML; charset=x")));
    assert!(textual(&row("entity_bytes", "application/json")));
    assert!(!textual(&row("entity_bytes", "image/png")));
    assert_eq!(
        text("caf\u{e9}".as_bytes(), &row("reencoded_text", "")).unwrap(),
        "café"
    );
    assert!(text(b"\xff", &row("serialized_dom", "")).is_err());
    assert!(text(b"x", &row("unavailable", "")).is_err());
    let _ = encode_body(b"");
}

#[test]
fn splitlines_matches_reference() {
    assert_eq!(
        splitlines("a\r\nb\rc\nd\u{2028}e\u{85}"),
        vec!["a", "b", "c", "d", "e"]
    );
    assert_eq!(splitlines(""), Vec::<&str>::new());
    assert_eq!(splitlines("\n\n"), vec!["", ""]);
    assert_eq!(splitlines("x\u{b}y\u{c}z\u{1c}"), vec!["x", "y", "z"]);
}

// Table test: unified diff output over 119 line-list pairs.
#[test]
fn unified_diff_matches_reference_difflib() {
    let path = format!(
        "{}/tests/fixtures/unified_diff.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let rows: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert!(rows.len() >= 100);
    let lines = |v: &Value| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_owned())
            .collect()
    };
    for (n, row) in rows.iter().enumerate() {
        let (a, b) = (lines(&row["a"]), lines(&row["b"]));
        let (ar, br): (Vec<&str>, Vec<&str>) = (
            a.iter().map(String::as_str).collect(),
            b.iter().map(String::as_str).collect(),
        );
        assert_eq!(
            difflib::unified_diff(&ar, &br, "left", "right"),
            lines(&row["diff"]),
            "row {n}"
        );
    }
}
