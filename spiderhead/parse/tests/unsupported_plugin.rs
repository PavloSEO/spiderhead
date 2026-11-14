//! The registry check that runs through crawl, evidence and rules belongs to later crates.

use spiderhead_parse::dom::parse_document;
use spiderhead_parse::parser::content::unsupported_plugin_count;

fn count(html: &str) -> u64 {
    unsupported_plugin_count(&parse_document(html))
}

#[test]
fn embed_element_counts_as_a_plugin() {
    let html =
        r#"<html><body><embed src="game.swf" type="application/x-shockwave-flash"></body></html>"#;
    assert_eq!(count(html), 1);
}

#[test]
fn applet_element_always_counts() {
    let html = "<html><body><applet code='Game.class'></applet></body></html>";
    assert_eq!(count(html), 1);
}

#[test]
fn object_with_image_type_is_excluded() {
    let html =
        r#"<html><body><object type="image/svg+xml" data="icon.svg"></object></body></html>"#;
    assert_eq!(count(html), 0);
}

#[test]
fn object_with_pdf_type_is_excluded() {
    for type_attr in [
        "application/pdf",
        " APPLICATION/PDF ",
        "application/pdf; version=1.7",
    ] {
        let html = format!(
            r#"<html><body><object type="{type_attr}" data="brochure.pdf"></object></body></html>"#
        );
        assert_eq!(count(&html), 0, "type attribute {type_attr:?}");
    }
}

#[test]
fn object_with_flash_type_still_counts() {
    let html = concat!(
        r#"<html><body><object type="application/x-shockwave-flash" data="movie.swf">"#,
        "</object></body></html>"
    );
    assert_eq!(count(html), 1);
}

#[test]
fn page_with_no_plugin_elements_counts_zero() {
    let html = "<html><body><video src='clip.mp4'></video><img src='x.jpg'></body></html>";
    assert_eq!(count(html), 0);
}
