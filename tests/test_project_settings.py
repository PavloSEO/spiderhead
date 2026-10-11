"""Revisioned project settings, owner proposals and recoverable deletion."""

from __future__ import annotations

import copy
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
from seohead.projects import project_settings as settings_module
from seohead.projects.run_observation import start
from seohead.projects.runtime import DEFAULT_POLICY, project_policy
from seohead.projects.workspace import create_project


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
    created = propose_settings(project, {"label": "Proposed label"}, expected_revision=0)
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
    assert read_settings(project)["operation_id"] == created["proposal"]["id"]
    assert list_proposals(project)["items"][0]["id"] == created["proposal"]["id"]
    assert action_page(project, client="mcp", outcome="applied")["total"] == 1


def test_settings_proposal_lock_fails_fast_when_contended(project, monkeypatch):
    from contextlib import contextmanager

    @contextmanager
    def contended(_root):
        raise ValueError("another writer owns the project workspace")
        yield

    monkeypatch.setattr(settings_module, "project_write_lock", contended)

    with pytest.raises(ValueError, match="busy"):
        propose_settings(project, {"label": "Pending"}, expected_revision=0)


def test_failed_proposal_write_resolves_its_reserved_action_row(project, monkeypatch):
    original_write = settings_module.write_document

    def fail_proposal_store(root, name, data, *, expected_revision=None):
        if name == settings_module.PROPOSALS_FILE:
            raise OSError("synthetic proposal-store failure")
        return original_write(root, name, data, expected_revision=expected_revision)

    monkeypatch.setattr(settings_module, "write_document", fail_proposal_store)
    with pytest.raises(OSError, match="synthetic proposal-store failure"):
        propose_settings(project, {"label": "Not persisted"}, expected_revision=0)

    assert list_proposals(project)["total"] == 0
    assert action_page(project, client="mcp", outcome="refused")["total"] == 1


def test_owner_review_recovers_after_setting_applied_but_proposal_finalize_failed(
    project, monkeypatch
):
    created = propose_settings(project, {"label": "Recovery"}, expected_revision=0)
    original_write = settings_module.write_document
    fail_finalize = True

    def write_document(root, name, data, *, expected_revision=None):
        nonlocal fail_finalize
        if (
            name == settings_module.PROPOSALS_FILE
            and data["proposals"][-1]["status"] == "applied"
            and fail_finalize
        ):
            fail_finalize = False
            raise OSError("synthetic finalize failure")
        return original_write(root, name, data, expected_revision=expected_revision)

    monkeypatch.setattr(settings_module, "write_document", write_document)
    with pytest.raises(OSError, match="synthetic finalize failure"):
        review_proposal(
            project,
            created["proposal"]["id"],
            "approve",
            expected_revision=created["revision"],
        )
    assert read_settings(project)["settings"]["label"] == "Recovery"
    applying = list_proposals(project)
    assert applying["items"][0]["status"] == "applying"

    recovered = review_proposal(
        project,
        created["proposal"]["id"],
        "approve",
        expected_revision=applying["revision"],
    )

    assert recovered["proposal"]["status"] == "applied"
    assert action_page(project, client="mcp", outcome="applied")["total"] == 1


def test_policy_proposal_retry_uses_persisted_operation_id(project, monkeypatch):
    policy = copy.deepcopy(DEFAULT_POLICY)
    policy["approval_thresholds"]["pages"] = 777
    created = propose_settings(project, {"crawl_policy": policy}, expected_revision=0)
    original_write = settings_module.write_document
    fail_finalize = True

    def write_document(root, name, data, *, expected_revision=None):
        nonlocal fail_finalize
        if (
            name == settings_module.PROPOSALS_FILE
            and data["proposals"][-1]["status"] == "applied"
            and fail_finalize
        ):
            fail_finalize = False
            raise OSError("synthetic policy finalize failure")
        return original_write(root, name, data, expected_revision=expected_revision)

    monkeypatch.setattr(settings_module, "write_document", write_document)
    with pytest.raises(OSError, match="synthetic policy finalize failure"):
        review_proposal(
            project,
            created["proposal"]["id"],
            "approve",
            expected_revision=created["revision"],
        )
    current = project_policy(str(project))
    assert current["operation_id"] == created["proposal"]["id"]

    applying = list_proposals(project)
    recovered = review_proposal(
        project,
        created["proposal"]["id"],
        "approve",
        expected_revision=applying["revision"],
    )

    assert recovered["proposal"]["status"] == "applied"
    assert action_page(project, client="mcp", outcome="applied")["total"] == 1


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


def test_project_delete_does_not_move_while_shared_writer_lock_is_unavailable(
    project, monkeypatch
):
    from contextlib import contextmanager
    from seohead.projects import runtime

    @contextmanager
    def busy(_root):
        raise ValueError("another writer owns the project workspace")
        yield

    monkeypatch.setattr(runtime, "project_write_lock", busy)

    with pytest.raises(ValueError, match="another writer"):
        delete_project(project)
    assert project.is_dir()
