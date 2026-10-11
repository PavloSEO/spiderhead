"""Saved URL queries over source-bound declared-resource evidence."""

from __future__ import annotations

from seohead.mcp import handlers
from seohead.storage.native_scan import NativeScan
from tests.test_batch_resource_graph import _store_html, _v2_metadata


def test_resource_filters_and_metrics_reuse_retained_page_document(tmp_path):
    path = tmp_path / "resources.sqlite"
    resource_url = "https://example.test/assets/hero.png"
    with NativeScan.create(path, format_version="scan.v2", **_v2_metadata()) as scan:
        _store_html(scan, '<img src="/assets/hero.png" alt="Hero">')
        scan.finish_capture()

    by_kind = handlers.scan_url_query(
        input_path=str(path),
        filters=[{"column": "resource_kind", "op": "eq", "value": "image"}],
        columns=["url", "resource_count", "resource_images", "resource_bytes", "resource_bytes_state"],
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

    assert result["ok"] is True
    assert result["facets"] == {"with_resources": 1, "with_images": 1}
    assert result["facets_state"] == "exact"
