"""Structured project event log: append validation, source/text filters and pagination."""

from __future__ import annotations

import json

import pytest

from seohead.projects import event_log
from seohead.projects.workspace import create_project


@pytest.fixture
def project(tmp_path):
    root = tmp_path / "site"
    create_project(root, "https://example.test/")
    return root


def _add(project, source="agent", actor="agent", text="step"):
    return event_log.append(project, source=source, actor=actor, text=text)["event"]


def test_append_then_read_newest_first_with_source_actor_and_sequence(project):
    first = _add(project, source="user", actor="user", text="Pause the crawl")
    second = _add(project, source="scans", actor="schedule", text="Scan 3 finished")

    result = event_log.page(project)

    assert result["total"] == 2
    assert [row["sequence"] for row in result["items"]] == [2, 1]
    assert result["items"][0] == second
    assert result["items"][1] == first
    assert result["items"][0]["source"] == "scans"
    assert result["items"][0]["actor"] == "schedule"


def test_append_never_touches_log_md_or_project_json(project):
    before = {name: (project / name).read_bytes() for name in ("log.md", "project.json")}

    _add(project, text="Note only")

    assert {name: (project / name).read_bytes() for name in before} == before
    assert (project / event_log.FILE).is_file()


def test_source_and_case_insensitive_text_filters(project):
    _add(project, source="agent", text="Found 52 new URLs")
    _add(project, source="user", text="Check the robots rules")
    _add(project, source="agent", text="found a redirect loop")

    agent_only = event_log.page(project, source="agent")
    assert agent_only["total"] == 2
    assert {row["source"] for row in agent_only["items"]} == {"agent"}

    matched = event_log.page(project, query="FOUND")
    assert matched["total"] == 2

    combined = event_log.page(project, source="user", query="robots")
    assert combined["total"] == 1
    assert combined["items"][0]["text"] == "Check the robots rules"


def test_revision_cursor_returns_only_newer_events_and_marks_unchanged(project):
    first = _add(project, text="first")
    _add(project, text="second")

    changed = event_log.page(project, since_revision=first["sequence"])
    unchanged = event_log.page(project, since_revision=2)

    assert changed["revision"] == 2
    assert changed["not_modified"] is False
    assert [row["text"] for row in changed["items"]] == ["second"]
    assert unchanged["revision"] == 2
    assert unchanged["not_modified"] is True
    assert unchanged["items"] == []


def test_follow_waits_for_a_new_event(project, monkeypatch):
    _add(project, text="first")
    original_sleep = event_log.time.sleep
    appended = False

    def sleep(_seconds):
        nonlocal appended
        if not appended:
            appended = True
            _add(project, text="followed")
        original_sleep(0)

    monkeypatch.setattr(event_log.time, "sleep", sleep)
    result = event_log.page(project, since_revision=1, follow=True, wait_seconds=1)

    assert result["revision"] == 2
    assert result["not_modified"] is False
    assert [row["text"] for row in result["items"]] == ["followed"]


def test_pagination_walks_all_events_without_gaps(project):
    for index in range(5):
        _add(project, text=f"event {index}")

    seen = []
    offset = 0
    while offset is not None:
        result = event_log.page(project, offset=offset, limit=2)
        seen.extend(row["text"] for row in result["items"])
        offset = result["pagination"]["next_offset"]

    assert seen == [f"event {index}" for index in reversed(range(5))]
    last = event_log.page(project, offset=4, limit=2)["pagination"]
    assert last["next_offset"] is None
    assert last["previous_offset"] == 2


def test_empty_log_reads_as_empty_page(project):
    result = event_log.page(project)
    assert result["total"] == 0
    assert result["items"] == []


@pytest.mark.parametrize(
    "kwargs",
    [
        {"source": "bot", "actor": "agent", "text": "x"},
        {"source": "agent", "actor": "robot", "text": "x"},
        {"source": "agent", "actor": "agent", "text": "   "},
        {"source": "agent", "actor": "agent", "text": "a\x00b"},
        {"source": "agent", "actor": "agent", "text": "x" * (event_log.MAX_TEXT + 1)},
    ],
)
def test_invalid_appends_are_refused_and_write_nothing(project, kwargs):
    with pytest.raises(ValueError):
        event_log.append(project, **kwargs)
    assert not (project / event_log.FILE).exists()


@pytest.mark.parametrize(
    "kwargs",
    [
        {"offset": -1},
        {"limit": 0},
        {"limit": event_log.MAX_LIMIT + 1},
        {"source": "bot"},
        {"query": "x" * (event_log.MAX_QUERY + 1)},
        {"since_revision": -1},
        {"follow": True},
        {"wait_seconds": event_log.MAX_FOLLOW_SECONDS + 1},
    ],
)
def test_invalid_reads_are_refused(project, kwargs):
    with pytest.raises(ValueError):
        event_log.page(project, **kwargs)


def test_malformed_or_unsafe_event_file_refuses_instead_of_hiding_rows(project):
    _add(project, text="good")
    path = project / event_log.FILE
    with path.open("a", encoding="utf-8") as stream:
        stream.write("{not json}\n")

    with pytest.raises(ValueError, match="line 2"):
        event_log.page(project)

    path.unlink()
    row = {
        "sequence": 1,
        "occurred_at": "2026-10-10T00:00:00Z",
        "source": "app",
        "actor": "agent",
        "text": "x",
        "extra": 1,
    }
    path.write_text(json.dumps(row) + "\n", encoding="utf-8")
    with pytest.raises(ValueError):
        event_log.page(project)


def test_symlinked_event_file_refuses(project, tmp_path):
    target = tmp_path / "elsewhere.jsonl"
    target.write_text("", encoding="utf-8")
    (project / event_log.FILE).symlink_to(target)

    with pytest.raises(ValueError, match="unsafe"):
        event_log.append(project, source="agent", actor="agent", text="x")
    with pytest.raises(ValueError, match="unsafe"):
        event_log.page(project)


def test_handlers_expose_append_and_page_through_the_shared_registry(project):
    from seohead.mcp.handlers import HANDLERS

    appended = HANDLERS["project_event_append"](
        directory=str(project), source="user", actor="user", text="Note from the operator"
    )
    assert appended["ok"] is True
    assert appended["event"]["sequence"] == 1

    page = HANDLERS["project_event_page"](directory=str(project), source="user", limit=5)
    assert [item["text"] for item in page["items"]] == ["Note from the operator"]


def test_cli_parses_event_append_and_page_into_handler_kwargs(project):
    from seohead.cli import _build_kwargs, build_parser

    parser = build_parser()
    args = parser.parse_args(
        [
            "project-event-append",
            "--directory",
            str(project),
            "--source",
            "agent",
            "--actor",
            "agent",
            "--text",
            "Scan queued",
        ]
    )
    handler, kwargs = _build_kwargs("project-event-append", args)
    assert handler == "project_event_append"
    assert kwargs == {
        "directory": str(project),
        "source": "agent",
        "actor": "agent",
        "text": "Scan queued",
    }

    args = parser.parse_args(
        ["project-event-page", "--directory", str(project), "--query", "scan", "--limit", "3"]
    )
    handler, kwargs = _build_kwargs("project-event-page", args)
    assert handler == "project_event_page"
    assert kwargs["query"] == "scan"
    assert kwargs["limit"] == 3
