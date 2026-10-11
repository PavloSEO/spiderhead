"""Select saved finding URLs in temporary SQLite tables; the scan remains read-only."""

from __future__ import annotations

import sqlite3

from .inputs import resolve_audit_source


class UnmeasuredChecks(ValueError):
    """Requested saved checks were explicitly unmeasured, never an empty clean selection."""


def _code(value):
    return str(value).casefold().replace("-", "_")


def materialize(con, path, *, checks=None, severity=None, timeout_seconds=5.0):
    """Stream validated findings and group members into a bounded-memory URL selection.

    audit.v2 collections stream; the compatibility JSON reader retains its finite
    byte limit. No index is persisted and no finding is recalculated.
    """
    wanted = {_code(value) for value in ([checks] if isinstance(checks, str) else checks or [])}
    from .audit_v2 import AuditV2Reader, audit_v2_path

    if audit_v2_path(path).exists():
        source = AuditV2Reader(path, light_binding=True, timeout_seconds=timeout_seconds)
    else:
        source, _notices = resolve_audit_source(path)
    streaming = hasattr(source, "iter_collection")
    try:
        header = source.header if streaming else source
        skipped_rows = (
            source.iter_collection("/run/checks_skipped")
            if streaming and "/run/checks_skipped" in source.collections
            else (header.get("run") or {}).get("checks_skipped", [])
        )
        skipped = [row for row in skipped_rows if wanted and _code(row["id"]) in wanted]
        if skipped:
            raise UnmeasuredChecks("; ".join(f"{row['id']}: {row['reason']}" for row in skipped))
        coverage = (header.get("summary") or {}).get("check_coverage") or {}
        disabled = {_code(value) for value in coverage.get("checks_disabled_ids", [])}
        if wanted & disabled:
            raise UnmeasuredChecks(
                "requested check(s) disabled in saved audit: "
                + ", ".join(sorted(wanted & disabled))
            )
        measured = {_code(value) for value in coverage.get("checks_silent_ids", [])} & wanted
        issues = (
            source.iter_collection("/issues")
            if streaming and "/issues" in source.collections
            else header.get("issues", [])
        )
        groups = (
            source.iter_collection("/groups")
            if streaming and "/groups" in source.collections
            else header.get("groups", [])
        )
        con.execute("PRAGMA query_only=OFF")
        con.execute("PRAGMA temp_store=FILE")
        con.execute("CREATE TEMP TABLE issue_urls(url_id INTEGER PRIMARY KEY)")
        con.execute("CREATE TEMP TABLE issue_groups(group_id TEXT PRIMARY KEY) WITHOUT ROWID")
        for issue in issues:
            if wanted and _code(issue["check"]) in wanted:
                measured.add(_code(issue["check"]))
            if wanted and _code(issue["check"]) not in wanted:
                continue
            if severity and issue["severity"] != severity:
                continue
            details = issue.get("details")
            source_page = (
                details.get("source_page")
                if str(issue.get("check", "")).startswith("IMG_") and isinstance(details, dict)
                else None
            )
            for url in (issue.get("target_url"), source_page):
                if isinstance(url, str) and url:
                    con.execute(
                        "INSERT OR IGNORE INTO issue_urls SELECT url_id FROM urls WHERE url=?",
                        (url,),
                    )
            group = issue.get("group_id")
            if group:
                con.execute("INSERT OR IGNORE INTO issue_groups VALUES(?)", (group,))
        if wanted - measured:
            raise UnmeasuredChecks(
                "requested check(s) absent from saved audit measurement inventory: "
                + ", ".join(sorted(wanted - measured))
            )
        for group in groups:
            if not con.execute(
                "SELECT 1 FROM issue_groups WHERE group_id=?", (group["group_id"],)
            ).fetchone():
                continue
            for url in group.get("urls") or []:
                con.execute(
                    "INSERT OR IGNORE INTO issue_urls SELECT url_id FROM urls WHERE url=?", (url,)
                )
        con.execute("PRAGMA query_only=ON")
    except sqlite3.OperationalError as exc:
        if "interrupted" in str(exc).lower():
            from .audit_v2 import AuditReadTimeout

            raise AuditReadTimeout(
                "saved issue selection exceeded its explicit query budget"
            ) from exc
        raise
    finally:
        if streaming:
            source.close()
