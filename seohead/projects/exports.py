"""Bounded project task and scan-comparison exports with durable progress/history."""

from __future__ import annotations

import csv
import contextlib
import hashlib
import json
import os
import re
import shutil
import threading
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterator

from seohead.core.filesystem import atomic_write_bytes

from .runtime import read_document, write_document
from .workspace import _load

HISTORY_NAME = "exports.json"
HISTORY_FORMAT = "seohead.project-exports.v1"
JOB_DIR = Path("reports/.export-jobs")
MAX_EXPORT_ROWS = 10_000_000
MAX_EXPORT_BYTES = 4 * 1024 * 1024 * 1024
XLSX_CELL_LIMIT = 32_767
_ACTIVE: dict[str, threading.Thread] = {}
_ACTIVE_LOCK = threading.RLock()

TASK_COLUMNS = (
    "task_id",
    "title",
    "kind",
    "source_hash",
    "operation",
    "state",
    "attempt_status",
    "assignee",
    "description",
    "acceptance_criterion",
    "finding_ref",
    "method_ref",
    "competitors",
    "affected_url_count",
    "affected_url_state",
    "affected_url_reason",
    "actor",
    "complete",
    "reason",
)
COMPARE_COLUMNS = ("category", "check", "severity", "target_url", "row_json")
TASK_COMMENT_COLUMNS = ("task_id", "comment_ordinal", "created_at", "actor", "text", "reference")
TASK_HISTORY_COLUMNS = (
    "task_id", "event_type", "event_ordinal", "occurred_at", "revision", "actor", "status", "entry_json"
)


class ProjectExportError(ValueError):
    """A project export is invalid, incomplete, or could not be published."""


def _now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def _safe_cell(value: Any) -> str:
    if value is None:
        return ""
    if isinstance(value, (dict, list)):
        value = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    text = str(value)
    if text.startswith(("=", "+", "-", "@", "\t", "\r")):
        return "'" + text
    return text


def _job_path(root: Path, job_id: str, *, create: bool = False) -> Path:
    try:
        normalized = str(uuid.UUID(job_id))
    except (TypeError, ValueError) as exc:
        raise ProjectExportError("job_id must be a UUID") from exc
    reports = root / "reports"
    if reports.is_symlink() or not reports.is_dir():
        raise ProjectExportError("project reports directory is unavailable or unsafe")
    directory = root / JOB_DIR
    if directory.is_symlink() or (directory.exists() and not directory.is_dir()):
        raise ProjectExportError("unsafe export-job directory")
    if create:
        directory.mkdir(parents=True, mode=0o700, exist_ok=True)
    elif not directory.is_dir():
        raise ProjectExportError("export job is unavailable")
    return directory / f"{normalized}.json"


def _acquire_job_lock(root: Path, job_id: str, *, create: bool) -> tuple[int | None, bool]:
    """Use a portable advisory lock so status and retries can distinguish live workers."""
    job_path = _job_path(root, job_id, create=create)
    path = job_path.with_suffix(".lock")
    if not create and not path.exists():
        return None, False
    if path.is_symlink():
        raise ProjectExportError("unsafe export lock path")
    flags = os.O_RDWR | (os.O_CREAT if create else 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags, 0o600)
    try:
        if os.name == "nt":
            import msvcrt

            if os.fstat(descriptor).st_size == 0:
                if not create:
                    os.close(descriptor)
                    return None, False
                os.write(descriptor, b"\0")
            os.lseek(descriptor, 0, os.SEEK_SET)
            try:
                msvcrt.locking(descriptor, msvcrt.LK_NBLCK, 1)
            except OSError:
                os.close(descriptor)
                return None, True
        else:
            import fcntl

            try:
                fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                os.close(descriptor)
                return None, True
        return descriptor, False
    except BaseException:
        with contextlib.suppress(OSError):
            os.close(descriptor)
        raise


def _release_job_lock(descriptor: int | None) -> None:
    if descriptor is None:
        return
    try:
        if os.name == "nt":
            import msvcrt

            os.lseek(descriptor, 0, os.SEEK_SET)
            msvcrt.locking(descriptor, msvcrt.LK_UNLCK, 1)
        else:
            import fcntl

            fcntl.flock(descriptor, fcntl.LOCK_UN)
    finally:
        os.close(descriptor)


def _read_job(root: Path, job_id: str) -> dict[str, Any]:
    path = _job_path(root, job_id, create=False)
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 64 * 1024:
        raise ProjectExportError("export job is unavailable")
    value = json.loads(path.read_text(encoding="utf-8"))
    if (
        not isinstance(value, dict)
        or value.get("record_format") != "seohead.project-export-job.v1"
        or value.get("job_id") != str(uuid.UUID(job_id))
    ):
        raise ProjectExportError("export job has an invalid record")
    return value


def _write_job(root: Path, job: dict[str, Any]) -> None:
    path = _job_path(root, job["job_id"], create=True)
    content = (json.dumps(job, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode()
    atomic_write_bytes(path, content)


def _check_cancel(root: Path, job: dict[str, Any]) -> None:
    cancel_path = _job_path(root, job["job_id"], create=False).with_suffix(".cancel")
    if cancel_path.is_file() and not cancel_path.is_symlink():
        raise _ExportCancelled("export cancelled by the caller")


def _artifact_paths(root: Path, job: dict[str, Any]) -> tuple[Path, Path]:
    output = job.get("output")
    job_id = str(uuid.UUID(job["job_id"]))
    temporary_name = job.get("temporary", f".{job_id}.partial")
    if (
        not isinstance(output, str)
        or Path(output).name != output
        or output.startswith(".")
        or not isinstance(temporary_name, str)
        or Path(temporary_name).name != temporary_name
        or temporary_name != f".{job_id}.partial"
    ):
        raise ProjectExportError("export job has unsafe output paths")
    reports = root / "reports"
    if reports.is_symlink() or not reports.is_dir():
        raise ProjectExportError("project reports directory is unavailable or unsafe")
    return reports / output, reports / temporary_name


def _matches_artifact(path: Path, result: Any) -> bool:
    if (
        not isinstance(result, dict)
        or not isinstance(result.get("path"), str)
        or type(result.get("bytes")) is not int
        or not 0 <= result["bytes"] <= MAX_EXPORT_BYTES
        or not isinstance(result.get("sha256"), str)
        or not re.fullmatch(r"[a-f0-9]{64}", result["sha256"])
        or path.is_symlink()
        or not path.is_file()
    ):
        return False
    try:
        if path.stat().st_size != result["bytes"]:
            return False
        digest = hashlib.sha256()
        with path.open("rb") as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(block)
        return digest.hexdigest() == result["sha256"]
    except OSError:
        return False


def _open_temporary(path: Path, *, text: bool = False):
    descriptor = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    return (
        os.fdopen(descriptor, "w", encoding="utf-8", newline="")
        if text
        else os.fdopen(descriptor, "wb")
    )


class _ExportCancelled(Exception):
    pass


def _known_scan_uuids(root: Path) -> set[str]:
    from seohead.storage.history import list_scans
    from .runtime import preparation_status

    directories = [root / "scans"]
    prep = preparation_status(root)
    for row in prep.get("competitors", []) or []:
        relative = row.get("directory") if isinstance(row, dict) else None
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
        try:
            child_root, child_project = _load(child)
        except (OSError, ValueError):
            continue
        if (
            child_root != child.resolve()
            or child_project.get("project_uuid") != row.get("project_uuid")
        ):
            continue
        directories.append(child_root / "scans")
    known: set[str] = set()
    for directory in directories:
        if not directory.is_dir():
            continue
        offset = 0
        while True:
            page = list_scans(directory, offset=offset, limit=100)
            if page.get("errors"):
                raise ProjectExportError("one or more retained scan records are unavailable")
            known.update(row["uuid"] for row in page["items"] if row.get("uuid"))
            offset += len(page["items"])
            if not page["has_more"]:
                break
    return known


def _compare_source(root: Path, manifest_path: str) -> tuple[Path, dict[str, Any], list[str]]:
    path = Path(manifest_path)
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 16 * 1024 * 1024:
        raise ProjectExportError("compare manifest must be a bounded regular local file")
    try:
        manifest = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise ProjectExportError(f"cannot read compare manifest: {exc}") from exc
    if not isinstance(manifest, dict) or manifest.get("schema_version") != "compare.v2":
        raise ProjectExportError("compare dataset requires a compare.v2 manifest")
    sources = [manifest.get("before"), manifest.get("after")]
    if any(not isinstance(source, dict) or not source.get("scan_uuid") for source in sources):
        raise ProjectExportError("compare manifest has no exact retained scan identities")
    known = _known_scan_uuids(root)
    unknown = [source["scan_uuid"] for source in sources if source["scan_uuid"] not in known]
    if unknown:
        raise ProjectExportError("both compare scans must belong to this project or a competitor workspace")
    files = manifest.get("files")
    if not isinstance(files, dict):
        raise ProjectExportError("compare manifest has no bounded dataset files")
    names = sorted(name for name, item in files.items() if isinstance(item, dict))
    if not names:
        raise ProjectExportError("compare manifest contains no datasets")
    if sum(int(files[name].get("rows", 0)) for name in names) > MAX_EXPORT_ROWS:
        raise ProjectExportError(f"comparison exceeds the {MAX_EXPORT_ROWS}-row export bound")
    return path, manifest, names


def _task_rows(directory: str) -> tuple[list[dict[str, Any]], int]:
    from .catalogue import load_catalogue
    from .coverage import _read, _status
    from .progress import _item_state

    root, project = _load(directory)
    document = _read(root, project)
    if document is None:
        raise ProjectExportError("initialize the project checklist before exporting tasks")
    status = _status(root, document, load_catalogue(), project)
    if status.get("state") != "initialized":
        raise ProjectExportError("initialize the project checklist before exporting tasks")
    status_by_id = {item["id"]: item for item in status.get("items", [])}
    rows = []
    for item_id, stored in sorted(document["items"].items()):
        item = status_by_id.get(item_id)
        if item is None:
            continue
        definition = stored["definition"]
        task = item.get("task") or {}
        affected = item.get("affected_urls") or {}
        rows.append(
            {
                "task_id": item_id,
                "title": item.get("title"),
                "kind": definition.get("kind"),
                "source_hash": definition.get("source_hash"),
                "operation": definition.get("operation"),
                "state": _item_state(item),
                "attempt_status": item.get("attempt_status"),
                "assignee": task.get("assignee"),
                "description": task.get("description"),
                "acceptance_criterion": task.get("acceptance_criterion"),
                "finding_ref": task.get("finding_ref"),
                "method_ref": task.get("method_ref"),
                "competitors": task.get("competitors") or [],
                "affected_url_count": affected.get("count"),
                "affected_url_state": affected.get("state", "not_measured"),
                "affected_url_reason": affected.get("reason"),
                "actor": item.get("actor"),
                "complete": bool(item.get("complete")),
                "reason": item.get("reason"),
            }
        )
    return rows, status["revision"]


def _task_related_rows(
    directory: str, dataset: str
) -> tuple[Iterator[dict[str, Any]], int, dict[str, Any]]:
    """Stream comments or append-only definition/attempt history from validated coverage."""
    from .coverage import _read

    root, project = _load(directory)
    document = _read(root, project)
    if document is None:
        raise ProjectExportError("initialize the project checklist before exporting task history")
    if dataset == "task_comments":
        total = sum(len(item.get("comments", [])) for item in document["items"].values())

        def iterate_comments() -> Iterator[dict[str, Any]]:
            for task_id, item in sorted(document["items"].items()):
                for ordinal, comment in enumerate(item.get("comments", []), 1):
                    yield {
                        "task_id": task_id,
                        "comment_ordinal": ordinal,
                        "created_at": comment["created_at"],
                        "actor": comment["actor"],
                        "text": comment["text"],
                        "reference": comment.get("reference"),
                    }

        return iterate_comments(), total, {"checklist_revision": document["revision"]}

    if dataset != "task_history":
        raise ProjectExportError("dataset must be tasks, task_comments, task_history, or compare")
    total = sum(
        len(item["definitions"]) + len(item["records"])
        for item in document["items"].values()
    )

    def iterate_history() -> Iterator[dict[str, Any]]:
        for task_id, item in sorted(document["items"].items()):
            events = [
                (version["observed_at"], 0, "definition", version)
                for version in item["definitions"]
            ] + [
                (record["recorded_at"], 1, "attempt", record)
                for record in item["records"]
            ]
            events.sort(key=lambda event: (event[0], event[1]))
            for ordinal, (occurred_at, _tie_break, event_type, entry) in enumerate(events, 1):
                is_attempt = event_type == "attempt"
                yield {
                    "task_id": task_id,
                    "event_type": event_type,
                    "event_ordinal": ordinal,
                    "occurred_at": occurred_at,
                    "revision": entry.get("revision") if is_attempt else None,
                    "actor": (entry.get("actor") or entry.get("reviewer")) if is_attempt else None,
                    "status": entry.get("status") if is_attempt else None,
                    "entry_json": entry,
                }

    return iterate_history(), total, {"checklist_revision": document["revision"]}


def _rows(directory: str, dataset: str, compare_manifest: str | None):
    if dataset == "tasks":
        rows, revision = _task_rows(directory)
        return rows, len(rows), {"checklist_revision": revision}
    if dataset in {"task_comments", "task_history"}:
        return _task_related_rows(directory, dataset)
    if dataset != "compare" or compare_manifest is None:
        raise ProjectExportError("dataset must be tasks, task_comments, task_history, or compare with a compare_manifest")
    root, _ = _load(directory)
    path, manifest, names = _compare_source(root, compare_manifest)
    from seohead.sf.core.compare_store import iter_compare_rows

    total = sum(int(manifest["files"][name]["rows"]) for name in names)

    def iterate() -> Iterator[dict[str, Any]]:
        for name in names:
            for record in iter_compare_rows(path, name):
                yield {
                    "category": name,
                    "check": record.get("check") or record.get("issue", {}).get("check"),
                    "severity": record.get("severity") or record.get("issue", {}).get("severity"),
                    "target_url": record.get("target_url") or record.get("issue", {}).get("target_url"),
                    "row_json": record,
                }

    return iterate(), total, {
        "before": manifest["before"],
        "after": manifest["after"],
        "compare_manifest_sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
    }


def _write_rows(
    root: Path,
    job: dict[str, Any],
    rows: Iterator[dict[str, Any]],
    total: int,
    columns: tuple[str, ...],
) -> dict[str, Any]:
    out_dir = root / "reports"
    if out_dir.is_symlink() or not out_dir.is_dir():
        raise ProjectExportError("project reports directory is unavailable or unsafe")
    target, temporary = _artifact_paths(root, job)
    if target.parent != out_dir or target.is_symlink() or os.path.lexists(target):
        raise ProjectExportError("export output must be a new file directly under reports/")
    suffix = {"json": ".json", "csv": ".csv", "xlsx": ".xlsx"}[job["format"]]
    if target.suffix.casefold() != suffix:
        raise ProjectExportError(f"output extension must be {suffix}")
    if temporary.is_symlink() or os.path.lexists(temporary):
        raise ProjectExportError("export temporary path already exists")
    digest = hashlib.sha256()
    processed = 0
    stream = None
    workbook = None
    sheet = None

    def checkpoint() -> None:
        job["processed_rows"] = processed
        job["updated_at"] = _now()
        _write_job(root, job)
        _check_cancel(root, job)
        if shutil.disk_usage(out_dir).free < 10 * 1024**3:
            raise ProjectExportError("project export stopped to preserve the 10 GiB free-space reserve")

    try:
        job.update(state="running", phase="writing", temporary=temporary.name, updated_at=_now())
        _write_job(root, job)
        if job["format"] == "json":
            stream = _open_temporary(temporary)
            prefix = json.dumps(
                {"format": "seohead.project-export.v1", "dataset": job["dataset"], "source": job["source"]},
                ensure_ascii=False,
                sort_keys=True,
                separators=(",", ":"),
            )[:-1] + ',"rows":['
            stream.write(prefix.encode("utf-8"))
            digest.update(prefix.encode("utf-8"))
        elif job["format"] == "csv":
            stream = _open_temporary(temporary, text=True)
            writer = csv.writer(stream, lineterminator="\n")
            writer.writerow(list(columns))
        else:
            try:
                from openpyxl import Workbook
            except ImportError as exc:
                raise ProjectExportError("XLSX export requires the reports extra (openpyxl)") from exc
            if total > 1_048_575:
                raise ProjectExportError("XLSX has one header row and a 1,048,575 data-row limit; use complete CSV or JSON")
            workbook = Workbook(write_only=True)
            sheet_name = {
                "tasks": "Tasks",
                "task_comments": "Comments",
                "task_history": "Task history",
                "compare": "Compare",
            }[job["dataset"]]
            sheet = workbook.create_sheet(sheet_name)
            sheet.append(list(columns))
            descriptor = os.open(temporary, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
            os.close(descriptor)
        for row in rows:
            processed += 1
            if processed > MAX_EXPORT_ROWS:
                raise ProjectExportError(f"export exceeds the {MAX_EXPORT_ROWS}-row bound")
            values = [_safe_cell(row.get(column)) for column in columns]
            if job["format"] == "xlsx" and any(len(value) > XLSX_CELL_LIMIT for value in values):
                raise ProjectExportError("XLSX cell exceeds 32,767 characters; use complete CSV or JSON")
            if job["format"] == "json":
                encoded = json.dumps(row, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
                chunk = ("," if processed > 1 else "") + encoded
                data = chunk.encode("utf-8")
                stream.write(data)
                digest.update(data)
            elif job["format"] == "csv":
                writer.writerow(values)
            else:
                sheet.append(values)
            if processed % 256 == 0:
                checkpoint()
            if stream is not None and stream.tell() > MAX_EXPORT_BYTES:
                raise ProjectExportError("export exceeds its 4 GiB byte bound")
        if processed != total:
            raise ProjectExportError("export row count changed during generation")
        if job["format"] == "json":
            suffix_text = "]}\n"
            stream.write(suffix_text.encode("utf-8"))
            digest.update(suffix_text.encode("utf-8"))
            stream.flush()
            os.fsync(stream.fileno())
            size = stream.tell()
            stream.close()
            stream = None
        elif job["format"] == "csv":
            stream.flush()
            os.fsync(stream.fileno())
            size = stream.buffer.tell()
            stream.close()
            stream = None
            with temporary.open("rb") as digest_stream:
                for block in iter(lambda: digest_stream.read(1024 * 1024), b""):
                    digest.update(block)
        else:
            workbook.save(temporary)
            size = temporary.stat().st_size
            with temporary.open("rb") as digest_stream:
                for block in iter(lambda: digest_stream.read(1024 * 1024), b""):
                    digest.update(block)
        if size > MAX_EXPORT_BYTES:
            raise ProjectExportError("export exceeds its 4 GiB byte bound")
        result = {
            "path": target.relative_to(root).as_posix(),
            "rows": processed,
            "bytes": size,
            "sha256": digest.hexdigest(),
        }
        job.update(
            state="running",
            phase="publishing",
            result=result,
            processed_rows=processed,
            updated_at=_now(),
        )
        _write_job(root, job)
        _check_cancel(root, job)
        os.link(temporary, target, follow_symlinks=False)
        temporary.unlink()
        job.update(phase="history", updated_at=_now())
        _write_job(root, job)
        return result
    except BaseException:
        if stream is not None:
            stream.close()
        with contextlib.suppress(OSError):
            temporary.unlink()
        raise


def _record_published_export(root: Path, directory: str, job: dict[str, Any]) -> dict[str, Any]:
    """Verify/publish the prepared artifact, then idempotently record its receipt."""
    target, temporary = _artifact_paths(root, job)
    result = job.get("result")
    if not isinstance(result, dict) or result.get("path") != target.relative_to(root).as_posix():
        raise ProjectExportError("published export has no matching prepared result")
    if target.is_symlink():
        raise ProjectExportError("published export path is a symbolic link")
    if target.exists():
        if not _matches_artifact(target, result):
            raise ProjectExportError("published export does not match its prepared checksum")
    else:
        if not _matches_artifact(temporary, result):
            raise ProjectExportError("prepared export is unavailable or does not match its checksum")
        os.link(temporary, target, follow_symlinks=False)
    if temporary.exists() and not temporary.is_symlink() and _matches_artifact(temporary, result):
        temporary.unlink()

    job.update(state="running", phase="history", updated_at=_now())
    _write_job(root, job)
    try:
        receipt = record_export(
            directory,
            dataset=job["dataset"],
            fmt=job["format"],
            path=result["path"],
            row_count=result["rows"],
            byte_count=result["bytes"],
            sha256=result["sha256"],
            source=job.get("source") or {},
            job_id=job["job_id"],
        )
    except Exception as exc:
        job.update(
            state="completed",
            phase="complete",
            history_state="failed",
            history_reason=f"history receipt could not be recorded ({type(exc).__name__}); retry this job_id",
            finished_at=_now(),
        )
    else:
        job.update(
            state="completed",
            phase="complete",
            history_state="recorded",
            history_reason="",
            export=receipt,
            finished_at=_now(),
        )
    _write_job(root, job)
    return job


def record_export(
    directory: str | Path,
    *,
    dataset: str,
    fmt: str,
    path: str,
    row_count: int,
    byte_count: int,
    sha256: str | None = None,
    source: dict[str, Any] | None = None,
    job_id: str | None = None,
) -> dict[str, Any]:
    """Append one success receipt; a job ID makes history retries idempotent."""
    root, project = _load(directory)
    document = read_document(root, HISTORY_NAME) or {
        "format": HISTORY_FORMAT,
        "project_uuid": project["project_uuid"],
        "revision": 0,
        "items": [],
    }
    if document.get("format") != HISTORY_FORMAT or document.get("project_uuid") != project["project_uuid"]:
        raise ProjectExportError("export history belongs to another project or format")
    items = document.get("items")
    if (
        not isinstance(items, list)
        or type(document.get("revision")) is not int
        or document["revision"] < 0
    ):
        raise ProjectExportError("export history items are malformed")
    for row in items:
        if not isinstance(row, dict):
            raise ProjectExportError("export history items are malformed")
        _validate_export(row)
    item = {
        "id": str(uuid.uuid4()),
        "recorded_at": _now(),
        "dataset": dataset,
        "format": fmt,
        "path": path,
        "row_count": row_count,
        "byte_count": byte_count,
        "sha256": sha256,
        "source": source or {},
        **({"job_id": str(uuid.UUID(job_id))} if job_id is not None else {}),
    }
    _validate_export(item)
    for _attempt in range(8):
        if job_id is not None:
            existing = next(
                (row for row in items if isinstance(row, dict) and row.get("job_id") == item["job_id"]),
                None,
            )
            if existing is not None:
                comparable = ("dataset", "format", "path", "row_count", "byte_count", "sha256", "source")
                if all(existing.get(key) == item.get(key) for key in comparable):
                    return existing
                raise ProjectExportError("export job receipt conflicts with its published result")
        updated = {
            **document,
            "revision": int(document["revision"]) + 1,
            "items": [*items, item][-10000:],
        }
        try:
            write_document(root, HISTORY_NAME, updated, expected_revision=document["revision"])
            break
        except ValueError as exc:
            if "revision conflict" not in str(exc):
                raise
            document = read_document(root, HISTORY_NAME) or {
                "format": HISTORY_FORMAT,
                "project_uuid": project["project_uuid"],
                "revision": 0,
                "items": [],
            }
            if document.get("format") != HISTORY_FORMAT or document.get("project_uuid") != project["project_uuid"]:
                raise ProjectExportError("export history belongs to another project or format") from exc
            items = document.get("items")
            if (
                not isinstance(items, list)
                or type(document.get("revision")) is not int
                or document["revision"] < 0
            ):
                raise ProjectExportError("export history is malformed") from exc
            for row in items:
                if not isinstance(row, dict):
                    raise ProjectExportError("export history items are malformed") from exc
                _validate_export(row)
    else:
        raise ProjectExportError("export history remained busy after bounded retries")
    return item


def _validate_export(item: dict[str, Any]) -> None:
    try:
        valid_id = isinstance(item.get("id"), str) and str(uuid.UUID(item["id"])) == item["id"]
        valid_job_id = item.get("job_id") is None or (
            isinstance(item.get("job_id"), str)
            and str(uuid.UUID(item["job_id"])) == item["job_id"]
        )
    except (TypeError, ValueError):
        valid_id = valid_job_id = False
    if (
        not valid_id
        or not isinstance(item.get("recorded_at"), str)
        or item.get("dataset") not in {"audit", "tasks", "task_comments", "task_history", "compare"}
        or item.get("format") not in {"json", "csv", "xlsx", "docx", "md", "pdf"}
        or not isinstance(item.get("path"), str)
        or type(item.get("row_count")) is not int
        or item["row_count"] < 0
        or type(item.get("byte_count")) is not int
        or item["byte_count"] < 0
        or not isinstance(item.get("source"), dict)
        or (
            item.get("sha256") is not None
            and (
                not isinstance(item.get("sha256"), str)
                or not re.fullmatch(r"[a-f0-9]{64}", item["sha256"])
            )
        )
        or not valid_job_id
    ):
        raise ProjectExportError("invalid export history item")


def exports_page(directory: str | Path, *, offset: int = 0, limit: int = 50) -> dict[str, Any]:
    if type(offset) is not int or offset < 0 or type(limit) is not int or not 1 <= limit <= 100:
        raise ProjectExportError("offset must be nonnegative and limit must be 1..100")
    root, project = _load(directory)
    document = read_document(root, HISTORY_NAME)
    if document is None:
        items = []
        revision = 0
    else:
        if document.get("format") != HISTORY_FORMAT or document.get("project_uuid") != project["project_uuid"]:
            raise ProjectExportError("export history belongs to another project or format")
        items = document.get("items", [])
        revision = document.get("revision", 0)
        if (
            not isinstance(items, list)
            or type(revision) is not int
            or revision < 0
            or any(not isinstance(item, dict) for item in items)
        ):
            raise ProjectExportError("export history is malformed")
        for item in items:
            _validate_export(item)
    total = len(items)
    page = list(reversed(items))[offset : offset + limit]
    return {
        "ok": True,
        "revision": revision,
        "items": page,
        "pagination": {
            "total": total,
            "offset": offset,
            "limit": limit,
            "next_offset": offset + limit if offset + limit < total else None,
            "previous_offset": max(0, offset - limit) if offset else None,
        },
    }


def _run_export(root: Path, directory: str, job: dict[str, Any], lock_fd: int) -> None:
    job.update(state="running", phase="preparing", started_at=_now(), updated_at=_now())
    _write_job(root, job)
    try:
        rows, total, source = _rows(directory, job["dataset"], job.get("compare_manifest"))
        job["total_rows"] = total
        job["source"] = source
        _write_job(root, job)
        _write_rows(
            root,
            job,
            iter(rows),
            total,
            {
                "tasks": TASK_COLUMNS,
                "task_comments": TASK_COMMENT_COLUMNS,
                "task_history": TASK_HISTORY_COLUMNS,
                "compare": COMPARE_COLUMNS,
            }[job["dataset"]],
        )
        _record_published_export(root, directory, job)
    except _ExportCancelled as exc:
        job.update(
            state="cancelled",
            phase="complete",
            result=None,
            history_state="not_recorded",
            reason=str(exc),
            finished_at=_now(),
        )
    except Exception as exc:
        result = job.get("result")
        try:
            target, _temporary = _artifact_paths(root, job)
            published = _matches_artifact(target, result)
        except (OSError, ValueError, ProjectExportError):
            published = False
        if published:
            job.update(
                state="completed",
                phase="complete",
                history_state="failed",
                history_reason=f"history receipt could not be finalized ({type(exc).__name__}); retry this job_id",
                finished_at=_now(),
            )
        else:
            job.update(
                state="failed",
                phase="complete",
                result=None,
                history_state="not_recorded",
                reason=f"{type(exc).__name__}: {exc}",
                finished_at=_now(),
            )
    finally:
        with contextlib.suppress(OSError, ProjectExportError):
            _write_job(root, job)
        with contextlib.suppress(OSError, ProjectExportError):
            cancel = _job_path(root, job["job_id"], create=False).with_suffix(".cancel")
            cancel.unlink(missing_ok=True)
        with _ACTIVE_LOCK:
            _ACTIVE.pop(job["job_id"], None)
        with contextlib.suppress(OSError):
            _release_job_lock(lock_fd)


def _resume_existing_export(
    root: Path, directory: str, job: dict[str, Any]
) -> dict[str, Any] | None:
    """Return a terminal job, finish a prepared publish, or reset owned partial work."""
    if job.get("state") == "cancelled":
        return job
    result = job.get("result")
    target, temporary = _artifact_paths(root, job)
    if job.get("state") == "completed":
        if not _matches_artifact(target, result):
            raise ProjectExportError("completed export is missing or no longer matches its recorded checksum")
        if job.get("history_state") == "failed":
            return _record_published_export(root, directory, job)
        return job

    if isinstance(result, dict):
        if target.exists() or target.is_symlink():
            if not _matches_artifact(target, result):
                raise ProjectExportError("export destination exists but does not match this job")
        elif _matches_artifact(temporary, result):
            os.link(temporary, target, follow_symlinks=False)
            temporary.unlink()
        else:
            result = None
            job.pop("result", None)

        if result is not None:
            return _record_published_export(root, directory, job)

    if target.exists() or target.is_symlink():
        raise ProjectExportError("export destination exists without a verifiable job receipt")
    if temporary.is_symlink():
        raise ProjectExportError("unsafe export temporary path")
    if temporary.exists():
        if not temporary.is_file():
            raise ProjectExportError("export temporary path is not a regular file")
        temporary.unlink()
    cancel = _job_path(root, job["job_id"], create=False).with_suffix(".cancel")
    if cancel.is_symlink():
        raise ProjectExportError("unsafe export cancellation marker")
    cancel.unlink(missing_ok=True)
    for key in ("result", "source", "started_at", "finished_at", "reason", "history_reason", "export"):
        job.pop(key, None)
    job.update(
        state="queued",
        phase="queued",
        processed_rows=0,
        total_rows=None,
        history_state="not_recorded",
        updated_at=_now(),
    )
    _write_job(root, job)
    return None


def _recoverable(root: Path, job: dict[str, Any]) -> bool:
    try:
        target, temporary = _artifact_paths(root, job)
    except (OSError, ValueError, ProjectExportError):
        return False
    result = job.get("result")
    if isinstance(result, dict):
        return _matches_artifact(target, result) or _matches_artifact(temporary, result)
    return (
        not target.exists()
        and not target.is_symlink()
        and (not temporary.exists() or temporary.is_file() and not temporary.is_symlink())
    )


def start_export(
    directory: str | Path,
    *,
    dataset: str,
    fmt: str,
    output: str,
    compare_manifest: str | None = None,
    job_id: str | None = None,
    wait: bool = False,
) -> dict[str, Any]:
    """Run or start a local task/compare export; no network request is made."""
    root, project = _load(directory)
    if dataset not in {"tasks", "task_comments", "task_history", "compare"}:
        raise ProjectExportError("dataset must be tasks, task_comments, task_history, or compare")
    if fmt not in {"json", "csv", "xlsx"}:
        raise ProjectExportError("format must be json, csv, or xlsx")
    if (dataset == "compare") != (compare_manifest is not None):
        raise ProjectExportError("compare_manifest is required only for compare exports")
    if not isinstance(output, str) or Path(output).name != output or output.startswith("."):
        raise ProjectExportError("output must be a new filename directly under reports/")
    output = output if Path(output).suffix else output + {"json": ".json", "csv": ".csv", "xlsx": ".xlsx"}[fmt]
    if Path(output).suffix.casefold() != {"json": ".json", "csv": ".csv", "xlsx": ".xlsx"}[fmt]:
        raise ProjectExportError("output extension must match format")
    identity = str(uuid.UUID(job_id)) if job_id is not None else str(uuid.uuid4())
    path = _job_path(root, identity, create=True)
    lock_fd, busy = _acquire_job_lock(root, identity, create=True)
    if busy or lock_fd is None:
        raise ProjectExportError("export job is already running")
    transferred = False
    try:
        if os.path.lexists(path):
            existing = _read_job(root, identity)
            if (
                existing.get("project_uuid") != project["project_uuid"]
                or existing.get("dataset") != dataset
                or existing.get("format") != fmt
                or existing.get("output") != output
                or existing.get("compare_manifest") != compare_manifest
            ):
                raise ProjectExportError("job_id belongs to a different export request")
            resumed = _resume_existing_export(root, str(root), existing)
            if resumed is not None:
                return {"ok": resumed.get("state") == "completed", **resumed}
            job = existing
        else:
            job = {
                "record_format": "seohead.project-export-job.v1",
                "job_id": identity,
                "project_uuid": project["project_uuid"],
                "dataset": dataset,
                "format": fmt,
                "output": output,
                "compare_manifest": compare_manifest,
                "state": "queued",
                "phase": "queued",
                "processed_rows": 0,
                "total_rows": None,
                "history_state": "not_recorded",
                "created_at": _now(),
                "updated_at": _now(),
            }
            _write_job(root, job)

        if wait:
            with _ACTIVE_LOCK:
                _ACTIVE[identity] = threading.current_thread()
            transferred = True
            _run_export(root, str(root), job, lock_fd)
            return {"ok": job.get("state") == "completed", **job}
        thread = threading.Thread(
            target=_run_export, args=(root, str(root), job, lock_fd), daemon=True
        )
        with _ACTIVE_LOCK:
            _ACTIVE[identity] = thread
        try:
            thread.start()
        except BaseException:
            with _ACTIVE_LOCK:
                _ACTIVE.pop(identity, None)
            raise
        transferred = True
        return {"ok": True, **job}
    finally:
        if not transferred:
            with contextlib.suppress(OSError):
                _release_job_lock(lock_fd)


def export_status(directory: str | Path, job_id: str) -> dict[str, Any]:
    root, project = _load(directory)
    job_id = str(uuid.UUID(job_id))
    job = _read_job(root, job_id)
    if job.get("project_uuid") != project["project_uuid"]:
        raise ProjectExportError("export job belongs to another project")
    with _ACTIVE_LOCK:
        active = job_id in _ACTIVE and _ACTIVE[job_id].is_alive()
    if not active:
        lock_fd, busy = _acquire_job_lock(root, job_id, create=False)
        active = busy
        _release_job_lock(lock_fd)
    if job["state"] in {"queued", "running"} and not active:
        return {
            "ok": True,
            **job,
            "state": "interrupted",
            "recoverable": _recoverable(root, job),
        }
    return {
        "ok": True,
        **job,
        "recoverable": _recoverable(root, job) if job.get("history_state") == "failed" else False,
    }


def cancel_export(directory: str | Path, job_id: str) -> dict[str, Any]:
    root, project = _load(directory)
    job_id = str(uuid.UUID(job_id))
    job = _read_job(root, job_id)
    if job.get("project_uuid") != project["project_uuid"]:
        raise ProjectExportError("export job belongs to another project")
    if job.get("state") not in {"queued", "running"}:
        return {"ok": True, "state": job.get("state"), "cancelled": False}
    cancel = _job_path(root, job_id).with_suffix(".cancel")
    if cancel.is_symlink():
        raise ProjectExportError("unsafe export cancellation marker")
    cancel.touch(mode=0o600, exist_ok=True)
    return {"ok": True, "state": "cancelling", "cancelled": True, "job_id": job_id}
