"""Project journal: manual and automatic entries land in events.jsonl and log.md."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from seohead.projects import journal, run_observation
from seohead.projects.coverage import (
    coverage_status,
    initialize_coverage,
    load_catalogue,
    record_execution,
)
from seohead.projects.event_log import FILE
from seohead.projects.workspace import create_project


@pytest.fixture
def project(tmp_path) -> Path:
    root = tmp_path / "project"
    create_project(root, "https://example.test/")
    initialize_coverage(root)
    return root


def _events(root: Path) -> list[dict]:
    return [json.loads(line) for line in (root / FILE).read_text(encoding="utf-8").splitlines()]


def _log_bullets(root: Path) -> list[str]:
    return [
        line
        for line in (root / "log.md").read_text(encoding="utf-8").splitlines()
        if line.startswith("- ")
    ]


def test_record_writes_event_and_mirrors_bullet_to_log(project):
    result = journal.record(project, source="user", actor="user", text="Pause\nthe crawl")

    assert result["ok"] is True
    assert _events(project)[-1]["text"] == "Pause\nthe crawl"
    assert _log_bullets(project)[-1].endswith("user/user · Pause the crawl")


def test_manual_mcp_path_also_mirrors_to_log(project):
    from seohead.mcp.project_handlers import project_event_append

    project_event_append(str(project), "agent", "agent", "Checked robots by hand")

    assert _log_bullets(project)[-1].endswith("agent/agent · Checked robots by hand")


def test_note_is_silent_for_a_directory_that_is_not_a_project(tmp_path):
    assert journal.note(tmp_path, source="scans", actor="agent", text="crawl x finished") is None
    assert list(tmp_path.iterdir()) == []


def test_crawl_finish_is_journaled(project):
    run = run_observation.start(
        str(project),
        kind="native",
        mode="spider",
        max_urls=100,
        config_fingerprint="synthetic",
        artifact=project / "scans" / "live.sqlite",
    )

    run_observation.finish(str(project), run["id"], state="finished", reason="all URLs fetched")

    text = f"crawl {run['id']} finished: all URLs fetched"
    assert _events(project)[-1] == {**_events(project)[-1], "source": "scans", "text": text}
    assert _log_bullets(project)[-1].endswith(f"scans/agent · {text}")


def test_run_start_and_first_measured_progress_are_journaled(project):
    run = run_observation.start(
        str(project),
        kind="native",
        mode="spider",
        max_urls=100,
        config_fingerprint="synthetic",
        artifact=project / "scans" / "live.sqlite",
    )

    run_observation.progress(
        str(project), run["id"], fetched=2, queued=3, inflight=1, excluded=0
    )
    run_observation.progress(
        str(project), run["id"], fetched=3, queued=2, inflight=1, excluded=0
    )

    rows = _events(project)
    assert [row["text"] for row in rows] == [
        f"run {run['id']} started (native/spider)",
        f"run {run['id']} progress: fetched=2, queued=3, inflight=1",
    ]
    assert len(_log_bullets(project)) == 2


def test_checklist_attempt_is_journaled(project):
    item_id = next(iter(load_catalogue()))
    record_execution(
        project,
        item_id,
        {"status": "running", "reason": "Synthetic attempt"},
        coverage_status(project)["revision"],
    )

    last = _events(project)[-1]
    assert last["source"] == "agent"
    assert last["text"] == f"checklist {item_id}: running"


def test_report_build_is_journaled_for_project_reports(project, monkeypatch):
    from seohead import reports

    monkeypatch.setattr(
        reports,
        "_build_report",
        lambda *args, **kwargs: {"ok": True, "outputs": [str(project / "reports" / "r.xlsx")]},
    )

    assert reports.build_report({}, "xlsx", project=str(project))["ok"] is True
    assert _events(project)[-1]["text"].startswith("report xlsx built: ")


def test_failed_report_build_is_not_journaled(project, monkeypatch):
    from seohead import reports

    monkeypatch.setattr(
        reports, "_build_report", lambda *args, **kwargs: {"ok": False, "error": "x"}
    )

    reports.build_report({}, "xlsx", project=str(project))

    assert not (project / FILE).exists()
    assert _log_bullets(project) == []
