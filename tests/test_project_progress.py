"""Compact project progress stays bounded and uses the scoped coverage contract."""

from __future__ import annotations

import json

import pytest

from seohead import cli
from seohead.mcp import handlers
from seohead.mcp.mcp_server import build_server
from seohead.projects.coverage import (
    coverage_status,
    initialize_coverage,
    record_execution,
    update_item,
)
from seohead.projects.progress import project_progress
from seohead.projects.workspace import create_project


def _project(tmp_path):
    root = tmp_path / "project"
    create_project(root, "https://example.test/", label="Synthetic project")
    initialize_coverage(root)
    return root


def _edit(root, **item):
    return update_item(root, item, coverage_status(root)["revision"])


def _record(root, item_id, **record):
    return record_execution(root, item_id, record, coverage_status(root)["revision"])


def _plan(item_ids):
    return {
        "reviewer": "Synthetic reviewer",
        "population": {
            "kind": "unknown",
            "size": None,
            "urls": [],
            "name": None,
            "source": "Synthetic scope fixture",
            "reason": "URL population intentionally unknown in this fixture",
            "templates": {},
        },
        "tasks": {"kind": "selection", "ids": item_ids, "source": "Synthetic task selection"},
    }


def test_unscoped_progress_has_no_percent_or_implicit_completion(tmp_path):
    root = _project(tmp_path)
    before = (root / "coverage.json").read_bytes()

    result = project_progress(root, limit=5, offset=0)

    ratio = result["audit_task_completion"]
    assert ratio["label"] == "Audit-task completion"
    assert ratio["percent"] is None
    assert ratio["numerator"] is None and ratio["denominator"] is None
    assert "explicit audit plan" in ratio["reason"]
    assert result["url_population"]["state"] == "unknown"
    assert result["audit_complete"] is False
    assert result["next_actions"][0]["id"] == "scope:agree"
    assert len(result["items"]) <= 5
    assert (root / "coverage.json").read_bytes() == before


def test_task_percent_reuses_core_axis_and_stays_separate_from_url_coverage(tmp_path):
    root = _project(tmp_path)
    _edit(root, id="custom:manual-review", title="Review synthetic page sample")
    initialize_coverage(
        root,
        expected_revision=coverage_status(root)["revision"],
        plan=_plan(["custom:manual-review"]),
    )
    status = _record(
        root,
        "custom:manual-review",
        status="succeeded",
        reason="Synthetic reviewer signed off",
        reviewer="Synthetic reviewer",
        signoff=True,
    )

    result = project_progress(root)
    axis = status["coverage"]["audit_tasks"]
    ratio = result["audit_task_completion"]

    assert result["scope"]["recorded_for_all_sites"] is True
    assert ratio["state"] == "measured"
    assert ratio["numerator"] == axis["numerator"] == 1
    assert ratio["denominator"] == axis["denominator"] == 1
    assert ratio["percent"] == 100.0
    assert result["url_population"]["state"] == "unknown"
    assert result["audit_complete"] is False


def test_state_view_distinguishes_running_blocked_unavailable_stale_review_and_deliverable(
    tmp_path,
):
    root = _project(tmp_path)
    scope = {"site": "https://example.test/", "template": None, "urls": []}
    for item_id, title in (
        ("custom:remaining", "Remaining automatic task"),
        ("custom:running", "Running task"),
        ("custom:unavailable", "Unavailable task"),
        ("custom:stale", "Stale manual review"),
        ("custom:review", "Manual review"),
        ("custom:not-run", "Never attempted manual check"),
        ("custom:deliverable", "Deliverable review"),
    ):
        _edit(root, id=item_id, title=title)
    for item_id in ("custom:remaining", "custom:running", "custom:unavailable"):
        _edit(
            root,
            id=item_id,
            execution_kind="automatic",
            operation="check:BROKEN_PAGE_4XX",
            scope=scope,
        )
    _edit(root, id="custom:blocked", title="Blocked task", dependencies=["custom:remaining"])
    _edit(root, id="custom:deliverable", execution_kind="deliverable")
    item_ids = [
        "custom:remaining",
        "custom:running",
        "custom:unavailable",
        "custom:stale",
        "custom:review",
        "custom:not-run",
        "custom:deliverable",
        "custom:blocked",
    ]
    initialize_coverage(
        root,
        expected_revision=coverage_status(root)["revision"],
        plan=_plan(item_ids),
    )
    _record(root, "custom:running", status="running", reason="Synthetic run in progress")
    _record(root, "custom:unavailable", status="unavailable", reason="Synthetic input missing")
    _record(
        root,
        "custom:stale",
        status="succeeded",
        reason="Synthetic signoff",
        reviewer="Synthetic reviewer",
        signoff=True,
    )
    _edit(root, id="custom:stale", title="Changed after the recorded review")

    result = project_progress(root, limit=100)
    row_states = {item["id"]: item["state"] for item in result["items"]}

    assert row_states["custom:remaining"] == "remaining"
    assert row_states["custom:running"] == "running"
    assert row_states["custom:unavailable"] == "unavailable"
    assert row_states["custom:stale"] == "stale"
    assert row_states["custom:review"] == "not_run"
    assert row_states["custom:not-run"] == "not_run"
    assert row_states["custom:deliverable"] == "deliverable"
    assert row_states["custom:blocked"] == "blocked"
    assert result["state_counts"]["running"] == 1
    assert result["state_counts"]["blocked"] >= 1
    assert result["state_counts"]["unavailable"] == 1
    assert result["state_counts"]["stale"] == 1
    assert result["state_counts"]["not_run"] >= 2
    assert result["state_counts"]["deliverable"] == 1


def test_progress_pages_are_bounded_and_validate_arguments(tmp_path):
    root = _project(tmp_path)
    first = project_progress(root, limit=4, offset=0)
    second = project_progress(root, limit=4, offset=4)

    assert len(first["items"]) == 4
    assert first["pagination"] == {
        "limit": 4,
        "offset": 0,
        "total": second["pagination"]["total"],
        "next_offset": 4,
    }
    assert len({item["id"] for item in first["items"] + second["items"]}) == 8
    assert len(first["next_actions"]) <= 5
    with pytest.raises(ValueError, match="limit"):
        project_progress(root, limit=0)
    with pytest.raises(ValueError, match="offset"):
        project_progress(root, offset=-1)
    with pytest.raises(ValueError, match="limit"):
        project_progress(root, limit=True)


def test_cli_progress_emits_only_the_requested_page(tmp_path, capsys):
    root = _project(tmp_path)

    exit_code = cli.main(
        [
            "project",
            "progress",
            "--directory",
            str(root),
            "--limit",
            "2",
            "--offset",
            "1",
        ]
    )

    assert exit_code == 0
    document = json.loads(capsys.readouterr().out)
    assert len(document["items"]) == 2
    assert document["pagination"]["offset"] == 1


def test_cli_and_mcp_interfaces_share_pagination_arguments(monkeypatch):
    args = cli.build_parser().parse_args(
        ["project", "progress", "--directory", "project", "--limit", "7", "--offset", "14"]
    )
    handler_name, kwargs = cli._build_kwargs("project-progress", args)
    assert handler_name == "project_progress"
    assert kwargs == {"directory": "project", "limit": 7, "offset": 14}
    flat = cli.build_parser().parse_args(
        ["project-progress", "--directory", "project", "--limit", "7", "--offset", "14"]
    )
    assert cli._build_kwargs("project-progress", flat) == (handler_name, kwargs)

    captured = {}

    def capture(**kwargs):
        captured.update(kwargs)
        return {"state": "initialized"}

    monkeypatch.setattr(handlers, "project_progress", capture)
    tool = build_server()._tool_manager.get_tool("seo_project_progress")
    assert tool.fn(directory="project", limit=7, offset=14) == {"state": "initialized"}
    assert captured == {"directory": "project", "limit": 7, "offset": 14}
