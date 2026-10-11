"""Existing audit-consuming paths accept a scan without changing the audit."""

import json

import pytest

from seohead.reports import build_report
from seohead.storage import import_run
from tests.test_scan_artifact import BUILD
from tests.test_scan_artifact import legacy_run as legacy_run
from tests.test_scan_artifact_office import frozen_office_clock as frozen_office_clock


@pytest.fixture
def scan(legacy_run, tmp_path):
    return import_run(legacy_run, tmp_path / "scan.sqlite", producer_build=BUILD)


@pytest.mark.parametrize("fmt", ["json", "md", "csv", "xlsx", "docx"])
def test_existing_report_accepts_the_artifact_directly(
    legacy_run, scan, tmp_path, frozen_office_clock, fmt
):
    first, second = (
        tmp_path / "json-input" / f"report.{fmt}",
        tmp_path / "scan-input" / f"report.{fmt}",
    )
    assert build_report(str(legacy_run / "audit.json"), fmt, str(first))["ok"]
    assert build_report(str(scan), fmt, str(second))["ok"]
    assert first.read_bytes() == second.read_bytes()
    if fmt == "csv":
        for suffix in (".pages.csv", ".scope.csv"):
            first_sidecar, second_sidecar = first.with_suffix(suffix), second.with_suffix(suffix)
            assert first_sidecar.exists() == second_sidecar.exists()
            if first_sidecar.exists():
                assert first_sidecar.read_bytes() == second_sidecar.read_bytes()


def test_stale_adjacent_export_is_reported_without_becoming_input(legacy_run, scan, tmp_path):
    adjacent = scan.with_name("audit.json")
    adjacent.write_text('{"this":"is not the saved audit"}')
    output = tmp_path / "report.json"
    result = build_report(str(scan), "json", str(output))
    assert result["ok"]
    assert result["input_diagnostics"][0]["code"] == "adjacent_audit_mismatch"
    assert json.loads(output.read_text()) == json.loads((legacy_run / "audit.json").read_text())


def test_cli_report_and_tasks_accept_the_same_snapshot(legacy_run, scan, tmp_path, capsys):
    from seohead import cli
    from seohead.sf import cli as sf_cli

    assert (
        cli.main(
            [
                "report-build",
                "--audit",
                str(scan),
                "--format",
                "json",
                "--out",
                str(tmp_path / "report.json"),
            ]
        )
        == 0
    )
    assert json.loads((tmp_path / "report.json").read_text()) == json.loads(
        (legacy_run / "audit.json").read_text()
    )
    for source, name in ((legacy_run / "audit.json", "from-json"), (scan, "from-scan")):
        assert sf_cli.main(["tasks", "--json", str(source), "--out", str(tmp_path / name)]) == 0
    for name in ("tasks.json", "tasks.md"):
        assert (tmp_path / "from-json" / name).read_bytes() == (
            tmp_path / "from-scan" / name
        ).read_bytes()


def test_unified_mcp_report_and_sf_tasks_are_equal(legacy_run, scan, tmp_path):
    from seohead.mcp.mcp_server import build_server

    server = build_server()
    tool = server._tool_manager.get_tool("seo_report_build")
    result = tool.fn(audit=str(scan), fmt="json", out=str(tmp_path / "mcp.json"))
    assert result["ok"]
    assert json.loads((tmp_path / "mcp.json").read_text()) == json.loads(
        (legacy_run / "audit.json").read_text()
    )
    tasks = server._tool_manager.get_tool("sf_audit_tasks")
    for source, name in ((legacy_run / "audit.json", "json-tasks"), (scan, "scan-tasks")):
        tasks.fn(json_path=str(source), out=str(tmp_path / name))
    for name in ("tasks.json", "tasks.md"):
        assert (tmp_path / "json-tasks" / name).read_bytes() == (
            tmp_path / "scan-tasks" / name
        ).read_bytes()
    summary = server._tool_manager.get_tool("sf_audit_summary")
    assert summary.fn(json_path=str(scan)) == summary.fn(json_path=str(legacy_run / "audit.json"))
    issues = server._tool_manager.get_tool("sf_audit_issues")
    assert issues.fn(json_path=str(scan)) == issues.fn(json_path=str(legacy_run / "audit.json"))


def test_matching_adjacent_export_needs_no_diagnostic(legacy_run, scan, tmp_path):
    scan.with_name("audit.json").write_bytes((legacy_run / "audit.json").read_bytes())
    result = build_report(str(scan), "json", str(tmp_path / "output.json"))
    assert result["ok"] and "input_diagnostics" not in result


def test_foreign_sqlite_is_not_reinterpreted_as_an_adjacent_audit(legacy_run, scan, tmp_path):
    import sqlite3

    scan.with_name("audit.json").write_bytes((legacy_run / "audit.json").read_bytes())
    with sqlite3.connect(scan) as con:
        con.execute("PRAGMA application_id=42")
    result = build_report(str(scan), "json", str(tmp_path / "output.json"))
    assert not result["ok"] and "application_id" in result["error"]
    assert not (tmp_path / "output.json").exists()


def test_report_cannot_replace_its_sqlite_input(scan):
    original = scan.read_bytes()
    result = build_report(str(scan), "json", str(scan))
    assert not result["ok"] and "overwrite" in result["error"]
    assert scan.read_bytes() == original


def test_compare_handler_cli_and_mcp_keep_identical_comparability_warnings(
    legacy_run, scan, tmp_path, capsys
):
    import asyncio

    from seohead import cli
    from seohead.mcp import handlers
    from seohead.mcp.mcp_server import build_server

    original = str(legacy_run / "audit.json")
    expected = handlers.compare_crawls(original, original)
    assert handlers.compare_crawls(str(scan), original) == expected
    server = build_server()
    assert asyncio.run(
        server._tool_manager.get_tool("seo_compare_crawls").fn(
            before=str(scan), after=original
        )
    ) == expected
    assert cli.main(["compare-crawls", "--before", str(scan), "--after", original]) == 0
    assert json.loads(capsys.readouterr().out) == expected
    scan.with_name("audit.json").write_text("{}")
    warned = handlers.compare_crawls(str(scan), original)
    assert warned.pop("input_diagnostics")[0]["input"] == "before"
    assert warned == expected


def test_audit_context_is_preserved_without_fabricating_raw_corpus(legacy_run, tmp_path):
    import sqlite3

    from seohead.storage.exports import export_run

    path = legacy_run / "audit.json"
    document = json.loads(path.read_text())
    assert any(issue["check"] == "FORM_URL_INSECURE" for issue in document["issues"])
    assert document["run"]["crawl_config"]["robots.policy"] == "report_only"
    assert "requires_rendering" in document["run"]
    document["summary"]["sitemap"] = {
        "urls_in_sitemap": 2,
        "in_sitemap_not_linked": ["https://example.com/orphan"],
        "sitemap_url": "https://example.com/sitemap.xml",
    }
    path.write_text(json.dumps(document, indent=2))
    original = path.read_bytes()
    artifact = import_run(legacy_run, tmp_path / "context.sqlite", producer_build=BUILD)
    export_run(artifact, tmp_path / "context-export")
    assert (tmp_path / "context-export" / "audit.json").read_bytes() == original
    with sqlite3.connect(artifact) as con:
        for table in ("forms", "responses", "documents", "bodies", "resume_state"):
            assert con.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0] == 0


def test_mcp_diagnostics_do_not_turn_into_findings(legacy_run, scan, tmp_path):
    from seohead.mcp.mcp_server import build_server

    scan.with_name("audit.json").write_text("not JSON")
    server = build_server()
    summary = server._tool_manager.get_tool("sf_audit_summary").fn(json_path=str(scan))
    assert summary["input_diagnostics"][0]["code"] == "adjacent_audit_mismatch"
    tasks = server._tool_manager.get_tool("sf_audit_tasks").fn(
        json_path=str(scan), out=str(tmp_path / "tasks")
    )
    assert tasks["input_diagnostics"][0]["code"] == "adjacent_audit_mismatch"
    with pytest.warns(RuntimeWarning, match="adjacent_audit_mismatch"):
        issues = server._tool_manager.get_tool("sf_audit_issues").fn(json_path=str(scan))
    assert issues == json.loads((legacy_run / "audit.json").read_text())["issues"][:50]
