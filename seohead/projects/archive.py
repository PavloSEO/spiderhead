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
from datetime import date, datetime
from pathlib import Path
from typing import Any

from seohead import __version__
from seohead.projects.workspace import PROJECT_FORMAT, UTC, _directory, _load, open_project

ARCHIVE_FORMAT = "seohead.project-archive.v1"
ARCHIVE_VERSION = 1
MANIFEST = "manifest.json"
_FILES = (
    "project.json",
    "log.md",
    "inbox.json",
    "coverage.json",
    "events.jsonl",
    "sources.json",
    "finding-views.json",
    "project-settings.json",
    "settings-proposals.json",
    "actions.jsonl",
    "schedule.json",
    "schedule-state.json",
    "exports.json",
)
_PROJECT_STATE = frozenset(_FILES) - {"project.json", "log.md", "inbox.json", "coverage.json"}
_DIRECTORIES = ("scans", "reports")
# Name-based exclusion: credentials are never archived, whatever the store that holds them.
_CREDENTIAL = re.compile(
    r"secret|token|passw|cookie|credential|api[_-]?key|private[_-]?key|\.env|netrc|\.pem|\.key",
    re.IGNORECASE,
)
_SQLITE = (".sqlite", ".db")


def _credential_name(name: str) -> bool:
    return _CREDENTIAL.search(name) is not None


def _scan_timestamp(path: Path) -> tuple[str, str] | None:
    """Read a retained scan's stable age key without accepting arbitrary databases."""
    try:
        with contextlib.closing(
            sqlite3.connect(path.resolve().as_uri() + "?mode=ro", uri=True)
        ) as con:
            row = con.execute(
                "SELECT scan_uuid,COALESCE(NULLIF(finished_at,''),created_at) "
                "FROM scan WHERE singleton=1"
            ).fetchone()
    except sqlite3.Error:
        return None
    return (row[1] or "", row[0] or "") if row else None


def _validate_project_state(root: Path, rel: str) -> None:
    """Run each retained state file through its bounded, read-only project validator."""
    if rel == "events.jsonl":
        from seohead.projects.event_log import page

        page(root, limit=1)
    elif rel == "sources.json":
        from seohead.projects.bindings import list_bindings

        list_bindings(root)
    elif rel == "finding-views.json":
        from seohead.projects.finding_views import list_views

        list_views(root)
    elif rel == "project-settings.json":
        from seohead.projects.project_settings import read_settings

        read_settings(root)
    elif rel == "settings-proposals.json":
        from seohead.projects.project_settings import list_proposals

        list_proposals(root, limit=1)
    elif rel == "actions.jsonl":
        from seohead.projects.action_log import _parse, _path

        path = _path(root)
        if path.is_symlink():
            raise ValueError("actions.jsonl must be a regular project-local file")
        if path.exists():
            with path.open("r", encoding="utf-8") as stream:
                _parse(stream)
    elif rel in {"schedule.json", "schedule-state.json"}:
        from seohead.projects import schedule

        _schedule_root, project, settings, state = schedule._open(root)
        schedule.validate(settings)
        if (
            type(settings.get("revision")) is not int
            or settings["revision"] < 0
            or not isinstance(state, dict)
            or set(state)
            != {
                "format",
                "project_uuid",
                "revision",
                "last_window",
                "last_run_at",
                "last_result",
                "resume_artifact",
            }
            or state["format"] != schedule.STATE_FORMAT
            or state["project_uuid"] != project["project_uuid"]
            or type(state["revision"]) is not int
            or state["revision"] < 0
            or (state["last_window"] is not None and type(state["last_window"]) is not str)
            or (state["last_run_at"] is not None and type(state["last_run_at"]) is not str)
            or (
                state["last_result"] is not None
                and (
                    not isinstance(state["last_result"], list)
                    or any(not isinstance(item, dict) for item in state["last_result"])
                )
            )
            or (
                state["resume_artifact"] is not None
                and (
                    type(state["resume_artifact"]) is not str
                    or not state["resume_artifact"]
                    or len(state["resume_artifact"]) > 8192
                )
            )
            or (state["last_result"] is not None and len(state["last_result"]) > 100)
        ):
            raise ValueError("schedule-state.json has an unsupported shape")
        for field in ("last_window", "last_run_at"):
            value = state[field]
            if value is None:
                continue
            if field == "last_window":
                try:
                    date.fromisoformat(value)
                except (TypeError, ValueError) as exc:
                    raise ValueError("schedule-state.json last_window date is invalid") from exc
                continue
            try:
                stamp = datetime.fromisoformat(value.replace("Z", "+00:00"))
            except (AttributeError, ValueError) as exc:
                raise ValueError(f"schedule-state.json {field} timestamp is invalid") from exc
            if stamp.tzinfo is None or stamp.utcoffset() != UTC.utcoffset(stamp):
                raise ValueError(f"schedule-state.json {field} timestamp must be UTC")


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
        if (root / "actions.jsonl").is_symlink():
            raise ValueError("actions.jsonl must be a regular project-local file")
        _validate_project_state(root, "actions.jsonl")
    if (root / "coverage.json").exists():
        from seohead.projects.coverage import _read

        _read(root, project)
    if (root / "inbox.json").exists():
        from seohead.projects.inbox import list_entries

        list_entries(root, consumer="archive-validation", offset=0, limit=1)
    if (root / "exports.json").exists():
        from seohead.projects.exports import exports_page

        exports_page(root, offset=0, limit=1)


def _plan(
    root: Path, *, keep_newest_scans: int | None = None
) -> tuple[list[tuple[str, Path]], list[dict[str, str]]]:
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
    for rel, _path in included:
        if rel in _PROJECT_STATE:
            _validate_project_state(root, rel)
    if keep_newest_scans is not None:
        scans = [
            (rel, path, stamp)
            for rel, path in included
            if rel.startswith("scans/")
            and rel.lower().endswith(_SQLITE)
            and (stamp := _scan_timestamp(path)) is not None
        ]
        newest = {
            rel
            for rel, _path, _stamp in sorted(scans, key=lambda item: (item[2], item[0]))[
                -keep_newest_scans:
            ]
        }
        omitted = {rel for rel, _path, _stamp in scans if rel not in newest}
        included = [(rel, path) for rel, path in included if rel not in omitted]
        skipped.extend(
            {"path": rel, "reason": f"outside keep_newest_scans={keep_newest_scans}"}
            for rel in sorted(omitted)
        )
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


def _exclude_html_bodies(path: Path, *, older_than_days: int | None = None) -> int:
    """Remove stored page documents from an archive snapshot while preserving evidence tables."""
    with sqlite3.connect(path) as con:
        tables = {
            row[0] for row in con.execute("SELECT name FROM sqlite_master WHERE type='table'")
        }
        if not {"scan", "documents", "responses", "bodies"} <= tables:
            return 0
        header = con.execute("SELECT format_version FROM scan WHERE singleton=1").fetchone()
        if header is None or header[0] not in {"scan.v1", "scan.v2"}:
            return 0
        before = con.execute("SELECT COUNT(*) FROM bodies").fetchone()[0]
        if older_than_days is None:
            condition = (
                "body_sha256 IS NOT NULL AND (fidelity='serialized_dom' OR EXISTS "
                "(SELECT 1 FROM responses r WHERE r.response_id=documents.source_response_id "
                "AND lower(r.content_type) LIKE '%html%'))"
            )
            values = ()
        else:
            cutoff = datetime.now(UTC).timestamp() - older_than_days * 86400
            cutoff_at = datetime.fromtimestamp(cutoff, UTC).isoformat().replace("+00:00", "Z")
            condition = (
                "body_sha256 IS NOT NULL AND captured_at<? AND (fidelity='serialized_dom' "
                "OR EXISTS (SELECT 1 FROM responses r WHERE "
                "r.response_id=documents.source_response_id AND lower(r.content_type) LIKE '%html%'))"
            )
            values = (cutoff_at,)
            con.execute("CREATE TEMP TABLE archive_omitted_body_hashes(sha256 TEXT PRIMARY KEY)")
            con.execute(
                "INSERT OR IGNORE INTO archive_omitted_body_hashes "
                "SELECT body_sha256 FROM documents WHERE " + condition,
                values,
            )
        con.execute(
            "UPDATE documents SET body_sha256=NULL,body_state='unavailable',"
            "body_reason='not_in_corpus' WHERE " + condition,
            values,
        )
        if older_than_days is None:
            con.execute(
                "UPDATE responses SET body_sha256=NULL,body_state='unavailable',"
                "body_reason='not_in_corpus' WHERE body_sha256 IS NOT NULL AND purpose='page' "
                "AND lower(content_type) LIKE '%html%'"
            )
        else:
            con.execute(
                "UPDATE responses SET body_sha256=NULL,body_state='unavailable',"
                "body_reason='not_in_corpus' WHERE body_sha256 IS NOT NULL AND purpose='page' "
                "AND lower(content_type) LIKE '%html%' AND (received_at<? OR body_sha256 IN "
                "(SELECT sha256 FROM archive_omitted_body_hashes)) AND NOT EXISTS "
                "(SELECT 1 FROM documents d WHERE d.source_response_id=responses.response_id "
                "AND d.body_sha256 IS NOT NULL)",
                (cutoff_at,),
            )
        con.execute(
            "DELETE FROM bodies WHERE NOT EXISTS "
            "(SELECT 1 FROM documents WHERE documents.body_sha256=bodies.sha256) "
            "AND NOT EXISTS (SELECT 1 FROM responses WHERE responses.body_sha256=bodies.sha256)"
        )
        removed = before - con.execute("SELECT COUNT(*) FROM bodies").fetchone()[0]
        con.commit()
        if removed:
            # VACUUM only the staged archive snapshot; DELETE alone leaves old BLOB bytes
            # in free SQLite pages, where they would still be recoverable from the ZIP.
            con.execute("VACUUM")
            checkpoint = con.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone()
            if checkpoint is not None and checkpoint[0] != 0:
                raise ValueError("archived scan snapshot could not be checkpointed")
        return removed


def archive_project(
    directory: str | Path,
    out: str | Path,
    *,
    dry_run: bool = False,
    exclude_bodies: bool = False,
    exclude_bodies_older_than_days: int | None = None,
    keep_newest_scans: int | None = None,
) -> dict[str, Any]:
    """Write one zip for a validated project; dry_run only reports the plan and size estimate."""
    root, document = _load(directory)
    if type(exclude_bodies) is not bool:
        raise ValueError("exclude_bodies must be a boolean")
    if exclude_bodies_older_than_days not in {None, 30, 90}:
        raise ValueError("exclude_bodies_older_than_days must be 30, 90, or null")
    if exclude_bodies and exclude_bodies_older_than_days is not None:
        raise ValueError("choose exclude_bodies or exclude_bodies_older_than_days")
    if keep_newest_scans is not None and (
        type(keep_newest_scans) is not int or not 1 <= keep_newest_scans <= 10_000
    ):
        raise ValueError("keep_newest_scans must be from 1 to 10000")
    _validate_project_stores(root)
    target = Path(out)
    if not target.name or ".." in target.parts:
        raise ValueError("archive output must be a plain file path")
    parent = _directory(target.parent, "archive output parent")
    included, skipped = _plan(root, keep_newest_scans=keep_newest_scans)
    estimate = sum(path.stat().st_size for _, path in included)
    plan = {
        "ok": True,
        "dry_run": dry_run,
        "files": [rel for rel, _ in included],
        "skipped": skipped,
        "estimated_bytes": estimate,
        "exclude_bodies": exclude_bodies,
        "exclude_bodies_older_than_days": exclude_bodies_older_than_days,
        "keep_newest_scans": keep_newest_scans,
    }
    if dry_run:
        return plan
    if os.path.lexists(target):
        raise ValueError("archive output already exists; choose a new file")

    partial = parent / f".{target.name}.partial"
    with tempfile.TemporaryDirectory(prefix=".seohead-archive-", dir=parent) as scratch:
        staged: list[tuple[str, Path]] = []
        excluded_body_blobs = 0
        for rel, path in included:
            copy = Path(scratch) / rel.replace("/", "__")
            if rel.endswith(_SQLITE):
                _snapshot(path, copy)
                if exclude_bodies or exclude_bodies_older_than_days is not None:
                    excluded_body_blobs += _exclude_html_bodies(
                        copy,
                        older_than_days=exclude_bodies_older_than_days,
                    )
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
    plan.update(
        {
            "dry_run": False,
            "archive": str(target),
            "file_count": len(entries),
            "excluded_body_blobs": excluded_body_blobs,
        }
    )
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
            if info.filename != MANIFEST and _credential_name(info.filename):
                raise ValueError("archive contains a credential-like file name")
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
            _load(staging)
            for name in _PROJECT_STATE:
                path = staging / name
                if path.is_file():
                    _validate_project_state(staging, name)
            _validate_project_stores(staging)
            if os.path.lexists(dest):
                raise ValueError("restore target appeared during restore; nothing was replaced")
            os.rename(staging, dest)
        finally:
            shutil.rmtree(staging, ignore_errors=True)
    return {"ok": True, "restored": str(dest), "project": open_project(dest)["project"]}
