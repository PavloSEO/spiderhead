"""Portable project archive: one zip with a SHA-256 manifest, restorable on any OS.

Project-local archive set: project and settings metadata, bounded action/event histories,
inbox/coverage/export stores, top-level SQLite stores, and scans/ and reports/ artifacts.
"""

from __future__ import annotations

import contextlib
import hashlib
import json
import os
import re
import shutil
import sqlite3
import stat
import tempfile
import zipfile
from datetime import datetime
from pathlib import Path
from typing import Any

from seohead import __version__
from seohead.projects.workspace import PROJECT_FORMAT, UTC, _directory, _load, open_project

ARCHIVE_FORMAT = "seohead.project-archive.v1"
ARCHIVE_VERSION = 1
MANIFEST = "manifest.json"
_FILES = (
    "project.json",
    "project-settings.json",
    "settings-proposals.json",
    "log.md",
    "events.jsonl",
    "actions.jsonl",
    "inbox.json",
    "coverage.json",
    "exports.json",
)
_DIRECTORIES = ("scans", "reports")
# Name-based exclusion: credentials are never archived, whatever the store that holds them.
_CREDENTIAL = re.compile(
    r"secret|token|passw|cookie|credential|api[_-]?key|private[_-]?key|\.env|netrc|\.pem|\.key",
    re.IGNORECASE,
)
_SQLITE = (".sqlite", ".db")


def _credential_name(name: str) -> bool:
    return _CREDENTIAL.search(name) is not None


def _plan(root: Path) -> tuple[list[tuple[str, Path]], list[dict[str, str]]]:
    """Return (archived entries, skipped entries) relative to the project root."""
    included: list[tuple[str, Path]] = []
    skipped: list[dict[str, str]] = []

    def take(rel: str, path: Path) -> None:
        if _credential_name(rel):
            skipped.append({"path": rel, "reason": "credential-like name"})
        elif path.is_symlink() or not path.is_file():
            skipped.append({"path": rel, "reason": "not a regular file"})
        elif rel.endswith(("-wal", "-shm")):
            skipped.append({"path": rel, "reason": "SQLite companion; snapshot is taken instead"})
        else:
            included.append((rel, path))

    for entry in sorted(root.iterdir()):
        if entry.name in _FILES or (entry.name.endswith(_SQLITE) and entry.is_file()):
            take(entry.name, entry)
        elif entry.name not in _DIRECTORIES and not entry.name.startswith("."):
            skipped.append({"path": entry.name, "reason": "outside the archive set"})
    for name in _DIRECTORIES:
        base = root / name
        for dirpath, dirnames, filenames in os.walk(base, followlinks=False):
            dirnames[:] = sorted(d for d in dirnames if not d.startswith("."))
            for filename in sorted(filenames):
                if filename.startswith("."):
                    continue
                path = Path(dirpath) / filename
                take(path.relative_to(root).as_posix(), path)
    return included, skipped


def _sha256(path: Path) -> tuple[str, int]:
    digest, size = hashlib.sha256(), 0
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1 << 20), b""):
            digest.update(chunk)
            size += len(chunk)
    return digest.hexdigest(), size


def _snapshot(source: Path, destination: Path) -> None:
    """Copy a live SQLite store through the backup API so WAL contents are included."""
    with (
        contextlib.closing(
            sqlite3.connect(source.resolve().as_uri() + "?mode=ro", uri=True)
        ) as src,
        contextlib.closing(sqlite3.connect(destination)) as dst,
    ):
        src.backup(dst)


def _validate_project_stores(root: Path) -> None:
    """Consume each present local store through its authoritative read validator."""
    from seohead.projects.workspace import _load

    _, project = _load(root)
    if (root / "project-settings.json").exists():
        from seohead.projects.project_settings import read_settings

        read_settings(root)
    if (root / "settings-proposals.json").exists():
        from seohead.projects.project_settings import list_proposals

        list_proposals(root, offset=0, limit=1)
    if (root / "events.jsonl").exists():
        from seohead.projects.event_log import page

        page(root, offset=0, limit=1)
    if (root / "actions.jsonl").exists():
        from seohead.projects.action_log import page

        page(root, offset=0, limit=1)
    if (root / "coverage.json").exists():
        from seohead.projects.coverage import _read

        _read(root, project)
    if (root / "inbox.json").exists():
        from seohead.projects.inbox import list_entries

        list_entries(root, consumer="archive-validation", offset=0, limit=1)
    if (root / "exports.json").exists():
        from seohead.projects.exports import exports_page

        exports_page(root, offset=0, limit=1)


def archive_project(
    directory: str | Path, out: str | Path, *, dry_run: bool = False
) -> dict[str, Any]:
    """Write one zip for a validated project; dry_run only reports the plan and size estimate."""
    root, document = _load(directory)
    _validate_project_stores(root)
    target = Path(out)
    if not target.name or ".." in target.parts:
        raise ValueError("archive output must be a plain file path")
    parent = _directory(target.parent, "archive output parent")
    included, skipped = _plan(root)
    estimate = sum(path.stat().st_size for _, path in included)
    plan = {
        "ok": True,
        "dry_run": dry_run,
        "files": [rel for rel, _ in included],
        "skipped": skipped,
        "estimated_bytes": estimate,
    }
    if dry_run:
        return plan
    if os.path.lexists(target):
        raise ValueError("archive output already exists; choose a new file")

    partial = parent / f".{target.name}.partial"
    with tempfile.TemporaryDirectory(prefix=".seohead-archive-", dir=parent) as scratch:
        staged: list[tuple[str, Path]] = []
        for rel, path in included:
            copy = Path(scratch) / rel.replace("/", "__")
            if rel.endswith(_SQLITE):
                _snapshot(path, copy)
            else:
                shutil.copyfile(path, copy)
            staged.append((rel, copy))
        entries = []
        for rel, copy in staged:
            digest, size = _sha256(copy)
            entries.append({"path": rel, "size": size, "sha256": digest})
        manifest = {
            "format": ARCHIVE_FORMAT,
            "archive_version": ARCHIVE_VERSION,
            "project_format": PROJECT_FORMAT,
            "seohead_version": __version__,
            "project_uuid": document["project_uuid"],
            "created_at": datetime.now(UTC).isoformat().replace("+00:00", "Z"),
            "directories": list(_DIRECTORIES),
            "files": entries,
        }
        with zipfile.ZipFile(partial, "x", compression=zipfile.ZIP_DEFLATED) as archive:
            archive.writestr(MANIFEST, json.dumps(manifest, ensure_ascii=False, indent=2))
            for rel, copy in staged:
                archive.write(copy, rel)
    try:
        os.link(partial, target, follow_symlinks=False)
    finally:
        partial.unlink(missing_ok=True)
    plan.update({"dry_run": False, "archive": str(target), "file_count": len(entries)})
    return plan


def _member_name(name: str) -> str:
    parts = name.split("/")
    if (
        not name
        or name.startswith("/")
        or "\\" in name
        or ":" in parts[0]
        or any(part in {"", ".", ".."} for part in parts)
    ):
        raise ValueError(f"archive member is unsafe: {name!r}")
    return name


def restore_project(archive: str | Path, to: str | Path) -> dict[str, Any]:
    """Verify every member against the manifest, then publish the project at a new path."""
    source = Path(archive)
    if source.is_symlink() or not source.is_file():
        raise ValueError("archive must be an existing regular file")
    dest = Path(to)
    if not dest.name or ".." in dest.parts or os.path.lexists(dest):
        raise ValueError("restore target must be a new path")
    parent = _directory(dest.parent, "restore parent")

    with zipfile.ZipFile(source) as zf:
        infos = zf.infolist()
        names = [_member_name(info.filename) for info in infos]
        if len(set(names)) != len(names):
            raise ValueError("archive has duplicate members")
        for info in infos:
            if stat.S_ISLNK(info.external_attr >> 16):
                raise ValueError(f"archive member is a symlink: {info.filename!r}")
        manifest = json.loads(zf.read(MANIFEST).decode("utf-8"))
        if (
            not isinstance(manifest, dict)
            or manifest.get("format") != ARCHIVE_FORMAT
            or manifest.get("archive_version") != ARCHIVE_VERSION
            or manifest.get("project_format") != PROJECT_FORMAT
            or manifest.get("directories") != list(_DIRECTORIES)
            or not isinstance(manifest.get("files"), list)
        ):
            raise ValueError("archive manifest is unsupported")
        listed = {entry["path"]: entry for entry in manifest["files"]}
        members = {name for name in names if name != MANIFEST}
        if members != set(listed) or len(listed) != len(manifest["files"]):
            raise ValueError("archive members do not match the manifest")

        staging = Path(tempfile.mkdtemp(prefix=".seohead-restore-", dir=parent))
        try:
            for name in _DIRECTORIES:
                (staging / name).mkdir(mode=0o700)
            for info in infos:
                if info.filename == MANIFEST:
                    continue
                expected = listed[info.filename]
                target = staging / info.filename
                target.parent.mkdir(parents=True, exist_ok=True)
                digest, size = hashlib.sha256(), 0
                with zf.open(info) as src, target.open("xb") as dst:
                    for chunk in iter(lambda: src.read(1 << 20), b""):
                        digest.update(chunk)
                        size += len(chunk)
                        dst.write(chunk)
                if size != expected["size"] or digest.hexdigest() != expected["sha256"]:
                    raise ValueError(f"archive member failed verification: {info.filename!r}")
            _validate_project_stores(staging)
            if os.path.lexists(dest):
                raise ValueError("restore target appeared during restore; nothing was replaced")
            os.rename(staging, dest)
        finally:
            shutil.rmtree(staging, ignore_errors=True)
    return {"ok": True, "restored": str(dest), "project": open_project(dest)["project"]}
