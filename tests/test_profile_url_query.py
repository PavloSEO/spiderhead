"""The url-query latency baseline reads a scan and must leave its bytes untouched."""

from __future__ import annotations

import json
import sys

import pytest

from scripts import profile_url_query as profile
from tests.test_scan_url_query import _scan


@pytest.fixture(scope="module")
def scan_path(tmp_path_factory):
    return _scan(tmp_path_factory.mktemp("profile-url-query"))


def _run(monkeypatch, capsys, *argv):
    monkeypatch.setattr(sys, "argv", ["profile_url_query.py", *argv])
    code = profile.main()
    return code, json.loads(capsys.readouterr().out)


def test_acceptance_reports_every_case_and_keeps_scan_bytes(monkeypatch, capsys, scan_path):
    code, report = _run(
        monkeypatch, capsys, "--scan", str(scan_path), "--repeat", "2", "--min-urls", "1"
    )
    assert set(report["cases"]) == {
        *(name for name, *_ in profile.CASES),
        "all_facets",
        "whole_scan_issue_counters",
    }
    assert report["scan_unchanged"] is True
    assert report["urls"] > 0
    assert report["budget_ms"] == profile.BUDGET_MS
    assert report["cases"]["all_facets"]["facet_states_valid"] is True
    assert report["cases"]["all_facets"]["result_state"] in {"exact", "partial"}
    for case in report["cases"].values():
        assert case["ok"] is True
        assert case["rows"] <= profile.PAGE_ROWS
        assert case["min_ms"] <= case["median_ms"] <= case["max_ms"]
    assert code in {0, 3}


def test_deep_offset_past_the_end_returns_no_rows_without_error(monkeypatch, capsys, scan_path):
    _, report = _run(monkeypatch, capsys, "--scan", str(scan_path), "--repeat", "1")
    deep = report["cases"]["deep_page_sort_url"]
    assert deep["ok"] is True
    assert deep["rows"] == 0


def test_rejects_missing_scan_and_zero_repeats(monkeypatch, scan_path, tmp_path):
    monkeypatch.setattr(sys, "argv", ["p", "--scan", str(tmp_path / "absent.sqlite")])
    with pytest.raises(SystemExit):
        profile.main()
    monkeypatch.setattr(sys, "argv", ["p", "--scan", str(scan_path), "--repeat", "0"])
    with pytest.raises(SystemExit):
        profile.main()
    monkeypatch.setattr(sys, "argv", ["p", "--scan", str(scan_path), "--min-urls", "0"])
    with pytest.raises(SystemExit):
        profile.main()
