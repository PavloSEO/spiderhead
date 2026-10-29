//! Network-free tests for URL variant consolidation (`spiderhead_checks::mirrors`).

use std::cell::RefCell;
use std::collections::HashMap;

use spiderhead_checks::mirrors::{
    analyze, check_mirrors_with, fetch_chain, norm_final, Chain, Hop, HopClient, HopResponse,
    MirrorReport, WwwDns, MAX_HOPS,
};

fn www_ok() -> WwwDns {
    WwwDns {
        resolvable: true,
        a: vec!["1.2.3.4".to_owned()],
        cname: Vec::new(),
        source: "doh".to_owned(),
    }
}

fn www_missing() -> WwwDns {
    WwwDns {
        resolvable: false,
        a: Vec::new(),
        cname: Vec::new(),
        source: "doh".to_owned(),
    }
}

fn hop(url: &str, location: &str) -> Hop {
    Hop {
        url: url.to_owned(),
        status: 301,
        location: location.to_owned(),
    }
}

/// A reachable chain row, as the reference `_r` helper builds it.
fn row(
    variant: &str,
    group: &str,
    url: &str,
    status: Option<u16>,
    hops: Vec<Hop>,
    final_url: Option<&str>,
) -> Chain {
    let redirects = Some(hops.len());
    Chain {
        variant: variant.to_owned(),
        group: group.to_owned(),
        url: url.to_owned(),
        reachable: true,
        error: None,
        final_url: Some(final_url.unwrap_or(url).to_owned()),
        hops,
        status,
        redirects,
        loop_suspect: None,
    }
}

fn ok_row(variant: &str, group: &str, url: &str) -> Chain {
    row(variant, group, url, Some(200), Vec::new(), None)
}

fn redirect_row(variant: &str, group: &str, url: &str, to: &str) -> Chain {
    row(variant, group, url, Some(200), vec![hop(url, to)], Some(to))
}

fn healthy_results() -> Vec<Chain> {
    vec![
        ok_row("https://example.com/", "origin", "https://example.com/"),
        redirect_row(
            "http://example.com/",
            "origin",
            "http://example.com/",
            "https://example.com/",
        ),
        redirect_row(
            "https://www.example.com/",
            "origin",
            "https://www.example.com/",
            "https://example.com/",
        ),
        redirect_row(
            "http://www.example.com/",
            "origin",
            "http://www.example.com/",
            "https://example.com/",
        ),
        redirect_row(
            "/index.php",
            "index",
            "https://example.com/index.php",
            "https://example.com/",
        ),
    ]
}

fn run(url: &str, results: &[Chain]) -> MirrorReport {
    analyze(url, results, &www_ok()).unwrap_or_else(|error| panic!("analyze failed: {error}"))
}

#[test]
fn healthy_site_is_consolidated() {
    let out = run("https://example.com/", &healthy_results());
    assert!(out.consolidated);
    assert_eq!(
        out.canonical_origin.as_deref(),
        Some("https://example.com/")
    );
    assert!(out.duplicates_200.is_empty());
    assert!(out.downgrade_redirects.is_empty());
    assert_eq!(out.findings, ["all checked variants consolidate correctly"]);
}

#[test]
fn live_www_duplicate_is_detected() {
    let mut rows = healthy_results();
    rows[2] = ok_row(
        "https://www.example.com/",
        "origin",
        "https://www.example.com/",
    );
    let out = run("https://example.com/", &rows);
    assert!(!out.consolidated);
    assert!(out
        .duplicates_200
        .contains(&"https://www.example.com/".to_owned()));
}

#[test]
fn index_php_200_is_a_duplicate() {
    let mut rows = healthy_results();
    rows[4] = ok_row("/index.php", "index", "https://example.com/index.php");
    let out = run("https://example.com/", &rows);
    assert!(out.duplicates_200.contains(&"/index.php".to_owned()));
}

#[test]
fn uppercase_200_is_a_duplicate() {
    let mut rows = healthy_results();
    rows.push(ok_row("/SERVICES", "case", "https://example.com/SERVICES"));
    let out = run("https://example.com/services/", &rows);
    assert!(out.duplicates_200.contains(&"/SERVICES".to_owned()));
}

#[test]
fn slash_pair_both_200_is_a_duplicate() {
    let mut rows = healthy_results();
    rows.push(ok_row("/services", "path", "https://example.com/services"));
    rows.push(ok_row(
        "/services/",
        "path",
        "https://example.com/services/",
    ));
    let out = run("https://example.com/services/", &rows);
    assert!(out
        .findings
        .iter()
        .any(|f| f.contains("trailing-slash duplicate")));
}

#[test]
fn slash_pair_with_a_redirect_is_fine() {
    let mut rows = healthy_results();
    rows.push(redirect_row(
        "/services",
        "path",
        "https://example.com/services",
        "https://example.com/services/",
    ));
    rows.push(ok_row(
        "/services/",
        "path",
        "https://example.com/services/",
    ));
    let out = run("https://example.com/services/", &rows);
    assert!(!out
        .findings
        .iter()
        .any(|f| f.contains("trailing-slash duplicate")));
}

#[test]
fn downgrade_and_long_chain_are_reported() {
    let mut rows = healthy_results();
    rows[4] = row(
        "/index.php",
        "index",
        "https://example.com/index.php",
        Some(200),
        vec![
            hop("https://example.com/index.php", "http://example.com/"),
            hop("http://example.com/", "https://example.com/"),
        ],
        Some("https://example.com/"),
    );
    let out = run("https://example.com/", &rows);
    assert!(!out.downgrade_redirects.is_empty());
    assert!(!out.long_chains.is_empty());
    assert!(out.findings.iter().any(|f| f.contains("downgrade")));
}

#[test]
fn unresolvable_www_is_its_own_diagnosis() {
    let mut rows = healthy_results();
    rows.truncate(2);
    rows.push(Chain {
        variant: "https://www.example.com/".to_owned(),
        group: "origin".to_owned(),
        url: "https://www.example.com/".to_owned(),
        reachable: false,
        error: Some("dns".to_owned()),
        hops: Vec::new(),
        status: None,
        final_url: None,
        redirects: None,
        loop_suspect: None,
    });
    let out = analyze("https://example.com/", &rows, &www_missing())
        .unwrap_or_else(|error| panic!("analyze failed: {error}"));
    assert!(out.findings.iter().any(|f| f.contains("does not resolve")));
    assert!(out
        .unreachable
        .iter()
        .any(|d| d.variant == "https://www.example.com/"));
}

#[test]
fn www_primary_convergence_has_no_duplicate() {
    let canonical = "https://www.example.com/";
    let rows = vec![
        redirect_row(
            "https://example.com/",
            "origin",
            "https://example.com/",
            canonical,
        ),
        redirect_row(
            "http://example.com/",
            "origin",
            "http://example.com/",
            canonical,
        ),
        ok_row(canonical, "origin", canonical),
        redirect_row(
            "http://www.example.com/",
            "origin",
            "http://www.example.com/",
            canonical,
        ),
    ];
    let out = run(canonical, &rows);
    assert!(out.consolidated);
    assert_eq!(out.canonical_origin.as_deref(), Some(canonical));
    assert!(out.duplicates_200.is_empty());
}

#[test]
fn bare_primary_convergence_has_no_duplicate() {
    let out = run("https://example.com/", &healthy_results());
    assert!(out.consolidated);
    assert_eq!(
        out.canonical_origin.as_deref(),
        Some("https://example.com/")
    );
    assert!(out.duplicates_200.is_empty());
}

#[test]
fn two_independent_direct_200_origins_stay_duplicates() {
    let rows = vec![
        row(
            "https://example.com/",
            "origin",
            "https://example.com/",
            Some(200),
            vec![hop("https://example.com/", "http://example.com/")],
            Some("http://example.com/"),
        ),
        ok_row("http://example.com/", "origin", "http://example.com/"),
        ok_row(
            "https://www.example.com/",
            "origin",
            "https://www.example.com/",
        ),
        row(
            "http://www.example.com/",
            "origin",
            "http://www.example.com/",
            Some(200),
            vec![hop("http://www.example.com/", "http://example.com/")],
            Some("http://example.com/"),
        ),
    ];
    let out = run("https://example.com/", &rows);
    assert!(!out.consolidated);
    assert!(out
        .duplicates_200
        .contains(&"http://example.com/".to_owned()));
    assert!(out
        .duplicates_200
        .contains(&"https://www.example.com/".to_owned()));
}

#[test]
fn loop_suspect_is_reported() {
    let mut rows = healthy_results();
    rows[0] = Chain {
        variant: "https://example.com/".to_owned(),
        group: "origin".to_owned(),
        url: "https://example.com/".to_owned(),
        reachable: true,
        error: None,
        hops: vec![hop("https://example.com/", "https://example.com/x"); MAX_HOPS],
        status: None,
        final_url: Some("https://example.com/x".to_owned()),
        redirects: Some(MAX_HOPS),
        loop_suspect: Some(true),
    };
    let out = run("https://example.com/", &rows);
    assert!(out.findings.iter().any(|f| f.contains("loop")));
}

#[test]
fn final_url_key_folds_host_and_trailing_slash() {
    let cases = [
        ("https://www.Example.com/", "https://www.example.com/"),
        ("HTTP://EXAMPLE.com:80/A/", "http://example.com:80/A"),
        ("example.com/a/b/", "example.com/a/b"),
    ];
    for (input, expected) in cases {
        let got = norm_final(Some(input)).unwrap_or(None);
        assert_eq!(got.as_deref(), Some(expected), "{input}");
    }
    assert_eq!(norm_final(None).unwrap_or(Some("x".to_owned())), None);
}

/// A scripted client: each URL maps to a response or a network error.
struct Scripted {
    routes: HashMap<String, Result<HopResponse, String>>,
    seen: RefCell<Vec<String>>,
}

impl HopClient for Scripted {
    fn get(&self, url: &str) -> Result<HopResponse, String> {
        self.seen.borrow_mut().push(url.to_owned());
        match self.routes.get(url) {
            Some(answer) => answer.clone(),
            None => Err("no route".to_owned()),
        }
    }
}

fn response(status: u16, location: Option<&str>) -> Result<HopResponse, String> {
    Ok(HopResponse {
        status,
        location: location.map(str::to_owned),
    })
}

// The walk resolves relative Location headers against the address that returned them.
#[test]
fn fetch_chain_resolves_relative_location_and_records_hops() {
    let client = Scripted {
        routes: HashMap::from([
            (
                "https://example.com/a".to_owned(),
                response(301, Some("/b")),
            ),
            ("https://example.com/b".to_owned(), response(200, None)),
        ]),
        seen: RefCell::new(Vec::new()),
    };
    let chain = fetch_chain(&client, "/a", "path", "https://example.com/a")
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(chain.reachable);
    assert_eq!(chain.status, Some(200));
    assert_eq!(chain.final_url.as_deref(), Some("https://example.com/b"));
    assert_eq!(chain.redirects, Some(1));
    assert_eq!(chain.hops[0].location, "https://example.com/b");
}

// A network failure is result data, with its text cut to 200 code points.
#[test]
fn fetch_chain_turns_a_network_failure_into_result_data() {
    let long_error = "e".repeat(300);
    let client = Scripted {
        routes: HashMap::from([("https://example.com/".to_owned(), Err(long_error))]),
        seen: RefCell::new(Vec::new()),
    };
    let chain = fetch_chain(
        &client,
        "https://example.com/",
        "origin",
        "https://example.com/",
    )
    .unwrap_or_else(|error| panic!("{error}"));
    assert!(!chain.reachable);
    assert_eq!(chain.error.map(|text| text.chars().count()), Some(200));
}

// A redirect loop stops after MAX_HOPS and is flagged, not followed forever.
#[test]
fn fetch_chain_stops_at_max_hops_on_a_loop() {
    let client = Scripted {
        routes: HashMap::from([(
            "https://example.com/".to_owned(),
            response(302, Some("https://example.com/")),
        )]),
        seen: RefCell::new(Vec::new()),
    };
    let chain = fetch_chain(
        &client,
        "https://example.com/",
        "origin",
        "https://example.com/",
    )
    .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(chain.loop_suspect, Some(true));
    assert_eq!(chain.redirects, Some(MAX_HOPS));
    assert_eq!(client.seen.borrow().len(), MAX_HOPS);
}

// The matrix walk fetches every variant of the target, then analyzes the chains.
#[test]
fn check_mirrors_with_walks_the_matrix_then_analyzes_it() {
    let client = Scripted {
        routes: HashMap::from([
            ("https://example.com/".to_owned(), response(200, None)),
            (
                "http://example.com/".to_owned(),
                response(301, Some("https://example.com/")),
            ),
            (
                "https://www.example.com/".to_owned(),
                response(301, Some("https://example.com/")),
            ),
            (
                "http://www.example.com/".to_owned(),
                response(301, Some("https://example.com/")),
            ),
            (
                "https://example.com/index.php".to_owned(),
                response(301, Some("https://example.com/")),
            ),
            (
                "https://example.com/index.html".to_owned(),
                response(301, Some("https://example.com/")),
            ),
        ]),
        seen: RefCell::new(Vec::new()),
    };
    let report = check_mirrors_with("https://example.com/", &www_ok(), &client)
        .unwrap_or_else(|error| panic!("{error}"))
        .unwrap_or_else(|| panic!("expected a report"));
    assert!(report.consolidated);
    assert_eq!(
        report.findings,
        ["all checked variants consolidate correctly"]
    );
    // Root URL: four origin mirrors and two index files.
    assert_eq!(report.variants.len(), 6);
}

#[test]
fn check_mirrors_with_rejects_an_empty_target_without_requests() {
    let client = Scripted {
        routes: HashMap::new(),
        seen: RefCell::new(Vec::new()),
    };
    let report = check_mirrors_with("   ", &www_ok(), &client).unwrap_or(None);
    assert!(report.is_none());
    assert!(client.seen.borrow().is_empty());
}
