"""Human terminal workflows, including data boundaries hidden by crop-only tests."""

from __future__ import annotations

import time
from io import StringIO
from threading import Event

from rich.console import Console

from seohead.projects.coverage import coverage_status, initialize_coverage, update_item
from seohead.projects.observer import observe
from seohead.projects.workspace import create_project
from seohead.tui.app import (
    _ObserverRefresh,
    _page_selection,
    _rate_text,
    _read_view,
    _view_key,
    _watch_detail_lines,
    _watch_lines,
    build_frame,
)
from seohead.tui.state import ShellState
from seohead.tui.theme import resolve_palette
from tests.test_project_progress import _plan


def _project(tmp_path):
    root = tmp_path / "project"
    create_project(root, "https://example.test/")
    initialize_coverage(root, plan=_plan(["scenario:structured-data"]))
    update_item(
        root,
        {"id": "custom:late", "title": "Inspect the retained product sample", "order": 999},
        coverage_status(root)["revision"],
    )
    initialize_coverage(root, plan=_plan(["scenario:structured-data", "custom:late"]))
    return root


def _render(root, state, snapshot, *, width=80, height=24, data=None, message=None):
    console = Console(width=width, height=height, file=StringIO(), record=True, no_color=True)
    console.print(
        build_frame(
            state,
            width=width,
            height=height,
            palette=resolve_palette(color=False),
            project=str(root),
            snapshot_override=(snapshot, []),
            view_override=data,
            message=message,
        )
    )
    return console.export_text()


def test_editor_suspend_resume_cursor_and_explicit_discard_preserve_other_draft():
    state = ShellState([], view="watch")
    state.handle_key("char:n")
    for char in "Hello":
        state.handle_key("char:" + char)
    state.handle_key("home")
    state.handle_key("delete")
    assert state.note_text == "ello"
    state.handle_key("escape")
    state.handle_key("char:g")
    state.handle_key("char:G")
    state.handle_key("escape")
    state.handle_key("char:n")
    assert state.note_text == "ello"
    state.handle_key("ctrl_x")
    assert state.note_text == ""
    state.handle_key("escape")
    state.handle_key("char:g")
    assert state.note_text == "G"


def test_large_paste_keeps_every_character_and_refuses_oversized_submit():
    state = ShellState([], view="note")
    text = "Проверка\n" * 1200
    state.handle_key("paste_start")
    for char in text:
        state.handle_key("enter" if char == "\n" else "char:" + char)
    state.handle_key("paste_end")
    state.handle_key("enter")
    assert state.note_text == text
    assert not state.note_ready and state.view == "note"


def test_filter_cancel_and_last_page_navigation_are_not_mutations():
    state = ShellState([], view="watch", watch_section="tasks", watch_query="old")
    state.handle_key("char:f")
    state.handle_key("char:X")
    state.handle_key("escape")
    assert state.watch_query == "old"
    state.watch_count = 2
    state.watch_next_offset = None
    state.handle_key("page_down")
    state.handle_key("end")
    state.handle_key("down")
    assert state.watch_offset == 0 and state.watch_index == 1


def test_task_details_show_human_title_reason_and_definition(tmp_path):
    root = _project(tmp_path)
    state = ShellState([], view="watch", watch_section="tasks")
    state.watch_item_id = "custom:late"
    state.handle_key("enter")
    assert state.view == "watch_detail"
    snapshot = observe(root)
    data = _read_view(str(root), state, snapshot)
    lines = _watch_detail_lines(
        str(root), state, resolve_palette(color=False), snapshot=snapshot, data=data
    )
    text = "\n".join(line.plain for line in lines)
    assert "Inspect the retained product sample" in text
    assert "Scope" in text and "Definition" in text and "Latest attempt" in text


def test_refresh_preserves_selected_identity_without_overriding_arrow_navigation():
    state = ShellState([], view="watch", watch_section="tasks")
    items = [{"id": "a"}, {"id": "b"}]
    _page_selection(state, items, {"total": 2})
    state.handle_key("down")
    _page_selection(state, items, {"total": 2})
    assert state.watch_selected_row_id == "b"
    _page_selection(state, [{"id": "new"}, *items], {"total": 3})
    assert state.watch_index == 2 and state.watch_selected_row_id == "b"


def test_structured_data_view_uses_existing_checklist_and_agreed_meter(tmp_path):
    root = _project(tmp_path)
    state = ShellState([], view="watch")
    state.handle_key("char:a")
    snapshot = observe(root)
    page = _read_view(str(root), state, snapshot)
    assert any(item["id"] == "scenario:structured-data" for item in page["items"])
    assert all(
        "schema" in str(item).lower() or "structured" in str(item).lower() for item in page["items"]
    )
    text = _render(root, ShellState([], view="watch"), snapshot, width=140, height=40, data={})
    assert "0 / 1" in text
    assert "None in agreed scope" in text


def test_cached_render_does_not_read_storage_and_receipt_is_visible(tmp_path, monkeypatch):
    from seohead.projects.run_observation import start
    from tests.test_scan_history import _finished

    root = _project(tmp_path)
    _finished(root / "scans" / "retained.sqlite")
    first = observe(root)
    start(
        root,
        kind="native",
        mode="spider",
        max_urls=100,
        config_fingerprint=first["scans"]["items"][0]["config_fingerprint"],
        artifact=root / "scans" / "retained.sqlite",
    )
    snapshot = observe(root)

    def forbidden(*args, **kwargs):
        raise AssertionError("render performed storage I/O")

    monkeypatch.setattr("seohead.tui.app._read_view", forbidden)
    monkeypatch.setattr("seohead.tui.app._watch_snapshot", forbidden)
    monkeypatch.setattr("pathlib.Path.resolve", forbidden)
    for width, height in ((80, 24), (120, 32), (237, 68)):
        text = _render(
            root,
            ShellState([], view="watch"),
            snapshot,
            width=width,
            height=height,
            data={},
            message="Note saved locally: receipt 123",
        )
        assert "Note saved locally: receipt 123" in text
        assert "Inspect the retained product sample" in text


def test_slow_page_reader_never_blocks_poll_or_editor(tmp_path, monkeypatch):
    root = _project(tmp_path)
    snapshot = observe(root)
    started, release = Event(), Event()
    monkeypatch.setattr("seohead.tui.app._watch_snapshot", lambda _: (snapshot, []))

    def slow(*args):
        started.set()
        release.wait(2)
        return {"items": []}

    monkeypatch.setattr("seohead.tui.app._read_view", slow)
    state = ShellState([], view="watch", watch_section="tasks")
    reader = _ObserverRefresh(str(root))
    try:
        reader.poll(state)
        assert started.wait(1)
        start = time.monotonic()
        reader.poll(state)
        state.handle_key("char:n")
        state.handle_key("char:П")
        assert time.monotonic() - start < 0.1
        assert state.note_text == "П"
    finally:
        release.set()
        reader.worker.join(2)


def test_explicit_note_write_is_off_input_loop_and_receipt_is_durable(tmp_path, monkeypatch):
    from seohead.projects.inbox import list_entries, submit

    root = _project(tmp_path)
    started, release = Event(), Event()

    def slow_submit(*args, **kwargs):
        started.set()
        release.wait(2)
        return submit(*args, **kwargs)

    monkeypatch.setattr("seohead.projects.inbox.submit", slow_submit)
    state = ShellState([], view="watch", note_text="Keep the product review evidence")
    reader = _ObserverRefresh(str(root))
    try:
        start = time.monotonic()
        reader.save(state)
        reader.poll(state)
        assert time.monotonic() - start < 0.1 and started.wait(1)
        assert reader.saving
    finally:
        release.set()
        reader.worker.join(2)
    reader.poll(state)
    assert reader.saved and not reader.saving
    assert "saved" in reader.saved[1]
    assert list_entries(root, consumer="test")["entries"][0]["text"] == state.note_text


def test_note_from_task_context_saves_the_exact_selected_task_reference(tmp_path):
    from seohead.projects.inbox import list_entries
    from seohead.tui.app import _save_note

    root = _project(tmp_path)
    state = ShellState(
        [],
        view="watch",
        watch_section="tasks",
        watch_item_id="custom:late",
        note_text="Please verify this task's retained evidence",
    )
    message = _save_note(str(root), state)
    assert "saved" in message and not state.note_error
    assert "task:custom:late" in list_entries(root, consumer="test")["entries"][0]["references"]


def test_site_inventory_opens_competitor_scan_without_mixing_owner_artifacts(tmp_path):
    from tests.test_project_observer_sites import _prepare_with_competitors
    from tests.test_scan_history import _finished

    root = tmp_path / "owner"
    prepared = _prepare_with_competitors(root)
    child = root / prepared["preparation"]["competitors"][0]["directory"]
    _finished(root / "scans" / "owner.sqlite")
    _finished(child / "scans" / "competitor.sqlite")
    snapshot = observe(root)
    state = ShellState([], view="watch", watch_section="sites", watch_index=1)
    palette = resolve_palette(color=False)
    _watch_lines(str(root), state, palette, None, snapshot=snapshot, data={})
    state.handle_key("enter")
    details = _watch_detail_lines(str(root), state, palette, snapshot=snapshot, data={})
    assert "competitor-one.example.test" in "\n".join(line.plain for line in details)
    state.handle_key("char:s")
    scans = _read_view(str(root), state, snapshot)
    assert state.watch_section == "scans"
    # Compact scan rows (#1397) carry uuid, not path; the competitor binding
    # itself is proven by the findings view below.
    assert len(scans["items"]) == 1 and scans["items"][0]["uuid"]
    _watch_lines(str(root), state, palette, None, snapshot=snapshot, data=scans)
    state.handle_key("char:5")
    findings = _read_view(str(root), state, snapshot)
    assert findings["scan"]["path"] == str(child / "scans" / "competitor.sqlite")


def test_inbox_competitor_proposal_is_visible_without_creating_a_workspace(tmp_path):
    from seohead.projects.inbox import submit, triage

    root = _project(tmp_path)
    entry = submit(root, text="Consider the suggested competitor")["entry"]
    triage(
        root,
        entry_id=entry["id"],
        actor="agent/test",
        outcome={
            "kind": "competitor",
            "reason": "Candidate retained for specialist review",
            "competitors": ["https://suggested.example.test/"],
        },
    )
    before = sorted(str(path.relative_to(root)) for path in root.rglob("*"))
    snapshot = observe(root)
    state = ShellState([], view="watch", watch_section="sites", watch_index=1)
    palette = resolve_palette(color=False)
    lines = _watch_lines(str(root), state, palette, None, snapshot=snapshot, data={})
    assert "Suggested competitor" in "\n".join(line.plain for line in lines)
    state.handle_key("enter")
    details = "\n".join(
        line.plain
        for line in _watch_detail_lines(str(root), state, palette, snapshot=snapshot, data={})
    )
    assert entry["id"] in details and "not prepared or analyzed" in details
    state.handle_key("char:s")
    assert state.view == "watch_detail" and state.watch_site_uuid is None
    assert before == sorted(str(path.relative_to(root)) for path in root.rglob("*"))


def test_compact_home_keeps_collection_and_stale_state_visible(tmp_path):
    from seohead.projects.run_observation import progress, start

    root = _project(tmp_path)
    run = start(
        root,
        kind="native",
        mode="spider",
        max_urls=100,
        config_fingerprint="synthetic",
        artifact=None,
    )
    progress(
        root,
        run["id"],
        fetched=20,
        queued=79,
        inflight=1,
        excluded=2,
        rate_per_second=4.0,
        rate_window_seconds=1.0,
    )
    snapshot = observe(root)
    text = _render(root, ShellState([], view="watch"), snapshot, width=76, height=22, data={})
    assert "20 / 100 discovered" in text and "4.00 pages/s" in text
    assert "Queue 79" in text and "Sitemap:" in text and "Agreed tasks:" in text
    snapshot["runs"]["items"][0]["telemetry"].update(state="stale", current_rate_per_second=None)
    stale = _render(root, ShellState([], view="watch"), snapshot, width=76, height=22, data={})
    assert "Stale" in stale and "observed active" not in stale and "4.00 pages/s" not in stale


def test_note_can_start_from_evidence_detail_and_return_without_losing_context():
    state = ShellState(
        [],
        view="watch_detail",
        watch_section="findings",
        watch_detail_ordinal=7,
        watch_selected_scan_uuid="synthetic",
    )
    state.handle_key("char:n")
    state.handle_key("char:X")
    state.handle_key("escape")
    assert state.view == "watch_detail" and state.note_text == "X"
    state.handle_key("char:n")
    state.handle_key("enter")
    assert state.view == "watch_detail" and state.note_ready
    assert state.watch_detail_ordinal == 7 and state.watch_selected_scan_uuid == "synthetic"


def test_visible_scan_page_stays_ready_when_render_selects_a_scan(tmp_path):
    from tests.test_scan_history import _finished

    root = _project(tmp_path)
    _finished(root / "scans" / "retained.sqlite")
    snapshot = observe(root)
    state = ShellState([], view="watch", watch_section="scans")
    reader = _ObserverRefresh(str(root))
    reader.key = _view_key(state)
    reader.page = _read_view(str(root), state, snapshot)
    _watch_lines(
        str(root), state, resolve_palette(color=False), None, snapshot=snapshot, data=reader.page
    )
    assert state.watch_selected_scan_uuid
    assert reader.page_for(state) is reader.page
    state.handle_key("enter")
    assert state.view == "watch_detail"


def test_default_finding_scan_is_bound_before_publishing_its_ready_page(tmp_path):
    from tests.test_scan_history import _finished

    root = _project(tmp_path)
    _finished(root / "scans" / "retained.sqlite")
    snapshot = observe(root)
    state = ShellState([], view="watch", watch_section="findings")
    page = _read_view(str(root), state, snapshot)
    reader = _ObserverRefresh(str(root))
    reader.queue.put(("read", _view_key(state), (snapshot, []), page))
    reader.poll(state)
    assert state.watch_selected_scan_uuid == page["scan"]["uuid"]
    assert reader.page_for(state) is page
    assert reader.worker is None


def test_wide_home_keeps_active_work_with_site_scoped_identifiers(tmp_path):
    root = _project(tmp_path)
    snapshot = observe(root)
    active = snapshot["active_tasks"]["items"][0]
    active["id"] = "site:00000000-0000-4000-8000-000000000001/custom:late"
    text = _render(root, ShellState([], view="watch"), snapshot, width=140, height=40, data={})
    assert "CURRENT WORK" in text
    assert "Inspect the retained product sample" in text
    assert "NEXT ACTIONS" not in text


def test_sf_rate_names_its_measured_resource_population_without_a_machine_code():
    assert (
        _rate_text({"current_rate_per_second": 20.0, "unit": "urls_including_resources"})
        == "20.00 URLs incl. resources/s"
    )
    assert (
        _rate_text({"current_rate_per_second": None, "unit": "urls_including_resources"})
        == "rate unavailable"
    )


def test_hyphenated_command_help_uses_shared_handler_description():
    from seohead.tui.app import _command_reference

    lines = _command_reference("compare-crawls")
    assert "Diff two audits" in lines[1]
    assert "Command group" not in lines[1]
    assert "--out-dir" in "\n".join(lines)


def test_command_group_help_does_not_render_none_type_documentation():
    from seohead.tui.app import _command_reference

    lines = _command_reference("sf")
    assert lines[1] == "Command group; choose one of its listed subcommands."
    assert "None singleton" not in "\n".join(lines)
