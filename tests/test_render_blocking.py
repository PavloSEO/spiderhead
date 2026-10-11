"""Render Blocking Requests as a native crawl finding (issue #1022, slice S1).

No network: the head is read from the HTML the crawl already has.
"""

from __future__ import annotations

import csv
import json

import pandas as pd
import pytest

from seohead.checks.parser import parse_html
from seohead.sf.config import load_config
from seohead.sf.core.audit import run_audit
from seohead.sf.core.context import AuditContext
from seohead.sf.core.loader import LoadedExports
from seohead.sf.core.rules import check_render_blocking
from seohead.storage.native_scan import NativeScan
from tests.test_native_capture import _claim, _event, _renderer
from tests.test_scan_native import _metadata, _record, _runtime

TITLE = "A descriptive page title with sufficient length"
DESC = "A meta description deliberately longer than seventy characters to clear the validation threshold."

HEAD_HTML = """<!doctype html><html><head>
<script src="/a.js"></script>
<script src="/b.js" defer></script>
<script type="module" src="/m.js"></script>
<link rel="stylesheet" href="/c.css">
<link rel="stylesheet" href="/print.css" media="print">
<template><script src="/t.js"></script></template>
</head><body><p>x</p></body></html>"""


def test_parse_html_lists_only_blocking_head_resources():
    parsed = parse_html(HEAD_HTML, "https://example.com/page")
    assert parsed["render_blocking"] == [
        "https://example.com/a.js",
        "https://example.com/c.css",
    ]


@pytest.mark.parametrize("rel", ["stylesheet preload", "stylesheet custom", "STYLESHEET\tpreload"])
def test_raw_multitoken_stylesheet_rel_remains_render_blocking(rel):
    html = f'<html><head><link rel="{rel}" href="/style.css"></head></html>'
    parsed = parse_html(html, "https://example.com/")
    assert parsed["render_blocking"] == ["https://example.com/style.css"]


def test_unstored_native_render_blocking_survives_jsonl_projection():
    from dataclasses import asdict

    from seohead.crawl.collect import CrawlResult, PageRecord, _record_from_parsed
    from seohead.crawl.evidence import build_evidence

    parsed = parse_html(HEAD_HTML, "https://example.com/page")
    record = PageRecord(
        url="https://example.com/page",
        status_code=200,
        content_type="text/html",
        **_record_from_parsed(parsed),
    )
    restored = PageRecord(**json.loads(json.dumps(asdict(record))))
    evidence = build_evidence(CrawlResult(pages=[restored]))
    ctx = AuditContext(LoadedExports(frames=evidence["frames"]), load_config(None))
    assert ctx.scan_con is None
    check_render_blocking(ctx)
    issue = next(i for i in ctx.issues if i.check == "RENDER_BLOCKING")
    assert issue.details["resources"] == parsed["render_blocking"]
    assert ctx.asset_measurements["RENDER_BLOCKING"]["state"] == "complete"


def test_parse_html_without_head_resources_is_empty():
    parsed = parse_html("<html><head></head><body>x</body></html>", "https://example.com/")
    assert parsed["render_blocking"] == []


COLS = [
    "Address",
    "Content Type",
    "Status Code",
    "Status",
    "Indexability",
    "Title 1",
    "Meta Description 1",
    "H1-1",
    "Canonical Link Element 1",
    "Meta Robots 1",
    "Size (bytes)",
]


def _run(tmp_path, *, extra_cols=(), rows=()):
    d = tmp_path / "exports"
    d.mkdir()
    cols = COLS + list(extra_cols)
    with open(d / "internal_all.csv", "w", encoding="utf-8-sig", newline="") as fh:
        w = csv.writer(fh)
        w.writerow(cols)
        for url, extra in rows:
            base = [
                url,
                "text/html",
                "200",
                "OK",
                "Indexable",
                TITLE,
                DESC,
                "H",
                url,
                "index,follow",
                "5000",
            ]
            w.writerow(base + list(extra))
    return run_audit(input_mode="parse-exports", exports_dir=str(d), log=lambda m: None)


def test_render_blocking_skips_without_column(tmp_path):
    res = _run(tmp_path, rows=[("https://example.com/", [])])
    assert "RENDER_BLOCKING" in {s.id for s in res.skipped}
    assert not [i for i in res.issues if i.check == "RENDER_BLOCKING"]


def test_render_blocking_fires_per_page_with_urls(tmp_path):
    res = _run(
        tmp_path,
        extra_cols=["Render Blocking Resources"],
        rows=[
            ("https://example.com/bad", ["https://example.com/a.css\nhttps://example.com/a.js"]),
            ("https://example.com/good", [""]),
        ],
    )
    fired = [i for i in res.issues if i.check == "RENDER_BLOCKING"]
    assert {i.target_url for i in fired} == {"https://example.com/bad"}


def test_render_blocking_survives_disabled_metadata():
    parsed = parse_html(HEAD_HTML, "https://example.com/page", {"meta": False})
    assert parsed["render_blocking"] == ["https://example.com/a.js", "https://example.com/c.css"]


def test_native_retained_head_runs_without_changing_frozen_scan_schema(tmp_path):
    from seohead.sf.config import load_config
    from seohead.sf.core.context import AuditContext
    from seohead.sf.core.loader import LoadedExports
    from seohead.sf.core.rules import check_render_blocking
    from seohead.storage import open_scan
    from tests.test_scan_reanalysis_integration import _source

    path = tmp_path / "scan.sqlite"
    _source(path)
    with open_scan(path) as con:
        assert "render_blocking" not in {r[1] for r in con.execute("PRAGMA table_info(pages)")}
        ctx = AuditContext(LoadedExports(), load_config(None))
        ctx.scan_con = con
        check_render_blocking(ctx)
        issue = next(i for i in ctx.issues if i.check == "RENDER_BLOCKING")
        assert issue.target_url == "https://example.test/"
        assert issue.details["resources"] == ["https://example.test/app.js"]


def test_native_scan_ignores_legacy_projection_and_uses_retained_head(tmp_path):
    path = tmp_path / "scan.sqlite"
    html = '<html><head><script src="/current.js"></script></head></html>'
    with NativeScan.create(path, **_metadata()) as scan:
        lease = _claim(scan)
        record = _record()
        record.update(status_code=200, render_blocking=URL + "obsolete.js")
        scan.commit_page(
            lease,
            record,
            captures=[_event(URL, html.encode())],
            runtime=_runtime(),
            resource_inventory_state="complete",
        )
        assert "render_blocking" not in {r[1] for r in scan.con.execute("PRAGMA table_info(pages)")}
        ctx = _retained_context(scan)
        issue = next(i for i in ctx.issues if i.check == "RENDER_BLOCKING")
        assert issue.details["resources"] == [URL + "current.js"]


def test_native_unretained_body_is_unavailable_not_clean(tmp_path):
    from seohead.sf.config import load_config
    from seohead.sf.core.context import AuditContext
    from seohead.sf.core.loader import LoadedExports
    from seohead.sf.core.rules import check_render_blocking
    from seohead.storage import open_scan
    from tests.test_scan_reanalysis_integration import _source

    path = tmp_path / "scan.sqlite"
    _source(path, body_mode="off")
    with open_scan(path, require_audit=False) as con:
        ctx = AuditContext(LoadedExports(), load_config(None))
        ctx.scan_con = con
        check_render_blocking(ctx)
        assert not ctx.issues
        reason = next(i.reason for i in ctx.skipped if i.id == "RENDER_BLOCKING")
        assert "native crawl" in reason and "HTML" in reason
        assert ctx.asset_measurements["RENDER_BLOCKING"]["state"] == "unavailable"
        assert ctx.asset_measurements["RENDER_BLOCKING"]["unavailable"] == 1


URL = "https://example.test/"


def _retained_context(scan):
    ctx = AuditContext(LoadedExports(), load_config(None))
    ctx.scan_con = scan.con
    check_render_blocking(ctx)
    return ctx


def _commit_html(scan, html, *, url=URL, status=200, no_store=False):
    lease = _claim(scan, url)
    record = _record(url)
    record.update(status_code=status, size_bytes=len(html.encode()))
    changes = {"status_code": status, "effective_status_code": status}
    if no_store:
        changes.update(
            response_headers=(("content-type", "text/html"), ("cache-control", "no-store")),
            effective_headers=(("content-type", "text/html"), ("cache-control", "no-store")),
        )
    scan.commit_page(
        lease,
        record,
        captures=[_event(url, html.encode(), **changes)],
        runtime=_runtime(),
        resource_inventory_state="complete",
    )


def test_retained_render_blocking_uses_the_current_head_after_requeue(tmp_path):
    from seohead.storage.retry import requeue_scan

    path = tmp_path / "retry.sqlite"
    with NativeScan.create(path, **_metadata()) as scan:
        _commit_html(scan, '<html><head><script src="/old.js"></script></head></html>')
        scan.finish_without_audit("retained head fixture")
    backup = tmp_path / "before-requeue.sqlite"
    requeue_scan(path, where='url = "https://example.test/"', backup_path=backup)
    assert backup.exists()
    with NativeScan.open(path) as scan:
        _commit_html(scan, "<html><head></head><body>fixed</body></html>")
        assert scan.con.execute("SELECT COUNT(*) FROM documents").fetchone()[0] == 2
        ctx = _retained_context(scan)
        assert ctx.issues == []
        assert ctx.skipped == []
        assert ctx.asset_measurements["RENDER_BLOCKING"] == {
            "unit": "pages",
            "eligible": 1,
            "measured": 1,
            "partial": 0,
            "unavailable": 0,
            "state": "complete",
            "reason": "",
        }


@pytest.mark.parametrize("status", [301, 404, 500])
def test_retained_and_export_render_blocking_exclude_error_and_redirect_html(tmp_path, status):
    html = '<html><head><script src="/app.js"></script></head></html>'
    with NativeScan.create(tmp_path / "status.sqlite", **_metadata()) as scan:
        _commit_html(scan, html, status=status)
        native = _retained_context(scan)
    exports = LoadedExports()
    exports.frames["internal_all"] = pd.DataFrame(
        [
            {
                "Address": URL,
                "Content Type": "text/html",
                "Status Code": status,
                "Render Blocking Resources": json.dumps([URL + "app.js"]),
            }
        ]
    )
    supplied = AuditContext(exports, load_config(None))
    check_render_blocking(supplied)
    for ctx in (native, supplied):
        assert ctx.issues == []
        assert ctx.asset_measurements["RENDER_BLOCKING"]["eligible"] == 0
        assert ctx.asset_measurements["RENDER_BLOCKING"]["state"] == "unavailable"


def test_rendered_page_uses_its_active_static_head_after_requeue(tmp_path):
    from seohead.storage.retry import requeue_scan

    path = tmp_path / "rendered-retry.sqlite"
    with NativeScan.create(path, **_metadata()) as scan:
        _commit_html(scan, '<html><head><script src="/old.js"></script></head></html>')
        scan.finish_without_audit("retained head fixture")
    requeue_scan(
        path, where='url = "https://example.test/"', backup_path=tmp_path / "before.sqlite"
    )
    with NativeScan.open(path) as scan:
        _commit_html(scan, '<html><head><script src="/current.js"></script></head></html>')
        scan.commit_render(
            URL,
            {**_record(URL), "representation": "rendered"},
            html='<html><head><script src="/rendered-only.js"></script></head></html>',
            renderer=_renderer(URL),
            captured_at="2026-10-11T12:00:00Z",
        )
        ctx = _retained_context(scan)
        assert len(ctx.issues) == 1
        assert ctx.issues[0].details["resources"] == [URL + "current.js"]
        assert ctx.issues[0].status_code == 200
        assert ctx.asset_measurements["RENDER_BLOCKING"]["measured"] == 1


def test_missing_retained_head_preserves_partial_scope_when_another_page_fires(tmp_path):
    with NativeScan.create(tmp_path / "partial.sqlite", **_metadata()) as scan:
        _commit_html(scan, '<html><head><script src="/app.js"></script></head></html>')
        _commit_html(scan, "<html><head></head></html>", url=URL + "private", no_store=True)
        ctx = _retained_context(scan)
        assert len(ctx.issues) == 1
        assert ctx.issues[0].status_code == 200
        metrics = ctx.asset_measurements["RENDER_BLOCKING"]
        assert metrics["eligible"] == 2
        assert metrics["measured"] == metrics["unavailable"] == 1
        assert metrics["state"] == "partial" and metrics["reason"]


def test_head_analysis_budget_does_not_treat_an_oversized_retained_body_as_clean(tmp_path):
    with NativeScan.create(tmp_path / "oversized.sqlite", **_metadata()) as scan:
        _commit_html(scan, "<html><head></head><body>" + "x" * (2 * 1024 * 1024) + "</body></html>")
        ctx = _retained_context(scan)
        assert ctx.issues == []
        assert {item.id for item in ctx.skipped} == {"RENDER_BLOCKING"}
        assert ctx.asset_measurements["RENDER_BLOCKING"]["unavailable"] == 1


@pytest.mark.parametrize(("count", "tail"), [(205, ""), (80, "é" * 600)])
def test_retained_render_blocking_resource_samples_obey_count_and_byte_budgets(
    tmp_path, count, tail
):
    html = (
        "<html><head>"
        + "".join(f'<script src="/{index}{tail}.js"></script>' for index in range(count))
        + "</head></html>"
    )
    with NativeScan.create(tmp_path / "samples.sqlite", **_metadata()) as scan:
        _commit_html(scan, html)
        ctx = _retained_context(scan)
        issue = ctx.issues[0]
        sample = issue.details["resources"]
        assert len(sample) <= 200
        assert len(json.dumps(sample, ensure_ascii=True)) <= 64 * 1024
        assert issue.details["resources_observed"] == count
        assert issue.details["resources_omitted"] == count - len(sample) > 0
        assert issue.status_code == 200
        assert ctx.asset_measurements["RENDER_BLOCKING"]["partial"] == 1
        assert ctx.asset_measurements["RENDER_BLOCKING"]["state"] == "partial"


def test_export_render_blocking_missing_page_measurement_is_not_clean():
    exports = LoadedExports()
    exports.frames["internal_all"] = pd.DataFrame(
        [
            {
                "Address": URL,
                "Content Type": "text/html",
                "Status Code": 200,
                "Render Blocking Resources": "[]",
            },
            {
                "Address": URL + "missing",
                "Content Type": "text/html",
                "Status Code": 200,
                "Render Blocking Resources": None,
            },
        ]
    )
    ctx = AuditContext(exports, load_config(None))
    check_render_blocking(ctx)
    assert ctx.issues == []
    assert {item.id for item in ctx.skipped} == {"RENDER_BLOCKING"}
    metrics = ctx.asset_measurements["RENDER_BLOCKING"]
    assert metrics["eligible"] == 2 and metrics["measured"] == 1
    assert metrics["unavailable"] == 1 and metrics["state"] == "partial"


def test_export_render_blocking_bounded_samples_keep_status_and_counts():
    urls = [URL + str(index) + "é" * 600 + ".js" for index in range(80)]
    exports = LoadedExports()
    exports.frames["internal_all"] = pd.DataFrame(
        [
            {
                "Address": URL,
                "Content Type": "text/html",
                "Status Code": 201,
                "Render Blocking Resources": json.dumps(urls),
            }
        ]
    )
    ctx = AuditContext(exports, load_config(None))
    check_render_blocking(ctx)
    issue = ctx.issues[0]
    assert issue.status_code == 201
    assert len(json.dumps(issue.details["resources"], ensure_ascii=True)) <= 64 * 1024
    assert issue.details["resources_omitted"] == len(urls) - len(issue.details["resources"])
    assert ctx.asset_measurements["RENDER_BLOCKING"]["state"] == "partial"
