"""A replaced competitor workspace must not make the owner project unreadable."""

import shutil

from seohead.mcp import handlers
from seohead.projects.workspace import create_project


def test_replaced_competitor_workspace_is_reported_not_fatal(tmp_path):
    root = tmp_path / "project"
    create_project(root, "https://seohead.tech/")
    (root / "scans").mkdir(exist_ok=True)
    added = handlers.project_competitors_add(str(root), ["https://www.iana.org/"])
    row = added["competitors"][0] if "competitors" in added else None
    workspace = next((root / "competitors").iterdir())
    shutil.rmtree(workspace)
    create_project(workspace, "https://www.iana.org/")

    status = handlers.project_status(str(root))

    sites = status["checklist"]["sites"] if "checklist" in status else None
    text = repr(status)
    assert "competitor project identity mismatch" in text
    assert "unavailable" in text
    assert row is None or row
    assert sites is None or sites[0]["directory"] == "."
