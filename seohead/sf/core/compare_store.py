"""Disk-backed comparison with complete, deterministic finding exports.

The temporary SQLite index bounds working memory independently of URL/finding
count. A new output directory contains immutable NDJSON streams and a compact
manifest; no inline sample stands in for the full comparison population.
"""

from __future__ import annotations

import atexit
import ctypes
import errno
import gzip
import hashlib
import json
import os
import shutil
import sqlite3
import stat
import sys
import tempfile
import threading
import zlib
from collections import OrderedDict
from collections.abc import Iterable, Mapping
from contextlib import closing, nullcontext, suppress
from pathlib import Path
from typing import Any, Callable
from urllib.parse import urlsplit

from seohead.core.filesystem import fsync_directory
from seohead.storage.audit_v2 import AuditV2Reader

from .compare import (
    CompareError,
    _header,
    _host_changed,
    _iter_rows,
    _key,
    _load_correspondence,
    _mapped_url,
    _measurement_gaps,
    _page_facts,
    _run,
    preflight,
)
from .evidence_contract import comparison_compatibility, comparison_warnings

_BUCKETS = ("entered", "left", "appeared", "disappeared", "unchanged")
_INDEX_BATCH = 10_000
_READ_INDEX_LIMIT = 1
_READ_INDEXES: OrderedDict[tuple[Any, ...], "_ReadIndex"] = OrderedDict()
_READ_INDEXES_LOCK = threading.RLock()
_SQLITE_MAX_INTEGER = (1 << 63) - 1


def _check_cancel(cancel_event: threading.Event | None) -> None:
    if cancel_event is not None and cancel_event.is_set():
        raise CompareError("comparison was cancelled")


def _rename_directory_noreplace(source: Path, destination: Path) -> None:
    """Atomically publish a directory without replacing any existing destination."""
    libc = ctypes.CDLL(None, use_errno=True)
    if sys.platform == "darwin" and hasattr(libc, "renamex_np"):
        rename = libc.renamex_np
        rename.argtypes = (ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint)
        rename.restype = ctypes.c_int
        result = rename(os.fsencode(source), os.fsencode(destination), 0x00000004)  # RENAME_EXCL
    elif sys.platform.startswith("linux") and hasattr(libc, "renameat2"):
        rename = libc.renameat2
        rename.argtypes = (ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint)
        rename.restype = ctypes.c_int
        result = rename(-100, os.fsencode(source), -100, os.fsencode(destination), 1)  # AT_FDCWD, RENAME_NOREPLACE
    else:
        if destination.exists() or destination.is_symlink():
            raise FileExistsError(errno.EEXIST, os.strerror(errno.EEXIST), str(destination))
        os.rename(source, destination)
        return
    if result != 0:
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error), str(destination))


def _safe_member(path: Path, *, directory: bool = False, optional: bool = False, max_links: int = 1) -> None:
    try:
        info = path.lstat()
    except FileNotFoundError:
        if optional:
            return
        raise CompareError(f"comparison checkpoint member is missing: {path.name}")
    if stat.S_ISLNK(info.st_mode) or (not stat.S_ISDIR(info.st_mode) if directory else not stat.S_ISREG(info.st_mode)):
        raise CompareError(f"comparison checkpoint member is unsafe: {path.name}")
    if not directory and info.st_nlink > max_links:
        raise CompareError(f"comparison checkpoint member has unexpected hard links: {path.name}")


def _json(value: Any) -> str:
    return json.dumps(
        value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False
    )


def _file_sha256(path: Path) -> tuple[str, int]:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
            size += len(block)
    return digest.hexdigest(), size


def _collection_count(source: Any, name: str) -> int | None:
    if isinstance(source, AuditV2Reader):
        count = source.collections.get(f"/{name}")
        return count if type(count) is int else None
    value = source.get(name) if isinstance(source, Mapping) else None
    return len(value) if isinstance(value, list) else None


def _segment_definitions(source: Any) -> list[dict[str, Any]]:
    run = _header(source).get("run")
    config = run.get("crawl_config") if isinstance(run, Mapping) else None
    if not isinstance(config, Mapping):
        return []
    analysis = config.get("analysis.segments") or []
    if isinstance(analysis, list) and analysis:
        if any(not isinstance(item, Mapping) for item in analysis):
            return []
        return [dict(item) for item in analysis]
    scope = config.get("scope.segments") or []
    if not isinstance(scope, list):
        return []
    definitions = []
    for item in scope:
        if not isinstance(item, Mapping) or not isinstance(item.get("name"), str):
            return []
        rules = []
        if item.get("prefix"):
            rules.append({"op": "prefix", "field": "path", "value": item["prefix"]})
        if item.get("host"):
            rules.append({"op": "eq", "field": "host", "value": item["host"]})
        pattern = item.get("pattern")
        if pattern:
            if hasattr(pattern, "pattern"):
                pattern = pattern.pattern
            rules.append({"op": "regex", "field": "url", "value": pattern})
        definitions.append({"name": item["name"], "rules": rules})
    return definitions


def _primary_segment(page: Mapping[str, Any], ordered_segments: list[Any]) -> str:
    if not ordered_segments:
        return "unsegmented"
    from seohead.sf.core.segments import UNSEGMENTED, _segment_matches

    url = page["url"]
    parts = urlsplit(url)
    record = {**page, "path": parts.path, "host": (parts.hostname or "").lower()}
    resolved: dict[str, set[str]] = {}
    primary = None
    for segment in ordered_segments:
        matched = _segment_matches(segment, record, resolved)
        resolved[segment.name] = {url} if matched else set()
        if matched and primary is None:
            primary = segment.name
    return "unsegmented" if primary in (None, UNSEGMENTED) else str(primary)


def _job_id(identities: Mapping[str, Any], force: bool, correspondence: Any, compression: str) -> str:
    declaration_hash = hashlib.sha256(_json(correspondence).encode("utf-8")).hexdigest() if correspondence else None
    key = _json({"identities": identities, "force": force, "correspondence": declaration_hash, "compression": compression})
    return hashlib.sha256(key.encode("utf-8")).hexdigest()[:32]


def _notify(
    progress: Callable[[Any], None] | None,
    *,
    job_id: str,
    state: str,
    stage: str,
    completed: int | None,
    total: int | None,
    reason: str | None = None,
) -> None:
    if progress is None:
        return
    try:
        from seohead.core.job_control import JobProgressUpdate

        progress(
            JobProgressUpdate(
                job_id=job_id,
                state=state,
                stage=stage,
                completed=completed,
                total=total,
                reason=reason,
            )
        )
    except Exception:
        # A notification transport must not corrupt or stop the saved comparison.
        return


def _state_value(con: sqlite3.Connection, key: str, default: Any = None) -> Any:
    row = con.execute("SELECT value FROM job_state WHERE key=?", (key,)).fetchone()
    return json.loads(row[0]) if row else default


def _set_state(con: sqlite3.Connection, key: str, value: Any) -> None:
    con.execute(
        "INSERT INTO job_state(key,value) VALUES (?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        (key, _json(value)),
    )


def _discard_completed_checkpoint(work: Path, job_id: str) -> None:
    """Remove a published job's now-redundant work cache when no writer holds it."""
    from seohead.core.filesystem import lock_exclusive, open_lock, unlock

    if work.is_symlink() or not work.is_dir():
        return
    info = work.lstat()
    try:
        job = json.loads((work / "job.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return
    if job.get("job_id") != job_id:
        return
    try:
        fd = open_lock(work / ".writer.lock", create=False)
    except OSError:
        return
    try:
        try:
            lock_exclusive(fd)
        except OSError:
            return
        unlock(fd)
    finally:
        os.close(fd)
    with suppress(OSError):
        if os.path.samestat(info, work.lstat()):
            shutil.rmtree(work)


def _index_collection(
    con: sqlite3.Connection,
    source: Any,
    side: int,
    name: str,
    ordered_segments: list[Any],
    *,
    checkpoint_key: str,
    progress: Callable[[Any], None] | None,
    job_id: str,
    progress_base: int,
    progress_total: int | None,
    cancel_event: threading.Event | None,
) -> int:
    source_backed = isinstance(source, AuditV2Reader)
    offset = int(_state_value(con, checkpoint_key, 0))
    total = _collection_count(source, name)
    if total is not None and offset > total:
        raise CompareError("comparison checkpoint exceeds its source collection")
    last_offset = offset
    if source_backed:
        if total is None:
            raise CompareError("validated audit.v2 source lacks a collection count")
        rows = ((ordinal, source.get_item(f"/{name}", ordinal)) for ordinal in range(offset, total))
    else:
        rows = ((ordinal, row) for ordinal, row in enumerate(_iter_rows(source, name)) if ordinal >= offset)
    for ordinal, row in rows:
        _check_cancel(cancel_event)
        last_offset = ordinal + 1
        if name == "pages":
            if not isinstance(row, Mapping) or not isinstance(row.get("url"), str) or not row["url"]:
                raise CompareError("comparison page must contain a nonempty URL")
            document = _json(row)
            segment = _primary_segment(row, ordered_segments)
            try:
                con.execute(
                    "INSERT INTO pages VALUES (?,?,?,?,?,?)",
                    (side, row["url"], row["url"], ordinal, None if source_backed else document, segment),
                )
            except sqlite3.IntegrityError as exc:
                raise CompareError(f"comparison has duplicate page URL {row['url']!r}") from exc
        else:
            if (
                not isinstance(row, Mapping)
                or not isinstance(row.get("check"), str)
                or not row["check"]
            ):
                raise CompareError("comparison finding must contain a nonempty check")
            if row.get("target_url") is not None and not isinstance(row["target_url"], str):
                raise CompareError("comparison finding target_url must be a string or null")
            check, url = _key(row)
            document = _json(row)
            try:
                con.execute(
                    "INSERT INTO issues VALUES (?,?,?,?,?,?)",
                    (side, check, url, url, ordinal, None if source_backed else document),
                )
            except sqlite3.IntegrityError as exc:
                raise CompareError(f"comparison has duplicate finding key {(check, url)!r}") from exc
        next_offset = ordinal + 1
        if next_offset % _INDEX_BATCH == 0:
            _set_state(con, checkpoint_key, next_offset)
            con.commit()
            _notify(
                progress,
                job_id=job_id,
                state="running",
                stage=f"index_{'before' if side == 0 else 'after'}_{name}",
                completed=progress_base + next_offset,
                total=progress_total,
            )
    if total is not None and offset != total:
        _set_state(con, checkpoint_key, total)
        con.commit()
        _notify(
            progress,
            job_id=job_id,
            state="running",
            stage=f"index_{'before' if side == 0 else 'after'}_{name}",
            completed=progress_base + total,
            total=progress_total,
        )
    elif total is None:
        _set_state(con, checkpoint_key, last_offset)
        con.commit()
    return total if total is not None else int(_state_value(con, checkpoint_key, 0))


def _row(source: Any, name: str, ordinal: int, document: str | None) -> Any:
    return json.loads(document) if document is not None else source.get_item(f"/{name}", ordinal)


def _pair(before_url: str, after_url: str, kind: str, state: str) -> dict[str, Any]:
    return {
        "before_url": before_url,
        "after_url": after_url,
        "correspondence_key": after_url,
        "kind": kind,
        "state": state,
        "before_origin": f"{urlsplit(before_url).scheme}://{urlsplit(before_url).netloc}",
        "after_origin": f"{urlsplit(after_url).scheme}://{urlsplit(after_url).netloc}",
        "host_changed": _host_changed(before_url, after_url),
    }


def _bind(
    con: sqlite3.Connection,
    declaration: Mapping[str, Any],
    *,
    progress: Callable[[Any], None] | None = None,
    job_id: str = "",
    total: int | None = None,
    cancel_event: threading.Event | None = None,
) -> None:
    completed = 0
    for (before_url,) in con.execute("SELECT url FROM pages WHERE side=0 ORDER BY url"):
        _check_cancel(cancel_event)
        completed += 1
        after_url, kind = _mapped_url(before_url, declaration)
        if after_url is None:
            if completed % _INDEX_BATCH == 0:
                _notify(progress, job_id=job_id, state="running", stage="url_correspondence", completed=completed, total=total)
            continue
        matched = con.execute("SELECT 1 FROM pages WHERE side=1 AND url=?", (after_url,)).fetchone()
        state = "matched" if matched else "after_not_crawled"
        con.execute(
            "INSERT INTO pairs VALUES (?,?,?,?)",
            (before_url, after_url, state, _json(_pair(before_url, after_url, kind, state))),
        )
        if matched:
            con.execute(
                "UPDATE pages SET comparison_url=? WHERE side=0 AND url=?", (after_url, before_url)
            )
        if completed % _INDEX_BATCH == 0:
            _notify(progress, job_id=job_id, state="running", stage="url_correspondence", completed=completed, total=total)
    for before_url, after_url in sorted(declaration["pairs"].items()):
        if not con.execute("SELECT 1 FROM pages WHERE side=0 AND url=?", (before_url,)).fetchone():
            con.execute(
                "INSERT INTO pairs VALUES (?,?,?,?)",
                (
                    before_url,
                    after_url,
                    "before_not_crawled",
                    _json(_pair(before_url, after_url, "explicit_pair", "before_not_crawled")),
                ),
            )
    # Include identity-mapped pages in collision detection: A -> B must not
    # silently merge with an existing, otherwise unmapped baseline B.
    collision = con.execute(
        "SELECT comparison_url FROM pages WHERE side=0 GROUP BY comparison_url HAVING count(*)>1 LIMIT 1"
    ).fetchone()
    if collision:
        raise CompareError(f"url correspondence has a page collision at {collision[0]!r}")
    con.execute("""UPDATE issues SET comparison_url=COALESCE(
        (SELECT after_url FROM pairs WHERE before_url=issues.url AND state='matched'), url)
        WHERE side=0""")


def _export(
    root: Path,
    name: str,
    rows: Iterable[Any],
    *,
    compression: str = "none",
    on_progress: Callable[[int], None] | None = None,
    cancel_event: threading.Event | None = None,
) -> dict[str, Any]:
    digest = hashlib.sha256()
    count = 0
    size = 0
    compressed = compression == "gzip"
    path = root / f"{name}.ndjson{'.gz' if compressed else ''}"
    with path.open("xb") as raw:
        # Exclude clock and destination path from gzip headers: the same rows
        # produce the same bytes in separate CLI and MCP output directories.
        with (
            gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0, compresslevel=6)
            if compressed
            else nullcontext(raw)
        ) as stream:
            for row in rows:
                _check_cancel(cancel_event)
                data = (_json(row) + "\n").encode("utf-8")
                stream.write(data)
                if not compressed:
                    digest.update(data)
                count += 1
                size += len(data)
                if on_progress is not None and count % _INDEX_BATCH == 0:
                    on_progress(count)
        raw.flush()
        os.fsync(raw.fileno())
    if on_progress is not None and count % _INDEX_BATCH:
        on_progress(count)
    stored_size = size
    if compressed:
        stored_size = 0
        with path.open("rb") as stream:
            for data in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(data)
                stored_size += len(data)
    return {
        "path": path.name,
        "format": "ndjson.gz" if compressed else "ndjson",
        "rows": count,
        "bytes": stored_size,
        "sha256": digest.hexdigest(),
        **({"compression": "gzip", "uncompressed_bytes": size} if compressed else {}),
    }


def iter_compare_rows(manifest_path: str | Path, name: str):
    """Validate and stream one complete comparison file, plain or gzip.

    The stored-byte hash is verified before yielding. Exhaust the iterator to
    validate JSONL framing, decompressor integrity, row and decoded byte counts.
    """
    manifest_path = Path(manifest_path)
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        if manifest.get("schema_version") != "compare.v2":
            raise CompareError("unsupported comparison manifest schema")
        entry = manifest["files"][name]
        relative = entry["path"]
        if (
            not isinstance(relative, str)
            or Path(relative).name != relative
            or relative in {".", ".."}
        ):
            raise CompareError("comparison file path must be a local filename")
        path = manifest_path.parent / relative
        if path.is_symlink():
            raise CompareError("comparison file must not be a symbolic link")
        format_ = entry["format"]
        if format_ not in {"ndjson", "ndjson.gz"}:
            raise CompareError("unsupported comparison file format")
        compressed = format_ == "ndjson.gz"
        if compressed and entry.get("compression") != "gzip":
            raise CompareError("comparison compression metadata mismatch")
        expected_size = entry["uncompressed_bytes"] if compressed else entry["bytes"]
        for value in (entry["bytes"], expected_size, entry["rows"]):
            if type(value) is not int or value < 0:
                raise CompareError("invalid comparison file count metadata")
        count = size = stored_size = 0
        digest = hashlib.sha256()
        # Keep the same descriptor across the digest and decode passes.
        with path.open("rb") as raw:
            for data in iter(lambda: raw.read(1024 * 1024), b""):
                stored_size += len(data)
                digest.update(data)
            if stored_size != entry["bytes"] or digest.hexdigest() != entry["sha256"]:
                raise CompareError("comparison file checksum or byte count mismatch")
            raw.seek(0)
            with (
                gzip.GzipFile(mode="rb", fileobj=raw) if compressed else nullcontext(raw)
            ) as stream:
                for line in stream:
                    size += len(line)
                    count += 1
                    if size > expected_size or count > entry["rows"] or not line.endswith(b"\n"):
                        raise CompareError("comparison file row framing or count mismatch")
                    row = json.loads(line)
                    if not isinstance(row, dict):
                        raise CompareError("comparison file contains a non-object row")
                    yield row
            if count != entry["rows"] or size != expected_size:
                raise CompareError("comparison file row or decoded byte count mismatch")
    except (OSError, EOFError, ValueError, KeyError, TypeError, zlib.error) as exc:
        if isinstance(exc, CompareError):
            raise
        raise CompareError(f"invalid comparison file {name!r}: {exc}") from exc


class _ReadIndex:
    """A process-local disk index retained across pages of one validated stream."""

    def __init__(
        self,
        manifest_path: Path,
        name: str,
        temp_dir: tempfile.TemporaryDirectory,
    ) -> None:
        self.manifest_path = manifest_path
        self.name = name
        self.temp_dir = temp_dir
        self.lock = threading.RLock()
        self.con = sqlite3.connect(Path(temp_dir.name) / "page.sqlite", check_same_thread=False)
        self.con.execute("PRAGMA temp_store=FILE")
        self.con.execute("PRAGMA cache_size=-8192")
        self.con.executescript(
            "CREATE TABLE rows (sort_url TEXT,sort_check TEXT,sort_severity TEXT,"
            "segment TEXT,change_key TEXT,url_fold TEXT,tie_key INTEGER,document TEXT);"
        )
        batch = []
        ordinal = 0
        for row in iter_compare_rows(manifest_path, name):
            issue = row.get("issue") if name == "segment_changes" else row
            if issue is None:
                issue = row.get("after") or row
            if name == "segment_changes" and "before" in row:
                after = row.get("after") or {}
                before = row.get("before") or {}
                url = after.get("target_url") or before.get("target_url") or ""
                check = after.get("check") or before.get("check") or ""
                severity = after.get("severity") or before.get("severity") or ""
            else:
                url = issue.get("target_url") or issue.get("url") or ""
                check = issue.get("check") or ""
                severity = issue.get("severity") or ""
            document = _json(row)
            batch.append(
                (
                    str(url).casefold(),
                    str(check).casefold(),
                    str(severity).casefold(),
                    row.get("segment", ""),
                    row.get("change", ""),
                    str(url).casefold(),
                    ordinal,
                    document,
                )
            )
            ordinal += 1
            if len(batch) >= _INDEX_BATCH:
                self.con.executemany("INSERT INTO rows VALUES (?,?,?,?,?,?,?,?)", batch)
                self.con.commit()
                batch.clear()
        if batch:
            self.con.executemany("INSERT INTO rows VALUES (?,?,?,?,?,?,?,?)", batch)
        self.con.executescript(
            "CREATE INDEX rows_url_order ON rows(sort_url,tie_key);"
            "CREATE INDEX rows_check_order ON rows(sort_check,tie_key);"
            "CREATE INDEX rows_severity_order ON rows(sort_severity,tie_key);"
            "CREATE INDEX rows_segment_change ON rows(segment,change_key);"
        )
        self.con.commit()
        self.con.execute("PRAGMA query_only=ON")

    def close(self) -> None:
        with self.lock:
            self.con.close()
            self.temp_dir.cleanup()

    def page(
        self,
        *,
        offset: int,
        limit: int,
        sort: str,
        direction: str,
        contains: str | None,
        segment: str | None,
        change: str | None,
        max_bytes: int,
    ) -> dict[str, Any]:
        clauses, params = [], []
        if segment is not None:
            clauses.append("segment=?")
            params.append(segment)
        if change is not None:
            clauses.append("change_key=?")
            params.append(change)
        if contains is not None:
            clauses.append("instr(url_fold,?)>0")
            params.append(contains.casefold())
        where = " WHERE " + " AND ".join(clauses) if clauses else ""
        order = "ASC" if direction == "asc" else "DESC"
        sort_column = {"url": "sort_url", "check": "sort_check", "severity": "sort_severity"}[sort]
        with self.lock:
            total = self.con.execute(f"SELECT COUNT(*) FROM rows{where}", params).fetchone()[0]
            rows, size = [], 0
            query_params = [*params, limit, offset]
            for (document,) in self.con.execute(
                f"SELECT document FROM rows{where} ORDER BY {sort_column} {order},tie_key ASC LIMIT ? OFFSET ?",
                query_params,
            ):
                row = json.loads(document)
                encoded = len((_json(row) + "\n").encode("utf-8"))
                if size + encoded > max_bytes:
                    if not rows:
                        raise CompareError("one comparison row exceeds max_bytes")
                    break
                rows.append(row)
                size += encoded
        return {
            "ok": True,
            "format": "compare.v2.page",
            "file": self.name,
            "total": total,
            "offset": offset,
            "returned": len(rows),
            "has_more": offset + len(rows) < total,
            "sort": sort,
            "direction": direction,
            **({"segment": segment} if segment is not None else {}),
            **({"change": change} if change is not None else {}),
            "rows": rows,
        }


def _clear_read_indexes() -> None:
    with _READ_INDEXES_LOCK:
        for index in _READ_INDEXES.values():
            with suppress(Exception):
                index.close()
        _READ_INDEXES.clear()


atexit.register(_clear_read_indexes)


def read_compare_page(
    manifest_path: str | Path,
    name: str,
    *,
    offset: int = 0,
    limit: int = 200,
    sort: str = "url",
    direction: str = "asc",
    contains: str | None = None,
    segment: str | None = None,
    change: str | None = None,
    max_bytes: int = 1_048_576,
) -> dict[str, Any]:
    """Read a bounded page; retain a disk index for repeated pages in this process."""
    if not isinstance(name, str):
        raise CompareError("file must be a comparison stream name")
    if type(offset) is not int or not 0 <= offset <= _SQLITE_MAX_INTEGER:
        raise CompareError("offset must be an integer from 0 through 9223372036854775807")
    if type(limit) is not int or not 1 <= limit <= 500:
        raise CompareError("limit must be an integer 1..500")
    if sort not in {"url", "check", "severity"}:
        raise CompareError("sort must be url, check, or severity")
    if direction not in {"asc", "desc"}:
        raise CompareError("direction must be asc or desc")
    if type(max_bytes) is not int or not 4096 <= max_bytes <= 8 * 1024 * 1024:
        raise CompareError("max_bytes must be 4096..8388608")
    if contains is not None and (not isinstance(contains, str) or len(contains) > 2048):
        raise CompareError("contains must be at most 2048 characters")
    if segment is not None and (not isinstance(segment, str) or len(segment) > 256):
        raise CompareError("segment must be at most 256 characters")
    if change is not None and change not in _BUCKETS:
        raise CompareError("change must be entered, left, appeared, disappeared, or unchanged")
    if name != "segment_changes" and (segment is not None or change is not None):
        raise CompareError("segment and change filters require file=segment_changes")
    manifest_path = Path(manifest_path).absolute()
    if manifest_path.is_symlink():
        raise CompareError("comparison manifest must not be a symbolic link")
    try:
        raw_manifest = manifest_path.read_bytes()
        manifest = json.loads(raw_manifest)
    except (OSError, ValueError, TypeError) as exc:
        raise CompareError(f"could not read comparison manifest for {name!r}") from exc
    if (
        not isinstance(manifest, Mapping)
        or manifest.get("schema_version") != "compare.v2"
        or not isinstance(manifest.get("files"), Mapping)
        or name not in manifest["files"]
    ):
        raise CompareError(f"comparison manifest does not contain file {name!r}")
    entry = manifest["files"][name]
    if not isinstance(entry, Mapping):
        raise CompareError("comparison file entry is malformed")
    relative = entry.get("path") if isinstance(entry, Mapping) else None
    if not isinstance(relative, str) or Path(relative).name != relative or relative in {".", ".."}:
        raise CompareError("comparison file path must be a local filename")
    data_path = manifest_path.parent / relative
    try:
        file_info = data_path.lstat()
        manifest_info = manifest_path.stat()
    except OSError as exc:
        raise CompareError(f"comparison data file is unavailable for {name!r}") from exc
    if data_path.is_symlink():
        raise CompareError("comparison file must not be a symbolic link")
    file_signature = (file_info.st_dev, file_info.st_ino, file_info.st_size, file_info.st_mtime_ns, file_info.st_ctime_ns)
    key = (str(manifest_path), name, hashlib.sha256(raw_manifest).hexdigest(), file_signature,
           manifest_info.st_dev, manifest_info.st_ino, manifest_info.st_size, manifest_info.st_mtime_ns, manifest_info.st_ctime_ns)
    with _READ_INDEXES_LOCK:
        index = _READ_INDEXES.get(key)
        if index is None:
            stale = [cache_key for cache_key in _READ_INDEXES if cache_key[:2] == (str(manifest_path), name)]
            for cache_key in stale:
                _READ_INDEXES.pop(cache_key).close()
            temporary = tempfile.TemporaryDirectory(prefix="spiderhead-compare-index-")
            try:
                index = _ReadIndex(manifest_path, name, temporary)
            except BaseException:
                temporary.cleanup()
                raise
            _READ_INDEXES[key] = index
            while len(_READ_INDEXES) > _READ_INDEX_LIMIT:
                _old_key, old = _READ_INDEXES.popitem(last=False)
                old.close()
        else:
            _READ_INDEXES.move_to_end(key)
        return index.page(
            offset=offset,
            limit=limit,
            sort=sort,
            direction=direction,
            contains=contains,
            segment=segment,
            change=change,
            max_bytes=max_bytes,
        )


def compare_markdown(manifest_path: str | Path, *, max_bytes: int = 65_536) -> str:
    """Render a bounded summary report from a complete compare.v2 package."""
    if type(max_bytes) is not int or not 4096 <= max_bytes <= 65_536:
        raise CompareError("max_bytes must be 4096..65536")
    manifest_path = Path(manifest_path)
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise CompareError(f"could not read comparison manifest: {exc}") from exc
    if not isinstance(manifest, dict) or manifest.get("schema_version") != "compare.v2":
        raise CompareError("unsupported comparison manifest schema")
    summary = manifest.get("summary")
    if not isinstance(summary, Mapping):
        raise CompareError("comparison manifest is missing summary counts")
    conservation = manifest.get("conservation")
    if not isinstance(conservation, Mapping) or conservation.get("state") != "complete":
        raise CompareError("comparison manifest does not prove complete row conservation")
    before = manifest.get("before") if isinstance(manifest.get("before"), Mapping) else {}
    after = manifest.get("after") if isinstance(manifest.get("after"), Mapping) else {}
    lines = [
        "# Saved scan comparison",
        "",
        f"Before scan: `{before.get('scan_uuid', 'unknown')}`",
        f"After scan: `{after.get('scan_uuid', 'unknown')}`",
        "",
        "| Change | Findings |",
        "|---|---:|",
    ]
    for key in _BUCKETS:
        value = summary.get(key)
        if type(value) is not int or value < 0:
            raise CompareError(f"comparison summary has an invalid {key} count")
        lines.append(f"| {key.title()} | {value} |")
    lines.extend(("", f"Conservation: `{conservation.get('state')}`."))
    segments = manifest.get("segment_breakdown")
    if isinstance(segments, Mapping):
        lines.extend(("", f"Segment breakdown: `{segments.get('state', 'unavailable')}`."))
        for name, values in sorted((segments.get("segments") or {}).items()):
            if isinstance(values, Mapping) and values.get("state") == "compared":
                before, after, changes = values.get("before"), values.get("after"), values.get("changes")
                if isinstance(before, Mapping) and isinstance(after, Mapping) and isinstance(changes, Mapping):
                    before_pages, after_pages = before.get("pages"), after.get("pages")
                    before_issues, after_issues = before.get("issues"), after.get("issues")
                    if not all(type(item) is int for item in (before_pages, after_pages, before_issues, after_issues)):
                        continue
                    lines.append(
                        f"- {name}: pages {before_pages} → {after_pages} "
                        f"({after_pages-before_pages:+}); findings {before_issues} → "
                        f"{after_issues} ({after_issues-before_issues:+}); "
                        + ", ".join(f"{bucket} {changes.get(bucket, 0)}" for bucket in _BUCKETS)
                        + "."
                    )
    by_check = []
    for row in iter_compare_rows(manifest_path, "by_check"):
        by_check.append(row)
        if len(by_check) > 500:
            raise CompareError("report exceeds the 500-check limit")
    if by_check:
        lines.extend(
            (
                "",
                "## Findings by check",
                "",
                "| Check | Entered | Left | Appeared | Disappeared |",
                "|---|---:|---:|---:|---:|",
            )
        )
        for row in by_check:
            check = str(row.get("check", "")).replace("|", "\\|")
            lines.append(
                f"| {check} | {row.get('entered', 0)} | {row.get('left', 0)} | "
                f"{row.get('appeared', 0)} | {row.get('disappeared', 0)} |"
            )
    warnings = manifest.get("warnings")
    if isinstance(warnings, list) and warnings:
        lines.extend(("", "## Comparison caveats", ""))
        lines.extend(f"- {str(warning).replace(chr(10), ' ')}" for warning in warnings[:50])
    report = "\n".join(lines) + "\n"
    if len(report.encode("utf-8")) > max_bytes:
        raise CompareError("report exceeds max_bytes")
    return report


def _facts(con: sqlite3.Connection, before_source: Any, after_source: Any):
    for raw_pair, before_ordinal, raw_before, after_ordinal, raw_after in con.execute("""
        SELECT pairs.document, b.ordinal, b.document, a.ordinal, a.document FROM pairs
        JOIN pages b ON b.side=0 AND b.url=pairs.before_url
        JOIN pages a ON a.side=1 AND a.url=pairs.after_url
        WHERE pairs.state='matched' ORDER BY pairs.before_url
    """):
        pair = json.loads(raw_pair)
        before = _page_facts(_row(before_source, "pages", before_ordinal, raw_before))
        after = _page_facts(_row(after_source, "pages", after_ordinal, raw_after))
        yield {
            **{
                key: pair[key]
                for key in ("before_url", "after_url", "correspondence_key", "kind", "host_changed")
            },
            "before": before,
            "after": after,
            "changed": sorted(name for name in before if before[name] != after[name]),
        }


def _publish(root: Path, destination: Path, *, job_id: str, cancel_event: threading.Event | None) -> None:
    """Atomically publish the complete package without replacing foreign output."""
    reservation = destination.with_name(f".{destination.name}.compare-publish-{job_id}")
    try:
        fd = os.open(reservation, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError:
        _safe_member(reservation)
        if reservation.stat().st_size != 0:
            raise CompareError("comparison publish reservation is malformed")
    else:
        with os.fdopen(fd, "wb") as stream:
            stream.flush()
            os.fsync(stream.fileno())
        fsync_directory(destination.parent)

    if destination.exists():
        _safe_member(destination, directory=True)
        members = list(destination.iterdir())
        if not members:
            raise FileExistsError(f"comparison output directory already exists and is not owned by this job: {destination}")
        # Recover only a prior hard-link publication whose every visible file is still
        # the exact staged inode. Empty or foreign directories are never adopted.
        for member in members:
            staged = root / member.name
            if member.is_symlink() or not member.is_file() or not staged.is_file() or not os.path.samefile(member, staged):
                raise CompareError(f"comparison publish destination contains an unowned member: {member.name}")
        for member in members:
            member.unlink()
        destination.rmdir()
    _check_cancel(cancel_event)
    try:
        _rename_directory_noreplace(root, destination)
        fsync_directory(destination.parent)
        reservation.unlink()
        fsync_directory(destination.parent)
    except BaseException:
        with suppress(OSError):
            if reservation.is_file() and not reservation.is_symlink() and reservation.stat().st_size == 0:
                reservation.unlink()
        raise


def compare_to_files(
    before: Any,
    after: Any,
    *,
    out_dir: str | Path,
    force: bool,
    correspondence: Any,
    compression: str = "none",
    resume: bool = False,
    progress: Callable[[Any], None] | None = None,
    cancel_event: threading.Event | None = None,
) -> dict[str, Any]:
    """Write or resume a disk-backed comparison with a validated output cache."""
    from seohead.core.verification import source_identity
    from seohead.core.filesystem import atomic_write_bytes, lock_exclusive, open_lock, unlock

    destination = Path(out_dir).absolute()
    declaration = _load_correspondence(correspondence) if correspondence is not None else None
    identities = {"before": source_identity(before), "after": source_identity(after)}
    job_id = _job_id(identities, force, declaration, compression)
    destination.parent.mkdir(parents=True, exist_ok=True)
    work = destination.with_name(f".{destination.name}.compare-work")
    if destination.exists():
        if resume and (destination / "compare.json").is_file() is False:
            reservation = destination.with_name(f".{destination.name}.compare-publish-{job_id}")
            if reservation.exists() and work.is_dir() and not work.is_symlink():
                try:
                    _safe_member(reservation)
                    if reservation.stat().st_size != 0:
                        raise CompareError("comparison publish reservation is malformed")
                    _safe_member(work / "output", directory=True)
                    members = list(destination.iterdir())
                    staged = work / "output"
                    owned = bool(members) and all(
                        not member.is_symlink()
                        and member.is_file()
                        and (staged / member.name).is_file()
                        and os.path.samefile(member, staged / member.name)
                        for member in members
                    )
                    if owned:
                        for member in members:
                            member.unlink()
                        destination.rmdir()
                except (OSError, CompareError):
                    pass
        if destination.is_symlink() or not (destination / "compare.json").is_file():
            raise FileExistsError(f"comparison output already exists: {destination}")
        cached_manifest = destination / "compare.json"
        if cached_manifest.is_symlink():
            raise CompareError("comparison manifest must not be a symbolic link")
        cached = json.loads(cached_manifest.read_text(encoding="utf-8"))
        cached_job = cached.get("job") if isinstance(cached, Mapping) else None
        cached_before = cached.get("before") if isinstance(cached, Mapping) else None
        cached_after = cached.get("after") if isinstance(cached, Mapping) else None
        if (
            not isinstance(cached, Mapping)
            or cached.get("schema_version") != "compare.v2"
            or not isinstance(cached_job, Mapping)
            or cached_job.get("id") != job_id
            or not isinstance(cached_before, Mapping)
            or cached_before.get("audit_sha256") != identities["before"]["audit_sha256"]
            or not isinstance(cached_after, Mapping)
            or cached_after.get("audit_sha256") != identities["after"]["audit_sha256"]
            or cached.get("force") is not force
            or cached.get("compression", "none") != compression
        ):
            raise FileExistsError(f"comparison output already exists for different inputs: {destination}")
        for name in cached.get("files", {}):
            for _row_data in iter_compare_rows(destination / "compare.json", name):
                pass
        _discard_completed_checkpoint(work, job_id)
        reservation = destination.with_name(f".{destination.name}.compare-publish-{job_id}")
        with suppress(OSError, ValueError, TypeError):
            _safe_member(reservation, optional=True)
            if reservation.exists() and reservation.stat().st_size == 0:
                reservation.unlink()
        _notify(progress, job_id=job_id, state="finished", stage="cached", completed=1, total=1)
        return {**cached, "manifest": str(destination / "compare.json"), "out_dir": str(destination), "cache_hit": True}

    if work.exists() and not resume:
        raise FileExistsError(f"comparison checkpoint exists; pass resume=True to continue: {work}")
    if work.is_symlink() or (work.exists() and not work.is_dir()):
        raise CompareError("comparison checkpoint path must be a regular directory")
    if resume and not work.exists():
        raise FileNotFoundError(f"no comparison checkpoint exists to resume: {work}")
    if not work.exists():
        work.mkdir()
        (work / "output").mkdir()
        initial_job = {
                "schema_version": "compare.job.v1",
                "job_id": job_id,
                "identities": identities,
                "force": force,
                "compression": compression,
                "correspondence_sha256": hashlib.sha256(_json(declaration).encode("utf-8")).hexdigest() if declaration else None,
                "files": {},
                "state": "running",
                "stage": "indexing",
            }
        atomic_write_bytes(work / "job.json", (_json(initial_job) + "\n").encode("utf-8"))
    _safe_member(work, directory=True)
    work_info = work.lstat()
    job_path = work / "job.json"
    succeeded = False
    try:
        _safe_member(job_path)
        job = json.loads(job_path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise CompareError("comparison checkpoint is unreadable; it was preserved") from exc
    if not isinstance(job, Mapping) or job.get("schema_version") != "compare.job.v1" or job.get("job_id") != job_id:
        raise CompareError("comparison checkpoint belongs to different inputs or options")
    lock_path = work / ".writer.lock"
    _safe_member(lock_path, optional=True)
    lock_fd = open_lock(lock_path)
    try:
        lock_exclusive(lock_fd)
    except OSError as exc:
        os.close(lock_fd)
        raise CompareError("comparison job already has an active writer") from exc

    def checkpoint(state: str, stage: str, **extra: Any) -> None:
        _check_cancel(cancel_event)
        job.update(state=state, stage=stage, updated_at=__import__("datetime").datetime.now(__import__("datetime").timezone.utc).isoformat(), **extra)
        atomic_write_bytes(job_path, (_json(job) + "\n").encode("utf-8"), durable=True)

    def report(state: str, stage: str, completed: int | None, total: int | None, reason: str | None = None) -> None:
        _check_cancel(cancel_event)
        checkpoint(state, stage, completed=completed, total=total, reason=reason)
        _notify(progress, job_id=job_id, state=state, stage=stage, completed=completed, total=total, reason=reason)

    def persist_progress(update: Any) -> None:
        job.update(
            state=update.state,
            stage=update.stage,
            completed=update.completed,
            total=update.total,
            reason=update.reason,
        )
        atomic_write_bytes(job_path, (_json(job) + "\n").encode("utf-8"), durable=True)
        if progress is not None:
            progress(update)

    try:
        _safe_member(work / "output", directory=True)
        _safe_member(work / "index.sqlite", optional=True)
        for suffix in ("-journal", "-wal", "-shm"):
            _safe_member(work / f"index.sqlite{suffix}", optional=True)
        for member in (work / "output").iterdir():
            _safe_member(member, max_links=2)
        root = work / "output"
        db_path = work / "index.sqlite"
        before_defs, after_defs = _segment_definitions(before), _segment_definitions(after)
        compatible_segments = before_defs == after_defs
        from seohead.sf.core.segments import resolve_order

        before_segments = resolve_order(before_defs) if before_defs else []
        after_segments = resolve_order(after_defs) if after_defs else []
        counts = {(side, name): _collection_count(source, name)
                  for side, source in ((0, before), (1, after)) for name in ("pages", "issues")}
        total_index = None if any(value is None for value in counts.values()) else sum(counts.values())
        with closing(sqlite3.connect(db_path)) as con:
            con.execute("PRAGMA temp_store=FILE")
            con.execute("PRAGMA cache_size=-8192")
            con.execute("PRAGMA synchronous=FULL")
            existing = con.execute("SELECT name FROM sqlite_master WHERE type='table' AND name='job_state'").fetchone()
            if not existing:
                con.executescript("""
                    CREATE TABLE job_state(key TEXT PRIMARY KEY,value TEXT NOT NULL);
                    CREATE TABLE pages(side INTEGER,url TEXT,comparison_url TEXT,ordinal INTEGER,document TEXT,segment TEXT NOT NULL,PRIMARY KEY(side,url));
                    CREATE TABLE issues(side INTEGER,check_key TEXT,url TEXT,comparison_url TEXT,ordinal INTEGER,document TEXT,PRIMARY KEY(side,check_key,url));
                    CREATE TABLE pairs(before_url TEXT PRIMARY KEY,after_url TEXT,state TEXT,document TEXT);
                    CREATE TABLE delta(bucket TEXT,check_key TEXT,url TEXT,issue_rowid INTEGER,segment TEXT NOT NULL,PRIMARY KEY(issue_rowid));
                """)
                _set_state(con, "job_id", job_id)
                _set_state(con, "phase", "indexing")
                con.commit()
            elif _state_value(con, "job_id") != job_id:
                raise CompareError("comparison checkpoint index belongs to different inputs")

            phase = _state_value(con, "phase", "indexing")
            valid_phases = {"indexing", "binding", "classify_before", "classify_after", "export"}
            if phase not in valid_phases:
                raise CompareError("comparison checkpoint has an invalid phase")
            for side, source in ((0, before), (1, after)):
                for name in ("pages", "issues"):
                    total = counts[(side, name)]
                    offset = _state_value(con, f"indexed:{side}:{name}", 0)
                    if type(offset) is not int or offset < 0 or (total is not None and offset > total):
                        raise CompareError("comparison checkpoint has an invalid source cursor")
                    actual, maximum = con.execute(
                        f"SELECT count(*),COALESCE(max(ordinal),-1) FROM {name} WHERE side=?", (side,)
                    ).fetchone()
                    if actual != offset or maximum >= offset:
                        raise CompareError("comparison checkpoint cursor does not match indexed rows")
                total_issues = counts[(side, "issues")]
                cursor = _state_value(con, f"classified:{side}", 0)
                actual = con.execute("SELECT count(*) FROM issues WHERE side=? AND ordinal<?", (side, cursor if type(cursor) is int else 0)).fetchone()[0]
                if type(cursor) is not int or cursor < 0 or (total_issues is not None and cursor > total_issues) or actual != min(cursor, total_issues or cursor):
                    raise CompareError("comparison checkpoint has an invalid classification cursor")
            if phase != "indexing":
                for side in (0, 1):
                    for name in ("pages", "issues"):
                        if _state_value(con, f"indexed:{side}:{name}", 0) != counts[(side, name)]:
                            raise CompareError("comparison checkpoint phase skips unfinished source indexing")
            if phase in {"classify_before", "classify_after", "export"}:
                expected_before = counts[(0, "issues")]
                if _state_value(con, "classified:0", 0) != expected_before:
                    raise CompareError("comparison checkpoint skips unfinished before-side classification")
            if phase in {"classify_after", "export"}:
                expected_after = counts[(1, "issues")]
                if _state_value(con, "classified:1", 0) != expected_after:
                    raise CompareError("comparison checkpoint skips unfinished after-side classification")
            if phase == "export":
                before_count, after_count = counts[(0, "issues")], counts[(1, "issues")]
                delta_count = con.execute("SELECT count(*) FROM delta").fetchone()[0]
                matched = con.execute(
                    "SELECT count(*) FROM issues b JOIN issues a ON a.side=1 AND a.check_key=b.check_key AND a.comparison_url=b.comparison_url WHERE b.side=0"
                ).fetchone()[0]
                if before_count + after_count != delta_count + 2 * matched:
                    raise CompareError("comparison checkpoint classification does not conserve findings")
            if phase == "indexing":
                completed = 0
                order = ((0, before, "pages", before_segments), (0, before, "issues", before_segments),
                         (1, after, "pages", after_segments), (1, after, "issues", after_segments))
                for side, source, name, ordered_segments in order:
                    base = completed
                    done = _index_collection(
                        con, source, side, name, ordered_segments,
                        checkpoint_key=f"indexed:{side}:{name}", progress=persist_progress,
                        job_id=job_id, progress_base=base, progress_total=total_index,
                        cancel_event=cancel_event,
                    )
                    completed += done
                _set_state(con, "phase", "binding")
                con.commit()
                phase = "binding"
            if phase == "binding":
                report("running", "binding", 0, 1)
                con.execute("BEGIN")
                if declaration is not None:
                    con.execute("DELETE FROM pairs")
                    con.execute("UPDATE pages SET comparison_url=url")
                    con.execute("UPDATE issues SET comparison_url=url")
                    page_total = counts[(0, "pages")]
                    _bind(
                        con,
                        declaration,
                        progress=persist_progress,
                        job_id=job_id,
                        total=page_total,
                        cancel_event=cancel_event,
                    )
                con.execute("CREATE UNIQUE INDEX IF NOT EXISTS issue_identity ON issues(side,check_key,comparison_url)")
                con.execute("CREATE INDEX IF NOT EXISTS page_identity ON pages(side,comparison_url)")
                con.execute("CREATE INDEX IF NOT EXISTS issue_ordinal ON issues(side,ordinal)")
                _set_state(con, "phase", "classify_before")
                con.commit()
                phase = "classify_before"

            for side, label in ((0, "before"), (1, "after")):
                stage = f"classify_{label}"
                if _state_value(con, "phase") != stage:
                    continue
                total = counts[(side, "issues")]
                offset = int(_state_value(con, f"classified:{side}", 0))
                partial_other = bool(_run(after if side == 0 else before).get("crawl_partial"))
                present_bucket, absent_bucket = (("left", "disappeared") if side == 0 else ("entered", "appeared"))
                while True:
                    _check_cancel(cancel_event)
                    rows = con.execute(
                        "SELECT ordinal FROM issues WHERE side=? AND ordinal>=? ORDER BY ordinal LIMIT ?",
                        (side, offset, _INDEX_BATCH),
                    ).fetchall()
                    if not rows:
                        break
                    con.execute("BEGIN")
                    next_offset = rows[-1][0] + 1
                    segment_fallback = (
                        "unavailable" if not compatible_segments or before_defs else "unsegmented"
                    )
                    con.execute(
                        "INSERT INTO delta(bucket,check_key,url,issue_rowid,segment) "
                        "SELECT CASE WHEN i.url='' OR ? OR op.url IS NOT NULL THEN ? ELSE ? END, "
                        "i.check_key,i.url,i.rowid,CASE WHEN ? THEN 'unavailable' ELSE COALESCE(p.segment,?) END FROM issues i "
                        "LEFT JOIN pages p ON p.side=i.side AND p.url=i.url "
                        "LEFT JOIN pages op ON op.side=? AND op.comparison_url=i.comparison_url "
                        "WHERE i.side=? AND i.ordinal>=? AND i.ordinal<? AND NOT EXISTS ("
                        "SELECT 1 FROM issues j WHERE j.side=? AND j.check_key=i.check_key "
                        "AND j.comparison_url=i.comparison_url)",
                        (
                            partial_other,
                            present_bucket,
                            absent_bucket,
                            not compatible_segments,
                            segment_fallback,
                            1 - side,
                            side,
                            offset,
                            next_offset,
                            1 - side,
                        ),
                    )
                    offset = next_offset
                    _set_state(con, f"classified:{side}", offset)
                    if total is not None and offset >= total:
                        _set_state(con, "phase", "classify_after" if side == 0 else "export")
                    con.commit()
                    report("running", stage, offset, total)
                if _state_value(con, "phase") == stage:
                    _set_state(con, "phase", "classify_after" if side == 0 else "export")
                    con.commit()
            con.execute("CREATE INDEX IF NOT EXISTS delta_order ON delta(bucket,check_key,url)")
            con.execute("CREATE INDEX IF NOT EXISTS delta_segment ON delta(segment,bucket,check_key,url)")
            con.commit()

            output_root = root
            files: dict[str, Any] = dict(job.get("files") or {})

            def export(name: str, rows: Iterable[Any], *, total: int | None = None) -> None:
                info = files.get(name)
                path = output_root / f"{name}.ndjson{'.gz' if compression == 'gzip' else ''}"
                if info and path.is_file() and not path.is_symlink():
                    digest, size = _file_sha256(path)
                    if digest == info.get("sha256") and size == info.get("bytes"):
                        return
                if path.exists():
                    info = path.lstat()
                    if not path.is_file() or path.is_symlink() or info.st_nlink != 1:
                        raise CompareError("uncheckpointed comparison output file is unsafe to replace")
                    path.unlink()
                report("running", f"export_{name}", 0, total)
                latest = 0
                def on_progress(count: int) -> None:
                    nonlocal latest
                    _check_cancel(cancel_event)
                    latest = count
                    report("running", f"export_{name}", count, total)
                _check_cancel(cancel_event)
                files[name] = _export(output_root, name, rows, compression=compression, on_progress=on_progress, cancel_event=cancel_event)
                checkpoint("running", "export", files=files, completed_file=name, completed_rows=latest)

            for bucket in _BUCKETS[:-1]:
                total = con.execute("SELECT COUNT(*) FROM delta WHERE bucket=?", (bucket,)).fetchone()[0]
                export(
                    bucket,
                    (_row((before, after)[row[0]], "issues", row[1], row[2])
                     for row in con.execute(
                         "SELECT i.side,i.ordinal,i.document FROM delta d JOIN issues i ON i.rowid=d.issue_rowid WHERE d.bucket=? ORDER BY d.check_key,d.url",
                         (bucket,),
                     )),
                    total=total,
                )
            unchanged_total = con.execute(
                "SELECT COUNT(*) FROM issues b JOIN issues a ON a.side=1 AND a.check_key=b.check_key AND a.comparison_url=b.comparison_url WHERE b.side=0"
            ).fetchone()[0]
            export(
                "unchanged",
                ({"before": _row(before, "issues", row[0], row[1]),
                  "after": _row(after, "issues", row[2], row[3])}
                 for row in con.execute(
                     "SELECT b.ordinal,b.document,a.ordinal,a.document FROM issues b JOIN issues a ON a.side=1 AND a.check_key=b.check_key AND a.comparison_url=b.comparison_url WHERE b.side=0 ORDER BY b.check_key,b.url"
                 )),
                total=unchanged_total,
            )
            export(
                "by_check",
                ({"check": row[0], **dict(zip(_BUCKETS[:-1], row[1:], strict=True))}
                 for row in con.execute(
                     "SELECT check_key,sum(bucket='entered'),sum(bucket='left'),sum(bucket='appeared'),sum(bucket='disappeared') FROM delta GROUP BY check_key ORDER BY check_key"
                 )),
            )
            segment_rows_total = con.execute("SELECT COUNT(*) FROM delta").fetchone()[0] + unchanged_total
            def segment_changes():
                for row in con.execute(
                    "SELECT d.bucket,d.segment,i.side,i.ordinal,i.document FROM delta d JOIN issues i ON i.rowid=d.issue_rowid ORDER BY d.segment,d.bucket,d.check_key,d.url"
                ):
                    yield {"segment": row[1], "change": row[0], "issue": _row((before, after)[row[2]], "issues", row[3], row[4])}
                for row in con.execute(
                    "SELECT p.segment,b.ordinal,b.document,a.ordinal,a.document FROM issues b JOIN issues a ON a.side=1 AND a.check_key=b.check_key AND a.comparison_url=b.comparison_url LEFT JOIN pages p ON p.side=1 AND p.url=a.url WHERE b.side=0 ORDER BY p.segment,b.check_key,b.url"
                ):
                    segment = row[0] or ("unsegmented" if not before_defs and compatible_segments else "unavailable")
                    if not compatible_segments:
                        segment = "unavailable"
                    yield {"segment": segment, "change": "unchanged",
                           "before": _row(before, "issues", row[1], row[2]),
                           "after": _row(after, "issues", row[3], row[4])}
            export("segment_changes", segment_changes(), total=segment_rows_total)
            if declaration is not None:
                export("correspondence", (json.loads(row[0]) for row in con.execute("SELECT document FROM pairs ORDER BY before_url")))
                export("facts", _facts(con, before, after))

            counts = {bucket: files[bucket]["rows"] for bucket in _BUCKETS}
            totals = {}
            page_totals = {}
            for side, label in ((0, "before"), (1, "after")):
                totals[label] = con.execute("SELECT count(*) FROM issues WHERE side=?", (side,)).fetchone()[0]
                page_totals[label] = con.execute("SELECT count(*) FROM pages WHERE side=?", (side,)).fetchone()[0]
                identities[label]["urls_crawled"] = page_totals[label]
            conservation = {
                "before_issues": totals["before"],
                "after_issues": totals["after"],
                "before_accounted": counts["left"] + counts["disappeared"] + counts["unchanged"],
                "after_accounted": counts["entered"] + counts["appeared"] + counts["unchanged"],
            }
            if conservation["before_issues"] != conservation["before_accounted"] or conservation["after_issues"] != conservation["after_accounted"]:
                raise CompareError("comparison conservation failed")

            segment_summary = {"unsegmented": {"before": {"pages": 0, "issues": 0}, "after": {"pages": 0, "issues": 0}, "changes": dict.fromkeys(_BUCKETS, 0)}}
            if compatible_segments:
                for definition in before_defs:
                    name = str(definition.get("name", ""))
                    if name:
                        segment_summary.setdefault(name, {"before": {"pages": 0, "issues": 0}, "after": {"pages": 0, "issues": 0}, "changes": dict.fromkeys(_BUCKETS, 0)})
            for side, label in ((0, "before"), (1, "after")):
                for segment, pages in con.execute("SELECT segment,count(*) FROM pages WHERE side=? GROUP BY segment", (side,)):
                    segment_summary.setdefault(segment, {"before": {"pages": 0, "issues": 0}, "after": {"pages": 0, "issues": 0}, "changes": dict.fromkeys(_BUCKETS, 0)})[label]["pages"] = pages
                for segment, issues in con.execute(
                    "SELECT COALESCE(p.segment,'unavailable'),count(*) FROM issues i LEFT JOIN pages p ON p.side=i.side AND p.url=i.url WHERE i.side=? GROUP BY COALESCE(p.segment,'unavailable')", (side,)
                ):
                    name = segment or "unavailable"
                    segment_summary.setdefault(name, {"before": {"pages": 0, "issues": 0}, "after": {"pages": 0, "issues": 0}, "changes": dict.fromkeys(_BUCKETS, 0)})[label]["issues"] = issues
            for segment, bucket, amount in con.execute("SELECT segment,bucket,count(*) FROM delta GROUP BY segment,bucket"):
                segment_summary.setdefault(segment, {"before": {"pages": 0, "issues": 0}, "after": {"pages": 0, "issues": 0}, "changes": dict.fromkeys(_BUCKETS, 0)})["changes"][bucket] = amount
            for segment, amount in con.execute(
                "SELECT COALESCE(p.segment,'unavailable'),count(*) FROM issues b JOIN issues a ON a.side=1 AND a.check_key=b.check_key AND a.comparison_url=b.comparison_url LEFT JOIN pages p ON p.side=1 AND p.url=a.url WHERE b.side=0 GROUP BY COALESCE(p.segment,'unavailable')"
            ):
                segment_summary.setdefault(segment or "unavailable", {"before": {"pages": 0, "issues": 0}, "after": {"pages": 0, "issues": 0}, "changes": dict.fromkeys(_BUCKETS, 0)})["changes"]["unchanged"] = amount
            if compatible_segments and before_defs:
                for name, values in segment_summary.items():
                    values["state"] = "unavailable" if name == "unavailable" else "compared"

            warnings = list(dict.fromkeys(preflight(before, after) + comparison_warnings(_header(before), _header(after))))
            result = {
                "schema_version": "compare.v2", **identities, "force": force,
                "compression": compression,
                "warnings": warnings,
                "compatibility": comparison_compatibility(_header(before), _header(after)),
                "summary": counts,
                "segment_breakdown": {
                    "state": "compared" if compatible_segments and before_defs else "unavailable",
                    **({} if compatible_segments and before_defs else {"reason": "both runs must retain matching named segment definitions"}),
                    "whole_site": {"before": {"pages": page_totals["before"], "issues": totals["before"]},
                                   "after": {"pages": page_totals["after"], "issues": totals["after"]},
                                   "changes": counts},
                    "segments": segment_summary if compatible_segments and before_defs else {},
                },
                "conservation": {**conservation, "state": "complete"},
                "policies": {
                    "identity": "exact check and target URL; declared correspondence only",
                    "duplicates": "reject duplicate page URLs, finding keys, and correspondence collisions",
                    "suppression": "all retained findings, including suppressed rows; no view filtering",
                    "unchanged": "same finding key; both evidence rows retained, not an evidence-equality assertion",
                    "partial": "partial or incompatible observations remain warned deltas, not verified fixes",
                    "resume": "source identity and comparison options must match the persistent checkpoint",
                },
                "files": files,
                "job": {"id": job_id, "state": "finished", "resumable": False, "cache_hit": False},
            }
            gaps = _measurement_gaps(before, after)
            if gaps:
                result["measurement_gaps"] = gaps
            if declaration is not None:
                result["correspondence"] = {"schema_version": "url-correspondence.v1", "origin_map": declaration["origin_map"]}

            manifest_path = root / "compare.json"
            staged = root / ".compare.json.staged"
            if manifest_path.exists():
                info = manifest_path.lstat()
                if manifest_path.is_symlink() or not manifest_path.is_file() or info.st_nlink != 1:
                    raise CompareError("checkpoint manifest path is unsafe to replace")
                manifest_path.unlink()
            if staged.exists():
                info = staged.lstat()
                if staged.is_symlink() or not staged.is_file() or info.st_nlink != 1:
                    raise CompareError("checkpoint staging path is unsafe to replace")
                staged.unlink()
            with staged.open("xb") as stream:
                stream.write((_json(result) + "\n").encode("utf-8"))
                stream.flush()
                os.fsync(stream.fileno())
            os.link(staged, manifest_path)
            staged.unlink()
            fsync_directory(root)
            report("running", "publishing", 0, 1)
            _check_cancel(cancel_event)
            _publish(root, destination, job_id=job_id, cancel_event=cancel_event)
            checkpoint("finished", "complete", files=files, completed=1, total=1)
            _notify(progress, job_id=job_id, state="finished", stage="complete", completed=1, total=1)
            succeeded = True
        return {**result, "manifest": str(destination / "compare.json"), "out_dir": str(destination), "cache_hit": False}
    except BaseException as exc:
        with suppress(Exception):
            checkpoint("failed", str(job.get("stage", "compare")), reason=str(exc)[:500])
        _notify(progress, job_id=job_id, state="failed", stage=str(job.get("stage", "compare"))[:64], completed=job.get("completed"), total=job.get("total"), reason=str(exc)[:500])
        raise
    finally:
        unlock(lock_fd)
        os.close(lock_fd)
        if succeeded:
            with suppress(OSError):
                if os.path.samestat(work_info, work.lstat()):
                    shutil.rmtree(work)
