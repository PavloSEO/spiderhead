"""Owner URL caps, latest-scan reuse and report source for project preparation."""

from __future__ import annotations

import sqlite3
from datetime import datetime, timedelta, timezone

import pytest

from seohead.crawl.settings import fingerprint, load
from seohead.projects.runtime import latest_project_scan, prepare_project
from seohead.projects.workspace import create_project
from seohead.storage.native_scan import NativeScan
from tests.test_scan_history import _event, _metadata, _record, _runtime, _save_audit

UTC = timezone.utc


def _saved_scan(path, *, cap=50, stopped=False):
    config = load(overrides={"speed.min_delay_seconds": 0, "limits.max_urls": cap})
    metadata = {**_metadata(), "config": config, "config_fingerprint": fingerprint(config)}
    with NativeScan.create(path, **metadata) as scan:
        scan.enqueue([("https://example.test/", 0)])
        lease = scan.claim(1)[0]
        scan.commit_page(
            lease,
            _record(lease.url),
            captures=[_event(lease.url, b"<html>body</html>")],
            runtime=_runtime(),
        )
        if stopped:
            scan.stop_by_budget(f"url limit reached ({cap})")
        _save_audit(scan, config)
        assert scan.finish_capture("finished")
    finished_at = (datetime.now(UTC) - timedelta(days=1)).isoformat()
    with sqlite3.connect(path) as con:
        con.execute("UPDATE scan SET finished_at=?", (finished_at,))


def _project(tmp_path):
    project = tmp_path / "project"
    create_project(project, "https://example.test/")
    (project / "scans").mkdir(exist_ok=True)
    return project


def _refusing_crawl(calls):
    def crawl(**kwargs):
        calls.append(kwargs)
        return {"ok": False, "error": "crawl must not run"}

    return crawl


def test_preparation_reuses_latest_complete_scan_without_crawling(tmp_path):
    project = _project(tmp_path)
    _saved_scan(project / "scans" / "full.sqlite", cap=500)
    calls = []

    result = prepare_project(str(project), tools={"crawl_site": _refusing_crawl(calls)})

    assert calls == []
    crawl = result["preparation"]["steps"]["crawl"]
    assert crawl["state"] == "run"
    assert crawl["reused"] is True
    assert crawl["artifact"] == "scans/full.sqlite"


def test_requested_cap_is_passed_to_the_crawl_and_scales_other_budgets(tmp_path):
    project = _project(tmp_path)
    calls = []

    prepare_project(str(project), tools={"crawl_site": _refusing_crawl(calls)}, max_urls=120)

    assert calls[0]["overrides"] == {
        "limits.max_urls": 120,
        "limits.max_requests": 360,
        "limits.max_crawl_seconds": 240,
        "sitemaps.auto_discover": True,
    }


def test_cap_stop_is_reported_as_reaching_the_owner_limit_not_partial(tmp_path):
    project = _project(tmp_path)
    _saved_scan(
        project / "scans" / "capped.sqlite",
        cap=120,
        stopped=True,
    )
    calls = []

    result = prepare_project(
        str(project), tools={"crawl_site": _refusing_crawl(calls)}, max_urls=120
    )

    assert calls == []
    crawl = result["preparation"]["steps"]["crawl"]
    assert crawl["state"] == "run"
    assert crawl["partial"] is False
    assert crawl["reason"] == "reached your URL limit (120)"
    assert crawl["reused"] is True


def test_saved_scan_with_another_cap_is_not_reused_for_a_requested_cap(tmp_path):
    project = _project(tmp_path)
    _saved_scan(project / "scans" / "other.sqlite", cap=500)
    calls = []

    prepare_project(str(project), tools={"crawl_site": _refusing_crawl(calls)}, max_urls=120)

    assert len(calls) == 1


def test_non_integer_or_out_of_range_cap_is_refused(tmp_path):
    project = _project(tmp_path)
    for value in (0, -1, 1_000_001, True, "50"):
        with pytest.raises(ValueError, match="max_urls"):
            prepare_project(str(project), tools={"crawl_site": _refusing_crawl([])}, max_urls=value)


def test_latest_project_scan_is_the_newest_complete_audited_scan(tmp_path):
    project = _project(tmp_path)
    _saved_scan(project / "scans" / "partial.sqlite", cap=50, stopped=True)
    _saved_scan(project / "scans" / "full.sqlite", cap=500)

    assert latest_project_scan(str(project)).name == "full.sqlite"


def test_latest_project_scan_refuses_when_no_complete_scan_exists(tmp_path):
    project = _project(tmp_path)
    with pytest.raises(ValueError, match="no finished, complete audited scan"):
        latest_project_scan(str(project))
