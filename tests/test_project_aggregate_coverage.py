"""Aggregate project coverage must not inherit a primary site's known denominator."""

from __future__ import annotations

import json

from seohead.projects.coverage import coverage_status, initialize_coverage
from seohead.projects.runtime import PREPARATION_FORMAT, aggregate_coverage
from seohead.projects.workspace import create_project


def test_uninitialized_competitor_withholds_aggregate_state_and_counts(tmp_path):
    root = tmp_path / "project"
    primary = create_project(root, "https://example.test/")["project"]
    initialize_coverage(root)
    child = root / "competitors" / "candidate"
    child.parent.mkdir()
    competitor = create_project(child, "https://competitor.test/")["project"]
    (root / "preparation.json").write_text(
        json.dumps(
            {
                "format": PREPARATION_FORMAT,
                "project_uuid": primary["project_uuid"],
                "revision": 1,
                "state": "partial",
                "steps": {},
                "competitors": [
                    {
                        "url": competitor["site"]["target"],
                        "directory": "competitors/candidate",
                        "project_uuid": competitor["project_uuid"],
                    }
                ],
            }
        ),
        encoding="utf-8",
    )
    result = aggregate_coverage(str(root), coverage_status(root))
    assert result["state"] == "not_initialized"
    assert result["complete"] is False
    assert result["counts_known"] is False
    assert result["counts"] is None
    assert result["items"] == []
    assert result["sites"][0]["state"] == "initialized"
    assert result["sites"][1]["state"] == "not_initialized"


def test_competitor_manifest_missing_site_leaves_only_its_row_unavailable(tmp_path):
    root = tmp_path / "project"
    primary = create_project(root, "https://example.test/")["project"]
    initialize_coverage(root)
    child = root / "competitors" / "candidate"
    child.parent.mkdir()
    competitor = create_project(child, "https://competitor.test/")["project"]
    manifest = child / "project.json"
    damaged = json.loads(manifest.read_text(encoding="utf-8"))
    del damaged["site"]
    manifest.write_text(json.dumps(damaged), encoding="utf-8")
    (root / "preparation.json").write_text(
        json.dumps(
            {
                "format": PREPARATION_FORMAT,
                "project_uuid": primary["project_uuid"],
                "revision": 1,
                "state": "partial",
                "steps": {},
                "competitors": [
                    {
                        "url": competitor["site"]["target"],
                        "directory": "competitors/candidate",
                        "project_uuid": competitor["project_uuid"],
                    }
                ],
            }
        ),
        encoding="utf-8",
    )
    result = aggregate_coverage(str(root), coverage_status(root))
    assert result["sites"][0]["state"] == "initialized"
    assert result["sites"][1]["state"] == "unavailable"
    assert result["sites"][1]["directory"] == "competitors/candidate"
