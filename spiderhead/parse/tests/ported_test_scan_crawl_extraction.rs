//! The shared link application, form edges and throttle cases need the crawl crate and are
//! deferred; see the port report.

use spiderhead_parse::models::LinkObservation;
use spiderhead_parse::parse_html;
use spiderhead_parse::types::ParseOptions;

#[test]
fn default_parser_output_is_unchanged_when_observation_caps_are_omitted() {
    let html = r#"
    <a href="/one">One</a><a href="https://outside.example/two">Two</a>
    <form method="post"><input type="password"></form>
    "#;
    let baseline =
        parse_html(html, "https://example.test/", &ParseOptions::default()).expect("parse_html");
    let explicit_none = parse_html(
        html,
        "https://example.test/",
        &ParseOptions {
            max_link_observations: None,
            max_form_observations: None,
            ..ParseOptions::default()
        },
    )
    .expect("parse_html");
    assert_eq!(explicit_none, baseline);
    assert!(baseline.link_observation.is_none());
    assert!(baseline.forms_omitted.is_none());
}

#[test]
fn capped_parser_keeps_prefix_but_reports_full_link_counts() {
    // The reference monkeypatch on classify_link (call-count) is reference mechanics and is not ported.
    let parsed = parse_html(
        r#"
        <a href="/one">One</a><a href="https://outside.example/two">Two</a>
        <a href="/three">Three</a>
        "#,
        "https://example.test/",
        &ParseOptions {
            classify_links: true,
            max_link_observations: Some(1),
            ..ParseOptions::default()
        },
    )
    .expect("parse_html");

    let hrefs: Vec<&str> = parsed.links.iter().map(|ln| ln.href.as_str()).collect();
    assert_eq!(hrefs, vec!["https://example.test/one"]);
    assert_eq!(
        parsed.link_observation,
        Some(LinkObservation {
            stored: 1,
            total: 3,
            external_total: 1,
            omitted: 2,
        })
    );
}
