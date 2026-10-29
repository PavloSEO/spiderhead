//! Ports of the parser-level tests in the reference suite: frame inventory of the
//! parsed page. The audit-level tests of that file (CONTENT_IN_IFRAME / THIN_CONTENT) need the
//! checks crate and are listed in the slice report instead.

use spiderhead_parse::parser::parse_html;
use spiderhead_parse::types::ParseOptions;

fn parse(html: &str, url: &str) -> spiderhead_parse::models::ParsedPage {
    parse_html(html, url, &ParseOptions::default()).expect("fixture HTML parses")
}

#[test]
fn a_frame_is_recorded_with_where_it_sits_and_who_serves_it() {
    let parsed = parse(
        "<html><body>\
         <header><iframe src=\"/chat-widget\"></iframe></header>\
         <main><p>tiny</p><iframe src=\"/the-actual-copy.html\" title=\"Copy\"></iframe>\
         <iframe src=\"https://www.youtube.com/embed/x\" loading=\"lazy\"></iframe></main>\
         </body></html>",
        "https://example.com/page",
    );
    let frames = &parsed.frames;
    let raw: Vec<&str> = frames.iter().map(|f| f.raw_src.as_str()).collect();
    assert_eq!(
        raw,
        [
            "/chat-widget",
            "/the-actual-copy.html",
            "https://www.youtube.com/embed/x"
        ]
    );
    let (widget, copy, embed) = (&frames[0], &frames[1], &frames[2]);
    // A frame in the header is a widget; the content area is what matters.
    assert!(!widget.in_content_area);
    assert!(copy.in_content_area && copy.same_origin);
    assert_eq!(copy.src, "https://example.com/the-actual-copy.html");
    assert_eq!(copy.title, "Copy");
    assert!(embed.in_content_area && !embed.same_origin);
    assert_eq!(embed.loading, "lazy");
}

#[test]
fn a_template_only_frame_is_not_a_frame() {
    let parsed = parse(
        "<html><body><main><template><iframe src=\"/never\"></iframe></template></main></body></html>",
        "https://example.com/page",
    );
    assert!(parsed.frames.is_empty());
}

#[test]
fn a_javascript_populated_frame_still_counts() {
    let parsed = parse(
        "<html><body><main><iframe></iframe></main></body></html>",
        "https://example.com/page",
    );
    assert_eq!(parsed.frames.len(), 1);
    let frame = &parsed.frames[0];
    assert_eq!(frame.src, "");
    assert!(frame.same_origin);
    assert!(frame.in_content_area);
}

#[test]
fn the_marker_used_to_locate_frames_does_not_survive_into_the_output() {
    let parsed = parse(
        "<html><body><main><iframe src=\"/x\"></iframe></main></body></html>",
        "https://example.com/page",
    );
    assert!(!parsed.text.contains("seohead-frame"));
    assert!(!parsed.content_text.contains("seohead-frame"));
}
