"""Inbox entries load in every reader; goal ids are references; journal records checklist attempts."""

from __future__ import annotations

import uuid

import pytest

from seohead import cli
from seohead.projects.coverage import coverage_status, initialize_coverage
from seohead.projects.event_log import page as event_page
from seohead.projects.inbox import list_entries, submit, unread_summary
from seohead.projects.observer import observe
from seohead.projects.workspace import create_project
from tests.test_project_progress import _edit, _project, _record


@pytest.mark.parametrize("kind", ["note", "proposed_goal", "question"])
def test_cli_written_entries_of_every_kind_load_in_every_reader(tmp_path, capsys, kind):
    root = tmp_path / "project"
    create_project(root, "https://example.test/", label="Synthetic inbox fixture")
    assert (
        cli.main(
            [
                "project-inbox-submit",
                "--directory",
                str(root),
                "--text",
                f"Synthetic {kind} from the CLI",
                "--kind",
                kind,
            ]
        )
        == 0
    )
    capsys.readouterr()
    rows = list_entries(root, consumer="owner/test")["entries"]
    assert [row["kind"] for row in rows] == [kind]
    assert unread_summary(root, consumer="owner/test")["count"] == 1
    snapshot = observe(root)
    assert snapshot["inbox"]["pagination"]["total"] == 1


def test_goal_ids_are_accepted_as_inbox_references(tmp_path):
    root = tmp_path / "project"
    create_project(root, "https://example.test/", label="Synthetic inbox fixture")
    goal = submit(root, text="Compare the named competitor", kind="proposed_goal")["entry"]
    note = submit(root, text="Follow up on the goal", references=[goal["id"]])["entry"]
    assert note["references"] == [goal["id"]]
    with pytest.raises(ValueError, match="invalid project reference"):
        submit(root, text="Bogus goal", references=["inbox:not-a-uuid"])
    with pytest.raises(ValueError, match="invalid project reference"):
        submit(root, text="Bogus goal", references=[f"inbox:{uuid.uuid4().hex[:8]}"])


def test_never_attempted_manual_item_is_not_run_not_review(tmp_path):
    from seohead.projects.progress import project_progress

    root = _project(tmp_path)
    _edit(root, id="custom:manual", title="Manual check", execution_kind="manual")
    initialize_coverage(
        root,
        expected_revision=coverage_status(root)["revision"],
        plan={
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
            "tasks": {"kind": "selection", "ids": ["custom:manual"], "source": "Synthetic"},
        },
    )
    states = {row["id"]: row["state"] for row in project_progress(root, limit=100)["items"]}
    assert states["custom:manual"] == "not_run"


def test_checklist_attempts_are_journaled_for_the_desktop_journal(tmp_path):
    root = _project(tmp_path)
    _edit(root, id="custom:attempt", title="Attempted check", execution_kind="manual")
    _record(root, "custom:attempt", status="unavailable", reason="Synthetic input missing")
    events = event_page(root, limit=50)
    texts = [item["text"] for item in events["items"]] if "items" in events else []
    assert any("checklist custom:attempt: unavailable" in text for text in texts), events
    log = (root / "log.md").read_text(encoding="utf-8")
    assert "checklist custom:attempt: unavailable" in log


def test_issues_read_model_counts_each_check_and_pages_past_the_first_twenty(tmp_path):
    from seohead.projects import observer
    from seohead.storage.audit_v2 import write_audit_v2
    from tests.test_scan_audit_v2 import _scan

    root = tmp_path / "project"
    create_project(root, "https://example.test/", label="Synthetic issues fixture")
    path = root / "scans" / "synthetic.sqlite"
    binding = _scan(path)
    issues = [
        {
            "id": str(index),
            "check": "CHECK_A" if index < 25 else "CHECK_B",
            "severity": "warning",
            "message": "synthetic",
            "target_url": f"https://example.test/{index}",
        }
        for index in range(30)
    ]
    write_audit_v2(
        path,
        {"issues": [], "pages": [], "run": {}},
        {"/issues": issues, "/pages": [{"url": "https://example.test/", "text": "x"}]},
        binding,
    )
    findings = observer.observe(root)["scans"]["items"][0]["evidence"]["findings"]
    assert findings["total"] == 30
    assert findings["by_check"] == {"CHECK_A": 25, "CHECK_B": 5}
    assert len(findings["items"]) == 20
    assert findings["next_offset"] == 20
    assert observer.findings_page(root, offset=20, limit=20)["items"][0]["ordinal"] == 20
