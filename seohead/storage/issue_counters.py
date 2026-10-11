"""Exact, whole-audit issue and check counters for saved scan query summaries."""

from __future__ import annotations

import sqlite3
from pathlib import Path
from typing import Any

from seohead.sf.core.registry import CHECKS

from .audit_v2 import AuditReadTimeout, AuditV2Reader, audit_v2_path
from .inputs import resolve_audit_source


def summarize(
    path: str | Path,
    *,
    scan_connection=None,
    expected_source: dict[str, Any] | None = None,
    timeout_seconds: float = 5.0,
) -> dict[str, Any]:
    """Return persisted audit totals and registry-wide measured/skipped states.

    The summary is source evidence written by the audit engine. Missing fields or a missing
    audit stay unavailable; this function never reconstructs absent counts as zero.
    """
    source = None
    try:
        companion = audit_v2_path(path)
        if companion.exists():
            source = AuditV2Reader(path, light_binding=True, timeout_seconds=timeout_seconds)
            if expected_source is not None and (
                source.binding.get("scan_uuid") != expected_source.get("scan_uuid")
                or source.binding.get("evidence_revision")
                != expected_source.get("evidence_revision")
            ):
                return _unavailable("saved audit belongs to a different scan evidence revision")
            header = source.header
            run = dict(header.get("run") or {})
            coverage = dict((header.get("summary") or {}).get("check_coverage") or {})
            for target, pointer in (
                ("checks_skipped", "/run/checks_skipped"),
                ("checks_disabled", "/run/checks_disabled"),
            ):
                if pointer in source.collections:
                    run[target] = list(source.iter_collection(pointer))
            for target, pointer in (
                ("checks_silent_ids", "/summary/check_coverage/checks_silent_ids"),
                ("checks_disabled_ids", "/summary/check_coverage/checks_disabled_ids"),
            ):
                if pointer in source.collections:
                    coverage[target] = list(source.iter_collection(pointer))
            header = {
                **header,
                "run": run,
                "summary": {
                    **header.get("summary", {}),
                    "check_coverage": coverage,
                },
            }
        elif scan_connection is not None:
            row = scan_connection.execute(
                "SELECT document_json,evidence_revision FROM audit WHERE singleton=1"
            ).fetchone()
            if row is None:
                return _unavailable("saved scan has no current audit")
            if expected_source is not None and row["evidence_revision"] != expected_source.get(
                "evidence_revision"
            ):
                return _unavailable("saved audit belongs to a different scan evidence revision")
            import json

            header = json.loads(row["document_json"])
        else:
            source, _ = resolve_audit_source(path)
            header = source.header if hasattr(source, "header") else source
        summary = header.get("summary")
        run = header.get("run")
        if not isinstance(summary, dict) or not isinstance(run, dict):
            return _unavailable("saved audit does not contain a complete summary")
        totals = summary.get("totals")
        by_severity = summary.get("by_severity")
        by_check = summary.get("by_check")
        coverage = summary.get("check_coverage")
        if not all(isinstance(item, dict) for item in (totals, by_severity, by_check, coverage)):
            return _unavailable("saved audit summary counters are missing")
        issues_total = totals.get("issues_total")
        findings_total = totals.get("findings_total", issues_total)
        if any(type(value) is not int or value < 0 for value in (issues_total, findings_total)):
            return _unavailable("saved audit issue totals are missing")
        severities = {}
        for name in ("critical", "warning", "notice"):
            value = by_severity.get(name)
            if type(value) is not int or value < 0:
                return _unavailable(f"saved audit {name} counter is missing")
            severities[name] = value
        checks = by_check
        if any(
            not isinstance(key, str) or type(value) is not int or value < 0
            for key, value in checks.items()
        ):
            return _unavailable("saved audit per-check counters are invalid")
        coverage_counts = [
            coverage.get(key)
            for key in (
                "checks_total",
                "checks_fired",
                "checks_skipped",
                "checks_disabled",
                "checks_silent",
            )
        ]
        if any(type(value) is not int or value < 0 for value in coverage_counts):
            return _unavailable("saved audit check coverage counters are missing")
        exclusions = summary.get("finding_exclusions") or {}
        suppressed_by_check = exclusions.get("by_check") or {}
        check_states = _check_states(CHECKS, checks, coverage, run, suppressed_by_check)
        return {
            "state": "exact",
            "reason": "",
            "issues_total": issues_total,
            "findings_total": findings_total,
            "by_severity": severities,
            "by_check": dict(checks),
            **coverage_summary(coverage),
            "checks": check_states,
        }
    except (OSError, ValueError, KeyError, TypeError, sqlite3.Error, AuditReadTimeout) as exc:
        return _unavailable(f"saved audit counters are unavailable: {exc}")
    finally:
        if source is not None and hasattr(source, "close"):
            source.close()


def _check_states(registry, by_check, coverage, run, suppressed_by_check) -> list[dict[str, Any]]:
    skipped = {
        str(item.get("id", "")).upper(): str(item.get("reason") or "check was skipped")
        for item in run.get("checks_skipped", [])
        if isinstance(item, dict)
    }
    disabled_rows = run.get("checks_disabled", [])
    disabled = {
        (str(item.get("id", "")).upper() if isinstance(item, dict) else str(item).upper()): (
            str(item.get("reason") or "disabled in profile")
            if isinstance(item, dict)
            else "disabled in profile"
        )
        for item in disabled_rows
    }
    disabled.update(
        {
            str(value).upper(): "disabled in profile"
            for value in coverage.get("checks_disabled_ids", [])
        }
    )
    silent = {str(value).upper() for value in coverage.get("checks_silent_ids", [])}
    checks = {str(key).upper(): value for key, value in by_check.items()}
    suppressed = {str(key).upper(): value for key, value in suppressed_by_check.items()}
    output = []
    for check_id, definition in registry.items():
        key = str(check_id).upper()
        if key in checks:
            state, reason, findings = "fired", "", checks[key]
        elif key in silent:
            state, reason, findings = "silent", "", 0
        elif key in suppressed:
            state, reason, findings = (
                "fired_suppressed",
                "all findings suppressed by saved policy",
                0,
            )
        elif key in skipped:
            state, reason, findings = "skipped", skipped[key], None
        elif key in disabled:
            state, reason, findings = "disabled", disabled[key], None
        else:
            state, reason, findings = "unavailable", "check state is absent from saved audit", None
        output.append(
            {
                "id": check_id,
                "severity": definition.get("severity"),
                "state": state,
                "findings": findings,
                "suppressed_findings": suppressed.get(key, 0),
                "reason": reason,
            }
        )
    return output


def _unavailable(reason: str) -> dict[str, Any]:
    return {
        "state": "unavailable",
        "reason": reason,
        "issues_total": None,
        "findings_total": None,
        "by_severity": None,
        "by_check": None,
        "checks_run": None,
        "checks_skipped": None,
        "checks_disabled": None,
        "checks_total": None,
        "checks": None,
    }


def coverage_summary(coverage: Any) -> dict[str, Any]:
    """Project exact evaluated/skipped/disabled registry totals from saved coverage metadata."""
    keys = ("checks_total", "checks_fired", "checks_skipped", "checks_disabled", "checks_silent")
    if not isinstance(coverage, dict) or any(
        type(coverage.get(key)) is not int or coverage[key] < 0 for key in keys
    ):
        return {
            "state": "unavailable",
            "reason": "saved audit check coverage counters are missing",
            "checks_run": None,
            "checks_skipped": None,
            "checks_disabled": None,
            "checks_total": None,
        }
    return {
        "state": "exact",
        "reason": "",
        "checks_run": coverage["checks_fired"] + coverage["checks_silent"],
        "checks_skipped": coverage["checks_skipped"],
        "checks_disabled": coverage["checks_disabled"],
        "checks_total": coverage["checks_total"],
    }
