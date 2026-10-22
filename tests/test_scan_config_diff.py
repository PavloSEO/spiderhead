"""Field-by-field diff of two saved crawl configurations (#1529).

Read-only: both scans' recorded effective settings are read from their
headers, the sites are never re-read, and every difference shows both values.
"""

from __future__ import annotations

import json
import subprocess
import sys

from seohead.storage.config_diff import diff_configs, scan_config_diff
from seohead.storage.native_scan import NativeScan
from tests.test_scan_native import _metadata, _runtime


def _write_scan(path, *, overrides):
    with NativeScan.create(path, **_metadata(**overrides)) as scan:
        scan.enqueue([("https://example.test/", 0)])
        lease = scan.claim(1)[0]
        from tests.test_scan_native import _record

        record = _record("https://example.test/")
        record["crawl_depth"] = lease.depth
        runtime = _runtime()
        runtime["max_depth_reached"] = lease.depth
        scan.commit_page(lease, record, links=[], runtime=runtime)
        scan.finish_capture()
    return path


def test_three_changed_settings_list_exactly_those_three_fields():
    before = {
        "limits": {"max_urls": 100, "max_depth": 5},
        "robots": {"policy": "respect"},
        "http": {"timeout_seconds": 10.0},
    }
    after = {
        "limits": {"max_urls": 250, "max_depth": 5},
        "robots": {"policy": "ignore"},
        "http": {"timeout_seconds": 10.0, "user_agent": "bot"},
    }
    result = diff_configs(before, after)
    assert [item["path"] for item in result["changed"]] == [
        "limits.max_urls",
        "robots.policy",
    ]
    assert result["changed"][0] == {"path": "limits.max_urls", "before": 100, "after": 250}
    assert result["added"] == [{"path": "http.user_agent", "after": "bot"}]
    assert result["removed"] == []
    assert result["identical"] is False


def test_identical_configurations_report_no_differences():
    config = {"limits": {"max_urls": 10}, "http": {"headers": {"x": "1"}}}
    result = diff_configs(config, json.loads(json.dumps(config)))
    assert result["identical"] is True
    assert result["changed"] == result["added"] == result["removed"] == []


def test_two_saved_scans_diff_through_their_recorded_headers(tmp_path):
    before = _write_scan(tmp_path / "a.sqlite", overrides={"limits.max_urls": 10})
    after = _write_scan(
        tmp_path / "b.sqlite",
        overrides={"limits.max_urls": 10, "robots.policy": "ignore", "speed.adaptive": False},
    )
    result = scan_config_diff(before, after)
    paths = [item["path"] for item in result["changed"]]
    assert "robots.policy" in paths and "speed.adaptive" in paths
    item = next(i for i in result["changed"] if i["path"] == "robots.policy")
    assert item["before"] == "respect" and item["after"] == "ignore"
    assert result["identical"] is False


def test_two_identical_saved_scans_are_identical(tmp_path):
    before = _write_scan(tmp_path / "a.sqlite", overrides={"limits.max_urls": 10})
    after = _write_scan(tmp_path / "b.sqlite", overrides={"limits.max_urls": 10})
    assert scan_config_diff(before, after)["identical"] is True


def test_the_cli_runs_the_same_diff(tmp_path):
    before = _write_scan(tmp_path / "a.sqlite", overrides={"limits.max_urls": 10})
    after = _write_scan(tmp_path / "b.sqlite", overrides={"limits.max_urls": 20})
    out = subprocess.run(
        [
            sys.executable,
            "-m",
            "seohead",
            "scan-config-diff",
            "--before",
            str(before),
            "--after",
            str(after),
        ],
        capture_output=True,
        text=True,
        check=True,
    )
    payload = json.loads(out.stdout)
    assert payload["changed"] == [{"path": "limits.max_urls", "before": 10, "after": 20}]
