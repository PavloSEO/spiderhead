#![recursion_limit = "512"]

//!
//! Scans are built with `NativeScanSchemaMixin::create`, pages go in through
//! `NativeScan::commit_page`, and `fragment_links::evaluate` reads the resulting artifact.
//! Not ported here (see the report of this slice): the static/rendered lane (needs
//! `RenderHost` for `NativeScan`), the constant-patching tests, the handler/CLI/MCP test,
//! the native-audit tests (checks/handlers) and the SF export audit (`sf.core.audit`).

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use spiderhead_core::settings_data;
use spiderhead_store::crawl_capture::CaptureEvent;
use spiderhead_store::fragment_links::{
    evaluate, findings, paginate, DEFAULT_MAX_DECODED_BYTES, MAX_OCCURRENCES,
};
use spiderhead_store::native_scan::NativeScan;
use spiderhead_store::scan_lease::{NativeScanLease, PageCommit};
use spiderhead_store::scan_schema::NativeScanSchemaMixin;

const BASE: &str = "https://example.test";

/// Temporary directory removed when the fixture goes out of scope.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A fresh directory and the path of the `scan.sqlite` inside it.
fn fixture(name: &str) -> (Scratch, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "spiderhead-tests-08-fragment-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join("scan.sqlite");
    (Scratch(dir), path)
}

/// reference `_config()`: resolved defaults with the delay and resource fetching switched off.
fn config() -> Map<String, Value> {
    let mut config = settings_data::defaults().clone();
    config["speed"]["min_delay_seconds"] = json!(0);
    config["resources"]["fetch"] = json!(false);
    config.as_object().cloned().expect("object")
}

/// Create a `scan.v1` artifact at `path` and fill it with `build`; the writer closes on return.
///
/// reference passes `config_fingerprint=fingerprint(config)`; the Rust fingerprint needs a host,
/// so this port passes `None`, which the evaluation does not read.
fn build(path: &Path, build: impl FnOnce(&mut NativeScan)) {
    let versions: Map<String, Value> = ["python", "sqlite", "httpx", "lxml", "beautifulsoup4"]
        .iter()
        .map(|key| ((*key).to_owned(), json!("test")))
        .collect();
    let mut scan = NativeScanSchemaMixin::create(
        path,
        &format!("{BASE}/"),
        &config(),
        None,
        "3.0.0",
        &"a".repeat(40),
        &versions,
        &[],
        None,
        &[],
        "scan.v1",
    )
    .expect("create scan");
    build(&mut scan);
}

fn runtime() -> Map<String, Value> {
    json!({
        "max_depth_reached": 0,
        "elapsed_seconds": 0.0,
        "circuit_timeout_streak": 0,
        "circuit_server_error_streak": 0,
        "crawl_delay_applied": null,
        "throttle": {"delay_seconds": 0.0, "concurrency": 1, "consecutive_ok": 0}
    })
    .as_object()
    .cloned()
    .expect("object")
}

/// reference `_event()`: a complete 200 HTML response; `edit` applies the per-test changes.
fn event(url: &str, body: &[u8], edit: impl FnOnce(&mut CaptureEvent)) -> CaptureEvent {
    let mut event = CaptureEvent {
        method: "GET".into(),
        requested_url: url.into(),
        effective_url: url.into(),
        redirect_history: Vec::new(),
        requested_at: "2026-09-06T10:00:00Z".into(),
        received_at: "2026-09-06T10:00:01Z".into(),
        status_code: Some(200),
        request_headers: vec![("accept".into(), "text/html".into())],
        credentials_used: false,
        response_headers: vec![("content-type".into(), "text/html; charset=utf-8".into())],
        content_type: "text/html; charset=utf-8".into(),
        content_encoding: String::new(),
        entity_bytes: Some(body.to_vec()),
        body_fidelity: "entity_bytes".into(),
        body_state: "complete".into(),
        body_reason: "none".into(),
        error: String::new(),
        error_kind: String::new(),
        effective_status_code: Some(200),
        effective_headers: vec![("content-type".into(), "text/html; charset=utf-8".into())],
        response_time: Some(0.25),
        session_changed: false,
        http_version: None,
        timing_state: "unavailable".into(),
        transport_phases: None,
    };
    edit(&mut event);
    event
}

/// reference `_record()`: every `PageRecord` field at its dataclass default, with `url` set and
/// `changes` applied. The native writer requires the full column set.
fn record(url: &str, changes: &[(&str, Value)]) -> Map<String, Value> {
    let mut fields = json!({
        "url": url,
        "status_code": 200,
        "content_type": "text/html",
        "size_bytes": 128,
        "response_time": 0.25,
        "redirect_url": "",
        "title": "",
        "meta_description": "",
        "h1": "",
        "h1_2": "",
        "h2": "",
        "canonical": "",
        "meta_robots": "",
        "x_robots": "",
        "og_title": "",
        "og_description": "",
        "og_image": "",
        "og_url": "",
        "word_count": 0,
        "text_ratio": null,
        "content_frames": 0,
        "content_frames_same_origin": 0,
        "meta_description_count": 0,
        "h1_alt_text": "",
        "lorem_ipsum_count": 0,
        "images_total": 0,
        "images_missing_alt_attr": 0,
        "images_max_alt_length": 0,
        "plugin_elements": 0,
        "mobile_alternate_broken": 0,
        "meta_fragment": "",
        "ajax_scheme_outlinks": 0,
        "crawl_depth": 0,
        "content_encoding": "",
        "meta_refresh": "",
        "http_refresh": "",
        "charset": "",
        "doctype": "",
        "viewport": "",
        "title_outside_head": null,
        "meta_description_outside_head": null,
        "canonical_outside_head": null,
        "directives_outside_head": null,
        "hreflang_outside_head": null,
        "hreflang": [],
        "amphtml": "",
        "heading_outline": [],
        "invalid_link_targets": null,
        "transport_upgraded": false,
        "sentence_count": null,
        "avg_words_per_sentence": null,
        "flesch": null,
        "readability": "",
        "link_placement": {"in_heading": [], "in_heading_total": 0, "image_no_text": [], "image_no_text_total": 0},
        "trust_signals": null,
        "duplicate_ids": null,
        "head_count": 0,
        "body_count": 0,
        "head_not_first": false,
        "invalid_head_elements": "",
        "outlinks": 0,
        "external_outlinks": 0,
        "jsonld_blocks_found": 0,
        "jsonld_blocks_parsed": 0,
        "error": "",
        "error_kind": "",
        "cache_status": "",
        "body_unavailable": "",
        "representation": "static",
        "redirect_chain": [],
        "final_url": "",
        "canonical_chain": [],
        "final_canonical": "",
    })
    .as_object()
    .cloned()
    .expect("object");
    for (key, value) in changes {
        fields.insert((*key).to_owned(), value.clone());
    }
    fields
}

/// reference `_commit()`: enqueue, claim and commit one page. `body = None` leaves no document.
fn commit(
    scan: &mut NativeScan,
    url: &str,
    body: Option<&[u8]>,
    record: Option<Map<String, Value>>,
    edit: impl FnOnce(&mut CaptureEvent),
) {
    scan.enqueue(&[(url.to_owned(), 0)], "enqueue")
        .expect("enqueue");
    let lease = scan.claim(1).expect("claim").remove(0);
    let captures = match body {
        Some(bytes) => vec![event(url, bytes, edit)],
        None => Vec::new(),
    };
    let resource_inventory_state = if captures.is_empty() {
        None
    } else {
        Some("complete".to_owned())
    };
    let commit = PageCommit {
        lease: Some(lease),
        record: record.unwrap_or_else(|| self::record(url, &[])),
        captures,
        resource_inventory_state,
        runtime: Some(runtime()),
        ..PageCommit::default()
    };
    scan.commit_page(&commit).expect("commit_page");
}

/// reference `_commit(scan, url, html)` for the common complete-HTML case.
fn commit_html(scan: &mut NativeScan, url: &str, html: &str) {
    commit(scan, url, Some(html.as_bytes()), None, |_| {});
}

fn evaluate_at(path: &Path, max_occurrences: i64) -> Map<String, Value> {
    let con = Connection::open(path).expect("open artifact");
    evaluate(&con, DEFAULT_MAX_DECODED_BYTES, max_occurrences).expect("evaluate")
}

fn evaluate_default(path: &Path) -> Map<String, Value> {
    evaluate_at(path, MAX_OCCURRENCES)
}

fn occurrences(result: &Map<String, Value>) -> Vec<Map<String, Value>> {
    result["occurrences"]
        .as_array()
        .expect("occurrences array")
        .iter()
        .map(|item| item.as_object().expect("occurrence object").clone())
        .collect()
}

/// reference `_one()`: the single occurrence with this raw href.
fn one(result: &Map<String, Value>, raw_href: &str) -> Map<String, Value> {
    let matched: Vec<Map<String, Value>> = occurrences(result)
        .into_iter()
        .filter(|item| item["raw_href"] == json!(raw_href))
        .collect();
    assert_eq!(
        matched.len(),
        1,
        "expected exactly one {raw_href:?} occurrence"
    );
    matched.into_iter().next().expect("one occurrence")
}

#[test]
fn same_page_targets_resolve_or_report_missing() {
    let (_dir, path) = fixture("same-page");
    let html = concat!(
        r##"<html><body><a href="#section">ok</a><a href="#absent">missing</a>"##,
        r#"<div id="section">x</div></body></html>"#
    );
    build(&path, |scan| commit_html(scan, &format!("{BASE}/"), html));

    let result = evaluate_default(&path);
    let states: Map<String, Value> = occurrences(&result)
        .iter()
        .map(|item| {
            (
                item["raw_href"].as_str().unwrap().to_owned(),
                item["state"].clone(),
            )
        })
        .collect();
    assert_eq!(
        Value::Object(states),
        json!({"#section": "resolved", "#absent": "missing"})
    );
    assert_eq!(
        one(&result, "#section")["match"],
        json!({"kind": "element_id", "value": "section"})
    );
    assert_eq!(
        one(&result, "#absent")["reason"],
        json!("no_matching_fragment_target")
    );
    assert_eq!(
        result["states"],
        json!({"resolved": 1, "missing": 1, "skipped": 0})
    );
    assert_eq!(result["coverage"]["state"], json!("complete"));
}

#[test]
fn cross_page_resolution_and_query_string_distinction() {
    let (_dir, path) = fixture("cross-page");
    build(&path, |scan| {
        commit_html(
            scan,
            &format!("{BASE}/"),
            r#"<html><body><a href="/guide#section">a</a><a href="/guide?x=1#section">b</a></body></html>"#,
        );
        commit_html(
            scan,
            &format!("{BASE}/guide"),
            r#"<html><body><div id="section"></div></body></html>"#,
        );
        commit_html(
            scan,
            &format!("{BASE}/guide?x=1"),
            "<html><body><p>different</p></body></html>",
        );
    });

    let result = evaluate_default(&path);
    assert_eq!(one(&result, "/guide#section")["state"], json!("resolved"));
    let query_hit = one(&result, "/guide?x=1#section");
    assert_eq!(query_hit["state"], json!("missing"));
    assert_eq!(
        query_hit["destination_url"],
        json!(format!("{BASE}/guide?x=1"))
    );
}

#[test]
fn absent_and_non_http_destinations_are_named_skips() {
    let (_dir, path) = fixture("absent-destinations");
    let html = concat!(
        r#"<html><body><a href="/absent#x">internal</a>"#,
        r#"<a href="https://other.test/p#x">external</a>"#,
        r#"<a href="mailto:ops@example.test#x">mailto</a></body></html>"#
    );
    build(&path, |scan| commit_html(scan, &format!("{BASE}/"), html));

    let result = evaluate_default(&path);
    assert_eq!(
        one(&result, "/absent#x")["reason"],
        json!("destination_not_in_scan")
    );
    assert_eq!(
        one(&result, "https://other.test/p#x")["reason"],
        json!("destination_not_in_scan")
    );
    assert_eq!(
        one(&result, "mailto:ops@example.test#x")["reason"],
        json!("destination_scheme_unsupported")
    );
    assert_eq!(result["states"]["missing"], json!(0));
    // An unreadable destination is unanswered evidence: coverage can never
    // read complete while named skips exist.
    assert_eq!(result["coverage"]["state"], json!("partial"));
    assert_eq!(result["coverage"]["occurrences_skipped"], json!(3));
    assert_eq!(
        result["coverage"]["skip_reasons"],
        json!({"destination_not_in_scan": 2, "destination_scheme_unsupported": 1})
    );
}

#[test]
fn case_sensitive_ids_and_duplicate_sources() {
    let (_dir, path) = fixture("case-sensitive");
    let html = concat!(
        r##"<html><body><a href="#section">lower</a><a href="#Section">upper</a>"##,
        r##"<a href="#absent">a</a><a href="#absent">b</a>"##,
        r#"<div id="Section"></div><span id="Section"></span></body></html>"#
    );
    build(&path, |scan| commit_html(scan, &format!("{BASE}/"), html));

    let result = evaluate_default(&path);
    // `Section` does not satisfy `section`.
    assert_eq!(one(&result, "#section")["state"], json!("missing"));
    // Duplicate ids resolve once.
    assert_eq!(one(&result, "#Section")["state"], json!("resolved"));
    let missing: Vec<Value> = occurrences(&result)
        .into_iter()
        .filter(|item| item["state"] == json!("missing"))
        .map(|item| item["ordinal"].clone())
        .collect();
    assert_eq!(missing, vec![json!(0), json!(2), json!(3)]);
    let grouped = findings(&result);
    assert_eq!(grouped.len(), 2);
    let absent = grouped
        .iter()
        .find(|group| group["fragment"] == json!("absent"))
        .expect("absent group");
    assert_eq!(absent["occurrences_count"], json!(2));
    let ordinals: Vec<Value> = absent["locations"]
        .as_array()
        .expect("locations")
        .iter()
        .map(|loc| loc["ordinal"].clone())
        .collect();
    assert_eq!(ordinals, vec![json!(2), json!(3)]);
}

#[test]
fn serialized_fragment_match_wins_before_decoding() {
    let (_dir, path) = fixture("serialized-first");
    let dest = r#"<html><body><div id="%73ection"></div><div id="section"></div></body></html>"#;
    build(&path, |scan| {
        commit_html(
            scan,
            &format!("{BASE}/"),
            r##"<html><body><a href="/d#%73ection">x</a></body></html>"##,
        );
        commit_html(scan, &format!("{BASE}/d"), dest);
    });

    let hit = one(&evaluate_default(&path), "/d#%73ection");
    // Serialized-first: the literal id `%73ection` wins over decoded `section`.
    assert_eq!(hit["state"], json!("resolved"));
    assert_eq!(
        hit["match"],
        json!({"kind": "element_id", "value": "%73ection"})
    );
}

#[test]
fn decoded_match_when_no_serialized_id() {
    let (_dir, path) = fixture("decoded-match");
    build(&path, |scan| {
        commit_html(
            scan,
            &format!("{BASE}/"),
            r##"<html><body><a href="/d#%73ection">x</a></body></html>"##,
        );
        commit_html(
            scan,
            &format!("{BASE}/d"),
            r#"<html><body><div id="section"></div></body></html>"#,
        );
    });

    let hit = one(&evaluate_default(&path), "/d#%73ection");
    assert_eq!(
        hit["match"],
        json!({"kind": "element_id", "value": "section"})
    );
    assert_eq!(hit["decoded_fragment"], json!("section"));
}

#[test]
fn serialized_anchor_name_wins_before_decoding() {
    // The serialized stage tries `<a name>` too: `name="%41"` beats a decoded `id="A"`.
    let (_dir, path) = fixture("anchor-name-first");
    let dest = r#"<html><body><a name="%41"></a><div id="A"></div></body></html>"#;
    build(&path, |scan| {
        commit_html(
            scan,
            &format!("{BASE}/"),
            r##"<html><body><a href="/d#%41">x</a></body></html>"##,
        );
        commit_html(scan, &format!("{BASE}/d"), dest);
    });

    let hit = one(&evaluate_default(&path), "/d#%41");
    assert_eq!(hit["state"], json!("resolved"));
    assert_eq!(hit["match"], json!({"kind": "anchor_name", "value": "%41"}));
}

#[test]
fn serialized_anchor_name_resolves_without_a_decoded_target() {
    let (_dir, path) = fixture("anchor-name-only");
    build(&path, |scan| {
        commit_html(
            scan,
            &format!("{BASE}/"),
            r##"<html><body><a href="/d#%41">x</a></body></html>"##,
        );
        commit_html(
            scan,
            &format!("{BASE}/d"),
            r#"<html><body><a name="%41"></a></body></html>"#,
        );
    });

    let hit = one(&evaluate_default(&path), "/d#%41");
    assert_eq!(hit["match"], json!({"kind": "anchor_name", "value": "%41"}));
}

#[test]
fn percent_decoding_utf8_plus_and_malformed() {
    let (_dir, path) = fixture("percent");
    let dest = concat!(
        r#"<html><body><div id="日本語"></div><div id="a+b"></div>"#,
        r#"<div id="a%2Bb"></div><div id="a b"></div><div id="100%"></div>"#,
        "</body></html>"
    );
    let src = concat!(
        r#"<html><body><a href="/d#%E6%97%A5%E6%9C%AC%E8%AA%9E">utf8</a>"#,
        r#"<a href="/d#a%2Bb">encoded-plus</a><a href="/d#a+b">literal-plus</a>"#,
        r#"<a href="/d#a%20b">space</a><a href="/d#100%25">percent</a>"#,
        r#"<a href="/d#%zz">malformed</a><a href="/d#%FF">invalid-utf8</a>"#,
        "</body></html>"
    );
    build(&path, |scan| {
        commit_html(scan, &format!("{BASE}/"), src);
        commit_html(scan, &format!("{BASE}/d"), dest);
    });

    let result = evaluate_default(&path);
    assert_eq!(
        one(&result, "/d#%E6%97%A5%E6%9C%AC%E8%AA%9E")["state"],
        json!("resolved")
    );
    // `a%2Bb`: literal id `a%2Bb` wins serialized-first over decoded `a+b`.
    assert_eq!(
        one(&result, "/d#a%2Bb")["match"],
        json!({"kind": "element_id", "value": "a%2Bb"})
    );
    // `a+b`: `+` is a literal plus -- decoded `a+b`, never `a b`.
    assert_eq!(
        one(&result, "/d#a+b")["match"],
        json!({"kind": "element_id", "value": "a+b"})
    );
    assert_eq!(
        one(&result, "/d#a%20b")["match"],
        json!({"kind": "element_id", "value": "a b"})
    );
    assert_eq!(
        one(&result, "/d#100%25")["match"],
        json!({"kind": "element_id", "value": "100%"})
    );
    let malformed = one(&result, "/d#%zz");
    assert_eq!(malformed["state"], json!("missing"));
    // Malformed escapes pass through.
    assert_eq!(malformed["decoded_fragment"], json!("%zz"));
    let invalid = one(&result, "/d#%FF");
    assert_eq!(invalid["state"], json!("missing"));
    // Invalid UTF-8 becomes U+FFFD.
    assert_eq!(invalid["decoded_fragment"], json!("\u{FFFD}"));
}

#[test]
fn empty_fragment_top_and_empty_id() {
    let (_dir, path) = fixture("empty-top");
    let src = concat!(
        r##"<html><body><a href="#">empty</a><a href="#top">top</a>"##,
        r##"<a href="#TOP">upper-top</a><div id=""></div></body></html>"##
    );
    build(&path, |scan| {
        commit_html(scan, &format!("{BASE}/"), src);
        commit_html(
            scan,
            &format!("{BASE}/with-top-id"),
            r#"<html><body><div id="top"></div></body></html>"#,
        );
        commit_html(
            scan,
            &format!("{BASE}/p"),
            concat!(
                r##"<html><body><a href="/with-top-id#top">x</a>"##,
                r##"<a href="/with-top-id#">top of other page</a></body></html>"##
            ),
        );
    });

    let result = evaluate_default(&path);
    assert_eq!(
        one(&result, "#")["match"],
        json!({"kind": "top", "value": ""})
    );
    assert_eq!(
        one(&result, "#top")["match"],
        json!({"kind": "top", "value": "top"})
    );
    assert_eq!(
        one(&result, "#TOP")["match"],
        json!({"kind": "top", "value": "top"})
    );
    assert_eq!(
        one(&result, "/with-top-id#")["match"],
        json!({"kind": "top", "value": ""})
    );
    // A real `top` element wins before the top-of-document fallback.
    assert_eq!(
        one(&result, "/with-top-id#top")["match"],
        json!({"kind": "element_id", "value": "top"})
    );
}

#[test]
fn anchor_name_legacy_policy() {
    let (_dir, path) = fixture("legacy-name");
    let html = concat!(
        r##"<html><body><a name="legacy"></a><a href="#legacy">by-name</a>"##,
        r##"<a href="#unknown">none</a></body></html>"##
    );
    build(&path, |scan| commit_html(scan, &format!("{BASE}/"), html));

    let result = evaluate_default(&path);
    assert_eq!(
        one(&result, "#legacy")["match"],
        json!({"kind": "anchor_name", "value": "legacy"})
    );
    assert_eq!(one(&result, "#unknown")["state"], json!("missing"));
}

#[test]
fn complete_empty_destination_proves_target_absent() {
    let (_dir, path) = fixture("empty-destination");
    build(&path, |scan| {
        commit_html(
            scan,
            &format!("{BASE}/"),
            r##"<html><body><a href="/empty#x">x</a></body></html>"##,
        );
        commit(scan, &format!("{BASE}/empty"), Some(b""), None, |_| {});
    });

    assert_eq!(
        one(&evaluate_default(&path), "/empty#x")["state"],
        json!("missing")
    );
}

#[test]
fn incomplete_evidence_is_named_never_a_finding() {
    let (_dir, path) = fixture("incomplete");
    let src = concat!(
        r##"<html><body><a href="/truncated#x">t</a><a href="/omitted#x">o</a>"##,
        r##"<a href="/failed#x">f</a><a href="/nodoc#x">n</a><a href="/doc.pdf#x">p</a>"##,
        "</body></html>"
    );
    build(&path, |scan| {
        commit_html(scan, &format!("{BASE}/"), src);
        commit(
            scan,
            &format!("{BASE}/truncated"),
            Some(b"<html><body>partial"),
            None,
            |e| {
                e.body_state = "truncated".into();
                e.body_reason = "truncated".into();
            },
        );
        commit(
            scan,
            &format!("{BASE}/omitted"),
            Some(b"<html><body>budget</body></html>"),
            None,
            |e| {
                e.body_state = "omitted".into();
                e.body_reason = "body_budget_exhausted".into();
            },
        );
        commit(scan, &format!("{BASE}/failed"), Some(b""), None, |e| {
            e.body_state = "unavailable".into();
            e.body_reason = "fetch_failed".into();
            e.body_fidelity = "unavailable".into();
        });
        // No captures: no document row at all.
        commit(scan, &format!("{BASE}/nodoc"), None, None, |_| {});
        commit(
            scan,
            &format!("{BASE}/doc.pdf"),
            Some(b"%PDF-1.4"),
            Some(record(
                &format!("{BASE}/doc.pdf"),
                &[("content_type", json!("application/pdf"))],
            )),
            |e| {
                e.content_type = "application/pdf".into();
                e.response_headers = vec![("content-type".into(), "application/pdf".into())];
                e.effective_headers = vec![("content-type".into(), "application/pdf".into())];
            },
        );
    });

    let result = evaluate_default(&path);
    let mut reasons = Map::new();
    for item in occurrences(&result) {
        let raw = item["raw_href"].as_str().expect("raw_href");
        let key = raw
            .rsplit('/')
            .next()
            .expect("segment")
            .split('#')
            .next()
            .expect("key");
        reasons.insert(key.to_owned(), item["reason"].clone());
    }
    assert_eq!(reasons["truncated"], json!("destination_body_truncated"));
    assert_eq!(reasons["omitted"], json!("destination_body_omitted"));
    assert_eq!(reasons["failed"], json!("destination_body_unavailable"));
    assert_eq!(reasons["nodoc"], json!("destination_document_absent"));
    assert_eq!(reasons["doc.pdf"], json!("destination_not_html"));
    assert_eq!(result["states"]["missing"], json!(0));
    assert_eq!(result["states"]["skipped"], json!(5));
    assert_eq!(result["coverage"]["state"], json!("partial"));
    assert_eq!(result["coverage"]["occurrences_skipped"], json!(5));
    assert_eq!(
        result["coverage"]["skip_reasons"],
        json!({
            "destination_body_omitted": 1,
            "destination_body_truncated": 1,
            "destination_body_unavailable": 1,
            "destination_document_absent": 1,
            "destination_not_html": 1
        })
    );
}

#[test]
fn non_html_media_types_are_named_not_substring_matched() {
    // `text/html` is a media type, not a substring: `application/xhtml+xml` follows XML
    // fragment rules and `application/nothtml` is not HTML either, so both are named skips.
    let (_dir, path) = fixture("media-types");
    build(&path, |scan| {
        commit_html(
            scan,
            &format!("{BASE}/"),
            r##"<html><body><a href="/x#t">x</a><a href="/n#t">n</a></body></html>"##,
        );
        for (path_part, content_type) in [
            ("/x", "application/xhtml+xml"),
            ("/n", "application/nothtml"),
        ] {
            let url = format!("{BASE}{path_part}");
            commit(
                scan,
                &url,
                Some(br#"<html><body><div id="t"></div></body></html>"#),
                Some(record(&url, &[("content_type", json!(content_type))])),
                |e| {
                    e.content_type = content_type.into();
                    e.response_headers = vec![("content-type".into(), content_type.into())];
                    e.effective_headers = vec![("content-type".into(), content_type.into())];
                },
            );
        }
    });

    let result = evaluate_default(&path);
    assert_eq!(
        one(&result, "/x#t")["reason"],
        json!("destination_not_html")
    );
    assert_eq!(
        one(&result, "/n#t")["reason"],
        json!("destination_not_html")
    );
    assert_eq!(result["states"]["missing"], json!(0));
    assert_eq!(result["coverage"]["state"], json!("partial"));
}

#[test]
fn unavailable_source_document_is_named() {
    let (_dir, path) = fixture("unavailable-source");
    build(&path, |scan| {
        commit(
            scan,
            &format!("{BASE}/broken-source"),
            Some(b"<html><body>partial"),
            None,
            |e| {
                e.body_state = "truncated".into();
                e.body_reason = "truncated".into();
            },
        );
        commit(scan, &format!("{BASE}/no-capture"), None, None, |_| {});
    });

    let result = evaluate_default(&path);
    assert_eq!(result["occurrences"], json!([]));
    let unavailable: Vec<(String, String)> = result["coverage"]["unavailable_sources"]
        .as_array()
        .expect("unavailable_sources")
        .iter()
        .map(|item| {
            (
                item["url"].as_str().expect("url").to_owned(),
                item["reason"].as_str().expect("reason").to_owned(),
            )
        })
        .collect();
    assert!(unavailable.contains(&(
        format!("{BASE}/broken-source"),
        "body_truncated/truncated".to_owned()
    )));
    assert!(unavailable.contains(&(format!("{BASE}/no-capture"), "document_absent".to_owned())));
    assert_eq!(result["coverage"]["state"], json!("partial"));
}

#[test]
fn template_content_is_inert() {
    let (_dir, path) = fixture("template");
    let html = concat!(
        r#"<html><body><template><div id="ghost"></div>"#,
        r##"<a href="#ghost2">inert-link</a></template>"##,
        r##"<a href="#ghost">live-link</a></body></html>"##
    );
    build(&path, |scan| commit_html(scan, &format!("{BASE}/"), html));

    let result = evaluate_default(&path);
    let raw_hrefs: Vec<Value> = occurrences(&result)
        .iter()
        .map(|item| item["raw_href"].clone())
        .collect();
    assert_eq!(raw_hrefs, vec![json!("#ghost")]);
    assert_eq!(one(&result, "#ghost")["state"], json!("missing"));
}

#[test]
fn base_href_resolution() {
    let (_dir, path) = fixture("base-href");
    build(&path, |scan| {
        commit_html(
            scan,
            &format!("{BASE}/dir/page"),
            r##"<html><head><base href="/base/"></head><body><a href="guide#sec">x</a></body></html>"##,
        );
        commit_html(
            scan,
            &format!("{BASE}/base/guide"),
            r#"<html><body><div id="sec"></div></body></html>"#,
        );
    });

    let hit = one(&evaluate_default(&path), "guide#sec");
    assert_eq!(hit["state"], json!("resolved"));
    assert_eq!(hit["destination_url"], json!(format!("{BASE}/base/guide")));
}

#[test]
fn redirect_destination_uses_landing_document() {
    let (_dir, path) = fixture("redirect");
    let moved = b"<html><body>moved</body></html>";
    build(&path, |scan| {
        commit_html(
            scan,
            &format!("{BASE}/"),
            r##"<html><body><a href="/old#sec">follow</a><a href="/gone#sec">gone</a></body></html>"##,
        );
        commit(
            scan,
            &format!("{BASE}/old"),
            Some(moved),
            Some(record(
                &format!("{BASE}/old"),
                &[
                    ("status_code", json!(301)),
                    ("redirect_url", json!(format!("{BASE}/new"))),
                ],
            )),
            |e| e.status_code = Some(301),
        );
        commit_html(
            scan,
            &format!("{BASE}/new"),
            r#"<html><body><div id="sec"></div></body></html>"#,
        );
        commit(
            scan,
            &format!("{BASE}/gone"),
            Some(moved),
            Some(record(
                &format!("{BASE}/gone"),
                &[
                    ("status_code", json!(301)),
                    ("redirect_url", json!(format!("{BASE}/absent"))),
                ],
            )),
            |e| e.status_code = Some(301),
        );
    });

    let result = evaluate_default(&path);
    let followed = one(&result, "/old#sec");
    assert_eq!(followed["state"], json!("resolved"));
    assert_eq!(
        followed["destination_document_url"],
        json!(format!("{BASE}/new"))
    );
    assert_eq!(
        one(&result, "/gone#sec")["reason"],
        json!("destination_redirect_target_absent")
    );
}

#[test]
fn fragment_directive_is_a_named_skip() {
    let (_dir, path) = fixture("directive");
    let html = concat!(
        r##"<html><body><a href="#:~:text=quoted">text-only</a>"##,
        r##"<a href="#sec:~:text=quoted">element-plus</a><div id="sec"></div>"##,
        r##"<a href="#gap:~:text=quoted">gap</a></body></html>"##
    );
    build(&path, |scan| commit_html(scan, &format!("{BASE}/"), html));

    let result = evaluate_default(&path);
    let text_only = one(&result, "#:~:text=quoted");
    assert_eq!(text_only["state"], json!("skipped"));
    assert_eq!(text_only["reason"], json!("fragment_directive_unverified"));
    let element_plus = one(&result, "#sec:~:text=quoted");
    assert_eq!(element_plus["state"], json!("resolved"));
    assert_eq!(element_plus["note"], json!("fragment_directive_unverified"));
    assert_eq!(
        one(&result, "#gap:~:text=quoted")["reason"],
        json!("fragment_directive_unverified")
    );
}

#[test]
fn pagination_is_stable_and_reports_coverage() {
    let (_dir, path) = fixture("pagination");
    let anchors: String = (0..5)
        .map(|i| format!(r##"<a href="#m{i}">m{i}</a>"##))
        .collect();
    build(&path, |scan| {
        commit_html(
            scan,
            &format!("{BASE}/"),
            &format!("<html><body>{anchors}</body></html>"),
        );
    });

    let capped = evaluate_at(&path, 3);
    assert_eq!(occurrences(&capped).len(), 3);
    assert_eq!(capped["coverage"]["occurrences_omitted"], json!(2));
    assert_eq!(capped["coverage"]["occurrences_seen"], json!(5));
    assert_eq!(capped["coverage"]["state"], json!("partial"));

    let full = evaluate_default(&path);
    let first = paginate(&full, 0, 2, None, None);
    let again = paginate(&full, 0, 2, None, None);
    assert_eq!(first, again);
    assert_eq!(first["total"], json!(5));
    assert_eq!(first["returned"], json!(2));
    assert_eq!(first["next_offset"], json!(2));
    let raw: Vec<Value> = first["occurrences"]
        .as_array()
        .expect("occurrences")
        .iter()
        .map(|item| item["raw_href"].clone())
        .collect();
    assert_eq!(raw, vec![json!("#m0"), json!("#m1")]);
    let tail = paginate(&full, 4, 2, None, None);
    assert_eq!(tail["returned"], json!(1));
    assert!(tail["next_offset"].is_null());
    let missing_only = paginate(&full, 0, 100, Some("missing"), None);
    assert_eq!(missing_only["total"], json!(5));
    assert_eq!(missing_only["states"]["missing"], json!(5));
    let empty = paginate(&full, 0, 100, Some("resolved"), None);
    assert_eq!(empty["total"], json!(0));
    assert_eq!(empty["occurrences"], json!([]));
}

#[test]
fn malformed_fragment_href_is_a_named_skip() {
    // A fragment-bearing href the URL resolver refuses is unanswered evidence: dropping it
    // would report the page as fully verified.
    let (_dir, path) = fixture("malformed-href");
    let html = concat!(
        r##"<html><body><a href="#yes">y</a><a href="http://[bad#frag">bad</a>"##,
        r#"<div id="yes"></div></body></html>"#
    );
    build(&path, |scan| commit_html(scan, &format!("{BASE}/"), html));

    let result = evaluate_default(&path);
    let bad = one(&result, "http://[bad#frag");
    assert_eq!(bad["state"], json!("skipped"));
    assert_eq!(bad["reason"], json!("href_unresolvable"));
    assert_eq!(bad["resolved_url"], json!(""));
    assert_eq!(bad["fragment"], json!("frag"));
    assert_eq!(
        result["states"],
        json!({"resolved": 1, "missing": 0, "skipped": 1})
    );
    assert_eq!(result["coverage"]["anchors_seen"], json!(2));
    assert_eq!(result["coverage"]["state"], json!("partial"));
    assert_eq!(
        result["coverage"]["skip_reasons"],
        json!({"href_unresolvable": 1})
    );
}

#[test]
fn result_is_json_serializable_and_bounded() {
    let (_dir, path) = fixture("json");
    build(&path, |scan| {
        commit_html(
            scan,
            &format!("{BASE}/"),
            r##"<html><body><a href="#x">x</a></body></html>"##,
        );
    });

    let result = evaluate_default(&path);
    let encoded = serde_json::to_string(&Value::Object(result)).expect("serialize");
    let decoded: Value = serde_json::from_str(&encoded).expect("parse");
    assert_eq!(decoded["analysis"], json!("fragment_links.v1"));
}

#[test]
fn evaluation_is_deterministic() {
    let (_dir, path) = fixture("deterministic");
    build(&path, |scan| {
        commit_html(
            scan,
            &format!("{BASE}/"),
            r##"<html><body><a href="/b#x">1</a><a href="#y">2</a></body></html>"##,
        );
        commit_html(
            scan,
            &format!("{BASE}/b"),
            r#"<html><body><div id="x"></div></body></html>"#,
        );
    });

    assert_eq!(evaluate_default(&path), evaluate_default(&path));
}
