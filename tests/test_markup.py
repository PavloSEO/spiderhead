"""Offline structural markup evidence, including source/tree-builder differences."""

import pytest
from bs4 import BeautifulSoup

from seohead.checks.markup import A11Y_CODES, AMP_CODES, markup_facts

AMP_HEAD = """<meta charset="utf-8">
<meta name="viewport" content="width=device-width">
<script async src="https://cdn.ampproject.org/v0.js"></script>
<style amp-boilerplate>body{visibility:hidden}</style>
<noscript><style amp-boilerplate>body{visibility:visible}</style></noscript>"""


def facts(html, parser="lxml"):
    return markup_facts(html, BeautifulSoup(html, parser))


def codes(result, group):
    key = "errors" if group == "amp" else "issues"
    return {entry["code"] for entry in result[group][key]}


def amp_document(head=AMP_HEAD, body="<main><h1>Title</h1></main>", marker="amp"):
    return f'<!doctype html><html {marker} lang="en"><head>{head}</head><body>{body}</body></html>'


@pytest.mark.parametrize("marker", ["amp", "⚡"])
def test_amp_happy_path_is_structural_only_and_width_only_viewport_is_valid(marker):
    result = facts(amp_document(marker=marker))
    assert result["amp"]["declared"] is True
    assert result["amp"]["errors"] == []
    assert result["amp"]["validator_state"] == "unavailable"
    assert "licence" in result["amp"]["validator_reason"]
    assert result["accessibility"]["issues"] == []
    assert "contrast requires computed styles" in result["accessibility"]["unavailable"]


def test_non_amp_has_no_amp_errors():
    result = facts('<html lang="en"><body><main>Text</main></body></html>')
    assert result["amp"]["declared"] is False
    assert result["amp"]["errors"] == []


@pytest.mark.parametrize(
    "fake",
    [
        "<!-- <html amp> -->",
        '<script>const example="<html amp>";</script>',
        "<template><html amp></html></template>",
        "<textarea><html amp></textarea>",
        '<div data-example="<html amp>">Example</div>',
        "&lt;html amp&gt;",
    ],
)
def test_example_markup_does_not_declare_an_amp_document(fake):
    result = facts(fake)
    assert result["amp"]["declared"] is False
    assert result["amp"]["errors"] == []


def test_explicit_source_structure_survives_lxml_manufactured_head_and_body():
    result = facts('<html amp lang="en"><meta charset="utf-8"><p>Text</p></html>')
    assert codes(result, "amp") == set(AMP_CODES)


@pytest.mark.parametrize(
    "fake",
    [
        "<!-- <!doctype html><head></head><body></body> -->",
        '<script>const x="<!doctype html><head></head><body></body>"</script>',
        "<template><head></head><body></body></template>",
    ],
)
def test_comments_scripts_templates_cannot_supply_required_document_structure(fake):
    result = facts(f'<html amp lang="en">{fake}</html>')
    assert {"AMP_INVALID_DOCTYPE", "AMP_MISSING_HEAD", "AMP_MISSING_BODY"} <= codes(result, "amp")


@pytest.mark.parametrize("doctype", ['<!DOCTYPE html PUBLIC "legacy">', "<!doctype svg>", ""])
def test_amp_requires_html5_doctype(doctype):
    result = facts(amp_document().replace("<!doctype html>", doctype))
    assert "AMP_INVALID_DOCTYPE" in codes(result, "amp")


def test_doctype_after_document_content_is_invalid():
    result = facts("Text" + amp_document())
    assert "AMP_INVALID_DOCTYPE" in codes(result, "amp")


@pytest.mark.parametrize(
    "head, expected",
    [
        (AMP_HEAD.replace('charset="utf-8"', 'charset="windows-1251"'), "AMP_INVALID_CHARSET"),
        ("<title>Title</title>" + AMP_HEAD, "AMP_INVALID_CHARSET"),
        (AMP_HEAD.replace("width=device-width", "initial-scale=1"), "AMP_INVALID_VIEWPORT"),
        (AMP_HEAD.replace(" async", ""), "AMP_INVALID_RUNTIME"),
        (
            AMP_HEAD.replace("https://cdn.ampproject.org/v0.js", "https://example.test/v0.js"),
            "AMP_INVALID_RUNTIME",
        ),
        (
            AMP_HEAD.replace("<script async", '<script type="application/ld+json" async'),
            "AMP_INVALID_RUNTIME",
        ),
        (
            AMP_HEAD.replace("<noscript>", "<div>").replace("</noscript>", "</div>"),
            "AMP_INVALID_BOILERPLATE",
        ),
        (AMP_HEAD.replace("body{visibility:hidden}", ""), "AMP_INVALID_BOILERPLATE"),
    ],
)
def test_structural_failures_identify_the_failed_requirement(head, expected):
    result = facts(amp_document(head=head))
    assert expected in codes(result, "amp")
    error = next(error for error in result["amp"]["errors"] if error["code"] == expected)
    assert "expected" in error and "observed" in error


def test_required_head_markup_in_template_or_body_does_not_pass():
    result = facts(amp_document(head=f"<template>{AMP_HEAD}</template>", body=AMP_HEAD))
    assert {
        "AMP_INVALID_CHARSET",
        "AMP_INVALID_VIEWPORT",
        "AMP_INVALID_RUNTIME",
        "AMP_INVALID_BOILERPLATE",
    } <= codes(result, "amp")


def test_amp_css_presence_is_not_a_full_boilerplate_validator():
    result = facts(amp_document(head=AMP_HEAD.replace("body{visibility:hidden}", "not valid CSS")))
    assert result["amp"]["errors"] == []
    assert result["amp"]["unavailable"]


def test_first_a11y_rules_with_useful_evidence():
    result = facts(
        '<html><body><h1>Title</h1><h3>Details</h3><img src="/photo.png"><input id="email" name="email"></body></html>'
    )
    assert codes(result, "accessibility") == set(A11Y_CODES)
    issues = {entry["code"]: entry for entry in result["accessibility"]["issues"]}
    assert issues["A11Y_IMAGE_ALT_MISSING"]["samples"][0]["src"] == "/photo.png"
    assert issues["A11Y_FORM_LABEL_MISSING"]["samples"][0]["id"] == "email"
    assert issues["A11Y_HEADING_ORDER"]["samples"][0]["previous_level"] == 1
    assert all(issue["count"] == 1 for issue in issues.values())


def test_form_names_labels_aria_and_hidden_controls():
    result = facts("""<html lang="en"><body><main>
      <label for="a">First</label><input id="a">
      <label>Second <input></label><input aria-label="Third">
      <span id="fourth">Fourth</span><input aria-labelledby="missing fourth">
      <span id="hidden-label" hidden>Hidden name</span><input aria-labelledby="hidden-label">
      <input title="Fallback name"><input type="hidden"><input type="submit">
      <input type="reset"><input type="button"><input type="image"><button>Go</button>
      <div hidden><input></div><div aria-hidden="true"><select></select></div>
      <div inert><textarea></textarea></div>
    </main></body></html>""")
    assert result["accessibility"]["issues"] == []


def test_empty_labels_missing_aria_references_and_control_values_are_not_labels():
    result = facts("""<html lang="en"><body><main>
      <label for="a"> </label><input id="a">
      <input aria-label=" " aria-labelledby="missing" placeholder="Email">
      <label><select><option>Choice</option></select></label>
      <label><textarea>Default text</textarea></label>
      <template><span id="fake">Fake name</span></template><input aria-labelledby="fake">
    </main></body></html>""")
    issue = result["accessibility"]["issues"][0]
    assert issue["code"] == "A11Y_FORM_LABEL_MISSING"
    assert issue["count"] == 5


def test_decorative_hidden_and_inert_images_do_not_trigger_missing_alt():
    result = facts("""<html lang="en"><body><main>
      <img alt=""><img role="presentation"><img role="none">
      <div aria-hidden="true"><img></div><div hidden><img></div>
      <div style="display: none !important"><img></div>
      <template><img><input><h6>Fake heading</h6></template>
      <h1>Title</h1><h2>Section</h2><h1>Next title</h1>
    </main></body></html>""")
    assert result["accessibility"]["issues"] == []


def test_aria_named_images_do_not_trigger_missing_alt():
    result = facts("""<html lang="en"><body><main>
      <span id="photo-name">Product photo</span>
      <span id="hidden-name" hidden>Diagram</span>
      <img aria-label="Product"><img aria-labelledby="photo-name">
      <img aria-labelledby="missing hidden-name">
      <amp-img aria-label="AMP product"></amp-img>
    </main></body></html>""")
    assert result["accessibility"]["issues"] == []


def test_dangling_empty_ambiguous_or_inert_image_names_are_missing():
    result = facts("""<html lang="en"><body><main>
      <span id="empty"> </span>
      <span id="duplicate">First</span><span id="duplicate">Second</span>
      <template><span id="inert-name">Example</span></template>
      <img aria-label=" "><img aria-labelledby="missing">
      <img aria-labelledby="empty"><img aria-labelledby="duplicate">
      <img aria-labelledby="inert-name">
    </main></body></html>""")
    issue = result["accessibility"]["issues"][0]
    assert issue["code"] == "A11Y_IMAGE_ALT_MISSING"
    assert issue["count"] == 5


def test_inert_main_cannot_satisfy_a_landmark_and_empty_shell_is_not_content():
    result = facts(
        '<html lang="en"><body><template><main>Fake</main></template><p>Real</p></body></html>'
    )
    assert codes(result, "accessibility") == {"A11Y_MAIN_LANDMARK_MISSING"}
    shell = facts(
        '<html lang="en"><body><div><script>bootstrap()</script></div><!-- Text --></body></html>'
    )
    assert shell["accessibility"]["issues"] == []


def test_aria_main_and_plain_body_text_are_recognized():
    assert (
        facts('<html lang="en"><body><div role="main">Text</div></body></html>')["accessibility"][
            "issues"
        ]
        == []
    )
    assert codes(facts('<html lang="en"><body>Text</body></html>'), "accessibility") == {
        "A11Y_MAIN_LANDMARK_MISSING"
    }


def test_hidden_only_body_is_not_meaningful_content():
    result = facts('<html lang="en"><body><input type="hidden" value="csrf"></body></html>')
    assert result["accessibility"]["issues"] == []


def test_label_for_overrides_wrapping_and_ambiguous_references_are_not_names():
    result = facts("""<html lang="en"><body><main>
      <label for="elsewhere">Name<input id="nested"></label>
      <span id="duplicate">Name</span><span id="duplicate">Other</span>
      <input aria-labelledby="duplicate">
    </main></body></html>""")
    issue = result["accessibility"]["issues"][0]
    assert issue["code"] == "A11Y_FORM_LABEL_MISSING"
    assert issue["count"] == 2


def test_issue_samples_are_bounded_and_counts_are_complete():
    result = facts(amp_document(body="<main>" + '<img src="/photo.png">' * 45 + "</main>"))
    issue = result["accessibility"]["issues"][0]
    assert issue["code"] == "A11Y_IMAGE_ALT_MISSING"
    assert issue["count"] == 45
    assert len(issue["samples"]) == result["accessibility"]["sample_limit"] == 20
    assert issue["samples_truncated"] is True


def test_amp_evidence_is_also_bounded():
    result = facts(
        amp_document(head='<meta charset="utf-8">' + '<script src="/wrong.js"></script>' * 45)
    )
    error = next(
        error for error in result["amp"]["errors"] if error["code"] == "AMP_INVALID_RUNTIME"
    )
    assert error["observed"]["count"] == 45
    assert len(error["observed"]["samples"]) == 20
    assert error["observed"]["samples_truncated"] is True


def test_caller_soup_is_unchanged():
    html = amp_document()
    soup = BeautifulSoup(html, "lxml")
    before = str(soup)
    markup_facts(html, soup)
    assert str(soup) == before
