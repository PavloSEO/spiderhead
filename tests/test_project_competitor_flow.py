"""Competitor workspaces: crawl the URL as entered, accept the documented consumer key,
open with a damaged competitor, and keep competitor crawl results out of owner items."""

from __future__ import annotations

import json

from seohead.mcp import handlers
from seohead.mcp.handlers import _evidence_root, _project_start_url, _record_scoped_failure
from seohead.projects.coverage import initialize_coverage
from seohead.projects.runtime import add_competitors, preparation_status
from seohead.projects.workspace import create_project, open_project, project_status


def _candidate(url: str) -> dict:
    return {"url": url, "source": "desktop paste", "observed_at": "2026-10-11T00:00:00Z"}


def _owner_with_www_competitor(tmp_path):
    root = tmp_path / "owner"
    create_project(root, "https://owner.example.test/")
    initialize_coverage(root, template=None)
    row = add_competitors(str(root), [_candidate("https://www.iana.org/")])["competitors"][0]
    return root, root / row["directory"]


def test_competitor_crawl_starts_from_the_url_as_entered(tmp_path):
    _, child = _owner_with_www_competitor(tmp_path)

    opened = open_project(child)["project"]

    assert opened["site"]["target"] == "https://iana.org/"
    assert _project_start_url(opened) == "https://www.iana.org/"


def test_owner_project_still_starts_from_its_own_target(tmp_path):
    root, _ = _owner_with_www_competitor(tmp_path)

    assert _project_start_url(open_project(root)["project"]) == "https://owner.example.test/"


def test_competitor_crawl_result_never_reaches_owner_items(tmp_path):
    root, child = _owner_with_www_competitor(tmp_path)
    before = project_status(root)["checklist"]["counts"]

    _record_scoped_failure(root, "https://www.iana.org/", "crawl interrupted")

    assert project_status(root)["checklist"]["counts"] == before
    assert _evidence_root(root, "https://www.iana.org/") is None
    assert _evidence_root(root, "https://owner.example.test/") == root
    assert _evidence_root(child, "https://www.iana.org/") == child


def test_competitors_add_accepts_the_consumer_key(tmp_path):
    root = tmp_path / "owner"
    create_project(root, "https://owner.example.test/")
    candidates = [_candidate("https://one.example.test/")]

    plain = handlers.project_competitors_add(str(root), candidates)
    assert "inbox_unread" not in plain

    with_consumer = handlers.project_competitors_add(
        str(root), [_candidate("https://two.example.test/")], consumer="agent-1"
    )
    assert "inbox_unread" in with_consumer
    assert len(preparation_status(str(root))["competitors"]) == 2


def test_opening_a_project_survives_a_damaged_competitor_workspace(tmp_path):
    root, child = _owner_with_www_competitor(tmp_path)
    manifest = child / "project.json"
    document = json.loads(manifest.read_text(encoding="utf-8"))
    document["project_uuid"] = "00000000-0000-4000-8000-000000000000"
    manifest.write_text(json.dumps(document), encoding="utf-8")

    opened = open_project(root)
    assert opened["ok"] is True
    rows = project_status(root)["checklist"]["sites"]
    assert rows[1]["state"] == "unavailable"
    assert project_status(root)["checklist"]["complete"] is False
