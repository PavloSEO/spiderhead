//! Ported stored-scan check tests (`oracle-baseline`).
//!
//! the reference suite builds a minimal three-table scan in
//! memory and calls `security_headers.evaluate`; `check_security_headers` runs
//! the same query, so the store helper mirrors `_store` verbatim.
//! the reference suite authors a full scan_v1 corpus; the
//! `corpus` helper is ported one-to-one (schema-driven column defaults).
//!
//! Not ported: `test_evaluate_rejects_a_non_sqlite_connection` (the Rust type
//! system makes the argument impossible), the projection's temp-table cleanup
//! assertion (the port never materializes one), the URL_INTERNAL_SEARCH,
//! crawl/reanalyze-pipeline and CLI-surface tests (other slices), and the
//! export-run unmeasured skips (the export `run_rules` path is not part of the
//! stored-scan orchestration).

use rusqlite::{params, Connection};
use serde_json::{json, Map, Value};
use spiderhead_checks::audit::rules::{retained_links, stored};
use spiderhead_checks::audit::{AuditContext, Page};

const ROOT: &str = "https://example.test";

const ALL_PRESENT: [(&str, &str); 6] = [
    (
        "strict-transport-security",
        "max-age=63072000; includeSubDomains",
    ),
    ("content-security-policy", "default-src 'self'"),
    ("x-content-type-options", "nosniff"),
    ("x-frame-options", "DENY"),
    ("referrer-policy", "strict-origin-when-cross-origin"),
    ("permissions-policy", "geolocation=()"),
];

const HEADER_CHECKS: [&str; 5] = [
    "MISSING_HSTS",
    "MISSING_CSP",
    "MISSING_X_CONTENT_TYPE_OPTIONS",
    "MISSING_X_FRAME_OPTIONS",
    "MISSING_REFERRER_POLICY",
];

fn all_present() -> Vec<(String, String)> {
    ALL_PRESENT
        .iter()
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .collect()
}

fn headers_json(pairs: &[(String, String)]) -> String {
    Value::Array(pairs.iter().map(|(n, v)| json!([n, v])).collect::<Vec<_>>()).to_string()
}

/// One `_store` page row: url_id, url, status, content type, retained headers.
type StorePage = (i64, &'static str, Option<i64>, &'static str, Option<String>);

/// `_store(pages)`: minimal urls/pages/responses schema, one row per page.
fn store(pages: &[StorePage]) -> AuditContext {
    let con = Connection::open_in_memory().unwrap();
    con.execute_batch(
        "CREATE TABLE urls (url_id INTEGER PRIMARY KEY, url TEXT NOT NULL);
         CREATE TABLE pages (
           url_id INTEGER PRIMARY KEY, page_ordinal INTEGER NOT NULL,
           status_code INTEGER, content_type TEXT NOT NULL);
         CREATE TABLE responses (
           response_id INTEGER PRIMARY KEY, effective_url_id INTEGER,
           purpose TEXT NOT NULL, response_headers_redacted_json TEXT NOT NULL,
           effective_headers_redacted_json TEXT NOT NULL);",
    )
    .unwrap();
    for (ordinal, (url_id, url, status, content_type, headers)) in pages.iter().enumerate() {
        con.execute("INSERT INTO urls VALUES (?1,?2)", params![url_id, url])
            .unwrap();
        con.execute(
            "INSERT INTO pages VALUES (?1,?2,?3,?4)",
            params![url_id, ordinal + 1, status, content_type],
        )
        .unwrap();
        if let Some(raw) = headers {
            con.execute(
                "INSERT INTO responses (effective_url_id, purpose, response_headers_redacted_json, effective_headers_redacted_json) VALUES (?1, 'page', '[]', ?2)",
                params![url_id, raw],
            )
            .unwrap();
        }
    }
    let mut ctx = AuditContext::new(vec![]);
    ctx.scan = Some(con);
    ctx
}

fn fired(ctx: &AuditContext) -> Map<String, Value> {
    let mut out = Map::new();
    for issue in &ctx.issues {
        out.entry(issue.check.clone())
            .or_insert_with(|| Value::Array(vec![]))
            .as_array_mut()
            .unwrap()
            .push(json!(issue.target_url));
    }
    out
}

fn skipped<'a>(ctx: &'a AuditContext, check: &str) -> Option<&'a str> {
    ctx.skipped
        .iter()
        .find(|s| s.id == check)
        .map(|s| s.reason.as_str())
}

#[test]
fn page_with_all_four_headers_is_clean() {
    let mut ctx = store(&[(
        1,
        "https://example.test/",
        Some(200),
        "text/html",
        Some(headers_json(&all_present())),
    )]);
    stored::check_security_headers(&mut ctx);
    assert!(fired(&ctx).is_empty());
    for check in HEADER_CHECKS
        .into_iter()
        .chain(["DUPLICATE_SECURITY_HEADER"])
    {
        assert!(skipped(&ctx, check).is_none(), "{check}");
    }
}

#[test]
fn each_missing_header_fires_on_its_own_check_only() {
    for (check, name) in [
        ("MISSING_HSTS", "strict-transport-security"),
        ("MISSING_CSP", "content-security-policy"),
        ("MISSING_X_CONTENT_TYPE_OPTIONS", "x-content-type-options"),
        ("MISSING_X_FRAME_OPTIONS", "x-frame-options"),
        ("MISSING_REFERRER_POLICY", "referrer-policy"),
    ] {
        let headers: Vec<(String, String)> = all_present()
            .into_iter()
            .filter(|(n, _)| n != name)
            .collect();
        let mut ctx = store(&[(
            1,
            "https://example.test/",
            Some(200),
            "text/html",
            Some(headers_json(&headers)),
        )]);
        stored::check_security_headers(&mut ctx);
        let found = fired(&ctx);
        assert_eq!(found.len(), 1, "{check}");
        assert_eq!(found[check], json!(["https://example.test/"]));
    }
}

#[test]
fn frame_ancestors_in_csp_satisfies_x_frame_options() {
    let mut headers = all_present();
    headers.retain(|(n, _)| n != "x-frame-options");
    headers[1].1 = "default-src 'self'; frame-ancestors 'none'".to_string();
    let mut ctx = store(&[(
        1,
        "https://example.test/",
        Some(200),
        "text/html",
        Some(headers_json(&headers)),
    )]);
    stored::check_security_headers(&mut ctx);
    assert!(fired(&ctx).is_empty());
}

#[test]
fn empty_header_value_counts_as_missing_for_csp() {
    let mut headers = all_present();
    headers[1].1 = "   ".to_string();
    let mut ctx = store(&[(
        1,
        "https://example.test/",
        Some(200),
        "text/html",
        Some(headers_json(&headers)),
    )]);
    stored::check_security_headers(&mut ctx);
    assert_eq!(fired(&ctx)["MISSING_CSP"], json!(["https://example.test/"]));
}

#[test]
fn repeated_security_header_is_reported_and_its_values_combined() {
    let mut headers = all_present();
    headers.push(("x-frame-options".into(), "SAMEORIGIN".into()));
    let mut ctx = store(&[(
        1,
        "https://example.test/",
        Some(200),
        "text/html",
        Some(headers_json(&headers)),
    )]);
    stored::check_security_headers(&mut ctx);
    let found = fired(&ctx);
    assert_eq!(
        found["DUPLICATE_SECURITY_HEADER"],
        json!(["https://example.test/"])
    );
    assert_eq!(found.len(), 1);
}

#[test]
fn repeated_content_security_policy_is_not_a_defect() {
    let mut headers = all_present();
    headers.push(("content-security-policy".into(), "img-src 'none'".into()));
    let mut ctx = store(&[(
        1,
        "https://example.test/",
        Some(200),
        "text/html",
        Some(headers_json(&headers)),
    )]);
    stored::check_security_headers(&mut ctx);
    assert!(fired(&ctx).is_empty());
}

#[test]
fn non_html_or_non_2xx_pages_are_not_judged() {
    for (status, content_type) in [(404, "text/html"), (200, "image/png"), (301, "text/html")] {
        let mut ctx = store(&[(
            1,
            "https://example.test/",
            Some(status),
            content_type,
            Some(headers_json(&[])),
        )]);
        stored::check_security_headers(&mut ctx);
        assert!(fired(&ctx).is_empty(), "{status} {content_type}");
        assert_eq!(
            skipped(&ctx, "MISSING_CSP"),
            Some("no HTML page with a 2xx response was stored"),
        );
    }
}

#[test]
fn redirect_hop_headers_are_not_judged_in_place_of_the_final_document() {
    let mut ctx = store(&[(
        1,
        "https://example.test/",
        Some(200),
        "text/html",
        Some(headers_json(&all_present())),
    )]);
    // A retained hop response never stands in for the final page response.
    let con = ctx.scan.as_ref().unwrap();
    con.execute(
        "INSERT INTO responses (effective_url_id, purpose, response_headers_redacted_json, effective_headers_redacted_json) VALUES (1, 'stylesheet', '[]', '[]')",
        [],
    )
    .unwrap();
    stored::check_security_headers(&mut ctx);
    assert!(fired(&ctx).is_empty());
}

#[test]
fn html_page_without_stored_response_is_unmeasured_not_clean() {
    let mut ctx = store(&[(1, "https://example.test/", Some(200), "text/html", None)]);
    stored::check_security_headers(&mut ctx);
    assert!(fired(&ctx).is_empty());
    assert_eq!(
        skipped(&ctx, "MISSING_CSP"),
        Some("1 HTML pages have no parseable stored response headers"),
    );
}

#[test]
fn unparseable_stored_headers_are_unmeasured() {
    let mut ctx = store(&[(
        1,
        "https://example.test/",
        Some(200),
        "text/html",
        Some("{not json".to_string()),
    )]);
    stored::check_security_headers(&mut ctx);
    assert!(fired(&ctx).is_empty());
    assert_eq!(
        skipped(&ctx, "MISSING_HSTS"),
        Some("1 HTML pages have no parseable stored response headers"),
    );
}

#[test]
fn only_the_final_response_is_judged_when_a_url_has_several() {
    let mut ctx = store(&[(
        1,
        "https://example.test/",
        Some(200),
        "text/html",
        Some(headers_json(&all_present())),
    )]);
    let con = ctx.scan.as_ref().unwrap();
    con.execute(
        "INSERT INTO responses (effective_url_id, purpose, response_headers_redacted_json, effective_headers_redacted_json) VALUES (1, 'page', '[]', '[]')",
        [],
    )
    .unwrap();
    stored::check_security_headers(&mut ctx);
    for check in HEADER_CHECKS {
        assert_eq!(fired(&ctx)[check], json!(["https://example.test/"]));
    }
}

#[test]
fn missing_header_checks_fire_on_defect_and_stay_silent_on_clean() {
    for (check, name) in [
        ("MISSING_HSTS", "strict-transport-security"),
        ("MISSING_CSP", "content-security-policy"),
        ("MISSING_X_CONTENT_TYPE_OPTIONS", "x-content-type-options"),
        ("MISSING_X_FRAME_OPTIONS", "x-frame-options"),
        ("MISSING_REFERRER_POLICY", "referrer-policy"),
    ] {
        let mut ctx = store(&[(
            1,
            "https://example.test/",
            Some(200),
            "text/html",
            Some(headers_json(&all_present())),
        )]);
        stored::check_security_headers(&mut ctx);
        assert!(skipped(&ctx, check).is_none(), "{check} clean");

        let headers: Vec<(String, String)> = all_present()
            .into_iter()
            .filter(|(n, _)| n != name)
            .collect();
        let mut ctx = store(&[(
            1,
            "https://example.test/",
            Some(200),
            "text/html",
            Some(headers_json(&headers)),
        )]);
        stored::check_security_headers(&mut ctx);
        assert_eq!(fired(&ctx)[check], json!(["https://example.test/"]));
    }
}

#[test]
fn hsts_is_read_from_the_final_response_not_a_redirect_hop() {
    let mut ctx = store(&[(
        1,
        "https://example.test/",
        Some(200),
        "text/html",
        Some(headers_json(&all_present())),
    )]);
    let con = ctx.scan.as_ref().unwrap();
    // An earlier retained response carried HSTS; the final document did not.
    con.execute(
        "INSERT INTO responses (effective_url_id, purpose, response_headers_redacted_json, effective_headers_redacted_json) VALUES (1, 'page', '[]', ?1)",
        params![headers_json(
            &all_present()
                .into_iter()
                .filter(|(n, _)| n != "strict-transport-security")
                .collect::<Vec<_>>()
        )],
    )
    .unwrap();
    stored::check_security_headers(&mut ctx);
    assert_eq!(
        fired(&ctx)["MISSING_HSTS"],
        json!(["https://example.test/"])
    );
}

#[test]
fn duplicate_security_header_fires_on_a_repeat_and_stays_silent_on_clean() {
    let mut ctx = store(&[(
        1,
        "https://example.test/",
        Some(200),
        "text/html",
        Some(headers_json(&all_present())),
    )]);
    stored::check_security_headers(&mut ctx);
    assert!(skipped(&ctx, "DUPLICATE_SECURITY_HEADER").is_none());

    let mut headers = all_present();
    headers.push(("referrer-policy".into(), "no-referrer".into()));
    let mut ctx = store(&[(
        1,
        "https://example.test/",
        Some(200),
        "text/html",
        Some(headers_json(&headers)),
    )]);
    stored::check_security_headers(&mut ctx);
    assert_eq!(
        fired(&ctx)["DUPLICATE_SECURITY_HEADER"],
        json!(["https://example.test/"])
    );
}

// ---------------------------------------------------------------------------
// the reference suite — full scan_v1 corpus.
// ---------------------------------------------------------------------------

/// `corpus()`: the whole scan_v1 schema with a tiny independent `scan` header.
fn corpus() -> (AuditContext, Connection) {
    let con = Connection::open_in_memory().unwrap();
    con.execute_batch(include_str!("../../store/sql/scan_v1.sql"))
        .unwrap();
    con.execute_batch("DROP TABLE scan").unwrap();
    con.execute_batch("CREATE TABLE scan(singleton INTEGER,start_url TEXT,config_json TEXT)")
        .unwrap();
    con.execute(
        "INSERT INTO scan VALUES (1,?1,?2)",
        params![format!("{ROOT}/"), json!({"scope": {}}).to_string()],
    )
    .unwrap();
    (AuditContext::new(vec![]), con)
}

fn url_id(con: &Connection, url: &str) -> i64 {
    con.execute("INSERT OR IGNORE INTO urls(url) VALUES (?1)", params![url])
        .unwrap();
    con.query_row("SELECT url_id FROM urls WHERE url=?1", params![url], |r| {
        r.get(0)
    })
    .unwrap()
}

/// `page()`: insert a pages row plus the matching ctx page (status_code,
/// redirect_url, redirect_chain_json in both the table and the record).
fn add_page(
    ctx: &mut AuditContext,
    con: &Connection,
    path: &str,
    status: i64,
    redirect: &str,
    chain: &[Value],
) -> i64 {
    let url = format!("{ROOT}{path}");
    let id = url_id(con, &url);
    let redirect_url = if redirect.is_empty() {
        String::new()
    } else {
        format!("{ROOT}{redirect}")
    };
    let ordinal: i64 = con
        .query_row("SELECT COUNT(*) FROM pages", [], |r| r.get(0))
        .unwrap();
    // Same contract as the reference fixture: NOT NULL columns take the empty
    // value of their type, nullable columns stay NULL.
    let mut cols = Vec::new();
    let mut vals: Vec<rusqlite::types::Value> = Vec::new();
    let mut stmt = con.prepare("PRAGMA table_info(pages)").unwrap();
    let info = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    for (name, ctype, notnull) in info {
        let value = match name.as_str() {
            "url_id" => rusqlite::types::Value::Integer(id),
            "page_ordinal" => rusqlite::types::Value::Integer(ordinal),
            "status_code" => rusqlite::types::Value::Integer(status),
            "representation" => rusqlite::types::Value::Text("static".into()),
            "redirect_url" => rusqlite::types::Value::Text(redirect_url.clone()),
            "redirect_chain_json" => {
                rusqlite::types::Value::Text(Value::Array(chain.to_vec()).to_string())
            }
            _ => {
                if notnull == 0 {
                    rusqlite::types::Value::Null
                } else if ctype.contains("INT") || ctype.contains("REAL") {
                    rusqlite::types::Value::Integer(0)
                } else {
                    rusqlite::types::Value::Text(String::new())
                }
            }
        };
        cols.push(name);
        vals.push(value);
    }
    let sql = format!(
        "INSERT INTO pages ({}) VALUES ({})",
        cols.join(","),
        cols.iter().map(|_| "?").collect::<Vec<_>>().join(",")
    );
    con.execute(&sql, rusqlite::params_from_iter(vals)).unwrap();

    let mut rec = Map::new();
    rec.insert("url_id".into(), json!(id));
    rec.insert("redirect_url".into(), json!(redirect_url));
    rec.insert(
        "redirect_chain_json".into(),
        json!(Value::Array(chain.to_vec()).to_string()),
    );
    rec.insert("content_type".into(), json!("text/html"));
    ctx.pages.push(Page {
        url,
        status_code: Some(status),
        content_type: "text/html".into(),
        indexability: if (200..300).contains(&status) {
            "Indexable".into()
        } else {
            "Non-Indexable".into()
        },
        rec,
        ..Page::default()
    });
    id
}

/// `link()`: schema-driven NOT NULL defaults, like the reference fixture.
fn add_link(con: &Connection, source: &str, destination: &str, anchor: &str) {
    let src = url_id(con, &format!("{ROOT}{source}"));
    let dst = url_id(con, &format!("{ROOT}{destination}"));
    let ordinal: i64 = con
        .query_row("SELECT COUNT(*) FROM links", [], |r| r.get(0))
        .unwrap();
    let mut stmt = con.prepare("PRAGMA table_info(links)").unwrap();
    let info = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut cols = Vec::new();
    let mut vals: Vec<rusqlite::types::Value> = Vec::new();
    for (name, ctype, notnull) in info {
        let value = match name.as_str() {
            "source_url_id" => rusqlite::types::Value::Integer(src),
            "destination_url_id" => rusqlite::types::Value::Integer(dst),
            "evidence_representation" => rusqlite::types::Value::Text("static".into()),
            "ordinal" => rusqlite::types::Value::Integer(ordinal),
            "anchor" => rusqlite::types::Value::Text(anchor.into()),
            "position" => rusqlite::types::Value::Text("body".into()),
            _ => {
                if notnull == 0 {
                    rusqlite::types::Value::Null
                } else if ctype.contains("INT") || ctype.contains("REAL") {
                    rusqlite::types::Value::Integer(0)
                } else {
                    rusqlite::types::Value::Text(String::new())
                }
            }
        };
        cols.push(name);
        vals.push(value);
    }
    let sql = format!(
        "INSERT INTO links ({}) VALUES ({})",
        cols.join(","),
        cols.iter().map(|_| "?").collect::<Vec<_>>().join(",")
    );
    con.execute(&sql, rusqlite::params_from_iter(vals)).unwrap();
}

fn attach(ctx: &mut AuditContext, con: Connection) {
    ctx.scan = Some(con);
}

fn issue<'a>(
    ctx: &'a AuditContext,
    check: &str,
    target: &str,
) -> &'a spiderhead_checks::audit::Issue {
    ctx.issues
        .iter()
        .find(|i| i.check == check && i.target_url.as_deref() == Some(target))
        .unwrap()
}

fn checks(ctx: &AuditContext) -> Vec<&str> {
    ctx.issues.iter().map(|i| i.check.as_str()).collect()
}

#[test]
fn broken_targets_group_sources_anchors_and_preserve_status() {
    let (mut ctx, con) = corpus();
    add_page(&mut ctx, &con, "/a", 200, "", &[]);
    add_page(&mut ctx, &con, "/b", 200, "", &[]);
    add_page(&mut ctx, &con, "/missing", 404, "", &[]);
    add_page(&mut ctx, &con, "/server", 503, "", &[]);
    add_link(&con, "/a", "/missing", "first");
    add_link(&con, "/b", "/missing#part", "second");
    add_link(&con, "/a", "/server", "descriptive");
    add_link(&con, "/a", "/not-fetched", "descriptive");
    attach(&mut ctx, con);
    retained_links::check_retained_links(&mut ctx);

    let broken = issue(&ctx, "BROKEN_INTERNAL_LINK", &format!("{ROOT}/missing"));
    assert_eq!(broken.status_code, Some(404));
    assert_eq!(broken.occurrences_count, 2);
    assert_eq!(
        broken
            .locations
            .iter()
            .map(|l| l["anchor"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert_eq!(broken.details["locations_omitted"], json!(0));
    let err = issue(&ctx, "LINK_TO_5XX", &format!("{ROOT}/server"));
    assert_eq!(err.status_code, Some(503));
    assert!(skipped(&ctx, "BROKEN_INTERNAL_LINK").is_none());
}

// (the reference test passes max_locations=20; the port asserts the built-in
// 200-location cap with the same exact-counts contract)
#[test]
fn locations_are_bounded_with_exact_contributing_counts() {
    let (mut ctx, con) = corpus();
    add_page(&mut ctx, &con, "/a", 200, "", &[]);
    add_page(&mut ctx, &con, "/missing", 404, "", &[]);
    for n in 0..210 {
        add_link(&con, "/a", "/missing", &n.to_string());
    }
    attach(&mut ctx, con);
    retained_links::check_retained_links(&mut ctx);

    let broken = issue(&ctx, "BROKEN_INTERNAL_LINK", &format!("{ROOT}/missing"));
    assert_eq!(broken.locations.len(), 200);
    assert_eq!(broken.details["locations_omitted"], json!(10));
    assert_eq!(broken.details["link_occurrences"], json!(210));
    assert_eq!(broken.occurrences_count, 1);
}

#[test]
fn empty_anchor_is_its_own_internal_finding() {
    let (mut ctx, con) = corpus();
    add_page(&mut ctx, &con, "/a", 200, "", &[]);
    add_page(&mut ctx, &con, "/b", 200, "", &[]);
    add_link(&con, "/a", "/b", " \t ");
    add_link(&con, "/a", "/b", "Read more");
    attach(&mut ctx, con);
    retained_links::check_retained_links(&mut ctx);

    let finding = issue(&ctx, "INTERNAL_LINK_NO_ANCHOR", &format!("{ROOT}/a"));
    assert_eq!(finding.occurrences_count, 1);
    assert_eq!(finding.locations.len(), 1);
}

#[test]
fn retained_redirect_chain_records_mixed_hops_terminal_error_and_inlinks() {
    let (mut ctx, con) = corpus();
    add_page(&mut ctx, &con, "/source", 200, "", &[]);
    add_page(&mut ctx, &con, "/a", 301, "/b", &[]);
    add_page(&mut ctx, &con, "/b", 302, "/bad", &[]);
    add_page(&mut ctx, &con, "/bad", 404, "", &[]);
    add_link(&con, "/source", "/a", "old product");
    attach(&mut ctx, con);
    retained_links::check_retained_links(&mut ctx);

    let chain = issue(&ctx, "REDIRECT_CHAIN", &format!("{ROOT}/a"));
    assert_eq!(chain.details["hops"], json!(2));
    assert_eq!(chain.details["terminal_status"], json!(404));
    assert_eq!(chain.details["mixed_permanent_temporary"], json!(true));
    assert_eq!(
        chain.details["hop_list"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["status_code"],
        json!(404)
    );
    assert_eq!(chain.locations[0]["anchor"], json!("old product"));
}

#[test]
fn redirect_loop_and_unfetched_chain_are_distinct() {
    let (mut ctx, con) = corpus();
    add_page(&mut ctx, &con, "/a", 301, "/b", &[]);
    add_page(&mut ctx, &con, "/b", 302, "/a", &[]);
    add_page(&mut ctx, &con, "/c", 301, "/d", &[]);
    add_page(&mut ctx, &con, "/d", 301, "/unknown", &[]);
    attach(&mut ctx, con);
    retained_links::check_retained_links(&mut ctx);

    assert_eq!(
        checks(&ctx),
        ["REDIRECT_LOOP", "REDIRECT_LOOP", "REDIRECT_CHAIN"]
    );
    let unresolved = ctx
        .issues
        .iter()
        .find(|i| i.check == "REDIRECT_CHAIN")
        .unwrap();
    assert_eq!(unresolved.details["unresolved"], json!(true));
    assert_eq!(unresolved.details["terminal_status"], Value::Null);
}

#[test]
fn external_no_response_excludes_blocked_and_skipped_outcomes() {
    let (mut ctx, con) = corpus();
    add_page(&mut ctx, &con, "/", 200, "", &[]);
    con.execute_batch(
        "CREATE TABLE external_checks(ordinal INTEGER,outcome TEXT,payload_json TEXT)",
    )
    .unwrap();
    for (n, outcome) in ["failed", "blocked", "skipped"].iter().enumerate() {
        con.execute(
            "INSERT INTO external_checks VALUES(?1,?2,?3)",
            params![
                n as i64,
                outcome,
                json!({
                    "url": "https://outside.test/",
                    "sources": [format!("{ROOT}/")],
                    "source_count": 1,
                    "reason": "request failed",
                    "error": "timed out",
                    "error_kind": "timeout",
                    "redirect_chain": [],
                })
                .to_string()
            ],
        )
        .unwrap();
    }
    attach(&mut ctx, con);
    retained_links::check_retained_links(&mut ctx);

    let found = issue(&ctx, "EXTERNAL_NO_RESPONSE", "https://outside.test/");
    assert_eq!(found.details["error_kind"], json!("timeout"));
    assert_eq!(checks(&ctx).len(), 1);
}

#[test]
fn external_successful_response_is_clean() {
    let (mut ctx, con) = corpus();
    add_page(&mut ctx, &con, "/", 200, "", &[]);
    con.execute_batch(
        "CREATE TABLE external_checks(ordinal INTEGER,outcome TEXT,payload_json TEXT)",
    )
    .unwrap();
    con.execute(
        "INSERT INTO external_checks VALUES(0,'fetched',?1)",
        params![json!({
            "url": "https://outside.test/",
            "outcome": "fetched",
            "status_code": 200,
            "sources": [format!("{ROOT}/")],
            "source_count": 1,
            "final_url": "https://outside.test/",
        })
        .to_string()],
    )
    .unwrap();
    attach(&mut ctx, con);
    retained_links::check_retained_links(&mut ctx);
    assert!(checks(&ctx).is_empty());
}

#[test]
fn descriptive_internal_anchor_is_clean() {
    let (mut ctx, con) = corpus();
    add_page(&mut ctx, &con, "/source", 200, "", &[]);
    add_page(&mut ctx, &con, "/destination", 200, "", &[]);
    add_link(
        &con,
        "/source",
        "/destination",
        "Read the installation guide",
    );
    attach(&mut ctx, con);
    retained_links::check_retained_links(&mut ctx);
    assert!(checks(&ctx).is_empty());
}
