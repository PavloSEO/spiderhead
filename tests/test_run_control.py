"""Run-ID control accepts only active, live native scans in the selected project."""

import pytest

from seohead.projects import run_control
from seohead.projects.workspace import create_project


class _Scan:
    def execute(self, _query):
        return self

    def fetchone(self):
        return ("running",)

    def close(self):
        return None


def _active_run(run_id="a" * 32, *, state="running", pid_state="live"):
    return {
        "id": run_id,
        "state": state,
        "source_kind": "native",
        "pid_state": pid_state,
        "artifact": "scans/scan.sqlite",
    }


def test_pause_request_is_consumed_once_and_checkpoints_by_run_id(tmp_path, monkeypatch):
    project = tmp_path / "shop"
    create_project(project, "https://example.test/")
    scan_path = project / "scans" / "scan.sqlite"
    scan_path.write_bytes(b"scan")
    run = _active_run()
    monkeypatch.setattr("seohead.storage.open_scan", lambda *_args, **_kwargs: _Scan())
    monkeypatch.setattr(
        "seohead.projects.run_observation.status",
        lambda *_args, **_kwargs: {"items": [run]},
    )

    requested = run_control.request(project, run["id"], "pause")
    assert requested == {"ok": True, "run_id": run["id"], "state": "pause_requested"}
    assert run_control.consume(project, run["id"]) == "pause"
    assert run_control.consume(project, run["id"]) is None
    with run_control._connect(project) as con:
        assert con.execute(
            "SELECT state FROM requests WHERE run_id=?", (run["id"],)
        ).fetchone() == ("applied",)


@pytest.mark.parametrize(
    "run",
    [
        None,
        _active_run(state="partial"),
        _active_run(pid_state="abandoned"),
        {**_active_run(), "source_kind": "sitemap"},
    ],
)
def test_control_rejects_stale_or_non_live_runs(tmp_path, monkeypatch, run):
    project = tmp_path / "shop"
    create_project(project, "https://example.test/")
    monkeypatch.setattr(
        "seohead.projects.run_observation.status",
        lambda *_args, **_kwargs: {"items": [] if run is None else [run]},
    )
    with pytest.raises(ValueError, match="active native crawl"):
        run_control.request(project, "a" * 32, "stop")


def test_control_rejects_an_existing_pending_request(tmp_path, monkeypatch):
    project = tmp_path / "shop"
    create_project(project, "https://example.test/")
    (project / "scans" / "scan.sqlite").write_bytes(b"scan")
    run = _active_run()
    monkeypatch.setattr("seohead.storage.open_scan", lambda *_args, **_kwargs: _Scan())
    monkeypatch.setattr(
        "seohead.projects.run_observation.status",
        lambda *_args, **_kwargs: {"items": [run]},
    )
    run_control.request(project, run["id"], "pause")
    with pytest.raises(ValueError, match="already pending"):
        run_control.request(project, run["id"], "stop")


def test_post_insert_recheck_reports_applied_request_when_run_exits(tmp_path, monkeypatch):
    project = tmp_path / "shop"
    create_project(project, "https://example.test/")
    (project / "scans" / "scan.sqlite").write_bytes(b"scan")
    run = _active_run()
    calls = 0

    def status(*_args, **_kwargs):
        nonlocal calls
        calls += 1
        if calls == 1:
            return {"items": [run]}
        assert run_control.consume(project, run["id"]) == "pause"
        return {"items": []}

    monkeypatch.setattr("seohead.storage.open_scan", lambda *_args, **_kwargs: _Scan())
    monkeypatch.setattr("seohead.projects.run_observation.status", status)

    requested = run_control.request(project, run["id"], "pause")
    assert requested == {"ok": True, "run_id": run["id"], "state": "pause_applied"}


def test_post_insert_recheck_deletes_only_a_still_pending_request(tmp_path, monkeypatch):
    project = tmp_path / "shop"
    create_project(project, "https://example.test/")
    (project / "scans" / "scan.sqlite").write_bytes(b"scan")
    run = _active_run()
    calls = 0

    def status(*_args, **_kwargs):
        nonlocal calls
        calls += 1
        return {"items": [run]} if calls == 1 else {"items": []}

    monkeypatch.setattr("seohead.storage.open_scan", lambda *_args, **_kwargs: _Scan())
    monkeypatch.setattr("seohead.projects.run_observation.status", status)

    with pytest.raises(ValueError, match="no longer an active"):
        run_control.request(project, run["id"], "stop")
    with run_control._connect(project) as con:
        assert con.execute("SELECT 1 FROM requests WHERE run_id=?", (run["id"],)).fetchone() is None


def test_controlled_reporter_turns_the_next_progress_boundary_into_checkpoint_interrupt(
    tmp_path, monkeypatch
):
    project = tmp_path / "shop"
    create_project(project, "https://example.test/")
    reporter = run_control.ControlledRunReporter(project, "a" * 32)
    monkeypatch.setattr(run_control, "consume", lambda *_args: "pause")
    with pytest.raises(run_control.RunControlInterrupt) as exc_info:
        reporter(0, 1)
    assert exc_info.value.action == "pause"
    assert reporter.control_action == "pause"
