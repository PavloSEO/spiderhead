"""Explicit bounded one-shot scans for a task's structured competitor set."""

from __future__ import annotations

import asyncio
import contextlib
import hashlib
import json
import os
import threading
import time
import uuid
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable
from urllib.parse import urlsplit

from seohead.core.filesystem import atomic_write_bytes, lock_exclusive, open_lock, unlock

from .coverage import _read, coverage_status
from .runtime import _competitor_identity, read_document
from .workspace import _load

MAX_COMPETITORS = 20
MAX_CONCURRENCY = 3
MAX_SITE_URLS = 10_000
MAX_TOTAL_URLS = 50_000
MAX_SITE_SECONDS = 3600
MAX_TOTAL_SECONDS = 8 * 3600
MAX_SITE_REQUESTS = 30_000
MAX_TOTAL_REQUESTS = 60_000
MAX_RUN_MANIFESTS = 100
MAX_MANIFEST_BYTES = 2 * 1024 * 1024
RUN_FORMAT = "seohead.competitor-scan-run.v1"
RUN_DIRECTORY = Path("reports/.competitor-scan-runs")
RUN_LOCK = ".competitor-scan.lock"
_PLAN_FIELDS = (
    "url",
    "project_uuid",
    "directory",
    "max_urls",
    "max_requests",
    "max_crawl_seconds",
    "mode",
    "user_agent",
    "min_delay_seconds",
    "sitemap_url",
    "urls",
    "schedule_revision",
)
_SAVED_PLAN_FIELDS = set(_PLAN_FIELDS) - {"urls"}
_SITE_STATES = {"pending", "running", "interrupted", "completed", "partial", "failed", "cancelled"}
_LOCAL_RUN_LOCKS: dict[str, threading.Lock] = {}
_LOCAL_RUN_LOCKS_GUARD = threading.Lock()


class CompetitorScanError(ValueError):
    """A competitor scan run could not be validated or recovered safely."""


def _now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def _run_directory(root: Path, *, create: bool) -> Path:
    reports = root / "reports"
    if reports.is_symlink() or not reports.is_dir():
        raise CompetitorScanError("project reports directory is unavailable or unsafe")
    directory = root / RUN_DIRECTORY
    if directory.is_symlink():
        raise CompetitorScanError("competitor scan run directory is unsafe")
    if create:
        directory.mkdir(parents=True, mode=0o700, exist_ok=True)
    if directory.exists() and not directory.is_dir():
        raise CompetitorScanError("competitor scan run path is not a directory")
    return directory


def _manifest_path(root: Path, run_id: str, *, create: bool = False) -> Path:
    try:
        normalized = str(uuid.UUID(run_id))
    except (TypeError, ValueError) as exc:
        raise CompetitorScanError("run_id must be a UUID") from exc
    return _run_directory(root, create=create) / f"{normalized}.json"


def _read_manifest(root: Path, run_id: str, project_uuid: str) -> dict[str, Any]:
    path = _manifest_path(root, run_id)
    if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_MANIFEST_BYTES:
        raise CompetitorScanError("competitor scan run is unavailable")
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise CompetitorScanError("competitor scan run is invalid") from exc
    if (
        not isinstance(document, dict)
        or document.get("format") != RUN_FORMAT
        or document.get("run_id") != str(uuid.UUID(run_id))
        or document.get("project_uuid") != project_uuid
        or not isinstance(document.get("sites"), list)
        or len(document["sites"]) > MAX_COMPETITORS
        or not isinstance(document.get("task_id"), str)
        or type(document.get("coverage_revision")) is not int
        or document["coverage_revision"] < 0
        or not isinstance(document.get("fingerprint"), str)
        or len(document["fingerprint"]) != 64
        or any(char not in "0123456789abcdef" for char in document["fingerprint"])
        or document.get("state") not in {"running", "interrupted", "completed", "partial", "failed", "cancelled"}
        or document.get("runner_token") is not None
        and (
            not isinstance(document.get("runner_token"), str)
            or len(document["runner_token"]) != 32
            or any(char not in "0123456789abcdef" for char in document["runner_token"])
        )
        or type(document.get("concurrency")) is not int
        or not 1 <= document["concurrency"] <= MAX_CONCURRENCY
        or type(document.get("approve_large_crawl")) is not bool
        or not isinstance(document.get("budgets"), dict)
        or set(document["budgets"])
        != {"urls", "requests", "seconds", "max_urls", "max_requests", "max_seconds"}
        or any(type(value) is not int or value < 0 for value in document["budgets"].values())
        or document["budgets"]["max_urls"] != MAX_TOTAL_URLS
        or document["budgets"]["max_requests"] != MAX_TOTAL_REQUESTS
        or document["budgets"]["max_seconds"] != MAX_TOTAL_SECONDS
    ):
        raise CompetitorScanError("competitor scan run has an unsupported shape")
    totals = {"urls": 0, "requests": 0, "seconds": 0}
    for site in document["sites"]:
        if not isinstance(site, dict) or (_SAVED_PLAN_FIELDS | {"input_hash"}) - set(site):
            raise CompetitorScanError("competitor scan run has an invalid site plan")
        if (
            not isinstance(site["url"], str)
            or not isinstance(site["project_uuid"], str)
            or not isinstance(site["directory"], str)
            or Path(site["directory"]).is_absolute()
            or ".." in Path(site["directory"]).parts
            or not site["directory"].startswith("competitors/")
            or type(site["max_urls"]) is not int
            or not 1 <= site["max_urls"] <= MAX_SITE_URLS
            or type(site["max_requests"]) is not int
            or not 1 <= site["max_requests"] <= MAX_SITE_REQUESTS
            or type(site["max_crawl_seconds"]) is not int
            or not 1 <= site["max_crawl_seconds"] <= MAX_SITE_SECONDS
            or site["mode"] not in {"spider", "sitemap", "list"}
            or not isinstance(site["user_agent"], str)
            or not isinstance(site["input_hash"], str)
            or len(site["input_hash"]) != 64
            or any(char not in "0123456789abcdef" for char in site["input_hash"])
            or type(site["schedule_revision"]) is not int
            or site["schedule_revision"] < 0
            or site.get("state") not in _SITE_STATES
            or not isinstance(site.get("artifact"), str)
            or Path(site["artifact"]).is_absolute()
            or ".." in Path(site["artifact"]).parts
            or not site["artifact"].startswith("scans/")
        ):
            raise CompetitorScanError("competitor scan run has an invalid site record")
        totals["urls"] += site["max_urls"]
        totals["requests"] += site["max_requests"]
        totals["seconds"] += site["max_crawl_seconds"]
    if any(document["budgets"][key] != totals[key] for key in totals) or any(
        totals[key] > document["budgets"][f"max_{key}"]
        for key in totals
    ):
        raise CompetitorScanError("competitor scan run budgets disagree with its per-site plans")
    return document


def _write_manifest(root: Path, manifest: dict[str, Any]) -> None:
    path = _manifest_path(root, manifest["run_id"], create=True)
    payload = (
        json.dumps(manifest, ensure_ascii=False, sort_keys=True, indent=2, allow_nan=False) + "\n"
    ).encode("utf-8")
    if len(payload) > MAX_MANIFEST_BYTES:
        raise CompetitorScanError("competitor scan run exceeds its byte limit")
    from .runtime import project_write_lock

    with project_write_lock(root, timeout_seconds=5.0):
        atomic_write_bytes(path, payload, mode=0o600)


def _publish_start(root: Path, manifest: dict[str, Any], cancel_path: Path) -> None:
    from .runtime import project_write_lock

    with project_write_lock(root, timeout_seconds=5.0):
        if cancel_path.is_symlink():
            raise CompetitorScanError("competitor scan cancellation marker is unsafe")
        cancel_path.unlink(missing_ok=True)
        _write_manifest(root, manifest)


def _publish_final(root: Path, manifest: dict[str, Any], cancel_path: Path) -> None:
    from .runtime import project_write_lock

    with project_write_lock(root, timeout_seconds=5.0):
        _write_manifest(root, manifest)
        if cancel_path.is_symlink():
            raise CompetitorScanError("competitor scan cancellation marker is unsafe")
        cancel_path.unlink(missing_ok=True)


def _cancel_path(root: Path, run_id: str, *, create: bool = False) -> Path:
    return _manifest_path(root, run_id, create=create).with_suffix(".cancel")


def _cancelled(root: Path, run_id: str, callback: Callable[[], bool] | None) -> bool:
    marker = _cancel_path(root, run_id)
    if marker.is_symlink():
        raise CompetitorScanError("competitor scan cancellation marker is unsafe")
    if marker.is_file():
        return True
    return callback is not None and bool(callback())


def _scan_reference(root: Path, target: Path, value: Any) -> tuple[str, Path]:
    """Validate a crawl result and return project-relative and absolute artifact paths."""
    scan = value
    if isinstance(scan, dict):
        scan = scan.get("path")
    if not isinstance(scan, str) or not scan:
        raise ValueError("crawl returned no retained scan path")
    source_path = Path(scan)
    if source_path.is_symlink():
        raise ValueError("crawl result is not a regular retained scan")
    scan_path = source_path.resolve()
    scans_root = (target / "scans").resolve()
    try:
        scan_relative = scan_path.relative_to(root.resolve()).as_posix()
        scan_path.relative_to(scans_root)
    except (OSError, ValueError) as exc:
        raise ValueError("crawl result is not a retained child-project scan") from exc
    if scan_path.is_symlink() or not scan_path.is_file():
        raise ValueError("crawl result is not a regular retained scan")
    return scan_relative, scan_path


def _targets(root: Path, project: dict[str, Any], task: dict[str, Any]) -> list[dict[str, Any]]:
    prep = read_document(root, "preparation.json")
    if prep is None:
        raise ValueError("register competitor workspaces before scanning a task's competitors")
    candidates = prep.get("competitors")
    if not isinstance(candidates, list):
        raise ValueError("project competitor list is invalid")
    requested = task["definition"].get("competitors") or []
    if not isinstance(requested, list) or not requested or len(requested) > MAX_COMPETITORS:
        raise ValueError("the task has no bounded competitor list")
    found: dict[str, dict[str, Any]] = {}
    for row in candidates:
        if not isinstance(row, dict):
            continue
        try:
            identity = _competitor_identity(row["url"])
            relative = row["directory"]
            if (
                not isinstance(relative, str)
                or Path(relative).is_absolute()
                or ".." in Path(relative).parts
                or not relative.startswith("competitors/")
            ):
                continue
            child = root / relative
            if child.is_symlink() or not child.resolve().is_relative_to(root.resolve()):
                continue
            child_root, child_project = _load(child)
            if (
                child_root != child.resolve()
                or child_project["project_uuid"] != row.get("project_uuid")
                or child_project["site"]["target"] != identity
            ):
                continue
        except (OSError, KeyError, TypeError, ValueError):
            continue
        if identity in found:
            raise ValueError("project contains duplicate competitor workspaces")
        found[identity] = {**row, "root": child_root, "project": child_project}

    output = []
    seen = set()
    requested_urls = 0
    for item in requested:
        if not isinstance(item, dict) or set(item) != {"url", "max_urls"}:
            raise ValueError("each task competitor requires url and max_urls")
        if type(item["max_urls"]) is not int or not 1 <= item["max_urls"] <= MAX_SITE_URLS:
            raise ValueError(f"each task competitor max_urls must be 1..{MAX_SITE_URLS}")
        requested_urls += item["max_urls"]
        if requested_urls > MAX_TOTAL_URLS:
            raise ValueError(f"aggregate competitor URL cap is {MAX_TOTAL_URLS}")
        identity = _competitor_identity(item["url"])
        if identity in seen:
            raise ValueError("task contains duplicate competitor sites")
        seen.add(identity)
        candidate = found.get(identity)
        if candidate is None:
            raise ValueError(f"competitor workspace is unavailable for {item['url']}")
        if identity == _competitor_identity(project["site"]["target"]):
            raise ValueError("primary site cannot be scanned as a competitor")
        output.append({**candidate, "max_urls": item["max_urls"]})
    return output


def _crawl_plan(target: dict[str, Any], requested_max_urls: int) -> dict[str, Any]:
    from .schedule import show as schedule_show

    schedule = schedule_show(target["root"])
    settings = schedule["settings"]
    crawl = settings["crawl"]
    cap = min(requested_max_urls, crawl["max_urls"], MAX_SITE_URLS)
    if cap < 1:
        raise ValueError("competitor URL cap must be positive")
    max_requests = min(MAX_SITE_REQUESTS, max(150, cap * 3), crawl["max_requests"])
    max_seconds = min(MAX_SITE_SECONDS, max(60, cap * 2))
    mode = crawl["mode"]
    plan: dict[str, Any] = {
        "url": target["url"],
        "project_uuid": target["project_uuid"],
        "directory": target["directory"],
        "max_urls": cap,
        "max_requests": max_requests,
        "max_crawl_seconds": max_seconds,
        "mode": mode,
        "user_agent": settings["user_agent"]["custom"] or settings["user_agent"]["preset"],
        "min_delay_seconds": float(crawl["min_delay_seconds"]),
        "sitemap_url": crawl["sitemap_url"],
        "urls": list(crawl["urls"]),
        "schedule_revision": schedule["revision"],
    }
    if mode == "list" and not plan["urls"]:
        monitor = read_document(target["root"], "monitor.json") or {}
        plan["urls"] = list((monitor.get("policy") or {}).get("urls") or [])
    if mode == "list" and not plan["urls"]:
        raise ValueError("the competitor's list crawl has no configured URLs")
    if mode == "list" and (
        not isinstance(plan["urls"], list)
        or len(plan["urls"]) > MAX_SITE_URLS
        or any(
            not isinstance(url, str)
            or urlsplit(url).scheme not in {"http", "https"}
            or not urlsplit(url).netloc
            for url in plan["urls"]
        )
    ):
        raise ValueError("the competitor's URL list is invalid or unbounded")
    if mode == "list" and len(set(plan["urls"])) != len(plan["urls"]):
        raise ValueError("the competitor's URL list contains duplicates")
    if mode == "list" and len(plan["urls"]) > cap:
        raise ValueError("the task cap is below the configured list size; refusing to truncate it")
    return plan


def _request(target: dict[str, Any], plan: dict[str, Any], scan_out: Path, *, approve: bool) -> dict[str, Any]:
    kwargs: dict[str, Any] = {
        "project": str(target["root"]),
        "max_urls": plan["max_urls"],
        "min_delay": plan["min_delay_seconds"],
        "user_agent": plan["user_agent"],
        "overrides": {
            "limits.max_requests": plan["max_requests"],
            "limits.max_crawl_seconds": plan["max_crawl_seconds"],
        },
        "scan_out": str(scan_out),
        "approve_large_crawl": approve,
    }
    if plan["mode"] == "spider":
        kwargs["url"] = target["url"]
        kwargs["overrides"]["sitemaps.auto_discover"] = True
    elif plan["mode"] == "sitemap":
        kwargs.update(url=plan["sitemap_url"], sitemap=plan["sitemap_url"], sitemap_only=True)
    else:
        kwargs["urls"] = list(plan["urls"])
    return kwargs


def _read_scan(scan_path: Path) -> dict[str, Any]:
    from seohead.storage.history import _read_scan as read_scan

    return read_scan(scan_path)


def _plan_digest(plan: dict[str, Any]) -> str:
    payload = json.dumps(
        plan,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    ).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


def _compatibility_plan(plan: dict[str, Any]) -> dict[str, Any]:
    """Only crawl inputs affect resume compatibility; revisions remain manifest provenance."""
    return {key: value for key, value in plan.items() if key != "schedule_revision"}


def _manifest_fingerprint(task_id: str, plans: list[dict[str, Any]]) -> str:
    payload = json.dumps(
        {
            "task_id": task_id,
            "plans": [_compatibility_plan(plan) for plan in plans],
        },
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    ).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


def _legacy_manifest_fingerprint(
    task_id: str, coverage_revision: int, plans: list[dict[str, Any]]
) -> str:
    """Recognize manifests written before resume stopped binding to global coverage revisions."""
    payload = json.dumps(
        {"task_id": task_id, "revision": coverage_revision, "plans": plans},
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    ).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


def _site_inputs_match(site: dict[str, Any], plan: dict[str, Any]) -> bool:
    # The second hash keeps existing manifests resumable when their stored schedule revision
    # still matches. New manifests exclude that metadata-only revision from this identity.
    return site.get("input_hash") in {
        _plan_digest(plan),
        _plan_digest(_compatibility_plan(plan)),
    }


def _manifest_matches_inputs(
    manifest: dict[str, Any], task_id: str, plans: list[dict[str, Any]]
) -> bool:
    if manifest.get("task_id") != task_id:
        return False
    if manifest.get("fingerprint") not in {
        _manifest_fingerprint(task_id, plans),
        _legacy_manifest_fingerprint(task_id, manifest.get("coverage_revision"), plans),
    }:
        return False
    planned = {(plan["project_uuid"], plan["url"]): plan for plan in plans}
    sites = manifest.get("sites")
    if not isinstance(sites, list) or len(sites) != len(plans):
        return False
    for site in sites:
        if not isinstance(site, dict):
            return False
        plan = planned.get((site.get("project_uuid"), site.get("url")))
        if plan is None or not _site_inputs_match(site, plan):
            return False
    return True


def task_competitor_run_reference(
    directory: str | Path, *, task_id: str, task: dict[str, Any]
) -> dict[str, Any] | None:
    """Return a bounded latest compatible run reference for one current task, without writes."""
    try:
        root, project = _load(directory)
        if task.get("definition", {}).get("kind") != "custom":
            return None
        if not task["definition"].get("competitors"):
            return None
        targets = _targets(root, project, task)
        plans, _budgets = _plans(targets)
        manifests = _manifests(root, project["project_uuid"])
        compatible = [
            manifest
            for manifest in manifests
            if _manifest_matches_inputs(manifest, task_id, plans)
        ]
        if not compatible:
            return None
        resumable = [manifest for manifest in compatible if _incomplete(manifest)]
        manifest = max(resumable or compatible, key=lambda item: item["updated_at"])
        active = _run_lock_active(root)
        state = manifest["state"]
        if state == "running" and not active:
            state = "interrupted"
        return {
            "run_id": manifest["run_id"],
            "state": state,
            "resumable": _incomplete(manifest),
            "approve_large_crawl": manifest["approve_large_crawl"],
            "producer_build": manifest.get("producer_build"),
            "updated_at": manifest["updated_at"],
            "coverage_revision": manifest["coverage_revision"],
            "completed_sites": sum(
                site.get("state") == "completed" for site in manifest["sites"]
            ),
            "total_sites": len(manifest["sites"]),
        }
    except (OSError, TypeError, ValueError):
        return {"state": "unavailable", "resumable": False, "reason": "run history is unavailable"}


def _manifest_result(manifest: dict[str, Any], *, resumed: bool) -> dict[str, Any]:
    state = manifest["state"]
    results = [
        {
            key: item.get(key)
            for key in (
                "url",
                "project_uuid",
                "max_urls",
                "max_requests",
                "max_crawl_seconds",
                "state",
                "scan",
                "scan_uuid",
                "partial",
                "finish_reason",
                "reason",
                "fetched",
                "queued",
            )
        }
        for item in manifest["sites"]
    ]
    return {
        "ok": state == "completed",
        "run_id": manifest["run_id"],
        "state": state,
        "task_id": manifest["task_id"],
        "coverage_revision": manifest["coverage_revision"],
        "concurrency": manifest["concurrency"],
        "budgets": manifest["budgets"],
        "cancel_reason": manifest.get("cancel_reason"),
        "results": results,
        "resumed": resumed,
    }


def _local_run_lock(root: Path) -> threading.Lock:
    key = str(root.resolve())
    with _LOCAL_RUN_LOCKS_GUARD:
        return _LOCAL_RUN_LOCKS.setdefault(key, threading.Lock())


def _open_run_lock(root: Path) -> tuple[int, threading.Lock]:
    local = _local_run_lock(root)
    if not local.acquire(blocking=False):
        raise CompetitorScanError("another competitor scan owns the project")
    fd = None
    path = root / RUN_LOCK
    try:
        fd = open_lock(path)
        lock_exclusive(fd)
        return fd, local
    except OSError as exc:
        if fd is not None:
            os.close(fd)
        local.release()
        raise CompetitorScanError("another competitor scan owns the project") from exc


def _set_run_lock_owner(fd: int, run_id: str, runner_token: str) -> None:
    """Write identity in place while holding the lock; never replace its inode."""
    record = json.dumps(
        {"run_id": run_id, "runner_token": runner_token}, separators=(",", ":")
    ).encode("ascii")
    os.ftruncate(fd, 1)
    os.lseek(fd, 0, os.SEEK_SET)
    if os.write(fd, b"\0") != 1:
        raise OSError("could not initialize competitor lock metadata")
    os.lseek(fd, 1, os.SEEK_SET)
    view = memoryview(record)
    while view:
        written = os.write(fd, view)
        if written <= 0:
            raise OSError("could not persist competitor lock identity")
        view = view[written:]
    os.fsync(fd)


def _run_lock_owner_locked(root: Path) -> tuple[bool, dict[str, str] | None]:
    """Read exact active-run identity while admission is fenced by project_write_lock."""
    path = root / RUN_LOCK
    if not os.path.lexists(path):
        return False, None
    try:
        fd = open_lock(path, create=False)
    except OSError as exc:
        raise CompetitorScanError("competitor scan lock is unsafe or unavailable") from exc
    try:
        os.lseek(fd, 1, os.SEEK_SET)
        raw = os.read(fd, 256)
    finally:
        os.close(fd)
    active = _run_lock_active(root)
    if not active:
        return False, None
    try:
        value = json.loads(raw.decode("ascii"))
    except (UnicodeDecodeError, ValueError) as exc:
        raise CompetitorScanError("active competitor runner identity is unavailable") from exc
    if (
        not isinstance(value, dict)
        or set(value) != {"run_id", "runner_token"}
        or not isinstance(value["run_id"], str)
        or not isinstance(value["runner_token"], str)
        or len(value["runner_token"]) != 32
        or any(char not in "0123456789abcdef" for char in value["runner_token"])
    ):
        raise CompetitorScanError("active competitor runner identity is invalid")
    try:
        value["run_id"] = str(uuid.UUID(value["run_id"]))
    except (TypeError, ValueError) as exc:
        raise CompetitorScanError("active competitor runner identity is invalid") from exc
    return True, value


def _run_lock_active(root: Path) -> bool:
    local = _local_run_lock(root)
    if not local.acquire(blocking=False):
        return True
    path = root / RUN_LOCK
    if not os.path.lexists(path):
        local.release()
        return False
    try:
        fd = open_lock(path, create=False)
    except OSError as exc:
        local.release()
        raise CompetitorScanError("competitor scan lock is unsafe or unavailable") from exc
    locked = False
    try:
        try:
            lock_exclusive(fd)
        except OSError:
            return True
        locked = True
        return False
    finally:
        try:
            if locked:
                with contextlib.suppress(OSError):
                    unlock(fd)
            os.close(fd)
        finally:
            local.release()


def has_active_run(root: str | Path) -> bool:
    """Return whether any competitor runner owns this project's durable lock."""
    return _run_lock_active(Path(root))


def _plans(target_rows: list[dict[str, Any]]) -> tuple[list[dict[str, Any]], dict[str, int]]:
    plans = [_crawl_plan(target, target["max_urls"]) for target in target_rows]
    budgets = {
        "urls": sum(plan["max_urls"] for plan in plans),
        "requests": sum(plan["max_requests"] for plan in plans),
        "seconds": sum(plan["max_crawl_seconds"] for plan in plans),
        "max_urls": MAX_TOTAL_URLS,
        "max_requests": MAX_TOTAL_REQUESTS,
        "max_seconds": MAX_TOTAL_SECONDS,
    }
    if budgets["urls"] > MAX_TOTAL_URLS:
        raise ValueError(f"aggregate competitor URL cap is {MAX_TOTAL_URLS}")
    if budgets["requests"] > MAX_TOTAL_REQUESTS:
        raise ValueError(f"aggregate competitor request cap is {MAX_TOTAL_REQUESTS}")
    if budgets["seconds"] > MAX_TOTAL_SECONDS:
        raise ValueError(f"aggregate competitor crawl-time cap is {MAX_TOTAL_SECONDS} seconds")
    return plans, budgets


def _new_manifest(
    root: Path,
    project: dict[str, Any],
    *,
    run_id: str,
    task_id: str,
    expected_revision: int,
    concurrency: int,
    approve_large_crawl: bool,
    producer_build: str | None,
    plans: list[dict[str, Any]],
    budgets: dict[str, int],
) -> dict[str, Any]:
    sites = []
    from seohead.storage.history import new_scan_path

    for plan in plans:
        target_root = root / plan["directory"]
        child_uuid = plan["project_uuid"]
        scan_path = new_scan_path(
            target_root / "scans", plan["url"], str(uuid.uuid4())
        )
        artifact = scan_path.resolve().relative_to(target_root.resolve()).as_posix()
        sites.append(
            {
                **{key: value for key, value in plan.items() if key != "urls"},
                "input_hash": _plan_digest(_compatibility_plan(plan)),
                "state": "pending",
                "artifact": artifact,
                "scan": None,
                "scan_uuid": None,
                "partial": None,
                "finish_reason": None,
                "reason": None,
                "fetched": 0,
                "queued": 0,
                "started_at": None,
                "finished_at": None,
            }
        )
    stamp = _now()
    return {
        "format": RUN_FORMAT,
        "run_id": run_id,
        "project_uuid": project["project_uuid"],
        "task_id": task_id,
        "coverage_revision": expected_revision,
        "state": "running",
        "runner_token": None,
        "started_at": stamp,
        "updated_at": stamp,
        "finished_at": None,
        "concurrency": concurrency,
        "approve_large_crawl": approve_large_crawl,
        "producer_build": producer_build,
        "fingerprint": _manifest_fingerprint(task_id, plans),
        "budgets": budgets,
        "sites": sites,
    }


def _manifests(root: Path, project_uuid: str) -> list[dict[str, Any]]:
    directory = _run_directory(root, create=False)
    if not directory.exists():
        return []
    paths = sorted(directory.glob("*.json"), key=lambda path: path.name)
    if len(paths) > MAX_RUN_MANIFESTS:
        raise CompetitorScanError(f"competitor scan history exceeds {MAX_RUN_MANIFESTS} runs")
    output = []
    for path in paths:
        if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_MANIFEST_BYTES:
            raise CompetitorScanError("competitor scan history contains an unsafe manifest")
        run_id = path.stem
        output.append(_read_manifest(root, run_id, project_uuid))
    return output


def _incomplete(manifest: dict[str, Any]) -> bool:
    return any(item["state"] in {"pending", "running", "interrupted"} for item in manifest["sites"])


def _resolve_site_target(root: Path, targets: list[dict[str, Any]], site: dict[str, Any]) -> dict[str, Any]:
    match = next(
        (
            row
            for row in targets
            if row["project_uuid"] == site["project_uuid"] and row["url"] == site["url"]
        ),
        None,
    )
    if match is None or site["directory"] != match["directory"]:
        raise CompetitorScanError("competitor workspace changed since the run checkpoint")
    return match


def scan_task_competitors(
    directory: str | Path,
    *,
    task_id: str,
    expected_revision: int,
    confirm: bool,
    concurrency: int = MAX_CONCURRENCY,
    approve_large_crawl: bool = False,
    producer_build: str | None = None,
    tools: dict[str, Callable[..., Any]],
    progress: Callable[[str, int, int], None] | None = None,
    cancel_requested: Callable[[], bool] | None = None,
    run_id: str | None = None,
) -> dict[str, Any]:
    """Scan a task's competitors with durable per-site checkpoints and fixed budgets."""
    if confirm is not True:
        raise ValueError("confirm=true is required to contact competitor sites")
    if not isinstance(task_id, str) or not task_id:
        raise ValueError("task_id is required")
    if type(expected_revision) is not int:
        raise ValueError("expected_revision is required")
    if type(concurrency) is not int or not 1 <= concurrency <= MAX_CONCURRENCY:
        raise ValueError(f"concurrency must be 1..{MAX_CONCURRENCY}")
    if type(approve_large_crawl) is not bool:
        raise ValueError("approve_large_crawl must be a boolean")
    if producer_build is not None and not isinstance(producer_build, str):
        raise ValueError("producer_build must be text")
    # Validate injected side effects before claiming the project run lock.
    if not isinstance(tools, dict) or not callable(tools.get("crawl_site")):
        raise ValueError("the interface must supply its existing crawl_site handler")
    if progress is not None and not callable(progress):
        raise ValueError("progress must be callable")
    if cancel_requested is not None and not callable(cancel_requested):
        raise ValueError("cancel_requested must be callable")
    if run_id is not None:
        try:
            run_id = str(uuid.UUID(run_id))
        except (TypeError, ValueError) as exc:
            raise ValueError("run_id must be a UUID") from exc

    root, project = _load(directory)
    checklist = coverage_status(root)
    if checklist.get("revision") != expected_revision:
        raise ValueError(f"coverage revision conflict: current revision is {checklist.get('revision')}")
    document = _read(root, project)
    saved = (document or {}).get("items", {}).get(task_id)
    if not saved or saved["definition"].get("kind") != "custom":
        raise ValueError("task_id must name a current custom task")
    target_rows = _targets(root, project, saved)
    plans, budgets = _plans(target_rows)
    fingerprint = _manifest_fingerprint(task_id, plans)

    from .runtime import project_write_lock

    with project_write_lock(root):
        current_root, current_project = _load(directory)
        if current_root != root or current_project["project_uuid"] != project["project_uuid"]:
            raise CompetitorScanError("project changed before competitor-run admission")
        lock_fd, local_run_lock = _open_run_lock(root)
    try:
        # Recheck the revision/task under the run lock before publishing a manifest.
        current = coverage_status(root)
        if current.get("revision") != expected_revision:
            raise ValueError(f"coverage revision conflict: current revision is {current.get('revision')}")
        if run_id is not None:
            existing_path = _manifest_path(root, run_id)
            if existing_path.exists():
                manifest = _read_manifest(root, run_id, project["project_uuid"])
                if not _manifest_matches_inputs(manifest, task_id, plans):
                    raise CompetitorScanError("run inputs changed since this run_id was created")
                if not _incomplete(manifest):
                    return _manifest_result(manifest, resumed=True)
                resumed = True
            else:
                manifest = _new_manifest(
                    root,
                    project,
                    run_id=run_id,
                    task_id=task_id,
                    expected_revision=expected_revision,
                    concurrency=concurrency,
                    approve_large_crawl=approve_large_crawl,
                    producer_build=producer_build,
                    plans=plans,
                    budgets=budgets,
                )
                resumed = False
        else:
            previous = [
                item
                for item in _manifests(root, project["project_uuid"])
                if item["task_id"] == task_id
                and _incomplete(item)
                and _manifest_matches_inputs(item, task_id, plans)
            ]
            if previous:
                manifest = max(previous, key=lambda item: item["updated_at"])
                resumed = True
            else:
                if len(_manifests(root, project["project_uuid"])) >= MAX_RUN_MANIFESTS:
                    raise CompetitorScanError(f"competitor scan history is limited to {MAX_RUN_MANIFESTS} runs")
                manifest = _new_manifest(
                    root,
                    project,
                    run_id=str(uuid.uuid4()),
                    task_id=task_id,
                    expected_revision=expected_revision,
                    concurrency=concurrency,
                    approve_large_crawl=approve_large_crawl,
                    producer_build=producer_build,
                    plans=plans,
                    budgets=budgets,
                )
                resumed = False

        if resumed and (
            manifest["approve_large_crawl"] != approve_large_crawl
            or manifest.get("producer_build") != producer_build
        ):
            raise CompetitorScanError("approval or producer settings changed since the run checkpoint")
        manifest["concurrency"] = concurrency
        current_plans = {
            (plan["project_uuid"], plan["url"]): plan for plan in plans
        }
        if any(
            current_plans.get((site["project_uuid"], site["url"])) is None
            or not _site_inputs_match(
                site, current_plans[(site["project_uuid"], site["url"])]
            )
            for site in manifest["sites"]
        ):
            raise CompetitorScanError("competitor scan site settings changed since the run checkpoint")

        cancel_path = _cancel_path(root, manifest["run_id"], create=True)
        manifest["state"] = "running"
        manifest["finished_at"] = None
        manifest.pop("cancel_reason", None)
        manifest["runner_token"] = uuid.uuid4().hex
        with project_write_lock(root):
            _set_run_lock_owner(lock_fd, manifest["run_id"], manifest["runner_token"])
            _publish_start(root, manifest, cancel_path)
        crawl = tools["crawl_site"]
        state_lock = threading.RLock()
        cancel_event = threading.Event()
        progress_last_write: dict[str, float] = {}

        def save() -> None:
            manifest["updated_at"] = _now()
            _write_manifest(root, manifest)

        def execute(site_index: int) -> None:
            site = manifest["sites"][site_index]
            target = _resolve_site_target(root, target_rows, site)
            child_root = target["root"]
            artifact_rel = site["artifact"]
            artifact = child_root / artifact_rel
            if artifact.is_symlink() or not artifact.resolve().is_relative_to((child_root / "scans").resolve()):
                raise CompetitorScanError("competitor scan checkpoint path is unsafe")

            with state_lock:
                current_state = site["state"]
                if current_state in {"completed", "partial", "failed", "cancelled"}:
                    return
                if _cancelled(root, manifest["run_id"], cancel_requested) or cancel_event.is_set():
                    site.update(state="cancelled", reason="cancelled before this site started", finished_at=_now())
                    save()
                    return
                recovering = current_state in {"running", "interrupted"}
                if recovering and not artifact.is_file():
                    site.update(
                        state="failed",
                        reason="interrupted site has no retained scan artifact to resume",
                        finished_at=_now(),
                    )
                    save()
                    return
                if not recovering and artifact.exists():
                    site.update(
                        state="failed",
                        reason="scan artifact exists without a started checkpoint; refusing to overwrite",
                        finished_at=_now(),
                    )
                    save()
                    return
                site.update(state="running", started_at=site.get("started_at") or _now(), reason=None)
                save()

            if recovering:
                try:
                    existing_record = _read_scan(artifact)
                except (OSError, ValueError) as exc:
                    with state_lock:
                        site.update(state="failed", reason=f"interrupted scan is unreadable: {type(exc).__name__}", finished_at=_now())
                        save()
                    return
                if not existing_record["crawl_partial"] and not existing_record["corpus_partial"]:
                    with state_lock:
                        site.update(
                            state="completed",
                            scan=(Path(site["directory"]) / artifact_rel).as_posix(),
                            scan_uuid=existing_record["scan_uuid"],
                            partial=False,
                            finish_reason=existing_record["finish_reason"],
                            finished_at=_now(),
                            reason=None,
                        )
                        save()
                    return
                kwargs = {
                    "project": str(child_root),
                    "resume": str(artifact),
                    "approve_large_crawl": manifest["approve_large_crawl"],
                    "producer_build": manifest["producer_build"],
                }
            else:
                plan = current_plans[(site["project_uuid"], site["url"])]
                kwargs = _request(
                    target,
                    plan,
                    artifact,
                    approve=manifest["approve_large_crawl"],
                )
                kwargs["producer_build"] = manifest["producer_build"]

            def on_progress(fetched: int, queued: int) -> None:
                with state_lock:
                    site["fetched"] = max(0, int(fetched))
                    site["queued"] = max(0, int(queued))
                    checkpoint = time.monotonic()
                    if checkpoint - progress_last_write.get(site["url"], 0.0) >= 5.0:
                        progress_last_write[site["url"]] = checkpoint
                        save()
                if cancel_event.is_set() or _cancelled(root, manifest["run_id"], cancel_requested):
                    raise KeyboardInterrupt("competitor scan cancelled")
                if progress is not None:
                    progress(site["url"], int(fetched), int(queued))

            kwargs["progress"] = on_progress
            try:
                result = crawl(**kwargs)
            except (KeyboardInterrupt, SystemExit, asyncio.CancelledError) as exc:
                with state_lock:
                    site.update(
                        state="interrupted",
                        reason=type(exc).__name__,
                        finished_at=_now(),
                        scan=(Path(site["directory"]) / artifact_rel).as_posix()
                        if artifact.is_file()
                        else None,
                    )
                    save()
                raise
            except Exception as exc:
                with state_lock:
                    site.update(state="failed", reason=str(exc)[:1024], finished_at=_now())
                    save()
                return

            if not isinstance(result, dict) or result.get("ok") is False:
                with state_lock:
                    site.update(
                        state="failed",
                        reason=(str(result.get("error", "crawl failed")) if isinstance(result, dict) else "invalid crawl result")[:1024],
                        finished_at=_now(),
                    )
                    save()
                return
            scan_value = result.get("scan") or result.get("scan_path") or str(artifact)
            try:
                scan_rel, scan_path = _scan_reference(root, child_root, scan_value)
                scan_record = _read_scan(scan_path)
            except (OSError, ValueError) as exc:
                with state_lock:
                    site.update(state="failed", reason=str(exc)[:1024], finished_at=_now())
                    save()
                return
            partial = bool(scan_record["crawl_partial"] or scan_record["corpus_partial"])
            cancelled = _cancelled(root, manifest["run_id"], cancel_requested) or cancel_event.is_set()
            state = "interrupted" if cancelled and partial else "partial" if partial else "completed"
            with state_lock:
                site.update(
                    state=state,
                    scan=scan_rel,
                    scan_uuid=scan_record["scan_uuid"],
                    partial=partial,
                    finish_reason=scan_record["finish_reason"],
                    reason="cancelled" if state == "interrupted" else None,
                    finished_at=_now(),
                )
                save()

        executor = ThreadPoolExecutor(max_workers=concurrency, thread_name_prefix="competitor-crawl")
        cancellation: BaseException | None = None
        try:
            futures = {
                executor.submit(execute, index): index
                for index in range(len(manifest["sites"]))
            }
            for future in as_completed(futures):
                try:
                    future.result()
                except (KeyboardInterrupt, SystemExit, asyncio.CancelledError) as exc:
                    cancellation = exc
                    cancel_event.set()
                    from .runtime import project_write_lock

                    with project_write_lock(root, timeout_seconds=5.0):
                        atomic_write_bytes(cancel_path, b"cancel requested\n", mode=0o600)
                    break
                except Exception as exc:
                    site = manifest["sites"][futures[future]]
                    with state_lock:
                        site.update(
                            state="failed",
                            reason=f"{type(exc).__name__}: {exc}"[:1024],
                            finished_at=_now(),
                        )
                        save()
        except (KeyboardInterrupt, SystemExit, asyncio.CancelledError) as exc:
            cancellation = exc
            cancel_event.set()
            from .runtime import project_write_lock

            with project_write_lock(root, timeout_seconds=5.0):
                atomic_write_bytes(cancel_path, b"cancel requested\n", mode=0o600)
        finally:
            executor.shutdown(wait=True, cancel_futures=False)

        if cancellation is not None:
            with state_lock:
                manifest["state"] = "interrupted"
                manifest["cancel_reason"] = type(cancellation).__name__
                for site in manifest["sites"]:
                    if site["state"] in {"pending", "running"}:
                        site.update(
                            state="interrupted",
                            reason=type(cancellation).__name__,
                            finished_at=_now(),
                        )
                manifest["finished_at"] = _now()
                _publish_final(root, manifest, cancel_path)
            raise cancellation

        with state_lock:
            site_states = [site["state"] for site in manifest["sites"]]
            if any(state in {"pending", "running", "interrupted"} for state in site_states):
                manifest["state"] = "interrupted"
            elif any(state == "cancelled" for state in site_states):
                manifest["state"] = "cancelled"
            elif any(state == "failed" for state in site_states):
                manifest["state"] = "failed"
            elif any(state == "partial" for state in site_states):
                manifest["state"] = "partial"
            else:
                manifest["state"] = "completed"
            manifest["finished_at"] = _now()
            _publish_final(root, manifest, cancel_path)
        return _manifest_result(manifest, resumed=resumed)
    finally:
        try:
            with contextlib.suppress(OSError):
                unlock(lock_fd)
        finally:
            try:
                os.close(lock_fd)
            finally:
                local_run_lock.release()


def get_competitor_scan_run(directory: str | Path, run_id: str) -> dict[str, Any]:
    """Read durable per-site state for an explicitly started competitor scan run."""
    root, project = _load(directory)
    from .runtime import project_write_lock

    with project_write_lock(root):
        manifest = _read_manifest(root, run_id, project["project_uuid"])
        active, owner = _run_lock_owner_locked(root)
        matches_owner = bool(
            manifest["state"] == "running"
            and active
            and owner
            and owner["run_id"] == manifest["run_id"]
            and manifest.get("runner_token") == owner["runner_token"]
        )
    marker = _cancel_path(root, run_id)
    if marker.is_symlink():
        raise CompetitorScanError("competitor scan cancellation marker is unsafe")
    requested = marker.is_file() and matches_owner
    results = _manifest_result(manifest, resumed=False)["results"]
    if manifest["state"] == "running" and not matches_owner:
        for item in results:
            if item.get("state") == "running":
                item["state"] = "interrupted"
                item["reason"] = "this run no longer owns the project lock"
    return {
        "ok": True,
        "run_id": manifest["run_id"],
        "state": (
            "cancel_requested"
            if requested and manifest["state"] == "running" and matches_owner
            else "interrupted"
            if manifest["state"] == "running" and not matches_owner
            else manifest["state"]
        ),
        "cancel_requested": requested,
        "runner_active": matches_owner,
        "task_id": manifest["task_id"],
        "coverage_revision": manifest["coverage_revision"],
        "updated_at": manifest["updated_at"],
        "budgets": manifest["budgets"],
        "results": results,
    }


def cancel_competitor_scan_run(directory: str | Path, run_id: str) -> dict[str, Any]:
    """Request cancellation of an active run; the crawler stops at its next progress boundary."""
    root, project = _load(directory)
    _manifest_path(root, run_id)
    from .runtime import project_write_lock

    with project_write_lock(root, timeout_seconds=5.0):
        manifest = _read_manifest(root, run_id, project["project_uuid"])
        active, owner = _run_lock_owner_locked(root)
        matches_owner = bool(
            manifest["state"] == "running"
            and active
            and owner
            and owner["run_id"] == manifest["run_id"]
            and manifest.get("runner_token") == owner["runner_token"]
        )
        if manifest["state"] != "running" or not matches_owner:
            return {
                "ok": True,
                "run_id": manifest["run_id"],
                "state": "interrupted" if manifest["state"] == "running" else manifest["state"],
                "cancelled": False,
            }
        marker = _cancel_path(root, run_id)
        if marker.is_symlink():
            raise CompetitorScanError("competitor scan cancellation marker is unsafe")
        atomic_write_bytes(marker, b"cancel requested\n", mode=0o600)
    return {"ok": True, "run_id": str(uuid.UUID(run_id)), "state": "cancel_requested", "cancelled": True}
