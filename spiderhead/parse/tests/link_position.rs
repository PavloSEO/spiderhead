//! Ports of the reference suite: link position classification (nav, header, content,
//! sidebar, footer), first-match-wins rule order, malformed selectors and the `classify_links`
//! parse option.

use serde_json::json;
use spiderhead_parse::content_area::find_content_root;
use spiderhead_parse::dom::{parse_document, ElementRef, Selector, Soup};
use spiderhead_parse::link_position::{classify_link, rules_from_config};
use spiderhead_parse::parse_html;
use spiderhead_parse::types::{ContentAreaConfig, ParseOptions};

const PAGE: &str = r#"<html><body>
<nav><a href="/nav1">Nav</a></nav>
<header><a href="/hdr1">Header</a></header>
<main>
  <p>Real body copy <a href="/content1">Content</a></p>
  <aside class="sidebar"><a href="/side1">Sidebar</a></aside>
</main>
<footer><a href="/foot1">Footer</a></footer>
</body></html>"#;

/// The `<a>` whose `href` attribute equals `href`.
fn anchor<'a>(soup: &'a Soup, href: &str) -> ElementRef<'a> {
    let anchors = Selector::parse("a").unwrap();
    soup.select(&anchors)
        .find(|el| el.value().attr("href") == Some(href))
        .unwrap()
}

#[test]
fn classify_link_assigns_nav_content_and_footer_correctly() {
    let soup = parse_document(PAGE);
    let root = find_content_root(&soup, None).unwrap();
    let root = Some(&root);
    assert_eq!(classify_link(&anchor(&soup, "/nav1"), root, None), "nav");
    assert_eq!(classify_link(&anchor(&soup, "/hdr1"), root, None), "header");
    assert_eq!(
        classify_link(&anchor(&soup, "/content1"), root, None),
        "content"
    );
    assert_eq!(
        classify_link(&anchor(&soup, "/side1"), root, None),
        "sidebar"
    );
    assert_eq!(
        classify_link(&anchor(&soup, "/foot1"), root, None),
        "footer"
    );
}

#[test]
fn rules_are_ordered_first_match_wins() {
    let soup =
        parse_document(r#"<html><body><nav><a class="promo" href="/x">x</a></nav></body></html>"#);
    let root = find_content_root(&soup, None).unwrap();
    let link = anchor(&soup, "/x");

    let promo_first = rules_from_config(Some(&json!([
        {"position": "promo", "selector": ".promo"},
        {"position": "nav", "selector": "nav"},
    ])))
    .unwrap();
    assert_eq!(
        classify_link(&link, Some(&root), Some(&promo_first)),
        "promo"
    );

    let nav_first = rules_from_config(Some(&json!([
        {"position": "nav", "selector": "nav"},
        {"position": "promo", "selector": ".promo"},
    ])))
    .unwrap();
    assert_eq!(classify_link(&link, Some(&root), Some(&nav_first)), "nav");
}

#[test]
fn site_specific_selector_catches_a_non_nav_menu() {
    let soup = parse_document(
        r#"<html><body><div class="mega-menu"><a href="/m">Menu</a></div></body></html>"#,
    );
    let root = find_content_root(&soup, None).unwrap();
    let link = anchor(&soup, "/m");
    assert_eq!(classify_link(&link, Some(&root), None), "content");

    let rules = rules_from_config(Some(
        &json!([{"position": "nav", "selector": ".mega-menu"}]),
    ))
    .unwrap();
    assert_eq!(classify_link(&link, Some(&root), Some(&rules)), "nav");
}

#[test]
fn malformed_site_specific_selector_is_skipped_not_raised() {
    let soup = parse_document(r#"<html><body><a href="/x">x</a></body></html>"#);
    let root = find_content_root(&soup, None).unwrap();
    let link = anchor(&soup, "/x");
    let rules = rules_from_config(Some(
        &json!([{"position": "broken", "selector": ":::not-css:::"}]),
    ))
    .unwrap();
    assert_eq!(classify_link(&link, Some(&root), Some(&rules)), "content");
}

#[test]
fn link_outside_a_narrowed_content_root_is_other_not_content() {
    let soup = parse_document(
        r#"<html><body><div class="promo"><a href="/x">x</a></div><main><p>real content</p></main></body></html>"#,
    );
    let config = ContentAreaConfig {
        include_selector: Some("main".into()),
        ..ContentAreaConfig::default()
    };
    let root = find_content_root(&soup, Some(&config)).unwrap();
    assert_eq!(
        classify_link(&anchor(&soup, "/x"), Some(&root), None),
        "other"
    );
}

#[test]
fn parse_html_classify_links_option_is_off_by_default() {
    let out = parse_html(PAGE, "https://example.com/", &ParseOptions::default()).unwrap();
    assert!(out.links.iter().all(|item| item.position.is_none()));
}

#[test]
fn parse_html_classify_links_option_wires_position_into_links() {
    let options = ParseOptions {
        classify_links: true,
        ..ParseOptions::default()
    };
    let out = parse_html(PAGE, "https://example.com/", &options).unwrap();
    let position = |href: &str| {
        out.links
            .iter()
            .find(|item| item.href == href)
            .and_then(|item| item.position.clone())
    };
    assert_eq!(position("https://example.com/nav1").as_deref(), Some("nav"));
    assert_eq!(
        position("https://example.com/foot1").as_deref(),
        Some("footer")
    );
    assert_eq!(
        position("https://example.com/content1").as_deref(),
        Some("content")
    );
}
