"""Durable, local notes and proposed goals for an agent-owned project.

The inbox is deliberately a small project sidecar instead of another task or
scan registry.  It only records a specialist's intent and the explicit receipt
and goal decisions made by an agent.  It never starts a scan, calls a provider,
or injects text into a model conversation.
"""

from __future__ import annotations

import copy
import fcntl
import hashlib
import json
import os
import re
import uuid
from collections.abc import Iterator
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from seohead.core.common import utc_iso_z as _now
from seohead.core.filesystem import atomic_write_bytes

from .workspace import _load

FORMAT = "seohead.project-inbox.v1"
MAX_BYTES = 4 * 1024 * 1024
MAX_TEXT = 8_000
MAX_ENTRIES = 10_000
MAX_PAGE = 100
_CONSUMER = re.compile(r"[a-z][a-z0-9._/-]{0,127}\Z")
_REFERENCE = re.compile(r"(?:goal|task|scan|finding|section):[A-Za-z0-9._/-]{1,128}\Z")


def _text(value: Any, name: str, maximum: int = MAX_TEXT) -> str:
    if type(value) is not str or not value.strip() or len(value) > maximum:
        raise ValueError(f"{name} must be nonempty text of at most {maximum} characters")
    return value.strip()


def _consumer(value: Any) -> str:
    if type(value) is not str or not _CONSUMER.fullmatch(value) or ".." in value:
        raise ValueError("consumer must be a bounded stable local identifier")
    return value


def _reference(value: Any) -> bool:
    if type(value) is not str:
        return False
    if _REFERENCE.fullmatch(value):
        return True
    if not value.startswith("task:") or len(value) > 256:
        return False
    from .coverage import _identifier

    item_id = value.removeprefix("task:")
    try:
        if item_id.startswith("site:"):
            scope, separator, item_id = item_id.partition("/")
            site_id = scope.removeprefix("site:")
            if not separator or str(uuid.UUID(site_id)) != site_id:
                return False
        _identifier(item_id)
    except ValueError:
        return False
    return True


def _references(value: Any) -> list[str]:
    if value is None:
        return []
    if not isinstance(value, list) or len(value) > 20:
        raise ValueError("references must be a list of at most 20 stable project references")
    if any(not _reference(item) for item in value):
        raise ValueError("references contain an invalid project reference")
    return list(dict.fromkeys(value))


def _stamp(value: Any) -> None:
    if type(value) is not str:
        raise ValueError("project inbox entry has an invalid timestamp")
    try:
        timestamp = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as exc:
        raise ValueError("project inbox entry has an invalid timestamp") from exc
    if timestamp.tzinfo is None or timestamp.utcoffset() != timezone.utc.utcoffset(timestamp):
        raise ValueError("project inbox entry has an invalid timestamp")


def _entry(value: Any) -> dict[str, Any]:
    required = {
        "id",
        "kind",
        "text",
        "references",
        "author_role",
        "created_at",
        "delivery",
        "goal_state",
    }
    if (
        not isinstance(value, dict)
        or set(value) - (required | {"triage", "accepted_at", "completed_at"})
        or not required <= set(value)
    ):
        raise ValueError("project inbox entry has an unsupported shape")
    if type(value["id"]) is not str or not value["id"].startswith("inbox:"):
        raise ValueError("project inbox entry has an invalid id")
    if value["kind"] not in {"note", "proposed_goal", "question"}:
        raise ValueError("project inbox entry has an invalid kind")
    _text(value["text"], "entry text")
    _references(value["references"])
    if value["author_role"] not in {"specialist", "agent"}:
        raise ValueError("project inbox entry has an invalid author role")
    _stamp(value["created_at"])
    for key in ("accepted_at", "completed_at"):
        if key in value:
            _stamp(value[key])
    if value["goal_state"] not in {None, "proposed", "accepted", "completed"}:
        raise ValueError("project inbox entry has an invalid goal state")
    if value["kind"] != "proposed_goal" and value["goal_state"] is not None:
        raise ValueError("only proposed goals can have a goal state")
    if value["kind"] == "proposed_goal" and value["goal_state"] is None:
        raise ValueError("proposed goals need a goal state")
    delivery = value["delivery"]
    if not isinstance(delivery, dict) or len(delivery) > 100:
        raise ValueError("project inbox delivery state is invalid")
    for consumer, receipt in delivery.items():
        _consumer(consumer)
        if not isinstance(receipt, dict) or set(receipt) - {"read_at", "acknowledged_at"}:
            raise ValueError("project inbox delivery receipt is invalid")
        for stamp in receipt.values():
            if type(stamp) is not str:
                raise ValueError("project inbox delivery receipt is invalid")
    triage = value.get("triage", [])
    if not isinstance(triage, list) or len(triage) > 20:
        raise ValueError("project inbox triage history is invalid")
    for outcome in triage:
        _triage_receipt(outcome)
    return copy.deepcopy(value)


def _triage_receipt(value: Any) -> None:
    required = {"kind", "reason", "actor", "recorded_at"}
    optional = {"task_ids", "goal_id", "competitors"}
    if (
        not isinstance(value, dict)
        or set(value) - (required | optional)
        or not required <= set(value)
    ):
        raise ValueError("project inbox triage receipt has an unsupported shape")
    if value["kind"] not in {"task", "goal", "competitor", "blocked", "rejected"}:
        raise ValueError("project inbox triage receipt has an invalid kind")
    _text(value["reason"], "triage reason", 512)
    _text(value["actor"], "triage actor", 128)
    if type(value["recorded_at"]) is not str:
        raise ValueError("project inbox triage receipt has an invalid timestamp")
    try:
        timestamp = datetime.fromisoformat(value["recorded_at"].replace("Z", "+00:00"))
    except ValueError as exc:
        raise ValueError("project inbox triage receipt has an invalid timestamp") from exc
    if timestamp.tzinfo is None or timestamp.utcoffset() != timezone.utc.utcoffset(timestamp):
        raise ValueError("project inbox triage receipt has an invalid timestamp")
    task_ids = value.get("task_ids")
    if task_ids is not None and (
        not isinstance(task_ids, list)
        or not task_ids
        or len(task_ids) > 20
        or len(task_ids) != len(set(task_ids))
        or any(type(item) is not str or not item.startswith("custom:") for item in task_ids)
    ):
        raise ValueError("project inbox triage receipt has invalid task ids")
    goal_id = value.get("goal_id")
    if goal_id is not None and (type(goal_id) is not str or not goal_id.startswith("inbox:")):
        raise ValueError("project inbox triage receipt has an invalid goal id")
    competitors = value.get("competitors")
    if competitors is not None and (
        not isinstance(competitors, list)
        or not competitors
        or len(competitors) > 20
        or len(competitors) != len(set(competitors))
        or any(type(item) is not str for item in competitors)
    ):
        raise ValueError("project inbox triage receipt has invalid competitors")
    expected_optional = (
        {"task_ids"}
        if value["kind"] == "task"
        else {"goal_id"}
        if value["kind"] == "goal"
        else {"competitors"}
        if value["kind"] == "competitor"
        else set()
    )
    if {key for key in optional if key in value} != expected_optional:
        raise ValueError("project inbox triage receipt does not match its kind")


def _document(root: Path, project: dict[str, Any]) -> dict[str, Any]:
    path = root / "inbox.json"
    if not path.exists():
        return {
            "format": FORMAT,
            "revision": 0,
            "project_uuid": project["project_uuid"],
            "entries": [],
        }
    if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_BYTES:
        raise ValueError("project inbox is missing, unsafe, or exceeds its byte limit")
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise ValueError("project inbox is not valid JSON") from exc
    if not isinstance(document, dict) or set(document) != {
        "format",
        "revision",
        "project_uuid",
        "entries",
    }:
        raise ValueError("project inbox has an unsupported shape")
    if document["format"] != FORMAT or document["project_uuid"] != project["project_uuid"]:
        raise ValueError("project inbox belongs to a different project or format")
    if type(document["revision"]) is not int or document["revision"] < 0:
        raise ValueError("project inbox has an invalid revision")
    if not isinstance(document["entries"], list) or len(document["entries"]) > MAX_ENTRIES:
        raise ValueError("project inbox has too many entries")
    ids = set()
    for item in document["entries"]:
        entry = _entry(item)
        if entry["id"] in ids:
            raise ValueError("project inbox has duplicate entry ids")
        ids.add(entry["id"])
    return document


def _write(root: Path, document: dict[str, Any]) -> None:
    payload = (
        json.dumps(document, ensure_ascii=False, indent=2, sort_keys=True, allow_nan=False) + "\n"
    )
    if len(payload.encode()) > MAX_BYTES:
        raise ValueError("project inbox exceeds its byte limit")
    atomic_write_bytes(root / "inbox.json", payload.encode("utf-8"))


def _read_document(directory: str | Path) -> tuple[Path, dict[str, Any]]:
    """Load a validated inbox without creating a writer lock or sidecar."""
    root, project = _load(directory)
    return root, _document(root, project)


@contextmanager
def _transaction(
    directory: str | Path, expected_revision: int | None = None
) -> Iterator[tuple[Path, dict[str, Any]]]:
    root, project = _load(directory)
    lock_path = root / ".inbox.lock"
    fd = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o600)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        document = _document(root, project)
        if expected_revision is not None and (
            type(expected_revision) is not int or expected_revision != document["revision"]
        ):
            raise ValueError(f"inbox revision conflict: current revision is {document['revision']}")
        before = copy.deepcopy(document)
        yield root, document
        if document != before:
            document["revision"] += 1
            _write(root, document)
    finally:
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)


def _public(entry: dict[str, Any], consumer: str | None = None) -> dict[str, Any]:
    result = {
        key: entry[key]
        for key in ("id", "kind", "text", "references", "author_role", "created_at", "goal_state")
    }
    result["accepted_at"] = entry.get("accepted_at")
    result["completed_at"] = entry.get("completed_at")
    result["triage"] = copy.deepcopy(entry.get("triage", []))
    if consumer is not None:
        receipt = entry["delivery"].get(consumer, {})
        result["read_at"] = receipt.get("read_at")
        result["acknowledged_at"] = receipt.get("acknowledged_at")
    return result


def submit(
    directory: str | Path,
    *,
    text: str,
    kind: str = "note",
    references: list[str] | None = None,
    author_role: str = "specialist",
    expected_revision: int | None = None,
) -> dict[str, Any]:
    """Store a note or proposed goal without executing anything."""
    if kind not in {"note", "proposed_goal", "question"}:
        raise ValueError("kind must be note, proposed_goal or question")
    if author_role not in {"specialist", "agent"}:
        raise ValueError("author_role must be specialist or agent")
    with _transaction(directory, expected_revision) as (_, document):
        entry: dict[str, Any] = {
            "id": f"inbox:{uuid.uuid4()}",
            "kind": kind,
            "text": _text(text, "text"),
            "references": _references(references),
            "author_role": author_role,
            "created_at": _now(),
            "delivery": {},
            "goal_state": "proposed" if kind == "proposed_goal" else None,
            "triage": [],
        }
        document["entries"].append(entry)
        return {"ok": True, "revision": document["revision"] + 1, "entry": _public(entry)}


def list_entries(
    directory: str | Path,
    *,
    consumer: str,
    offset: int = 0,
    limit: int = 20,
    include_acknowledged: bool = True,
) -> dict[str, Any]:
    """Read a bounded page without changing delivery state."""
    consumer = _consumer(consumer)
    if (
        type(offset) is not int
        or offset < 0
        or type(limit) is not int
        or not 1 <= limit <= MAX_PAGE
    ):
        raise ValueError("offset must be nonnegative and limit must be from 1 to 100")
    _, document = _read_document(directory)
    entries = [
        entry
        for entry in document["entries"]
        if include_acknowledged or not entry["delivery"].get(consumer, {}).get("acknowledged_at")
    ]
    page = entries[offset : offset + limit]
    return {
        "ok": True,
        "revision": document["revision"],
        "entries": [_public(item, consumer) for item in page],
        "pagination": {
            "offset": offset,
            "limit": limit,
            "total": len(entries),
            "next_offset": offset + len(page) if offset + len(page) < len(entries) else None,
        },
    }


def mark_read(
    directory: str | Path,
    *,
    consumer: str,
    entry_ids: list[str],
    expected_revision: int | None = None,
) -> dict[str, Any]:
    """Record inspection.  A read message remains unread until acknowledgment."""
    consumer = _consumer(consumer)
    if (
        not isinstance(entry_ids, list)
        or not entry_ids
        or len(entry_ids) > MAX_PAGE
        or len(entry_ids) != len(set(entry_ids))
    ):
        raise ValueError("entry_ids must be a unique bounded nonempty list")
    with _transaction(directory, expected_revision) as (_, document):
        entries = {entry["id"]: entry for entry in document["entries"]}
        if any(item not in entries for item in entry_ids):
            raise ValueError("unknown inbox entry")
        stamp = _now()
        changed = False
        for item in entry_ids:
            receipt = entries[item]["delivery"].setdefault(consumer, {})
            if "read_at" not in receipt:
                receipt["read_at"] = stamp
                changed = True
        return {
            "ok": True,
            "revision": document["revision"] + int(changed),
            "entries": [_public(entries[item], consumer) for item in entry_ids],
        }


def acknowledge(
    directory: str | Path,
    *,
    consumer: str,
    entry_ids: list[str],
    expected_revision: int | None = None,
) -> dict[str, Any]:
    """Explicitly acknowledge entries; idempotent retries retain the original receipt."""
    consumer = _consumer(consumer)
    with _transaction(directory, expected_revision) as (_, document):
        entries = {entry["id"]: entry for entry in document["entries"]}
        if (
            not isinstance(entry_ids, list)
            or not entry_ids
            or len(entry_ids) > MAX_PAGE
            or len(entry_ids) != len(set(entry_ids))
            or any(item not in entries for item in entry_ids)
        ):
            raise ValueError("entry_ids must name unique existing inbox entries")
        stamp, changed = _now(), False
        for item in entry_ids:
            receipt = entries[item]["delivery"].setdefault(consumer, {})
            if "read_at" not in receipt:
                receipt["read_at"] = stamp
            if "acknowledged_at" not in receipt:
                receipt["acknowledged_at"] = stamp
                changed = True
        return {
            "ok": True,
            "revision": document["revision"] + int(changed),
            "entries": [_public(entries[item], consumer) for item in entry_ids],
        }


def goal_task_id(entry_id: str) -> str:
    """The checklist task that an accepted goal creates; one per goal, derived from its id."""
    return "custom:goal-" + entry_id.removeprefix("inbox:")


def _goal_task_title(text: str) -> str:
    return ("Owner goal: " + " ".join(text.split()))[:512]


def set_goal_state(
    directory: str | Path, *, entry_id: str, state: str, expected_revision: int | None = None
) -> dict[str, Any]:
    """Accept or complete a proposed goal; acceptance creates its checklist task once."""
    if state not in {"accepted", "completed"}:
        raise ValueError("goal state must be accepted or completed")
    with _transaction(directory, expected_revision) as (_, document):
        entry: dict[str, Any] | None = next(
            (item for item in document["entries"] if item["id"] == entry_id), None
        )
        if entry is None or entry["kind"] != "proposed_goal":
            raise ValueError("entry is not a proposed goal")
        if state == "completed" and entry["goal_state"] != "accepted":
            raise ValueError("a proposed goal must be accepted before completion")
        if state == "accepted":
            from .coverage import ensure_goal_task

            ensure_goal_task(
                directory,
                item_id=goal_task_id(entry["id"]),
                title=_goal_task_title(entry["text"]),
            )
        changed = entry["goal_state"] != state
        entry["goal_state"] = state
        if changed:
            entry[f"{state}_at"] = _now()
        return {
            "ok": True,
            "revision": document["revision"] + int(changed),
            "entry": _public(entry),
        }


def triage(
    directory: str | Path,
    *,
    entry_id: str,
    outcome: dict[str, Any],
    actor: str,
    expected_revision: int | None = None,
) -> dict[str, Any]:
    """Append an explicit agent decision to a human note without starting work."""
    entry_id = _text(entry_id, "entry_id", 256)
    actor = _text(actor, "triage actor", 128)
    if not isinstance(outcome, dict) or "kind" not in outcome:
        raise ValueError("triage outcome must be an object with a kind")
    kind = outcome.get("kind")
    reason = _text(outcome.get("reason"), "triage reason", 512)
    with _transaction(directory, expected_revision) as (root, document):
        entry = next((item for item in document["entries"] if item["id"] == entry_id), None)
        if entry is None or entry["kind"] != "note" or entry["author_role"] != "specialist":
            raise ValueError("triage applies only to a specialist note")
        receipt: dict[str, Any] = {
            "kind": kind,
            "reason": reason,
            "actor": actor,
            "recorded_at": _now(),
        }
        if kind == "task":
            task_ids = outcome.get("task_ids")
            if not isinstance(task_ids, list) or not task_ids or len(task_ids) > 20:
                raise ValueError("task triage requires one or more custom task ids")
            from .coverage import coverage_status

            rows = {row["id"]: row for row in coverage_status(root).get("items", [])}
            if len(task_ids) != len(set(task_ids)) or any(
                type(task_id) is not str
                or not task_id.startswith("custom:")
                or task_id not in rows
                or rows[task_id]["stale"]
                or rows[task_id]["complete"]
                for task_id in task_ids
            ):
                raise ValueError("task triage requires current incomplete custom checklist tasks")
            receipt["task_ids"] = task_ids
        elif kind == "goal":
            goal_id = outcome.get("goal_id")
            goal = next((item for item in document["entries"] if item["id"] == goal_id), None)
            if (
                goal is None
                or goal["kind"] != "proposed_goal"
                or goal["goal_state"] not in {"proposed", "accepted"}
            ):
                raise ValueError("goal triage requires a current stored proposed goal id")
            receipt["goal_id"] = goal_id
        elif kind == "competitor":
            competitors = outcome.get("competitors")
            if not isinstance(competitors, list) or not competitors or len(competitors) > 20:
                raise ValueError("competitor triage requires one or more candidate URLs")
            from .workspace import _load, _target

            _, project = _load(root)
            normalized = [_target(item) for item in competitors]
            if len(normalized) != len(set(normalized)) or project["site"]["target"] in normalized:
                raise ValueError("competitor candidates must be distinct non-primary sites")
            receipt["competitors"] = normalized
        elif kind not in {"blocked", "rejected"}:
            raise ValueError("triage kind must be task, goal, competitor, blocked, or rejected")
        if set(outcome) != {"kind", "reason"} | (
            {"task_ids"}
            if kind == "task"
            else {"goal_id"}
            if kind == "goal"
            else {"competitors"}
            if kind == "competitor"
            else set()
        ):
            raise ValueError("triage outcome has unsupported fields")
        entry.setdefault("triage", []).append(receipt)
        _triage_receipt(receipt)
        return {"ok": True, "revision": document["revision"] + 1, "entry": _public(entry)}


def unread_summary(directory: str | Path, *, consumer: str, limit: int = 10) -> dict[str, Any]:
    """Return bounded references for a tool response; it never consumes messages."""
    consumer = _consumer(consumer)
    if type(limit) is not int or not 1 <= limit <= MAX_PAGE:
        raise ValueError("limit must be from 1 to 100")
    _, document = _read_document(directory)
    unread = [
        entry
        for entry in document["entries"]
        if not entry["delivery"].get(consumer, {}).get("acknowledged_at")
    ]
    return {
        "count": len(unread),
        "entries": [
            {"id": entry["id"], "kind": entry["kind"], "references": entry["references"]}
            for entry in unread[:limit]
        ],
        "truncated": len(unread) > limit,
    }


def unread_goal_tasks(directory: str | Path, *, consumer: str, limit: int = 10) -> dict[str, Any]:
    """Accepted goals whose checklist task this consumer has not acknowledged yet."""
    consumer = _consumer(consumer)
    if type(limit) is not int or not 1 <= limit <= MAX_PAGE:
        raise ValueError("limit must be from 1 to 100")
    _, document = _read_document(directory)
    unread = [
        entry
        for entry in document["entries"]
        if entry["kind"] == "proposed_goal"
        and entry["goal_state"] == "accepted"
        and not entry["delivery"].get(consumer, {}).get("acknowledged_at")
    ]
    return {
        "count": len(unread),
        "items": [
            {
                "entry_id": entry["id"],
                "task_id": goal_task_id(entry["id"]),
                "title": _goal_task_title(entry["text"]),
                "accepted_at": entry["accepted_at"],
            }
            for entry in unread[:limit]
        ],
        "truncated": len(unread) > limit,
    }


def fingerprint(directory: str | Path) -> str:
    """A test-only observer aid: hash raw inbox bytes without modifying source artifacts."""
    root, _ = _load(directory)
    path = root / "inbox.json"
    return hashlib.sha256(path.read_bytes() if path.exists() else b"").hexdigest()
