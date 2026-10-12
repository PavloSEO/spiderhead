"""Private project history and the local daily cap for explicit GSC inspection samples."""

from datetime import datetime, timezone

import pytest

from seohead.data_sources import credentials, gsc_inspection_history
from seohead.projects import workspace


def _project(tmp_path):
    return workspace.create_project(str(tmp_path / "project"), "https://example.test")


def test_gsc_inspection_reservation_enforces_per_site_daily_cap_and_releases_missing_auth(
    tmp_path, monkeypatch
):
    config_root = tmp_path / "config"
    config_root.mkdir(mode=0o700)
    monkeypatch.setattr(credentials, "CONFIG_ROOT", config_root)
    project = _project(tmp_path)["path"]
    now = datetime(2026, 10, 12, 10, tzinfo=timezone.utc)
    first = gsc_inspection_history.reserve(
        project,
        "sc-domain:example.test",
        [f"https://example.test/{index}" for index in range(50)],
        now=now,
    )
    second = gsc_inspection_history.reserve(
        project,
        "sc-domain:example.test",
        [f"https://example.test/extra-{index}" for index in range(1990)],
        now=now,
    )

    assert first["reserved"] == 50 and first["remaining_after"] == 1950
    assert second["reserved"] == 50 and second["deferred"] == 1940
    released = gsc_inspection_history.settle(
        project, first["reservation_id"], [], requests_made=0, now=now
    )
    assert released["state"] == "released"
    third = gsc_inspection_history.reserve(
        project,
        "sc-domain:example.test",
        ["https://example.test/retry"],
        now=now,
    )
    assert third["reserved"] == 1


def test_gsc_inspection_history_keeps_only_sanitized_result_fields(tmp_path, monkeypatch):
    config_root = tmp_path / "config"
    config_root.mkdir(mode=0o700)
    monkeypatch.setattr(credentials, "CONFIG_ROOT", config_root)
    project = _project(tmp_path)["path"]
    reservation = gsc_inspection_history.reserve(
        project, "sc-domain:example.test", ["https://example.test/a"]
    )
    stored = gsc_inspection_history.settle(
        project,
        reservation["reservation_id"],
        [
            {
                "url": "https://example.test/a",
                "state": "complete",
                "index_status": {"verdict": "PASS", "userCanonical": "https://example.test/a"},
                "amp_result": {"verdict": "FAIL"},
                "rich_results_result": {"verdict": "PASS"},
                "token": "must-not-be-stored",
            }
        ],
        requests_made=1,
    )
    history = gsc_inspection_history.latest(project, "sc-domain:example.test")

    assert stored["stored"] == 1
    assert history["rows"][0]["index_status"]["verdict"] == "PASS"
    assert "token" not in str(history)


def test_gsc_inspection_settlement_rejects_urls_outside_reserved_population(tmp_path, monkeypatch):
    config_root = tmp_path / "config"
    config_root.mkdir(mode=0o700)
    monkeypatch.setattr(credentials, "CONFIG_ROOT", config_root)
    project = _project(tmp_path)["path"]
    reservation = gsc_inspection_history.reserve(
        project, "sc-domain:example.test", ["https://example.test/a"]
    )
    with pytest.raises(ValueError, match="reserved population"):
        gsc_inspection_history.settle(
            project,
            reservation["reservation_id"],
            [
                {
                    "url": "https://other.example.test/private",
                    "state": "complete",
                    "index_status": {"verdict": "PASS"},
                }
            ],
            requests_made=1,
        )


def test_gsc_inspection_latest_history_is_paged_by_url(tmp_path, monkeypatch):
    config_root = tmp_path / "config"
    config_root.mkdir(mode=0o700)
    monkeypatch.setattr(credentials, "CONFIG_ROOT", config_root)
    project = _project(tmp_path)["path"]
    urls = ["https://example.test/a", "https://example.test/b"]
    reservation = gsc_inspection_history.reserve(project, "sc-domain:example.test", urls)
    gsc_inspection_history.settle(
        project,
        reservation["reservation_id"],
        [{"url": url, "state": "complete", "index_status": {"verdict": "PASS"}} for url in urls],
        requests_made=2,
    )

    first = gsc_inspection_history.latest(project, "sc-domain:example.test", limit=1)
    second = gsc_inspection_history.latest(
        project, "sc-domain:example.test", limit=1, offset=first["next_offset"]
    )

    assert first["total"] == 2 and first["has_more"] is True
    assert first["next_offset"] == 1
    assert second["has_more"] is False
    assert len({first["rows"][0]["url"], second["rows"][0]["url"]}) == 2
