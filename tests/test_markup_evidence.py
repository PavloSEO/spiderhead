"""Document markup is retained and audited without changing frozen scan.v1 SQL."""

import json

import pandas as pd
import pytest

from seohead.checks.markup import A11Y_CODES, AMP_CODES
from seohead.checks.parser import parse_html
from seohead.sf.config import load_config
from seohead.sf.core.context import AuditContext
from seohead.sf.core.loader import LoadedExports
from seohead.sf.core.markup_rules import SCHEMA_CODES, check_document_markup
from seohead.storage import ScanError, open_scan
from seohead.storage.markup_evidence import validate_context
from seohead.storage.native_scan import NativeScan
from seohead.storage.structured_evidence import structured_payload
from tests.test_native_capture import _claim, _event
from tests.test_scan_native import _metadata, _record, _runtime

URL = "https://example.test/"
AMP = "https://example.test/amp"


def _scan(tmp_path, pages, *, parsed=True):
    path = tmp_path / "markup.sqlite"
    metadata = _metadata()
    with NativeScan.create(path, **metadata) as scan:
        schema = list(scan.con.execute("SELECT name,sql FROM sqlite_master ORDER BY name"))
        for url, html in pages:
            lease = _claim(scan, url)
            record = _record(url)
            record["status_code"] = 200
            markup = parse_html(html, url) if parsed else None
            scan.commit_page(
                lease,
                record,
                captures=[_event(url, body=html.encode())],
                runtime=_runtime(),
                content_capture={
                    "html": html if parsed else None,
                    "parsed": markup,
                    "settings": metadata["config"],
                    "indexable": True,
                    "canonical_target": "",
                    "unavailable_reason": "" if parsed else "fixture body unparsed",
                    "extraction_rules": None,
                },
            )
        assert list(scan.con.execute("SELECT name,sql FROM sqlite_master ORDER BY name")) == schema
    return path


def _ctx(con=None, targets=False):
    exports = LoadedExports(
        frames={
            "internal_all": pd.DataFrame(
                [
                    {
                        "Address": URL,
                        "Status Code": 200,
                        "Content Type": "text/html",
                        "AMP HTML": AMP if targets else "",
                    },
                    {"Address": AMP, "Status Code": 200, "Content Type": "text/html"},
                ]
            )
        }
    )
    ctx = AuditContext(exports, load_config(None))
    ctx.scan_con = con
    return ctx


def test_native_markup_emits_all_static_failures_with_document_identity(tmp_path):
    html = '<html amp><body><h1>One</h1><h3>Three</h3><input id="x"><img src="x.jpg"></body></html>'
    path = _scan(tmp_path, [(AMP, html)])
    con = open_scan(path, require_audit=False)
    try:
        ctx = _ctx(con)
        check_document_markup(ctx)
        fired = {issue.check for issue in ctx.issues}
        assert set(A11Y_CODES) <= fired
        assert set(AMP_CODES) - {"AMP_MISSING_BODY"} <= fired
        assert "AMP_MISSING_BODY" not in fired
        assert all(issue.details["document_id"] > 0 for issue in ctx.issues)
        assert all(issue.details["representation"] == "static" for issue in ctx.issues)
        facts = json.loads(
            con.execute(
                "SELECT payload_json FROM context_items WHERE kind='markup_evidence'"
            ).fetchone()[0]
        )["facts"]
        assert facts["amp"]["validator_state"] == "unavailable"
        assert "licence" in facts["amp"]["validator_reason"]
    finally:
        con.close()


def test_paired_amp_without_marker_is_a_finding_and_structure_unavailable(tmp_path):
    path = _scan(tmp_path, [(AMP, '<html lang="en"><body><main>AMP target</main></body></html>')])
    con = open_scan(path, require_audit=False)
    try:
        ctx = _ctx(con, targets=True)
        check_document_markup(ctx)
        assert "AMP_MISSING_HTML_ATTRIBUTE" in {issue.check for issue in ctx.issues}
        assert set(AMP_CODES) <= {item.id for item in ctx.skipped}
    finally:
        con.close()


def test_exports_and_unparsed_native_bodies_skip_instead_of_pass(tmp_path):
    ctx = _ctx()
    check_document_markup(ctx)
    assert set(A11Y_CODES) | set(AMP_CODES) | set(SCHEMA_CODES) <= {item.id for item in ctx.skipped}
    assert not ctx.issues
    path = _scan(tmp_path, [(URL, "<html amp><body>unparsed</body></html>")], parsed=False)
    con = open_scan(path, require_audit=False)
    try:
        ctx = _ctx(con)
        check_document_markup(ctx)
        assert not ctx.issues
        assert set(A11Y_CODES) | set(AMP_CODES) <= {item.id for item in ctx.skipped}
    finally:
        con.close()


def test_markup_envelope_rejects_wrong_binding_and_invalid_counts(tmp_path):
    path = _scan(tmp_path, [(URL, "<html><body><input></body></html>")])
    con = open_scan(path, require_audit=False)
    try:
        row = dict(
            con.execute("SELECT * FROM context_items WHERE kind='markup_evidence'").fetchone()
        )
        payload = json.loads(row["payload_json"])
        validate_context(con, row, payload)
        payload["source_document_id"] += 100
        with pytest.raises(ScanError, match="wrong document"):
            validate_context(con, row, payload)
        payload = json.loads(row["payload_json"])
        payload["facts"]["accessibility"]["issues"][0]["count"] = False
        with pytest.raises(ScanError, match="issue is invalid"):
            validate_context(con, row, payload)
    finally:
        con.close()


@pytest.mark.parametrize("format", ["microdata", "rdfa"])
def test_html_schema_formats_are_retained_as_validation_not_absent(tmp_path, format):
    if format == "microdata":
        html = '<div itemscope itemtype="https://schema.org/Product"><span itemprop="inventedProperty">bad</span></div>'
    else:
        html = '<div vocab="https://schema.org/" typeof="Product"><span property="inventedProperty">bad</span></div>'
    payload = structured_payload(
        page_url_id=1,
        source_document_id=1,
        representation="static",
        parsed=parse_html(html, URL),
        html=html,
    )
    assert payload["state"] == "structurally_invalid"
    path = _scan(tmp_path, [(URL, html)])
    con = open_scan(path, require_audit=False)
    try:
        ctx = _ctx(con)
        check_document_markup(ctx)
        fired = {issue.check for issue in ctx.issues}
        assert "STRUCTURED_DATA_SCHEMA_ERROR" in fired
        assert "STRUCTURED_DATA_RICH_RESULT_REQUIRED_MISSING" in fired
    finally:
        con.close()


@pytest.mark.parametrize(
    "property_iri",
    [
        "schema.org/name",
        "https://schema.org.example.test/name",
        "https://example.test/schema.org/name",
        "https://schema.org@example.test/name",
    ],
)
def test_unrelated_property_iri_does_not_establish_schema_rdfa(property_iri):
    html = f'<span property="{property_iri}">Example</span>'
    payload = structured_payload(
        page_url_id=1,
        source_document_id=1,
        representation="static",
        parsed=parse_html(html, URL),
        html=html,
    )
    assert payload["state"] == "absent"


@pytest.mark.parametrize("property_iri", ["https://schema.org/name", "schema:name"])
def test_schema_property_without_a_subject_is_invalid_not_absent(property_iri):
    html = f'<span property="{property_iri}">Example</span>'
    payload = structured_payload(
        page_url_id=1,
        source_document_id=1,
        representation="static",
        parsed=parse_html(html, URL),
        html=html,
    )
    assert payload["state"] == "structurally_invalid"


def test_rdfa_graph_is_retained_as_native_validation():
    html = """
    <div vocab="https://schema.org/" typeof="Product" about="#product">
      <span property="name">Widget</span>
      <a rel="offers" href="#offer">Offer</a>
      <div about="#offer" typeof="Offer">
        <meta property="price" content="19.99">
        <meta property="priceCurrency" content="USD">
      </div>
    </div>"""
    payload = structured_payload(
        page_url_id=1,
        source_document_id=1,
        representation="static",
        parsed=parse_html(html, URL),
        html=html,
    )
    assert payload["state"] == "valid"
    assert payload["validation"]["structural_state"] == "valid"
    assert payload["validation"]["feature_eligibility_state"] == "locally_checked"
    assert payload["validation"]["feature_checks"][0]["rules_version"]
    assert set(payload["validation"]) == {
        "syntax_state",
        "structural_state",
        "feature_eligibility_state",
        "unsupported_context_normalization",
        "entity_errors",
        "entity_warnings",
        "feature_checks",
    }


def test_retained_rdfa_property_copy_keeps_the_v2_validation_shape():
    from pathlib import Path

    html = (Path(__file__).parent / "fixtures" / "rdfa_copy_graph.html").read_text("utf-8")
    payload = structured_payload(
        page_url_id=1,
        source_document_id=1,
        representation="static",
        parsed=parse_html(html, URL),
        html=html,
    )
    assert payload["state"] == "valid"
    assert payload["validation"]["structural_state"] == "valid"
    assert payload["validation"]["feature_checks"] == []
    assert set(payload["validation"]) == {
        "syntax_state",
        "structural_state",
        "feature_eligibility_state",
        "unsupported_context_normalization",
        "entity_errors",
        "entity_warnings",
        "feature_checks",
    }


def test_schema_warning_is_a_native_finding(tmp_path):
    html = '<script type="application/ld+json">{"@type":"Product","name":"A","brand":"Brand literal"}</script>'
    path = _scan(tmp_path, [(URL, html)])
    con = open_scan(path, require_audit=False)
    try:
        ctx = _ctx(con)
        check_document_markup(ctx)
        assert "STRUCTURED_DATA_SCHEMA_WARNING" in {issue.check for issue in ctx.issues}
    finally:
        con.close()


def test_one_missing_legacy_context_prevents_clean_sibling_pass(tmp_path):
    html = '<html lang="en"><body><main>Healthy</main></body></html>'
    path = _scan(tmp_path, [(URL, html), (AMP, html)])
    # Simulate an older native capture with no markup lane, preserving SQL shape.
    import sqlite3

    writer = sqlite3.connect(path)
    writer.execute(
        "DELETE FROM context_items WHERE kind='markup_evidence' AND item_key LIKE 'page:2:%'"
    )
    writer.commit()
    writer.close()
    con = open_scan(path, require_audit=False)
    try:
        ctx = _ctx(con)
        check_document_markup(ctx)
        assert set(A11Y_CODES) <= {item.id for item in ctx.skipped}
        assert not ctx.issues
    finally:
        con.close()


def test_native_defective_markup_fires_all_implemented_checks(tmp_path):
    html = '<html amp><div><h1>One</h1><h3>Three</h3><input><img src="x"><script type="application/ld+json">{"@type":"Product","brand":"literal","bogus":"bad"}</script></div></html>'
    path = _scan(tmp_path, [(AMP, html)])
    con = open_scan(path, require_audit=False)
    try:
        ctx = _ctx(con)
        check_document_markup(ctx)
        fired = {issue.check for issue in ctx.issues}
        assert {
            "AMP_INVALID_DOCTYPE",
            "AMP_MISSING_HEAD",
            "AMP_MISSING_BODY",
            "AMP_INVALID_CHARSET",
            "AMP_INVALID_VIEWPORT",
            "AMP_INVALID_RUNTIME",
            "AMP_INVALID_BOILERPLATE",
            "A11Y_HTML_LANG_MISSING",
            "A11Y_FORM_LABEL_MISSING",
            "A11Y_IMAGE_ALT_MISSING",
            "A11Y_HEADING_ORDER",
            "A11Y_MAIN_LANDMARK_MISSING",
            "STRUCTURED_DATA_SCHEMA_ERROR",
            "STRUCTURED_DATA_SCHEMA_WARNING",
            "STRUCTURED_DATA_RICH_RESULT_REQUIRED_MISSING",
        } <= fired
    finally:
        con.close()


@pytest.mark.parametrize("newer_document", [False, True])
def test_missing_latest_static_amp_context_never_inherits_sibling_or_older_pass(
    tmp_path, newer_document
):
    import sqlite3

    from tests.test_markup import amp_document

    # A valid sibling isolates omitted evidence; a malformed sibling already
    # fires checks, which cannot also occupy the mutually exclusive skip bucket.
    html = amp_document()
    path = _scan(tmp_path, [(URL, html), (AMP, html)])
    writer = sqlite3.connect(path)
    writer.row_factory = sqlite3.Row
    if newer_document:
        document = dict(writer.execute("SELECT * FROM documents WHERE url_id=2").fetchone())
        document.pop("document_id")
        writer.execute(
            "INSERT INTO documents ("
            + ",".join(document)
            + ") VALUES ("
            + ",".join("?" for _ in document)
            + ")",
            tuple(document.values()),
        )
    else:
        writer.execute(
            "DELETE FROM context_items WHERE kind='markup_evidence' AND item_key LIKE 'page:2:%'"
        )
    writer.commit()
    writer.close()
    con = open_scan(path, require_audit=False)
    try:
        ctx = _ctx(con, targets=True)
        check_document_markup(ctx)
        assert set(AMP_CODES) | {"AMP_MISSING_HTML_ATTRIBUTE"} <= {item.id for item in ctx.skipped}
        assert all(issue.target_url != AMP for issue in ctx.issues)
    finally:
        con.close()


def test_native_healthy_markup_stays_silent_for_all_implemented_checks(tmp_path):
    html = """<!doctype html><html amp lang="en"><head><meta charset="utf-8">
      <meta name="viewport" content="width=device-width"><script async src="https://cdn.ampproject.org/v0.js"></script>
      <style amp-boilerplate>body{visibility:hidden}</style><noscript><style amp-boilerplate>body{visibility:visible}</style></noscript>
      <script type="application/ld+json">{"@type":"Product","name":"Fixture","offers":{"@type":"Offer","price":"10","priceCurrency":"USD"}}</script>
      </head><body><main><h1>Title</h1><h2>Second</h2><label for="name">Name</label><input id="name"><img src="x" alt="Fixture"></main></body></html>"""
    path = _scan(tmp_path, [(AMP, html)])
    con = open_scan(path, require_audit=False)
    try:
        ctx = _ctx(con, targets=True)
        check_document_markup(ctx)
        fired = {issue.check for issue in ctx.issues}
        assert not (
            {
                "AMP_INVALID_DOCTYPE",
                "AMP_MISSING_HEAD",
                "AMP_MISSING_BODY",
                "AMP_INVALID_CHARSET",
                "AMP_INVALID_VIEWPORT",
                "AMP_INVALID_RUNTIME",
                "AMP_INVALID_BOILERPLATE",
                "AMP_MISSING_HTML_ATTRIBUTE",
                "A11Y_HTML_LANG_MISSING",
                "A11Y_FORM_LABEL_MISSING",
                "A11Y_IMAGE_ALT_MISSING",
                "A11Y_HEADING_ORDER",
                "A11Y_MAIN_LANDMARK_MISSING",
                "STRUCTURED_DATA_SCHEMA_ERROR",
                "STRUCTURED_DATA_SCHEMA_WARNING",
                "STRUCTURED_DATA_RICH_RESULT_REQUIRED_MISSING",
            }
            & fired
        )
    finally:
        con.close()


@pytest.mark.parametrize(
    "html",
    [
        '<div vocab="https://schema.org/"><span property="name">Orphan</span></div>',
        '<div vocab="https://schema.org/" typeof="Article"><span property="bogus">bad</span><a rel="author" href="#ada">Ada</a></div>',
    ],
)
def test_invalid_rdfa_never_becomes_absent_and_proven_errors_survive(tmp_path, html):
    path = _scan(tmp_path, [(URL, html)])
    con = open_scan(path, require_audit=False)
    try:
        payload = json.loads(
            con.execute(
                "SELECT payload_json FROM context_items WHERE kind='structured_evidence'"
            ).fetchone()[0]
        )
        assert payload["state"] == "structurally_invalid"
        assert payload["reason"] == "Schema.org graph validation found structural errors"
        assert payload["validation"]["structural_state"] == "structurally_invalid"
        assert payload["validation"]["entity_errors"] > 0
        ctx = _ctx(con)
        check_document_markup(ctx)
        fired = {issue.check for issue in ctx.issues}
        if "bogus" in html:
            assert "STRUCTURED_DATA_SCHEMA_ERROR" in fired
        assert "STRUCTURED_DATA_RICH_RESULT_REQUIRED_MISSING" not in fired
    finally:
        con.close()


def test_foreign_sibling_does_not_hide_confirmed_schema_error(tmp_path):
    html = '<script type="application/ld+json">{"@type":"Article","bogus":"bad"}</script><div vocab="https://example.test/vocab/" typeof="Thing"></div>'
    path = _scan(tmp_path, [(URL, html)])
    con = open_scan(path, require_audit=False)
    try:
        ctx = _ctx(con)
        check_document_markup(ctx)
        assert "STRUCTURED_DATA_SCHEMA_ERROR" in {issue.check for issue in ctx.issues}
    finally:
        con.close()


@pytest.mark.parametrize("source", ["null", "1", "{}", "[]"])
def test_syntactically_parsed_non_entity_json_has_native_schema_error(tmp_path, source):
    path = _scan(tmp_path, [(URL, '<script type="application/ld+json">' + source + "</script>")])
    con = open_scan(path, require_audit=False)
    try:
        ctx = _ctx(con)
        check_document_markup(ctx)
        assert "STRUCTURED_DATA_SCHEMA_ERROR" in {issue.check for issue in ctx.issues}
    finally:
        con.close()
