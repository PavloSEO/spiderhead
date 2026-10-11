"""Whole-scan counters preserve the saved audit's explicit measured and unavailable states."""

from __future__ import annotations

from seohead.storage import issue_counters


def test_summary_keeps_fired_silent_skipped_and_disabled_checks_distinct(tmp_path, monkeypatch):
    monkeypatch.setattr(
        issue_counters,
        "CHECKS",
        {
            "FIRED": {"severity": "critical"},
            "SILENT": {"severity": "warning"},
            "SKIPPED": {"severity": "notice"},
            "DISABLED": {"severity": "warning"},
        },
    )
    audit = {
        "run": {
            "checks_skipped": [{"id": "SKIPPED", "reason": "no_source_export"}],
            "checks_disabled": [{"id": "DISABLED", "reason": "disabled_in_profile"}],
        },
        "summary": {
            "totals": {"issues_total": 2},
            "by_severity": {"critical": 1, "warning": 1, "notice": 0},
            "by_check": {"FIRED": 2},
            "check_coverage": {
                "checks_total": 4,
                "checks_fired": 1,
                "checks_skipped": 1,
                "checks_disabled": 1,
                "checks_silent": 1,
                "checks_silent_ids": ["SILENT"],
                "checks_disabled_ids": ["DISABLED"],
            },
        },
    }
    monkeypatch.setattr(issue_counters, "resolve_audit_source", lambda _path: (audit, []))

    got = issue_counters.summarize(tmp_path / "scan.sqlite")

    assert got["state"] == "exact"
    assert got["issues_total"] == 2
    assert got["by_severity"] == {"critical": 1, "warning": 1, "notice": 0}
    assert got["checks_run"] == 2
    assert got["checks_run"] + got["checks_skipped"] + got["checks_disabled"] == got["checks_total"]
    assert {row["id"]: row["state"] for row in got["checks"]} == {
        "FIRED": "fired",
        "SILENT": "silent",
        "SKIPPED": "skipped",
        "DISABLED": "disabled",
    }
    assert got["checks"][2]["reason"] == "no_source_export"


def test_missing_counter_fields_are_unavailable_not_zero(tmp_path, monkeypatch):
    monkeypatch.setattr(issue_counters, "CHECKS", {})
    monkeypatch.setattr(
        issue_counters,
        "resolve_audit_source",
        lambda _path: ({"run": {}, "summary": {"totals": {}}}, []),
    )

    got = issue_counters.summarize(tmp_path / "scan.sqlite")

    assert got["state"] == "unavailable"
    assert got["issues_total"] is None
    assert got["by_severity"] is None
