"""Disabled-by-default local monitoring over immutable retained observations.

This module stores bounded caller-imported observations and provides one explicit
one-shot HTTP collection path. It never starts a timer, daemon, background
scheduler, or delivery transport. That keeps schedule activation outside a
project file while still making restarts, overlap refusal, budgets and
full-refresh policy observable and testable.
"""

from __future__ import annotations

import dataclasses
import hashlib
import json
import os
import re
import time
import uuid
from collections.abc import Callable
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any

from seohead.core.common import utc_iso_z as _now
from seohead.core.filesystem import atomic_write_bytes
from seohead.crawl.user_agents import DEFAULT_USER_AGENT

from .runtime import read_document, write_document
from .workspace import _load as _workspace_load

FORMAT = "seohead.monitor.v1"
NAME = "monitor.json"
_SEVERITIES = ("notice", "warning", "critical")
_QUALIFIERS = ("fresh", "revalidated", "stale", "unavailable", "partial", "failed")
_CACHE_STATES = {
    "fresh": {"fresh"},
    "revalidated": {"revalidated"},
    "stale": {"cached"},
    "unavailable": {"unavailable"},
    "partial": {"unavailable"},
    "failed": {"unavailable"},
}
_TRACKED_FIELDS = ("status", "indexability", "canonical", "robots", "metadata", "content", "links")
_DEFAULTS = {
    "interval_seconds": 3600,
    "max_render_requests": 0,
    "suppression_runs": 1,
    "severity_threshold": "warning",
}
_DEFAULT_LEASE_SECONDS = 300


def _expiry(stamp: str, seconds: int) -> str:
    parsed = datetime.fromisoformat(stamp.replace("Z", "+00:00"))
    return (parsed + timedelta(seconds=seconds)).isoformat().replace("+00:00", "Z")


def _load(directory: str | Path) -> tuple[Path, dict[str, Any]]:
    root, project = _workspace_load(directory)
    document = read_document(root, NAME) or {
        "format": FORMAT,
        "revision": 0,
        "project_uuid": project["project_uuid"],
        "policy": None,
        "runs": [],
        "runner": {"state": "idle", "runs_since_full_refresh": 0, "next_url_offset": 0},
    }
    if document.get("format") != FORMAT or document.get("project_uuid") != project["project_uuid"]:
        raise ValueError("monitor belongs to another project or format")
    if not isinstance(document.get("runs"), list) or len(document["runs"]) > 10_000:
        raise ValueError("monitor has invalid retained run history")
    document.setdefault("runner", {"state": "idle", "runs_since_full_refresh": 0})
    document["runner"].setdefault("next_url_offset", 0)
    return root, document


def _policy(value: Any) -> dict[str, Any]:
    required = {"enabled", "urls", "max_urls", "max_requests", "full_refresh_every"}
    if not isinstance(value, dict) or not required <= set(value):
        raise ValueError("monitor policy must declare bounded incremental scope")
    unknown = set(value) - (required | set(_DEFAULTS))
    if unknown:
        raise ValueError("monitor policy has unsupported fields")
    policy = {**_DEFAULTS, **value}
    if type(policy["enabled"]) is not bool:
        raise ValueError("monitor policy enabled must be boolean")
    urls = policy["urls"]
    if (
        not isinstance(urls, list)
        or not urls
        or len(urls) > 500
        or len(set(urls)) != len(urls)
        or any(
            not isinstance(url, str) or not url.startswith(("http://", "https://")) for url in urls
        )
    ):
        raise ValueError("monitor URLs must be a unique bounded absolute HTTP(S) list")
    for key in (
        "max_urls",
        "max_requests",
        "full_refresh_every",
        "interval_seconds",
        "suppression_runs",
    ):
        if type(policy[key]) is not int or policy[key] < 1:
            raise ValueError(f"monitor {key} must be a positive integer")
    if type(policy["max_render_requests"]) is not int or policy["max_render_requests"] < 0:
        raise ValueError("monitor max_render_requests must be a nonnegative integer")
    if policy["max_urls"] > len(urls) or policy["max_requests"] < len(urls):
        raise ValueError("monitor request and URL budgets are inconsistent")
    if policy["severity_threshold"] not in _SEVERITIES:
        raise ValueError("monitor severity_threshold must be notice, warning or critical")
    return policy


def _due(document: dict[str, Any]) -> dict[str, Any]:
    policy = document.get("policy")
    if policy is None:
        return {"state": "unconfigured", "reason": "configure a bounded policy first"}
    runner = document["runner"]
    if not policy["enabled"]:
        return {"state": "disabled", "reason": "monitoring is disabled by policy"}
    if runner.get("state") == "running":
        return {"state": "claimed", "reason": "a local runner already owns the next bounded pass"}
    full = (
        not document["runs"]
        or runner.get("runs_since_full_refresh", 0) >= policy["full_refresh_every"]
    )
    urls = policy["urls"]
    offset = runner.get("next_url_offset", 0) % len(urls)
    planned_urls = (
        urls if full else [urls[(offset + item) % len(urls)] for item in range(policy["max_urls"])]
    )
    return {
        "state": "due",
        "mode": "full" if full else "incremental",
        "url_limit": len(planned_urls),
        "planned_urls": planned_urls,
        "request_budget": policy["max_requests"],
        "render_request_budget": policy["max_render_requests"],
        "interval_seconds": policy["interval_seconds"],
        "next_due_at": runner.get("next_due_at"),
        "reason": "periodic full coverage refresh" if full else "bounded incremental refresh",
    }


def _planned_due(document: dict[str, Any]) -> dict[str, Any]:
    """Keep a claimed full pass full even when the runner is already active."""
    runner = document["runner"]
    plan = runner.get("plan")
    if runner.get("state") == "running" and isinstance(plan, dict) and plan.get("state") == "due":
        return plan
    return _due(document)


def configure(
    directory: str | Path, policy: dict[str, Any], expected_revision: int = 0
) -> dict[str, Any]:
    root, document = _load(directory)
    if document["revision"] != expected_revision:
        raise ValueError("monitor revision conflict")
    if document["runner"].get("state") == "running":
        raise ValueError("cancel the active local runner before changing its policy")
    document["policy"] = _policy(policy)
    document["revision"] += 1
    write_document(root, NAME, document, expected_revision=expected_revision)
    return {
        "ok": True,
        "revision": document["revision"],
        "policy": document["policy"],
        "state": "configured_not_scheduled",
        "due": _due(document),
    }


def _change_key(change: dict[str, Any]) -> str:
    return json.dumps(change, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def _threshold(policy: dict[str, Any], change: dict[str, Any]) -> bool:
    return _SEVERITIES.index(change.get("severity", "notice")) >= _SEVERITIES.index(
        policy["severity_threshold"]
    )


def _source(value: Any, root: Path) -> dict[str, Any] | None:
    if value is None:
        return None
    if not isinstance(value, dict) or set(value) != {
        "body_ref",
        "body_sha256",
        "validation_ref",
        "validation_sha256",
        "links_sha256",
    }:
        raise ValueError("monitor source provenance is invalid")
    for ref_key, hash_key in (("body_ref", "body_sha256"), ("validation_ref", "validation_sha256")):
        reference, digest = value[ref_key], value[hash_key]
        if reference is None and digest is None and ref_key == "body_ref":
            continue
        if (
            not isinstance(reference, str)
            or not reference.startswith("reports/monitor/")
            or ".." in Path(reference).parts
            or not isinstance(digest, str)
            or not re.fullmatch(r"[0-9a-f]{64}", digest)
        ):
            raise ValueError("monitor source provenance is invalid")
        from .evidence import _digest, artifact_path
        from .runtime import project_policy

        path = artifact_path(root, reference)
        budget = project_policy(str(root))["policy"]["evidence_hash"]
        actual = _digest(
            path,
            max_bytes=budget["max_bytes"],
            deadline=time.monotonic() + budget["max_seconds"],
        )
        if actual != digest:
            raise ValueError("monitor source provenance does not match retained bytes")
    if not isinstance(value["links_sha256"], str) or not re.fullmatch(
        r"[0-9a-f]{64}", value["links_sha256"]
    ):
        raise ValueError("monitor source provenance is invalid")
    return value


def _observation(item: Any, policy: dict[str, Any], root: Path) -> dict[str, Any]:
    if not isinstance(item, dict) or set(item) - {
        "url",
        "changes",
        "qualifier",
        "measurement",
        "request_count",
        "render_request_count",
        "cache_state",
        "failure_reason",
        "source",
    }:
        raise ValueError("invalid scoped monitor observation")
    if item.get("url") not in policy["urls"] or not isinstance(item.get("changes"), list):
        raise ValueError("invalid scoped monitor observation")
    qualifier = item.get("qualifier", "fresh")
    if qualifier not in _QUALIFIERS:
        raise ValueError("monitor observation qualifier is invalid")
    cache_state = item.get("cache_state")
    if cache_state is None:
        cache_state = next(iter(_CACHE_STATES[qualifier]))
    if cache_state not in _CACHE_STATES[qualifier]:
        raise ValueError("monitor observation qualifier and cache_state are inconsistent")
    request_count = item.get("request_count", 1)
    render_request_count = item.get("render_request_count", 0)
    if type(request_count) is not int or request_count < 0:
        raise ValueError("monitor observation request_count is invalid")
    if type(render_request_count) is not int or render_request_count < 0:
        raise ValueError("monitor observation render_request_count is invalid")
    failure_reason = item.get("failure_reason")
    if failure_reason is not None and (
        not isinstance(failure_reason, str)
        or not failure_reason.strip()
        or len(failure_reason) > 512
    ):
        raise ValueError("monitor observation failure_reason is invalid")
    if qualifier == "failed" and failure_reason is None:
        raise ValueError("failed monitor observation requires failure_reason")
    measurement = item.get("measurement")
    if measurement is not None and (
        not isinstance(measurement, dict)
        or not set(measurement) <= set(_TRACKED_FIELDS)
        or len(measurement) > len(_TRACKED_FIELDS)
        or len(json.dumps(measurement, sort_keys=True, ensure_ascii=False).encode()) > 64 * 1024
    ):
        raise ValueError("monitor observation measurement is invalid")
    changes: list[dict[str, Any]] = []
    for change in item["changes"]:
        if not isinstance(change, dict) or change.get("severity") not in _SEVERITIES:
            raise ValueError("monitor changes need a declared severity")
        changes.append(change)
    return {
        "url": item["url"],
        "changes": changes,
        "qualifier": qualifier,
        "measurement": measurement,
        "request_count": request_count,
        "render_request_count": render_request_count,
        "cache_state": cache_state,
        "failure_reason": failure_reason.strip() if isinstance(failure_reason, str) else None,
        "source": _source(item.get("source"), root),
        "measured_at": _now(),
    }


def _previous_measurement(document: dict[str, Any], url: str) -> tuple[dict[str, Any], str] | None:
    for earlier in reversed(document["runs"]):
        for observation in reversed(earlier.get("observations", [])):
            if (
                observation.get("url") == url
                and observation.get("qualifier") in {"fresh", "revalidated"}
                and isinstance(observation.get("measurement"), dict)
            ):
                return observation["measurement"], earlier["scan_id"]
    return None


def _active_alerts(document: dict[str, Any], url: str) -> dict[str, dict[str, Any]]:
    """Return unresolved per-field alerts and their original measured value."""
    active: dict[str, dict[str, Any]] = {}
    for earlier in document["runs"]:
        for alert in earlier.get("alerts", []):
            if alert.get("url") != url:
                continue
            for change in alert.get("changes", []):
                field = change.get("field")
                if not isinstance(field, str):
                    continue
                existing = active.get(field)
                active[field] = {
                    "change": change,
                    "original_before": (
                        existing["original_before"]
                        if existing is not None
                        else change.get("before")
                    ),
                }
        for recovery in earlier.get("recoveries", []):
            if recovery.get("url") == url and isinstance(recovery.get("change"), dict):
                field = recovery["change"].get("field")
                if isinstance(field, str):
                    active.pop(field, None)
    return active


def _measurement_changes(
    document: dict[str, Any], observation: dict[str, Any]
) -> list[dict[str, Any]]:
    """Compare only two measured values; omitted and unavailable fields stay unknown."""
    if observation["qualifier"] in {"stale", "unavailable", "partial", "failed"}:
        return []
    current = observation["measurement"]
    previous = _previous_measurement(document, observation["url"])
    if current is None or previous is None:
        return []
    baseline, baseline_scan_id = previous
    changes = []
    active = _active_alerts(document, observation["url"])
    for field in _TRACKED_FIELDS:
        if field not in current or field not in baseline or current[field] == baseline[field]:
            continue
        prior = active.get(field)
        if prior is not None and current[field] == prior["original_before"]:
            changes.append(
                {
                    "kind": "recovered",
                    "field": field,
                    "before": baseline[field],
                    "after": current[field],
                    "baseline_scan_id": baseline_scan_id,
                    "alert_baseline_scan_id": prior["change"].get("baseline_scan_id"),
                    "severity": prior["change"].get("severity", "warning"),
                }
            )
            continue
        changes.append(
            {
                "kind": f"{field}_changed",
                "field": field,
                "before": baseline[field],
                "after": current[field],
                "baseline_scan_id": baseline_scan_id,
                "severity": "warning"
                if field in {"status", "indexability", "canonical", "robots"}
                else "notice",
            }
        )
    return changes


def _recent_alerts(document: dict[str, Any], suppression_runs: int) -> set[str]:
    """Suppress repeats for a bounded number of completed monitor passes only."""
    completed = []
    for earlier in reversed(document["runs"]):
        coverage = earlier.get("coverage")
        complete = (
            coverage.get("complete")
            if isinstance(coverage, dict)
            else earlier.get("state") != "partial"
        )
        if complete and earlier.get("state") != "failed":
            completed.append(earlier)
        if len(completed) == suppression_runs:
            break
    return {
        _change_key(change)
        for earlier in completed
        for alert in earlier.get("alerts", [])
        for change in alert.get("changes", [])
    }


def _coverage(planned_urls: list[str], observations: list[dict[str, Any]]) -> dict[str, Any]:
    """Make missing and failed measurement evidence explicit instead of implicit deletion."""
    observed_urls = [item["url"] for item in observations]
    qualifier_counts = {qualifier: 0 for qualifier in _QUALIFIERS}
    for observation in observations:
        qualifier_counts[observation["qualifier"]] += 1
    unobserved = [url for url in planned_urls if url not in set(observed_urls)]
    failures = [
        {"url": item["url"], "reason": item["failure_reason"]}
        for item in observations
        if item["qualifier"] == "failed"
    ]
    complete = not unobserved and not any(
        qualifier_counts[qualifier] for qualifier in ("stale", "unavailable", "partial", "failed")
    )
    return {
        "planned_url_count": len(planned_urls),
        "observed_url_count": len(observations),
        "unobserved_url_count": len(unobserved),
        "unobserved_urls": unobserved,
        "fresh_count": qualifier_counts["fresh"],
        "revalidated_count": qualifier_counts["revalidated"],
        "stale_count": qualifier_counts["stale"],
        "unavailable_count": qualifier_counts["unavailable"],
        "partial_count": qualifier_counts["partial"],
        "failed_count": qualifier_counts["failed"],
        "failures": failures,
        "complete": complete,
    }


def run(
    directory: str | Path,
    scan_id: str,
    observations: list[dict[str, Any]],
    expected_revision: int,
) -> dict[str, Any]:
    """Retain an already-collected bounded diff; no fetch, timer or delivery occurs."""
    root, document = _load(directory)
    if document["revision"] != expected_revision or document["policy"] is None:
        raise ValueError("monitor revision conflict or missing policy")
    policy = document["policy"]
    if not isinstance(scan_id, str) or not scan_id.strip() or len(scan_id) > 512:
        raise ValueError("scan_id required")
    runner_state = document["runner"].get("state", "idle")
    if runner_state in {"cancelled", "backoff", "interrupted"}:
        raise ValueError("restart the monitor claim before retaining observations")
    due = _planned_due(document)
    planned_urls = list(due.get("planned_urls", policy["urls"]))
    limit = len(planned_urls) if due.get("state") == "due" else policy["max_urls"]
    if not isinstance(observations, list) or not observations or len(observations) > limit:
        raise ValueError("observations exceed configured incremental scope")
    normalized = [_observation(item, policy, root) for item in observations]
    for item in normalized:
        derived = _measurement_changes(document, item)
        seen = {_change_key(change) for change in item["changes"]}
        item["changes"].extend(change for change in derived if _change_key(change) not in seen)
    if len({item["url"] for item in normalized}) != len(normalized):
        raise ValueError("monitor observations may contain each URL only once")
    if sum(item["request_count"] for item in normalized) > policy["max_requests"]:
        raise ValueError("monitor observations exceed configured request budget")
    if sum(item["render_request_count"] for item in normalized) > policy["max_render_requests"]:
        raise ValueError("monitor observations exceed configured render request budget")
    observed_urls = {item["url"] for item in normalized}
    if due.get("state") == "due" and not observed_urls <= set(planned_urls):
        raise ValueError("monitor observations are outside the planned bounded scope")
    if (
        due.get("state") == "due"
        and due.get("mode") == "full"
        and observed_urls != set(planned_urls)
    ):
        raise ValueError(
            "periodic full refresh must retain an observation for every configured URL"
        )
    coverage = _coverage(planned_urls, normalized)
    previous = _recent_alerts(document, policy["suppression_runs"])
    alerts: list[dict[str, Any]] = []
    recoveries: list[dict[str, Any]] = []
    for item in normalized:
        if item["qualifier"] in {"stale", "unavailable", "partial", "failed"}:
            continue
        meaningful = [
            change
            for change in item["changes"]
            if _threshold(policy, change) and change.get("kind") != "recovered"
        ]
        fresh = [change for change in meaningful if _change_key(change) not in previous]
        if fresh:
            alerts.append({"url": item["url"], "changes": fresh, "state": "actionable"})
        recoveries.extend(
            {"url": item["url"], "change": change, "state": "recovery"}
            for change in item["changes"]
            if change.get("kind") == "recovered" and _threshold(policy, change)
        )
    state = (
        "failed"
        if coverage["failed_count"] and not coverage["fresh_count"] + coverage["revalidated_count"]
        else "partial"
        if not coverage["complete"]
        else "actionable"
        if alerts or recoveries
        else "quiet"
    )
    retained = {
        "scan_id": scan_id.strip(),
        "recorded_at": _now(),
        "mode": due.get("mode", "incremental"),
        "planned_urls": planned_urls,
        "observations": normalized,
        "observed_urls": [item["url"] for item in normalized],
        "coverage": coverage,
        "alerts": alerts,
        "recoveries": recoveries,
        "baseline": {"scan_id": scan_id.strip(), "observations": normalized},
        "state": state,
        "notification": "none",
    }
    document["runs"].append(retained)
    completed_full = due.get("mode") == "full" and coverage["complete"]
    completed_incremental = due.get("mode") == "incremental" and coverage["complete"]
    offset = document["runner"].get("next_url_offset", 0)
    if completed_full:
        offset = 0
    elif completed_incremental:
        offset = (offset + len(planned_urls)) % len(policy["urls"])
    document["runner"] = {
        "state": "idle",
        "runs_since_full_refresh": (
            0
            if completed_full
            else policy["full_refresh_every"]
            if due.get("mode") == "full"
            else document["runner"].get("runs_since_full_refresh", 0)
            + (1 if completed_incremental else 0)
        ),
        "next_url_offset": offset,
        "last_finished_at": retained["recorded_at"],
        "next_due_at": _expiry(_now(), policy["interval_seconds"]),
    }
    document["revision"] += 1
    write_document(root, NAME, document, expected_revision=expected_revision)
    return {
        "ok": True,
        "revision": document["revision"],
        "run": retained,
        "coverage": coverage,
        "notification": "none",
        "due": _due(document),
    }


def schedule(
    directory: str | Path,
    *,
    action: str,
    expected_revision: int,
    lease_seconds: int = _DEFAULT_LEASE_SECONDS,
) -> dict[str, Any]:
    """Claim, cancel, back off or recover a local runner without starting a timer."""
    root, document = _load(directory)
    if document["revision"] != expected_revision or document["policy"] is None:
        raise ValueError("monitor revision conflict or missing policy")
    if action not in {"start", "cancel", "backoff", "recover"}:
        raise ValueError("schedule action must be start, cancel, backoff or recover")
    if type(lease_seconds) is not int or not 1 <= lease_seconds <= 3600:
        raise ValueError("monitor lease_seconds must be an integer from 1 to 3600")
    current = document["runner"].get("state", "idle")
    if action == "start":
        if not document["policy"]["enabled"]:
            raise ValueError("monitor policy is disabled; explicit enablement is required")
        if current == "running":
            raise ValueError("monitor runner already active; overlapping schedules are refused")
        claimed_at = _now()
        runner = {
            **document["runner"],
            "state": "running",
            "claimed_at": claimed_at,
            "claim_id": f"monitor:{uuid.uuid4()}",
            "lease_expires_at": _expiry(claimed_at, lease_seconds),
            "plan": _due(document),
            "dispatched_urls": [],
        }
    elif action == "recover":
        if current != "running":
            raise ValueError("only an interrupted running claim can be recovered")
        runner = {**document["runner"], "state": "interrupted", "interrupted_at": _now()}
    else:
        runner = {
            **document["runner"],
            "state": "cancelled" if action == "cancel" else "backoff",
            "changed_at": _now(),
        }
    document["runner"] = runner
    document["revision"] += 1
    write_document(root, NAME, document, expected_revision=expected_revision)
    return {
        "ok": True,
        "revision": document["revision"],
        "runner": runner,
        "scheduled": False,
        "due": _due(document),
    }


def _artifact(root: Path, relative: str, payload: bytes) -> tuple[str, str]:
    path = root / relative
    if (
        path.is_symlink()
        or not path.parent.is_dir()
        or not path.parent.resolve().is_relative_to(root.resolve())
    ):
        raise ValueError("monitor artifact path is unsafe")
    atomic_write_bytes(path, payload)
    from .evidence import _digest

    digest = _digest(path, max_bytes=len(payload), deadline=time.monotonic() + 5)
    if digest != hashlib.sha256(payload).hexdigest():
        raise ValueError("monitor artifact hash verification failed")
    return relative, digest


def _reports_root(root: Path) -> Path:
    reports = root / "reports"
    if (
        reports.is_symlink()
        or not reports.is_dir()
        or not reports.resolve().is_relative_to(root.resolve())
    ):
        raise ValueError("project reports directory is unsafe")
    return reports


def _new_report_target(reports: Path, name: str) -> Path:
    target = reports / name
    if os.path.lexists(target):
        expected = target.is_dir() if name.endswith("-cache") else target.is_file()
        if target.is_symlink() or not expected:
            raise ValueError("monitor report target is unsafe")
    if not target.parent.resolve().is_relative_to(reports.resolve()):
        raise ValueError("monitor report target is unsafe")
    return target


def _source_artifacts(
    root: Path,
    claim_id: str,
    record: Any,
    parsed: dict[str, Any] | None,
    transport: dict[str, Any],
) -> dict[str, Any]:
    """Retain body and post-cache validation evidence under this project only."""
    token = hashlib.sha256(record.url.encode("utf-8")).hexdigest()
    reports_root = _reports_root(root)
    monitor_root = reports_root / "monitor"
    if monitor_root.exists() and (monitor_root.is_symlink() or not monitor_root.is_dir()):
        raise ValueError("monitor artifact directory is unsafe")
    monitor_root.mkdir(mode=0o700, exist_ok=True)
    directory = monitor_root / claim_id.removeprefix("monitor:")
    if directory.is_symlink() or not directory.resolve().is_relative_to(root.resolve()):
        raise ValueError("monitor artifact directory is unsafe")
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    body = parsed.get("_raw_html") if isinstance(parsed, dict) else None
    links = parsed.get("links") if isinstance(parsed, dict) else []
    link_digest = hashlib.sha256(
        json.dumps(
            links if isinstance(links, list) else [], sort_keys=True, ensure_ascii=False
        ).encode("utf-8")
    ).hexdigest()
    body_ref = body_hash = None
    from seohead.crawl.cache import parse_cache_control

    retain_body = (
        isinstance(body, str)
        and transport.get("status_code") in {200, 304}
        and "no-store" not in parse_cache_control(transport.get("cache_control") or "")
        and not transport.get("set_cookie")
    )
    if retain_body:
        body_ref, body_hash = _artifact(
            root,
            str(directory.relative_to(root) / f"{token}.body.html"),
            body.encode("utf-8"),
        )
    validation = {
        "url": record.url,
        "effective_status_code": record.status_code,
        "cache_status": record.cache_status,
        "transport": transport,
        "validators": {
            "etag": transport.get("etag"),
            "last_modified": transport.get("last_modified"),
        },
        "body_ref": body_ref,
        "body_sha256": body_hash,
        "links_sha256": link_digest,
        "record": dataclasses.asdict(record),
    }
    validation_ref, validation_hash = _artifact(
        root,
        str(directory.relative_to(root) / f"{token}.validation.json"),
        (json.dumps(validation, ensure_ascii=False, sort_keys=True) + "\n").encode("utf-8"),
    )
    return {
        "body_ref": body_ref,
        "body_sha256": body_hash,
        "validation_ref": validation_ref,
        "validation_sha256": validation_hash,
        "links_sha256": link_digest,
    }


def _measurement(record: Any, source: dict[str, Any]) -> tuple[dict[str, Any], str, str | None]:
    if record.error or record.status_code is None:
        return {}, "failed", record.error or "collector produced no HTTP status"
    if record.cache_status == "hit":
        return (
            {"status": record.status_code},
            "stale",
            "fresh cache evidence was reused without a network revalidation",
        )
    if record.body_unavailable or source["body_sha256"] is None:
        return (
            {"status": record.status_code},
            "unavailable",
            record.body_unavailable or "body unavailable",
        )
    from seohead.checks.parser import robots_directives

    directives = robots_directives(record.meta_robots, record.x_robots)
    return (
        {
            "status": record.status_code,
            "indexability": "noindex" if "noindex" in directives else "indexable",
            "canonical": record.canonical,
            "robots": ",".join(sorted(directives)),
            "metadata": {"title": record.title, "description": record.meta_description},
            "content": source["body_sha256"],
            "links": {
                "internal": max(record.outlinks - record.external_outlinks, 0),
                "sha256": source["links_sha256"],
            },
        },
        "revalidated" if record.cache_status == "revalidated" else "fresh",
        None,
    )


def _claim_preview(document: dict[str, Any], stamp: str) -> dict[str, Any]:
    runner = document["runner"]
    plan = runner.get("plan")
    if runner.get("state") != "running" or not isinstance(plan, dict):
        raise ValueError("an explicit active monitor claim is required")
    expiry = runner.get("lease_expires_at")
    if not isinstance(expiry, str):
        raise ValueError("monitor claim lacks a lease")
    claim_id = runner.get("claim_id")
    try:
        valid_claim = f"monitor:{uuid.UUID(str(claim_id).removeprefix('monitor:'))}" == claim_id
    except ValueError:
        valid_claim = False
    if not valid_claim:
        raise ValueError("monitor claim id is invalid")
    policy = _policy(document.get("policy"))
    planned = plan.get("planned_urls")
    dispatched = runner.get("dispatched_urls", [])
    if (
        plan.get("state") != "due"
        or plan.get("mode") not in {"full", "incremental"}
        or not isinstance(planned, list)
        or not planned
        or len(planned) != len(set(planned))
        or not set(planned) <= set(policy["urls"])
        or plan.get("url_limit") != len(planned)
        or plan.get("request_budget") != policy["max_requests"]
        or plan.get("render_request_budget") != policy["max_render_requests"]
        or (plan["mode"] == "full" and planned != policy["urls"])
        or (plan["mode"] == "incremental" and len(planned) > policy["max_urls"])
        or not isinstance(dispatched, list)
        or len(dispatched) != len(set(dispatched))
        or not set(dispatched) <= set(planned)
    ):
        raise ValueError("monitor claim plan is invalid")
    expired = datetime.fromisoformat(stamp.replace("Z", "+00:00")) >= datetime.fromisoformat(
        expiry.replace("Z", "+00:00")
    )
    return {
        "claim_id": claim_id,
        "lease_expires_at": expiry,
        "expired": expired,
        "plan": plan,
        "dispatched_urls": list(dispatched),
        "remaining_urls": [url for url in planned if url not in dispatched],
    }


def collect_once(
    directory: str | Path,
    *,
    expected_revision: int,
    apply: bool = False,
    now: Callable[[], str] | None = None,
) -> dict[str, Any]:
    """Preview or execute one claimed bounded HTTP/cache collection pass.

    The policy interval remains scheduling advice. This performs no work unless
    an operator has separately created a current claim and sets ``apply=True``.
    """
    if type(apply) is not bool:
        raise ValueError("monitor apply must be a boolean")
    root, document = _load(directory)
    if document["revision"] != expected_revision:
        raise ValueError("monitor revision conflict")
    stamp = (now or _now)()
    preview = _claim_preview(document, stamp)
    if not apply:
        return {"ok": True, "applied": False, "revision": document["revision"], "preview": preview}
    if preview["expired"]:
        document["runner"] = {
            **document["runner"],
            "state": "interrupted",
            "interrupted_at": stamp,
            "reason": "monitor claim lease expired before collection",
        }
        document["revision"] += 1
        write_document(root, NAME, document, expected_revision=expected_revision)
        raise ValueError("monitor claim lease expired; explicitly start a new claim")
    if preview["dispatched_urls"]:
        raise ValueError(
            "monitor claim was interrupted after dispatch; explicitly start a new claim"
        )
    if not preview["remaining_urls"]:
        raise ValueError("monitor claim has no undispatched URLs; explicitly start a new claim")

    from seohead.crawl.cache import ResponseCache
    from seohead.crawl.collect import fetch_one
    from seohead.crawl.throttle import DispatchGate, Throttle
    from seohead.recon.net import crawl_transport_options, http_client

    reports = _reports_root(root)
    cache = ResponseCache(_new_report_target(reports, "monitor-cache"))
    throttle = Throttle(start_delay=0.5, min_delay=0.5, max_concurrency=1)
    gate = DispatchGate(throttle, time.sleep, max_requests=document["policy"]["max_requests"])
    transport: dict[str, Any] = {}

    def response_facts(response: Any) -> None:
        headers = {str(key).lower(): str(value) for key, value in response.headers.items()}
        transport.update(
            {
                "status_code": response.status_code,
                "cache_control": headers.get("cache-control"),
                "etag": headers.get("etag"),
                "last_modified": headers.get("last-modified"),
                "set_cookie": bool(headers.get("set-cookie")),
            }
        )

    client, _ = http_client(
        15,
        follow_redirects=False,
        event_hooks={"response": [response_facts]},
        **crawl_transport_options(),
    )
    observations = []
    try:
        for url in preview["remaining_urls"]:
            # Write before dispatch: a crashed process leaves unknown work instead of replaying it.
            root, document = _load(directory)
            if document["revision"] != expected_revision:
                raise ValueError("monitor revision conflict")
            current = _claim_preview(document, (now or _now)())
            if current["claim_id"] != preview["claim_id"]:
                raise ValueError("monitor claim changed before collection")
            if current["expired"]:
                document["runner"] = {
                    **document["runner"],
                    "state": "interrupted",
                    "interrupted_at": (now or _now)(),
                    "reason": "monitor claim lease expired during collection",
                }
                document["revision"] += 1
                write_document(root, NAME, document, expected_revision=expected_revision)
                raise ValueError("monitor claim lease expired; explicitly start a new claim")
            runner = document["runner"]
            runner["dispatched_urls"] = [*runner.get("dispatched_urls", []), url]
            document["revision"] += 1
            write_document(root, NAME, document, expected_revision=expected_revision)
            expected_revision = document["revision"]
            transport.clear()
            client.cookies.clear()
            requests_before = gate.requests_used
            record, parsed = fetch_one(
                url,
                client=client,
                cache=cache,
                throttle=throttle,
                wait=gate.wait_turn,
                user_agent=DEFAULT_USER_AGENT,
            )
            source = _source_artifacts(root, preview["claim_id"], record, parsed, transport)
            client.cookies.clear()
            measurement, qualifier, failure_reason = _measurement(record, source)
            observations.append(
                {
                    "url": url,
                    "changes": [],
                    "qualifier": qualifier,
                    "measurement": measurement or None,
                    "cache_state": "revalidated"
                    if qualifier == "revalidated"
                    else "fresh"
                    if qualifier == "fresh"
                    else "cached"
                    if qualifier == "stale"
                    else "unavailable",
                    "request_count": gate.requests_used - requests_before,
                    "render_request_count": 0,
                    "failure_reason": failure_reason,
                    "source": source,
                }
            )
    finally:
        client.close()
    root, document = _load(directory)
    if document["revision"] != expected_revision:
        raise ValueError("monitor revision conflict")
    current = _claim_preview(document, (now or _now)())
    if current["claim_id"] != preview["claim_id"]:
        raise ValueError("monitor claim changed before completion")
    if current["expired"]:
        document["runner"] = {
            **document["runner"],
            "state": "interrupted",
            "interrupted_at": (now or _now)(),
            "reason": "monitor claim lease expired before completion",
        }
        document["revision"] += 1
        write_document(root, NAME, document, expected_revision=expected_revision)
        raise ValueError("monitor claim lease expired; explicitly start a new claim")
    retained = run(directory, str(preview["claim_id"]), observations, expected_revision)
    return {"ok": True, "applied": True, "preview": preview, **retained}


def local_deliver(directory: str | Path, *, scan_id: str, expected_revision: int) -> dict[str, Any]:
    """Record one authorized local receipt without a network destination."""
    root, project = _workspace_load(directory)
    from seohead.projects.service_delivery import DeliveryReceipts, MonitorServiceDelivery

    reports = _reports_root(root)
    receipt_path = reports / "monitor-delivery-receipts.sqlite"
    if (
        not receipt_path.parent.resolve().is_relative_to(reports.resolve())
        or os.path.lexists(receipt_path)
        and (receipt_path.is_symlink() or not receipt_path.is_file())
    ):
        raise ValueError("monitor delivery receipt store is unsafe")
    service = MonitorServiceDelivery(
        project_uuid=project["project_uuid"],
        allowed_destinations={"local:receipt"},
        receipts=DeliveryReceipts(receipt_path),
        send=lambda _destination, _payload, _receipt: None,
        enabled=True,
    )
    return deliver(
        directory,
        scan_id=scan_id,
        destination="local:receipt",
        service=service,
        expected_revision=expected_revision,
    )


def deliver(
    directory: str | Path,
    *,
    scan_id: str,
    destination: str,
    service: Any,
    expected_revision: int,
) -> dict[str, Any]:
    """Explicitly hand one retained complete run to an injected authorized service.

    This function cannot construct a destination, load credentials, or activate a
    schedule.  ``service`` is a caller-owned :class:`MonitorServiceDelivery`.
    """
    root, document = _load(directory)
    if document["revision"] != expected_revision:
        raise ValueError("monitor revision conflict")
    if getattr(service, "project_uuid", None) != document["project_uuid"]:
        raise PermissionError("monitor service is not authorized for this project")
    run = next(
        (item for item in reversed(document["runs"]) if item.get("scan_id") == scan_id), None
    )
    if run is None:
        raise ValueError("retained monitor scan_id is unknown")
    result = service.deliver(run, destination)
    # Evidence stays immutable; this small receipt projection only records that
    # a caller-owned authorized destination accepted the already-retained event.
    newly_sent = [receipt for receipt in result["receipts"] if receipt["state"] == "sent"]
    if newly_sent:
        run.setdefault("delivery", []).extend(
            {"destination": destination, **receipt} for receipt in newly_sent
        )
        document["revision"] += 1
        write_document(root, NAME, document, expected_revision=expected_revision)
    return {"ok": True, "revision": document["revision"], **result}


def status(directory: str | Path) -> dict[str, Any]:
    _, document = _load(directory)
    return {
        "ok": True,
        "revision": document["revision"],
        "policy": document["policy"],
        "runner": document["runner"],
        "last_run": document["runs"][-1] if document["runs"] else None,
        "due": _due(document),
    }
