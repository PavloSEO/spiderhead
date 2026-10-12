"""Saved URL queries over source-bound declared-resource evidence."""

from __future__ import annotations

import hashlib
import sqlite3

import pytest

from seohead.crawl.settings import load
from seohead.mcp import handlers
from seohead.storage import ScanError, url_query_projection
from seohead.storage.native_scan import NativeScan
from seohead.storage.resource_graph import capture
from seohead.storage.retry import requeue_scan
from tests.test_batch_resource_graph import _store_html, _v2_metadata
from tests.test_native_capture import _renderer
from tests.test_scan_native import _metadata, _record, _runtime


def test_resource_filters_and_metrics_reuse_retained_page_document(tmp_path):
    path = tmp_path / "resources.sqlite"
    resource_url = "https://example.test/assets/hero.png"
    with NativeScan.create(path, format_version="scan.v2", **_v2_metadata()) as scan:
        _store_html(scan, '<img src="/assets/hero.png" alt="Hero">')
        scan.finish_capture()

    by_kind = handlers.scan_url_query(
        input_path=str(path),
        filters=[{"column": "resource_kind", "op": "eq", "value": "image"}],
        columns=[
            "url",
            "resource_count",
            "resource_images",
            "resource_bytes",
            "resource_bytes_state",
        ],
    )
    by_url = handlers.scan_url_query(
        input_path=str(path),
        filters=[{"column": "resource_url", "op": "eq", "value": resource_url}],
        columns=["url"],
    )

    assert by_kind["ok"] is True
    assert by_kind["filtered_total"] == 1
    assert by_kind["rows"][0]["resource_count"] == 1
    assert by_kind["rows"][0]["resource_images"] == 1
    assert by_kind["rows"][0]["resource_bytes"] is None
    assert by_kind["rows"][0]["resource_bytes_state"] == "unavailable"
    assert by_url["filtered_total"] == 1
    assert by_url["rows"][0]["url"] == "https://example.test/"
    evidence = handlers.scan_evidence(input_path=str(path), section="resources")
    assert evidence["evidence"]["aggregates"]["blocks_by_kind"] == {"image": 1}


def test_resource_facets_are_source_aware(tmp_path):
    path = tmp_path / "resources.sqlite"
    with NativeScan.create(path, format_version="scan.v2", **_v2_metadata()) as scan:
        _store_html(scan, '<img src="/image.png">')
        scan.finish_capture()

    result = handlers.scan_url_query(input_path=str(path), facets=["with_resources", "with_images"])

    assert result["ok"] is True, result
    assert result["facets"] == {"with_resources": 1, "with_images": 1}
    assert result["facets_state"] == "exact"


def test_projection_resource_kind_filters_preserve_exact_kind_identity(tmp_path):
    path = tmp_path / "resource-kinds.sqlite"
    with NativeScan.create(path, format_version="scan.v2", **_v2_metadata()) as scan:
        _store_html(
            scan,
            '<script src="/classic.js"></script>'
            '<script type="module" src="/module.js"></script>'
            '<img src="/pixel.png">',
        )
        scan.finish_capture()

    con = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    con.row_factory = sqlite3.Row
    try:
        assert url_query_projection.load(con) is not None
    finally:
        con.close()

    classic = handlers.scan_url_query(
        input_path=str(path),
        filters=[{"column": "resource_kind", "op": "eq", "value": "script"}],
        columns=["url", "resource_count", "resource_scripts"],
    )
    module = handlers.scan_url_query(
        input_path=str(path),
        filters=[{"column": "resource_kind", "op": "eq", "value": "module"}],
        columns=["url"],
    )
    image = handlers.scan_url_query(
        input_path=str(path),
        filters=[{"column": "resource_kind", "op": "eq", "value": "image"}],
        columns=["url"],
    )

    assert classic["filtered_total"] == 1
    assert classic["rows"][0]["resource_count"] == 3
    assert classic["rows"][0]["resource_scripts"] == 2
    assert module["filtered_total"] == 1
    assert image["filtered_total"] == 1


def test_projection_counts_resources_for_the_selected_rendered_document(tmp_path):
    path = tmp_path / "rendered-resource-projection.sqlite"
    with NativeScan.create(path, format_version="scan.v2", **_v2_metadata()) as scan:
        _store_html(scan, '<img src="/static.png">')
        rendered = _record("https://example.test/")
        rendered.update(representation="rendered", title="Rendered")
        scan.commit_render(
            "https://example.test/",
            rendered,
            html='<img src="/rendered.png">',
            renderer=_renderer("https://example.test/"),
            captured_at="2026-10-12T10:00:00Z",
            resource_inventory_state="complete",
            content_capture={
                "html": '<img src="/rendered.png">',
                "parsed": None,
                "settings": {},
                "indexable": None,
                "canonical_target": "",
                "unavailable_reason": "",
                "extraction_rules": {},
            },
        )
        scan.finish_capture()

    result = handlers.scan_url_query(
        input_path=str(path),
        filters=[{"column": "resource_kind", "op": "eq", "value": "image"}],
        columns=["url", "resource_count", "resource_images"],
    )

    assert result["ok"] is True, result
    assert result["filtered_total"] == 1
    assert result["rows"][0]["resource_count"] == 1
    assert result["rows"][0]["resource_images"] == 1


def test_projection_rejects_stale_revision_and_failed_rebuild_publication(tmp_path, monkeypatch):
    path = tmp_path / "projection-stale.sqlite"
    with NativeScan.create(path, format_version="scan.v2", **_v2_metadata()) as scan:
        _store_html(scan, '<img src="/pixel.png">')
        scan.finish_capture()

    con = sqlite3.connect(path)
    con.row_factory = sqlite3.Row
    try:
        con.execute("BEGIN IMMEDIATE")
        con.execute("UPDATE scan SET evidence_revision=evidence_revision+1 WHERE singleton=1")
        assert url_query_projection.load(con) is None
        con.rollback()

        for update in (
            "UPDATE scan SET scan_uuid='different-scan' WHERE singleton=1",
            "UPDATE scan SET capabilities_json=capabilities_json||' ' WHERE singleton=1",
            "UPDATE scan SET start_url=start_url||'different' WHERE singleton=1",
        ):
            con.execute("BEGIN IMMEDIATE")
            con.execute(update)
            assert url_query_projection.load(con) is None
            con.rollback()

        def fail_facets(*_args, **_kwargs):
            raise sqlite3.OperationalError("injected optional projection failure")

        monkeypatch.setattr("seohead.storage.url_query._facet_counts", fail_facets)
        con.execute("BEGIN IMMEDIATE")
        assert url_query_projection.persist(con) is False
        assert url_query_projection.load(con) is None
        con.rollback()
        assert url_query_projection.load(con) is not None
    finally:
        con.close()


def test_projection_records_partial_resource_coverage_as_unavailable(tmp_path):
    path = tmp_path / "projection-partial.sqlite"
    with NativeScan.create(path, format_version="scan.v2", **_v2_metadata()) as scan:
        _store_html(scan, '<img src="/pixel.png">')
        scan.con.execute(
            "UPDATE context_items SET completeness='partial' WHERE kind='resource_graph_coverage'"
        )
        scan.finish_capture()

    unavailable = handlers.scan_url_query(input_path=str(path), columns=["url", "resource_count"])
    facets = handlers.scan_url_query(input_path=str(path), facets=["with_images"])

    assert unavailable["ok"] is False
    assert unavailable["reason_code"] == "resource_graph_unavailable"
    assert facets["facets"] == {"with_images": None}
    assert facets["facet_states"] == {"with_images": "unavailable"}


def test_finished_resource_capture_is_rejected_to_preserve_projection_freshness(tmp_path):
    path = tmp_path / "finished-resource-capture.sqlite"
    with NativeScan.create(path, format_version="scan.v2", **_v2_metadata()) as scan:
        _store_html(scan, '<img src="/pixel.png">')
        scan.finish_capture()
        with pytest.raises(ScanError, match="immutable"):
            capture(
                scan,
                load(overrides={"storage.format_version": "scan.v2", "resources.fetch": True}),
            )


def test_retry_requeue_invalidates_published_projection(tmp_path):
    path = tmp_path / "projection-retry.sqlite"
    with NativeScan.create(path, format_version="scan.v2", **_v2_metadata()) as scan:
        _store_html(scan, '<img src="/pixel.png">')
        scan.finish_capture()
    assert handlers.scan_url_query(input_path=str(path), facets=["with_images"])["facets"] == {
        "with_images": 1
    }

    requeue_scan(
        path,
        where='url = "https://example.test/"',
        backup_path=tmp_path / "before-projection-retry.sqlite",
    )

    con = sqlite3.connect(path)
    try:
        assert con.execute("SELECT COUNT(*) FROM scan_url_query_projection_meta").fetchone()[0] == 0
    finally:
        con.close()


def test_scan_v1_url_query_keeps_schema_and_database_bytes_read_only(tmp_path):
    path = tmp_path / "frozen-v1.sqlite"
    with NativeScan.create(path, format_version="scan.v1", **_metadata()) as scan:
        url = "https://example.test/"
        scan.enqueue([(url, 0)])
        scan.commit_page(scan.claim(1)[0], _record(url), runtime=_runtime())
        scan.finish_capture()
    before = hashlib.sha256(path.read_bytes()).digest()

    result = handlers.scan_url_query(input_path=str(path), columns=["url", "title"])

    after = hashlib.sha256(path.read_bytes()).digest()
    con = sqlite3.connect(path)
    try:
        version = con.execute("PRAGMA user_version").fetchone()[0]
        projection_tables = con.execute(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' "
            "AND name LIKE 'scan_url_query_projection_%'"
        ).fetchone()[0]
    finally:
        con.close()
    assert result["ok"] is True
    assert before == after
    assert version == 1
    assert projection_tables == 0
