"""Persist a bounded scan-directory size observation for cheap project status reads."""

from __future__ import annotations

import os
import stat
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from .runtime import read_document, write_document
from .workspace import _load

FILE = "project-measurement.json"
FORMAT = "seohead.project-measurement.v1"
MAX_ENTRIES = 10_000
_MISSING = {
    "state": "unavailable",
    "relative_path": "scans",
    "total_bytes": None,
    "scan_count": None,
    "measured_at": None,
    "reason": "measurement has not been recorded",
}


def _document(directory: str | Path) -> tuple[Path, dict[str, Any], dict[str, Any] | None]:
    root, project = _load(directory)
    current = read_document(root, FILE)
    if current is not None and (
        set(current) != {"format", "version", "project_uuid", "revision", "storage"}
        or current["format"] != FORMAT
        or type(current["version"]) is not int
        or current["version"] != 1
        or current["project_uuid"] != project["project_uuid"]
        or type(current["revision"]) is not int
        or current["revision"] < 1
    ):
        raise ValueError("project measurement has an unsupported shape")
    if current is not None:
        storage = current["storage"]
        if not isinstance(storage, dict) or set(storage) != {
            "state",
            "relative_path",
            "total_bytes",
            "scan_count",
            "measured_at",
            "reason",
        }:
            raise ValueError("project storage measurement has an unsupported shape")
        if (
            storage["state"] not in {"available", "unavailable"}
            or storage["relative_path"] != "scans"
        ):
            raise ValueError("project storage measurement state is invalid")
        if storage["measured_at"] is not None and type(storage["measured_at"]) is not str:
            raise ValueError("project storage measurement timestamp is invalid")
        if storage["reason"] is not None and type(storage["reason"]) is not str:
            raise ValueError("project storage measurement reason is invalid")
        for name in ("total_bytes", "scan_count"):
            value = storage[name]
            if value is not None and (type(value) is not int or value < 0):
                raise ValueError(f"project storage {name} must be a nonnegative integer or null")
        if storage["state"] == "available" and (
            storage["total_bytes"] is None
            or storage["scan_count"] is None
            or storage["reason"] is not None
        ):
            raise ValueError("available project storage measurement is incomplete")
        if storage["state"] == "unavailable" and (
            storage["total_bytes"] is not None
            or storage["scan_count"] is not None
            or not storage["reason"]
        ):
            raise ValueError("unavailable project storage measurement requires a reason")
    return root, project, current


def read_storage_measurement(directory: str | Path) -> dict[str, Any]:
    """Return the persisted storage observation without walking project files."""
    _root, _project, current = _document(directory)
    return dict(current["storage"]) if current else dict(_MISSING)


def refresh_storage_measurement(directory: str | Path) -> dict[str, Any]:
    """Measure direct regular files in scans/ once after a scan reaches terminal state."""
    root, project, current = _document(directory)
    measured_at = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
    storage = {**_MISSING, "measured_at": measured_at, "reason": None}
    scans = root / "scans"
    total_bytes = 0
    scan_count = 0
    try:
        with os.scandir(scans) as entries:
            for number, entry in enumerate(entries, 1):
                if number > MAX_ENTRIES:
                    raise ValueError(f"scans directory exceeds {MAX_ENTRIES} entries")
                info = entry.stat(follow_symlinks=False)
                if stat.S_ISLNK(info.st_mode):
                    raise ValueError("scans directory contains a symbolic link")
                if stat.S_ISDIR(info.st_mode):
                    raise ValueError("nested scan directories are not included in this measurement")
                if not stat.S_ISREG(info.st_mode):
                    raise ValueError("scans directory contains a non-regular entry")
                total_bytes += info.st_size
                if entry.name.lower().endswith((".sqlite", ".sqlite3", ".db")):
                    scan_count += 1
    except OSError as exc:
        storage["reason"] = f"scan directory could not be measured: {type(exc).__name__}"
    except ValueError as exc:
        storage["reason"] = str(exc)
    else:
        storage.update(state="available", total_bytes=total_bytes, scan_count=scan_count)
    document = {
        "format": FORMAT,
        "version": 1,
        "project_uuid": project["project_uuid"],
        "revision": (current["revision"] if current else 0) + 1,
        "storage": storage,
    }
    write_document(root, FILE, document, expected_revision=current["revision"] if current else 0)
    return storage
