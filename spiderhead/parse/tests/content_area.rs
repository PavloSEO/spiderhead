//! Ports of the reference suite: the configurable content area (issue #19), automatic
//! detection (issue #96), and inline SVG/MathML text exclusion (issue #140). The subprocess
//! test that imports the reference module directly is deferred (reference-only mechanics).

use spiderhead_parse::content_area::{extract_area_text, resolve_content_area};
use spiderhead_parse::dom::parse_document;
use spiderhead_parse::parser::parse_html;
use spiderhead_parse::types::{ContentAreaConfig, ParseOptions};

const HTML: &str = r#"<html><body>
<nav>Home Products Services About Contact Blog Careers Support Login Sign up now</nav>
<main id="content"><h1>Widget</h1><p>A short but real product description.</p>
<a href="/related">related</a></main>
<footer>Copyright policy terms privacy sitemap careers investors press newsletter</footer>
</body></html>"#;

fn cfg() -> ContentAreaConfig {
    ContentAreaConfig::default()
}

fn include(selector: &str) -> ContentAreaConfig {
    ContentAreaConfig {
        include_selector: Some(selector.to_owned()),
        ..cfg()
    }
}

fn options_with_content_area(content_area: ContentAreaConfig) -> ParseOptions {
    ParseOptions {
        content_area: Some(content_area),
        ..ParseOptions::default()
    }
}

fn area_text(html: &str, config: Option<&ContentAreaConfig>) -> (String, String) {
    let soup = parse_document(html);
    let area = resolve_content_area(&soup, config).expect("resolve");
    (extract_area_text(&area), area.strategy.clone())
}

#[test]
fn default_detects_main_and_says_which_container_it_used() {
    let (text, strategy) = area_text(HTML, None);
    assert_eq!(strategy, "auto_main");
    assert!(text.contains("Widget") && text.contains("short but real product description"));
    assert!(!text.contains("Sign up now")); // nav
    assert!(!text.contains("newsletter")); // footer
}

#[test]
fn include_selector_wins_and_is_recorded() {
    let (text, strategy) = area_text(HTML, Some(&include("#content")));
    assert_eq!(strategy, "include_selector");
    assert!(text.contains("Widget"));
    assert!(!text.contains("Sign up now"));
}

#[test]
fn template_candidates_never_win_visible_content_selection() {
    let html = r#"<html><body>
    <template><main id="draft">Unreleased draft</main></template>
    <main id="published">Published guide</main>
    </body></html>"#;
    let (text, strategy) = area_text(html, None);
    assert_eq!(strategy, "auto_main");
    assert_eq!(text, "Published guide");

    let (text, strategy) = area_text(html, Some(&include("main")));
    assert_eq!(strategy, "include_selector");
    assert_eq!(text, "Published guide");

    let (text, strategy) = area_text(html, Some(&include("#draft")));
    assert_eq!(strategy, "fallback_default_body");
    assert_eq!(text, "Published guide");
}

#[test]
fn missing_selector_falls_back_and_says_so() {
    let (text, strategy) = area_text(HTML, Some(&include("#nope")));
    assert_eq!(strategy, "fallback_default_body");
    // Still falls back to the whole (exclusion-filtered) body, not an empty region.
    assert!(text.contains("Widget"));
}

#[test]
fn exclude_selectors_removes_class_or_id_based_boilerplate() {
    let html = r#"<html><body><div class="mega-menu">Deals Sale Clearance</div><p>Real content here.</p></body></html>"#;
    let config = ContentAreaConfig {
        exclude_selectors: Some(vec![".mega-menu".to_owned()]),
        ..cfg()
    };
    let (text, strategy) = area_text(html, Some(&config));
    assert_eq!(strategy, "default_body");
    assert!(text.contains("Real content here."));
    assert!(!text.contains("Deals"));
}

#[test]
fn exclude_tags_empty_list_disables_the_default_exclusions() {
    // Detection would scope this to <main>; the exclusion behaviour is exercised on the body.
    let config = ContentAreaConfig {
        root_selector: Some("body".to_owned()),
        exclude_tags: Some(Vec::new()),
        ..cfg()
    };
    let (text, _) = area_text(HTML, Some(&config));
    assert!(text.contains("Sign up now")); // nav kept this time
    assert!(text.contains("newsletter")); // footer kept this time
}

#[test]
fn word_count_changes_with_content_area_but_link_count_does_not() {
    let full_options = options_with_content_area(ContentAreaConfig {
        root_selector: Some("body".to_owned()),
        exclude_tags: Some(Vec::new()),
        ..cfg()
    });
    let full = parse_html(HTML, "https://example.com/page", &full_options).expect("parse");
    let scoped = parse_html(
        HTML,
        "https://example.com/page",
        &options_with_content_area(include("#content")),
    )
    .expect("parse");
    assert!(scoped.word_count < full.word_count);
    // Restricting the content area is a statement about text, not about links.
    assert_eq!(scoped.links.len(), full.links.len());
    assert!(full.links.iter().any(|l| l.href.ends_with("/related")));
}

#[test]
fn content_area_strategy_appears_per_page() {
    let default =
        parse_html(HTML, "https://example.com/page", &ParseOptions::default()).expect("parse");
    assert_eq!(default.content_area_strategy.as_deref(), Some("auto_main"));
    let scoped = parse_html(
        HTML,
        "https://example.com/page",
        &options_with_content_area(ContentAreaConfig {
            root_selector: Some("#nope".to_owned()),
            ..cfg()
        }),
    )
    .expect("parse");
    assert_eq!(
        scoped.content_area_strategy.as_deref(),
        Some("fallback_default_body")
    );
}

#[test]
fn content_area_strategy_none_when_text_disabled() {
    let options = ParseOptions {
        text: false,
        ..ParseOptions::default()
    };
    let off = parse_html(HTML, "https://example.com/page", &options).expect("parse");
    assert!(off.content_area_strategy.is_none());
    assert_eq!(off.word_count, 0);
}

const WORDPRESS_SHAPE: &str = r##"<html><body>
<a class="skip-link" href="#content">Skip to content</a>
<header><div class="site-branding">header</div><p>Call us any time on 555 0100</p></header>
<main id="content"><article><h1>Post</h1><p>The body of the article itself.</p></article></main>
<footer>Copyright</footer>
</body></html>"##;

#[test]
fn content_outside_main_is_not_counted_as_content() {
    let (text, strategy) = area_text(WORDPRESS_SHAPE, None);
    assert_eq!(strategy, "auto_main");
    assert!(text.contains("The body of the article itself."));
    assert!(!text.contains("Skip to content"));
    assert!(!text.contains("header"));
    assert!(!text.contains("555 0100"));
}

#[test]
fn role_main_is_used_when_there_is_no_main_element() {
    let html = r#"<html><body><header>masthead</header><div role="main"><p>Real text.</p></div></body></html>"#;
    let (text, strategy) = area_text(html, None);
    assert_eq!(strategy, "auto_role_main");
    assert!(text.contains("Real text."));
    assert!(!text.contains("masthead"));
}

#[test]
fn article_is_used_when_there_is_neither() {
    let html =
        "<html><body><header>masthead</header><article><p>Real text.</p></article></body></html>";
    let (text, strategy) = area_text(html, None);
    assert_eq!(strategy, "auto_article");
    assert!(text.contains("Real text."));
}

#[test]
fn a_page_with_none_of_the_three_still_resolves_to_the_body() {
    let html = "<html><body><div class='wrap'><p>Real text.</p></div></body></html>";
    let (text, strategy) = area_text(html, None);
    assert_eq!(strategy, "default_body");
    assert!(text.contains("Real text."));
}

#[test]
fn an_explicit_selector_still_wins_over_detection() {
    let (text, strategy) = area_text(WORDPRESS_SHAPE, Some(&include("header")));
    assert_eq!(strategy, "include_selector");
    assert!(text.contains("header"));
}

#[test]
fn a_configured_selector_that_matches_nothing_does_not_silently_auto_detect() {
    let (text, strategy) = area_text(WORDPRESS_SHAPE, Some(&include("#nope")));
    assert_eq!(strategy, "fallback_default_body");
    assert!(text.contains("The body of the article itself."));
}

#[test]
fn header_and_aside_are_excluded_on_the_fallback_path() {
    let html = "<html><body><header>masthead</header><aside>promo block</aside><div><p>Real text.</p></div></body></html>";
    let (text, strategy) = area_text(html, None);
    assert_eq!(strategy, "default_body");
    assert!(text.contains("Real text."));
    assert!(!text.contains("masthead"));
    assert!(!text.contains("promo block"));
}

#[test]
fn svg_icon_titles_do_not_inflate_word_count() {
    let icons: String = (0..20)
        .map(|i| format!("<svg><title>search icon {i}</title></svg>"))
        .collect();
    let body: Vec<String> = (0..40).map(|i| format!("word{i}")).collect();
    let html = format!("<html><body>{icons}<p>{}</p></body></html>", body.join(" "));
    let parsed =
        parse_html(&html, "https://example.com/", &ParseOptions::default()).expect("parse");
    assert_eq!(parsed.word_count, 40);
}

#[test]
fn extract_area_text_excludes_svg_and_math() {
    let html = "<html><body><svg><text>decorative label</text></svg>\
                <math><mtext>x plus y</mtext></math><p>Real body copy.</p></body></html>";
    let (text, _) = area_text(html, None);
    assert_eq!(text, "Real body copy.");
}
