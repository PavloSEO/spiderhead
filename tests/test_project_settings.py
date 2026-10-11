"""Revisioned project settings, owner proposals and recoverable deletion."""

from __future__ import annotations

from pathlib import Path

import pytest

from seohead.projects.action_log import page as action_page
from seohead.projects.project_settings import (
    delete_project,
    list_proposals,
    propose_settings,
    read_settings,
    review_proposal,
    update_label,
)
from seohead.projects.run_observation import start
from seohead.projects.workspace import create_project
from seohead.core import filesystem


@pytest.fixture
def project(tmp_path):
    root = tmp_path / "site"
    create_project(root, "https://example.test/", label="Example")
    return root


def test_project_label_update_requires_current_revision(project):
    assert read_settings(project)["revision"] == 0
    updated = update_label(project, "Updated site", expected_revision=0)

    assert updated["revision"] == 1
    assert updated["settings"]["label"] == "Updated site"
    with pytest.raises(ValueError, match="revision conflict"):
        update_label(project, "Stale", expected_revision=0)


def test_mcp_settings_proposal_needs_local_owner_review(project):
    created = propose_settings(project, {"label": "Proposed label"})
    assert created["proposal"]["status"] == "pending"
    assert read_settings(project)["settings"]["label"] == "Example"

    reviewed = review_proposal(
        project,
        created["proposal"]["id"],
        "approve",
        expected_revision=created["revision"],
    )

    assert reviewed["proposal"]["status"] == "applied"
    assert read_settings(project)["settings"]["label"] == "Proposed label"
    assert list_proposals(project)["items"][0]["id"] == created["proposal"]["id"]
    assert action_page(project, client="mcp", outcome="pending")["total"] == 1


def test_settings_proposal_lock_fails_fast_when_contended(project, monkeypatch):
    def contended(_descriptor):
        raise BlockingIOError("busy")

    monkeypatch.setattr(filesystem, "lock_exclusive", contended)

    with pytest.raises(ValueError, match="busy"):
        propose_settings(project, {"label": "Pending"})


def test_project_delete_moves_exports_to_sibling_trash(project):
    export = project / "reports" / "kept.txt"
    export.write_text("synthetic", encoding="utf-8")

    result = delete_project(project)

    trash_path = Path(result["trash_path"])
    assert result["exports_retained"] is True
    assert not project.exists()
    assert (trash_path / "reports" / "kept.txt").read_text(encoding="utf-8") == "synthetic"
    assert action_page(trash_path, client="cli", outcome="deleted")["total"] == 1


def test_project_delete_refuses_a_live_run(project):
    start(
        project,
        kind="native",
        mode="spider",
        max_urls=10,
        config_fingerprint="synthetic",
        artifact=project / "scans" / "live.sqlite",
    )

    with pytest.raises(ValueError, match="active run"):
        delete_project(project)
    assert project.is_dir()
