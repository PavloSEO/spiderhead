"""Project-bound reports over streamed audit.v2 scans, budget-capped scope, and CLI --input."""

from __future__ import annotations

import sqlite3
from datetime import datetime, timedelta, timezone

import pytest

from seohead.cli import main as cli_main
from seohead.crawl.settings import fingerprint, load
from seohead.mcp import handlers
from seohead.sf.core.models import AuditResult, Issue, Page
from seohead.storage.native_scan import NativeScan
from tests.test_project_prepare_reuse import _project
from tests.test_scan_history import _event, _metadata, _record, _runtime


def _streamed_scan(path, *, cap=500, stopped=False):
    """A finished native scan whose audit is saved as a streamed audit.v2 companion."""
    config = load(overrides={"speed.min_delay_seconds": 0, "limits.max_urls": cap})
    metadata = {**_metadata(), "config": config, "config_fingerprint": fingerprint(config)}
    run = {
        "input_mode": "crawl",
        "source": "https://example.test/",
        "generated_at": "2026-10-04T00:00:00Z",
    }
    if stopped:
        run.update(crawl_partial=True, crawl_finish_reason="stopped_by_budget")
    with NativeScan.create(path, **metadata) as scan:
        scan.enqueue([("https://example.test/", 0)])
        lease = scan.claim(1)[0]
        scan.commit_page(
            lease,
            _record(lease.url),
            captures=[_event(lease.url, b"<html>body</html>")],
            runtime=_runtime(),
        )
        if stopped:
            scan.stop_by_budget(f"url limit reached ({cap})")
        result = AuditResult(
            run=run,
            summary={"totals": {}, "by_severity": {}, "by_check": {}, "health_score": None},
            issues=[
                Issue(check="TITLE_MISSING", severity="warning", source="fixture", message="x")
            ],
            pages=[Page(url="https://example.test/", metrics={"title": ""})],
        )
        header, collections = result.audit_v2_parts()
        scan.save_audit_v2(header, collections)
        assert scan.finish_capture("finished")
    finished_at = (datetime.now(timezone.utc) - timedelta(days=1)).isoformat()
    with sqlite3.connect(path) as con:
        con.execute("UPDATE scan SET finished_at=?", (finished_at,))


def test_project_bound_markdown_report_carries_checklist_coverage(tmp_path):
    project = _project(tmp_path)
    scan = project / "scans" / "full.sqlite"
    _streamed_scan(scan)
    out = tmp_path / "bound.md"

    result = handlers.report_build(audit=str(scan), fmt="md", out=str(out), project=str(project))

    assert result["ok"], result
    assert "## Project checklist coverage" in out.read_text(encoding="utf-8")


def test_budget_capped_scan_is_selected_and_reported_bound_with_its_scope(tmp_path):
    project = _project(tmp_path)
    _streamed_scan(project / "scans" / "capped.sqlite", cap=120, stopped=True)
    out = tmp_path / "capped.md"

    result = handlers.audit_workflow(
        directory=str(project), action="report", fmt="md", out=str(out)
    )

    assert result["ok"], result
    assert "notice" not in result
    text = out.read_text(encoding="utf-8")
    assert "## Project checklist coverage" in text
    assert "Partial crawl — scope is limited" in text
    assert "stopped: stopped_by_budget" in text


def test_bound_csv_report_writes_the_coverage_companion(tmp_path):
    project = _project(tmp_path)
    scan = project / "scans" / "full.sqlite"
    _streamed_scan(scan)
    out = tmp_path / "bound.csv"

    result = handlers.report_build(audit=str(scan), fmt="csv", out=str(out), project=str(project))

    assert result["ok"], result
    assert out.with_suffix(".coverage.csv").exists()


def test_cli_input_accepts_format_key_for_report_build(tmp_path, capsys):
    import json

    project = _project(tmp_path)
    scan = project / "scans" / "full.sqlite"
    _streamed_scan(scan)
    out = tmp_path / "cli.md"
    payload = json.dumps({"audit": str(scan), "format": "md", "out": str(out)})

    code = cli_main(["report-build", "--input", payload])

    captured = capsys.readouterr()
    assert code == 0, captured
    assert "unexpected keyword argument" not in captured.out + captured.err
    assert out.exists()


def test_bound_pdf_overview_renders_checklist_coverage(tmp_path):
    pytest.importorskip("pypdf")
    project = _project(tmp_path)
    scan = project / "scans" / "full.sqlite"
    _streamed_scan(scan)
    out = tmp_path / "bound.pdf"

    result = handlers.report_build(
        audit=str(scan),
        fmt="pdf",
        out=str(out),
        project=str(project),
        pdf_policy="overview-v1",
    )

    assert result["ok"], result
    from pypdf import PdfReader

    text = "".join(page.extract_text() for page in PdfReader(str(out)).pages)
    assert "Project checklist items" in text


def test_bound_json_report_is_refused_instead_of_silently_unbound(tmp_path):
    project = _project(tmp_path)
    scan = project / "scans" / "full.sqlite"
    _streamed_scan(scan)
    out = tmp_path / "bound.json"

    result = handlers.report_build(audit=str(scan), fmt="json", out=str(out), project=str(project))

    assert result["ok"] is False
    assert "cannot carry project coverage" in result["error"]
    assert not out.exists()
