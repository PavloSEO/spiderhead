"""NativeScan acceptance coverage for retained-body comparisons."""

from __future__ import annotations

import json
import sqlite3

from seohead.crawl.sqlite_resources import capture_resources
from seohead.storage.body_diff import body_diff
from seohead.storage.corpus import store_response
from seohead.storage.native_scan import NativeScan
from tests.test_scan_native import _record, _runtime
from tests.test_scan_resource_integration import (
    _event,
    _metadata_for_resources,
    _rendered_record,
    _renderer,
    _Response,
    _settings,
)

PAGE_URL = "https://example.test/"
RESOURCE_URL = "https://example.test/app.js"


def _resource_scan(
    path, *, body: bytes, kind: str = "script", variant: str = "resource-v1", **config
):
    settings = _settings(**{"resources.fetch": True, "robots.policy": "ignore", **config})
    content_type = "application/javascript" if kind == "script" else "text/css"

    def fetcher(url):
        assert url == RESOURCE_URL
        return _Response(body, content_type)

    with NativeScan.create(
        path, **_metadata_for_resources(**{"robots.policy": "ignore", **config})
    ) as scan:
        scan.enqueue([(PAGE_URL, 0)])
        lease = scan.claim(1)[0]
        scan.commit_page(
            lease,
            _record(),
            captures=[_event(PAGE_URL, b"<html>static</html>", "text/html")],
            resources=[{"kind": kind, "url": RESOURCE_URL, "raw_url": "/app.js"}],
            resource_inventory_state="complete",
            runtime=_runtime(),
        )
        capture_resources(scan, settings, fetcher=fetcher, sleeper=lambda _seconds: None)
        scan.con.execute(
            "UPDATE responses SET variant_key=? WHERE request_url_id="
            "(SELECT url_id FROM urls WHERE url=?) AND purpose=?",
            (variant, RESOURCE_URL, kind),
        )
        scan.con.commit()


def _static_and_rendered_scan(path, *, static: bytes, diagnostic: bytes, renderer: dict):
    with NativeScan.create(path, **_metadata_for_resources(**{"robots.policy": "ignore"})) as scan:
        scan.enqueue([(PAGE_URL, 0)])
        lease = scan.claim(1)[0]
        scan.commit_page(
            lease,
            _record(),
            captures=[_event(PAGE_URL, static, "text/html")],
            resources=[],
            resource_inventory_state="complete",
            runtime=_runtime(),
        )
        static_id = scan.con.execute(
            "SELECT document_id FROM documents WHERE representation='static'"
        ).fetchone()[0]
        scan.commit_render(
            PAGE_URL,
            _rendered_record(PAGE_URL),
            html="<html>rendered</html>",
            renderer=renderer,
            captured_at="2026-09-06T10:02:00Z",
            resources=[],
            resource_inventory_state="complete",
        )
        policy = json.loads(scan.con.execute("SELECT retention_json FROM scan").fetchone()[0])
        store_response(
            scan.con,
            _event(PAGE_URL, diagnostic, "text/html"),
            purpose="page",
            policy=policy,
        )
        scan.con.commit()
        return static_id


def _mark_inventory(scan: sqlite3.Connection, document_id: int):
    scan.execute(
        "INSERT INTO context_items(kind,item_key,payload_version,payload_json,completeness,reason) "
        "VALUES('resource_inventory',?,'scan_context.v1',?,'complete','')",
        (
            f"document:{document_id}",
            json.dumps({"document_id": document_id, "state": "complete", "omitted": 0}),
        ),
    )


def test_static_uses_active_inventory_after_page_selects_rendered_document(tmp_path):
    left_path = tmp_path / "left.sqlite"
    right_path = tmp_path / "right.sqlite"
    left_static = _static_and_rendered_scan(
        left_path,
        static=b"<html>same static</html>",
        diagnostic=b"<html>left diagnostic</html>",
        renderer=_renderer(PAGE_URL),
    )
    _static_and_rendered_scan(
        right_path,
        static=b"<html>same static</html>",
        diagnostic=b"<html>right diagnostic</html>",
        renderer=_renderer(PAGE_URL),
    )
    with sqlite3.connect(left_path) as left, sqlite3.connect(right_path) as right:
        result = body_diff(left, right, PAGE_URL)
    assert result["status"] == "unchanged"
    assert result["left"]["document_id"] == left_static


def test_body_diff_compares_static_with_rendered_in_one_scan(tmp_path):
    path = tmp_path / "scan.sqlite"
    _static_and_rendered_scan(
        path,
        static=b"<html><body>server copy</body></html>",
        diagnostic=b"<html>diagnostic</html>",
        renderer=_renderer(PAGE_URL),
    )
    with sqlite3.connect(path) as scan:
        result = body_diff(
            scan,
            scan,
            PAGE_URL,
            left_representation="static",
            right_representation="rendered",
            text=True,
        )
    assert result["status"] == "changed"
    assert result["representation"] == "cross-representation"
    assert result["left_representation"] == "static"
    assert result["right_representation"] == "rendered"
    assert "server copy" in "\n".join(result["text_diff"])


def test_body_read_and_diff_keep_multiple_inventory_variants_inspectable(tmp_path):
    path = tmp_path / "variants.sqlite"
    _static_and_rendered_scan(
        path,
        static=b"<html>primary variant</html>",
        diagnostic=b"<html>secondary variant</html>",
        renderer=_renderer(PAGE_URL),
    )
    with sqlite3.connect(path) as scan:
        documents = scan.execute(
            "SELECT document_id,source_response_id FROM documents "
            "WHERE representation='static' ORDER BY document_id"
        ).fetchall()
        _primary_id, primary_response = documents[0]
        secondary_id, secondary_response = documents[-1]
        scan.execute(
            "UPDATE responses SET variant_key='primary-v1' WHERE response_id=?",
            (primary_response,),
        )
        scan.execute(
            "UPDATE responses SET variant_key='secondary-v2' WHERE response_id=?",
            (secondary_response,),
        )
        _mark_inventory(scan, secondary_id)
        result = body_diff(scan, scan, PAGE_URL)
        selected = body_diff(scan, scan, PAGE_URL, variant_key="secondary-v2")
        scan.commit()

    assert result["status"] == "not_comparable"
    assert "variant_key is required" in result["reason"]
    assert selected["status"] == "unchanged"
    assert selected["left"]["document_id"] == secondary_id


def test_rendered_comparison_preserves_explicit_variant_constraint(tmp_path):
    path = tmp_path / "rendered.sqlite"
    _static_and_rendered_scan(
        path,
        static=b"<html>static</html>",
        diagnostic=b"<html>diagnostic</html>",
        renderer=_renderer(PAGE_URL),
    )
    with sqlite3.connect(path) as scan:
        result = body_diff(scan, scan, PAGE_URL, representation="rendered", variant_key="http-v1")

    assert result["status"] == "not_comparable"
    assert "requested HTTP variant is missing" in result["reason"]


def test_body_diff_cli_and_mcp_share_cross_representation_options(tmp_path, capsys):
    from seohead import cli
    from seohead.mcp.mcp_server import build_server

    path = tmp_path / "scan.sqlite"
    _static_and_rendered_scan(
        path,
        static=b"<html><body>server copy</body></html>",
        diagnostic=b"<html>diagnostic</html>",
        renderer=_renderer(PAGE_URL),
    )
    args = [
        "scan-body-diff",
        "--left",
        str(path),
        "--right",
        str(path),
        "--url",
        PAGE_URL,
        "--left-representation",
        "static",
        "--right-representation",
        "rendered",
        "--text",
    ]
    assert cli.main(args) == 0
    command_result = json.loads(capsys.readouterr().out)
    tool = build_server()._tool_manager.get_tool("seo_scan_body_diff")
    mcp_result = tool.fn(
        left=str(path),
        right=str(path),
        url=PAGE_URL,
        left_representation="static",
        right_representation="rendered",
        text=True,
    )
    assert command_result == mcp_result


def test_static_resource_response_fallback_is_retained_and_variant_safe(tmp_path):
    left_path = tmp_path / "left-resource.sqlite"
    right_path = tmp_path / "right-resource.sqlite"
    other_variant_path = tmp_path / "other-resource.sqlite"
    _resource_scan(left_path, body=b"window.app=true", variant="resource-v1")
    _resource_scan(right_path, body=b"window.app=true", variant="resource-v1")
    _resource_scan(other_variant_path, body=b"window.app=true", variant="resource-v2")
    with (
        sqlite3.connect(left_path) as left,
        sqlite3.connect(right_path) as right,
        sqlite3.connect(other_variant_path) as other,
    ):
        result = body_diff(left, right, RESOURCE_URL)
        mismatch = body_diff(left, other, RESOURCE_URL)
        rendered = body_diff(left, right, RESOURCE_URL, representation="rendered")
    assert result["status"] == "unchanged"
    assert result["left"]["document_id"] is None
    assert mismatch["status"] == "not_comparable"
    assert "HTTP variants differ" in mismatch["reason"]
    assert rendered["status"] == "missing_evidence"


def test_static_resource_fallback_preserves_state_and_purpose(tmp_path):
    truncated_path = tmp_path / "truncated.sqlite"
    omitted_path = tmp_path / "omitted.sqlite"
    unavailable_path = tmp_path / "unavailable.sqlite"
    complete_path = tmp_path / "complete.sqlite"
    stylesheet_path = tmp_path / "stylesheet.sqlite"
    _resource_scan(
        truncated_path,
        body=b"window.app=true",
        variant="resource-v1",
        **{"resources.max_response_bytes": 4},
    )
    _resource_scan(
        omitted_path,
        body=b"window.app=true",
        variant="resource-v1",
        **{"storage.max_body_bytes": 4},
    )
    _resource_scan(
        unavailable_path,
        body=b"window.app=true",
        variant="resource-v1",
        **{"resources.max_requests": 0},
    )
    _resource_scan(complete_path, body=b"body{color:blue}", variant="resource-v1")
    _resource_scan(
        stylesheet_path,
        body=b"body{color:blue}",
        kind="stylesheet",
        variant="resource-v1",
    )
    with (
        sqlite3.connect(truncated_path) as truncated,
        sqlite3.connect(omitted_path) as omitted,
        sqlite3.connect(unavailable_path) as unavailable,
        sqlite3.connect(complete_path) as complete,
        sqlite3.connect(stylesheet_path) as stylesheet,
    ):
        missing = body_diff(truncated, complete, RESOURCE_URL, variant_key="resource-v1")
        omission = body_diff(omitted, complete, RESOURCE_URL, variant_key="resource-v1")
        unavailable_result = body_diff(
            unavailable, complete, RESOURCE_URL, variant_key="resource-v1"
        )
        purpose = body_diff(complete, stylesheet, RESOURCE_URL, variant_key="resource-v1")
    assert missing["status"] == "missing_evidence"
    assert "truncated" in missing["reason"]
    assert omission["status"] == "missing_evidence"
    assert "omitted/body_budget_exhausted" in omission["reason"]
    assert unavailable_result["status"] == "missing_evidence"
    assert "body observation is missing" in unavailable_result["reason"]
    assert purpose["status"] == "not_comparable"
    assert purpose["reason"] == "body purpose differs between scans"


def test_rendered_body_diff_requires_matching_renderer_provenance(tmp_path):
    left_path = tmp_path / "left-rendered.sqlite"
    right_path = tmp_path / "right-rendered.sqlite"
    left_renderer = _renderer(PAGE_URL)
    right_renderer = _renderer(PAGE_URL)
    right_renderer["settings"]["viewport"]["width"] = 390
    _static_and_rendered_scan(
        left_path,
        static=b"<html>static</html>",
        diagnostic=b"<html>diagnostic</html>",
        renderer=left_renderer,
    )
    _static_and_rendered_scan(
        right_path,
        static=b"<html>static</html>",
        diagnostic=b"<html>diagnostic</html>",
        renderer=right_renderer,
    )
    with sqlite3.connect(left_path) as left, sqlite3.connect(right_path) as right:
        result = body_diff(left, right, PAGE_URL, representation="rendered")
        right.execute("UPDATE documents SET renderer_json='{}' WHERE representation='rendered'")
        unknown = body_diff(left, right, PAGE_URL, representation="rendered")
    assert result["status"] == "not_comparable"
    assert result["reason"] == "rendered renderer provenance differs between scans"
    assert unknown["status"] == "not_comparable"
    assert unknown["reason"] == "rendered renderer provenance is unavailable for right"
