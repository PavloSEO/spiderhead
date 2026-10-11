"""Read-only projection for terminal and future web project observers.

This module deliberately composes the existing project, coverage, preparation,
scan-history and inbox stores.  It owns no crawl, task, finding, or job state.
"""

from __future__ import annotations

import copy
import hashlib
import json
import re
from collections import OrderedDict
from contextlib import closing
from datetime import datetime, timezone
from pathlib import Path
from threading import RLock
from typing import Any

from .inbox import list_entries, unread_summary
from .progress import project_progress
from .workspace import _load, _target, project_status

LOG_BYTES = 32 * 1024
FINDING_PAGE_SIZE = 50
MAX_FINDING_PAGE_SIZE = 100
_FINDING_SORTS = ("severity", "check", "target_url", "id")
_CHECKLIST_SORTS = ("id", "priority", "state", "updated")
_CHECKLIST_STATES = frozenset(
    {
        "excluded",
        "not_agreed",
        "stale",
        "running",
        "unavailable",
        "blocked",
        "completed",
        "review",
        "deliverable",
        "remaining",
    }
)
_SEVERITY_RANK = {"critical": 0, "error": 1, "warning": 2, "notice": 3, "info": 4}
_EVIDENCE_CACHE_LIMIT = 32
_EVIDENCE_CACHE: OrderedDict[tuple[Any, ...], dict[str, Any]] = OrderedDict()
_EVIDENCE_CACHE_LOCK = RLock()
_FINDINGS_CACHE_LIMIT = 8
_FINDINGS_CACHE_MAX_BYTES = 8 * 1024 * 1024
_FINDINGS_CACHE: OrderedDict[tuple[Any, ...], dict[str, Any]] = OrderedDict()
_FINDINGS_CACHE_LOCK = RLock()
_REVISION = re.compile(r"[0-9a-f]{64}\Z")
_VOLATILE_REVISION_KEYS = {"observed_at", "age_seconds", "current_rate_per_second"}


def _log(root: Path) -> dict[str, Any]:
    path = root / "log.md"
    size = path.stat().st_size
    with path.open("rb") as stream:
        stream.seek(max(0, size - LOG_BYTES))
        text = stream.read(LOG_BYTES).decode("utf-8", errors="replace")
    return {"text": text, "truncated": size > LOG_BYTES, "bytes": size}


def _stable_projection(value: Any) -> Any:
    if isinstance(value, dict):
        return {
            key: _stable_projection(item)
            for key, item in value.items()
            if key not in _VOLATILE_REVISION_KEYS
        }
    if isinstance(value, list):
        return [_stable_projection(item) for item in value]
    return value


def _revisioned(
    payload: dict[str, Any], since_revision: str | None
) -> dict[str, Any]:
    if since_revision is not None and (
        not isinstance(since_revision, str) or not _REVISION.fullmatch(since_revision)
    ):
        raise ValueError("since_revision must be a 64-character lowercase SHA-256 token")
    stable = _stable_projection(payload)
    encoded = json.dumps(
        stable, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False
    ).encode("utf-8")
    revision = hashlib.sha256(encoded).hexdigest()
    if since_revision == revision:
        return {"ok": True, "revision": revision, "not_modified": True}
    return {**payload, "revision": revision, "not_modified": False}


def _stat_state(path: Path) -> tuple[int, int, int, int] | None:
    """Return enough identity to reject cache entries after an artifact replacement."""
    try:
        info = path.stat()
    except OSError:
        return None
    return info.st_dev, info.st_ino, info.st_mtime_ns, info.st_size


def _evidence_cache_key(row: dict[str, Any]) -> tuple[Any, ...]:
    """Key one evidence projection by artifact bytes and retained scan configuration.

    A running SQLite scan can commit through its WAL while the main file's
    timestamp is unchanged.  Include the WAL and SHM state in every key, even
    when either sidecar is currently absent, so a new checkpoint immediately
    invalidates the prior projection on the next observer refresh.
    """
    from seohead.storage.audit_v2 import audit_v2_path

    source = Path(row["path"])
    return (
        str(source.resolve()),
        _stat_state(source),
        _stat_state(source.with_name(source.name + "-wal")),
        _stat_state(source.with_name(source.name + "-shm")),
        _stat_state(audit_v2_path(source)),
        row.get("uuid"),
        row.get("lifecycle"),
        row.get("finish_reason"),
        row.get("crawl_partial"),
        row.get("corpus_partial"),
        row.get("evidence_revision"),
        row.get("config_fingerprint"),
        row.get("format_version"),
        row.get("source_kind"),
    )


def _lru_get(
    cache: OrderedDict[tuple[Any, ...], Any],
    lock: RLock,
    key: tuple[Any, ...],
    *,
    copy_value: bool,
) -> Any | None:
    """Return a cached value; copy on the way out when the cache owns mutable state."""
    with lock:
        value = cache.get(key)
        if value is None:
            return None
        cache.move_to_end(key)
        return copy.deepcopy(value) if copy_value else value


def _lru_put(
    cache: OrderedDict[tuple[Any, ...], Any],
    lock: RLock,
    limit: int,
    key: tuple[Any, ...],
    value: Any,
    *,
    copy_value: bool,
) -> None:
    """Store a value, evicting least recently used entries beyond the limit."""
    with lock:
        cache[key] = copy.deepcopy(value) if copy_value else value
        cache.move_to_end(key)
        while len(cache) > limit:
            cache.popitem(last=False)


def _read_retained_findings(row: dict[str, Any]) -> dict[str, Any]:
    """Read only findings, bounding their bytes before constructing a cache entry."""
    from seohead.storage import read_audit
    from seohead.storage.audit_v2 import AuditV2Reader, audit_v2_path

    if audit_v2_path(row["path"]).exists():
        with AuditV2Reader(row["path"], light_binding=True) as reader:
            size = reader.con.execute(
                "SELECT COALESCE(SUM(length(CAST(value_json AS BLOB))),0) FROM items WHERE pointer='/issues'"
            ).fetchone()[0]
            if size > _FINDINGS_CACHE_MAX_BYTES:
                raise ValueError(
                    "finding collection exceeds the in-memory cache; use bounded finding pages"
                )
            audit = reader.header
            issues = list(reader.iter_collection("/issues"))
    else:
        audit = read_audit(row["path"])
        issues = audit.get("issues")
    if not isinstance(issues, list) or any(not isinstance(item, dict) for item in issues):
        raise ValueError("retained scan audit has no readable finding list")
    run = audit.get("run")
    return {
        "issues": tuple(issues),
        "skipped_checks": copy.deepcopy(run.get("checks_skipped", []))
        if isinstance(run, dict)
        else [],
        "schema_version": audit.get("schema_version"),
        "generated_at": run.get("generated_at") if isinstance(run, dict) else None,
    }


def _retained_findings(row: dict[str, Any]) -> dict[str, Any]:
    """Reuse bounded finding rows while source, audit and retained state match."""
    key = _evidence_cache_key(row)
    cached = _lru_get(_FINDINGS_CACHE, _FINDINGS_CACHE_LOCK, key, copy_value=False)
    if cached is not None:
        return cached
    findings = _read_retained_findings(row)
    if _evidence_cache_key(row) != key:
        return findings
    try:
        size = len(json.dumps(findings, ensure_ascii=False, separators=(",", ":")).encode("utf-8"))
    except (TypeError, ValueError):
        return findings
    if size <= _FINDINGS_CACHE_MAX_BYTES:
        _lru_put(
            _FINDINGS_CACHE,
            _FINDINGS_CACHE_LOCK,
            _FINDINGS_CACHE_LIMIT,
            key,
            findings,
            copy_value=False,
        )
    return findings


def _read_scan_evidence(row: dict[str, Any]) -> dict[str, Any]:
    """Read collector evidence even while an audit is not yet available."""
    from seohead.storage import open_scan_mode
    from seohead.storage.status import scan_status

    path = row["path"]
    from seohead.storage.native_scan import NativeScan

    light_reader = getattr(NativeScan, "observe", None)
    if (
        row.get("source_kind") == "native"
        and row.get("lifecycle") == "running"
        and callable(light_reader)
    ):
        try:
            live = light_reader(path)
        except (OSError, ValueError) as exc:
            return {"state": "unavailable", "reason": str(exc)}
        return {
            "state": "available",
            "validation": live["validation"],
            "frontier": {
                "state": "available",
                "reason": "committed metadata snapshot",
                "counts": {
                    name: live["counts"][name]
                    for name in ("queued", "inflight", "done", "excluded")
                },
            },
            "committed_page_outcomes": None,
            "committed_pages": live["counts"]["pages"],
            "sitemaps": {
                "fetch_summaries": {},
                "state": "unavailable",
                "reason": "not in lightweight collection snapshot",
            },
            "findings": {
                "state": "unavailable",
                "reason": "collection still running; no current finalized audit projected",
                "total": None,
                "by_severity": {},
                "items": [],
                "truncated": False,
            },
            "skipped_checks": None,
        }
    try:
        status = scan_status(path)
        with closing(open_scan_mode(path, require_audit=False, light=True)[0]) as con:
            sitemap_rows = con.execute(
                "SELECT completeness,COUNT(*) FROM context_items "
                "WHERE kind='sitemap_fetch_summary' GROUP BY completeness"
            ).fetchall()
    except (OSError, ValueError, KeyError) as exc:
        return {"state": "unavailable", "reason": str(exc)}

    evidence = {
        "state": "available",
        "validation": status["validation"],
        "frontier": status["frontier"],
        "committed_page_outcomes": status["committed_page_outcomes"],
        "sitemaps": {"fetch_summaries": {key: value for key, value in sitemap_rows}},
    }
    try:
        from seohead.storage.audit_v2 import AuditV2Reader, audit_v2_path

        if audit_v2_path(path).exists():
            with AuditV2Reader(path, light_binding=True) as reader:
                total = reader.count("/issues")
                severity = {
                    key: count
                    for key, count in reader.con.execute(
                        "SELECT json_extract(value_json,'$.severity'),COUNT(*) FROM items "
                        "WHERE pointer='/issues' GROUP BY json_extract(value_json,'$.severity')"
                    )
                    if isinstance(key, str)
                }
                by_check = {
                    key: count
                    for key, count in reader.con.execute(
                        "SELECT json_extract(value_json,'$.check'),COUNT(*) FROM items "
                        "WHERE pointer='/issues' GROUP BY json_extract(value_json,'$.check')"
                    )
                    if isinstance(key, str)
                }
                first = reader.con.execute(
                    "SELECT value_json FROM items WHERE pointer='/issues' ORDER BY ordinal LIMIT 20"
                )
                first = [
                    _finding_summary(json.loads(item[0]), ordinal)
                    for ordinal, item in enumerate(first)
                ]
                skipped = reader.header.get("run", {}).get("checks_skipped", [])
                if "/run/checks_skipped" in reader.collections:
                    skipped = list(reader.iter_collection("/run/checks_skipped"))
                evidence.update(
                    findings={
                        "state": "available",
                        "reason": None,
                        "total": total,
                        "by_severity": severity,
                        "by_check": by_check,
                        "items": first,
                        "truncated": total > len(first),
                        "next_offset": len(first) if total > len(first) else None,
                    },
                    skipped_checks=skipped,
                )
                return evidence
        retained = _retained_findings(row)
    except (OSError, ValueError, KeyError) as exc:
        evidence.update(
            findings={
                "state": "unavailable",
                "reason": str(exc),
                "total": None,
                "by_severity": {},
                "by_check": {},
                "items": [],
                "truncated": False,
            },
            skipped_checks=None,
        )
        return evidence
    by_severity: dict[str, int] = {}
    by_check: dict[str, int] = {}
    finding_items = []
    for issue in retained["issues"]:
        severity = issue.get("severity") if isinstance(issue, dict) else None
        if isinstance(severity, str):
            by_severity[severity] = by_severity.get(severity, 0) + 1
        check = issue.get("check") if isinstance(issue, dict) else None
        if isinstance(check, str):
            by_check[check] = by_check.get(check, 0) + 1
        if isinstance(issue, dict) and len(finding_items) < 20:
            finding_items.append(
                {
                    key: issue.get(key)
                    for key in (
                        "id",
                        "check",
                        "severity",
                        "target_url",
                        "message",
                        "fingerprint",
                    )
                }
            )
    evidence.update(
        findings={
            "state": "available",
            "reason": None,
            "total": sum(by_severity.values()),
            "by_severity": by_severity,
            "by_check": by_check,
            "items": finding_items,
            "truncated": len(retained["issues"]) > len(finding_items),
            "next_offset": len(finding_items)
            if len(retained["issues"]) > len(finding_items)
            else None,
        },
        skipped_checks=retained["skipped_checks"],
    )
    return evidence


def _scan_evidence(row: dict[str, Any]) -> dict[str, Any]:
    """Read one retained projection once per unchanged scan/audit state.

    The terminal observer redraws each second.  Reopening and materialising a
    completed audit for every redraw made a passive second-screen view slower
    than the crawler it was meant to observe.  Cache only successful projections
    and verify source, WAL/SHM, audit companion and retained config state before
    reusing one.  Failures remain uncached so recovery is visible immediately.
    """
    key = _evidence_cache_key(row)
    cached = _lru_get(_EVIDENCE_CACHE, _EVIDENCE_CACHE_LOCK, key, copy_value=True)
    if cached is not None:
        return cached
    evidence = _read_scan_evidence(row)
    if evidence.get("state") != "available":
        return evidence
    # Do not retain an observation read while its source was being changed.
    # The following refresh will read the stable state instead.
    if _evidence_cache_key(row) != key:
        return evidence
    _lru_put(
        _EVIDENCE_CACHE,
        _EVIDENCE_CACHE_LOCK,
        _EVIDENCE_CACHE_LIMIT,
        key,
        evidence,
        copy_value=True,
    )
    return copy.deepcopy(evidence)


def _relative_artifact(root: Path, path: str) -> str | None:
    """Return a project-relative retained-artifact reference, never a host path."""
    try:
        return Path(path).resolve().relative_to(root.resolve()).as_posix()
    except (OSError, ValueError):
        return None


def _site_scans(root: Path, scans: dict[str, Any], *, limit: int) -> dict[str, Any]:
    """Bound one site's retained scans and keep their evidence tied to an artifact."""
    items = []
    for row in scans["items"][:limit]:
        artifact = _relative_artifact(root, row["path"])
        item = {key: value for key, value in row.items() if key != "path"}
        if artifact is None:
            item["artifact"] = {
                "state": "unavailable",
                "reason": "retained scan path is outside the project workspace",
            }
            item["evidence"] = item["artifact"]
        else:
            item["artifact"] = {"state": "available", "path": artifact}
            item["evidence"] = _scan_evidence(row)
        items.append(item)
    return {
        "total": scans["total"],
        "shown": len(items),
        "has_more": scans.get("has_more", False) or scans["total"] > len(items),
        "errors": scans["errors"],
        "items": items,
    }


def _method_coverage(checklist: dict[str, Any]) -> dict[str, Any]:
    """Summarise expected and recorded scenario/skill work without a false percentage."""
    state = checklist.get("state")
    if state != "initialized":
        return {
            "state": state or "unavailable",
            "reason": checklist.get("reason") or "method coverage is unavailable",
            "kinds": {},
        }
    kinds: dict[str, dict[str, Any]] = {}
    for kind in ("scenario", "skill"):
        rows = [
            item
            for item in checklist.get("items", [])
            if item.get("kind") == kind
            and item.get("enabled", True)
            and item.get("applicability") == "applicable"
        ]
        states: dict[str, int] = {}
        for row in rows:
            value = row.get("state")
            if isinstance(value, str):
                states[value] = states.get(value, 0) + 1
        kinds[kind] = {
            "expected": len(rows) if checklist.get("plan") is not None else None,
            "applicable": len(rows),
            "basis": "agreed applicable methods"
            if checklist.get("plan") is not None
            else "audit plan not recorded",
            "completed": sum(1 for row in rows if row.get("complete") is True),
            "states": states,
        }
    return {"state": "available", "reason": None, "kinds": kinds}


def _coverage_summary(checklist: dict[str, Any]) -> dict[str, Any]:
    """Expose a bounded coverage projection, preserving unknown and partial states."""
    axes = {}
    for name, axis in (checklist.get("coverage") or {}).items():
        if not isinstance(axis, dict):
            continue
        axes[name] = {
            key: axis.get(key)
            for key in (
                "state",
                "reason",
                "numerator",
                "denominator",
                "measured_urls",
                "unfinished",
                "unverified_measurements",
                "population_kind",
                "population_name",
            )
            if key in axis
        }
    return {
        "state": checklist.get("state") or "unavailable",
        "reason": checklist.get("reason"),
        "complete": checklist.get("complete"),
        "counts": checklist.get("counts"),
        "axes": axes,
    }


def _site_projection(
    root: Path,
    project: dict[str, Any],
    checklist: dict[str, Any],
    scans: dict[str, Any],
    *,
    role: str,
    source: str | None = None,
    observed_at: str | None = None,
    candidate_state: str | None = None,
    directory: str = ".",
    scan_limit: int,
    run_offset: int = 0,
    run_limit: int = 20,
) -> dict[str, Any]:
    """Build one read-only own-site or competitor observation row."""
    from .run_observation import status as run_status

    return {
        "role": role,
        "project_uuid": project["project_uuid"],
        "directory": directory,
        "site": project["site"],
        "candidate": {
            "state": candidate_state,
            "source": source,
            "observed_at": observed_at,
        }
        if role == "competitor"
        else None,
        "coverage": _coverage_summary(checklist),
        "methods": _method_coverage(checklist),
        "scans": _site_scans(root, scans, limit=scan_limit),
        "runs": run_status(root, offset=run_offset, limit=run_limit),
    }


def _competitor_sites(
    root: Path,
    preparation: dict[str, Any],
    *,
    scan_limit: int,
    coverage_views: dict | None = None,
    run_offset: int = 0,
    run_limit: int = 20,
) -> list[dict]:
    """Read declared competitor workspaces only; absent or damaged work stays explicit."""
    from seohead.storage.history import list_scans

    from .coverage import coverage_status

    sites = []
    for candidate in preparation.get("competitors", []):
        directory = candidate.get("directory")
        try:
            if not isinstance(directory, str):
                raise ValueError("competitor workspace reference is unavailable")
            child = root / directory
            child_root, child_project = _load(child)
            if child_project["project_uuid"] != candidate.get("project_uuid"):
                raise ValueError("competitor project identity mismatch")
            if child_project["site"]["target"] != _target(candidate.get("url")):
                raise ValueError("competitor site identity mismatch")
            sites.append(
                _site_projection(
                    child_root,
                    child_project,
                    (coverage_views or {}).get(str(child_root))
                    or coverage_status(child_root, _verify_evidence=False),
                    list_scans(child_root / "scans"),
                    role="competitor",
                    source=candidate.get("source"),
                    observed_at=candidate.get("observed_at"),
                    candidate_state=candidate.get("state"),
                    directory=directory,
                    scan_limit=scan_limit,
                    run_offset=run_offset,
                    run_limit=run_limit,
                )
            )
        except (OSError, ValueError) as exc:
            sites.append(
                {
                    "role": "competitor",
                    "project_uuid": candidate.get("project_uuid"),
                    "directory": directory,
                    "site": {"target": candidate.get("url"), "host": None, "label": None},
                    "candidate": {
                        "state": candidate.get("state"),
                        "source": candidate.get("source"),
                        "observed_at": candidate.get("observed_at"),
                    },
                    "coverage": {
                        "state": "unavailable",
                        "reason": str(exc),
                        "complete": None,
                        "counts": None,
                        "axes": {},
                    },
                    "methods": {
                        "state": "unavailable",
                        "reason": str(exc),
                        "kinds": {},
                    },
                    "scans": {
                        "total": None,
                        "shown": 0,
                        "has_more": False,
                        "errors": [],
                        "items": [],
                    },
                }
            )
    return sites


def _scan_row(directory: str | Path, scan_uuid: str | None) -> dict[str, Any]:
    """Resolve one retained scan through the project history, never a caller path."""
    from seohead.storage.history import _catalog

    root, _project = _load(directory)
    rows, _errors = _catalog(root / "scans")
    if not rows:
        raise ValueError("project has no retained scans")
    if scan_uuid is None:
        return rows[0]
    row = next((item for item in rows if item["uuid"] == scan_uuid), None)
    if row is None:
        raise ValueError("retained scan is not part of this project")
    return row


def _finding_summary(issue: dict[str, Any], ordinal: int) -> dict[str, Any]:
    """Keep a browse page small; full original evidence stays in the retained artifact."""
    if not isinstance(issue, dict):
        raise ValueError("retained finding is not an object")
    result = {
        "ordinal": ordinal,
        "id": issue.get("id"),
        "check": issue.get("check"),
        "severity": issue.get("severity"),
        "target_url": issue.get("target_url") or issue.get("url"),
        "message": issue.get("message") or issue.get("text"),
        "fingerprint": issue.get("fingerprint"),
    }
    truncated = list(issue.get("truncated_fields", []))
    for key, value in result.items():
        if isinstance(value, str) and len(value) > 4096:
            result[key] = value[:4096] + "…"
            truncated.append(key)
    if truncated:
        result["truncated_fields"] = sorted(set(truncated))
    return result


def _bounded_value(value: Any, *, depth: int = 0) -> Any:
    """Expose retained finding evidence without turning a terminal page into a dump."""
    if depth >= 3:
        return "[nested evidence omitted]"
    if value is None or isinstance(value, bool | int | float):
        return value
    if isinstance(value, str):
        return value[:4096] + ("…" if len(value) > 4096 else "")
    if isinstance(value, list):
        return [_bounded_value(item, depth=depth + 1) for item in value[:20]]
    if isinstance(value, dict):
        return {
            str(key)[:128]: _bounded_value(item, depth=depth + 1)
            for key, item in list(value.items())[:40]
        }
    return str(value)[:1024]


def _projection_truncated(value: Any, *, depth: int = 0) -> bool:
    """Name every omission made by the bounded detail projection."""
    if depth >= 3:
        return True
    if isinstance(value, str):
        return len(value) > 4096
    if isinstance(value, list):
        return len(value) > 20 or any(
            _projection_truncated(item, depth=depth + 1) for item in value
        )
    if isinstance(value, dict):
        return len(value) > 40 or any(
            len(str(key)) > 128 or _projection_truncated(item, depth=depth + 1)
            for key, item in value.items()
        )
    return False


def _v2_findings_page(row, *, offset, limit, query, sort, descending):
    """Let SQLite bound sorting/paging; never materialize unrelated audit collections."""
    from seohead.storage.audit_v2 import AuditV2Reader

    fields = ("id", "check", "severity", "target_url", "url", "message", "text")
    with AuditV2Reader(row["path"]) as reader:
        reader.con.create_function("casefold", 1, lambda value: str(value or "").casefold())
        expression = " || ' ' || ".join(
            f"COALESCE(json_extract(value_json,'$.{field}'),'')" for field in fields
        )
        predicate = "pointer='/issues' AND instr(casefold(" + expression + "),?)>0"
        needle = query.casefold().strip()
        matched = reader.con.execute(
            "SELECT COUNT(*) FROM items WHERE " + predicate, (needle,)
        ).fetchone()[0]
        value = f"json_extract(value_json,'$.{sort}')"
        if sort == "target_url":
            value = "COALESCE(NULLIF(" + value + ",''),json_extract(value_json,'$.url'))"
        if sort == "severity":
            value = (
                "CASE casefold("
                + value
                + ") "
                + " ".join(f"WHEN '{name}' THEN {rank}" for name, rank in _SEVERITY_RANK.items())
                + " ELSE 99 END"
            )
            order = value
        else:
            order = f"({value} IS NULL OR {value}=''),casefold({value})"
        direction = " DESC" if descending else " ASC"
        order = (
            order.replace(",casefold", direction + ",casefold") + direction + ",ordinal" + direction
        )
        selected = reader.con.execute(
            "SELECT ordinal,value_json FROM items WHERE "
            + predicate
            + " ORDER BY "
            + order
            + " LIMIT ? OFFSET ?",
            (needle, limit, offset),
        )
        return (
            reader.count("/issues"),
            matched,
            [(item[0], _finding_summary(json.loads(item[1]), item[0])) for item in selected],
        )


def findings_page(
    directory: str | Path,
    *,
    scan_uuid: str | None = None,
    offset: int = 0,
    limit: int = FINDING_PAGE_SIZE,
    query: str = "",
    sort: str = "severity",
    descending: bool = False,
) -> dict[str, Any]:
    """Browse a retained scan's findings with bounded filter, sort and pagination.

    The scan is selected from the project's retained history, so this helper is
    deliberately unable to read an arbitrary SQLite path.  It is read-only and
    keeps an original ordinal that :func:`finding_detail` can use after sorting.
    """
    if type(offset) is not int or offset < 0:
        raise ValueError("finding offset must be non-negative")
    if type(limit) is not int or not 1 <= limit <= MAX_FINDING_PAGE_SIZE:
        raise ValueError("finding limit must be from 1 to 100")
    if type(query) is not str or len(query) > 256:
        raise ValueError("finding query must be text of at most 256 characters")
    if sort not in _FINDING_SORTS:
        raise ValueError("finding sort must be severity, check, target_url or id")
    row = _scan_row(directory, scan_uuid)
    from seohead.storage.audit_v2 import audit_v2_path

    if audit_v2_path(row["path"]).exists():
        source_count, matched_count, selected = _v2_findings_page(
            row, offset=offset, limit=limit, query=query, sort=sort, descending=descending
        )
        return _finding_page_result(
            row, query, sort, descending, source_count, matched_count, selected, offset, limit
        )
    issues = _retained_findings(row)["issues"]
    needle = query.casefold().strip()
    indexed = list(enumerate(issues))
    if needle:
        indexed = [
            (ordinal, issue)
            for ordinal, issue in indexed
            if needle
            in " ".join(
                str(issue.get(key, ""))
                for key in ("id", "check", "severity", "target_url", "url", "message", "text")
            ).casefold()
        ]

    def sort_key(item: tuple[int, dict[str, Any]]) -> tuple[bool, Any, int]:
        ordinal, issue = item
        value = issue.get(sort)
        if sort == "target_url":
            value = value or issue.get("url")
        if sort == "severity":
            rank = _SEVERITY_RANK.get(str(value).casefold())
            return rank is None, rank if rank is not None else 99, ordinal
        return value in (None, ""), str(value or "").casefold(), ordinal

    indexed.sort(key=sort_key, reverse=descending)
    selected = indexed[offset : offset + limit]
    return _finding_page_result(
        row, query, sort, descending, len(issues), len(indexed), selected, offset, limit
    )


def _finding_page_result(
    row, query, sort, descending, source_count, matched_count, selected, offset, limit
):
    return {
        "ok": True,
        "scan": {
            key: row.get(key)
            for key in ("uuid", "source_kind", "lifecycle", "finish_reason", "path")
        },
        "query": query,
        "sort": {"field": sort, "direction": "desc" if descending else "asc"},
        "counts": {"source": source_count, "matched": matched_count, "returned": len(selected)},
        "pagination": {
            "offset": offset,
            "limit": limit,
            "next_offset": offset + len(selected)
            if offset + len(selected) < matched_count
            else None,
            "previous_offset": max(0, offset - limit) if offset else None,
            "truncated": offset + len(selected) < matched_count,
        },
        "items": [_finding_summary(issue, ordinal) for ordinal, issue in selected],
    }


def finding_detail(
    directory: str | Path, *, scan_uuid: str | None = None, ordinal: int
) -> dict[str, Any]:
    """Return one retained finding and its bounded source evidence projection."""
    if type(ordinal) is not int or ordinal < 0:
        raise ValueError("finding ordinal must be non-negative")
    row = _scan_row(directory, scan_uuid)
    from seohead.storage.audit_v2 import AuditV2Reader, audit_v2_path

    if audit_v2_path(row["path"]).exists():
        with AuditV2Reader(row["path"]) as reader:
            saved = reader.con.execute(
                "SELECT value_json FROM items WHERE pointer='/issues' AND ordinal=?", (ordinal,)
            ).fetchone()
            if saved is None:
                raise ValueError("retained finding ordinal is unavailable")
            issue = json.loads(saved[0])
            retained = {
                "schema_version": reader.header.get("schema_version"),
                "generated_at": reader.header.get("run", {}).get("generated_at"),
            }
    else:
        retained = _retained_findings(row)
        issues = retained["issues"]
        if ordinal >= len(issues):
            raise ValueError("retained finding ordinal is unavailable")
        issue = issues[ordinal]
    return {
        "ok": True,
        "scan_uuid": row["uuid"],
        "finding": _finding_summary(issue, ordinal),
        "evidence": _bounded_value(issue),
        "projection_limits": {
            "string_chars": 4096,
            "list_items": 20,
            "object_fields": 40,
            "max_depth": 3,
            "source_path": row["path"],
        },
        "evidence_truncated": _projection_truncated(issue),
        "audit": {
            "schema_version": retained["schema_version"],
            "generated_at": retained["generated_at"],
        },
    }


def saved_view_page(
    directory: str | Path, *, name: str, scan_uuid: str | None = None, offset: int = 0
) -> dict[str, Any]:
    """Apply a saved local finding view to an in-project retained scan only."""
    row = _scan_row(directory, scan_uuid)
    from seohead.projects.finding_views import apply_view_to_audit
    from seohead.storage import read_audit

    result = apply_view_to_audit(directory, name, read_audit(row["path"]), offset=offset)
    return {"ok": True, "scan_uuid": row["uuid"], "result": result}


def observe(
    directory: str,
    *,
    consumer: str | None = None,
    scan_limit: int = 20,
    run_offset: int = 0,
    run_limit: int = 20,
    since_revision: str | None = None,
) -> dict[str, Any]:
    """Return a bounded observer snapshot without changing project evidence."""
    if since_revision is not None and (
        not isinstance(since_revision, str) or not _REVISION.fullmatch(since_revision)
    ):
        raise ValueError("since_revision must be a 64-character lowercase SHA-256 token")
    if type(scan_limit) is not int or not 1 <= scan_limit <= 100:
        raise ValueError("scan_limit must be from 1 to 100")
    _pagination(0, run_offset, run_limit)
    root, project = _load(directory)
    coverage_views: dict = {}
    status = project_status(root, _coverage_views=coverage_views, _verify_evidence=False)
    progress = project_progress(root, limit=100, _status=status, _coverage_views=coverage_views)
    from .execution import status as execution_status
    from .monitoring import status as monitor_status
    from .runtime import project_policy

    execution = execution_status(root, _coverage=coverage_views[str(root)], _verify_evidence=False)
    monitor = monitor_status(root)
    preparation = status["preparation"]
    sites = [
        _site_projection(
            root,
            project,
            coverage_views[str(root)],
            status["scans"],
            role="primary",
            scan_limit=scan_limit,
            run_offset=run_offset,
            run_limit=run_limit,
        )
    ]
    sites.extend(
        _competitor_sites(
            root,
            preparation,
            scan_limit=scan_limit,
            coverage_views=coverage_views,
            run_offset=run_offset,
            run_limit=run_limit,
        )
    )
    site_identities = {
        item["project_uuid"]: {"role": item["role"], "target": item["site"]["target"]}
        for item in sites
        if isinstance(item.get("project_uuid"), str)
    }
    checks = status["checklist"].get("items", [])
    methods = [
        {
            "id": item["id"],
            "item_id": item.get("item_id", item["id"]),
            "title": item["title"],
            "kind": item["kind"],
            "state": item["state"],
            "attempt_status": item["attempt_status"],
            "stale": item["stale"],
            "reason": item["reason"],
            "site": site_identities.get(
                item.get("project_uuid", project["project_uuid"]),
                {
                    "role": "unknown",
                    "target": item.get("scope", {}).get("site"),
                },
            ),
        }
        for item in checks
        if item["kind"] in {"scenario", "skill"}
    ]
    scans = status["scans"]
    scan_rows = [{**row, "evidence": _scan_evidence(row)} for row in scans["items"][:scan_limit]]
    snapshot: dict[str, Any] = {
        "ok": True,
        "project": status["project"],
        "progress": progress,
        "execution": execution,
        "monitor": monitor,
        "runs": sites[0]["runs"],
        "observed_at": datetime.now(timezone.utc).isoformat(),
        "policy": project_policy(str(root)),
        "sites": {
            "total": len(sites),
            "scan_limit_per_site": scan_limit,
            "items": sites,
        },
        "methods": methods,
        "preparation": {
            "state": preparation["state"],
            "reason": preparation.get("reason"),
            "steps": preparation.get("steps", {}),
            "competitors": preparation.get("competitors", []),
        },
        "scans": {
            "total": scans["total"],
            "shown": len(scan_rows),
            "errors": scans["errors"],
            "items": scan_rows,
        },
        "log": _log(root),
        # Goals and handoff prompts are inbox entries.  Listing is read-only;
        # the observer must never acknowledge an agent's work by looking at it.
        "inbox": list_entries(root, consumer=consumer or "observer/local", limit=100),
    }
    from .finding_views import list_views

    coverage = coverage_views[str(root)]
    snapshot["review"] = {
        "state": coverage.get("state"),
        "manual_waiting": (coverage.get("views") or {}).get("waiting_for_manual_review", []),
        "deliverable_ready": (coverage.get("views") or {}).get("deliverable_ready", []),
        "plan": coverage.get("plan"),
    }
    snapshot["saved_views"] = list_views(root)
    from .progress import _item_state

    active = [
        dict(item, display_state=_item_state(item))
        for item in checks
        if item.get("attempt_status") == "running"
        or (
            item.get("kind") == "custom"
            and item.get("applicability") == "applicable"
            and not item.get("complete")
        )
    ]
    active.sort(key=lambda item: item.get("attempt_status") != "running")
    snapshot["active_tasks"] = {
        "items": active[:100],
        "total": len(active),
        "has_more": len(active) > 100,
    }
    total_inbox = snapshot["inbox"]["pagination"]["total"]
    snapshot["latest_inbox"] = list_entries(
        root, consumer=consumer or "observer/local", limit=100, offset=max(0, total_inbox - 100)
    )
    if consumer is not None:
        snapshot["inbox_unread"] = unread_summary(root, consumer=consumer)
    return _revisioned(snapshot, since_revision)


def _pagination(total: int, offset: int, limit: int) -> dict:
    if type(offset) is not int or offset < 0 or type(limit) is not int or not 1 <= limit <= 100:
        raise ValueError("offset must be nonnegative and limit must be 1..100")
    return {
        "total": total,
        "offset": offset,
        "limit": limit,
        "next_offset": offset + limit if offset + limit < total else None,
        "previous_offset": max(0, offset - limit) if offset else None,
    }


def checklist_page(
    directory: str | Path,
    *,
    offset: int = 0,
    limit: int = 50,
    query: str = "",
    kind: str | None = None,
    state: str | None = None,
    sort: str = "id",
    descending: bool = False,
    states: list[str] | None = None,
) -> dict:
    """Filter before paging; keep rich checklist evidence and source-specific identities.

    ``sort="id"`` keeps the stored item order (the default); the other fields sort by value
    with item id as the tie-break. ``updated`` is the newest saved record time of the item.
    """
    from .progress import _item_state

    _pagination(0, offset, limit)
    if not isinstance(query, str) or len(query) > 256:
        raise ValueError("checklist query must be text of at most 256 characters")
    if kind not in {None, "method", "schema", "check", "skill", "scenario", "custom"}:
        raise ValueError("unsupported checklist kind")
    if sort not in _CHECKLIST_SORTS:
        raise ValueError("checklist sort must be id, priority, state or updated")
    if type(descending) is not bool:
        raise ValueError("checklist descending must be a boolean")
    if states is not None and (
        not isinstance(states, list)
        or len(states) > 8
        or any(value not in _CHECKLIST_STATES for value in states)
    ):
        raise ValueError("checklist states must be up to 8 known display states")
    from .coverage import _read

    root, project = _load(directory)
    saved_items = (_read(root, project) or {}).get("items", {})
    snapshot = project_status(directory, _verify_evidence=False)
    checklist = snapshot["checklist"]
    rows = []
    needle = query.casefold().strip()
    for item in checklist.get("items", []):
        text = " ".join(str(item.get(key) or "") for key in ("id", "title", "reason")).casefold()
        if kind == "method" and item.get("kind") not in {"scenario", "skill"}:
            continue
        if kind == "schema" and not any(
            word in text for word in ("schema", "structured data", "json-ld")
        ):
            continue
        if kind not in {None, "method", "schema"} and item.get("kind") != kind:
            continue
        display = _item_state(item)
        if needle not in text or (state is not None and state != display):
            continue
        if states is not None and display not in states:
            continue
        saved = saved_items.get(item.get("id"), {})
        stamps = [
            str(record.get("recorded_at") or "")
            for record in saved.get("records", [])
            if isinstance(record, dict)
        ]
        rows.append(dict(item, display_state=display, updated=max(stamps, default="") or None))
    if sort == "id":
        if descending:
            rows.reverse()
    else:
        field = {"priority": "priority", "state": "display_state", "updated": "updated"}[sort]
        rows.sort(
            key=lambda row: (str(row.get(field) or ""), str(row.get("id") or "")),
            reverse=descending,
        )
    return {
        "ok": True,
        "revision": checklist.get("revision"),
        "sort": {"field": sort, "direction": "desc" if descending else "asc"},
        "items": rows[offset : offset + limit],
        "pagination": _pagination(len(rows), offset, limit),
    }


def task_detail(directory: str | Path, *, item_id: str) -> dict:
    """Resolve the declared site then expose a bounded task/history/catalogue detail."""
    from .catalogue import load_catalogue
    from .coverage import _read, coverage_status
    from .progress import _item_state
    from .runtime import resolve_item_scope

    scoped, local_id = resolve_item_scope(str(directory), item_id, _verify_evidence=False)
    root, project = _load(scoped)
    view = coverage_status(root, _verify_evidence=False)
    row = next((item for item in view.get("items", []) if item["id"] == local_id), None)
    if row is None:
        raise ValueError("checklist item is unavailable")
    document = _read(root, project)
    saved = (document or {}).get("items", {}).get(local_id, {})
    records = saved.get("records", [])
    item = dict(
        row,
        id=item_id,
        item_id=local_id,
        display_state=_item_state(row),
        definition=saved.get("definition"),
        history=records[-20:],
        history_total=len(records),
        history_truncated=len(records) > 20,
        catalogue=load_catalogue().get(local_id),
    )
    return {"ok": True, "revision": view.get("revision"), "item": item}


checklist_detail = task_detail


def scans_page(directory: str | Path, *, offset: int = 0, limit: int = 20) -> dict:
    """Browse every retained scan without recomputing checklist coverage."""
    from seohead.storage.history import list_scans

    _pagination(0, offset, limit)
    root, _project = _load(directory)
    page = list_scans(root / "scans", offset=offset, limit=limit)
    return {
        "ok": True,
        "total": page["total"],
        "errors": page["errors"],
        "items": [dict(row, evidence=_scan_evidence(row)) for row in page["items"]],
        "pagination": _pagination(page["total"], offset, limit),
    }


def observe_activity(
    directory: str | Path, *, since_revision: str | None = None
) -> dict:
    """Cheap collector telemetry; never open retained scans or compute task coverage."""
    if since_revision is not None and (
        not isinstance(since_revision, str) or not _REVISION.fullmatch(since_revision)
    ):
        raise ValueError("since_revision must be a 64-character lowercase SHA-256 token")
    from .run_observation import status as run_status
    from .runtime import preparation_status

    root, project = _load(directory)
    sites = [
        {
            "project_uuid": project["project_uuid"],
            "site": project["site"],
            "role": "primary",
            "directory": ".",
            "runs": run_status(root),
        }
    ]
    for candidate in preparation_status(str(root)).get("competitors", []):
        relative = candidate.get("directory")
        try:
            if (
                not isinstance(relative, str)
                or not relative.startswith("competitors/")
                or ".." in Path(relative).parts
            ):
                raise ValueError("unsafe competitor project reference")
            child = root / relative
            if child.is_symlink() or not child.resolve().is_relative_to(root):
                raise ValueError("unsafe competitor project reference")
            child_root, metadata = _load(child)
            if metadata["project_uuid"] != candidate.get("project_uuid") or metadata["site"][
                "target"
            ] != _target(candidate.get("url")):
                raise ValueError("competitor project identity mismatch")
            sites.append(
                {
                    "project_uuid": metadata["project_uuid"],
                    "site": metadata["site"],
                    "role": "competitor",
                    "directory": relative,
                    "runs": run_status(child_root),
                }
            )
        except (OSError, ValueError) as exc:
            sites.append(
                {
                    "project_uuid": candidate.get("project_uuid"),
                    "site": {"target": candidate.get("url")},
                    "role": "competitor",
                    "directory": relative,
                    "runs": {"state": "unavailable", "reason": str(exc), "items": []},
                }
            )
    return _revisioned({
        "ok": True,
        "observed_at": datetime.now(timezone.utc).isoformat(),
        "sites": {"total": len(sites), "items": sites},
    }, since_revision)
