"""Durable, read-only-observable local collector attempts.

This is intentionally a project-side operation log, not another crawler or a
job scheduler.  A collector records its own lifecycle around existing native or
Screaming Frog calls; the observer only reads the document and checks whether a
recorded local PID is still alive.
"""

from __future__ import annotations

import copy
import math
import os
import subprocess
import time
import uuid
from collections import deque
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from .coverage import _now
from .runtime import read_document, write_document
from .workspace import _load as _load_workspace

FORMAT = "seohead.run-observation.v1"
NAME = "run-observation.json"
MAX_RUNS = 100
MAX_EVENTS_PER_RUN = 200
MAX_EVENT_MESSAGE = 240
STALE_SAMPLE_SECONDS = 5.0
RATE_WINDOW_SECONDS = 5.0
WRITE_ATTEMPTS = 20
_KINDS = {"native", "screaming_frog", "sitemap"}
_MODES = {"spider", "list", "sf_live", "sf_exports", "sitemap"}
_STATES = {"running", "finished", "partial", "failed", "cancelled"}
_PHASES = {"admission", "collection", "render", "external", "analysis", "finalizing"}


def _text(value: Any, name: str, maximum: int) -> str:
    if not isinstance(value, str) or not value or len(value) > maximum:
        raise ValueError(f"{name} must be nonempty text of at most {maximum} characters")
    return value


def _counter(value: Any, name: str) -> int:
    if type(value) is not int or value < 0:
        raise ValueError(f"{name} must be a nonnegative integer")
    return value


def _optional_counter(value: Any, name: str) -> int | None:
    return None if value is None else _counter(value, name)


def _relative_artifact(root: Path, artifact: str | Path | None) -> str | None:
    if artifact is None:
        return None
    try:
        relative = Path(artifact).resolve().relative_to(root.resolve())
    except (OSError, ValueError) as exc:
        raise ValueError("collector artifact must be inside the project workspace") from exc
    if not relative.parts or relative.parts[0] not in {"scans", "reports"}:
        raise ValueError("collector artifact must be under scans/ or reports/")
    return relative.as_posix()


def _empty(project_uuid: str) -> dict[str, Any]:
    return {"format": FORMAT, "revision": 0, "project_uuid": project_uuid, "runs": []}


def _process_identity(pid: int) -> str | None:
    """Return a platform process-start identity when the host exposes one."""
    if os.name == "nt":
        import ctypes
        from ctypes import wintypes

        class _FileTime(ctypes.Structure):
            _fields_ = [("low", wintypes.DWORD), ("high", wintypes.DWORD)]

        try:
            kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
            open_process = kernel32.OpenProcess
            open_process.argtypes = (wintypes.DWORD, wintypes.BOOL, wintypes.DWORD)
            open_process.restype = wintypes.HANDLE
            get_times = kernel32.GetProcessTimes
            get_times.argtypes = (
                wintypes.HANDLE,
                ctypes.POINTER(_FileTime),
                ctypes.POINTER(_FileTime),
                ctypes.POINTER(_FileTime),
                ctypes.POINTER(_FileTime),
            )
            get_times.restype = wintypes.BOOL
            close_handle = kernel32.CloseHandle
            close_handle.argtypes = (wintypes.HANDLE,)
            close_handle.restype = wintypes.BOOL
            handle = open_process(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
            if not handle:
                return None
            created = _FileTime()
            exited = _FileTime()
            kernel = _FileTime()
            user = _FileTime()
            try:
                measured = get_times(
                    handle,
                    ctypes.byref(created),
                    ctypes.byref(exited),
                    ctypes.byref(kernel),
                    ctypes.byref(user),
                )
                if not measured:
                    return None
                value = (created.high << 32) | created.low
                return f"windows:{value}"
            finally:
                close_handle(handle)
        except (AttributeError, OSError, ValueError):
            return None
    try:
        result = subprocess.run(
            ["ps", "-o", "lstart=", "-p", str(pid)],
            capture_output=True,
            check=False,
            text=True,
            timeout=1,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    value = result.stdout.strip() if result.returncode == 0 else ""
    return value or None


def _normalize(document: dict[str, Any]) -> None:
    """Read old live-run records without making an observer write a migration."""
    for run in document.get("runs", []) if isinstance(document.get("runs"), list) else []:
        if not isinstance(run, dict):
            continue
        run.setdefault("source_metadata", None)
        run.setdefault(
            "telemetry",
            {
                "sampled_at": None,
                "rate_window_seconds": None,
                "queue_semantics": "unknown",
                "source": "legacy",
            },
        )
        collector = run.get("collector")
        if isinstance(collector, dict):
            collector.setdefault("origin", None)
            collector.setdefault("aggregate_max_requests_per_second", None)
        if "controller_pid" not in run and "pid" in run:
            run["controller_pid"] = run["pid"]
            run["controller_start_identity"] = None
            run["collector_pid"] = None
            run["collector_start_identity"] = None
            run["collector_started_at"] = None


def _validate(document: dict[str, Any], project_uuid: str) -> None:
    if (
        set(document) != {"format", "revision", "project_uuid", "runs"}
        or document["format"] != FORMAT
        or document["project_uuid"] != project_uuid
        or type(document["revision"]) is not int
        or document["revision"] < 0
        or not isinstance(document["runs"], list)
        or len(document["runs"]) > MAX_RUNS
    ):
        raise ValueError("run observation document has an unsupported shape")
    ids = set()
    for run in document["runs"]:
        if not isinstance(run, dict) or set(run) != {
            "id",
            "kind",
            "state",
            "started_at",
            "finished_at",
            "pid",
            "controller_pid",
            "controller_start_identity",
            "collector_pid",
            "collector_start_identity",
            "collector_started_at",
            "collector",
            "artifact",
            "counters",
            "finish_reason",
            "events",
            "telemetry",
            "source_metadata",
        }:
            raise ValueError("run observation contains an unsupported run")
        _text(run["id"], "run id", 64)
        if run["id"] in ids:
            raise ValueError("run observation contains duplicate run IDs")
        ids.add(run["id"])
        if run["kind"] not in _KINDS or run["state"] not in _STATES:
            raise ValueError("run observation has an unsupported kind or state")
        if type(run["pid"]) is not int or run["pid"] <= 0:
            raise ValueError("run observation has an invalid PID")
        if run["controller_pid"] != run["pid"]:
            raise ValueError("run observation controller PID disagrees with its compatibility PID")
        if run["controller_start_identity"] is not None and not isinstance(
            run["controller_start_identity"], str
        ):
            raise ValueError("run observation controller identity is invalid")
        if run["collector_pid"] is not None and (
            type(run["collector_pid"]) is not int or run["collector_pid"] <= 0
        ):
            raise ValueError("run observation collector PID is invalid")
        if run["collector_start_identity"] is not None and not isinstance(
            run["collector_start_identity"], str
        ):
            raise ValueError("run observation collector identity is invalid")
        if run["collector_started_at"] is not None and not isinstance(
            run["collector_started_at"], str
        ):
            raise ValueError("run observation collector start time is invalid")
        if not isinstance(run["collector"], dict) or set(run["collector"]) != {
            "mode",
            "max_urls",
            "max_requests",
            "max_crawl_seconds",
            "max_requests_per_second",
            "config_fingerprint",
            "resumed",
            "origin",
            "aggregate_max_requests_per_second",
        }:
            raise ValueError("run observation collector shape is invalid")
        if run["collector"]["mode"] not in _MODES:
            raise ValueError("run observation collector mode is invalid")
        _counter(run["collector"]["max_urls"], "collector max_urls")
        _counter(run["collector"]["max_requests"], "collector max_requests")
        _counter(run["collector"]["max_crawl_seconds"], "collector max_crawl_seconds")
        if run["collector"]["max_requests_per_second"] is not None and (
            not isinstance(run["collector"]["max_requests_per_second"], float)
            or not math.isfinite(run["collector"]["max_requests_per_second"])
            or run["collector"]["max_requests_per_second"] < 0
        ):
            raise ValueError("run observation request-rate limit is invalid")
        if not isinstance(run["collector"]["config_fingerprint"], str):
            raise ValueError("run observation config fingerprint is invalid")
        if type(run["collector"]["resumed"]) is not bool:
            raise ValueError("run observation resumed flag is invalid")
        origin = run["collector"]["origin"]
        if origin is not None and (not isinstance(origin, str) or not origin or len(origin) > 253):
            raise ValueError("run observation origin is invalid")
        aggregate_rate = run["collector"]["aggregate_max_requests_per_second"]
        if aggregate_rate is not None and (
            not isinstance(aggregate_rate, float)
            or not math.isfinite(aggregate_rate)
            or aggregate_rate < 0
        ):
            raise ValueError("run observation aggregate request rate is invalid")
        if run["artifact"] is not None and not isinstance(run["artifact"], str):
            raise ValueError("run observation artifact is invalid")
        if not isinstance(run["counters"], dict) or set(run["counters"]) != {
            "fetched",
            "queued",
            "inflight",
            "excluded",
            "rate_per_second",
        }:
            raise ValueError("run observation counters are invalid")
        for name, value in run["counters"].items():
            if name == "rate_per_second":
                if value is not None and (
                    not isinstance(value, float) or not math.isfinite(value) or value < 0
                ):
                    raise ValueError("run observation rate is invalid")
            else:
                _optional_counter(value, name)
        source = run["source_metadata"]
        if source is not None:
            if (
                run["kind"] != "sitemap"
                or not isinstance(source, dict)
                or set(source)
                != {"root", "declared_url_count", "parsed_documents", "error_count", "truncated"}
            ):
                raise ValueError("run source metadata has an unsupported shape")
            _text(source["root"], "sitemap root", 8192)
            for name in ("declared_url_count", "parsed_documents", "error_count"):
                _counter(source[name], name)
            if type(source["truncated"]) is not bool:
                raise ValueError("sitemap truncated state must be boolean")
        telemetry = run["telemetry"]
        if not isinstance(telemetry, dict) or set(telemetry) != {
            "sampled_at",
            "rate_window_seconds",
            "queue_semantics",
            "source",
        }:
            raise ValueError("run telemetry has an unsupported shape")
        if telemetry["sampled_at"] is not None:
            if not isinstance(telemetry["sampled_at"], str):
                raise ValueError("run sample timestamp must be text")
            stamp = datetime.fromisoformat(telemetry["sampled_at"].replace("Z", "+00:00"))
            if stamp.tzinfo is None:
                raise ValueError("run sample timestamp must include a timezone")
        window = telemetry["rate_window_seconds"]
        if window is not None and (
            type(window) not in {float, int} or not math.isfinite(window) or window < 0
        ):
            raise ValueError("run rate window is invalid")
        if telemetry["queue_semantics"] not in {"unknown", "separate", "outstanding"}:
            raise ValueError("run queue semantics is invalid")
        _text(telemetry["source"], "telemetry source", 64)
        if run["finish_reason"] is not None and not isinstance(run["finish_reason"], str):
            raise ValueError("run observation finish reason is invalid")
        if not isinstance(run["events"], list) or len(run["events"]) > MAX_EVENTS_PER_RUN:
            raise ValueError("run observation events are invalid")


def _load(directory: str | Path) -> tuple[Path, dict[str, Any], dict[str, Any]]:
    root, project = _load_workspace(directory)
    document = read_document(root, NAME) or _empty(project["project_uuid"])
    _normalize(document)
    _validate(document, project["project_uuid"])
    return root, project, document


def _event(run: dict[str, Any], phase: str, code: str, *, message: str | None = None) -> None:
    if phase not in _PHASES:
        raise ValueError("run observation phase is invalid")
    entry = {"at": _now(), "phase": phase, "code": _text(code, "event code", 64)}
    if message is not None:
        entry["message"] = _text(message, "event message", MAX_EVENT_MESSAGE)
    run["events"].append(entry)
    del run["events"][:-MAX_EVENTS_PER_RUN]


def _find(document: dict[str, Any], run_id: str) -> dict[str, Any]:
    run_id = _text(run_id, "run id", 64)
    found = next((item for item in document["runs"] if item["id"] == run_id), None)
    if found is None:
        raise ValueError("run observation is not part of this project")
    return found


def _new_run_id(value: str | None) -> str:
    if value is None:
        return uuid.uuid4().hex
    if not isinstance(value, str) or not value:
        raise ValueError("observer run ID must be a UUID")
    try:
        return str(uuid.UUID(value))
    except (TypeError, ValueError) as exc:
        raise ValueError("observer run ID must be a UUID") from exc


def _save(root: Path, document: dict[str, Any]) -> None:
    expected = document["revision"]
    document["revision"] += 1
    write_document(root, NAME, document, expected_revision=expected)


def _contention(exc: ValueError) -> bool:
    return str(exc) in {"project document revision conflict", f"another writer owns {NAME}"}


def _retry_delay(attempt: int) -> None:
    time.sleep(0.01 * (attempt + 1))


def _owner_busy(run: dict[str, Any]) -> bool:
    """A running record whose controller may still be alive blocks a new scan."""
    owner = _runtime_state(run["controller_pid"], run["controller_start_identity"], running=True)
    return owner in {"live", "unknown"}


def live_holder(directory: str | Path) -> str | None:
    """Describe the running collector whose controller is still alive, if there is one."""
    _, _, document = _load(directory)
    for run in document["runs"]:
        if run["state"] == "running" and _owner_busy(run):
            return f"{run['kind']} run {run['id']} (pid {run['controller_pid']})"
    return None


def start(
    directory: str | Path,
    *,
    kind: str,
    mode: str,
    max_urls: int,
    max_requests: int = 0,
    max_crawl_seconds: int = 0,
    max_requests_per_second: float | None = None,
    config_fingerprint: str,
    artifact: str | Path | None,
    resumed: bool = False,
    origin: str | None = None,
    aggregate_max_requests_per_second: float | None = None,
    run_id: str | None = None,
    counters: dict[str, int | None] | None = None,
) -> dict[str, Any]:
    """Persist one project-bound local collector before it starts doing work."""
    if kind not in _KINDS or mode not in _MODES:
        raise ValueError("run kind or collector mode is invalid")
    _counter(max_urls, "max_urls")
    _counter(max_requests, "max_requests")
    _counter(max_crawl_seconds, "max_crawl_seconds")
    if max_requests_per_second is not None and (
        not isinstance(max_requests_per_second, float)
        or not math.isfinite(max_requests_per_second)
        or max_requests_per_second < 0
    ):
        raise ValueError("max_requests_per_second must be a finite nonnegative float")
    if type(resumed) is not bool:
        raise ValueError("resumed must be boolean")
    if origin is not None and (not isinstance(origin, str) or not origin or len(origin) > 253):
        raise ValueError("origin must be bounded nonempty text or null")
    if aggregate_max_requests_per_second is not None and (
        not isinstance(aggregate_max_requests_per_second, float)
        or not math.isfinite(aggregate_max_requests_per_second)
        or aggregate_max_requests_per_second < 0
    ):
        raise ValueError("aggregate maximum request rate must be finite and nonnegative")
    run_id = _new_run_id(run_id)
    started_at = _now()
    controller_pid = os.getpid()
    controller_identity = _process_identity(controller_pid)
    for attempt in range(WRITE_ATTEMPTS):
        root, _project, document = _load(directory)
        if any(item["id"] == run_id for item in document["runs"]):
            raise ValueError("observer run ID already exists in this project")
        for item in document["runs"]:
            if item["state"] == "running" and _owner_busy(item):
                raise ValueError(f"project busy; owner={item['id']} pid={item['controller_pid']}")
        run = {
            "id": run_id,
            "kind": kind,
            "state": "running",
            "started_at": started_at,
            "finished_at": None,
            "pid": controller_pid,
            "controller_pid": controller_pid,
            "controller_start_identity": controller_identity,
            "collector_pid": None,
            "collector_start_identity": None,
            "collector_started_at": None,
            "collector": {
                "mode": mode,
                "max_urls": max_urls,
                "max_requests": max_requests,
                "max_crawl_seconds": max_crawl_seconds,
                "max_requests_per_second": max_requests_per_second,
                "config_fingerprint": _text(config_fingerprint, "config fingerprint", 256),
                "resumed": resumed,
                "origin": origin,
                "aggregate_max_requests_per_second": aggregate_max_requests_per_second,
            },
            "artifact": _relative_artifact(root, artifact),
            "counters": {
                **{
                    name: _optional_counter((counters or {}).get(name), name)
                    for name in ("fetched", "queued", "inflight", "excluded")
                },
                "rate_per_second": (counters or {}).get("rate_per_second"),
            },
            "finish_reason": None,
            "events": [],
            "source_metadata": None,
            "telemetry": {
                "sampled_at": None,
                "rate_window_seconds": None,
                "queue_semantics": "unknown",
                "source": "collector",
            },
        }
        _event(run, "admission", "started")
        if len(document["runs"]) >= MAX_RUNS:
            terminal = next((item for item in document["runs"] if item["state"] != "running"), None)
            if terminal is None:
                raise ValueError("run observation capacity reached; active runs cannot be evicted")
            document["runs"].remove(terminal)
        document["runs"].append(run)
        try:
            _save(root, document)
            return copy.deepcopy(run)
        except ValueError as exc:
            if not _contention(exc) or attempt + 1 == WRITE_ATTEMPTS:
                raise
            _retry_delay(attempt)
    raise AssertionError("unreachable")


def phase(directory: str | Path, run_id: str, name: str, *, code: str = "entered") -> None:
    for attempt in range(WRITE_ATTEMPTS):
        root, _project, document = _load(directory)
        run = _find(document, run_id)
        if run["state"] != "running":
            return
        _event(run, name, code)
        try:
            _save(root, document)
            return
        except ValueError as exc:
            if not _contention(exc) or attempt + 1 == WRITE_ATTEMPTS:
                raise
            _retry_delay(attempt)


def collector_started(directory: str | Path, run_id: str, pid: int) -> None:
    """Bind an SF child PID after spawn; it is distinct from the controller PID."""
    if type(pid) is not int or pid <= 0:
        raise ValueError("collector PID must be a positive integer")
    identity = _process_identity(pid)
    started_at = _now()
    for attempt in range(WRITE_ATTEMPTS):
        root, _project, document = _load(directory)
        run = _find(document, run_id)
        if run["state"] != "running":
            return
        run["collector_pid"] = pid
        run["collector_start_identity"] = identity
        run["collector_started_at"] = started_at
        _event(run, "collection", "collector_started")
        try:
            _save(root, document)
            return
        except ValueError as exc:
            if not _contention(exc) or attempt + 1 == WRITE_ATTEMPTS:
                raise
            _retry_delay(attempt)


def progress(
    directory: str | Path,
    run_id: str,
    *,
    fetched: int | None,
    queued: int | None,
    inflight: int | None = None,
    excluded: int | None = None,
    rate_per_second: float | None = None,
    rate_window_seconds: float | None = None,
    queue_semantics: str = "separate",
    source: str = "collector",
) -> None:
    """Record measured collector counters; no percentage or site total is inferred."""
    values = {
        "fetched": _optional_counter(fetched, "fetched"),
        "queued": _optional_counter(queued, "queued"),
        "inflight": _optional_counter(inflight, "inflight"),
        "excluded": _optional_counter(excluded, "excluded"),
        "rate_per_second": rate_per_second,
    }
    if rate_per_second is not None and (
        not isinstance(rate_per_second, float)
        or not math.isfinite(rate_per_second)
        or rate_per_second < 0
    ):
        raise ValueError("rate_per_second must be a finite nonnegative float")
    for attempt in range(WRITE_ATTEMPTS):
        root, _project, document = _load(directory)
        run = _find(document, run_id)
        if run["state"] != "running":
            return
        run["counters"] = values
        run["telemetry"] = {
            "sampled_at": _now(),
            "rate_window_seconds": rate_window_seconds,
            "queue_semantics": queue_semantics,
            "source": source,
        }
        _validate(document, document["project_uuid"])
        _event(run, "collection", "progress")
        try:
            _save(root, document)
            return
        except ValueError as exc:
            if not _contention(exc) or attempt + 1 == WRITE_ATTEMPTS:
                raise
            _retry_delay(attempt)


def finish(
    directory: str | Path,
    run_id: str,
    *,
    state: str,
    reason: str,
    counters: dict[str, int | float | None] | None = None,
) -> None:
    if state not in _STATES - {"running"}:
        raise ValueError("run terminal state is invalid")
    normalized_counters = (
        {
            **{
                name: _optional_counter(counters.get(name), name)
                for name in ("fetched", "queued", "inflight", "excluded")
            },
            "rate_per_second": counters.get("rate_per_second"),
        }
        if counters is not None
        else None
    )
    finish_reason = _text(reason, "finish reason", 500)
    finished_at = _now()
    for attempt in range(WRITE_ATTEMPTS):
        root, _project, document = _load(directory)
        run = _find(document, run_id)
        if run["state"] != "running":
            return
        if normalized_counters is not None:
            run["counters"] = normalized_counters
            run["telemetry"]["sampled_at"] = finished_at
        run["state"] = state
        run["finish_reason"] = finish_reason
        run["finished_at"] = finished_at
        _event(run, "finalizing", "finished" if state == "finished" else state)
        try:
            _save(root, document)
        except ValueError as exc:
            if not _contention(exc) or attempt + 1 == WRITE_ATTEMPTS:
                raise
            _retry_delay(attempt)
            continue
        from .journal import note

        note(root, source="scans", actor="agent", text=f"crawl {run_id} {state}: {finish_reason}")
        return


def finish_sitemap(directory: str | Path, run_id: str, result: dict) -> None:
    """Retain final parsed-document evidence, never infer fetched HTML or completion ratio."""
    if (
        not isinstance(result, dict)
        or not isinstance(result.get("sitemaps"), list)
        or not isinstance(result.get("errors"), list)
    ):
        raise ValueError("sitemap result requires measured document and error lists")
    metadata = {
        "root": _text(result.get("root"), "sitemap root", 8192),
        "declared_url_count": _counter(result.get("count"), "declared URL count"),
        "parsed_documents": len(result["sitemaps"]),
        "error_count": len(result["errors"]),
        "truncated": result.get("truncated"),
    }
    if type(metadata["truncated"]) is not bool or type(result.get("ok")) is not bool:
        raise ValueError("sitemap result requires explicit success and truncation states")
    for attempt in range(WRITE_ATTEMPTS):
        root, _project, document = _load(directory)
        run = _find(document, run_id)
        if run["kind"] != "sitemap":
            raise ValueError("sitemap result belongs to a sitemap run")
        if run["state"] != "running":
            return
        stamp = _now()
        run["source_metadata"] = metadata
        run["counters"] = {
            "fetched": metadata["parsed_documents"],
            "queued": None,
            "inflight": None,
            "excluded": None,
            "rate_per_second": None,
        }
        run["telemetry"] = {
            "sampled_at": stamp,
            "rate_window_seconds": None,
            "queue_semantics": "unknown",
            "source": "sitemap_result",
        }
        run["state"] = (
            "failed"
            if not result["ok"]
            else "partial"
            if metadata["truncated"] or metadata["error_count"]
            else "finished"
        )
        run["finish_reason"] = (
            "sitemap_failed"
            if not result["ok"]
            else "truncated"
            if metadata["truncated"]
            else "source_errors"
            if metadata["error_count"]
            else "finished"
        )
        run["finished_at"] = stamp
        _event(run, "finalizing", run["state"])
        _validate(document, document["project_uuid"])
        try:
            _save(root, document)
            return
        except ValueError as exc:
            if not _contention(exc) or attempt + 1 == WRITE_ATTEMPTS:
                raise
            _retry_delay(attempt)


def _runtime_state(pid: int | None, identity: str | None, *, running: bool) -> str:
    if not running:
        return "retained"
    if pid is None:
        return "unavailable"
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return "abandoned"
    except PermissionError:
        return "unknown"
    except (OSError, OverflowError, ValueError):
        return "unknown"
    current = _process_identity(pid)
    if identity is None or current is None:
        return "unknown"
    if current != identity:
        return "stale"
    return "live"


def status(directory: str | Path, *, limit: int = 20, offset: int = 0) -> dict[str, Any]:
    """Read all stored running records and one terminal page without writing.

    Offset counts terminal records in reverse admission order. Retention is
    bounded; totals describe the current document, not the project's lifetime.
    """
    if type(limit) is not int or not 1 <= limit <= MAX_RUNS:
        raise ValueError(f"run observation limit must be 1..{MAX_RUNS}")
    _counter(offset, "run observation offset")
    _root, _project, document = _load(directory)
    rows = []
    active = [run for run in document["runs"] if run["state"] == "running"]
    terminal = [run for run in reversed(document["runs"]) if run["state"] != "running"]
    page = terminal[offset : offset + limit]
    selected_ids = {run["id"] for run in active + page}
    has_more = offset + len(page) < len(terminal)
    runtime_cache: dict[tuple, str] = {}

    def runtime(pid, identity, running):
        key = (pid, identity, running)
        if key not in runtime_cache:
            runtime_cache[key] = _runtime_state(pid, identity, running=running)
        return runtime_cache[key]

    for run in reversed(document["runs"]):
        if run["id"] not in selected_ids:
            continue
        row = copy.deepcopy(run)
        row["source_kind"] = run["kind"]
        observed_at = _now()
        running = run["state"] == "running"
        controller_state = runtime(run["controller_pid"], run["controller_start_identity"], running)
        collector_state = runtime(run["collector_pid"], run["collector_start_identity"], running)
        row["pid_state"] = controller_state
        row["controller"] = {
            "pid": run["controller_pid"],
            "start_identity": run["controller_start_identity"],
            "state": controller_state,
            "observed_at": observed_at,
        }
        row["collector_runtime"] = {
            "pid": run["collector_pid"],
            "start_identity": run["collector_start_identity"],
            "started_at": run["collector_started_at"],
            "state": collector_state,
            "observed_at": observed_at,
        }
        sampled = row["telemetry"]["sampled_at"]
        age = (
            None
            if sampled is None
            else max(
                0.0,
                (
                    datetime.now(timezone.utc)
                    - datetime.fromisoformat(sampled.replace("Z", "+00:00"))
                ).total_seconds(),
            )
        )
        freshness = (
            "unavailable"
            if age is None
            else "retained"
            if not running
            else "stale"
            if age > STALE_SAMPLE_SECONDS
            else "fresh"
        )
        row["telemetry"].update(
            unit="sitemap_documents"
            if row["kind"] == "sitemap"
            else "urls_including_resources"
            if row["telemetry"]["source"].startswith("sf_")
            else "pages",
            age_seconds=age,
            state=freshness,
            current_rate_per_second=row["counters"]["rate_per_second"]
            if freshness == "fresh"
            else None,
        )
        rows.append(row)
    return {
        "ok": True,
        "revision": document["revision"],
        "total": len(document["runs"]),
        "active_total": len(active),
        "terminal_total": len(terminal),
        "items": rows,
        "has_more": has_more,
        "pagination": {
            "offset": offset,
            "limit": limit,
            "total": len(terminal),
            "next_offset": offset + len(page) if has_more else None,
            "has_more": has_more,
        },
        "retention": {
            "max_runs": MAX_RUNS,
            "eviction": "oldest_terminal_on_start",
            "evicted_total": None,
        },
    }


class NativeRunReporter:
    """Coalesce measured counters without inventing values absent from legacy callbacks."""

    def __init__(
        self, directory: str | Path, run_id: str, downstream=None, *, source="native"
    ) -> None:
        self.directory = str(directory)
        self.run_id = run_id
        self.downstream = downstream
        self.last_write: float | None = None
        self.fetched = 0
        self.queued = 0
        self.inflight = None
        self.excluded = None
        self.samples: deque[tuple[float, int]] = deque(maxlen=64)
        self.rate_per_second: float | None = None
        self.rate_window_seconds: float | None = None
        self.structured = False
        self.source = source

    def __call__(self, fetched: int, queued: int) -> None:
        if self.downstream is not None:
            self.downstream(fetched, queued)
        if not self.structured:
            self._record(fetched, queued, None, None, "outstanding")

    def observe_counts(self, counters: dict) -> None:
        """Receive the native writer's transaction-consistent separate counters."""
        self.structured = True
        self._record(
            *(counters.get(key) for key in ("fetched", "queued", "inflight", "excluded")),
            "separate",
            force=bool(counters.get("final", False)),
        )

    def _record(self, fetched, queued, inflight, excluded, semantics, *, force=False):
        self.fetched, self.queued = fetched, queued
        self.inflight, self.excluded = inflight, excluded
        now = time.monotonic()
        if not force and self.last_write is not None and now - self.last_write < 0.5:
            return
        if self.samples and (
            fetched < self.samples[-1][1] or now - self.samples[-1][0] > RATE_WINDOW_SECONDS
        ):
            self.samples.clear()
        self.samples.append((now, fetched))
        while len(self.samples) > 2 and now - self.samples[1][0] >= RATE_WINDOW_SECONDS:
            self.samples.popleft()
        then, prior = self.samples[0]
        self.rate_window_seconds = now - then if now > then else None
        self.rate_per_second = (fetched - prior) / (now - then) if now > then else None
        try:
            progress(
                self.directory,
                self.run_id,
                fetched=fetched,
                queued=queued,
                inflight=inflight,
                excluded=excluded,
                rate_per_second=self.rate_per_second,
                rate_window_seconds=self.rate_window_seconds,
                queue_semantics=semantics,
                source=(
                    "sf_stdout_19_8"
                    if self.source == "sf"
                    else "native_frontier"
                    if self.structured
                    else "native_callback"
                ),
            )
        except (OSError, ValueError):
            return
        self.last_write = now

    def enter(self, name: str, *, code: str = "entered") -> None:
        try:
            phase(self.directory, self.run_id, name, code=code)
        except (OSError, ValueError):
            return

    def counters(self) -> dict[str, int | float | None]:
        return {
            "fetched": self.fetched,
            "queued": self.queued,
            "inflight": self.inflight,
            "excluded": self.excluded,
            "rate_per_second": self.rate_per_second,
        }
