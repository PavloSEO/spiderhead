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
from datetime import datetime, timezone
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


def list_bindings(directory: str | Path) -> dict[str, Any]:
    """Return the project's bindings; a project without sources.json has none."""
    root, _ = _load(directory)
    bindings = [_view(item) for item in _read(root)]
    return {"ok": True, "path": str(root), "count": len(bindings), "bindings": bindings}


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
