"""Manual project-source bindings: which provider resource IDs a project uses.

Bindings are local ID records in ``sources.json`` beside ``project.json``. They carry
no credentials, no service-account paths, and no provider responses, and nothing here
touches the network. ``project.json`` is never rewritten, so existing projects keep
opening unchanged.
"""

from __future__ import annotations

import json
import os
import re
from datetime import date, datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from seohead.core.filesystem import fsync_directory
from seohead.projects.workspace import _load

SOURCES_FILE = "sources.json"
SOURCES_FORMAT = "seohead.project.sources.v1"
SOURCES_VERSION = 1
SERVICES = ("gsc", "ga4", "gtm", "metrika", "webmaster", "bing", "topvisor", "cloudflare")
MAX_BINDINGS = 200
_MAX_BYTES = 262_144
_MAX_RESOURCE = 512
_MAX_LABEL = 128
_CONTROL = re.compile(r"[\x00-\x1f\x7f]")
_BINDING_KEYS = {"service", "resource", "label", "origin", "linked_at"}
UTC = timezone.utc
_SOURCE_SERVICES = {
    "gsc": "gsc",
    "ga4": "ga4",
    "metrika": "metrika",
    "webmaster": "webmaster",
    "webmaster_history": "webmaster",
}
_NO_ACCESS_STATES = {"no_access", "invalid_grant", "insufficient_scope"}


def _resource(value: Any) -> str:
    if (
        type(value) is not str
        or not value
        or len(value) > _MAX_RESOURCE
        or _CONTROL.search(value)
        or value != value.strip()
    ):
        raise ValueError("resource must be a bounded, trimmed ID without control characters")
    return value


def _label(value: Any) -> str | None:
    if value is None:
        return None
    if type(value) is not str or not value.strip() or len(value) > _MAX_LABEL:
        raise ValueError("label must be a bounded human label")
    if _CONTROL.search(value):
        raise ValueError("label must not contain control characters")
    return value


def _service(value: Any) -> str:
    if value not in SERVICES:
        raise ValueError(f"unknown service; expected one of: {', '.join(SERVICES)}")
    return value


def _read(root: Path) -> list[dict[str, Any]]:
    path = root / SOURCES_FILE
    if not os.path.lexists(path):
        return []
    if path.is_symlink() or not path.is_file() or path.stat().st_size > _MAX_BYTES:
        raise ValueError("sources.json is unsafe or exceeds its byte limit")
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise ValueError("sources.json is not valid JSON") from exc
    if (
        not isinstance(document, dict)
        or set(document) != {"format", "version", "bindings"}
        or document["format"] != SOURCES_FORMAT
        or document["version"] != SOURCES_VERSION
        or not isinstance(document["bindings"], list)
        or len(document["bindings"]) > MAX_BINDINGS
    ):
        raise ValueError("sources.json has an unsupported shape")
    for item in document["bindings"]:
        if not isinstance(item, dict) or set(item) != _BINDING_KEYS:
            raise ValueError("sources.json binding has an unsupported shape")
        _service(item["service"])
        _resource(item["resource"])
        _label(item["label"])
    return document["bindings"]


def _write(root: Path, bindings: list[dict[str, Any]]) -> None:
    target = root / SOURCES_FILE
    if target.is_symlink():
        raise ValueError("sources.json must not be a symlink")
    text = json.dumps(
        {"format": SOURCES_FORMAT, "version": SOURCES_VERSION, "bindings": bindings},
        ensure_ascii=False,
        indent=2,
        sort_keys=True,
    )
    staged = root / ".sources.json.stage"
    staged.unlink(missing_ok=True)
    descriptor = os.open(staged, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            stream.write(text + "\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(staged, target)
        fsync_directory(root)
    finally:
        staged.unlink(missing_ok=True)


def _view(binding: dict[str, Any]) -> dict[str, Any]:
    return {**binding, "status": "selected"}


def _source_readiness(entry: dict[str, Any], as_of: date) -> dict[str, Any]:
    from seohead.data_sources.sources_db import SOURCES

    source = entry["source"]
    spec = SOURCES[source]
    lag = spec.lag_days
    end = as_of - timedelta(days=lag)
    start = end - timedelta(days=27)
    expected = {(start + timedelta(days=offset)).isoformat() for offset in range(28)}
    coverage = entry.get("coverage", {})
    data_days = set(coverage.get("complete", [])) & expected
    covered_days = data_days | (set(coverage.get("empty", [])) & expected)
    last_attempts = entry.get("last_attempts", {})
    access_failures = {
        day: state
        for day, state in last_attempts.items()
        if day in expected and state in _NO_ACCESS_STATES
    }
    state = (
        "no_access"
        if access_failures
        else "data_28d"
        if len(covered_days) == 28 and data_days
        else "empty_28d"
        if len(covered_days) == 28
        else "partial_28d"
        if data_days
        else "no_recent_data"
    )
    return {
        "state": state,
        "data_28d": bool(data_days),
        "coverage_28d": len(covered_days) == 28,
        "data_days": len(data_days),
        "available_days": len(covered_days),
        "expected_days": 28,
        "window": {"start": start.isoformat(), "end": end.isoformat()},
        "access_state": "no_access" if access_failures else "unknown",
        "access_failure_days": sorted(access_failures),
        "sources": [source],
    }


def _history_resources(root: Path, as_of: date) -> tuple[list[dict[str, Any]], bool]:
    path = root / "sources.sqlite"
    if not os.path.lexists(path):
        output: list[dict[str, Any]] = []
    else:
        if path.is_symlink() or not path.is_file():
            raise ValueError("sources.sqlite is unsafe")
        from seohead.data_sources.sources_db import status as sources_status

        history = sources_status(path).get("sources", [])
        grouped: dict[tuple[str, str], list[dict[str, Any]]] = {}
        for entry in history:
            service = _SOURCE_SERVICES.get(entry.get("source"))
            if service:
                grouped.setdefault((service, entry["resource"]), []).append(entry)
        output = []
        for (service, resource), entries in grouped.items():
            summaries = [_source_readiness(entry, as_of) for entry in entries]
            failures = [item for item in summaries if item["state"] == "no_access"]
            data_sources = [item for item in summaries if item["data_28d"]]
            best = (
                failures[0]
                if failures
                else data_sources[0]
                if data_sources
                else max(summaries, key=lambda item: item["available_days"])
            )
            output.append(
                {
                    "service": service,
                    "resource": resource,
                    "history_state": best["state"],
                    "data_28d": best["data_28d"],
                    "coverage_28d": best["coverage_28d"],
                    "data_days": best["data_days"],
                    "available_days": best["available_days"],
                    "expected_days": best["expected_days"],
                    "window": best["window"],
                    "access_state": best["access_state"],
                    "sources": sorted(item["sources"][0] for item in summaries),
                }
            )
    cloudflare_has_more = False
    cloudflare_path = root / "cloudflare.sqlite"
    if os.path.lexists(cloudflare_path):
        if cloudflare_path.is_symlink() or not cloudflare_path.is_file():
            raise ValueError("cloudflare.sqlite is unsafe")
        from seohead.data_sources.cloudflare_history import list_zones

        local = list_zones(project=str(root), limit=100, offset=0)
        zones = local.get("zones", []) if isinstance(local, dict) else []
        cloudflare_has_more = bool(local.get("total", 0) > len(zones))
        for row in zones:
            if isinstance(row, dict) and isinstance(row.get("zone_id"), str):
                output.append(_cloudflare_history_state(row, as_of))
    return output, cloudflare_has_more


def _cloudflare_history_state(row: dict[str, Any], as_of: date) -> dict[str, Any]:
    observed = row.get("latest_observed_at")
    try:
        timestamp = datetime.fromisoformat(str(observed).replace("Z", "+00:00"))
        observed_day = timestamp.astimezone(UTC).date()
    except (TypeError, ValueError):
        observed_day = None
    period = row.get("latest_period") or {}
    start = period.get("from") if isinstance(period, dict) else None
    end = period.get("to") if isinstance(period, dict) else None
    try:
        period_start = date.fromisoformat(str(start)[:10])
        period_end = date.fromisoformat(str(end)[:10])
    except ValueError:
        period_start = period_end = None
    recent = observed_day is not None and as_of - timedelta(days=27) <= observed_day <= as_of
    complete_period = (
        period_start is not None
        and period_end is not None
        and period_start <= as_of - timedelta(days=27)
        and period_end >= as_of - timedelta(days=1)
    )
    sampled = bool(row.get("sampled"))
    truncated = bool(row.get("truncated"))
    complete = row.get("state") == "complete" and complete_period and not sampled and not truncated
    state = (
        "data_28d"
        if recent and complete
        else "partial_28d"
        if recent and row.get("state") in {"complete", "partial"}
        else "stale"
        if observed_day is not None
        else "no_history"
    )
    result = {
        "service": "cloudflare",
        "resource": row["zone_id"],
        "history_state": state,
        "data_28d": recent,
        "coverage_28d": recent and complete,
        "data_days": None,
        "available_days": None,
        "expected_days": 28,
        "window": {"start": start, "end": end},
        "access_state": "unknown",
        "sources": ["cloudflare"],
    }
    if row.get("zone"):
        result["label"] = row["zone"]
    return result


def list_bindings(directory: str | Path, *, as_of: str | date | None = None) -> dict[str, Any]:
    """Read selected bindings and local 28-day source history without provider calls."""
    root, _ = _load(directory)
    if isinstance(as_of, str):
        try:
            as_of = date.fromisoformat(as_of)
        except ValueError as exc:
            raise ValueError("as_of must be an ISO calendar date") from exc
    if as_of is not None and not isinstance(as_of, date):
        raise ValueError("as_of must be an ISO calendar date")
    as_of = as_of or datetime.now(UTC).date()
    known, resources_truncated = _history_resources(root, as_of)
    by_pair = {(item["service"], item["resource"]): item for item in known}
    selected = [_view(item) for item in _read(root)]
    for item in selected:
        history = by_pair.get((item["service"], item["resource"]))
        item["readiness"] = (
            {key: value for key, value in history.items() if key not in {"service", "resource"}}
            if history
            else {
                "history_state": "no_history",
                "data_28d": None,
                "coverage_28d": None,
                "data_days": 0,
                "available_days": 0,
                "expected_days": 28,
                "access_state": "unknown",
                "reason": "no local sync history for this binding",
            }
        )
    selected_pairs = {(item["service"], item["resource"]) for item in selected}
    resources = [
        {
            **item,
            "status": "selected"
            if (item["service"], item["resource"]) in selected_pairs
            else "not_selected",
        }
        for item in known
    ]
    return {
        "ok": True,
        "path": str(root),
        "count": len(selected),
        "bindings": selected,
        "resources": resources,
        "resources_truncated": resources_truncated,
        "as_of": as_of.isoformat(),
    }


def link(
    directory: str | Path, service: str, resource: str, label: str | None = None
) -> dict[str, Any]:
    """Bind one resource ID to the project; linking the same pair again changes nothing."""
    root, _ = _load(directory)
    service, resource, label = _service(service), _resource(resource), _label(label)
    bindings = _read(root)
    for item in bindings:
        if item["service"] == service and item["resource"] == resource:
            return {"ok": True, "path": str(root), "created": False, "binding": _view(item)}
    if len(bindings) >= MAX_BINDINGS:
        raise ValueError(f"a project holds at most {MAX_BINDINGS} source bindings")
    binding = {
        "service": service,
        "resource": resource,
        "label": label,
        "origin": "manual",
        "linked_at": datetime.now(UTC).isoformat().replace("+00:00", "Z"),
    }
    bindings.append(binding)
    _write(root, bindings)
    return {"ok": True, "path": str(root), "created": True, "binding": _view(binding)}


def unlink(directory: str | Path, service: str, resource: str) -> dict[str, Any]:
    """Remove one binding; an unknown pair is a clear error, not a crash."""
    root, _ = _load(directory)
    service, resource = _service(service), _resource(resource)
    bindings = _read(root)
    kept = [i for i in bindings if not (i["service"] == service and i["resource"] == resource)]
    if len(kept) == len(bindings):
        raise ValueError("no such source binding in this project")
    _write(root, kept)
    return {"ok": True, "path": str(root), "removed": {"service": service, "resource": resource}}
