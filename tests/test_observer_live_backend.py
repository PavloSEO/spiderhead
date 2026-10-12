"""Bounded observer projections and sampled telemetry, entirely offline."""

from __future__ import annotations

import json
import os
import sys
import time
from types import SimpleNamespace

import pytest

from seohead.projects import observer, run_observation
from seohead.projects.coverage import coverage_status, initialize_coverage, update_item
from seohead.projects.workspace import create_project
from seohead.sf.core import runner
from seohead.storage.audit_v2 import AuditV2Reader, write_audit_v2
from tests.test_project_observer_sites import _prepare_with_competitors
from tests.test_scan_audit_v2 import _scan


def _project(tmp_path):
    root = tmp_path / "project"
    create_project(root, "https://example.test/")
    return root


def _run(root):
    return run_observation.start(
        root,
        kind="native",
        mode="spider",
        max_urls=100,
        config_fingerprint="synthetic",
        artifact=root / "scans" / "test.sqlite",
    )


def test_observe_calculates_coverage_once_per_site(tmp_path, monkeypatch):
    import seohead.projects.coverage as coverage

    root = tmp_path / "project"
    _prepare_with_competitors(root)
    calls = []
    original = coverage._status

    def counted(root, *args, **kwargs):
        calls.append(str(root))
        return original(root, *args, **kwargs)

    monkeypatch.setattr(coverage, "_status", counted)
    observer.observe(str(root))
    assert len(calls) == len(set(calls)) == 3


def test_activity_never_reads_scan_or_coverage_and_keeps_competitor_runs(tmp_path, monkeypatch):
    root = tmp_path / "project"
    preparation = _prepare_with_competitors(root)["preparation"]
    child = root / preparation["competitors"][0]["directory"]
    active = _run(child)
    monkeypatch.setattr(observer, "project_status", lambda *a, **k: pytest.fail("heavy projection"))
    result = observer.observe_activity(root)
    assert result["sites"]["items"][1]["runs"]["items"][0]["id"] == active["id"]


def test_active_runs_survive_recent_terminal_limit_and_pid_queries_are_shared(
    tmp_path, monkeypatch
):
    root = _project(tmp_path)
    first = _run(root)
    for _ in range(4):
        run = _run(root)
        run_observation.finish(root, run["id"], state="finished", reason="fixture")
    second = _run(root)
    calls = []
    original = run_observation._process_identity
    monkeypatch.setattr(
        run_observation, "_process_identity", lambda pid: calls.append(pid) or original(pid)
    )
    result = run_observation.status(root, limit=1)
    assert {first["id"], second["id"]} <= {item["id"] for item in result["items"]}
    assert calls == [os.getpid()]
    assert result["active_total"] == 2 and result["has_more"]


def test_run_retention_never_evicts_active(tmp_path, monkeypatch):
    root = _project(tmp_path)
    monkeypatch.setattr(run_observation, "MAX_RUNS", 2)
    first = _run(root)
    last = _run(root)
    with pytest.raises(ValueError, match="active runs cannot"):
        _run(root)
    run_observation.finish(root, last["id"], state="finished", reason="fixture")
    replacement = _run(root)
    assert {row["id"] for row in run_observation.status(root, limit=2)["items"]} == {
        first["id"],
        replacement["id"],
    }


def test_reporter_measures_window_keeps_unknowns_and_hides_stale_current_speed(
    tmp_path, monkeypatch
):
    root = _project(tmp_path)
    run = _run(root)
    reporter = run_observation.NativeRunReporter(root, run["id"])
    clock = iter([10.0, 10.1, 11.0])
    monkeypatch.setattr(run_observation, "time", SimpleNamespace(monotonic=lambda: next(clock)))
    reporter(1, 8)
    reporter(2, 7)
    reporter(3, 6)
    monkeypatch.undo()
    row = run_observation.status(root)["items"][0]
    assert row["counters"]["rate_per_second"] == 2.0
    assert row["counters"]["inflight"] is None and row["counters"]["excluded"] is None
    assert row["telemetry"]["queue_semantics"] == "outstanding"
    path = root / run_observation.NAME
    document = json.loads(path.read_text())
    document["runs"][0]["telemetry"]["sampled_at"] = "2000-01-01T00:00:00Z"
    path.write_text(json.dumps(document))
    stale = run_observation.status(root)["items"][0]
    assert stale["telemetry"]["state"] == "stale"
    assert stale["telemetry"]["current_rate_per_second"] is None
    assert stale["counters"]["rate_per_second"] == 2.0


def test_structured_counters_are_not_replaced_by_legacy_callback(tmp_path):
    root = _project(tmp_path)
    run = _run(root)
    reporter = run_observation.NativeRunReporter(root, run["id"])
    reporter.observe_counts({"fetched": 2, "queued": 4, "inflight": 3, "excluded": 5})
    reporter(2, 7)
    assert reporter.counters()["queued"] == 4
    row = run_observation.status(root)["items"][0]
    assert row["counters"]["inflight"] == 3 and row["counters"]["excluded"] == 5
    assert row["telemetry"]["queue_semantics"] == "separate"


def test_v2_pages_and_details_never_materialize_graph_or_cache_large_issue_set(
    tmp_path, monkeypatch
):
    root = _project(tmp_path)
    path = root / "scans" / "test.sqlite"
    binding = _scan(path)
    write_audit_v2(
        path,
        {"issues": [], "pages": [], "run": {}},
        {
            "/issues": [
                {
                    "id": str(i),
                    "check": "TEST",
                    "severity": "warning",
                    "message": "note",
                    "target_url": f"https://example.test/{i}",
                }
                for i in range(30)
            ],
            "/pages": [{"url": "https://example.test/", "text": "large graph"}],
        },
        binding,
    )
    monkeypatch.setattr(
        AuditV2Reader, "materialize_legacy", lambda *a, **k: pytest.fail("materialized graph")
    )
    monkeypatch.setattr(observer, "_FINDINGS_CACHE_MAX_BYTES", 1)
    page = observer.findings_page(root, offset=10, limit=3)
    assert page["counts"] == {"source": 30, "matched": 30, "returned": 3}
    assert [row["ordinal"] for row in page["items"]] == [10, 11, 12]
    reverse = observer.findings_page(root, limit=2, descending=True)
    assert [row["ordinal"] for row in reverse["items"]] == [29, 28]
    detail = observer.finding_detail(root, ordinal=29)
    assert detail["finding"]["id"] == "29"
    assert observer.findings_page(root, query="no-match")["counts"]["matched"] == 0


def test_checklist_filter_pages_and_detail_keep_user_task_history(tmp_path):
    root = _project(tmp_path)
    initialize_coverage(root)
    update_item(
        root,
        {"id": "custom:late-task", "title": "Synthetic user task"},
        coverage_status(root)["revision"],
    )
    page = observer.checklist_page(root, query="Synthetic user task", limit=1)
    assert page["pagination"]["total"] == 1 and page["items"][0]["display_state"]
    detail = observer.task_detail(root, item_id="custom:late-task")
    assert detail["item"]["definition"]["title"] == "Synthetic user task"
    assert any(
        row["id"] == "custom:late-task"
        for row in observer.observe(str(root))["active_tasks"]["items"]
    )


def test_checklist_sorts_pages_and_multi_state_filters_keep_total(tmp_path):
    root = _project(tmp_path)
    initialize_coverage(root)
    for ident, priority in (("custom:b", "P2"), ("custom:a", "P0"), ("custom:c", "P1")):
        update_item(
            root,
            {"id": ident, "title": ident, "priority": priority},
            coverage_status(root)["revision"],
        )
    stored = observer.checklist_page(root, limit=100)
    assert stored["sort"] == {"field": "id", "direction": "asc"}
    ids = [row["id"] for row in stored["items"]]
    assert ids == [
        row["id"] for row in observer.checklist_page(root, limit=100, sort="id")["items"]
    ]
    by_priority = observer.checklist_page(root, limit=100, sort="priority", descending=True)
    priorities = [row["priority"] for row in by_priority["items"]]
    assert priorities == sorted(priorities, reverse=True)
    total = stored["pagination"]["total"]
    first = observer.checklist_page(root, limit=1, offset=0, sort="priority")
    second = observer.checklist_page(root, limit=1, offset=1, sort="priority")
    assert first["pagination"]["total"] == second["pagination"]["total"] == total
    assert first["items"][0]["id"] != second["items"][0]["id"]
    assert all("updated" in row for row in stored["items"])
    remaining = observer.checklist_page(root, limit=100, states=["remaining", "blocked"])
    assert 0 < remaining["pagination"]["total"] < total
    assert {row["display_state"] for row in remaining["items"]} <= {"remaining", "blocked"}
    assert observer.checklist_page(root, limit=100, states=["completed"])["items"] == []
    for bad in (
        {"sort": "title"},
        {"sort": "updated", "descending": "yes"},
        {"states": ["unknown"]},
        {"states": ["remaining"] * 9},
    ):
        with pytest.raises(ValueError):
            observer.checklist_page(root, **bad)


def test_sf_parser_distinguishes_count_and_localized_percent():
    row = runner.parse_sf_progress("[mActive=2, mCompleted=2,237, mWaiting=542, mCompleted=80,43%]")
    assert row["fetched"] == 2237 and row["queued"] == 542 and row["inflight"] == 2
    assert "percent" not in row and row["excluded"] is None
    assert runner.parse_sf_progress("80% complete") is None
    assert (
        runner.parse_sf_progress("[mActive=2, mCompleted=4, mWaiting=1, mCompleted=140%]") is None
    )


def test_sf_streams_before_exit_and_bounds_both_diagnostic_pipes(tmp_path):
    script = "import sys,time; print('[mActive=1, mCompleted=12, mWaiting=4, mCompleted=75%]', flush=True); time.sleep(.7); print('x'*100000); print('y'*100000,file=sys.stderr)"
    seen = []
    started = time.monotonic()
    done = runner._run_watched(
        [sys.executable, "-c", script],
        5,
        str(tmp_path),
        lambda x: None,
        on_progress=lambda sample: seen.append((time.monotonic() - started, sample)),
    )
    assert done.returncode == 0 and seen[0][0] < 0.6
    assert seen[0][1]["fetched"] == 12
    assert len(done.stdout) <= runner.SF_OUTPUT_TAIL_CHARS
    assert len(done.stderr) <= runner.SF_OUTPUT_TAIL_CHARS


def test_running_evidence_uses_light_storage_reader_when_available(monkeypatch):
    import seohead.storage.status as storage_status
    from seohead.storage.native_scan import NativeScan

    monkeypatch.setattr(
        NativeScan,
        "observe",
        lambda path: {
            "counts": {"pages": 7, "queued": 2, "inflight": 1, "done": 7, "excluded": 3},
            "validation": "metadata_only",
        },
        raising=False,
    )
    monkeypatch.setattr(
        storage_status, "scan_status", lambda path: pytest.fail("full corpus validation")
    )
    result = observer._read_scan_evidence(
        {"path": "unused.sqlite", "source_kind": "native", "lifecycle": "running"}
    )
    assert result["committed_pages"] == 7
    assert result["committed_page_outcomes"] is None
    assert result["frontier"]["counts"]["inflight"] == 1
    assert result["findings"]["state"] == "unavailable"


def test_large_finding_projection_reports_truncation_without_losing_source_identity():
    result = observer._finding_summary({"id": "stable", "message": "x" * 10000}, 123)
    assert result["id"] == "stable" and result["ordinal"] == 123
    assert len(result["message"]) == 4097 and result["truncated_fields"] == ["message"]


def test_sitemap_run_records_only_measured_documents_and_declared_urls(tmp_path):
    root = _project(tmp_path)
    run = run_observation.start(
        root,
        kind="sitemap",
        mode="sitemap",
        max_urls=0,
        config_fingerprint="synthetic",
        artifact=None,
    )
    pending = run_observation.status(root)["items"][0]
    assert all(value is None for value in pending["counters"].values())
    assert pending["telemetry"]["unit"] == "sitemap_documents"
    run_observation.finish_sitemap(
        root,
        run["id"],
        {
            "ok": True,
            "root": "https://example.test/sitemap.xml",
            "count": 25,
            "sitemaps": [{"url": "https://example.test/sitemap.xml"}],
            "errors": [{"error": "unavailable child"}],
            "truncated": False,
        },
    )
    row = run_observation.status(root)["items"][0]
    assert row["state"] == "partial" and row["finish_reason"] == "source_errors"
    assert row["counters"]["fetched"] == 1
    assert row["source_metadata"]["declared_url_count"] == 25
    assert row["telemetry"]["current_rate_per_second"] is None
    assert "percent" not in row


def test_unsupported_sf_huge_numbers_cannot_abort_a_collector():
    assert (
        runner.parse_sf_progress(
            "[mActive=" + "1" * 5000 + ", mCompleted=1, mWaiting=0, mCompleted=100%]"
        )
        is None
    )


def test_finding_detail_names_projection_omissions_and_preserves_source(tmp_path):
    root = _project(tmp_path)
    path = root / "scans" / "test.sqlite"
    binding = _scan(path)
    write_audit_v2(
        path,
        {"issues": [], "run": {}},
        {"/issues": [{"id": "stable", "message": "x" * 5000, "samples": list(range(30))}]},
        binding,
    )
    detail = observer.finding_detail(root, ordinal=0)
    assert detail["evidence_truncated"] is True
    assert detail["projection_limits"] == {
        "string_chars": 4096,
        "list_items": 20,
        "object_fields": 40,
        "max_depth": 3,
        "source_path": str(path),
    }
    assert len(detail["evidence"]["samples"]) == 20
    with AuditV2Reader(path) as retained:
        original = next(retained.iter_collection("/issues"))
    assert len(original["message"]) == 5000 and len(original["samples"]) == 30


@pytest.fixture(autouse=True)
def _several_active_runs_in_one_process(monkeypatch):
    """These fixtures keep several running records in one process, which start() now refuses."""
    monkeypatch.setattr(run_observation, "_owner_busy", lambda run: False)
