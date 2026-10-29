//! Tests for the bulk site audit, covering the behaviours of the
//! the reference suite (audit and page-row sections).

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{json, Value as Json};

use super::{
    audit_site, classify, first_h1, page_row, urls_from_sitemap, AuditOptions, SiteTools, ToolCall,
    PAGE_TOOLS, SEVERITY_RULES, SITE_TOOLS,
};

type BoxedTool = Box<dyn Fn(&ToolCall) -> Result<Json, String>>;

/// A caller-supplied registry: exactly the seam `audit_site` takes.
struct FakeTools {
    tools: Vec<(&'static str, BoxedTool)>,
}

impl SiteTools for FakeTools {
    fn has(&self, tool: &str) -> bool {
        self.tools.iter().any(|(name, _)| *name == tool)
    }
    fn call(&self, tool: &str, call: &ToolCall) -> Result<Json, String> {
        match self.tools.iter().find(|(name, _)| *name == tool) {
            Some((_, run)) => run(call),
            None => Err(format!("KeyError: '{tool}'")),
        }
    }
}

fn tool<F>(name: &'static str, run: F) -> (&'static str, BoxedTool)
where
    F: Fn(&ToolCall) -> Result<Json, String> + 'static,
{
    (name, Box::new(run))
}

fn static_tool(name: &'static str, response: Json) -> (&'static str, BoxedTool) {
    tool(name, move |_| Ok(response.clone()))
}

const FIRST_SITEMAP: &str = "https://example.test/sitemap-pages.xml";
const SECOND_SITEMAP: &str = "https://example.test/sitemap-products.xml";

/// The multi-root fixtures skip every other site tool and all page tools.
fn multi_root_skip() -> Vec<String> {
    let keep = ["robots_check", "sitemap_crawl"];
    SITE_TOOLS
        .into_iter()
        .filter(|name| !keep.contains(name))
        .chain(PAGE_TOOLS)
        .map(str::to_string)
        .collect()
}

fn run_audit(url: &str, skip: &[String], tools: &dyn SiteTools) -> Json {
    audit_site(
        url,
        &AuditOptions {
            urls: &[],
            limit: 10,
            concurrency: 5,
            render: false,
            skip,
            crux_evidence: None,
        },
        tools,
    )
    .expect("audit")
}

#[test]
fn severity_rules_are_ordered_from_worst() {
    // The first matching rule wins, so critical rules must precede warnings.
    let rank = |level: &str| match level {
        "critical" => 0,
        "warning" => 1,
        _ => 2,
    };
    let levels: Vec<i32> = SEVERITY_RULES
        .iter()
        .map(|(_, level)| rank(level))
        .collect();
    let mut sorted_levels = levels.clone();
    sorted_levels.sort_unstable();
    assert_eq!(levels, sorted_levels);
}

#[test]
fn page_that_robots_see_empty_is_critical() {
    assert_eq!(
        classify(r#"raw HTML contains an empty container <div id="root">"#),
        "critical"
    );
    assert_eq!(classify("the crawler receives an empty page"), "critical");
}

#[test]
fn self_removal_from_index_is_critical() {
    assert_eq!(
        classify("regional pages are canonicalized to another host"),
        "critical"
    );
    assert_eq!(classify("regional pages use noindex"), "critical");
}

#[test]
fn ordinary_observation_is_a_notice() {
    assert_eq!(classify("brotli is not enabled"), "notice");
    assert_eq!(classify(""), "notice");
}

#[test]
fn missing_key_security_headers_are_warnings_not_notices() {
    // Missing HSTS and CSP must remain actionable warnings, not generic notices.
    assert_eq!(
        classify("missing strict-transport-security — enforce HTTPS"),
        "warning"
    );
    assert_eq!(
        classify("missing content-security-policy — restrict script sources"),
        "warning"
    );
}

#[test]
fn missing_permissions_policy_is_a_notice() {
    assert_eq!(
        classify("missing permissions-policy — camera access"),
        "notice"
    );
}

#[test]
fn urls_are_read_from_the_sitemap_records() {
    let sitemap = json!({
        "count": 3,
        "urls": [
            {"loc": "https://example.com/a", "lastmod": "2026-01-01"},
            {"loc": "https://example.com/b"},
            {"loc": "https://example.com/a"}, // duplicate
        ],
    });
    assert_eq!(
        urls_from_sitemap(&sitemap, 10),
        vec!["https://example.com/a", "https://example.com/b"]
    );
}

#[test]
fn plain_string_urls_work_too() {
    assert_eq!(
        urls_from_sitemap(&json!({"urls": ["https://example.com/x"]}), 5),
        vec!["https://example.com/x"]
    );
}

#[test]
fn limit_is_respected() {
    let sitemap = json!({"urls": (0..100).map(|i| json!({"loc": format!("https://example.com/{i}")})).collect::<Vec<_>>()});
    assert_eq!(urls_from_sitemap(&sitemap, 7).len(), 7);
}

#[test]
fn nested_shapes_are_still_walked() {
    // A response-shape change must not silently reduce the audit to zero URLs.
    let weird = json!({"sitemaps": [{"entries": [{"loc": "https://example.com/deep"}]}]});
    assert_eq!(
        urls_from_sitemap(&weird, 5),
        vec!["https://example.com/deep"]
    );
}

#[test]
fn every_declared_sitemap_is_sampled_not_just_the_first() {
    // robots.txt can declare more than one independent Sitemap: directive; a page
    // only that second sitemap lists must still reach page selection.
    let calls: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let recorded = Rc::clone(&calls);
    let tools = FakeTools {
        tools: vec![
            static_tool(
                "robots_check",
                json!({"ok": true, "sitemaps": [FIRST_SITEMAP, SECOND_SITEMAP]}),
            ),
            tool("sitemap_crawl", move |call| {
                recorded.borrow_mut().push(call.url.to_string());
                let loc = if call.url == FIRST_SITEMAP {
                    "https://example.test/page-a"
                } else {
                    "https://example.test/product-b"
                };
                Ok(json!({"ok": true, "urls": [{"loc": loc}]}))
            }),
        ],
    };
    let result = run_audit("https://example.test/", &multi_root_skip(), &tools);

    assert_eq!(
        *calls.borrow(),
        vec![FIRST_SITEMAP.to_string(), SECOND_SITEMAP.to_string()]
    );
    let selected: Vec<&str> = result["pages"]
        .as_array()
        .expect("pages")
        .iter()
        .filter_map(|page| page["url"].as_str())
        .collect();
    assert!(selected.contains(&"https://example.test/page-a"));
    assert!(selected.contains(&"https://example.test/product-b"));
}

#[test]
fn two_partial_roots_are_not_reported_clean() {
    // Two roots that both fail must not collapse into {"ok": true, "urls": []}.
    let tools = FakeTools {
        tools: vec![
            static_tool(
                "robots_check",
                json!({"ok": true, "sitemaps": [FIRST_SITEMAP, SECOND_SITEMAP]}),
            ),
            tool("sitemap_crawl", |call| {
                Ok(json!({
                    "ok": true,
                    "urls": [],
                    "errors": [{"url": call.url, "error": "synthetic child unavailable"}],
                    "truncated": true,
                }))
            }),
        ],
    };
    let result = run_audit("https://example.test/", &multi_root_skip(), &tools);
    let sitemap = &result["site"]["sitemap_crawl"];

    assert_eq!(sitemap["ok"], json!(false));
    assert_eq!(sitemap["errors"].as_array().map(Vec::len), Some(2));
    assert_eq!(sitemap["truncated"], json!(true));
    assert!(
        !result["summary"]["tools_failed"]
            .as_array()
            .expect("tools_failed")
            .is_empty(),
        "a totally failed multi-root sitemap must be reported"
    );
}

#[test]
fn one_good_root_keeps_urls_and_names_the_failed_root() {
    let tools = FakeTools {
        tools: vec![
            static_tool(
                "robots_check",
                json!({"ok": true, "sitemaps": [FIRST_SITEMAP, SECOND_SITEMAP]}),
            ),
            tool("sitemap_crawl", |call| {
                if call.url == FIRST_SITEMAP {
                    Ok(json!({"ok": true, "urls": [{"loc": "https://example.test/page-a"}]}))
                } else {
                    Ok(
                        json!({"ok": true, "urls": [], "errors": [{"url": call.url, "error": "404"}]}),
                    )
                }
            }),
        ],
    };
    let result = run_audit("https://example.test/", &multi_root_skip(), &tools);
    let sitemap = &result["site"]["sitemap_crawl"];

    assert_eq!(sitemap["ok"], json!(true));
    let locs: Vec<&str> = sitemap["urls"]
        .as_array()
        .expect("urls")
        .iter()
        .filter_map(|entry| entry["loc"].as_str())
        .collect();
    assert_eq!(locs, vec!["https://example.test/page-a"]);
    assert_eq!(
        sitemap["errors"],
        json!([{"url": SECOND_SITEMAP, "error": "404"}])
    );
    // A non-fatal partial result still has to surface in the document, not only in
    // the raw site payload -- findings feed the Markdown/DOCX/XLSX report writers.
    let named: Vec<&Json> = result["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .filter(|finding| {
            finding["text"]
                .as_str()
                .is_some_and(|text| text.contains(SECOND_SITEMAP))
        })
        .collect();
    assert!(!named.is_empty());
    // classify() matches the severity rules by plain substring, so a finding whose
    // wording drifts by one word silently becomes a notice.
    let severities: Vec<&str> = named
        .iter()
        .filter_map(|finding| finding["severity"].as_str())
        .collect();
    assert_eq!(severities, vec!["critical"]);
}

#[test]
fn all_successful_multi_root_shape_is_unchanged() {
    // The all-successful aggregate keeps its original shape: no `errors` or
    // `truncated` keys appear when nothing went wrong.
    let tools = FakeTools {
        tools: vec![
            static_tool(
                "robots_check",
                json!({"ok": true, "sitemaps": [FIRST_SITEMAP, SECOND_SITEMAP]}),
            ),
            tool("sitemap_crawl", |call| {
                let loc = if call.url == FIRST_SITEMAP {
                    "https://example.test/page-a"
                } else {
                    "https://example.test/product-b"
                };
                Ok(json!({"ok": true, "urls": [{"loc": loc}]}))
            }),
        ],
    };
    let result = run_audit("https://example.test/", &multi_root_skip(), &tools);

    assert_eq!(
        result["site"]["sitemap_crawl"],
        json!({
            "ok": true,
            "urls": [
                {"loc": "https://example.test/page-a"},
                {"loc": "https://example.test/product-b"},
            ],
            "sources": [FIRST_SITEMAP, SECOND_SITEMAP],
        })
    );
}

const PARSE_RESULT: &str = r#"{
    "count": 1,
    "results": [{
        "url": "https://example.com/p",
        "status_code": 200,
        "title": "Page title",
        "meta_description": "Page description",
        "canonical": "https://example.com/p",
        "word_count": 640,
        "headings": {"h1": ["Primary H1"]}
    }]
}"#;

#[test]
fn page_row_reads_the_batch_shape_of_parse() {
    // Parse returns a batch shape; reading its top level would empty report columns.
    let row = page_row(
        "https://example.com/p",
        &[(
            "parse".to_string(),
            serde_json::from_str(PARSE_RESULT).expect("fixture"),
        )],
    );
    assert_eq!(row["status"], json!(200));
    assert_eq!(row["words"], json!(640));
    assert_eq!(row["title_length"], json!(10));
    assert_eq!(row["description_length"], json!(16));
    assert_eq!(row["h1"], json!("Primary H1"));
}

#[test]
fn h1_is_found_in_any_of_the_known_shapes() {
    assert_eq!(first_h1(&json!({"h1": "direct value"})), "direct value");
    assert_eq!(
        first_h1(&json!({"headings": {"h1": ["from mapping"]}})),
        "from mapping"
    );
    assert_eq!(
        first_h1(&json!({"headings": [{"level": "h1", "text": "from list"}]})),
        "from list"
    );
    assert_eq!(first_h1(&json!({})), "");
}

#[test]
fn failed_tool_becomes_a_page_issue_not_silence() {
    let row = page_row(
        "https://example.com/p",
        &[(
            "schema_check".to_string(),
            json!({"ok": false, "error": "request timed out"}),
        )],
    );
    assert!(row["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .any(|issue| issue
            .as_str()
            .is_some_and(|text| text.contains("request timed out"))));
}

#[test]
fn page_row_reads_schema_types_from_current_entity_shape() {
    let row = page_row(
        "https://example.com/p",
        &[(
            "schema_check".to_string(),
            json!({
                "ok": true,
                "entities": [
                    {"path": "$[0]", "types": ["WebPage"], "errors": [], "warnings": []},
                    {"path": "$[1]", "types": ["Organization"], "errors": [], "warnings": []},
                    {"path": "$[2]", "types": ["WebPage"], "errors": [], "warnings": []},
                ],
            }),
        )],
    );
    assert_eq!(row["schema_types"], json!("Organization, WebPage"));
}

#[test]
fn bad_input_is_rejected_before_any_network_call() {
    // limit="many" is unrepresentable in the typed API and is not ported.
    let tools = FakeTools { tools: Vec::new() };
    let empty = run_audit("", &[], &tools);
    assert_eq!(empty["ok"], json!(false));
    let garbage = run_audit("not a URL", &[], &tools);
    assert_eq!(garbage["ok"], json!(false));
    assert!(garbage["error"]
        .as_str()
        .is_some_and(|text| text.contains("does not look like a domain")));
}

const BLOCKED_PARSE: &str = r#"{
    "count": 1,
    "results": [{
        "url": "https://example.test/blocked",
        "status_code": 403,
        "ok": false,
        "title": "Just a moment...",
        "blocked_by_waf": "cf-mitigated: challenge",
        "word_count": 30
    }]
}"#;

const ERROR_PARSE: &str = r#"{
    "count": 1,
    "results": [{
        "url": "https://example.test/gone",
        "status_code": 410,
        "ok": false,
        "title": "410 Gone"
    }]
}"#;

fn parse_fixture(body: &str) -> (String, Json) {
    (
        "parse".to_string(),
        serde_json::from_str(body).expect("fixture"),
    )
}

#[test]
fn blocked_page_row_is_not_measured() {
    let row = page_row(
        "https://example.test/blocked",
        &[parse_fixture(BLOCKED_PARSE)],
    );
    assert_eq!(row["measured"], json!(false));
    assert_eq!(
        row["not_measured_reason"],
        json!("blocked_by_waf (cf-mitigated: challenge)")
    );
    assert_eq!(row["status"], json!(403));
    assert_eq!(row["issues"], json!([]));
}

#[test]
fn plain_error_page_row_is_not_measured() {
    let row = page_row("https://example.test/gone", &[parse_fixture(ERROR_PARSE)]);
    assert_eq!(row["measured"], json!(false));
    assert_eq!(row["not_measured_reason"], json!("HTTP 410"));
    assert_eq!(row["issues"], json!([]));
}

#[test]
fn served_page_row_keeps_its_issues() {
    let row = page_row(
        "https://example.test/served",
        &[
            parse_fixture(
                r#"{"count": 1, "results": [{
                    "url": "https://example.test/served",
                    "status_code": 200,
                    "ok": true,
                    "title": "Shared title",
                    "meta_description": "served page",
                    "word_count": 640
                }]}"#,
            ),
            (
                "schema_check".to_string(),
                json!({"ok": true, "findings": ["The page contains no JSON-LD blocks"]}),
            ),
        ],
    );
    assert_eq!(row["measured"], json!(true));
    assert!(row.get("not_measured_reason").is_none());
    assert!(row["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .any(|issue| issue
            .as_str()
            .is_some_and(|text| text.contains("no JSON-LD"))));
}

#[test]
fn blocked_and_served_audit() {
    // An audit over one blocked and one served page: findings stay empty because
    // nothing the block body produced may reach the totals.
    let tools = FakeTools {
        tools: vec![
            tool("parse", |call| {
                if call.url == "https://example.test/blocked" {
                    Ok(serde_json::from_str(BLOCKED_PARSE).expect("fixture"))
                } else {
                    Ok(json!({"count": 1, "results": [{
                        "url": call.url,
                        "status_code": 200,
                        "ok": true,
                        "title": "Shared title",
                        "meta_description": "served page",
                        "word_count": 640,
                    }]}))
                }
            }),
            tool("schema_check", |call| {
                if call.url == "https://example.test/blocked" {
                    Ok(json!({"ok": true, "findings": [
                        "The page returned HTTP 403: what follows describes the error page, not the requested URL",
                        "The page contains no JSON-LD blocks",
                    ]}))
                } else {
                    Ok(json!({"ok": true, "findings": []}))
                }
            }),
            static_tool("social_meta_check", json!({"ok": true, "missing": []})),
        ],
    };
    let skip: Vec<String> = SITE_TOOLS.iter().map(|name| name.to_string()).collect();
    let urls = [
        "https://example.test/blocked".to_string(),
        "https://example.test/served".to_string(),
    ];
    let result = audit_site(
        "https://example.test/",
        &AuditOptions {
            urls: &urls,
            limit: 10,
            concurrency: 5,
            render: false,
            skip: &skip,
            crux_evidence: None,
        },
        &tools,
    )
    .expect("audit");

    assert_eq!(result["summary"]["pages_checked"], json!(2));
    assert_eq!(
        result["summary"]["pages_not_measured"],
        json!([{
            "url": "https://example.test/blocked",
            "status": 403,
            "reason": "blocked_by_waf (cf-mitigated: challenge)",
        }])
    );
    assert_eq!(result["findings"], json!([]));
    assert_eq!(
        result["summary"]["findings_by_severity"],
        json!({"critical": 0, "warning": 0, "notice": 0})
    );
    let blocked = &result["pages"][0];
    assert_eq!(blocked["measured"], json!(false));
    assert_eq!(blocked["issues"], json!([]));
    let served = &result["pages"][1];
    assert_eq!(served["measured"], json!(true));
    // The duplicate title warning counts measured pages only; the block body's
    // title must not join it.
    assert_eq!(served["title"], json!("Shared title"));
}
