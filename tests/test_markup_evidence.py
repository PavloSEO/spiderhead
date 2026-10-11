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
        skipped = {item.id: item.reason for item in ctx.skipped}
        assert "licence" in skipped["AMP_VALIDATOR_ERRORS"]
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


def test_partial_rdfa_is_unavailable_not_valid():
    html = '<div vocab="https://schema.org/" typeof="Thing"><a rel="url" href="/x">X</a></div>'
    payload = structured_payload(
        page_url_id=1,
        source_document_id=1,
        representation="static",
        parsed=parse_html(html, URL),
        html=html,
    )
    assert payload["state"] == "unavailable"
    assert payload["validation"]["structural_state"] == "unavailable"


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
