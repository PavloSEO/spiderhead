"""Synthetic offline contracts for bounded and resumable competitor crawl runs."""

from __future__ import annotations

import threading
import time
from pathlib import Path

import pytest

from seohead.projects import competitor_scans, coverage, schedule
from seohead.projects.runtime import add_competitors
from seohead.projects.workspace import create_project


def _project(tmp_path: Path, *, count: int = 1, max_urls: int = 500):
    project = tmp_path / "owner"
    create_project(project, "https://owner.example.test/")
    coverage.initialize_coverage(project, template=None)
    candidates = [f"https://rival-{index}.example.test/" for index in range(count)]
    rows = add_competitors(str(project), candidates)["competitors"]
    for row in rows:
        child = project / row["directory"]
        schedule.configure(
            child,
            expected_revision=0,
            crawl={"max_urls": max_urls, "max_requests": 30_000},
        )
    definition = {
        "id": "custom:competitor-audit",
        "title": "Audit selected competitors",
        "competitors": [
            {"url": row["url"], "max_urls": max_urls} for row in rows
        ],
    }
    current = coverage.coverage_status(project)
    coverage.update_item(project, definition, expected_revision=current["revision"])
    final = coverage.coverage_status(project)
    return project, rows, final["revision"]


def _scan_record(*, partial: bool = False):
    return {
        "scan_uuid": "00000000-0000-4000-8000-000000000001",
        "crawl_partial": partial,
        "corpus_partial": False,
        "finish_reason": "request_budget" if partial else "complete",
    }


def _write_artifact(kwargs):
    path = Path(kwargs.get("scan_out") or kwargs["resume"])
    path.parent.mkdir(parents=True, exist_ok=True)
    if not path.exists():
        path.write_bytes(b"synthetic retained scan")
    return path


def _kwargs(project, revision, *, tools, **overrides):
    return {
        "directory": project,
        "task_id": "custom:competitor-audit",
        "expected_revision": revision,
        "confirm": True,
        "tools": tools,
        **overrides,
    }


def test_injected_crawler_is_validated_before_claiming_project_lock(tmp_path):
    project, _, revision = _project(tmp_path)

    with pytest.raises(ValueError, match="crawl_site"):
        competitor_scans.scan_task_competitors(**_kwargs(project, revision, tools={}))

    assert not (project / competitor_scans.RUN_LOCK).exists()


def test_aggregate_request_budget_refuses_before_crawl_or_lock(tmp_path):
    project, _, revision = _project(tmp_path, count=5, max_urls=10_000)
    calls = []

    def crawl(**kwargs):
        calls.append(kwargs)
        return {"ok": True}

    with pytest.raises(ValueError, match="aggregate competitor request cap"):
        competitor_scans.scan_task_competitors(
            **_kwargs(project, revision, tools={"crawl_site": crawl})
        )

    assert not calls
    assert not (project / competitor_scans.RUN_LOCK).exists()


def test_successful_run_records_per_site_and_fixed_budgets(tmp_path, monkeypatch):
    project, rows, revision = _project(tmp_path)
    calls = []

    def crawl(**kwargs):
        calls.append(kwargs)
        artifact = _write_artifact(kwargs)
        kwargs["progress"](4, 2)
        return {"ok": True, "scan": str(artifact)}

    monkeypatch.setattr(competitor_scans, "_read_scan", lambda _path: _scan_record())
    result = competitor_scans.scan_task_competitors(
        **_kwargs(project, revision, tools={"crawl_site": crawl})
    )

    assert result["state"] == "completed" and result["ok"] is True
    assert result["budgets"] == {
        "urls": 500,
        "requests": 1_500,
        "seconds": 1_000,
        "max_urls": competitor_scans.MAX_TOTAL_URLS,
        "max_requests": competitor_scans.MAX_TOTAL_REQUESTS,
        "max_seconds": competitor_scans.MAX_TOTAL_SECONDS,
    }
    assert len(calls) == 1
    assert calls[0]["max_urls"] == 500
    assert calls[0]["overrides"]["limits.max_requests"] == 1_500
    assert calls[0]["overrides"]["limits.max_crawl_seconds"] == 1_000
    assert result["results"][0]["project_uuid"] == rows[0]["project_uuid"]
    assert result["results"][0]["fetched"] == 4


def test_recovery_skips_completed_child_and_resumes_only_interrupted_child(tmp_path, monkeypatch):
    project, rows, revision = _project(tmp_path, count=2, max_urls=100)
    run_id = "00000000-0000-4000-8000-000000000042"
    calls = []
    second_url = rows[1]["url"]

    def interrupted_crawl(**kwargs):
        calls.append(kwargs)
        artifact = _write_artifact(kwargs)
        if kwargs.get("url") == second_url:
            raise KeyboardInterrupt("synthetic interruption")
        return {"ok": True, "scan": str(artifact)}

    monkeypatch.setattr(competitor_scans, "_read_scan", lambda path: _scan_record())
    with pytest.raises(KeyboardInterrupt):
        competitor_scans.scan_task_competitors(
            **_kwargs(
                project,
                revision,
                tools={"crawl_site": interrupted_crawl},
                concurrency=1,
                run_id=run_id,
            )
        )

    status = competitor_scans.get_competitor_scan_run(project, run_id)
    assert status["state"] == "interrupted"
    assert status["results"][0]["state"] == "completed"
    assert status["results"][1]["state"] == "interrupted"
    interrupted_path = project / status["results"][1]["scan"]
    recovery_reads = 0

    second_calls = []

    def resume_crawl(**kwargs):
        second_calls.append(kwargs)
        assert "resume" in kwargs and "scan_out" not in kwargs
        return {"ok": True, "scan": kwargs["resume"]}

    def recovery_records(path):
        nonlocal recovery_reads
        if Path(path).resolve() == interrupted_path.resolve() and recovery_reads == 0:
            recovery_reads += 1
            return _scan_record(partial=True)
        return _scan_record()

    monkeypatch.setattr(competitor_scans, "_read_scan", recovery_records)
    recovered = competitor_scans.scan_task_competitors(
        **_kwargs(
            project,
            revision,
            tools={"crawl_site": resume_crawl},
            concurrency=1,
            run_id=run_id,
        )
    )

    assert recovered["state"] == "completed"
    assert recovered["resumed"] is True
    assert len(second_calls) == 1
    assert second_calls[0]["project"] == str(project / rows[1]["directory"])
    assert len(calls) == 2


def test_run_id_status_and_cancel_marker_are_durable(tmp_path, monkeypatch):
    project, _, revision = _project(tmp_path)
    run_id = "00000000-0000-4000-8000-000000000043"
    started = threading.Event()
    errors = []

    def waiting_crawl(**kwargs):
        while True:
            started.set()
            kwargs["progress"](1, 1)
            time.sleep(0.01)

    monkeypatch.setattr(competitor_scans, "_read_scan", lambda _path: _scan_record(partial=True))

    def run():
        try:
            competitor_scans.scan_task_competitors(
                **_kwargs(
                    project,
                    revision,
                    tools={"crawl_site": waiting_crawl},
                    run_id=run_id,
                )
            )
        except BaseException as exc:
            errors.append(exc)

    thread = threading.Thread(target=run)
    thread.start()
    assert started.wait(timeout=5)
    active = competitor_scans.get_competitor_scan_run(project, run_id)
    assert active["state"] == "running" and active["runner_active"] is True
    requested = competitor_scans.cancel_competitor_scan_run(project, run_id)
    assert requested["cancelled"] is True
    thread.join(timeout=5)

    assert not thread.is_alive()
    assert any(isinstance(error, KeyboardInterrupt) for error in errors)
    final = competitor_scans.get_competitor_scan_run(project, run_id)
    assert final["state"] == "interrupted"
    assert final["results"][0]["state"] == "interrupted"
