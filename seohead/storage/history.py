"""Explicit local scan history operations; no catalog or background cleanup."""

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
import time
import uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace
from urllib.parse import urlsplit

from seohead.core import filesystem
from seohead.core.sqlite import open_readonly

from . import (
    APPLICATION_ID,
    MAX_RECORD_BYTES,
    READ_TIMEOUT_SECONDS,
    USER_VERSION,
    ScanError,
    _expected,
    _runtime,
    open_scan,
)

UTC = timezone.utc

_TABLE_COLUMNS = {
    "pages": "p.*, u.url",
    "links": "l.*, source.url AS source, destination.url AS destination",
    "forms": "*",
    "decisions": "*",
    "frontier": "f.*, u.url",
    "query_variants": "*",
    "context_items": "*",
    "responses": "response_id, request_url_id, request_ordinal, status_code, content_type, reported_size_bytes, body_sha256, body_state, body_reason, error, error_kind",
    "documents": "document_id, url_id, representation, source_response_id, body_sha256, captured_at, fidelity, body_state, body_reason",
    "resource_refs": "*",
    "audit": "singleton, schema_version, evidence_revision, analyzer_version, analyzer_revision, created_at, sha256",
}
DEFAULT_HISTORY_WARNING_BYTES = 20 * 1024 * 1024 * 1024


def recommended_name(start_url: str, scan_uuid: str, *, created_at: datetime | None = None) -> str:
    host = (urlsplit(start_url).hostname or "scan").lower()
    host = re.sub(r"[^a-z0-9.-]+", "-", host).strip(".-") or "scan"
    stamp = (created_at or datetime.now(UTC)).astimezone(UTC).strftime("%Y%m%dT%H%M%SZ")
    short_uuid = uuid.UUID(scan_uuid).hex[:8]
    if not short_uuid:
        raise ValueError("scan UUID is required for a collision-safe name")
    return f"{stamp}_{host}_{short_uuid}.sqlite"


def new_scan_path(directory: str | Path, start_url: str, scan_uuid: str) -> Path:
    root = Path(directory)
    if not root.is_dir():
        raise ScanError("selected scans directory does not exist")
    path = root / recommended_name(start_url, scan_uuid)
    if os.path.lexists(path):
        raise ScanError("generated scan path already exists")
    return path


def _lock_path(path: Path) -> Path:
    return path.with_name(path.name + ".writer.lock")


def _lock_is_active(path: Path) -> bool:
    try:
        filesystem.require_locking()
    except OSError as exc:
        raise ScanError(str(exc)) from exc
    lock = _lock_path(path)
    if lock.is_symlink():
        raise ScanError("scan writer lock path must not be a symlink")
    if not os.path.lexists(lock):
        return False
    fd = filesystem.open_lock(lock, create=False)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
            raise ScanError("scan lock must be a regular unaliased file")
        try:
            filesystem.lock_exclusive(fd)
        except OSError:
            return True
        filesystem.unlock(fd)
        return False
    finally:
        os.close(fd)


def _hold_writer_lock(path: Path) -> int:
    try:
        filesystem.require_locking()
    except OSError as exc:
        raise ScanError(str(exc)) from exc
    lock = _lock_path(path)
    if lock.is_symlink():
        raise ScanError("scan writer lock path must not be a symlink")
    fd = filesystem.open_lock(lock)
    try:
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
            raise OSError("scan lock must be a regular unaliased file")
        filesystem.lock_exclusive(fd)
        return fd
    except OSError as exc:
        os.close(fd)
        raise ScanError("scan has an active writer") from exc


def _regular(path: Path) -> os.stat_result:
    try:
        info = path.lstat()
    except OSError as exc:
        raise ScanError(f"scan file is unavailable: {exc}") from exc
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
        raise ScanError("scan path must be a regular unaliased SQLite file")
    return info


def _metadata_connection(path: Path, *, allow_native_v2: bool = False) -> sqlite3.Connection:
    """Validate identity/schema and bound metadata work without reading bodies."""
    _runtime()
    before = _regular(path)
    con = open_readonly(
        path.absolute(),
        row_factory=sqlite3.Row,
        pragmas=(
            "PRAGMA trusted_schema=OFF",
            "PRAGMA query_only=ON",
            "PRAGMA cache_size=-8192",
            "PRAGMA temp_store=FILE",
        ),
    )
    try:
        deadline = time.monotonic() + 5
        con.set_progress_handler(lambda: int(time.monotonic() > deadline), 10000)
        con.execute("BEGIN")
        if con.execute("PRAGMA application_id").fetchone()[0] != APPLICATION_ID:
            raise ScanError("foreign application_id")
        version = con.execute("PRAGMA user_version").fetchone()[0]
        if version == 2 and allow_native_v2:
            con.close()
            con = open_scan(path, require_audit=False, query_timeout_seconds=READ_TIMEOUT_SECONDS)
            after = _regular(path)
            if (before.st_dev, before.st_ino) != (after.st_dev, after.st_ino):
                raise ScanError("scan identity changed while opening")
            return con
        if version != USER_VERSION:
            raise ScanError("unsupported scan user_version; no automatic migration")
        expected = _expected()[0]
        objects = [
            tuple(row)
            for row in con.execute(
                "SELECT type,name,tbl_name,sql FROM sqlite_master WHERE name NOT GLOB 'sqlite_*' ORDER BY type,name LIMIT ?",
                (len(expected) + 1,),
            )
        ]
        if objects != expected:
            raise ScanError("scan.v1 schema differs")
        header = con.execute("SELECT format_version FROM scan WHERE singleton=1").fetchone()
        if (
            header is None
            or header[0] != "scan.v1"
            or con.execute("SELECT COUNT(*) FROM scan").fetchone()[0] != 1
        ):
            raise ScanError("scan.v1 requires one matching format header")
        after = _regular(path)
        if (before.st_dev, before.st_ino) != (after.st_dev, after.st_ino):
            raise ScanError("scan identity changed while opening")
        return con
    except BaseException:
        con.close()
        raise


def _read_scan(path: Path) -> dict:
    columns = "scan_uuid,lifecycle,finish_reason,pinned,crawl_partial,corpus_partial,start_url,config_fingerprint,finished_at,created_at,retention_json,writer_version,writer_revision,source_kind,parent_scan_uuid,evidence_revision,capabilities_json,format_version"
    with contextlib.closing(_metadata_connection(path)) as con:
        sizes = con.execute(
            "SELECT "
            + "+".join(
                "COALESCE(length(CAST(" + name + " AS BLOB)),0)" for name in columns.split(",")
            )
            + " FROM scan WHERE singleton=1"
        ).fetchone()
        if sizes is None or sizes[0] > MAX_RECORD_BYTES:
            raise ScanError("scan header is absent or exceeds metadata byte limit")
        row = dict(con.execute("SELECT " + columns + " FROM scan WHERE singleton=1").fetchone())
        try:
            uuid.UUID(row["scan_uuid"])
            if (
                row["format_version"] != "scan.v1"
                or row["pinned"] not in (0, 1)
                or row["crawl_partial"] not in (0, 1)
                or row["lifecycle"] not in {"running", "interrupted", "finished", "failed"}
            ):
                raise ValueError("invalid scan state")
            _finished_at(row["finished_at"])
        except (TypeError, ValueError) as exc:
            raise ScanError(f"invalid scan metadata: {exc}") from exc
        return row


def _finished_at(value: str | None) -> datetime:
    if not value:
        return datetime.min.replace(tzinfo=UTC)
    result = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if result.tzinfo is None:
        raise ValueError("scan timestamp must include its UTC offset")
    return result.astimezone(UTC)


def _metadata(path: Path) -> dict:
    row = _read_scan(path)
    state = _regular(path)
    start = row["start_url"] or ""
    try:
        retention = json.loads(row["retention_json"])
        warning = retention["history_warning_bytes"]
        # Zero explicitly disables the aggregate history-size warning.  Point-A
        # imports use that value because their source did not have a configured
        # local history threshold; it must not make an otherwise valid retained
        # scan disappear from history.
        if type(warning) is not int or warning < 0:
            raise ValueError("invalid history warning size")
        capabilities = json.loads(row["capabilities_json"])
    except (TypeError, ValueError, KeyError) as exc:
        raise ScanError(f"invalid scan history metadata: {exc}") from exc
    companions = 0
    for suffix in ("-wal", "-shm"):
        companion = path.with_name(path.name + suffix)
        if os.path.lexists(companion):
            companions += _regular(companion).st_size
    from .audit_v2 import audit_v2_path

    audit_companion = audit_v2_path(path)
    audit_state = _regular(audit_companion) if os.path.lexists(audit_companion) else None
    audit_bytes = audit_state.st_size if audit_state is not None else 0
    return {
        "uuid": row["scan_uuid"],
        "path": str(path.absolute()),
        "inode": {"device": state.st_dev, "inode": state.st_ino},
        "mtime_ns": state.st_mtime_ns,
        "bytes": state.st_size,
        "disk_bytes": state.st_size + companions + audit_bytes,
        "audit_bytes": audit_bytes,
        "audit_inode": (
            {"device": audit_state.st_dev, "inode": audit_state.st_ino}
            if audit_state is not None
            else None
        ),
        "lifecycle": row["lifecycle"],
        "finish_reason": row["finish_reason"],
        "pinned": bool(row["pinned"]),
        "crawl_partial": bool(row["crawl_partial"]),
        "corpus_partial": bool(row["corpus_partial"]),
        "start_url": start,
        "host": (urlsplit(start).hostname or "").lower(),
        "config_fingerprint": row["config_fingerprint"],
        "created_at": row["created_at"],
        "finished_at": row["finished_at"],
        "writer_version": row["writer_version"],
        "writer_revision": row["writer_revision"],
        "source_kind": row["source_kind"],
        "parent_scan_uuid": row["parent_scan_uuid"],
        "evidence_revision": row["evidence_revision"],
        "capabilities": capabilities,
        "history_warning_bytes": warning,
    }


def _directory(directory: str | Path) -> Path:
    root = Path(directory).resolve()
    if not root.is_dir():
        raise ScanError("selected scans directory does not exist")
    return root


def _catalog(directory: str | Path) -> tuple[list[dict], list[dict]]:
    root = _directory(directory)
    # A project root points at its scans/ directory (as project-scans does), so
    # `scan-list --directory <project>` lists the real scans instead of none (#1401).
    if root.name != "scans" and (root / "project.json").is_file() and (root / "scans").is_dir():
        root = root / "scans"
    items, errors = [], []
    used = 0
    candidates = (
        path
        for path in root.glob("*.sqlite")
        if not path.name.endswith(".audit-v2.sqlite")
        and not path.name.startswith(".native-scan-")
        and not path.name.startswith(".origin-pacing")  # the core's own pacing ledger
    )
    for index, path in enumerate(candidates):
        if index >= 10_000:
            raise ScanError("scan directory exceeds the 10,000-file history limit")
        try:
            item = _metadata(path)
        except (ScanError, sqlite3.Error, OSError, ValueError) as exc:
            error = {"path": str(path), "reason": str(exc)[:2048]}
            used += len(json.dumps(error, ensure_ascii=False).encode("utf-8"))
            if used > 64 * 1024 * 1024:
                raise ScanError("scan history metadata exceeds 64 MiB") from exc
            errors.append(error)
            continue
        used += len(json.dumps(item, ensure_ascii=False).encode("utf-8"))
        if used > 64 * 1024 * 1024:
            raise ScanError("scan history metadata exceeds 64 MiB")
        items.append(item)
    items.sort(
        key=lambda item: (_finished_at(item["finished_at"] or item["created_at"]), item["uuid"]),
        reverse=True,
    )
    return items, errors


def _pagination(offset: int, limit: int) -> None:
    if type(offset) is not int or type(limit) is not int or offset < 0 or not 1 <= limit <= 1000:
        raise ValueError("offset must be nonnegative and limit must be 1..1000")


def list_scans(directory: str | Path, *, offset: int = 0, limit: int = 100) -> dict:
    _pagination(offset, limit)
    items, errors = _catalog(directory)
    history_bytes = sum(item["disk_bytes"] for item in items)
    # A zero threshold means "disabled", not "warn for every nonempty
    # directory".  Ignore disabled per-scan policies when selecting a shared
    # warning threshold so an imported scan does not suppress or force a
    # warning for native scans that deliberately configured one.
    thresholds = [item["history_warning_bytes"] for item in items if item["history_warning_bytes"]]
    warning = min(thresholds) if thresholds else (0 if items else DEFAULT_HISTORY_WARNING_BYTES)
    return {
        "total": len(items),
        "offset": offset,
        "items": items[offset : offset + limit],
        "has_more": offset + limit < len(items),
        "errors": errors,
        "history_bytes": history_bytes,
        "history_warning_bytes": warning,
        "history_warning": warning > 0 and history_bytes >= warning,
    }


def inspect_scan(
    path: str | Path,
    *,
    table: str = "pages",
    offset: int = 0,
    limit: int = 100,
    max_bytes: int = 1_048_576,
    columns: list[str] | None = None,
    total: bool = False,
) -> dict:
    columns_filter = columns
    if table not in _TABLE_COLUMNS:
        raise ValueError("inspection table is not allowed")
    _pagination(offset, limit)
    if type(max_bytes) is not int or not 1 <= max_bytes <= MAX_RECORD_BYTES:
        raise ValueError("max_bytes must be within 1..8 MiB")
    with contextlib.closing(
        _metadata_connection(Path(path).absolute(), allow_native_v2=True)
    ) as con:
        if table == "pages":
            base = "SELECT p.*,u.url FROM pages p JOIN urls u USING(url_id) ORDER BY p.page_ordinal"
        elif table == "links":
            base = "SELECT l.*,source.url AS source,destination.url AS destination FROM links l JOIN urls source ON source.url_id=l.source_url_id JOIN urls destination ON destination.url_id=l.destination_url_id ORDER BY l.link_id"
        elif table == "frontier":
            base = "SELECT f.*,u.url FROM frontier f JOIN urls u USING(url_id) ORDER BY f.queue_ordinal"
        else:
            primary = sorted(
                (row[5], row[1])
                for row in con.execute("PRAGMA table_info(" + table + ")")
                if row[5]
            )
            order = ",".join('"' + name + '"' for _, name in primary)
            base = "SELECT " + _TABLE_COLUMNS[table] + " FROM " + table + " ORDER BY " + order
        columns = [item[0] for item in con.execute(base + " LIMIT 0").description]
        if columns_filter is not None:
            unknown = [name for name in columns_filter if name not in columns]
            if not columns_filter or unknown or len(set(columns_filter)) != len(columns_filter):
                raise ValueError("columns must be distinct names of the inspected table")
            columns = list(columns_filter)
        quoted = ['"' + name.replace('"', '""') + '"' for name in columns]
        size = "+".join("COALESCE(length(CAST(" + name + " AS BLOB)),0)" for name in quoted)
        limited = base + " LIMIT ? OFFSET ?"
        guarded = ",".join(
            "CASE WHEN _row_bytes<=? THEN " + name + " END AS " + name for name in quoted
        )
        query = (
            "WITH selected AS ("
            + limited
            + "), sized AS (SELECT *,("
            + size
            + ") AS _row_bytes FROM selected) SELECT _row_bytes,"
            + guarded
            + " FROM sized"
        )
        total_rows = (
            con.execute("SELECT COUNT(*) FROM (" + base + ")").fetchone()[0] if total else None
        )
        rows, used, has_more, truncated = [], 0, False, False
        for index, row in enumerate(
            con.execute(query, (limit + 1, offset, *([max_bytes] * len(columns))))
        ):
            if index == limit:
                has_more = True
                break
            if row[0] > max_bytes:
                has_more = truncated = True
                break
            item = dict(zip(columns, tuple(row)[1:], strict=True))
            count = len(json.dumps(item, ensure_ascii=False).encode("utf-8"))
            if used + count > max_bytes:
                has_more = truncated = True
                break
            rows.append(item)
            used += count
        return {
            "table": table,
            "offset": offset,
            "total": total_rows,
            "columns": columns,
            "rows": rows,
            "bytes": used,
            "truncated": truncated,
            "has_more": has_more,
            "next_offset": offset + len(rows),
        }


def snapshot_scan(path: str | Path, destination: str | Path) -> str:
    """Snapshot the scan and any audit.v2 companion as a validated pair."""
    from .audit_v2 import AuditV2Reader, audit_v2_path
    from .native_scan import NativeScan

    source = Path(path).absolute()
    _regular(source)
    if Path(destination).is_dir():
        header = _read_scan(source)
        destination = new_scan_path(destination, header["start_url"] or "", header["scan_uuid"])
    destination = Path(destination).absolute()
    audit_source = audit_v2_path(source)
    audit_target = audit_v2_path(destination)
    if os.path.lexists(audit_target):
        raise ScanError(f"snapshot audit companion already exists: {audit_target}")
    has_audit_v2 = os.path.lexists(audit_source)
    audit_reader = AuditV2Reader(source) if has_audit_v2 else None
    with contextlib.closing(open_scan(source, require_audit=False)):
        pass
    fd, stage_name = tempfile.mkstemp(
        prefix=".audit-pair-snapshot-", suffix=".sqlite", dir=destination.parent
    )
    os.close(fd)
    os.unlink(stage_name)
    staged_scan = Path(stage_name)
    staged_audit = audit_v2_path(staged_scan)
    published_audit = False
    with contextlib.closing(open_scan(source, require_audit=False)) as con:

        def validate_copy(copy):
            with contextlib.closing(open_scan(copy, require_audit=False)):
                pass

        reader = SimpleNamespace(path=source, con=con, inspect=validate_copy)
        try:
            NativeScan.snapshot(
                reader,
                staged_scan,
                temp_margin_bytes=audit_source.stat().st_size if has_audit_v2 else 0,
            )
            if has_audit_v2:
                audit_fd = os.open(
                    staged_audit,
                    os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0),
                    0o600,
                )
                with os.fdopen(audit_fd, "wb") as dest:
                    with audit_source.open("rb") as src:
                        shutil.copyfileobj(src, dest, length=1024 * 1024)
                    dest.flush()
                    os.fsync(dest.fileno())
                with AuditV2Reader(staged_scan):
                    pass
                os.link(staged_audit, audit_target, follow_symlinks=False)
                published_audit = True
                filesystem.fsync_directory(destination.parent)
            os.link(staged_scan, destination, follow_symlinks=False)
            filesystem.fsync_directory(destination.parent)
            return str(destination)
        except FileExistsError as exc:
            raise ScanError(f"snapshot target already exists: {destination}") from exc
        except (OSError, sqlite3.Error, ValueError) as exc:
            raise ScanError(f"cannot snapshot scan and audit companion: {exc}") from exc
        finally:
            if audit_reader is not None:
                audit_reader.close()
            with contextlib.suppress(FileNotFoundError):
                staged_scan.unlink()
            with contextlib.suppress(FileNotFoundError):
                staged_audit.unlink()
            if published_audit and not destination.exists():
                with contextlib.suppress(FileNotFoundError):
                    audit_target.unlink()


def pin_scan(path: str | Path, pinned: bool) -> None:
    if type(pinned) is not bool:
        raise ValueError("pinned must be boolean")
    scan_path = Path(path).absolute()
    before = _regular(scan_path)
    fd = _hold_writer_lock(scan_path)
    con = None
    try:
        with contextlib.closing(open_scan(scan_path, require_audit=False)):
            pass
        current = _regular(scan_path)
        if (before.st_dev, before.st_ino) != (current.st_dev, current.st_ino):
            raise ScanError("scan identity changed before pin update")
        con = sqlite3.connect(scan_path.as_uri() + "?mode=rw", uri=True, timeout=5)
        current = _regular(scan_path)
        if (before.st_dev, before.st_ino) != (current.st_dev, current.st_ino):
            raise ScanError("scan identity changed while opening for pin update")
        con.execute("PRAGMA trusted_schema=OFF")
        con.execute("PRAGMA synchronous=FULL")
        mode = con.execute("PRAGMA journal_mode=DELETE").fetchone()[0]
        if mode.lower() != "delete":
            raise ScanError("pin requires a checkpointed DELETE-journal artifact")
        con.execute("BEGIN IMMEDIATE")
        con.execute("UPDATE scan SET pinned=? WHERE singleton=1", (int(pinned),))
        con.commit()
    except sqlite3.Error as exc:
        if con is not None:
            con.rollback()
        raise ScanError(f"cannot change scan pin: {exc}") from exc
    finally:
        if con is not None:
            con.close()
        filesystem.unlock(fd)
        os.close(fd)


def _policy(older_than_days: int, keep_newest: int) -> None:
    if (
        type(older_than_days) is not int
        or not 0 <= older_than_days <= 36500
        or type(keep_newest) is not int
        or not 0 <= keep_newest <= 10000
    ):
        raise ValueError("retention needs 0..36500 days and 0..10000 newest scans")


def _eligible(items: list[dict], older_than_days: int, keep_newest: int) -> list[dict]:
    _policy(older_than_days, keep_newest)
    now = datetime.now(UTC)
    groups: dict[tuple[str, str], list[dict]] = {}
    for item in items:
        groups.setdefault((item["host"], item["config_fingerprint"]), []).append(item)
    candidates = []
    for group in groups.values():
        group.sort(
            key=lambda item: (
                _finished_at(item["finished_at"] or item["created_at"]),
                item["uuid"],
            ),
            reverse=True,
        )
        for item in group[keep_newest:]:
            if (
                item["lifecycle"] == "finished"
                and item["finished_at"]
                and not item["pinned"]
                and not item["crawl_partial"]
                and not item["corpus_partial"]
                and item["disk_bytes"] == item["bytes"] + item["audit_bytes"]
                and now - _finished_at(item["finished_at"]) >= timedelta(days=older_than_days)
            ):
                candidates.append(item)
    return sorted(candidates, key=lambda item: item["path"])


def _digest(plan: dict) -> str:
    return hashlib.sha256(
        json.dumps(
            {key: value for key, value in plan.items() if key != "digest"},
            sort_keys=True,
            allow_nan=False,
        ).encode("utf-8")
    ).hexdigest()


def prune_preview(
    directory: str | Path, *, older_than_days: int = 30, keep_newest: int = 5
) -> dict:
    _policy(older_than_days, keep_newest)
    root = _directory(directory)
    info = root.stat()
    items, errors = _catalog(root)
    candidates = [
        item
        for item in _eligible(items, older_than_days, keep_newest)
        if not _lock_is_active(Path(item["path"]))
    ]
    payload = {
        "directory": str(root),
        "directory_inode": {"device": info.st_dev, "inode": info.st_ino},
        "older_than_days": older_than_days,
        "keep_newest": keep_newest,
        "candidates": candidates,
        "errors": errors,
        "warning": "Deleting a scan can remove a useful comparison baseline.",
    }
    payload["digest"] = _digest(payload)
    return payload


def prune_apply(directory: str | Path, plan: dict) -> list[str]:
    from .audit_v2 import audit_v2_path

    root = _directory(directory)
    if (
        not isinstance(plan, dict)
        or set(plan)
        != {
            "directory",
            "directory_inode",
            "older_than_days",
            "keep_newest",
            "candidates",
            "errors",
            "warning",
            "digest",
        }
        or _digest(plan) != plan.get("digest")
        or str(root) != plan["directory"]
    ):
        raise ScanError("prune plan is stale or belongs to another directory")
    state = root.stat()
    if plan["directory_inode"] != {"device": state.st_dev, "inode": state.st_ino}:
        raise ScanError("prune directory identity changed")
    _policy(plan["older_than_days"], plan["keep_newest"])
    if not isinstance(plan["candidates"], list) or len(plan["candidates"]) > 10000:
        raise ScanError("invalid prune selection")
    held, validated = [], []
    seen = set()
    try:
        # Check every reviewed file and current retention rank before any unlink.
        for expected in plan["candidates"]:
            path = Path(expected["path"])
            if path.parent != root or path in seen:
                raise ScanError("prune candidate escapes its directory or is duplicated")
            seen.add(path)
            info = _regular(path)
            held.append(_hold_writer_lock(path))
            audit_path = audit_v2_path(path)
            if expected["audit_inode"] is not None:
                held.append(_hold_writer_lock(audit_path))
                audit_info = _regular(audit_path)
                if {"device": audit_info.st_dev, "inode": audit_info.st_ino} != expected[
                    "audit_inode"
                ]:
                    raise ScanError("prune audit companion identity changed")
            elif os.path.lexists(audit_path):
                raise ScanError("prune candidate gained an audit companion")
            if {"device": info.st_dev, "inode": info.st_ino} != expected["inode"]:
                raise ScanError("prune candidate identity changed")
            current = _metadata(path)
            if current != expected:
                raise ScanError("prune candidate metadata changed")
            validated.append((path, current))
        items, _errors = _catalog(root)
        eligible = {
            item["path"] for item in _eligible(items, plan["older_than_days"], plan["keep_newest"])
        }
        if any(str(path) not in eligible for path, _ in validated):
            raise ScanError("prune selection is no longer eligible under its retention policy")
        for path, expected in validated:
            if _metadata(path) != expected:
                raise ScanError("prune candidate changed before deletion")
        removed = []
        for path, expected in validated:
            audit_path = audit_v2_path(path)
            hold_path = None
            if expected["audit_inode"] is not None:
                hold_path = root / f".{audit_path.name}.prune-hold-{uuid.uuid4().hex}"
                os.link(audit_path, hold_path, follow_symlinks=False)
                try:
                    audit_path.unlink()
                except OSError as exc:
                    try:
                        hold_path.unlink()
                    except OSError as cleanup_exc:
                        raise ScanError(
                            f"scan and audit remain intact, but a duplicate recovery link remains at {hold_path}: {cleanup_exc}"
                        ) from cleanup_exc
                    raise exc
            try:
                path.unlink()
            except OSError:
                if hold_path is not None:
                    try:
                        os.link(hold_path, audit_path, follow_symlinks=False)
                    except FileExistsError as exc:
                        if not os.path.samefile(hold_path, audit_path):
                            raise ScanError(
                                f"could not restore pruned audit companion; recovery copy remains at {hold_path}"
                            ) from exc
                    except OSError as exc:
                        raise ScanError(
                            f"could not restore pruned audit companion; recovery copy remains at {hold_path}: {exc}"
                        ) from exc
                    try:
                        hold_path.unlink()
                    except OSError as cleanup_exc:
                        raise ScanError(
                            f"scan was preserved but audit companion has a duplicate recovery link at {hold_path}: {cleanup_exc}"
                        ) from cleanup_exc
                raise
            if hold_path is not None:
                try:
                    hold_path.unlink()
                except OSError as exc:
                    raise ScanError(
                        f"scan was pruned but its audit companion recovery copy remains at {hold_path}: {exc}"
                    ) from exc
            removed.append(str(path))
        if removed:
            from .native_scan import _fsync_directory

            _fsync_directory(root)
        return removed
    finally:
        for fd in held:
            with contextlib.suppress(OSError):
                filesystem.unlock(fd)
                os.close(fd)
