"""The project action log records bounded client and decision outcomes."""

from __future__ import annotations

import json

import pytest

from seohead.core import filesystem
from seohead.projects.action_log import page, record, resolve_pending
from seohead.projects.permissions import require_mcp_allowed
from seohead.projects.runtime import project_write_lock
from seohead.projects.workspace import create_project


@pytest.fixture
def project(tmp_path):
    root = tmp_path / "site"
    create_project(root, "https://example.test/")
    return root


def test_action_log_filters_by_client_outcome_and_text(project):
    record(
        project,
        client="mcp",
        action="project.settings.propose",
        outcome="pending",
        detail="Proposed a label change",
        proposal_id="abc123",
    )
    record(
        project,
        client="cli",
        action="project.settings.review",
        outcome="denied",
        detail="Owner denied the proposal",
        proposal_id="abc123",
    )

    pending = page(project, client="mcp", outcome="pending", query="label")
    denied = page(project, client="cli", outcome="denied")

    assert pending["revision"] == 2
    assert pending["items"][0]["proposal_id"] == "abc123"
    assert denied["items"][0]["client"] == "cli"


def test_owner_only_actions_fail_closed_over_mcp():
    with pytest.raises(ValueError, match="no trusted caller identity"):
        require_mcp_allowed("project.settings.review")
    require_mcp_allowed("project.settings.propose")


def test_action_log_fails_closed_without_a_lock_backend(project, monkeypatch):
    monkeypatch.setattr(filesystem, "fcntl", None)
    monkeypatch.setattr(filesystem, "msvcrt", None)

    with (
        pytest.raises(OSError, match="no supported OS file-lock backend"),
        project_write_lock(project),
    ):
        pass


def test_action_log_fails_fast_when_another_writer_holds_the_lock(project, monkeypatch):
    def contended(_descriptor):
        raise BlockingIOError("busy")

    monkeypatch.setattr(filesystem, "lock_exclusive", contended)

    with (
        pytest.raises(ValueError, match="project workspace is busy"),
        project_write_lock(project, timeout_seconds=0),
    ):
        pass


def test_pending_action_resolves_in_place_without_a_second_slot(project, monkeypatch):
    pending = record(
        project,
        client="mcp",
        action="project.settings.propose",
        outcome="pending",
        detail="A long pending detail reserves the terminal result row.",
        proposal_id="abc123",
    )["action"]
    from seohead.projects import action_log

    path = project / action_log.FILE
    monkeypatch.setattr(action_log, "MAX_BYTES", path.stat().st_size)
    resolved = resolve_pending(
        project,
        pending["sequence"],
        outcome="applied",
        detail="Approved.",
        proposal_id="abc123",
    )

    assert resolved["action"]["sequence"] == pending["sequence"]
    assert resolved["action"]["outcome"] == "applied"
    assert path.stat().st_size <= action_log.MAX_BYTES


def test_page_does_not_create_or_change_project_files(project):
    before = {
        path.relative_to(project): path.read_bytes()
        for path in project.rglob("*")
        if path.is_file()
    }

    assert page(project)["items"] == []

    after = {
        path.relative_to(project): path.read_bytes()
        for path in project.rglob("*")
        if path.is_file()
    }
    assert after == before


def test_page_normalizes_legacy_rows_without_rewriting_them(project):
    path = project / "actions.jsonl"
    row = {
        "sequence": 1,
        "occurred_at": "2026-10-11T10:00:00Z",
        "client": "cli",
        "action": "project.settings.update",
        "outcome": "applied",
        "detail": "Legacy action",
        "proposal_id": None,
    }
    path.write_text(json.dumps(row) + "\n", encoding="utf-8")
    original = path.read_bytes()

    result = page(project)

    assert result["items"][0]["operation_id"] is None
    assert path.read_bytes() == original
