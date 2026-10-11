"""A replaced competitor workspace must not make the owner project unreadable."""

import shutil

from seohead.mcp import handlers
from seohead.projects.coverage import initialize_coverage
from seohead.projects.workspace import create_project


def test_replaced_competitor_workspace_is_reported_not_fatal(tmp_path):
    root = tmp_path / "project"
    create_project(root, "https://owner.example.test/")
    initialize_coverage(root)
    handlers.project_competitors_add(str(root), ["https://competitor.example.test/"])
    workspace = next((root / "competitors").iterdir())
    shutil.rmtree(workspace)
    create_project(workspace, "https://competitor.example.test/")

    status = handlers.project_status(str(root))

    checklist = status["checklist"]
    owner, competitor = checklist["sites"]
    assert owner["directory"] == "."
    assert owner["state"] == "initialized"
    assert competitor["state"] == "unavailable"
    assert competitor["reason"] == "competitor project identity mismatch"
    assert checklist["counts"] is None
    assert checklist["counts_known"] is False
    assert checklist["complete"] is False
