//! Ported parse-level tests of the reference suite: the
//! `<meta http-equiv=refresh>` declaration survives parsing as written. The check-level and
//! evidence-frame tests of that reference file need the checks, crawl and SF rule layers and are
//! deferred to those crates.

use spiderhead_parse::parse_html;
use spiderhead_parse::types::ParseOptions;

const PAGE_HEAD: &str =
    "<html><head><title>A title long enough for the length rule to pass</title>\
     <meta name=\"description\" content=\"A description long enough for the length rule here.\">";

fn page(head_extra: &str) -> String {
    format!(
        "{PAGE_HEAD}{head_extra}</head><body><h1>Heading</h1><p>Some body copy.</p></body></html>"
    )
}

fn meta_refresh(html: &str, url: &str) -> String {
    parse_html(html, url, &ParseOptions::default())
        .expect("parse_html")
        .meta_refresh
}

#[test]
fn the_content_attribute_is_kept_as_written() {
    let html = page(r#"<meta http-equiv="refresh" content="0; url=/target">"#);
    assert_eq!(
        meta_refresh(&html, "https://example.com/old"),
        "0; url=/target"
    );
}

#[test]
fn a_template_refresh_is_not_a_declaration() {
    let html = "<html><head></head><body><template>\
                <meta http-equiv=\"refresh\" content=\"0; url=/never\">\
                </template></body></html>";
    assert_eq!(meta_refresh(html, "https://example.com/"), "");
}

#[test]
fn a_page_declaring_none_reports_an_empty_string() {
    assert_eq!(meta_refresh(&page(""), "https://example.com/"), "");
}
