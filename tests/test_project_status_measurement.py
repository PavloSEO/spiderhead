"""Project size observations are persisted and read without a directory walk."""

from __future__ import annotations

from seohead.projects.status_measurement import (
    read_storage_measurement,
    refresh_storage_measurement,
)
from seohead.projects.workspace import create_project


def test_storage_measurement_is_cached_and_reports_direct_scan_files(tmp_path):
    project = tmp_path / "site"
    create_project(project, "https://example.test/")
    scan = project / "scans" / "first.sqlite"
    scan.write_bytes(b"synthetic scan")
    (project / "scans" / "first.sqlite-wal").write_bytes(b"wal")

    measured = refresh_storage_measurement(project)
    cached = read_storage_measurement(project)

    assert measured["state"] == "available"
    assert measured["total_bytes"] == len(b"synthetic scan") + len(b"wal")
    assert measured["scan_count"] == 1
    assert cached == measured


def test_nested_scan_directory_is_explicitly_unavailable(tmp_path):
    project = tmp_path / "site"
    create_project(project, "https://example.test/")
    (project / "scans" / "nested").mkdir()

    measured = refresh_storage_measurement(project)

    assert measured["state"] == "unavailable"
    assert measured["total_bytes"] is None
    assert "nested" in measured["reason"]
