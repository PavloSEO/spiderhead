"""A project report over a streamed audit.v2 scan falls back to unbound with a notice."""

from __future__ import annotations

from seohead.mcp.handlers import audit_workflow
from seohead.projects.workspace import create_project
from seohead.storage.audit_v2 import write_audit_v2
from tests.test_scan_audit_v2 import _scan


def test_report_on_streamed_scan_is_built_unbound_with_notice(tmp_path):
    root = tmp_path / "project"
    create_project(root, "https://example.test/")
    scan = tmp_path / "scan.sqlite"
    binding = _scan(scan)
    header = {
        "schema_version": "2.0",
        "run": {"collector": "seohead.crawl", "generated_at": "2026-10-06T00:00:00Z"},
        "summary": {"totals": {"urls_crawled": 1, "issues_total": 0}, "by_severity": {}},
        "groups": [],
        "pages": [],
        "issues": [],
    }
    page = {"url": "https://example.test/", "status_code": 200, "metrics": {"title": "Page"}}
    write_audit_v2(scan, header, {"/issues": [], "/pages": [page]}, binding)
    out = tmp_path / "unbound.md"

    result = audit_workflow(str(root), action="report", audit=str(scan), fmt="md", out=str(out))

    assert result["ok"], result
    assert "not bound" in result["notice"]
    assert out.exists()
