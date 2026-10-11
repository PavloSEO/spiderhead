"""Native image semantics and persisted evidence from real retained capture contracts."""

from __future__ import annotations

import hashlib
import sqlite3

import pytest
from bs4 import BeautifulSoup

from seohead.checks.parser import extract_images
from seohead.crawl.settings import load
from seohead.crawl.sqlite_adapter import crawl_to_scan
from seohead.sf.config import load_config
from seohead.sf.core.context import AuditContext
from seohead.sf.core.loader import LoadedExports
from seohead.sf.core.rules import (
    check_native_image_resources,
    check_native_image_size,
    check_retained_image_declarations,
)
from seohead.storage import open_scan
from seohead.storage.audit_v2 import AuditV2Reader
from seohead.storage.native_scan import NativeScan
from seohead.storage.url_query import scan_url_query
from tests.test_batch_resource_graph import _Response
from tests.test_scan_reanalysis_integration import _runtime_versions, _save_native_audit

ROOT = "https://example.test"


def _capture(path, body, *, overrides=None, extra=None):
    settings = load(
        overrides={
            "speed.min_delay_seconds": 0,
            "limits.max_urls": 4,
            "robots.policy": "ignore",
            "storage.format_version": "scan.v2",
            "resources.fetch": True,
            "resources.max_requests": 0,
            **(overrides or {}),
        }
    )
    content = (
        "<!doctype html><html><head><title>Images</title></head><body>" + body + "</body></html>"
    ).encode()
    responses = {
        ROOT + "/": _Response(200, content, {"content-type": "text/html; charset=utf-8"}),
        ROOT + "/shared.png": _Response(200, b"small image", {"content-type": "image/png"}),
        **(extra or {}),
    }
    crawl_to_scan(
        ROOT + "/",
        scan_out=str(path),
        settings=settings,
        producer_version="3.0.0",
        producer_revision="a" * 40,
        runtime_versions=_runtime_versions(),
        fetcher=responses.__getitem__,
        sleeper=lambda _: None,
    )
    return settings


def _run(path):
    ctx = AuditContext(LoadedExports(), load_config(None), disk_backed_pages=True)
    ctx.scan_con = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    ctx.scan_con.row_factory = sqlite3.Row
    check_retained_image_declarations(ctx)
    check_native_image_resources(ctx)
    check_native_image_size(ctx)
    return ctx


def _close(ctx):
    ctx.scan_con.close()
    ctx.close()


@pytest.mark.parametrize(
    "markup,missing_attribute,missing_text,missing_dimensions",
    [
        ('<img src="/shared.png" alt="" width="1" height="1">', False, False, False),
        ('<img src="/shared.png" alt="  " width="1" height="1">', False, True, False),
        ('<img src="/shared.png" role="img" alt="" width="1" height="1">', False, True, False),
        (
            '<a href="/next"><img src="/shared.png" alt="" width="1" height="1"></a>',
            False,
            True,
            False,
        ),
        (
            '<a href="/next" aria-label="Next"><img src="/shared.png" alt="" width="1" height="1"></a>',
            False,
            False,
            False,
        ),
        (
            '<a href="/next">Next<img src="/shared.png" alt="" width="1" height="1"></a>',
            False,
            False,
            False,
        ),
        ('<img src="/shared.png" aria-label="Named" width="1" height="1">', True, False, False),
        ('<img src="/shared.png" role="presentation">', True, False, True),
        ('<img src="/shared.png" alt="" aria-hidden="true">', False, False, True),
        ('<img src="/shared.png" alt="" hidden>', False, False, False),
        ('<img src="/shared.png" alt="named" width="0" height="10">', False, False, True),
        ('<img src="/shared.png" alt="named" width="100%" height="10">', False, False, True),
    ],
)
def test_image_attribute_semantics(markup, missing_attribute, missing_text, missing_dimensions):
    facts = extract_images(BeautifulSoup(markup, "lxml"), base_url=ROOT + "/")
    assert len(facts) == 1
    assert facts[0]["missing_alt_attribute"] == missing_attribute
    assert facts[0]["missing_alt_text"] == missing_text
    assert bool(facts[0]["missing_dimensions"]) == missing_dimensions


@pytest.mark.parametrize(
    "label,named",
    [
        ('<span id="name">Image label</span>', True),
        ('<span id="name">   </span>', False),
        ('<span id="name">One</span><span id="name">Two</span>', False),
        ('<script id="name">Not an accessible label</script>', False),
    ],
)
def test_aria_names_do_not_replace_factual_alt_attribute(label, named):
    html = (
        label
        + '<img src="/shared.png" role="img" alt="" aria-labelledby="name" width="1" height="1">'
    )
    fact = extract_images(BeautifulSoup(html, "lxml"), base_url=ROOT + "/")[0]
    assert fact["missing_alt_text"] is not named
    assert fact["missing_alt_attribute"] is False


def test_retained_per_image_findings_have_primitive_evidence(tmp_path):
    path = tmp_path / "scan.sqlite"
    _capture(path, '<img src="/shared.png"><img src="/shared.png" role="img" alt="">')
    with open_scan(path, require_audit=False) as con:
        assert con.execute("SELECT COUNT(*) FROM pages").fetchone()[0] == 1
    ctx = _run(path)
    try:
        codes = {issue.check for issue in ctx.issues}
        assert {"IMG_MISSING_ALT_ATTRIBUTE", "IMG_MISSING_ALT", "IMG_MISSING_DIMENSIONS"} <= codes
        for issue in ctx.issues:
            assert issue.target_url == ROOT + "/shared.png"
            assert issue.status_code == 200
            assert issue.details["source_page"] == ROOT + "/"
            assert issue.details["reason"]
            assert issue.details["document_id"] > 0
            assert issue.locations[0]["link_path"].startswith("img[")
    finally:
        _close(ctx)


def test_known_page_count_survives_no_retained_body(tmp_path):
    path = tmp_path / "scan.sqlite"
    _capture(
        path,
        '<img src="/shared.png"><img alt="' + "x" * 150 + '" src="/shared.png">',
        overrides={"storage.body_mode": "off"},
    )
    ctx = _run(path)
    try:
        issues = [
            i for i in ctx.issues if i.check in {"IMG_MISSING_ALT_ATTRIBUTE", "IMG_ALT_TOO_LONG"}
        ]
        assert {i.check for i in issues} == {"IMG_MISSING_ALT_ATTRIBUTE", "IMG_ALT_TOO_LONG"}
        for i in issues:
            assert i.target_url == ROOT + "/" and i.status_code == 200
            assert i.details["target_scope"] == "page_aggregate"
            assert i.details["resource_url"] is None and i.details["resource_status_code"] is None
            assert i.details["resource_evidence_state"] == "unavailable"
    finally:
        _close(ctx)


def test_image_limits_preserve_partial_and_known_positive_counts(tmp_path):
    path = tmp_path / "scan.sqlite"
    body = '<img src="/shared.png" alt="" width="1" height="1">' * 2000 + '<img src="/shared.png">'
    _capture(path, body)
    ctx = _run(path)
    try:
        assert ctx.asset_measurements["IMG_MISSING_DIMENSIONS"]["state"] == "partial"
        fallback = [i for i in ctx.issues if i.check == "IMG_MISSING_ALT_ATTRIBUTE"]
        assert len(fallback) == 1 and fallback[0].details["target_scope"] == "page_aggregate"
        assert fallback[0].occurrences_count == 1
    finally:
        _close(ctx)


def test_reopened_native_queries_keep_all_sources_and_resource_evidence(tmp_path, monkeypatch):
    path = tmp_path / "scan.sqlite"
    settings = _capture(
        path,
        '<a href="/other">Other</a><img src="/shared.png">',
        extra={
            ROOT + "/other": _Response(
                200,
                b'<html><body><img src="/shared.png"></body></html>',
                {"content-type": "text/html"},
            ),
            ROOT + "/shared.png": _Response(404, b"gone", {"content-type": "image/png"}),
        },
    )
    # Exercise the same native audit.v2 publication contract used for large runs.
    monkeypatch.setattr(NativeScan, "save_audit", NativeScan._save_audit_v2)
    _save_native_audit(path, settings, start_url=ROOT + "/")
    before = hashlib.sha256(path.read_bytes()).hexdigest()
    with AuditV2Reader(path) as reader:
        issues = [
            row for row in reader.iter_collection("/issues") if row["check"].startswith("IMG_")
        ]
    broken = [i for i in issues if i["check"] == "IMG_BROKEN"]
    assert len(broken) == 2 and len({i["fingerprint"] for i in broken}) == 2
    for issue in broken:
        assert issue["status_code"] == 404 and issue["details"]["status_scope"] == "resource"
        observations = issue["evidence"]["contract"]["observations"]
        assert any(
            r["source_table"] == "resource_graph_fetches" and r["role"] == "target_resource"
            for r in observations
        )
        assert any(r["source_table"] == "resource_graph_occurrences" for r in observations)
    for check in ("IMG_BROKEN", "IMG_MISSING_ALT_ATTRIBUTE", "IMG_MISSING_DIMENSIONS"):
        result = scan_url_query(
            str(path), issue_check=[check], columns=["url", "status_code"], limit=20
        )
        assert result["ok"] and {row["url"] for row in result["rows"]} == {
            ROOT + "/",
            ROOT + "/other",
        }
    assert hashlib.sha256(path.read_bytes()).hexdigest() == before


def test_unmeasured_resource_status_is_not_substituted_from_page(tmp_path):
    path = tmp_path / "scan.sqlite"
    _capture(path, '<img src="/shared.png">', overrides={"resources.fetch": False})
    ctx = _run(path)
    try:
        issue = next(i for i in ctx.issues if i.check == "IMG_MISSING_ALT_ATTRIBUTE")
        assert issue.target_url == ROOT + "/shared.png" and issue.status_code is None
        assert issue.details["source_status_code"] == 200
        assert issue.details["resource_evidence_state"] == "unavailable"
    finally:
        _close(ctx)
