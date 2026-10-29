//! Ports of the parser-level tests in the reference suite: the declared amphtml target
//! is resolved, while empty or inert declarations are not targets. The AMP_* rule tests run the
//! audit rule engine and are listed in the slice report instead.

use spiderhead_parse::dom::parse_document;
use spiderhead_parse::parse_html;
use spiderhead_parse::parser::meta::extract_amphtml;
use spiderhead_parse::types::ParseOptions;

const BASE: &str = "https://example.test";
const DESKTOP: &str = "https://example.test/article";
const AMP: &str = "https://example.test/article/amp";

#[test]
fn extract_amphtml_resolves_the_first_declared_target() {
    let html = r#"<html><head><link rel="amphtml" href="/article/amp"><link rel="amphtml" href="/second"></head><body></body></html>"#;
    let page = parse_html(html, DESKTOP, &ParseOptions::default()).unwrap();
    assert_eq!(page.amphtml, AMP);
}

#[test]
fn extract_amphtml_ignores_empty_href_and_template_content() {
    let empty = r#"<html><head><link rel="amphtml" href=""></head></html>"#;
    let inert = r#"<html><head><template><link rel="amphtml" href="/x"></template></head></html>"#;
    assert_eq!(
        parse_html(empty, DESKTOP, &ParseOptions::default())
            .unwrap()
            .amphtml,
        ""
    );
    assert_eq!(
        parse_html(inert, DESKTOP, &ParseOptions::default())
            .unwrap()
            .amphtml,
        ""
    );
    let soup = parse_document(r#"<link rel="amphtml" href="/a">"#);
    assert_eq!(
        extract_amphtml(&soup, DESKTOP).unwrap(),
        format!("{BASE}/a")
    );
}
