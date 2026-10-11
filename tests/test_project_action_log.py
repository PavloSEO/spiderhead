"""The project action log records bounded client and decision outcomes."""

from __future__ import annotations

import pytest

from seohead.projects.action_log import page, record
from seohead.projects.permissions import require_mcp_allowed
from seohead.projects.workspace import create_project
from seohead.core import filesystem


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

    with pytest.raises(ValueError, match="locking is unavailable"):
        record(
            project,
            client="mcp",
            action="agent.heartbeat",
            outcome="applied",
        )


def test_action_log_fails_fast_when_another_writer_holds_the_lock(project, monkeypatch):
    def contended(_descriptor):
        raise BlockingIOError("busy")

    monkeypatch.setattr(filesystem, "lock_exclusive", contended)

    with pytest.raises(ValueError, match="busy"):
        record(
            project,
            client="mcp",
            action="agent.heartbeat",
            outcome="applied",
        )
