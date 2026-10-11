"""Scheduled scans: due rules across zones, overlap, budgets, UA identity, and trigger text."""

from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path

import pytest

from seohead.crawl import settings as crawl_settings
from seohead.crawl.user_agents import DEFAULT_TOKEN, DEFAULT_USER_AGENT, resolve
from seohead.projects import schedule, schedule_install
from seohead.projects.schedule import due_state, infer_time_zone, next_due, validate
from seohead.projects.workspace import create_project


def _settings(**overrides):
    base = schedule.default_settings("00000000-0000-4000-8000-000000000001")
    base["enabled"] = True
    base.update(overrides)
    return validate(base)


def _utc(text: str) -> datetime:
    return datetime.fromisoformat(text).replace(tzinfo=timezone.utc)


def test_window_is_evaluated_in_site_time_zone_across_dst():
    settings = _settings(time_zone="Europe/Minsk", window={"start": "01:00", "end": "06:00"})
    # 2026-10-12 00:30 UTC is 03:30 in Minsk (UTC+3): inside the window.
    inside = due_state(settings, "example.by", _utc("2026-10-12T00:30:00"), None)
    assert inside["due"] is True and inside["window"] == "2026-10-12"
    # 2026-10-12 04:30 UTC is 07:30 in Minsk: outside.
    assert due_state(settings, "example.by", _utc("2026-10-12T04:30:00"), None)["due"] is False


def test_tld_inference_and_fallback_are_reported():
    assert infer_time_zone("site.by") == ("Europe/Minsk", "tld")
    assert infer_time_zone("site.example") == ("UTC", "fallback")
    settings = _settings(time_zone="auto", window={"start": "01:00", "end": "06:00"})
    decision = due_state(settings, "site.example", _utc("2026-10-12T02:00:00"), None)
    assert decision["time_zone"] == "UTC" and decision["time_zone_source"] == "fallback"


def test_midnight_crossing_window_belongs_to_the_day_it_opened():
    settings = _settings(time_zone="UTC", window={"start": "23:00", "end": "02:00"})
    late = due_state(settings, "x.test", _utc("2026-10-12T23:30:00"), None)
    early = due_state(settings, "x.test", _utc("2026-10-13T01:30:00"), "2026-10-12")
    assert late["window"] == "2026-10-12"
    assert early["window"] == "2026-10-12"
    assert early["due"] is False and early["reason"] == "already ran in this window"
    assert due_state(settings, "x.test", _utc("2026-10-13T01:30:00"), None)["due"] is True


def test_weekdays_and_month_days_combine_as_alternatives():
    settings = _settings(
        time_zone="UTC",
        weekdays=["mon"],
        month_days=[15],
        window={"start": "01:00", "end": "06:00"},
    )
    monday = due_state(settings, "x.test", _utc("2026-10-12T02:00:00"), None)  # Monday
    tuesday = due_state(settings, "x.test", _utc("2026-10-13T02:00:00"), None)
    fifteenth = due_state(settings, "x.test", _utc("2026-10-15T02:00:00"), None)  # Thursday
    assert monday["due"] and fifteenth["due"]
    assert not tuesday["due"]


def test_next_due_skips_unselected_days():
    settings = _settings(
        time_zone="UTC", weekdays=["sat"], window={"start": "01:00", "end": "06:00"}
    )
    assert next_due(settings, "x.test", _utc("2026-10-12T10:00:00")) == "2026-10-17T01:00:00Z"


def test_validation_rejects_unbounded_or_unknown_settings():
    with pytest.raises(ValueError):
        _settings(time_zone="Mars/Olympus")
    with pytest.raises(ValueError):
        _settings(crawl={**_settings()["crawl"], "min_delay_seconds": 0.1})
    with pytest.raises(ValueError):
        _settings(surprise=True)
    with pytest.raises(ValueError):
        _settings(crawl={**_settings()["crawl"], "mode": "sitemap", "sitemap_url": None})


def test_user_agent_presets_custom_and_default_identity():
    assert resolve(None) == (DEFAULT_USER_AGENT, DEFAULT_TOKEN)
    assert DEFAULT_USER_AGENT.startswith("SpiderHeadBot/")
    assert "github.com/PavloSEO/spiderhead" in DEFAULT_USER_AGENT
    ua, token = resolve("googlebot")
    assert token == "Googlebot" and "Googlebot/2.1" in ua
    assert "Screaming" not in " ".join(resolve(name)[0] for name in ("bingbot", "gptbot"))
    assert resolve("My-Crawler/2.0 (+https://example.test)") == (
        "My-Crawler/2.0 (+https://example.test)",
        "My-Crawler",
    )
    with pytest.raises(ValueError):
        resolve("bad\r\nheader")


def test_default_identity_reaches_crawl_settings_and_robots_token():
    config = crawl_settings.load()
    assert config["http"]["user_agent"] == DEFAULT_USER_AGENT
    assert config["robots"]["user_agent_token"] == DEFAULT_TOKEN
    custom = crawl_settings.load(overrides={"http.user_agent": "MyBot/1.0 (x)"})
    assert custom["robots"]["user_agent_token"] == "MyBot"


def _project(tmp_path: Path) -> Path:
    root = tmp_path / "site"
    create_project(root, "https://example.by/")
    return root


def test_configure_is_revisioned_and_rejects_stale_writes(tmp_path):
    project = _project(tmp_path)
    first = schedule.configure(project, expected_revision=0, enabled=True, time_zone="Europe/Minsk")
    assert first["revision"] == 1 and first["settings"]["enabled"] is True
    with pytest.raises(ValueError, match="revision conflict"):
        schedule.configure(project, expected_revision=0, enabled=False)
    with pytest.raises(ValueError):
        schedule.configure(project, expected_revision=1, crawl={"mode": "bogus"})
    shown = schedule.show(project, now=_utc("2026-10-12T00:30:00"))
    assert shown["revision"] == 1 and shown["due"]["due"] is True


def _fake_scan(calls, *, partial=False):
    def scan(**kwargs):
        calls.append(kwargs)
        return {
            "ok": True,
            "scan": f"/scan/{len(calls)}.seohead",
            "partial": partial,
            "finish_reason": "x",
        }

    return scan


def test_run_due_passes_budgets_user_agent_and_writes_journal(tmp_path):
    project = _project(tmp_path)
    schedule.configure(
        project,
        expected_revision=0,
        enabled=True,
        time_zone="Europe/Minsk",
        user_agent={"preset": "bingbot", "custom": None},
        crawl={"max_urls": 300, "max_requests": 900, "min_delay_seconds": 2.0},
    )
    calls = []
    result = schedule.run_due(project, scan=_fake_scan(calls), now=_utc("2026-10-12T00:30:00"))
    assert result["ran"] is True and result["ok"] is True
    assert calls[0]["user_agent"] == "bingbot"
    assert calls[0]["max_urls"] == 300 and calls[0]["overrides"] == {"limits.max_requests": 900}
    assert calls[0]["min_delay"] == 2.0 and calls[0]["approve_large_crawl"] is False
    log = (project / "log.md").read_text(encoding="utf-8")
    assert "primary finished" in log
    # The same window never runs twice.
    again = schedule.run_due(project, scan=_fake_scan(calls), now=_utc("2026-10-12T01:00:00"))
    assert again["ran"] is False and len(calls) == 1


def test_run_due_refuses_overlap_and_stale_lock_is_reclaimed(tmp_path):
    project = _project(tmp_path)
    schedule.configure(project, expected_revision=0, enabled=True, time_zone="UTC")
    lock = project / ".schedule.lock"
    lock.write_text("held|2026-10-12T00:00:00+00:00", encoding="utf-8")
    calls = []
    held = schedule.run_due(project, scan=_fake_scan(calls), now=_utc("2026-10-12T02:00:00"))
    assert held == {"ok": True, "ran": False, "reason": "overlap refused"} and not calls
    assert lock.exists()
    late = schedule.run_due(project, scan=_fake_scan(calls), now=_utc("2026-10-13T03:00:00"))
    assert late["ran"] is True and not lock.exists()


def test_slice_mode_resumes_the_previous_partial_artifact(tmp_path):
    project = _project(tmp_path)
    schedule.configure(
        project, expected_revision=0, enabled=True, time_zone="UTC", crawl={"slice": True}
    )
    calls = []
    schedule.run_due(project, scan=_fake_scan(calls, partial=True), now=_utc("2026-10-12T02:00:00"))
    schedule.run_due(project, scan=_fake_scan(calls), now=_utc("2026-10-13T02:00:00"))
    assert calls[1] == {
        "resume": "/scan/1.seohead",
        "project": str(project),
        "approve_large_crawl": False,
    }


def test_list_mode_uses_project_urls_and_refuses_without_them(tmp_path):
    project = _project(tmp_path)
    schedule.configure(
        project,
        expected_revision=0,
        enabled=True,
        time_zone="UTC",
        crawl={"mode": "list", "urls": ["https://example.by/a", "https://example.by/b"]},
    )
    calls = []
    schedule.run_due(project, scan=_fake_scan(calls), now=_utc("2026-10-12T02:00:00"))
    assert calls[0]["urls"] == ["https://example.by/a", "https://example.by/b"]
    schedule.configure(project, expected_revision=1, crawl={"urls": []})
    result = schedule.run_due(project, scan=_fake_scan(calls), now=_utc("2026-10-13T02:00:00"))
    assert result["ok"] is False and "list mode has no URLs" in result["runs"][0]["error"]


def test_failure_of_one_run_is_recorded_not_raised(tmp_path):
    project = _project(tmp_path)
    schedule.configure(project, expected_revision=0, enabled=True, time_zone="UTC")

    def boom(**kwargs):
        raise OSError("network down")

    result = schedule.run_due(project, scan=boom, now=_utc("2026-10-12T02:00:00"))
    assert result["ok"] is False and result["runs"][0]["error"] == "OSError"
    state = json.loads((project / "schedule-state.json").read_text(encoding="utf-8"))
    assert state["last_window"] == "2026-10-12"


def test_install_writes_only_to_the_given_destination(tmp_path):
    project = _project(tmp_path)
    plist_path = tmp_path / "agents" / "x.plist"
    out = schedule_install.install(
        str(project), kind="launchd", dest=str(plist_path), python="/usr/bin/python3"
    )
    assert out["loaded"] is False and plist_path.exists()
    import plistlib

    data = plistlib.loads(plist_path.read_bytes())
    assert data["ProgramArguments"][:3] == ["/usr/bin/python3", "-m", "seohead.cli"]
    assert data["StartInterval"] == 3600
    cron = schedule_install.install(str(project), kind="cron", dest=str(tmp_path / "cron.txt"))
    assert cron["line"].startswith("0 * * * * ") and "schedule-run-due" in cron["line"]
    win = schedule_install.install(str(project), kind="windows")
    assert win["command"].startswith("schtasks /Create") and win["written"] is None
    removed = schedule_install.uninstall(str(project), kind="launchd", dest=str(plist_path))
    assert removed["removed"] == str(plist_path) and not plist_path.exists()
