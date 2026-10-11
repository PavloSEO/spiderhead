"""Public bounded readers/exporters retain core semantics across CLI and MCP."""

from __future__ import annotations

import inspect
import json

import pytest
from pydantic import ValidationError

from seohead import cli
from seohead.core.job_contracts import ScanOptions
from seohead.integrations.remote_api.backend import RemoteProjectLimits, SQLiteJobBackend
from seohead.mcp import handlers
from seohead.mcp.mcp_server import build_server
from seohead.projects.coverage import initialize_coverage
from seohead.projects.workspace import create_project
from tests.test_bi_bounded_delivery import package
from tests.test_remote_backend import SCANS_A, SITE, _api, _headers, _network


def _cli(command, *arguments):
    args = cli.build_parser().parse_args([command, *arguments])
    name, kwargs = cli._build_kwargs(command, args)
    return handlers.HANDLERS[name](**kwargs)


def test_bi_filter_cli_mcp_preserve_identical_selected_package(tmp_path):
    source = package(tmp_path)
    left, right = tmp_path / "cli", tmp_path / "mcp"
    where = {"url": ["https://example.test/1", "https://example.test/3"]}
    a = _cli(
        "bi-filter",
        "--package",
        str(source),
        "--dataset",
        "pages",
        "--out-dir",
        str(left),
        "--where",
        json.dumps(where),
        "--columns",
        "url,title",
        "--max-rows-per-file",
        "1",
    )
    tool = build_server()._tool_manager.get_tool("seo_bi_filter")
    b = tool.fn(
        package=str(source),
        dataset="pages",
        out_dir=str(right),
        where=where,
        columns=["url", "title"],
        max_rows_per_file=1,
    )
    assert a["conservation"] == b["conservation"]
    assert a["conservation"]["selected_rows"] == 2
    assert {p.name: p.read_bytes() for p in left.iterdir()} == {
        p.name: p.read_bytes() for p in right.iterdir()
    }
    assert tool.annotations.readOnlyHint is False
    assert tool.annotations.openWorldHint is False


def test_observer_cli_mcp_readers_preserve_project_and_page_contract(tmp_path):
    project = tmp_path / "project"
    create_project(project, "https://example.test/")
    initialize_coverage(project)
    original = {p.name: p.read_bytes() for p in project.iterdir() if p.is_file()}
    manager = build_server()._tool_manager
    page = _cli("project-checklist-page", "--directory", str(project), "--limit", "2")
    other = manager.get_tool("seo_project_checklist_page").fn(directory=str(project), limit=2)
    assert page == other
    item_id = page["items"][0]["id"]
    detail = _cli("project-task-detail", "--directory", str(project), "--item-id", item_id)
    assert detail == manager.get_tool("seo_project_task_detail").fn(
        directory=str(project), item_id=item_id
    )
    assert _cli("project-scans", "--directory", str(project)) == manager.get_tool(
        "seo_project_scans"
    ).fn(directory=str(project))
    assert _cli("project-activity", "--directory", str(project))["sites"]["total"] == 1
    assert original == {p.name: p.read_bytes() for p in project.iterdir() if p.is_file()}
    for name in (
        "project_activity",
        "project_checklist_page",
        "project_task_detail",
        "project_scans",
        "scan_navigation",
    ):
        annotations = manager.get_tool("seo_" + name).annotations
        assert annotations.readOnlyHint is True and annotations.openWorldHint is False


@pytest.mark.parametrize(
    "command,flags,expected",
    [
        (
            "scan-navigation",
            ["--scan", "scan.sqlite", "--document-id", "7", "--limit", "3", "--offset", "2"],
            {"input_path": "scan.sqlite", "document_id": 7, "limit": 3, "offset": 2},
        ),
        (
            "project-checklist-page",
            [
                "--directory",
                "project",
                "--query",
                "title",
                "--kind",
                "check",
                "--state",
                "failed",
                "--limit",
                "9",
                "--offset",
                "3",
            ],
            {
                "directory": "project",
                "descending": False,
                "query": "title",
                "kind": "check",
                "state": "failed",
                "limit": 9,
                "offset": 3,
            },
        ),
    ],
)
def test_cli_flags_and_mcp_defaults_share_handler_contract(command, flags, expected, monkeypatch):
    name, kwargs = cli._build_kwargs(command, cli.build_parser().parse_args([command, *flags]))
    assert kwargs == expected
    signature = inspect.signature(handlers.HANDLERS[name])
    calls = []
    monkeypatch.setattr(handlers, name, lambda **values: calls.append(values) or {"ok": True})
    build_server()._tool_manager.get_tool("seo_" + name).fn(**kwargs)
    bound = signature.bind(**kwargs)
    bound.apply_defaults()
    assert calls == [bound.arguments]


def test_scale_admission_stays_within_trusted_remote_project_budget(tmp_path, monkeypatch):
    _network(monkeypatch)
    request = {"target_url": SITE, "options": {"max_urls": 1_000_000, "max_requests": 2_000_000}}
    default = RemoteProjectLimits()
    assert (default.max_urls, default.max_requests) == (10_000, 20_000)
    backend = SQLiteJobBackend(tmp_path / "default", {"alpha": default}, producer_build="a" * 40)
    response = _api(backend).post(SCANS_A, headers=_headers(), json=request)
    assert response.status_code == 422
    assert response.json()["error"]["code"] == "budget_exceeded"
    assert backend.list_jobs("alpha", 0, 10) == []
    authorized = SQLiteJobBackend(
        tmp_path / "authorized",
        {"alpha": RemoteProjectLimits(max_urls=1_000_000, max_requests=2_000_000)},
        producer_build="a" * 40,
    )
    response = _api(authorized).post(SCANS_A, headers=_headers(), json=request)
    assert response.status_code == 202, response.text
    assert len(authorized.list_jobs("alpha", 0, 10)) == 1
    for values in (
        {"max_urls": 1_000_001},
        {"max_urls": 2_000_000},
        {"max_requests": 2_000_001},
        {"max_urls": True},
    ):
        with pytest.raises(ValidationError):
            ScanOptions(**values)


def test_scan_navigation_cli_mcp_share_retained_report(tmp_path):
    from seohead.storage.native_scan import NativeScan
    from tests.test_native_capture import _renderer
    from tests.test_native_render_atomic import _static_page
    from tests.test_scan_native import _metadata

    path = tmp_path / "scan.seohead"
    with NativeScan.create(path, **_metadata()) as scan:
        lease = _static_page(scan)
        scan.commit_render(
            lease.url,
            None,
            html="<html>fixture</html>",
            renderer=_renderer(lease.url),
            captured_at="2026-10-06T10:00:00Z",
        )
    before = path.read_bytes()
    result = _cli("scan-navigation", "--scan", str(path), "--limit", "1")
    assert result == build_server()._tool_manager.get_tool("seo_scan_navigation").fn(
        input_path=str(path), limit=1
    )
    assert result["state"] == "unavailable"
    assert path.read_bytes() == before


def test_explicit_project_sitemap_observation_matches_cli_mcp_and_preserves_units(
    tmp_path, monkeypatch
):
    from seohead.projects import run_observation

    project = tmp_path / "project"
    create_project(project, "https://example.test/")
    url = "https://example.test/sitemap.xml"
    result = {
        "ok": True,
        "root": url,
        "count": 25,
        "sitemaps": [{"url": url}],
        "errors": [],
        "truncated": False,
    }
    calls = []

    def crawl(value, concurrency):
        calls.append((value, concurrency))
        if len(calls) > 1:
            current = run_observation.status(project)["items"][0]
            assert current["kind"] == "sitemap"
        return result

    monkeypatch.setattr(handlers.sitemap, "crawl", crawl)
    assert handlers.sitemap_crawl(url) == result
    assert not (project / run_observation.NAME).exists()
    assert _cli("sitemap-crawl", "--url", url, "--project", str(project)) == result
    tool = build_server()._tool_manager.get_tool("seo_sitemap_crawl")
    assert tool.fn(url=url, project=str(project)) == result
    assert tool.annotations.readOnlyHint is False and tool.annotations.openWorldHint is True
    rows = run_observation.status(project)["items"]
    assert len(rows) == 2
    assert all(row["state"] == "finished" for row in rows)
    assert all(row["source_metadata"]["declared_url_count"] == 25 for row in rows)
    assert all(row["counters"]["fetched"] == 1 for row in rows)
    assert all(row["telemetry"]["unit"] == "sitemap_documents" for row in rows)
    for bad in (
        "https://other.test/sitemap.xml",
        url + "?token=secret",
        "https://user:pass@example.test/sitemap.xml",
    ):
        with pytest.raises(ValueError, match="project origin"):
            handlers.sitemap_crawl(bad, project=str(project))
    assert len(calls) == 3


def test_failed_observed_sitemap_is_terminal(tmp_path, monkeypatch):
    from seohead.projects import run_observation

    project = tmp_path / "project"
    create_project(project, "https://example.test/")

    def interrupted(*_):
        raise RuntimeError("synthetic failure")

    monkeypatch.setattr(handlers.sitemap, "crawl", interrupted)
    with pytest.raises(RuntimeError, match="synthetic failure"):
        handlers.sitemap_crawl("https://example.test/sitemap.xml", project=str(project))
    row = run_observation.status(project)["items"][0]
    assert row["state"] == "failed"
    assert row["counters"]["fetched"] is None


@pytest.mark.parametrize("compression", ["none", "gzip"])
def test_compare_output_cli_mcp_preserve_full_manifest_and_source_counts(tmp_path, compression):
    import asyncio

    from tests.test_compare_bounded import _source
    from tests.test_verify_fixes import A, B, _audit, _issue

    before = _audit(issues=[_issue("before-a", "TITLE_MISSING", A)])
    after = _audit(issues=[_issue("after-b", "DESC_MISSING", B)])
    with _source(tmp_path, "before", before), _source(tmp_path, "after", after):
        pass
    sources = [tmp_path / "before.sqlite", tmp_path / "after.sqlite"]
    original = [path.read_bytes() for path in sources]
    left, right = tmp_path / "cli-compare", tmp_path / "mcp-compare"
    a = _cli(
        "compare-crawls",
        "--before",
        str(sources[0]),
        "--after",
        str(sources[1]),
        "--out-dir",
        str(left),
        "--compression",
        compression,
    )
    tool = build_server()._tool_manager.get_tool("seo_compare_crawls")
    b = asyncio.run(
        tool.fn(
            before=str(sources[0]),
            after=str(sources[1]),
            out_dir=str(right),
            compression=compression,
        )
    )
    assert a["schema_version"] == b["schema_version"] == "compare.v2"
    assert a["files"]["left"]["format"] == ("ndjson.gz" if compression == "gzip" else "ndjson")
    if compression == "gzip":
        assert (left / a["files"]["left"]["path"]).read_bytes().startswith(b"\x1f\x8b")
        assert a["files"]["left"]["uncompressed_bytes"] > 0
    assert (
        a["conservation"]
        == b["conservation"]
        == {
            "before_issues": 1,
            "after_issues": 1,
            "before_accounted": 1,
            "after_accounted": 1,
            "state": "complete",
        }
    )
    assert {p.name: p.read_bytes() for p in left.iterdir()} == {
        p.name: p.read_bytes() for p in right.iterdir()
    }
    assert [path.read_bytes() for path in sources] == original
    assert tool.annotations.readOnlyHint is False and tool.annotations.openWorldHint is False
    legacy = handlers.compare_crawls(before, after)
    assert legacy["schema_version"] == "compare.v1"
    assert len(legacy["left"]) == 1 and len(legacy["entered"]) == 1

    from seohead.sf.core.compare_store import iter_compare_rows

    assert len(list(iter_compare_rows(a["manifest"], "left"))) == 1
    assert len(list(iter_compare_rows(a["manifest"], "entered"))) == 1


@pytest.mark.parametrize("overrides", [[], ["--kind", "note", "--author-role", "specialist"]])
def test_cli_json_proposed_goal_and_author_survive_unless_explicitly_overridden(
    tmp_path, overrides
):
    import os
    import subprocess
    import sys
    from pathlib import Path

    project = tmp_path / "project"
    create_project(project, "https://example.test/")
    payload = {
        "directory": str(project),
        "text": "Review this proposed goal",
        "kind": "proposed_goal",
        "author_role": "agent",
        "references": ["task:custom:review"],
    }
    root = Path(__file__).resolve().parents[1]
    completed = subprocess.run(
        [
            sys.executable,
            "-m",
            "seohead",
            "project-inbox-submit",
            "--input",
            json.dumps(payload),
            *overrides,
        ],
        cwd=root,
        env={**os.environ, "PYTHONPATH": str(root)},
        input="",
        text=True,
        capture_output=True,
        check=True,
    )
    entry = json.loads(completed.stdout)["entry"]
    if overrides:
        payload.update(kind="note", author_role="specialist")
    other = build_server()._tool_manager.get_tool("seo_project_inbox_submit").fn(**payload)["entry"]
    assert {
        key: entry[key] for key in ("kind", "author_role", "goal_state", "text", "references")
    } == {key: other[key] for key in ("kind", "author_role", "goal_state", "text", "references")}
    assert entry["kind"] == payload["kind"]
    assert entry["goal_state"] == ("proposed" if payload["kind"] == "proposed_goal" else None)


def test_inbox_submit_omitted_flags_keep_handler_defaults(tmp_path):
    project = tmp_path / "project"
    create_project(project, "https://example.test/")
    entry = _cli("project-inbox-submit", "--directory", str(project), "--text", "A note")["entry"]
    assert entry["kind"] == "note" and entry["author_role"] == "specialist"


def test_pdf_policy_cli_and_mcp_reach_same_builder(monkeypatch):
    from seohead import reports

    calls = []

    def build(audit, **kwargs):
        calls.append({"audit": audit, **kwargs})
        return {"ok": True}

    monkeypatch.setattr(reports, "build_report", build)
    _cli(
        "report-build",
        "--audit",
        "scan.sqlite",
        "--format",
        "pdf",
        "--out",
        "report.pdf",
        "--pdf-policy",
        "overview-v1",
    )
    tool = build_server()._tool_manager.get_tool("seo_report_build")
    tool.fn(audit="scan.sqlite", fmt="pdf", out="report.pdf", pdf_policy="overview-v1")
    assert calls[0] == calls[1]
    assert calls[0]["pdf_policy"] == "overview-v1"
    handlers.report_build(audit="audit.json", fmt="pdf", out="complete.pdf")
    assert "pdf_policy" not in calls[-1]


@pytest.mark.parametrize("fmt,audit_kind", [("xlsx", "stream"), ("pdf", "materialized")])
def test_pdf_policy_rejects_incompatible_public_inputs_without_outputs(tmp_path, fmt, audit_kind):
    from tests.test_pdf_stream import _fixture
    from tests.test_verify_fixes import _audit

    source, _ = _fixture(tmp_path, count=2)
    destination = tmp_path / ("output." + fmt)
    audit = str(source) if audit_kind == "stream" else _audit()
    result = handlers.report_build(
        audit=audit, fmt=fmt, out=str(destination), pdf_policy="overview-v1"
    )
    assert result["ok"] is False and "overview-v1" in result["error"]
    assert not destination.exists()
    assert not destination.with_suffix(".files").exists()


def test_monitor_preview_and_local_receipt_are_reachable_without_network(tmp_path, monkeypatch):
    from seohead.projects.monitoring import run, schedule
    from seohead.recon import net
    from tests.test_monitor_one_shot import _project

    directory, configured = _project(tmp_path, "https://example.test/page")
    claimed = schedule(directory, action="start", expected_revision=configured["revision"])
    before = {p.name: p.read_bytes() for p in directory.iterdir() if p.is_file()}
    monkeypatch.setattr(net, "http_client", lambda *a, **k: pytest.fail("unexpected network"))
    preview = _cli(
        "monitor-collect",
        "--directory",
        str(directory),
        "--expected-revision",
        str(claimed["revision"]),
    )
    manager = build_server()._tool_manager
    assert preview == manager.get_tool("seo_monitor_collect").fn(
        directory=str(directory), expected_revision=claimed["revision"]
    )
    assert preview["applied"] is False
    assert before == {p.name: p.read_bytes() for p in directory.iterdir() if p.is_file()}
    retained = run(
        directory,
        "scan:local",
        [
            {
                "url": "https://example.test/page",
                "changes": [{"kind": "status_changed", "severity": "warning"}],
            }
        ],
        claimed["revision"],
    )
    first = _cli(
        "monitor-local-deliver",
        "--directory",
        str(directory),
        "--scan-id",
        "scan:local",
        "--expected-revision",
        str(retained["revision"]),
    )
    second = manager.get_tool("seo_monitor_local_deliver").fn(
        directory=str(directory), scan_id="scan:local", expected_revision=first["revision"]
    )
    assert first["receipts"][0]["state"] == "sent"
    assert second["receipts"][0]["state"] == "delivered"
    assert manager.get_tool("seo_monitor_collect").annotations.openWorldHint is True
    assert manager.get_tool("seo_monitor_local_deliver").annotations.openWorldHint is False
    assert manager.get_tool("seo_monitor_local_deliver").annotations.readOnlyHint is False


@pytest.mark.parametrize(
    "value,flags,expected", [(True, [], True), (False, ["--apply"], True), (False, [], False)]
)
def test_monitor_apply_json_is_preserved_until_an_explicit_flag(
    monkeypatch, value, flags, expected
):
    from seohead.mcp import monitor_handlers

    calls = []
    monkeypatch.setattr(
        monitor_handlers, "monitor_collect", lambda **kwargs: calls.append(kwargs) or {"ok": True}
    )
    _cli(
        "monitor-collect",
        "--input",
        json.dumps({"directory": "project", "expected_revision": 3, "apply": value}),
        *flags,
    )
    assert calls == [{"directory": "project", "expected_revision": 3, "apply": expected}]


@pytest.mark.parametrize("failure", ["identity", "revision"])
def test_remediation_preflight_closes_retained_reader_on_failure(tmp_path, monkeypatch, failure):
    import sqlite3

    from seohead.core import verification
    from seohead.storage import inputs
    from seohead.storage.ledger import LedgerError
    from tests.test_compare_bounded import _source
    from tests.test_remediation_ledger import _ledger
    from tests.test_verify_fixes import _audit

    reader = _source(tmp_path, "baseline-reader", _audit())
    ledger = _ledger(tmp_path)
    monkeypatch.setattr(inputs, "load_audit_source", lambda *a, **k: reader)
    if failure == "identity":

        def reject_identity(_source):
            raise ValueError("synthetic identity mismatch")

        monkeypatch.setattr(verification, "source_identity", reject_identity)
    with pytest.raises((ValueError, LedgerError)):
        handlers.remediation_recheck(
            ledger=str(ledger),
            baseline="baseline",
            occurrence_keys=["selected"],
            actor="reviewer",
            expected_revision=-1,
            out_dir=str(tmp_path / "verification"),
        )
    with pytest.raises(sqlite3.ProgrammingError, match="closed"):
        reader.con.execute("SELECT 1")
    assert not (tmp_path / "verification").exists()
