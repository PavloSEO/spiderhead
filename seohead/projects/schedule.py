"""Scheduled project scans: settings in ``schedule.json``, due rules, and a headless runner.

Settings are a revisioned project document, edited through ``write_document`` with
``expected_revision``. Runtime facts (last window, slice artifact, lock) live in
``schedule-state.json``. ``run_due`` is safe to call often: it does nothing outside
the configured window, refuses overlap, and records one journal entry per decision.
The scan itself is injected by the interface layer, so the core never imports MCP.
"""

from __future__ import annotations

import asyncio
import os
import stat
from collections.abc import Callable
from datetime import datetime, time, timedelta, timezone
from pathlib import Path, PureWindowsPath
from typing import Any
from urllib.parse import urlsplit
from zoneinfo import ZoneInfo

from seohead.core.common import utc_iso_z as _now
from seohead.crawl.user_agents import resolve as resolve_user_agent

from . import journal
from .runtime import _competitor_identity, read_document, write_document
from .workspace import _load as _workspace_load

FORMAT = "seohead.schedule.v1"
STATE_FORMAT = "seohead.schedule-state.v1"
NAME = "schedule.json"
STATE_NAME = "schedule-state.json"
LOCK_NAME = ".schedule.lock"
MODES = ("spider", "sitemap", "list")
_WEEKDAYS = {"mon": 0, "tue": 1, "wed": 2, "thu": 3, "fri": 4, "sat": 5, "sun": 6}
_MAX_URLS = 1_000_000
_MAX_LIST = 500
# Host suffix -> IANA zone. Only a site's TLD is consulted; hreflang and server
# location are not read here, so an unmapped suffix falls back to UTC and says so.
_TLD_ZONES = {
    "by": "Europe/Minsk",
    "ru": "Europe/Moscow",
    "ua": "Europe/Kyiv",
    "kz": "Asia/Almaty",
    "de": "Europe/Berlin",
    "fr": "Europe/Paris",
    "uk": "Europe/London",
    "pl": "Europe/Warsaw",
    "it": "Europe/Rome",
    "es": "Europe/Madrid",
    "nl": "Europe/Amsterdam",
    "tr": "Europe/Istanbul",
    "jp": "Asia/Tokyo",
    "cn": "Asia/Shanghai",
    "in": "Asia/Kolkata",
    "au": "Australia/Sydney",
    "br": "America/Sao_Paulo",
}


def default_settings(project_uuid: str) -> dict[str, Any]:
    return {
        "format": FORMAT,
        "project_uuid": project_uuid,
        "revision": 0,
        "enabled": False,
        "time_zone": "auto",
        "weekdays": [],
        "month_days": [],
        "window": {"start": "01:00", "end": "06:00"},
        "include_competitors": False,
        "crawl": {
            "mode": "spider",
            "sitemap_url": None,
            "urls": [],
            "max_urls": 500,
            "max_requests": 2_000,
            "min_delay_seconds": 1.0,
            "slice": False,
        },
        "user_agent": {"preset": "spiderhead", "custom": None},
    }


def _clock(value: Any, label: str) -> time:
    if not isinstance(value, str) or len(value) != 5 or value[2] != ":":
        raise ValueError(f"{label} must be HH:MM")
    try:
        return time.fromisoformat(value)
    except ValueError as exc:
        raise ValueError(f"{label} must be HH:MM") from exc


def _bounded_int(value: Any, label: str, ceiling: int) -> int:
    if type(value) is not int or not 1 <= value <= ceiling:
        raise ValueError(f"{label} must be an integer from 1 to {ceiling}")
    return value


def validate(settings: Any) -> dict[str, Any]:
    """Return a normalized copy of a settings object or raise ValueError."""
    if not isinstance(settings, dict):
        raise ValueError("schedule settings must be an object")
    allowed = {
        "format",
        "project_uuid",
        "revision",
        "enabled",
        "time_zone",
        "weekdays",
        "month_days",
        "window",
        "include_competitors",
        "crawl",
        "user_agent",
    }
    unknown = set(settings) - allowed
    if unknown:
        raise ValueError("schedule has unsupported fields: " + ", ".join(sorted(unknown)))
    out = {**settings}
    if type(out.get("enabled")) is not bool:
        raise ValueError("schedule enabled must be boolean")
    if type(out.get("include_competitors")) is not bool:
        raise ValueError("include_competitors must be boolean")
    zone = out.get("time_zone")
    if zone != "auto":
        if not isinstance(zone, str):
            raise ValueError("time_zone must be 'auto' or an IANA zone name")
        try:
            ZoneInfo(zone)
        except (KeyError, ValueError) as exc:
            raise ValueError(f"unknown time_zone {zone!r}") from exc
    weekdays = out.get("weekdays")
    if not isinstance(weekdays, list) or any(
        not isinstance(item, str) or item.lower() not in _WEEKDAYS for item in weekdays
    ):
        raise ValueError("weekdays must be a list of mon..sun")
    out["weekdays"] = sorted({item.lower() for item in weekdays}, key=lambda name: _WEEKDAYS[name])
    month_days = out.get("month_days")
    if not isinstance(month_days, list) or any(
        type(item) is not int or not 1 <= item <= 31 for item in month_days
    ):
        raise ValueError("month_days must be integers 1..31")
    out["month_days"] = sorted(set(month_days))
    window = out.get("window")
    if not isinstance(window, dict) or set(window) != {"start", "end"}:
        raise ValueError("window requires start and end as HH:MM")
    _clock(window["start"], "window.start")
    _clock(window["end"], "window.end")
    if window["start"] == window["end"]:
        raise ValueError("window.start and window.end must differ")
    crawl = out.get("crawl")
    crawl_keys = {
        "mode",
        "sitemap_url",
        "urls",
        "max_urls",
        "max_requests",
        "min_delay_seconds",
        "slice",
    }
    if not isinstance(crawl, dict) or set(crawl) != crawl_keys:
        raise ValueError("crawl requires " + ", ".join(sorted(crawl_keys)))
    if crawl["mode"] not in MODES:
        raise ValueError("crawl.mode must be spider, sitemap or list")
    if crawl["sitemap_url"] is not None and not _is_http(crawl["sitemap_url"]):
        raise ValueError("crawl.sitemap_url must be an absolute HTTP(S) URL")
    if crawl["mode"] == "sitemap" and crawl["sitemap_url"] is None:
        raise ValueError("sitemap mode requires crawl.sitemap_url")
    urls = crawl["urls"]
    if (
        not isinstance(urls, list)
        or len(urls) > _MAX_LIST
        or len(set(urls)) != len(urls)
        or any(not _is_http(url) for url in urls)
    ):
        raise ValueError("crawl.urls must be a unique bounded list of absolute HTTP(S) URLs")
    _bounded_int(crawl["max_urls"], "crawl.max_urls", _MAX_URLS)
    _bounded_int(crawl["max_requests"], "crawl.max_requests", _MAX_URLS * 10)
    if type(crawl["slice"]) is not bool:
        raise ValueError("crawl.slice must be boolean")
    delay = crawl["min_delay_seconds"]
    if isinstance(delay, bool) or not isinstance(delay, (int, float)) or not 0.5 <= delay <= 60:
        raise ValueError("crawl.min_delay_seconds must be between 0.5 and 60")
    agent = out.get("user_agent")
    if not isinstance(agent, dict) or set(agent) != {"preset", "custom"}:
        raise ValueError("user_agent requires preset and custom")
    if agent["custom"] is not None:
        resolve_user_agent(agent["custom"])
    else:
        if not isinstance(agent["preset"], str):
            raise ValueError("user_agent.preset must be a preset name")
        resolve_user_agent(agent["preset"])
    return out


def _is_http(value: Any) -> bool:
    if not isinstance(value, str) or not value.startswith(("http://", "https://")):
        return False
    return bool(urlsplit(value).netloc)


def infer_time_zone(host: str) -> tuple[str, str]:
    """Return ``(zone, source)``: a TLD-based guess, or UTC when the TLD is unmapped."""
    suffix = host.rsplit(".", 1)[-1].lower() if "." in host else ""
    zone = _TLD_ZONES.get(suffix)
    if zone:
        return zone, "tld"
    return "UTC", "fallback"


def _zone_for(settings: dict[str, Any], host: str) -> tuple[ZoneInfo, str]:
    if settings["time_zone"] == "auto":
        name, source = infer_time_zone(host)
        return ZoneInfo(name), source
    return ZoneInfo(settings["time_zone"]), "explicit"


def _window_day(local: datetime, start: time, end: time) -> Any:
    """Return the local date the window belongs to when ``local`` is inside it, else None.

    A window that crosses midnight belongs to the date it opened on.
    """
    current = local.time()
    if start < end:
        return local.date() if start <= current < end else None
    if current >= start:
        return local.date()
    if current < end:
        return (local - timedelta(days=1)).date()
    return None


def _day_matches(settings: dict[str, Any], day: Any) -> bool:
    if not settings["weekdays"] and not settings["month_days"]:
        return True
    weekday_ok = any(_WEEKDAYS[name] == day.weekday() for name in settings["weekdays"])
    return weekday_ok or day.day in settings["month_days"]


def due_state(
    settings: dict[str, Any], host: str, now: datetime, last_window: str | None
) -> dict[str, Any]:
    """Decide whether a run is due at ``now`` (aware UTC), without side effects."""
    if not settings["enabled"]:
        return {"due": False, "reason": "disabled"}
    zone, source = _zone_for(settings, host)
    local = now.astimezone(zone)
    start = _clock(settings["window"]["start"], "window.start")
    end = _clock(settings["window"]["end"], "window.end")
    day = _window_day(local, start, end)
    base = {"local_time": local.isoformat(), "time_zone": zone.key, "time_zone_source": source}
    if day is None:
        return {**base, "due": False, "reason": "outside window"}
    if not _day_matches(settings, day):
        return {**base, "due": False, "reason": "day not selected"}
    key = day.isoformat()
    if last_window == key:
        return {**base, "due": False, "reason": "already ran in this window", "window": key}
    return {**base, "due": True, "reason": "due", "window": key}


def next_due(settings: dict[str, Any], host: str, now: datetime) -> str | None:
    """First window opening within 15 local days, as UTC ISO text, or None when disabled."""
    if not settings["enabled"]:
        return None
    zone, _ = _zone_for(settings, host)
    start = _clock(settings["window"]["start"], "window.start")
    local = now.astimezone(zone)
    for offset in range(15):
        day = local.date() + timedelta(days=offset)
        if not _day_matches(settings, day):
            continue
        opening = datetime.combine(day, start, tzinfo=zone)
        if opening >= now:
            return opening.astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    return None


def _state_default(project_uuid: str) -> dict[str, Any]:
    return {
        "format": STATE_FORMAT,
        "project_uuid": project_uuid,
        "revision": 0,
        "last_window": None,
        "last_run_at": None,
        "last_result": None,
        "resume_artifact": None,
    }


def _open(directory: str | Path) -> tuple[Path, dict[str, Any], dict[str, Any], dict[str, Any]]:
    root, project = _workspace_load(directory)
    uuid_value = project["project_uuid"]
    current = read_document(root, NAME)
    if current is None:
        settings = default_settings(uuid_value)
    else:
        if current.get("format") != FORMAT or current.get("project_uuid") != uuid_value:
            raise ValueError("schedule belongs to another project or format")
        settings = current
    state = read_document(root, STATE_NAME) or _state_default(uuid_value)
    if state.get("format") != STATE_FORMAT or state.get("project_uuid") != uuid_value:
        raise ValueError("schedule state belongs to another project or format")
    return root, project, settings, state


def show(directory: str | Path, *, now: datetime | None = None) -> dict[str, Any]:
    root, project, settings, state = _open(directory)
    stamp = now or datetime.now(timezone.utc)
    host = urlsplit(project["site"]["target"]).hostname or ""
    competitors = []
    if settings["include_competitors"]:
        for target in _competitor_targets(root, project):
            child_root, child_project, child_settings, child_state = _open(target["directory"])
            child_host = urlsplit(child_project["site"]["target"]).hostname or ""
            configured = read_document(child_root, NAME) is not None
            decision = due_state(child_settings, child_host, stamp, child_state.get("last_window"))
            competitors.append(
                {
                    "url": target["url"],
                    "directory": target["directory_relative"],
                    "schedule_configured": configured,
                    "revision": child_settings["revision"],
                    "enabled": child_settings["enabled"],
                    "due": decision,
                    "next_due_at": next_due(child_settings, child_host, stamp),
                }
            )
    return {
        "ok": True,
        "revision": settings["revision"],
        "settings": settings,
        "state": {k: state.get(k) for k in ("last_window", "last_run_at", "last_result")},
        "resume_artifact": state.get("resume_artifact"),
        "due": due_state(settings, host, stamp, state.get("last_window")),
        "next_due_at": next_due(settings, host, stamp),
        "competitors": competitors,
    }


def configure(
    directory: str | Path,
    *,
    expected_revision: int,
    settings: dict[str, Any] | None = None,
    **fields: Any,
) -> dict[str, Any]:
    """Merge a partial update into the settings and publish the next revision."""
    if type(expected_revision) is not int:
        raise ValueError("expected_revision is required")
    patch = {**(settings or {}), **fields}
    if "revision" in patch or "project_uuid" in patch or "format" in patch:
        raise ValueError("revision, project_uuid and format are managed by the runtime")
    root, project, current, _ = _open(directory)
    if current["revision"] != expected_revision:
        raise ValueError("schedule revision conflict")
    merged = {**current, **patch}
    merged["crawl"] = {**current["crawl"], **(patch.get("crawl") or {})}
    merged["window"] = {**current["window"], **(patch.get("window") or {})}
    merged["user_agent"] = {**current["user_agent"], **(patch.get("user_agent") or {})}
    merged["format"] = FORMAT
    merged["project_uuid"] = project["project_uuid"]
    merged["revision"] = current["revision"] + 1
    normalized = validate(merged)
    write_document(root, NAME, normalized, expected_revision=current["revision"])
    journal.note(
        directory,
        source="app",
        actor="schedule",
        text=f"schedule revision {normalized['revision']} saved",
    )
    return {"ok": True, "revision": normalized["revision"], "settings": normalized}


def _competitor_targets(root: Path, project: dict[str, Any]) -> list[dict[str, Any]]:
    """Resolve only declared competitor workspaces that still match their saved identity."""
    prep = read_document(root, "preparation.json") or {}
    rows = prep.get("competitors", []) or []
    if not isinstance(rows, list):
        raise ValueError("invalid project competitor list")
    targets = []
    seen = {project["project_uuid"]}
    for row in rows:
        if not isinstance(row, dict) or not row.get("directory") or not row.get("url"):
            continue
        relative = row["directory"]
        path_part = Path(relative) if isinstance(relative, str) else None
        if (
            path_part is None
            or path_part.is_absolute()
            or ".." in path_part.parts
            or not relative.startswith("competitors/")
        ):
            raise ValueError("unsafe competitor project reference")
        child = root / path_part
        if child.is_symlink() or not child.resolve().is_relative_to(root.resolve()):
            raise ValueError("unsafe competitor project reference")
        child_root, child_project = _workspace_load(child)
        if (
            child_root != child.resolve()
            or child_project["project_uuid"] != row.get("project_uuid")
            or child_project["site"]["target"] != _competitor_identity(row["url"])
            or child_project["project_uuid"] in seen
        ):
            raise ValueError("competitor project identity mismatch")
        seen.add(child_project["project_uuid"])
        targets.append(
            {
                "role": "competitor",
                "directory": str(child_root),
                "directory_relative": relative,
                "url": row["url"],
                "root": child_root,
                "project": child_project,
            }
        )
    return targets


def _list_urls(root: Path, settings: dict[str, Any]) -> list[str]:
    if settings["crawl"]["urls"]:
        return list(settings["crawl"]["urls"])
    monitor = read_document(root, "monitor.json") or {}
    policy = monitor.get("policy") or {}
    return list(policy.get("urls") or [])


def _stored_scan_path(root: Path, value: Any) -> Any:
    """Keep project-owned scan references portable inside schedule state."""
    if not isinstance(value, str) or not value:
        return value
    path = Path(value)
    if not path.is_absolute():
        path = root / path
    try:
        return path.resolve().relative_to(root.resolve()).as_posix()
    except (OSError, ValueError):
        return value


def _resume_scan_path(root: Path, value: Any) -> str | None:
    """Resolve relative state and rebase legacy absolute refs after project restore."""
    if not isinstance(value, str) or not value:
        return None
    path = Path(value)
    scans_root = (root / "scans").resolve()
    if not path.is_absolute() and not PureWindowsPath(value).is_absolute():
        candidate = (root / path).resolve()
        try:
            candidate.relative_to(scans_root)
        except ValueError as exc:
            raise ValueError("scheduled resume artifact must be inside project scans") from exc
        return str(candidate)
    if path.is_absolute():
        try:
            path.resolve().relative_to(scans_root)
            return str(path.resolve())
        except (OSError, ValueError):
            parts = path.parts
    else:
        parts = PureWindowsPath(value).parts
    scan_indexes = [index for index, part in enumerate(parts) if part.lower() == "scans"]
    if scan_indexes:
        relative = parts[scan_indexes[-1] + 1 :]
        if relative and ".." not in relative:
            rebased = root.joinpath("scans", *relative)
            try:
                rebased.resolve().relative_to(scans_root)
            except (OSError, ValueError):
                pass
            else:
                if rebased.is_file() and not rebased.is_symlink():
                    return str(rebased)
    raise ValueError("scheduled resume artifact cannot be resolved inside project scans")


def _request(
    settings: dict[str, Any], target: dict[str, Any], state: dict[str, Any], root_dir: Path
) -> dict[str, Any]:
    """Build crawl_site keyword arguments; a slice resumes the previous partial artifact."""
    crawl = settings["crawl"]
    agent = settings["user_agent"]
    user_agent = agent["custom"] if agent["custom"] is not None else agent["preset"]
    artifact = _resume_scan_path(root_dir, state.get("resume_artifact")) if crawl["slice"] else None
    if artifact and crawl["mode"] == "spider":
        return {"resume": artifact, "project": target["directory"], "approve_large_crawl": False}
    base: dict[str, Any] = {
        "project": target["directory"],
        "user_agent": user_agent,
        "max_urls": crawl["max_urls"],
        "overrides": {"limits.max_requests": crawl["max_requests"]},
        "min_delay": float(crawl["min_delay_seconds"]),
        "approve_large_crawl": False,
    }
    if crawl["mode"] == "spider":
        return {**base, "url": target["url"]}
    if crawl["mode"] == "sitemap":
        return {
            **base,
            "url": crawl["sitemap_url"],
            "sitemap": crawl["sitemap_url"],
            "sitemap_only": True,
        }
    urls = _list_urls(root_dir, settings)
    if not urls:
        raise ValueError("list mode has no URLs: set crawl.urls or monitor.json policy urls")
    return {**base, "urls": urls}


def _acquire(root: Path) -> int | None:
    """Hold a process-scoped OS lock for the complete run, with no expiring lease."""
    lock = root / LOCK_NAME
    if lock.is_symlink():
        return None
    flags = os.O_RDWR | os.O_CREAT | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(lock, flags, 0o600)
    except OSError:
        return None
    locked = False
    try:
        mode = os.fstat(fd).st_mode
        if lock.is_symlink() or not stat.S_ISREG(mode) or mode & 0o077:
            return None
        if os.name == "nt":
            import msvcrt

            if os.fstat(fd).st_size == 0:
                os.write(fd, b"\0")
            os.lseek(fd, 0, os.SEEK_SET)
            try:
                msvcrt.locking(fd, msvcrt.LK_NBLCK, 1)
            except OSError:
                return None
        else:
            import fcntl

            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError:
                return None
        locked = True
        return fd
    finally:
        if not locked:
            os.close(fd)


def _release(fd: int) -> None:
    """Release the OS lock; keep its inode so concurrent openers share one lock object."""
    try:
        if os.name == "nt":
            import msvcrt

            os.lseek(fd, 0, os.SEEK_SET)
            msvcrt.locking(fd, msvcrt.LK_UNLCK, 1)
        else:
            import fcntl

            fcntl.flock(fd, fcntl.LOCK_UN)
    finally:
        os.close(fd)


def active(root: str | Path) -> bool:
    """Return whether a scheduler owns this project's persistent OS lock."""
    from seohead.core import filesystem

    path = Path(root) / LOCK_NAME
    if not path.exists() and not path.is_symlink():
        return False
    try:
        fd = filesystem.open_lock(path, create=False)
    except OSError as exc:
        raise ValueError("schedule lock is unsafe or unavailable") from exc
    try:
        try:
            filesystem.lock_exclusive(fd)
        except OSError:
            return True
        filesystem.unlock(fd)
        return False
    finally:
        os.close(fd)


def run_due(
    directory: str | Path,
    *,
    scan: Callable[..., dict[str, Any]],
    now: datetime | None = None,
) -> dict[str, Any]:
    """Run the scheduled scans for one project if they are due. Safe to call often."""
    stamp = now or datetime.now(timezone.utc)
    root, project, settings, state = _open(directory)
    configured = read_document(root, NAME) is not None
    candidates = []
    unavailable: list[dict[str, Any]] = []
    if configured:
        host = urlsplit(project["site"]["target"]).hostname or ""
        primary_decision = due_state(settings, host, stamp, state.get("last_window"))
        if primary_decision["due"]:
            candidates.append(
                {
                    "role": "primary",
                    "directory": str(root),
                    "url": project["site"]["target"],
                    "root": root,
                    "project": project,
                    "settings": settings,
                    "state": state,
                    "decision": primary_decision,
                }
            )
    else:
        primary_decision = {"due": False, "reason": "no schedule configured"}
    if settings["include_competitors"]:
        for target in _competitor_targets(root, project):
            try:
                child_root, child_project, child_settings, child_state = _open(target["directory"])
                if read_document(child_root, NAME) is None:
                    continue
                host = urlsplit(child_project["site"]["target"]).hostname or ""
                decision = due_state(child_settings, host, stamp, child_state.get("last_window"))
            except (OSError, ValueError, KeyError, TypeError) as exc:
                unavailable.append(
                    {
                        "role": "competitor",
                        "url": target["url"],
                        "ok": False,
                        "error": type(exc).__name__,
                    }
                )
                continue
            if decision["due"]:
                candidates.append(
                    {
                        **target,
                        "settings": child_settings,
                        "state": child_state,
                        "decision": decision,
                    }
                )
    if not candidates:
        if unavailable:
            for item in unavailable:
                journal.note(
                    directory,
                    source="scans",
                    actor="schedule",
                    text=f"competitor {item['url']} schedule unavailable: {item['error']}",
                )
            return {
                "ok": False,
                "ran": False,
                "reason": "one or more competitor schedules are unavailable",
                "competitors_due": 0,
                "runs": unavailable,
            }
        return {
            "ok": True,
            "ran": False,
            "reason": primary_decision["reason"],
            "decision": primary_decision,
            "competitors_due": 0,
        }
    from .runtime import project_write_lock

    with project_write_lock(root):
        current_root, current_project, _, _ = _open(directory)
        if current_root != root or current_project["project_uuid"] != project["project_uuid"]:
            raise ValueError("project changed before scheduled-run admission")
        token = _acquire(root)
    if token is None:
        journal.note(
            directory,
            source="scans",
            actor="schedule",
            text="skipped: a scheduled run already holds the lock",
        )
        return {"ok": True, "ran": False, "reason": "overlap refused"}
    results: list[dict[str, Any]] = list(unavailable)
    try:
        for item in unavailable:
            journal.note(
                directory,
                source="scans",
                actor="schedule",
                text=f"competitor {item['url']} schedule unavailable: {item['error']}",
            )
        for target in candidates:
            target_root = target["root"]
            target_settings = target["settings"]
            target_state = target["state"]
            decision = target["decision"]
            child_token = None
            if target_root != root:
                with project_write_lock(target_root):
                    child_root, child_project, _, _ = _open(target["directory"])
                    if (
                        child_root != target_root
                        or child_project["project_uuid"] != target["project_uuid"]
                    ):
                        raise ValueError(
                            "competitor project changed before scheduled-run admission"
                        )
                    child_token = _acquire(target_root)
            if target_root != root and child_token is None:
                entry = {
                    "role": target["role"],
                    "url": target["url"],
                    "ok": False,
                    "scan": None,
                    "partial": False,
                    "finish_reason": None,
                    "error": "overlap refused",
                    "window": decision["window"],
                }
                results.append(entry)
                journal.note(
                    directory,
                    source="scans",
                    actor="schedule",
                    text=f"competitor {target['url']} skipped: another scheduled run holds its lock",
                )
                journal.note(
                    target["directory"],
                    source="scans",
                    actor="schedule",
                    text="competitor scan skipped: another scheduled run holds its lock",
                )
                continue
            try:
                try:
                    journal.note(
                        directory,
                        source="scans",
                        actor="schedule",
                        text=f"started {target['role']} window {decision['window']} mode {target_settings['crawl']['mode']}",
                    )
                    if target_root != root:
                        journal.note(
                            target["directory"],
                            source="scans",
                            actor="schedule",
                            text=f"started competitor scan window {decision['window']} mode {target_settings['crawl']['mode']}",
                        )
                    kwargs = _request(target_settings, target, target_state, target_root)
                    try:
                        outcome = scan(**kwargs)
                    except (KeyboardInterrupt, SystemExit, asyncio.CancelledError) as exc:
                        entry = {
                            "role": target["role"],
                            "url": target["url"],
                            "ok": False,
                            "scan": None,
                            "partial": False,
                            "finish_reason": "cancelled",
                            "error": type(exc).__name__,
                            "cancelled": True,
                            "window": decision["window"],
                        }
                        target_state["last_window"] = decision["window"]
                        target_state["last_run_at"] = _now()
                        target_state["last_result"] = [entry]
                        previous_revision = target_state.get("revision", 0)
                        target_state["revision"] = previous_revision + 1
                        write_document(
                            target_root,
                            STATE_NAME,
                            target_state,
                            expected_revision=previous_revision,
                        )
                        message = f"{target['role']} cancelled: {type(exc).__name__}"
                        journal.note(directory, source="scans", actor="schedule", text=message)
                        if target_root != root:
                            journal.note(
                                target["directory"],
                                source="scans",
                                actor="schedule",
                                text=message,
                            )
                        raise
                except Exception as exc:  # one target's failure must not hide the others
                    message = str(exc) if isinstance(exc, ValueError) else type(exc).__name__
                    outcome = {"ok": False, "error": message}
                entry = {
                    "role": target["role"],
                    "url": target["url"],
                    "ok": bool(outcome.get("ok", True)),
                    "scan": outcome.get("scan"),
                    "partial": bool(outcome.get("partial", False)),
                    "finish_reason": outcome.get("finish_reason"),
                    "error": outcome.get("error") if not outcome.get("ok", True) else None,
                    "window": decision["window"],
                }
                results.append(entry)
                if target_settings["crawl"]["slice"] and entry["partial"] and entry["scan"]:
                    target_state["resume_artifact"] = _stored_scan_path(target_root, entry["scan"])
                else:
                    target_state["resume_artifact"] = None
                target_state["last_window"] = decision["window"]
                target_state["last_run_at"] = _now()
                stored_entry = {**entry, "scan": _stored_scan_path(target_root, entry["scan"])}
                target_state["last_result"] = [stored_entry]
                previous_revision = target_state.get("revision", 0)
                target_state["revision"] = previous_revision + 1
                write_document(
                    target_root,
                    STATE_NAME,
                    target_state,
                    expected_revision=previous_revision,
                )
                journal.note(
                    directory,
                    source="scans",
                    actor="schedule",
                    text=f"{target['role']} {'finished' if entry['ok'] else 'failed'}"
                    + (
                        f" partial={entry['partial']}"
                        if entry["ok"]
                        else f" error={entry['error']}"
                    ),
                )
                if target_root != root:
                    journal.note(
                        target["directory"],
                        source="scans",
                        actor="schedule",
                        text=f"competitor {'finished' if entry['ok'] else 'failed'}"
                        + (
                            f" partial={entry['partial']}"
                            if entry["ok"]
                            else f" error={entry['error']}"
                        ),
                    )
            finally:
                if child_token is not None:
                    _release(child_token)
    finally:
        _release(token)
    return {
        "ok": all(item["ok"] for item in results),
        "ran": True,
        "window": primary_decision.get("window") or candidates[0]["decision"]["window"],
        "competitors_due": sum(item["role"] == "competitor" for item in candidates),
        "runs": results,
    }
