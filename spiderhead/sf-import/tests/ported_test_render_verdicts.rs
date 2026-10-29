//! Ported tests from the reference suite — only the retained
//! mobile-layout part is portable; `check_rendered_elements` lives in
//! the reference suite (slice sf-core-pack-04).

use serde_json::{json, Value};
use spiderhead_sf_import::context::AuditContext;
use spiderhead_sf_import::loader::LoadedExports;
use spiderhead_sf_import::mobile_checks::check_mobile_render_layout;

fn mobile_evidence(overflow: bool, small_text: bool, small_target: bool) -> Value {
    json!({
        "schema": "mobile_layout.v1",
        "state": "complete",
        "reason": "",
        "viewport_width_px": 390,
        "document_width_px": if overflow { 430 } else { 390 },
        "content_overflow_px": if overflow { 40 } else { 0 },
        "content_overflow": overflow,
        "small_text_count_lower_bound": if small_text { 1 } else { 0 },
        "small_target_count_lower_bound": if small_target { 1 } else { 0 },
        "thresholds": {"small_text_below_px": 12, "small_target_below_px": 48},
        "details": {
            "overflow_elements": [],
            "small_text_elements": [],
            "small_target_elements": []
        }
    })
}

/// Minimal retained schema the check's SQL needs (urls/documents/pages) with
/// one rendered row carrying `renderer_json`.
fn ctx_with_render_row(renderer: &Value) -> AuditContext {
    let con = rusqlite::Connection::open_in_memory().expect("in-memory con");
    con.execute_batch(
        "CREATE TABLE urls (url_id INTEGER PRIMARY KEY, url TEXT);
         CREATE TABLE documents (document_id INTEGER PRIMARY KEY, url_id INTEGER, renderer_json TEXT);
         CREATE TABLE pages (
             url_id INTEGER, document_id INTEGER, representation TEXT,
             status_code INTEGER, content_type TEXT
         );",
    )
    .expect("schema");
    con.execute(
        "INSERT INTO urls (url_id, url) VALUES (1, 'https://example.test/')",
        [],
    )
    .expect("url row");
    con.execute(
        "INSERT INTO documents (document_id, url_id, renderer_json) VALUES (1, 1, ?1)",
        [serde_json::to_string(renderer).expect("renderer json")],
    )
    .expect("document row");
    con.execute(
        "INSERT INTO pages (url_id, document_id, representation, status_code, content_type) \
         VALUES (1, 1, 'rendered', 200, 'text/html')",
        [],
    )
    .expect("page row");
    let mut ctx = AuditContext::new(LoadedExports::default(), json!({}), None, false).expect("ctx");
    ctx.scan_con = Some(con);
    ctx
}

fn fired(ctx: &AuditContext) -> std::collections::BTreeSet<String> {
    ctx.issues_iter().iter().map(|i| i.check.clone()).collect()
}

#[test]
fn mobile_render_checks_fire_on_measured_defects_and_stay_silent_when_clean() {
    let expected: std::collections::BTreeSet<String> = [
        "MOBILE_CONTENT_OVERFLOW",
        "MOBILE_TEXT_TOO_SMALL",
        "MOBILE_TARGET_TOO_SMALL",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    let mut defective = ctx_with_render_row(&json!({
        "mobile_layout": mobile_evidence(true, true, true)
    }));
    check_mobile_render_layout(&mut defective).expect("mobile check");
    assert!(expected.is_subset(&fired(&defective)));

    let mut clean = ctx_with_render_row(&json!({
        "mobile_layout": mobile_evidence(false, false, false)
    }));
    check_mobile_render_layout(&mut clean).expect("mobile check");
    assert!(fired(&clean).is_disjoint(&expected));
}
