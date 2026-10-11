"""Competitor candidates can be recorded without a preparation crawl."""

from __future__ import annotations

import pytest

from seohead.projects.runtime import add_competitors, preparation_status, prepare_project
from seohead.projects.workspace import create_project


def _candidate(url: str) -> dict:
    return {"url": url, "source": "desktop paste", "observed_at": "2026-10-11T00:00:00Z"}


def test_adds_candidates_as_separate_workspaces_without_crawling(tmp_path):
    root = tmp_path / "owner"
    create_project(root, "https://owner.example.test/")

    result = add_competitors(str(root), [_candidate("https://one.example.test/")])

    assert result["ok"] is True
    row = result["competitors"][0]
    assert row["url"] == "https://one.example.test/"
    assert row["state"] == "candidate; audit not run"
    assert (root / row["directory"] / "project.json").is_file()
    stored = preparation_status(str(root))
    assert stored["competitors"][0]["project_uuid"] == row["project_uuid"]
    assert stored["steps"]["competitors"]["count"] == 1
    assert not list((root / row["directory"]).glob("scans/*.sqlite"))


def test_second_call_keeps_earlier_candidates_and_refuses_duplicates(tmp_path):
    root = tmp_path / "owner"
    create_project(root, "https://owner.example.test/")
    add_competitors(str(root), [_candidate("https://one.example.test/")])

    result = add_competitors(str(root), [_candidate("https://two.example.test/")])
    assert [row["url"] for row in result["competitors"]] == [
        "https://one.example.test/",
        "https://two.example.test/",
    ]
    with pytest.raises(ValueError, match="already recorded"):
        add_competitors(str(root), [_candidate("https://one.example.test/")])
    assert len(preparation_status(str(root))["competitors"]) == 2


def test_refuses_primary_site_batch_duplicates_and_limit(tmp_path):
    root = tmp_path / "owner"
    create_project(root, "https://owner.example.test/")

    with pytest.raises(ValueError, match="primary site"):
        add_competitors(str(root), [_candidate("https://owner.example.test/")])
    with pytest.raises(ValueError, match="duplicate"):
        add_competitors(
            str(root),
            [_candidate("https://one.example.test/"), _candidate("https://one.example.test/")],
        )
    too_many = [_candidate(f"https://c{i}.example.test/") for i in range(6)]
    with pytest.raises(ValueError, match="project limit"):
        add_competitors(str(root), too_many)
    assert preparation_status(str(root))["state"] == "pending"


def test_refuses_while_preparation_lock_is_held(tmp_path):
    root = tmp_path / "owner"
    create_project(root, "https://owner.example.test/")
    (root / ".prepare.lock").write_text("", encoding="utf-8")

    with pytest.raises(ValueError, match="already running"):
        add_competitors(str(root), [_candidate("https://one.example.test/")])
    assert (root / ".prepare.lock").exists()


def test_preserves_prepared_state_written_by_preparation(tmp_path):
    root = tmp_path / "owner"
    create_project(root, "https://owner.example.test/")
    prepare_project(str(root), tools={"crawl_site": lambda **_: {"ok": False, "error": "offline"}})
    before = preparation_status(str(root))

    add_competitors(str(root), [_candidate("https://one.example.test/")])

    after = preparation_status(str(root))
    assert after["steps"]["crawl"] == before["steps"]["crawl"]
    assert after["revision"] == before["revision"] + 1
    assert len(after["competitors"]) == 1


def test_list_and_remove_keep_the_entered_url_and_leave_the_workspace(tmp_path):
    from seohead.projects.runtime import list_competitors, remove_competitor

    root = tmp_path / "owner"
    create_project(root, "https://owner.example.test/")
    row = add_competitors(str(root), [_candidate("https://www.iana.org/")])["competitors"][0]

    listed = list_competitors(str(root))["competitors"]
    assert [item["url"] for item in listed] == ["https://www.iana.org/"]

    removed = remove_competitor(str(root), "https://www.iana.org")
    assert removed["removed"] == "https://www.iana.org/"
    assert removed["workspace_kept"] == row["directory"]
    assert removed["competitors"] == []
    assert (root / row["directory"] / "project.json").is_file()
    assert preparation_status(str(root))["steps"]["competitors"]["state"] == "not_run"


def test_remove_refuses_an_unrecorded_competitor(tmp_path):
    from seohead.projects.runtime import remove_competitor

    root = tmp_path / "owner"
    create_project(root, "https://owner.example.test/")
    add_competitors(str(root), [_candidate("https://one.example.test/")])

    with pytest.raises(ValueError, match="not recorded"):
        remove_competitor(str(root), "https://two.example.test/")
    assert len(preparation_status(str(root))["competitors"]) == 1


def test_www_candidate_is_accepted_by_preparation_and_observer(tmp_path):
    from seohead.projects.observer import observe

    root = tmp_path / "owner"
    create_project(root, "https://owner.example.test/")
    prepare_project(
        str(root),
        tools={"crawl_site": lambda **_: {"ok": False, "error": "offline"}},
        competitors=[_candidate("https://www.competitor.example.test/")],
    )

    stored = preparation_status(str(root))["competitors"][0]
    assert stored["url"] == "https://www.competitor.example.test/"
    snapshot = observe(str(root))
    assert snapshot["sites"]["total"] == 2
    assert snapshot["sites"]["items"][1]["role"] == "competitor"


def test_www_duplicate_of_a_recorded_competitor_is_refused(tmp_path):
    root = tmp_path / "owner"
    create_project(root, "https://owner.example.test/")
    add_competitors(str(root), [_candidate("https://competitor.example.test/")])

    with pytest.raises(ValueError, match="already recorded"):
        add_competitors(str(root), [_candidate("https://www.competitor.example.test/")])


def test_observer_with_competitors_and_an_accepted_goal_does_not_fail(tmp_path):
    from seohead.projects.inbox import set_goal_state, submit
    from seohead.projects.observer import observe

    root = tmp_path / "owner"
    create_project(root, "https://owner.example.test/")
    add_competitors(str(root), [_candidate("https://www.one.example.test/")])
    goal = submit(root, text="Audit the owner site", kind="proposed_goal")["entry"]
    set_goal_state(root, entry_id=goal["id"], state="accepted")

    snapshot = observe(str(root), scan_limit=5)

    assert snapshot["sites"]["total"] == 2
    task_id = "custom:goal-" + goal["id"].removeprefix("inbox:")
    assert task_id in {item["item_id"] for item in snapshot["active_tasks"]["items"]}


def test_cli_and_mcp_expose_competitor_list_and_remove():
    import asyncio

    from seohead.cli import COMMANDS
    from seohead.core.input_contracts import COMMAND_CONTRACTS
    from seohead.mcp.mcp_server import build_server

    names = {"project-competitors-add", "project-competitors-list", "project-competitors-remove"}
    assert names <= set(COMMANDS)
    assert names <= {contract.command for contract in COMMAND_CONTRACTS}
    tools = asyncio.run(build_server().list_tools())
    tool_names = {tool.name for tool in tools}
    assert {
        "seo_project_competitors_add",
        "seo_project_competitors_list",
        "seo_project_competitors_remove",
    } <= tool_names
