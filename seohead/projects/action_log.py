"""Bounded project action history for pending, denied and completed operations."""

from __future__ import annotations

import json
import os
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from seohead.core import filesystem
from seohead.core.filesystem import atomic_write_bytes
from .workspace import _load

FILE = "actions.jsonl"
MAX_ACTIONS = 10_000
MAX_BYTES = 8 * 1024 * 1024
MAX_LIMIT = 200
MAX_TEXT = 1_000
CLIENTS = {"cli", "mcp", "system"}
OUTCOMES = {"pending", "applied", "approved", "denied", "stale", "refused", "deleted"}
_KEYS = {
    "sequence",
    "occurred_at",
    "client",
    "action",
    "outcome",
    "detail",
    "proposal_id",
    "operation_id",
}


def _text(value: Any, label: str, maximum: int = MAX_TEXT, *, empty: bool = True) -> str:
    if type(value) is not str or len(value) > maximum or (not empty and not value.strip()):
        raise ValueError(f"{label} must be bounded text")
    if "\x00" in value:
        raise ValueError(f"{label} must not contain NUL bytes")
    return value


def _validate(row: Any) -> dict[str, Any]:
    if not isinstance(row, dict) or set(row) != _KEYS:
        raise ValueError("project action has an unsupported shape")
    if type(row["sequence"]) is not int or row["sequence"] < 1:
        raise ValueError("project action sequence is invalid")
    if (
        type(row["client"]) is not str
        or row["client"] not in CLIENTS
        or type(row["outcome"]) is not str
        or row["outcome"] not in OUTCOMES
    ):
        raise ValueError("project action client or outcome is unsupported")
    _text(row["action"], "action", 128, empty=False)
    _text(row["detail"], "detail")
    if row["proposal_id"] is not None:
        _text(row["proposal_id"], "proposal_id", 64, empty=False)
    if row["operation_id"] is not None:
        _text(row["operation_id"], "operation_id", 64, empty=False)
    try:
        stamp = datetime.fromisoformat(row["occurred_at"].replace("Z", "+00:00"))
    except (AttributeError, ValueError) as exc:
        raise ValueError("project action time must be RFC3339 UTC") from exc
    if stamp.tzinfo is None or stamp.utcoffset() != timezone.utc.utcoffset(stamp):
        raise ValueError("project action time must be RFC3339 UTC")
    return row


def _path(root: Path) -> Path:
    path = root / FILE
    if path.is_symlink():
        raise ValueError("project action log is unsafe")
    return path


def _lock(root: Path) -> int:
    descriptor = None
    try:
        descriptor = filesystem.open_lock(root / f".{FILE}.lock")
        filesystem.lock_exclusive(descriptor)
        return descriptor
    except OSError as exc:
        if descriptor is not None:
            os.close(descriptor)
        raise ValueError("project action log is busy or locking is unavailable") from exc


def _unlock(descriptor: int) -> None:
    try:
        filesystem.unlock(descriptor)
    finally:
        os.close(descriptor)


def _parse(stream: Any) -> list[dict[str, Any]]:
    stream.seek(0, os.SEEK_END)
    if stream.tell() > MAX_BYTES:
        raise ValueError("project action log exceeds its byte limit")
    stream.seek(0)
    rows = []
    for number, line in enumerate(stream, 1):
        try:
            row = _validate(json.loads(line))
        except ValueError as exc:
            raise ValueError(f"project action line {number} is invalid: {exc}") from exc
        if row["sequence"] != len(rows) + 1:
            raise ValueError(f"project action line {number} has a noncontiguous sequence")
        if len(rows) >= MAX_ACTIONS:
            raise ValueError("project action log exceeds its entry limit")
        rows.append(row)
    return rows


def record(
    directory: str | Path,
    *,
    client: str,
    action: str,
    outcome: str,
    detail: str = "",
    proposal_id: str | None = None,
    operation_id: str | None = None,
) -> dict[str, Any]:
    """Append one action with a server-selected client value."""
    if (
        type(client) is not str
        or client not in CLIENTS
        or type(outcome) is not str
        or outcome not in OUTCOMES
    ):
        raise ValueError("project action client or outcome is unsupported")
    action = _text(action, "action", 128, empty=False)
    detail = _text(detail, "detail")
    if proposal_id is not None:
        proposal_id = _text(proposal_id, "proposal_id", 64, empty=False)
    root, _project = _load(directory)
    return record_root(
        root,
        client=client,
        action=action,
        outcome=outcome,
        detail=detail,
        proposal_id=proposal_id,
        operation_id=operation_id,
    )


def record_root(
    root: str | Path,
    *,
    client: str,
    action: str,
    outcome: str,
    detail: str = "",
    proposal_id: str | None = None,
    operation_id: str | None = None,
) -> dict[str, Any]:
    """Append to a previously validated project directory, including one just trashed."""
    if (
        type(client) is not str
        or client not in CLIENTS
        or type(outcome) is not str
        or outcome not in OUTCOMES
    ):
        raise ValueError("project action client or outcome is unsupported")
    action = _text(action, "action", 128, empty=False)
    detail = _text(detail, "detail")
    if proposal_id is not None:
        proposal_id = _text(proposal_id, "proposal_id", 64, empty=False)
    if operation_id is not None:
        operation_id = _text(operation_id, "operation_id", 64, empty=False)
    root = Path(root)
    path = _path(root)
    from .runtime import project_write_lock

    with project_write_lock(root):
        descriptor = _lock(root)
        try:
            with path.open("a+", encoding="utf-8") as stream:
                rows = _parse(stream)
                if len(rows) >= MAX_ACTIONS:
                    raise ValueError("project action log is full")
                row = {
                    "sequence": len(rows) + 1,
                    "occurred_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
                    "client": client,
                    "action": action,
                    "outcome": outcome,
                    "detail": detail,
                    "proposal_id": proposal_id,
                    "operation_id": operation_id,
                }
                line = json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n"
                stream.seek(0, os.SEEK_END)
                if stream.tell() + len(line.encode("utf-8")) > MAX_BYTES:
                    raise ValueError("project action log is full")
                stream.write(line)
                stream.flush()
        finally:
            _unlock(descriptor)
    return {"ok": True, "action": row}


def resolve_pending(
    directory: str | Path,
    sequence: int,
    *,
    outcome: str,
    detail: str,
    proposal_id: str | None = None,
    operation_id: str | None = None,
) -> dict[str, Any]:
    """Replace one reserved pending row with its terminal result atomically."""
    root, _project = _load(directory)
    return resolve_pending_root(
        root,
        sequence,
        outcome=outcome,
        detail=detail,
        proposal_id=proposal_id,
        operation_id=operation_id,
    )


def resolve_pending_root(
    root: str | Path,
    sequence: int,
    *,
    outcome: str,
    detail: str,
    proposal_id: str | None = None,
    operation_id: str | None = None,
) -> dict[str, Any]:
    if type(sequence) is not int or sequence < 1:
        raise ValueError("action sequence must be a positive integer")
    if type(outcome) is not str or outcome not in OUTCOMES - {"pending"}:
        raise ValueError("action resolution requires a terminal outcome")
    detail = _text(detail, "detail")
    if proposal_id is not None:
        proposal_id = _text(proposal_id, "proposal_id", 64, empty=False)
    if operation_id is not None:
        operation_id = _text(operation_id, "operation_id", 64, empty=False)
    root = Path(root)
    path = _path(root)
    from .runtime import project_write_lock

    with project_write_lock(root):
        descriptor = _lock(root)
        try:
            if not path.exists():
                raise ValueError("pending action log row is missing")
            with path.open("r", encoding="utf-8") as stream:
                rows = _parse(stream)
            index = next((i for i, row in enumerate(rows) if row["sequence"] == sequence), None)
            if index is None:
                raise ValueError("pending action log row is missing")
            if proposal_id is not None and rows[index]["proposal_id"] != proposal_id:
                raise ValueError("pending action proposal identity does not match")
            if operation_id is not None and rows[index]["operation_id"] != operation_id:
                raise ValueError("pending action operation identity does not match")
            if rows[index]["outcome"] != "pending":
                if rows[index]["outcome"] == outcome:
                    return {"ok": True, "action": rows[index], "already_resolved": True}
                raise ValueError("action log row is already terminal")
            row = {**rows[index], "outcome": outcome, "detail": detail}
            rows[index] = row
            content = "".join(
                json.dumps(item, sort_keys=True, separators=(",", ":")) + "\n" for item in rows
            ).encode("utf-8")
            if len(content) > MAX_BYTES:
                raise ValueError("resolved action log exceeds its byte limit")
            atomic_write_bytes(path, content)
        finally:
            _unlock(descriptor)
    return {"ok": True, "action": row}


def current_interface() -> str:
    """Resolve the process interface, never a caller-supplied role or client value."""
    from seohead.core.runlog import current_interface as read_interface

    value = read_interface()
    return value if value in {"cli", "mcp"} else "system"


def record_interface_action(
    directory: str | Path,
    *,
    action: str,
    outcome: str,
    detail: str = "",
    proposal_id: str | None = None,
    operation_id: str | None = None,
) -> dict[str, Any]:
    """Append an action using the process-owned CLI/MCP interface setting."""
    return record(
        directory,
        client=current_interface(),
        action=action,
        outcome=outcome,
        detail=detail,
        proposal_id=proposal_id,
        operation_id=operation_id,
    )


def page(
    directory: str | Path,
    *,
    offset: int = 0,
    limit: int = 50,
    client: str | None = None,
    outcome: str | None = None,
    query: str = "",
) -> dict[str, Any]:
    """Read a bounded newest-first page with client, outcome and text filters."""
    if (
        type(offset) is not int
        or offset < 0
        or type(limit) is not int
        or not 1 <= limit <= MAX_LIMIT
    ):
        raise ValueError(f"offset must be nonnegative and limit must be 1..{MAX_LIMIT}")
    if client is not None and (type(client) is not str or client not in CLIENTS):
        raise ValueError("project action client filter is unsupported")
    if outcome is not None and (type(outcome) is not str or outcome not in OUTCOMES):
        raise ValueError("project action outcome filter is unsupported")
    query = _text(query, "query", 200)
    root, _project = _load(directory)
    path = _path(root)
    from .runtime import project_write_lock

    with project_write_lock(root):
        descriptor = _lock(root)
        try:
            if path.exists():
                with path.open("r", encoding="utf-8") as stream:
                    rows = _parse(stream)
            else:
                rows = []
        finally:
            _unlock(descriptor)
    needle = query.casefold()
    matched = [
        row
        for row in reversed(rows)
        if (client is None or row["client"] == client)
        and (outcome is None or row["outcome"] == outcome)
        and (
            not needle
            or needle
            in " ".join(str(row[key]) for key in ("action", "detail", "proposal_id")).casefold()
        )
    ]
    return {
        "ok": True,
        "revision": rows[-1]["sequence"] if rows else 0,
        "total": len(matched),
        "items": matched[offset : offset + limit],
        "pagination": {
            "total": len(matched),
            "offset": offset,
            "limit": limit,
            "next_offset": offset + limit if offset + limit < len(matched) else None,
        },
    }


def latest_agent_activity(directory: str | Path) -> dict[str, str | None]:
    """Return the latest recorded MCP activity, never claim the client is connected."""
    result = page(directory, client="mcp", limit=MAX_LIMIT)
    latest = result["items"][0] if result["items"] else None
    return {
        "last_seen_at": latest["occurred_at"] if latest else None,
        "source": "action_log" if latest else None,
        "heartbeat_source": "mcp_agent_heartbeat"
        if latest and latest["action"] == "agent.heartbeat"
        else None,
    }
