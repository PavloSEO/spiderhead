"""Per-URL state across retained scans: absent, unreadable and changed states, numbering, limits."""

from __future__ import annotations

import hashlib

import pytest

from seohead import cli
from seohead.mcp.mcp_server import build_server
from seohead.storage import url_history
from seohead.storage.history import _metadata
from seohead.storage.native_scan import NativeScan
from tests.test_native_capture import _event
from tests.test_scan_native import _link, _record, _runtime
from tests.test_scan_native import _metadata as _scan_metadata

URL = "https://example.test/page"


def _make_scan(path, pages):
    """Write one native scan; pages maps URL -> field overrides of its page record."""
    with NativeScan.create(path, **_scan_metadata()) as scan:
        urls = list(pages) or ["https://example.test/"]
        scan.enqueue([(u, 0) for u in urls])
        for url in urls:
            record = _record(url) | {"crawl_depth": 0} | pages.get(url, {})
            scan.commit_page(
                scan.claim(1)[0], record, runtime=_runtime() | {"max_depth_reached": 1}
            )
        scan.finish_capture()
    return path


@pytest.fixture()
def scans(tmp_path):
    """Three scans in one directory: oldest has the URL 404, middle lacks it, newest has it 200."""
    directory = tmp_path / "scans"
    directory.mkdir()
    old = _make_scan(
        directory / "old.sqlite",
        {URL: {"status_code": 404, "title": "Gone"}, "https://example.test/": {}},
    )
    middle = _make_scan(directory / "middle.sqlite", {"https://example.test/": {}})
    new = _make_scan(
        directory / "new.sqlite",
        {
            URL: {
                "status_code": 200,
                "title": "Back",
                "meta_robots": "noindex",
                "canonical": "https://example.test/canonical",
            }
        },
    )
    uuids = {str(p): _metadata(p)["uuid"] for p in (old, middle, new)}
    return directory, uuids, {"old": old, "middle": middle, "new": new}


def test_history_lists_newest_first_with_numbering_and_absence(scans):
    directory, uuids, paths = scans
    got = url_history.url_history(directory, URL)
    assert got["ok"] is True and got["format"] == url_history.FORMAT
    assert got["scan_total"] == 3 and got["returned"] == 3 and got["has_more"] is False
    by_uuid = {row["scan_uuid"]: row for row in got["scans"]}
    assert [row["number"] for row in got["scans"]] == [3, 2, 1]
    assert got["scans"][0]["scan_uuid"] == uuids[str(paths["new"])]

    newest, middle, oldest = got["scans"]
    assert newest["state"] == "present" and newest["status_code"] == 200
    assert newest["indexability"] is False  # 2xx but noindex
    assert newest["canonical"] == "https://example.test/canonical"
    assert newest["title_hash"] == hashlib.sha256(b"Back").hexdigest()
    assert newest["title"] == "Back"
    assert newest["word_count"] is not None and newest["html_size_bytes"] is not None
    assert "response_time_seconds" in newest and "inlinks" in newest and "outlinks" in newest
    assert middle["state"] == "absent" and middle["status_code"] is None
    assert oldest["state"] == "present" and oldest["status_code"] == 404
    assert oldest["indexability"] is False
    assert oldest["title_hash"] == hashlib.sha256(b"Gone").hexdigest()
    transition = got["transitions"][0]
    assert transition["state"] == "not_comparable"
    assert transition["changes"] == {}
    assert got["transitions"][1]["state"] == "not_comparable"
    assert len(by_uuid) == 3


def test_limit_keeps_numbers_stable_and_flags_more(scans):
    directory, _uuids, _paths = scans
    got = url_history.url_history(directory, URL, limit=1)
    assert got["returned"] == 1 and got["has_more"] is True
    assert got["scans"][0]["number"] == 3


def test_unreadable_scan_is_reported_not_mistaken_for_absence(scans):
    directory, _uuids, paths = scans
    paths["middle"].write_bytes(b"not a sqlite file at all" * 100)
    got = url_history.url_history(directory, URL)
    # The corrupt file fails the catalog's metadata check: it is listed in errors with its path,
    # and it never shows up as an "absent" scan that would read as "the URL was not there".
    assert got["state"] == "partial" and got["scan_total"] == 2
    assert [row["state"] for row in got["scans"]] == ["present", "present"]
    assert any(str(paths["middle"]) == err["path"] for err in got["errors"])


def test_url_absent_everywhere_and_bounds(scans):
    directory, _uuids, _paths = scans
    got = url_history.url_history(directory, "https://example.test/nowhere")
    assert [row["state"] for row in got["scans"]] == ["absent"] * 3
    with pytest.raises(ValueError):
        url_history.url_history(directory, "")
    with pytest.raises(ValueError):
        url_history.url_history(directory, URL, limit=0)
    with pytest.raises(ValueError):
        url_history.url_history(directory, URL, limit=url_history.MAX_LIMIT + 1)


def test_history_reports_only_retained_external_link_response_codes(tmp_path):
    directory = tmp_path / "scans"
    directory.mkdir()
    external = "https://other.test/resource"
    path = directory / "external.sqlite"
    with NativeScan.create(path, **_scan_metadata()) as scan:
        scan.begin_collection()
        # External response evidence is retained on the destination's own frontier lease.
        scan.enqueue([(URL, 0), (external, 1)])
        scan.commit_page(
            scan.claim(1)[0],
            _record(URL) | {"crawl_depth": 0},
            links=[_link(URL, external)],
            runtime=_runtime() | {"max_depth_reached": 1},
        )
        scan.commit_page(
            scan.claim(1)[0],
            _record(external) | {"crawl_depth": 1, "status_code": 404},
            captures=[_event(external, status_code=404, effective_status_code=404)],
            runtime=_runtime() | {"max_depth_reached": 1},
        )
        scan.finish_capture()
    got = url_history.url_history(directory, URL)
    assert got["scans"][0]["external_link_response_codes"] == {
        "state": "measured",
        "external_targets": 1,
        "external_targets_observed": 1,
        "responses_measured": 1,
        "codes": {"404": 1},
    }


def test_history_keeps_unfetched_external_link_responses_unavailable(tmp_path):
    directory = tmp_path / "scans"
    directory.mkdir()
    _make_scan(
        directory / "unfetched.sqlite",
        {URL: {}},
    )
    # This helper-created scan has no retained complete link inventory; its apparent lack of
    # response rows must not be reported as zero external responses.
    got = url_history.url_history(directory, URL)
    evidence = got["scans"][0]["external_link_response_codes"]
    assert evidence["state"] == "unavailable"
    assert evidence["reason"]


def test_cli_rejects_a_directory_that_is_not_a_project(capsys, tmp_path):
    # A bare scans/ folder has no project.json: the command must fail, not report empty history.
    (tmp_path / "scans").mkdir()
    code = cli.main(["scan-url-history", "--project", str(tmp_path), "--url", URL])
    assert code != 0
    tool = build_server()._tool_manager.get_tool("seo_scan_url_history")
    assert tool.annotations.readOnlyHint is True
    assert set(tool.parameters["properties"]) >= {"project", "url", "limit"}
    capsys.readouterr()
