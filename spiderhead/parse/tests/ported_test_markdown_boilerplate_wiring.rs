//! Ports of the parse-level assertions in the reference suite: the
//! fetch helper, markdown extraction, and boilerplate hashing and grouping. Handler, CLI,
//! MCP and citability assertions are not here: those layers are not ported yet.

use spiderhead_parse::boilerplate_report::{
    boilerplate_consistency_report, boilerplate_hash, BoilerplatePage,
};
use spiderhead_parse::markdown_extract::extract_markdown;
use spiderhead_parse::models::ParseResult;
use spiderhead_parse::parser::{fetch_html, parse_url, FetchOutcome, FetchResponse, HtmlFetcher};
use spiderhead_parse::types::{ContentAreaConfig, ParseOptions};

const NAV_HTML: &str = "<html><body>\
<nav>Home Products Services About Contact Blog Careers Support Login Sign up now</nav>\
<main id=\"content\"><h1>Caching guide</h1>\
<p>Caching stores a copy of a server response and serves it again without \
querying the database, cutting response time in half according to a 2024 \
benchmark.</p></main>\
<footer>Copyright policy terms privacy sitemap careers investors press newsletter</footer>\
</body></html>";

/// Stands in for the HTTP client: every GET returns the same canned result.
struct FakeFetcher(Result<FetchResponse, String>);

impl HtmlFetcher for FakeFetcher {
    fn get(
        &self,
        _url: &str,
        _headers: &[(&str, &str)],
        _timeout: f64,
        _max_redirects: u32,
    ) -> Result<FetchResponse, String> {
        self.0.clone()
    }
}

fn response(html: &str, status_code: u16) -> FetchResponse {
    FetchResponse {
        final_url: "https://example.com/".to_owned(),
        status_code,
        text: html.to_owned(),
        headers: Vec::new(),
    }
}

fn page(url: &str, html: &str) -> BoilerplatePage {
    BoilerplatePage {
        url: url.to_owned(),
        html: Some(html.to_owned()),
        hash: None,
    }
}

#[test]
fn fetch_html_returns_raw_body_and_status() {
    let fetcher = FakeFetcher(Ok(response(NAV_HTML, 200)));
    match fetch_html(&fetcher, "https://example.com/", None) {
        FetchOutcome::Completed {
            ok,
            status_code,
            html,
            ..
        } => {
            assert!(ok);
            assert_eq!(status_code, 200);
            assert_eq!(html, NAV_HTML);
        }
        other => panic!("expected a completed fetch, got {other:?}"),
    }
}

#[test]
fn fetch_html_tolerates_non_2xx() {
    let fetcher = FakeFetcher(Ok(response("<html><body>gone</body></html>", 404)));
    match fetch_html(&fetcher, "https://example.com/missing", None) {
        FetchOutcome::Completed {
            ok, status_code, ..
        } => {
            assert!(ok);
            assert_eq!(status_code, 404);
        }
        other => panic!("expected a completed fetch, got {other:?}"),
    }
}

#[test]
fn fetch_html_reports_transport_failure() {
    let fetcher = FakeFetcher(Err("connection refused".to_owned()));
    match fetch_html(&fetcher, "https://example.com/", None) {
        FetchOutcome::Failed { ok, error, .. } => {
            assert!(!ok);
            assert!(error.contains("connection refused"));
        }
        other => panic!("expected a transport failure, got {other:?}"),
    }
}

#[test]
fn parse_url_ok_still_requires_2xx() {
    let fetcher = FakeFetcher(Ok(response("<html><body>gone</body></html>", 404)));
    match parse_url(
        &fetcher,
        "https://example.com/missing",
        &ParseOptions::default(),
        None,
    ) {
        ParseResult::Fetched(fetched) => {
            assert!(!fetched.ok);
            assert_eq!(fetched.status_code, 404);
            assert!(fetched.page.text.contains("gone"));
        }
        ParseResult::Failed(failed) => panic!("expected a parsed page, got {failed:?}"),
    }
}

#[test]
fn markdown_extract_from_html_is_pure_and_scoped() {
    let out = extract_markdown(NAV_HTML, None).unwrap();
    assert!(out.content_markdown.contains("Caching guide"));
    assert!(
        !out.content_markdown.contains("Sign up now"),
        "nav excluded"
    );
    assert!(
        !out.content_markdown.contains("newsletter"),
        "footer excluded"
    );
    assert!(
        out.full_markdown.contains("Sign up now"),
        "full rendering keeps nav"
    );
    assert_eq!(out.content_area_strategy, "auto_main");
}

#[test]
fn markdown_extract_from_url_fetches_then_renders() {
    let fetcher = FakeFetcher(Ok(response(NAV_HTML, 200)));
    let FetchOutcome::Completed { html, .. } = fetch_html(&fetcher, "https://example.com/", None)
    else {
        panic!("expected a completed fetch");
    };
    let out = extract_markdown(&html, None).unwrap();
    assert!(out.content_markdown.contains("Caching guide"));
    assert!(!out.content_markdown.contains("Sign up now"));
}

// (markdown half only; the citability half needs the checks crate and is deferred)
#[test]
fn template_content_is_absent_from_markdown() {
    let html = "<html><body><template><main><h1>Unreleased draft</h1>\
<p>Draft evidence that must not reach a reader.</p></main></template>\
<main><h1>Published guide</h1><p>Visible evidence for readers.</p></main>\
</body></html>";
    let out = extract_markdown(html, None).unwrap();
    assert!(out.content_markdown.contains("Published guide"));
    assert!(!out.content_markdown.contains("Unreleased draft"));
}

#[test]
fn markdown_extract_honors_content_area_config() {
    // Scoped to the body rather than the auto-detected <main>, with exclusions disabled, so
    // the nav survives.
    let config = ContentAreaConfig {
        root_selector: Some("body".to_owned()),
        exclude_tags: Some(Vec::new()),
        ..ContentAreaConfig::default()
    };
    let out = extract_markdown(NAV_HTML, Some(&config)).unwrap();
    assert!(out.content_markdown.contains("Sign up now"));
}

#[test]
fn boilerplate_report_flags_the_minority_template() {
    let common_nav = "<nav><a href='/a'>A</a><a href='/b'>B</a></nav>";
    let truncated_nav = "<nav><a href='/a'>A</a></nav>";
    let pages = vec![
        page(
            "https://example.com/1",
            &format!("<html><body>{common_nav}</body></html>"),
        ),
        page(
            "https://example.com/2",
            &format!("<html><body>{common_nav}</body></html>"),
        ),
        page(
            "https://example.com/3",
            &format!("<html><body>{common_nav}</body></html>"),
        ),
        page(
            "https://example.com/legacy",
            &format!("<html><body>{truncated_nav}</body></html>"),
        ),
    ];
    let out = boilerplate_consistency_report(&pages).unwrap();
    assert!(out.ok);
    assert_eq!(out.count, 4);
    let minority = out.minority_groups.expect("minority groups");
    assert_eq!(minority.len(), 1);
    assert_eq!(
        minority[0].urls,
        vec!["https://example.com/legacy".to_owned()]
    );
}

const TEMPLATE_A: &str =
    "<html><body><nav><a href='/a'>A</a><a href='/b'>B</a></nav></body></html>";
const TEMPLATE_B: &str = "<html><body><nav><a href='/x'>X</a></nav></body></html>";

#[test]
fn boilerplate_handoff_accepts_raw_html_from_two_templates() {
    let pages = vec![
        page("https://example.com/1", TEMPLATE_A),
        page("https://example.com/2", TEMPLATE_B),
    ];
    let out = boilerplate_consistency_report(&pages).unwrap();
    assert!(out.ok);
    let hashes: std::collections::BTreeSet<String> = pages
        .iter()
        .map(|p| boilerplate_hash(p.html.as_deref().unwrap()))
        .collect();
    assert_eq!(
        hashes.len(),
        2,
        "two distinct templates must hash differently"
    );
    assert_eq!(out.count, 2);
    assert_eq!(out.minority_groups.expect("minority groups").len(), 1);
}

#[test]
fn boilerplate_handoff_accepts_precomputed_hash_key() {
    let hash_a = boilerplate_hash(TEMPLATE_A);
    let hash_b = boilerplate_hash(TEMPLATE_B);
    assert_ne!(hash_a, hash_b);

    let via_html = boilerplate_consistency_report(&[
        page("https://example.com/1", TEMPLATE_A),
        page("https://example.com/2", TEMPLATE_B),
    ])
    .unwrap();
    let via_hash = boilerplate_consistency_report(&[
        BoilerplatePage {
            url: "https://example.com/1".to_owned(),
            html: None,
            hash: Some(hash_a.clone()),
        },
        BoilerplatePage {
            url: "https://example.com/2".to_owned(),
            html: None,
            hash: Some(hash_b.clone()),
        },
    ])
    .unwrap();
    let dominant = via_hash.dominant_hash.clone().expect("dominant hash");
    assert!(dominant == hash_a || dominant == hash_b);
    assert_eq!(via_html.count, via_hash.count);
    assert_eq!(via_html.minority_groups.unwrap().len(), 1);
    assert_eq!(via_hash.minority_groups.unwrap().len(), 1);
}

// A page that carries neither `html` nor `hash` (the reference `boilerplate_hash` key is not a
// field of the Rust input) falls into the no-boilerplate bucket, so the two pages collapse.
#[test]
fn boilerplate_handoff_rejects_the_misleading_boilerplate_hash_key() {
    let pages = vec![
        BoilerplatePage {
            url: "https://example.com/1".to_owned(),
            html: None,
            hash: None,
        },
        BoilerplatePage {
            url: "https://example.com/2".to_owned(),
            html: None,
            hash: None,
        },
    ];
    let out = boilerplate_consistency_report(&pages).unwrap();
    assert!(out.ok);
    assert_eq!(out.count, 2);
    assert_eq!(out.groups.len(), 1);
    assert_eq!(out.groups[0].count, 2);
}

#[test]
fn full_markdown_is_not_the_boilerplate_report_handoff() {
    let full_a = extract_markdown(TEMPLATE_A, None).unwrap().full_markdown;
    let full_b = extract_markdown(TEMPLATE_B, None).unwrap().full_markdown;
    let raw_hash_a = boilerplate_hash(TEMPLATE_A);
    let markdown_hash_a = boilerplate_hash(&full_a);
    assert_ne!(markdown_hash_a, raw_hash_a);
    assert_ne!(full_a, full_b, "templates still differ as text");
}

#[test]
fn boilerplate_report_separates_the_two_templates_from_raw_html() {
    const WITH_FOOTER: &str =
        "<html><body><header><nav><a href='/'>Home</a><a href='/a'>A</a></nav></header>\
<main><h1>Alpha</h1><p>one</p></main>\
<footer><p>(c) Example</p></footer></body></html>";
    const WITHOUT_FOOTER: &str =
        "<html><body><header><nav><a href='/'>Home</a><a href='/a'>A</a></nav></header>\
<main><h1>Beta</h1><p>two</p></main></body></html>";
    let report =
        boilerplate_consistency_report(&[page("/a", WITH_FOOTER), page("/b", WITHOUT_FOOTER)])
            .unwrap();
    assert_eq!(report.groups.len(), 2);
}

#[test]
fn the_same_pages_as_markdown_collapse_into_one_false_group() {
    const WITH_FOOTER: &str =
        "<html><body><header><nav><a href='/'>Home</a><a href='/a'>A</a></nav></header>\
<main><h1>Alpha</h1><p>one</p></main>\
<footer><p>(c) Example</p></footer></body></html>";
    const WITHOUT_FOOTER: &str =
        "<html><body><header><nav><a href='/'>Home</a><a href='/a'>A</a></nav></header>\
<main><h1>Beta</h1><p>two</p></main></body></html>";
    let as_markdown = [
        page(
            "/a",
            &extract_markdown(WITH_FOOTER, None).unwrap().full_markdown,
        ),
        page(
            "/b",
            &extract_markdown(WITHOUT_FOOTER, None)
                .unwrap()
                .full_markdown,
        ),
    ];
    let report = boilerplate_consistency_report(&as_markdown).unwrap();
    assert_eq!(report.groups.len(), 1, "the false clean");
}
