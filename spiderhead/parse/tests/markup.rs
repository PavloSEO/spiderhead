//! Offline structural markup evidence, including source/tree-builder differences.
//! in this crate).

use std::collections::BTreeSet;

use serde_json::{json, Value};
use spiderhead_parse::dom::parse_document;
use spiderhead_parse::markup::{markup_facts, MarkupFacts, A11Y_CODES, AMP_CODES};

const AMP_HEAD: &str = r#"<meta charset="utf-8">
<meta name="viewport" content="width=device-width">
<script async src="https://cdn.ampproject.org/v0.js"></script>
<style amp-boilerplate>body{visibility:hidden}</style>
<noscript><style amp-boilerplate>body{visibility:visible}</style></noscript>"#;

const MAIN_BODY: &str = "<main><h1>Title</h1></main>";

fn facts(html: &str) -> MarkupFacts {
    let soup = parse_document(html);
    markup_facts(html, &soup).expect("markup_facts")
}

/// Issue codes of the `amp` block (`errors`) or the `accessibility` block (`issues`).
fn amp_codes(result: &MarkupFacts) -> BTreeSet<String> {
    result.amp["errors"]
        .as_array()
        .expect("amp errors array")
        .iter()
        .map(|entry| entry["code"].as_str().expect("code").to_owned())
        .collect()
}

fn a11y_codes(result: &MarkupFacts) -> BTreeSet<String> {
    result
        .accessibility
        .issues
        .iter()
        .map(|entry| entry["code"].as_str().expect("code").to_owned())
        .collect()
}

fn set(codes: &[&str]) -> BTreeSet<String> {
    codes.iter().map(|code| (*code).to_owned()).collect()
}

fn amp_document(head: &str, body: &str, marker: &str) -> String {
    format!(
        "<!doctype html><html {marker} lang=\"en\"><head>{head}</head><body>{body}</body></html>"
    )
}

fn amp_doc() -> String {
    amp_document(AMP_HEAD, MAIN_BODY, "amp")
}

fn issue<'a>(result: &'a MarkupFacts, code: &str) -> &'a Value {
    result
        .accessibility
        .issues
        .iter()
        .find(|entry| entry["code"] == code)
        .unwrap_or_else(|| panic!("issue {code} missing"))
}

#[test]
fn amp_happy_path_is_structural_only_and_width_only_viewport_is_valid() {
    for marker in ["amp", "⚡"] {
        let result = facts(&amp_document(AMP_HEAD, MAIN_BODY, marker));
        assert_eq!(result.amp["declared"], true, "marker {marker}");
        assert!(
            result.amp["errors"].as_array().unwrap().is_empty(),
            "marker {marker}"
        );
        assert_eq!(result.amp["validator_state"], "unavailable");
        assert!(result.amp["validator_reason"]
            .as_str()
            .unwrap()
            .contains("licence"));
        assert!(result.accessibility.issues.is_empty(), "marker {marker}");
        assert!(result
            .accessibility
            .unavailable
            .iter()
            .any(|item| item == "contrast requires computed styles"));
    }
}

#[test]
fn non_amp_has_no_amp_errors() {
    let result = facts(r#"<html lang="en"><body><main>Text</main></body></html>"#);
    assert_eq!(result.amp["declared"], false);
    assert!(result.amp["errors"].as_array().unwrap().is_empty());
}

#[test]
fn example_markup_does_not_declare_an_amp_document() {
    let fakes = [
        "<!-- <html amp> -->",
        r#"<script>const example="<html amp>";</script>"#,
        "<template><html amp></html></template>",
        "<textarea><html amp></textarea>",
        r#"<div data-example="<html amp>">Example</div>"#,
        "&lt;html amp&gt;",
    ];
    for fake in fakes {
        let result = facts(fake);
        assert_eq!(result.amp["declared"], false, "fake {fake}");
        assert!(
            result.amp["errors"].as_array().unwrap().is_empty(),
            "fake {fake}"
        );
    }
}

#[test]
fn explicit_source_structure_survives_lxml_manufactured_head_and_body() {
    let result = facts(r#"<html amp lang="en"><meta charset="utf-8"><p>Text</p></html>"#);
    assert_eq!(amp_codes(&result), set(&AMP_CODES));
}

#[test]
fn comments_scripts_templates_cannot_supply_required_document_structure() {
    let fakes = [
        "<!-- <!doctype html><head></head><body></body> -->",
        r#"<script>const x="<!doctype html><head></head><body></body>"</script>"#,
        "<template><head></head><body></body></template>",
    ];
    for fake in fakes {
        let result = facts(&format!(r#"<html amp lang="en">{fake}</html>"#));
        let codes = amp_codes(&result);
        assert!(
            set(&[
                "AMP_INVALID_DOCTYPE",
                "AMP_MISSING_HEAD",
                "AMP_MISSING_BODY"
            ])
            .is_subset(&codes),
            "fake {fake}: {codes:?}"
        );
    }
}

#[test]
fn amp_requires_html5_doctype() {
    for doctype in ["<!DOCTYPE html PUBLIC \"legacy\">", "<!doctype svg>", ""] {
        let html = amp_doc().replace("<!doctype html>", doctype);
        let result = facts(&html);
        assert!(
            amp_codes(&result).contains("AMP_INVALID_DOCTYPE"),
            "doctype {doctype:?}"
        );
    }
}

#[test]
fn doctype_after_document_content_is_invalid() {
    let result = facts(&format!("Text{}", amp_doc()));
    assert!(amp_codes(&result).contains("AMP_INVALID_DOCTYPE"));
}

#[test]
fn structural_failures_identify_the_failed_requirement() {
    let cases: [(String, &str); 8] = [
        (
            AMP_HEAD.replace(r#"charset="utf-8""#, r#"charset="windows-1251""#),
            "AMP_INVALID_CHARSET",
        ),
        (
            format!("<title>Title</title>{AMP_HEAD}"),
            "AMP_INVALID_CHARSET",
        ),
        (
            AMP_HEAD.replace("width=device-width", "initial-scale=1"),
            "AMP_INVALID_VIEWPORT",
        ),
        (AMP_HEAD.replace(" async", ""), "AMP_INVALID_RUNTIME"),
        (
            AMP_HEAD.replace(
                "https://cdn.ampproject.org/v0.js",
                "https://example.test/v0.js",
            ),
            "AMP_INVALID_RUNTIME",
        ),
        (
            AMP_HEAD.replace(
                "<script async",
                r#"<script type="application/ld+json" async"#,
            ),
            "AMP_INVALID_RUNTIME",
        ),
        (
            AMP_HEAD
                .replace("<noscript>", "<div>")
                .replace("</noscript>", "</div>"),
            "AMP_INVALID_BOILERPLATE",
        ),
        (
            AMP_HEAD.replace("body{visibility:hidden}", ""),
            "AMP_INVALID_BOILERPLATE",
        ),
    ];
    for (head, expected) in cases {
        let result = facts(&amp_document(&head, MAIN_BODY, "amp"));
        assert!(amp_codes(&result).contains(expected), "expected {expected}");
        let error = result.amp["errors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["code"] == expected)
            .expect("error entry");
        assert!(error.get("expected").is_some() && error.get("observed").is_some());
    }
}

#[test]
fn required_head_markup_in_template_or_body_does_not_pass() {
    let result = facts(&amp_document(
        &format!("<template>{AMP_HEAD}</template>"),
        AMP_HEAD,
        "amp",
    ));
    let codes = amp_codes(&result);
    assert!(
        set(&[
            "AMP_INVALID_CHARSET",
            "AMP_INVALID_VIEWPORT",
            "AMP_INVALID_RUNTIME",
            "AMP_INVALID_BOILERPLATE",
        ])
        .is_subset(&codes),
        "{codes:?}"
    );
}

#[test]
fn amp_css_presence_is_not_a_full_boilerplate_validator() {
    let result = facts(&amp_document(
        &AMP_HEAD.replace("body{visibility:hidden}", "not valid CSS"),
        MAIN_BODY,
        "amp",
    ));
    assert!(result.amp["errors"].as_array().unwrap().is_empty());
    assert!(!result.amp["unavailable"].as_array().unwrap().is_empty());
}

#[test]
fn first_a11y_rules_with_useful_evidence() {
    let result = facts(
        r#"<html><body><h1>Title</h1><h3>Details</h3><img src="/photo.png"><input id="email" name="email"></body></html>"#,
    );
    assert_eq!(a11y_codes(&result), set(&A11Y_CODES));
    let image = issue(&result, "A11Y_IMAGE_ALT_MISSING");
    assert_eq!(image["samples"][0]["src"], "/photo.png");
    let form = issue(&result, "A11Y_FORM_LABEL_MISSING");
    assert_eq!(form["samples"][0]["id"], "email");
    let heading = issue(&result, "A11Y_HEADING_ORDER");
    assert_eq!(heading["samples"][0]["previous_level"], 1);
    for entry in &result.accessibility.issues {
        assert_eq!(entry["count"], 1, "{}", entry["code"]);
    }
}

#[test]
fn form_names_labels_aria_and_hidden_controls() {
    let result = facts(
        r#"<html lang="en"><body><main>
      <label for="a">First</label><input id="a">
      <label>Second <input></label><input aria-label="Third">
      <span id="fourth">Fourth</span><input aria-labelledby="missing fourth">
      <span id="hidden-label" hidden>Hidden name</span><input aria-labelledby="hidden-label">
      <input title="Fallback name"><input type="hidden"><input type="submit">
      <input type="reset"><input type="button"><input type="image"><button>Go</button>
      <div hidden><input></div><div aria-hidden="true"><select></select></div>
      <div inert><textarea></textarea></div>
    </main></body></html>"#,
    );
    assert!(result.accessibility.issues.is_empty());
}

#[test]
fn empty_labels_missing_aria_references_and_control_values_are_not_labels() {
    let result = facts(
        r#"<html lang="en"><body><main>
      <label for="a"> </label><input id="a">
      <input aria-label=" " aria-labelledby="missing" placeholder="Email">
      <label><select><option>Choice</option></select></label>
      <label><textarea>Default text</textarea></label>
      <template><span id="fake">Fake name</span></template><input aria-labelledby="fake">
    </main></body></html>"#,
    );
    let first = &result.accessibility.issues[0];
    assert_eq!(first["code"], "A11Y_FORM_LABEL_MISSING");
    assert_eq!(first["count"], 5);
}

#[test]
fn decorative_hidden_and_inert_images_do_not_trigger_missing_alt() {
    let result = facts(
        r#"<html lang="en"><body><main>
      <img alt=""><img role="presentation"><img role="none">
      <div aria-hidden="true"><img></div><div hidden><img></div>
      <div style="display: none !important"><img></div>
      <template><img><input><h6>Fake heading</h6></template>
      <h1>Title</h1><h2>Section</h2><h1>Next title</h1>
    </main></body></html>"#,
    );
    assert!(result.accessibility.issues.is_empty());
}

#[test]
fn aria_named_images_do_not_trigger_missing_alt() {
    let result = facts(
        r#"<html lang="en"><body><main>
      <span id="photo-name">Product photo</span>
      <span id="hidden-name" hidden>Diagram</span>
      <img aria-label="Product"><img aria-labelledby="photo-name">
      <img aria-labelledby="missing hidden-name">
      <amp-img aria-label="AMP product"></amp-img>
    </main></body></html>"#,
    );
    assert!(result.accessibility.issues.is_empty());
}

#[test]
fn dangling_empty_ambiguous_or_inert_image_names_are_missing() {
    let result = facts(
        r#"<html lang="en"><body><main>
      <span id="empty"> </span>
      <span id="duplicate">First</span><span id="duplicate">Second</span>
      <template><span id="inert-name">Example</span></template>
      <img aria-label=" "><img aria-labelledby="missing">
      <img aria-labelledby="empty"><img aria-labelledby="duplicate">
      <img aria-labelledby="inert-name">
    </main></body></html>"#,
    );
    let first = &result.accessibility.issues[0];
    assert_eq!(first["code"], "A11Y_IMAGE_ALT_MISSING");
    assert_eq!(first["count"], 5);
}

#[test]
fn inert_main_cannot_satisfy_a_landmark_and_empty_shell_is_not_content() {
    let result = facts(
        r#"<html lang="en"><body><template><main>Fake</main></template><p>Real</p></body></html>"#,
    );
    assert_eq!(a11y_codes(&result), set(&["A11Y_MAIN_LANDMARK_MISSING"]));
    let shell = facts(
        r#"<html lang="en"><body><div><script>bootstrap()</script></div><!-- Text --></body></html>"#,
    );
    assert!(shell.accessibility.issues.is_empty());
}

#[test]
fn aria_main_and_plain_body_text_are_recognized() {
    let aria = facts(r#"<html lang="en"><body><div role="main">Text</div></body></html>"#);
    assert!(aria.accessibility.issues.is_empty());
    let plain = facts(r#"<html lang="en"><body>Text</body></html>"#);
    assert_eq!(a11y_codes(&plain), set(&["A11Y_MAIN_LANDMARK_MISSING"]));
}

#[test]
fn hidden_only_body_is_not_meaningful_content() {
    let result = facts(r#"<html lang="en"><body><input type="hidden" value="csrf"></body></html>"#);
    assert!(result.accessibility.issues.is_empty());
}

#[test]
fn label_for_overrides_wrapping_and_ambiguous_references_are_not_names() {
    let result = facts(
        r#"<html lang="en"><body><main>
      <label for="elsewhere">Name<input id="nested"></label>
      <span id="duplicate">Name</span><span id="duplicate">Other</span>
      <input aria-labelledby="duplicate">
    </main></body></html>"#,
    );
    let first = &result.accessibility.issues[0];
    assert_eq!(first["code"], "A11Y_FORM_LABEL_MISSING");
    assert_eq!(first["count"], 2);
}

#[test]
fn issue_samples_are_bounded_and_counts_are_complete() {
    let body = format!("<main>{}</main>", r#"<img src="/photo.png">"#.repeat(45));
    let result = facts(&amp_document(AMP_HEAD, &body, "amp"));
    let first = &result.accessibility.issues[0];
    assert_eq!(first["code"], "A11Y_IMAGE_ALT_MISSING");
    assert_eq!(first["count"], 45);
    assert_eq!(first["samples"].as_array().unwrap().len(), 20);
    assert_eq!(result.accessibility.sample_limit, 20);
    assert_eq!(first["samples_truncated"], true);
}

#[test]
fn amp_evidence_is_also_bounded() {
    let head = format!(
        r#"<meta charset="utf-8">{}"#,
        r#"<script src="/wrong.js"></script>"#.repeat(45)
    );
    let result = facts(&amp_document(&head, MAIN_BODY, "amp"));
    let error = result.amp["errors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["code"] == "AMP_INVALID_RUNTIME")
        .expect("runtime error");
    assert_eq!(error["observed"]["count"], 45);
    assert_eq!(error["observed"]["samples"].as_array().unwrap().len(), 20);
    assert_eq!(error["observed"]["samples_truncated"], true);
}

#[test]
fn caller_soup_is_unchanged() {
    let html = amp_doc();
    let soup = parse_document(&html);
    let before = soup.html();
    markup_facts(&html, &soup).expect("markup_facts");
    assert_eq!(soup.html(), before);
    let _ = json!(null);
}
