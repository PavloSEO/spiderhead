"""Durable observer inbox: no action is implied by a notification."""

from __future__ import annotations

import os
from concurrent.futures import ThreadPoolExecutor
from multiprocessing import get_context

import pytest

from seohead.mcp.mcp_server import build_server
from seohead.projects.coverage import coverage_status, initialize_coverage, update_item
from seohead.projects.inbox import (
    acknowledge,
    fingerprint,
    list_entries,
    mark_read,
    set_goal_state,
    submit,
    triage,
    unread_summary,
)
from seohead.projects.observer import finding_detail, findings_page, observe
from seohead.projects.workspace import create_project
from tests.test_scan_history import _finished


def _project(tmp_path):
    root = tmp_path / "synthetic-project"
    create_project(root, "https://example.test/", label="Synthetic observer fixture")
    return root


def _files(root):
    return {
        path.relative_to(root): path.read_bytes()
        for path in sorted(root.rglob("*"))
        if path.is_file()
    }


def _separate_writer(directory: str) -> None:
    submit(directory, text="Written by a separate collector", references=["scan:synthetic-process"])


def _bound_mcp_observer(directory: str, consumer: str, allowlist: str, queue) -> None:
    """Spawn target: each stdio host reads only its own startup binding."""
    os.environ["SEOHEAD_MCP_CONSUMER_ID"] = consumer
    os.environ["SEOHEAD_MCP_PROJECT_ALLOWLIST"] = allowlist
    server = build_server()
    result = server._tool_manager.get_tool("seo_project_observe").fn(directory=directory)
    queue.put(result.get("inbox_unread"))


def _synthetic_crawl(**_kwargs) -> dict:
    """Keep MCP inbox tests offline and independent of a crawl target."""
    return {"ok": True, "state": "synthetic"}


def _bound_mcp_crawl(directory: str, consumer: str, allowlist: str, queue) -> None:
    """Spawn target: a fresh MCP host binds crawl's project workspace too."""
    os.environ["SEOHEAD_MCP_CONSUMER_ID"] = consumer
    os.environ["SEOHEAD_MCP_PROJECT_ALLOWLIST"] = allowlist
    from seohead.mcp import handlers

    handlers.crawl_site = _synthetic_crawl
    server = build_server()
    result = server._tool_manager.get_tool("seo_crawl_site").fn(project=directory)
    queue.put(result.get("inbox_unread"))


def test_note_read_ack_and_goal_transitions_are_explicit_and_durable(tmp_path):
    root = _project(tmp_path)
    note = submit(root, text="Review sitemap gap", references=["scan:synthetic-1"])["entry"]
    goal = submit(root, text="Compare the named competitor", kind="proposed_goal")["entry"]

    before = unread_summary(root, consumer="agent/session-a")
    assert before["count"] == 2
    inspected = mark_read(root, consumer="agent/session-a", entry_ids=[note["id"]])
    assert inspected["entries"][0]["read_at"] and inspected["entries"][0]["acknowledged_at"] is None
    assert unread_summary(root, consumer="agent/session-a")["count"] == 2
    acknowledged = acknowledge(root, consumer="agent/session-a", entry_ids=[note["id"]])
    assert acknowledged["entries"][0]["acknowledged_at"]
    assert unread_summary(root, consumer="agent/session-a")["count"] == 1
    assert (
        set_goal_state(root, entry_id=goal["id"], state="accepted")["entry"]["goal_state"]
        == "accepted"
    )
    assert (
        set_goal_state(root, entry_id=goal["id"], state="completed")["entry"]["goal_state"]
        == "completed"
    )
    with pytest.raises(ValueError, match="proposed goal"):
        set_goal_state(root, entry_id=note["id"], state="accepted")


def test_goal_transitions_record_timestamps_once(tmp_path):
    root = _project(tmp_path)
    goal = submit(root, text="Ship the audit", kind="proposed_goal")["entry"]
    assert goal["accepted_at"] is None and goal["completed_at"] is None
    accepted = set_goal_state(root, entry_id=goal["id"], state="accepted")["entry"]
    assert accepted["accepted_at"] and accepted["completed_at"] is None
    again = set_goal_state(root, entry_id=goal["id"], state="accepted")["entry"]
    assert again["accepted_at"] == accepted["accepted_at"]
    completed = set_goal_state(root, entry_id=goal["id"], state="completed")["entry"]
    assert completed["completed_at"] and completed["accepted_at"] == accepted["accepted_at"]


def test_specialist_note_triage_links_existing_tasks_goals_and_competitor_suggestions(tmp_path):
    root = _project(tmp_path)
    initialized = initialize_coverage(root)
    task_id = "custom:review-competitor"
    update_item(
        root,
        {"id": task_id, "title": "Review named competitor"},
        initialized["revision"],
    )
    task_note = submit(root, text="Please compare the named competitor")["entry"]
    task = triage(
        root,
        entry_id=task_note["id"],
        outcome={
            "kind": "task",
            "reason": "The controller created a scoped review task before work.",
            "task_ids": [task_id],
        },
        actor="agent/controller",
    )["entry"]
    assert task["triage"][-1]["task_ids"] == [task_id]
    assert unread_summary(root, consumer="agent/controller")["count"] == 1

    goal = submit(root, text="Decide the next audit scope", kind="proposed_goal")["entry"]
    goal_note = submit(root, text="Turn this into a proposed audit goal")["entry"]
    linked_goal = triage(
        root,
        entry_id=goal_note["id"],
        outcome={
            "kind": "goal",
            "reason": "The specialist must still explicitly accept the proposed goal.",
            "goal_id": goal["id"],
        },
        actor="agent/controller",
    )["entry"]
    assert linked_goal["triage"][-1]["goal_id"] == goal["id"]
    assert linked_goal["triage"][-1]["kind"] == "goal"

    competitor_note = submit(root, text="Consider this competitor")["entry"]
    candidate = triage(
        root,
        entry_id=competitor_note["id"],
        outcome={
            "kind": "competitor",
            "reason": "Candidate retained for later explicit project preparation.",
            "competitors": ["https://competitor.test/"],
        },
        actor="agent/controller",
    )["entry"]
    assert candidate["triage"][-1]["competitors"] == ["https://competitor.test/"]
    assert coverage_status(root)["state"] == "initialized"


@pytest.mark.parametrize("kind", ["blocked", "rejected"])
def test_specialist_note_triage_requires_explicit_nonexecution_reason(tmp_path, kind):
    root = _project(tmp_path)
    note = submit(root, text="A question requiring a decision")["entry"]
    result = triage(
        root,
        entry_id=note["id"],
        outcome={"kind": kind, "reason": "Synthetic missing evidence."},
        actor="agent/controller",
    )
    assert result["entry"]["triage"][-1]["kind"] == kind
    assert result["entry"]["triage"][-1]["reason"] == "Synthetic missing evidence."


def test_specialist_note_triage_does_not_link_a_completed_goal(tmp_path):
    root = _project(tmp_path)
    goal = submit(root, text="Completed work", kind="proposed_goal")
    set_goal_state(root, entry_id=goal["entry"]["id"], state="accepted")
    set_goal_state(root, entry_id=goal["entry"]["id"], state="completed")
    note = submit(root, text="Reuse the completed goal")
    with pytest.raises(ValueError, match="current stored proposed goal"):
        triage(
            root,
            entry_id=note["entry"]["id"],
            outcome={
                "kind": "goal",
                "reason": "This must fail because the target is already complete.",
                "goal_id": goal["entry"]["id"],
            },
            actor="agent/controller",
        )


def test_concurrent_observer_submissions_are_not_lost_and_read_only_is_stable(tmp_path):
    root = _project(tmp_path)
    scan_hash = fingerprint(root)

    def write(index):
        return submit(root, text=f"Synthetic note {index}", references=["section:coverage"])[
            "entry"
        ]["id"]

    with ThreadPoolExecutor(max_workers=4) as executor:
        ids = list(executor.map(write, range(8)))

    assert (root / ".inbox.lock").is_file()
    listed = list_entries(root, consumer="agent/session-a", limit=20)
    assert {entry["id"] for entry in listed["entries"]} == set(ids)
    assert listed["pagination"]["total"] == 8
    assert fingerprint(root) != scan_hash
    again = fingerprint(root)
    unread_summary(root, consumer="agent/session-a")
    assert fingerprint(root) == again


def test_inbox_reads_and_observer_snapshot_leave_a_new_project_byte_identical(tmp_path):
    root = _project(tmp_path)
    before = _files(root)

    assert list_entries(root, consumer="agent/session-a")["entries"] == []
    assert unread_summary(root, consumer="agent/session-a")["count"] == 0
    assert observe(root, consumer="agent/session-a")["inbox_unread"]["count"] == 0

    assert _files(root) == before
    assert not (root / ".inbox.lock").exists()


def test_separate_process_handoff_survives_writer_exit(tmp_path):
    root = _project(tmp_path)
    writer = get_context("spawn").Process(target=_separate_writer, args=(str(root),))
    writer.start()
    writer.join(timeout=15)
    assert writer.exitcode == 0

    handed_off = list_entries(root, consumer="agent/replacement")
    assert handed_off["entries"][0]["text"] == "Written by a separate collector"
    assert unread_summary(root, consumer="agent/replacement")["count"] == 1


def test_mcp_progress_returns_notice_without_consuming_it(tmp_path):
    root = _project(tmp_path)
    entry = submit(root, text="Keep this visible", references=["finding:synthetic-1"])["entry"]
    server = build_server()
    progress = server._tool_manager.get_tool("seo_project_progress")
    result = progress.fn(directory=str(root), consumer="agent/session-a")
    assert result["inbox_unread"] == {
        "count": 1,
        "entries": [{"id": entry["id"], "kind": "note", "references": ["finding:synthetic-1"]}],
        "truncated": False,
    }
    assert unread_summary(root, consumer="agent/session-a")["count"] == 1


def test_project_bound_mcp_work_call_gets_only_its_consumer_notice(tmp_path):
    root = _project(tmp_path)
    submit(root, text="Scoped note")
    server = build_server()
    facts = server._tool_manager.get_tool("seo_project_facts")
    result = facts.fn(
        directory=str(root),
        facts=[{"name": "cms", "value": "fixture", "provenance": "operator", "observed_at": None}],
        consumer="agent/session-a",
    )
    assert result["inbox_unread"]["count"] == 1
    assert unread_summary(root, consumer="agent/other-session")["count"] == 1
    assert unread_summary(root, consumer="agent/session-a")["count"] == 1


def test_project_bound_crawl_gets_host_notice_without_auto_acknowledgment(tmp_path, monkeypatch):
    (tmp_path / "one").mkdir()
    (tmp_path / "two").mkdir()
    first = _project(tmp_path / "one")
    second = _project(tmp_path / "two")
    entry = submit(first, text="Check retained crawl evidence")["entry"]
    submit(second, text="Keep this separate")

    monkeypatch.setenv("SEOHEAD_MCP_CONSUMER_ID", "agent/crawl")
    monkeypatch.setenv("SEOHEAD_MCP_PROJECT_ALLOWLIST", str(first))
    monkeypatch.setattr("seohead.mcp.handlers.crawl_site", _synthetic_crawl)
    crawl = build_server()._tool_manager.get_tool("seo_crawl_site")

    bound = crawl.fn(project=str(first))
    outside = crawl.fn(project=str(second))

    assert bound["inbox_unread"] == {
        "count": 1,
        "entries": [{"id": entry["id"], "kind": "note", "references": []}],
        "truncated": False,
    }
    assert "inbox_unread" not in outside
    assert unread_summary(first, consumer="agent/crawl")["count"] == 1
    assert unread_summary(second, consumer="agent/crawl")["count"] == 1


def test_observer_snapshot_keeps_missing_work_and_logs_visible(tmp_path):
    root = _project(tmp_path)
    (root / "log.md").write_text("# Project log\n\nSynthetic handoff evidence\n", encoding="utf-8")
    submit(root, text="Review missing competitor", kind="proposed_goal")

    result = observe(root, consumer="agent/session-a")

    assert result["progress"]["audit_complete"] is False
    assert result["preparation"]["state"] == "pending"
    assert result["execution"]["runs"] == []
    assert result["monitor"]["policy"] is None and result["monitor"]["last_run"] is None
    assert result["scans"]["total"] == 0
    assert result["inbox_unread"]["count"] == 1
    assert "Synthetic handoff evidence" in result["log"]["text"]


def test_utf8_dictation_text_survives_handoff_and_restart(tmp_path):
    root = _project(tmp_path)
    text = "Проверь, почему сценарий и конкурент ещё не пройдены"
    entry = submit(root, text=text, kind="proposed_goal")["entry"]

    # A new reader process would reload inbox.json through this public API.
    restored = list_entries(root, consumer="agent/next-session")["entries"]
    assert restored[0]["id"] == entry["id"]
    assert restored[0]["text"] == text


def test_observer_reads_retained_frontier_findings_and_sitemap_without_mutation(tmp_path):
    root = _project(tmp_path)
    scan = root / "scans" / "synthetic.sqlite"
    _finished(scan)
    before = scan.read_bytes()

    result = observe(root)

    evidence = result["scans"]["items"][0]["evidence"]
    assert evidence["state"] == "available"
    assert evidence["frontier"]["state"] == "available"
    assert evidence["findings"]["total"] >= 0
    assert len(evidence["findings"]["items"]) <= 20
    assert "fetch_summaries" in evidence["sitemaps"]
    assert scan.read_bytes() == before


def test_observer_finding_browser_filters_sorts_pages_and_keeps_scan_read_only(tmp_path):
    root = _project(tmp_path)
    scan = root / "scans" / "synthetic.sqlite"
    _finished(scan)
    before = scan.read_bytes()

    unfiltered = findings_page(root, limit=2, sort="target_url", descending=True)
    assert unfiltered["counts"]["source"] >= 1
    selected_url = unfiltered["items"][0]["target_url"]
    by_url = findings_page(root, url=selected_url, limit=100)
    assert by_url["counts"]["matched"] >= 1
    assert all(item["target_url"] == selected_url for item in by_url["items"])
    needle = unfiltered["items"][0]["check"]
    page = findings_page(root, query=needle, limit=2, sort="target_url", descending=True)
    assert page["counts"]["matched"] >= 1
    assert page["counts"]["returned"] <= 2
    detail = finding_detail(root, ordinal=page["items"][0]["ordinal"])
    assert detail["finding"]["check"] == needle
    assert detail["evidence"]["check"] == needle
    assert scan.read_bytes() == before


def test_host_consumer_binding_is_project_scoped_and_two_servers_do_not_share_it(
    tmp_path, monkeypatch
):
    (tmp_path / "one").mkdir()
    (tmp_path / "two").mkdir()
    first = _project(tmp_path / "one")
    second = _project(tmp_path / "two")
    submit(first, text="First project handoff")
    submit(second, text="Second project handoff")

    monkeypatch.setenv("SEOHEAD_MCP_CONSUMER_ID", "agent/first")
    monkeypatch.setenv("SEOHEAD_MCP_PROJECT_ALLOWLIST", str(first))
    first_server = build_server()
    observe_tool = first_server._tool_manager.get_tool("seo_project_observe")
    first_result = observe_tool.fn(directory=str(first))
    outside_result = observe_tool.fn(directory=str(second))
    assert first_result["inbox_unread"]["count"] == 1
    assert "inbox_unread" not in outside_result
    assert unread_summary(first, consumer="agent/first")["count"] == 1

    monkeypatch.setenv("SEOHEAD_MCP_CONSUMER_ID", "agent/second")
    monkeypatch.setenv("SEOHEAD_MCP_PROJECT_ALLOWLIST", str(second))
    second_server = build_server()
    second_result = second_server._tool_manager.get_tool("seo_project_observe").fn(
        directory=str(second)
    )
    assert second_result["inbox_unread"]["count"] == 1
    assert unread_summary(second, consumer="agent/second")["count"] == 1
    assert unread_summary(first, consumer="agent/first")["count"] == 1
    assert os.environ["SEOHEAD_MCP_CONSUMER_ID"] == "agent/second"


def test_restart_isolation_keeps_each_host_bound_to_its_own_project(tmp_path):
    (tmp_path / "one").mkdir()
    (tmp_path / "two").mkdir()
    first = _project(tmp_path / "one")
    second = _project(tmp_path / "two")
    submit(first, text="First process note")
    submit(second, text="Second process note")
    context = get_context("spawn")
    queue = context.Queue()
    first_process = context.Process(
        target=_bound_mcp_observer,
        args=(str(first), "agent/first-process", str(first), queue),
    )
    second_process = context.Process(
        target=_bound_mcp_observer,
        args=(str(second), "agent/second-process", str(second), queue),
    )
    first_process.start()
    second_process.start()
    first_process.join(timeout=20)
    second_process.join(timeout=20)
    assert first_process.exitcode == second_process.exitcode == 0
    notices = [queue.get(timeout=5), queue.get(timeout=5)]
    assert all(notice["count"] == 1 for notice in notices)
    assert unread_summary(first, consumer="agent/first-process")["count"] == 1
    assert unread_summary(second, consumer="agent/second-process")["count"] == 1


def test_crawl_host_binding_survives_restart_without_consuming_inbox(tmp_path):
    root = _project(tmp_path)
    submit(root, text="Read on the next project call")
    context = get_context("spawn")
    queue = context.Queue()

    for _ in range(2):
        process = context.Process(
            target=_bound_mcp_crawl,
            args=(str(root), "agent/restarted-crawl", str(root), queue),
        )
        process.start()
        process.join(timeout=20)
        assert process.exitcode == 0
        assert queue.get(timeout=5)["count"] == 1

    assert unread_summary(root, consumer="agent/restarted-crawl")["count"] == 1


@pytest.mark.parametrize(
    "item_id",
    [
        "custom:late",
        "check:TITLE_MISSING",
        "scenario:full-audit",
        "skill:workflow/control",
        "skill:general/seo-content",
        "site:12345678-1234-4234-8234-123456789abc/custom:late",
        "custom:" + "a" * 128,
        "synthetic",
    ],
)
def test_contextual_task_note_keeps_actual_identifier_through_durable_receipts(tmp_path, item_id):
    root = _project(tmp_path)
    reference = "task:" + item_id
    submitted = submit(root, text="Review this exact task", references=[reference])["entry"]
    entries = list_entries(root, consumer="agent/test")
    assert entries["entries"][0]["references"] == [reference]
    mark_read(root, consumer="agent/test", entry_ids=[submitted["id"]])
    acknowledge(root, consumer="agent/test", entry_ids=[submitted["id"]])
    assert list_entries(root, consumer="agent/test")["entries"][0]["references"] == [reference]


@pytest.mark.parametrize(
    "reference",
    [
        "task:custom:../secret",
        "task:custom:a:b",
        "task:https://example.test/",
        "task:unknown:value",
        "task:site:invalid/custom:late",
        "task:site:12345678-1234-4234-8234-123456789abc/../../secret",
        "task:custom:" + "a" * 129,
        "scan:custom:late",
        "finding:https://example.test/",
    ],
)
def test_task_note_namespace_does_not_admit_arbitrary_paths_or_other_namespace_uris(
    tmp_path, reference
):
    root = _project(tmp_path)
    submit(root, text="Existing note")
    before = _files(root)
    with pytest.raises(ValueError, match="invalid project reference"):
        submit(root, text="Rejected reference", references=[reference])
    assert _files(root) == before


def test_question_entry_is_stored_without_goal_state(tmp_path):
    # Issue #979: Desktop needs a "question" entry type; it carries no goal lifecycle.
    root = _project(tmp_path)
    question = submit(root, text="Why is the sitemap gap open?", kind="question")["entry"]
    assert question["kind"] == "question" and question["goal_state"] is None
    with pytest.raises(ValueError, match="proposed goal"):
        set_goal_state(root, entry_id=question["id"], state="accepted")
    with pytest.raises(ValueError, match="kind must be"):
        submit(root, text="Unknown kind", kind="ticket")


def test_accepted_goal_creates_one_checklist_task_and_keeps_it_on_retry(tmp_path):
    from seohead.projects.inbox import goal_task_id

    root = _project(tmp_path)
    goal = submit(root, text="  Compare   the named\ncompetitor ", kind="proposed_goal")["entry"]
    set_goal_state(root, entry_id=goal["id"], state="accepted")
    set_goal_state(root, entry_id=goal["id"], state="accepted")
    task_id = goal_task_id(goal["id"])
    rows = [row for row in coverage_status(root)["items"] if row["id"] == task_id]
    assert len(rows) == 1
    assert rows[0]["title"] == "Owner goal: Compare the named competitor"
    assert rows[0]["kind"] == "custom" and rows[0]["complete"] is False
    set_goal_state(root, entry_id=goal["id"], state="completed")
    assert any(row["id"] == task_id for row in coverage_status(root)["items"])


def test_goal_acceptance_is_refused_without_a_task_when_the_checklist_is_busy(tmp_path):
    root = _project(tmp_path)
    goal = submit(root, text="Blocked by a writer", kind="proposed_goal")["entry"]
    (root / ".coverage.lock").write_text("")
    with pytest.raises(ValueError, match="coverage writer is busy"):
        set_goal_state(root, entry_id=goal["id"], state="accepted")
    (root / ".coverage.lock").unlink()
    assert list_entries(root, consumer="agent/session-a")["entries"][0]["goal_state"] == "proposed"


def test_unread_owner_tasks_are_reported_until_the_agent_acknowledges_the_goal(tmp_path):
    from seohead.mcp.project_handlers import project_basic_status
    from seohead.projects.inbox import goal_task_id

    root = _project(tmp_path)
    goal = submit(root, text="Audit the competitor set", kind="proposed_goal")["entry"]
    proposed = project_basic_status(str(root), consumer="agent/session-a")
    assert proposed["owner_tasks_unread"]["count"] == 0

    set_goal_state(root, entry_id=goal["id"], state="accepted")
    status = project_basic_status(str(root), consumer="agent/session-a")
    assert status["owner_tasks_unread"]["count"] == 1
    assert status["owner_tasks_unread"]["items"][0]["task_id"] == goal_task_id(goal["id"])
    assert (
        status["owner_tasks_unread"]["items"][0]["title"] == "Owner goal: Audit the competitor set"
    )

    mark_read(root, consumer="agent/session-a", entry_ids=[goal["id"]])
    assert (
        project_basic_status(str(root), consumer="agent/session-a")["owner_tasks_unread"]["count"]
        == 1
    )
    acknowledge(root, consumer="agent/session-a", entry_ids=[goal["id"]])
    assert (
        project_basic_status(str(root), consumer="agent/session-a")["owner_tasks_unread"]["count"]
        == 0
    )
    assert "owner_tasks_unread" not in project_basic_status(str(root))


def test_accepted_goal_shows_as_unread_owner_task_until_acknowledged(tmp_path):
    from seohead.projects.inbox import acknowledge, goal_task_id, unread_goal_tasks

    root = _project(tmp_path)
    goal = submit(root, text="Check the pricing pages", kind="proposed_goal")["entry"]
    set_goal_state(root, entry_id=goal["id"], state="accepted")
    unread = unread_goal_tasks(root, consumer="agent/session-a")
    assert unread["count"] == 1
    assert unread["items"][0]["task_id"] == goal_task_id(goal["id"])
    status = server_status(root)
    assert status["owner_tasks_unread"]["count"] == 1
    acknowledge(root, consumer="agent/session-a", entry_ids=[goal["id"]])
    assert unread_goal_tasks(root, consumer="agent/session-a")["count"] == 0
    assert unread_goal_tasks(root, consumer="agent/session-b")["count"] == 1


def server_status(root):
    server = build_server()
    progress = server._tool_manager.get_tool("seo_project_progress")
    return progress.fn(directory=str(root), consumer="agent/session-a")


def test_accepted_goal_adds_only_its_own_task_to_a_fresh_checklist(tmp_path):
    import json

    from seohead.projects.catalogue import load_catalogue
    from seohead.projects.inbox import goal_task_id

    root = _project(tmp_path)
    goal = submit(root, text="Compare two competitors", kind="proposed_goal")["entry"]
    set_goal_state(root, entry_id=goal["id"], state="accepted")
    stored = json.loads((root / "coverage.json").read_text())["items"]
    assert set(stored) == {goal_task_id(goal["id"])}
    # Touching a built-in row materializes the catalogue on demand, not at goal acceptance.
    builtin = next(iter(load_catalogue()))
    update_item(root, {"id": builtin, "priority": "P2"}, coverage_status(root)["revision"])
    assert (
        next(row for row in coverage_status(root)["items"] if row["id"] == builtin)["priority"]
        == "P2"
    )


def test_busy_checklist_names_the_live_collector_instead_of_a_leftover_lock(tmp_path):
    from seohead.projects import run_observation

    root = _project(tmp_path)
    goal = submit(root, text="Blocked by a running crawl", kind="proposed_goal")["entry"]
    run = run_observation.start(
        root,
        kind="native",
        mode="spider",
        max_urls=10,
        config_fingerprint="synthetic",
        artifact=root / "scans" / "scan.sqlite",
    )
    (root / ".coverage.lock").write_text("")
    with pytest.raises(ValueError, match=rf"coverage writer is busy: native run {run['id']}"):
        set_goal_state(root, entry_id=goal["id"], state="accepted")
    (root / ".coverage.lock").unlink()


def test_unexplained_writer_lock_is_reported_as_a_leftover(tmp_path):
    root = _project(tmp_path)
    goal = submit(root, text="Blocked by a stale file", kind="proposed_goal")["entry"]
    (root / ".coverage.lock").write_text("")
    with pytest.raises(ValueError, match=r"no live holder is recorded.*leftover"):
        set_goal_state(root, entry_id=goal["id"], state="accepted")
    (root / ".coverage.lock").unlink()
