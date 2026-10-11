"""Owner task metadata is revisioned, attributed and separate from execution approval."""

from __future__ import annotations

import json

from seohead.projects.coverage import append_task_comment, coverage_status, initialize_coverage, update_item
from seohead.projects.observer import checklist_page, task_detail
from seohead.projects.workspace import create_project


def test_custom_task_metadata_comments_and_unknown_affected_urls(tmp_path):
    root = tmp_path / "project"
    create_project(root, "https://shop.example.test/")
    initialized = initialize_coverage(root)
    task_id = "custom:canonical-review"
    update = update_item(
        root,
        {
            "id": task_id,
            "title": "Review the canonical correction",
            "assignee": "web-team",
            "description": "Confirm the canonical points to the preferred URL.",
            "acceptance_criterion": "The retained rescan shows the expected canonical.",
            "finding_ref": {
                "scan_id": "scan-synthetic-2026-10-12",
                "finding_id": "finding-synthetic-001",
            },
            "method_ref": "scenario:project-control",
            "competitors": [{"url": "https://competitor.example.test/", "max_urls": 250}],
        },
        initialized["revision"],
    )

    row = next(
        item
        for item in checklist_page(root, query="canonical-review")["items"]
        if item["id"] == task_id
    )
    assert row["task"]["executor"] == "web-team"
    assert row["task"]["state"] == "remaining"
    assert row["task"]["competitors"] == [
        {"url": "https://competitor.example.test/", "max_urls": 250}
    ]
    assert row["affected_urls"] == {
        "count": None,
        "state": "not_measured",
        "reason": "the retained task measurement does not enumerate affected URLs",
    }

    append_task_comment(
        root,
        item_id=task_id,
        text="The finding is ready for owner review.",
        actor="agent/review-session",
        expected_revision=update["revision"],
        reference={
            "kind": "finding",
            "scan_id": "scan-synthetic-2026-10-12",
            "id": "finding-synthetic-001",
        },
    )
    detail = task_detail(root, item_id=task_id)
    assert detail["item"]["task"]["comments_total"] == 1
    assert detail["item"]["task"]["comments"][0]["actor"] == "agent/review-session"
    assert detail["item"]["task"]["comments"][0]["reference"]["kind"] == "finding"
    document = json.loads((root / "coverage.json").read_text(encoding="utf-8"))
    assert document["format"] == "seohead.coverage.v4"
    assert coverage_status(root)["task_kpis"]["total"] >= 1
