"""Create and read bounded, portable project workspaces without executing project content."""

from __future__ import annotations

import json
import math
import os
import re
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from seohead.recon.net import normalize_domain, normalize_url
from seohead.storage.history import list_scans

PROJECT_FORMAT = "seohead.project.v1"
PROJECT_VERSION = 1
_MAX_JSON_BYTES = 1_048_576
_REFERENCE = re.compile(r"[a-z][a-z0-9._/-]{0,127}\Z")
UTC = timezone.utc


def _directory(path: str | Path, label: str) -> Path:
    if not isinstance(path, (str, Path)) or not str(path):
        raise ValueError(f"{label} is required")
    value = Path(path)
    if value.is_symlink() or not value.is_dir():
        raise ValueError(f"{label} must be an existing non-symlink directory")
    return value.resolve()


def _target(value: str) -> str:
    if not isinstance(value, str) or len(value) > 2048:
        raise ValueError("target must be a bounded absolute HTTP(S) URL")
    from urllib.parse import urlsplit, urlunsplit

    raw = urlsplit(value)
    if raw.username or raw.password or raw.query or raw.fragment:
        raise ValueError(
            "target site identity must not include credentials, a query, or a fragment"
        )
    normalized = normalize_url(value)
    if not normalized:
        raise ValueError("target must be a public HTTP(S) site identity without credentials")
    parts = urlsplit(normalized)
    if parts.query or parts.fragment or not parts.hostname:
        raise ValueError("target site identity must not include a query or fragment")
    host = normalize_domain(parts.hostname)
    if not host:
        raise ValueError("target must have a valid public hostname")
    try:
        port = f":{parts.port}" if parts.port else ""
    except ValueError as exc:
        raise ValueError("target must have a valid HTTP(S) port") from exc
    return urlunsplit((parts.scheme.lower(), host + port, parts.path or "/", "", ""))


def _site(target: str, label: str | None) -> dict[str, str | None]:
    from urllib.parse import urlsplit

    host = normalize_domain(urlsplit(target).hostname or "")
    if not host:
        raise ValueError("target must have a valid public hostname")
    if label is not None and (type(label) is not str or not label.strip() or len(label) > 128):
        raise ValueError("label is a bounded human project label, not a host override")
    return {"host": host, "target": target, "label": label}


def _references(value: list[str] | None, label: str) -> list[str]:
    if value is None:
        return []
    if not isinstance(value, list) or len(value) > 100:
        raise ValueError(f"{label} must be a list of at most 100 references")
    if any(
        type(item) is not str or not _REFERENCE.fullmatch(item) or ".." in item for item in value
    ):
        raise ValueError(f"{label} entries must be bounded relative identifiers")
    return list(dict.fromkeys(value))


def _facts(value: list[dict[str, Any]] | None) -> list[dict[str, Any]]:
    if value is None:
        return []
    if not isinstance(value, list) or len(value) > 100:
        raise ValueError("facts must be a list of at most 100 provenance-backed values")
    result = []
    for item in value:
        if not isinstance(item, dict) or set(item) != {
            "name",
            "value",
            "provenance",
            "observed_at",
        }:
            raise ValueError("each fact requires name, value, provenance, and observed_at")
        name, fact, provenance, observed_at = (
            item["name"],
            item["value"],
            item["provenance"],
            item["observed_at"],
        )
        if (
            type(name) is not str
            or not re.fullmatch(r"[a-z][a-z0-9_]{0,63}", name)
            or type(provenance) is not str
            or not provenance
            or len(provenance) > 512
            or type(fact) not in {str, int, float, bool}
            or (type(fact) is float and not math.isfinite(fact))
            or (isinstance(fact, str) and len(fact) > 2048)
        ):
            raise ValueError("fact fields must be bounded scalar values with provenance")
        if observed_at is not None:
            if type(observed_at) is not str:
                raise ValueError("fact observed_at must be RFC3339 UTC or null when unknown")
            try:
                parsed = datetime.fromisoformat(observed_at.replace("Z", "+00:00"))
            except ValueError as exc:
                raise ValueError(
                    "fact observed_at must be RFC3339 UTC or null when unknown"
                ) from exc
            if parsed.tzinfo is None or parsed.utcoffset() != UTC.utcoffset(parsed):
                raise ValueError("fact observed_at must be RFC3339 UTC or null when unknown")
        if any(
            token in name.lower()
            for token in (
                "secret",
                "token",
                "password",
                "cookie",
                "authorization",
                "api_key",
                "apikey",
                "private_key",
                "credential",
            )
        ):
            raise ValueError("facts must not store credentials or secrets")
        result.append(
            {"name": name, "value": fact, "provenance": provenance, "observed_at": observed_at}
        )
    return result


def _document(
    target: str,
    label: str | None,
    facts: list[dict[str, Any]],
    templates: list[str],
    profiles: list[str],
) -> dict[str, Any]:
    return {
        "format": PROJECT_FORMAT,
        "version": PROJECT_VERSION,
        "project_uuid": str(uuid.uuid4()),
        "created_at": datetime.now(UTC).isoformat().replace("+00:00", "Z"),
        "site": _site(target, label),
        "facts": facts,
        "template_references": templates,
        "profile_references": profiles,
        "artifact_directories": {"scans": "scans", "reports": "reports", "log": "log.md"},
    }


def _write_new(path: Path, content: str) -> None:
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            descriptor = -1
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
    finally:
        if descriptor != -1:
            os.close(descriptor)


def _publish_manifest(project: Path, document: dict[str, Any]) -> None:
    """Publish the success marker last, atomically and without replacing a prior manifest."""
    staged = project / ".project.json.stage"
    _write_new(staged, json.dumps(document, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
    try:
        os.link(staged, project / "project.json", follow_symlinks=False)
        from seohead.core.filesystem import fsync_directory

        fsync_directory(project)
    finally:
        staged.unlink(missing_ok=True)


def create_project(
    directory: str | Path,
    target: str,
    *,
    label: str | None = None,
    facts: list[dict[str, Any]] | None = None,
    template_references: list[str] | None = None,
    profile_references: list[str] | None = None,
) -> dict[str, Any]:
    """Create one new project directory; existing paths and files are never replaced."""
    root = Path(directory)
    if ".." in root.parts:
        raise ValueError("project directory must not contain parent traversal")
    parent = _directory(root.parent, "project parent")
    if not root.name or root.name in {".", ".."} or os.path.lexists(root):
        raise ValueError("project directory already exists or is not a new path")
    target = _target(target)
    document = _document(
        target,
        label,
        _facts(facts),
        _references(template_references, "template_references"),
        _references(profile_references, "profile_references"),
    )
    project = parent / root.name
    project.mkdir(mode=0o700)
    for name in ("scans", "reports"):
        (project / name).mkdir(mode=0o700)
    _write_new(project / "log.md", "# Project log\n\nProject created; no audit work has run.\n")
    _publish_manifest(project, document)
    return open_project(project)


def _load(directory: str | Path) -> tuple[Path, dict[str, Any]]:
    root = _directory(directory, "project directory")
    for name in ("scans", "reports"):
        child = root / name
        if child.is_symlink() or not child.is_dir():
            raise ValueError(f"project {name} directory is missing or unsafe")
    log = root / "log.md"
    if log.is_symlink() or not log.is_file():
        raise ValueError("project log is missing or unsafe")
    manifest = root / "project.json"
    if manifest.is_symlink() or not manifest.is_file() or manifest.stat().st_size > _MAX_JSON_BYTES:
        raise ValueError("project.json is missing, unsafe, or exceeds its byte limit")
    try:
        document = json.loads(manifest.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise ValueError("project.json is not valid JSON") from exc
    if not isinstance(document, dict) or set(document) != {
        "format",
        "version",
        "project_uuid",
        "created_at",
        "site",
        "facts",
        "template_references",
        "profile_references",
        "artifact_directories",
    }:
        raise ValueError("project.json has an unsupported shape")
    if (
        document["format"] != PROJECT_FORMAT
        or type(document["version"]) is not int
        or document["version"] != PROJECT_VERSION
    ):
        raise ValueError("project.json format or version is unsupported")
    if type(document["project_uuid"]) is not str or type(document["created_at"]) is not str:
        raise ValueError("project UUID or creation time is invalid")
    try:
        uuid.UUID(document["project_uuid"])
        created_at = datetime.fromisoformat(document["created_at"].replace("Z", "+00:00"))
    except (TypeError, ValueError) as exc:
        raise ValueError("project UUID or creation time is invalid") from exc
    if created_at.tzinfo is None or created_at.utcoffset() != UTC.utcoffset(created_at):
        raise ValueError("project creation time must be RFC3339 UTC")
    raw_target = document["site"].get("target") if isinstance(document["site"], dict) else ""
    if not isinstance(raw_target, str):
        raise ValueError("project site identity is invalid")
    target = _target(raw_target)
    try:
        expected_site = _site(target, document["site"]["label"])
    except (KeyError, TypeError, ValueError) as exc:
        raise ValueError("project site identity is invalid") from exc
    if (
        not isinstance(document["site"], dict)
        or set(document["site"]) != {"host", "target", "label"}
        or document["site"] != expected_site
    ):
        raise ValueError("project site identity is invalid")
    _facts(document["facts"])
    _references(document["template_references"], "template_references")
    _references(document["profile_references"], "profile_references")
    if document["artifact_directories"] != {
        "scans": "scans",
        "reports": "reports",
        "log": "log.md",
    }:
        raise ValueError("project artifact references must remain portable relative paths")
    return root, document


def open_project(directory: str | Path, *, expected_site: str | None = None) -> dict[str, Any]:
    """Open a validated project without resolving or executing template references."""
    root, document = _load(directory)
    if expected_site is not None and normalize_domain(expected_site) != document["site"]["host"]:
        raise ValueError("project site identity conflicts with the requested site")
    from .coverage import coverage_status
    from .runtime import aggregate_coverage

    return {
        "ok": True,
        "project": _project_presentation(root, document),
        "path": str(root),
        "checklist": aggregate_coverage(str(root), coverage_status(root)),
    }


def project_status(
    directory: str | Path, *, _coverage_views: dict | None = None, _verify_evidence: bool = True
) -> dict[str, Any]:
    """Report scan history, per-site coverage and bounded operational status."""
    from .coverage import coverage_status
    from .runtime import aggregate_coverage, preparation_status

    root, document = _load(directory)
    views = {} if _coverage_views is None else _coverage_views
    if str(root) not in views:
        views[str(root)] = coverage_status(root, _verify_evidence=_verify_evidence)
    scans = list_scans(root / "scans")
    running_scan = _running_scan(root, scans)
    latest = scans["items"][0] if scans["items"] else None
    latest_completeness = None
    if latest is not None:
        partial = latest["lifecycle"] != "finished" or bool(
            latest["crawl_partial"] or latest["corpus_partial"]
        )
        latest_completeness = {
            "state": "partial" if partial else "complete",
            "crawl_partial": latest["crawl_partial"],
            "corpus_partial": latest["corpus_partial"],
            "reason": (
                latest["finish_reason"] or f"scan lifecycle is {latest['lifecycle']}"
                if partial
                else None
            ),
        }
    scan_errors = scans.get("errors", [])
    checklist = aggregate_coverage(
        str(root), views[str(root)], _coverage_views=views, _verify_evidence=_verify_evidence
    )
    agent_state = _agent_state(root)
    storage = _storage_measurement(root)
    presentation = _project_presentation(root, document)
    return {
        "ok": True,
        "project": {
            "site": presentation["site"],
            "settings_revision": presentation["settings_revision"],
            "template_references": document["template_references"],
            "profile_references": document["profile_references"],
        },
        "scans": scans,
        "summary": {
            "running_scan": running_scan,
            "open_tasks": _open_tasks(views[str(root)]),
            "latest_scan": (
                {
                    "uuid": latest["uuid"],
                    "state": latest["lifecycle"],
                    "completeness": latest_completeness,
                }
                if latest is not None
                else {
                    "uuid": None,
                    "state": None,
                    "completeness": None,
                    "reason": "no retained scan is available",
                }
            ),
            "folder_health": {
                "state": "degraded" if scan_errors else "ok",
                "reason": (
                    f"{len(scan_errors)} retained scan item(s) could not be read"
                    if scan_errors
                    else None
                ),
            },
        },
        "agent_state": agent_state,
        "storage": storage,
        "checklist": checklist,
        "preparation": preparation_status(str(root)),
    }


def _settings(root: Path) -> dict[str, Any]:
    try:
        from .project_settings import read_settings
    except ImportError:
        _root, document = _load(root)
        return {"revision": 0, "settings": {"label": document["site"]["label"]}}
    return read_settings(root)


def _project_presentation(root: Path, document: dict[str, Any]) -> dict[str, Any]:
    settings = _settings(root)
    return {
        **document,
        "site": {**document["site"], "label": settings["settings"].get("label")},
        "settings_revision": settings["revision"],
    }


def _agent_state(root: Path) -> dict[str, Any]:
    try:
        from .action_log import latest_agent_activity

        activity = latest_agent_activity(root)
    except (ImportError, OSError, ValueError) as exc:
        return {
            "last_seen_at": None,
            "connected": None,
            "heartbeat_source": None,
            "activity_source": None,
            "reason": f"agent activity is unavailable ({type(exc).__name__})",
        }
    last_seen = activity.get("last_seen_at")
    source = activity.get("source") if last_seen is not None else None
    return {
        "last_seen_at": last_seen,
        "connected": None,
        "heartbeat_source": activity.get("heartbeat_source") if last_seen is not None else None,
        "activity_source": source,
        "reason": (
            "last activity is recorded; current connection is not established"
            if last_seen is not None
            else "no agent activity is recorded"
        ),
    }


def _storage_measurement(root: Path) -> dict[str, Any]:
    try:
        from .status_measurement import read_storage_measurement

        return read_storage_measurement(root)
    except (ImportError, OSError, ValueError) as exc:
        return {
            "relative_path": "scans",
            "total_bytes": None,
            "scan_count": None,
            "measured_at": None,
            "state": "unavailable",
            "reason": f"project storage measurement is unavailable ({type(exc).__name__})",
        }


def _open_tasks(checklist: dict[str, Any]) -> dict[str, Any]:
    """Count current, applicable operator-authored checklist tasks only."""
    if checklist.get("state") != "initialized" or not isinstance(checklist.get("items"), list):
        return {
            "count": None,
            "state": "unavailable",
            "scope": "primary_project_custom_checklist",
            "reason": "the project checklist is not initialized",
        }
    tasks = [
        item
        for item in checklist["items"]
        if isinstance(item, dict)
        and isinstance(item.get("id"), str)
        and item["id"].startswith("custom:")
        and not item.get("complete")
        and not item.get("stale")
        and item.get("applicability") not in {"excluded", "not_agreed"}
    ]
    count = len(tasks)
    return {
        "count": count,
        "state": "measured" if count else "no_active_tasks",
        "scope": "primary_project_custom_checklist",
        "reason": None,
    }


def _running_scan(root: Path, scans: dict[str, Any]) -> dict[str, Any]:
    """Project the active collector without treating a missing observer as idle."""
    from .run_observation import NAME

    if not (root / NAME).is_file():
        active_scan = next(
            (item for item in scans.get("items", []) if item.get("lifecycle") == "running"), None
        )
        if active_scan is not None:
            return {
                "running": True,
                "id": active_scan["uuid"],
                "progress": None,
                "reason": "collector progress has not been recorded",
            }
        return {
            "running": None,
            "id": None,
            "progress": None,
            "reason": "no collector observation is recorded",
        }
    try:
        from .run_observation import status as run_status

        observed = run_status(root, limit=1)
    except (OSError, ValueError) as exc:
        return {
            "running": None,
            "id": None,
            "progress": None,
            "reason": f"collector status is unavailable: {type(exc).__name__}",
        }
    if observed["active_total"] == 0:
        active_scan = next(
            (item for item in scans.get("items", []) if item.get("lifecycle") == "running"), None
        )
        if active_scan is not None:
            return {
                "running": True,
                "id": active_scan["uuid"],
                "progress": None,
                "reason": "collector progress has not been recorded",
            }
        return {"running": False, "id": None, "progress": None, "reason": None}
    active = next((item for item in observed["items"] if item["state"] == "running"), None)
    if active is None:
        return {
            "running": None,
            "id": None,
            "progress": None,
            "reason": "active collector details are unavailable",
        }
    counters = active.get("counters") or {}
    return {
        "running": True,
        "id": active.get("id"),
        "progress": {
            "fetched": counters.get("fetched"),
            "queued": counters.get("queued"),
            "inflight": counters.get("inflight"),
            "excluded": counters.get("excluded"),
            "sampled_at": (active.get("telemetry") or {}).get("sampled_at"),
        },
        "reason": None,
    }
