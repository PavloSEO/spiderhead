"""Structured, append-only project event log with bounded, paginated reading.

Every event names its source (agent, user, scans, app), its actor (user, agent,
schedule) and a bounded text. Events live in ``events.jsonl`` beside ``log.md``;
``log.md`` stays the human narrative and is never rewritten here. Appending
never edits earlier lines and reading never writes.
"""

from __future__ import annotations

import fcntl
import json
import os
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from .workspace import _load

FILE = "events.jsonl"
SOURCES = ("agent", "user", "scans", "app")
ACTORS = ("user", "agent", "schedule")
MAX_TEXT = 2_000
MAX_QUERY = 200
MAX_EVENTS = 10_000
MAX_BYTES = 8 * 1024 * 1024
MAX_LIMIT = 200
MAX_FOLLOW_SECONDS = 30
_KEYS = {"sequence", "occurred_at", "source", "actor", "text"}


def _event(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != _KEYS:
        raise ValueError("project event has an unsupported shape")
    sequence = value["sequence"]
    if type(sequence) is not int or sequence < 1:
        raise ValueError("project event sequence is invalid")
    if value["source"] not in SOURCES or value["actor"] not in ACTORS:
        raise ValueError("project event source or actor is unsupported")
    if type(value["occurred_at"]) is not str or type(value["text"]) is not str:
        raise ValueError("project event fields are invalid")
    try:
        occurred = datetime.fromisoformat(value["occurred_at"].replace("Z", "+00:00"))
    except ValueError as exc:
        raise ValueError("project event time must be RFC3339 UTC") from exc
    if occurred.tzinfo is None:
        raise ValueError("project event time must be RFC3339 UTC")
    _text(value["text"])
    return value


def _text(value: Any) -> str:
    if not isinstance(value, str) or not value.strip() or len(value) > MAX_TEXT:
        raise ValueError(f"event text must be 1..{MAX_TEXT} characters")
    if "\x00" in value:
        raise ValueError("event text must not contain NUL bytes")
    return value


def _path(root: Path) -> Path:
    path = root / FILE
    if path.is_symlink():
        raise ValueError("project event log is unsafe")
    return path


def _parse(stream: Any) -> list[dict[str, Any]]:
    stream.seek(0, os.SEEK_END)
    if stream.tell() > MAX_BYTES:
        raise ValueError("project event log exceeds its byte limit")
    stream.seek(0)
    rows: list[dict[str, Any]] = []
    for number, line in enumerate(stream, 1):
        try:
            row = json.loads(line)
        except ValueError as exc:
            raise ValueError(f"project event line {number} is not valid JSON") from exc
        rows.append(_event(row))
    return rows


def append(directory: str | Path, *, source: str, actor: str, text: str) -> dict[str, Any]:
    """Append one validated event and return it with its sequence number."""
    if source not in SOURCES or actor not in ACTORS:
        raise ValueError("event source or actor is unsupported")
    text = _text(text).strip()
    root, _project = _load(directory)
    path = _path(root)
    with path.open("a+", encoding="utf-8") as stream:
        fcntl.flock(stream.fileno(), fcntl.LOCK_EX)
        try:
            rows = _parse(stream)
            if len(rows) >= MAX_EVENTS:
                raise ValueError("project event log is full")
            event = {
                "sequence": len(rows) + 1,
                "occurred_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
                "source": source,
                "actor": actor,
                "text": text,
            }
            stream.seek(0, os.SEEK_END)
            stream.write(json.dumps(event, sort_keys=True, separators=(",", ":")) + "\n")
            stream.flush()
        finally:
            fcntl.flock(stream.fileno(), fcntl.LOCK_UN)
    return {"ok": True, "event": event}


def page(
    directory: str | Path,
    *,
    offset: int = 0,
    limit: int = 50,
    source: str | None = None,
    query: str = "",
    since_revision: int | None = None,
    follow: bool = False,
    wait_seconds: int = 0,
) -> dict[str, Any]:
    """Read a newest-first page, optionally waiting for events after a revision."""
    if (
        type(offset) is not int
        or offset < 0
        or type(limit) is not int
        or not 1 <= limit <= MAX_LIMIT
    ):
        raise ValueError(f"offset must be nonnegative and limit must be 1..{MAX_LIMIT}")
    if source is not None and source not in SOURCES:
        raise ValueError("event source filter is unsupported")
    if not isinstance(query, str) or len(query) > MAX_QUERY:
        raise ValueError(f"query must be at most {MAX_QUERY} characters")
    if since_revision is not None and (type(since_revision) is not int or since_revision < 0):
        raise ValueError("since_revision must be a nonnegative integer")
    if type(follow) is not bool:
        raise ValueError("follow must be a boolean")
    if type(wait_seconds) is not int or not 0 <= wait_seconds <= MAX_FOLLOW_SECONDS:
        raise ValueError(f"wait_seconds must be within 0..{MAX_FOLLOW_SECONDS}")
    if follow and since_revision is None:
        raise ValueError("follow requires since_revision")

    root, _project = _load(directory)
    path = _path(root)

    def snapshot() -> dict[str, Any]:
        if path.exists():
            with path.open("r", encoding="utf-8") as stream:
                fcntl.flock(stream.fileno(), fcntl.LOCK_SH)
                try:
                    rows = _parse(stream)
                finally:
                    fcntl.flock(stream.fileno(), fcntl.LOCK_UN)
        else:
            rows = []
        revision = rows[-1]["sequence"] if rows else 0
        needle = query.casefold()
        matched = [
            row
            for row in reversed(rows)
            if (since_revision is None or row["sequence"] > since_revision)
            and (source is None or row["source"] == source)
            and (not needle or needle in row["text"].casefold())
        ]
        total = len(matched)
        return {
            "ok": True,
            "revision": revision,
            "not_modified": since_revision is not None and revision <= since_revision,
            "total": total,
            "items": matched[offset : offset + limit],
            "pagination": {
                "total": total,
                "offset": offset,
                "limit": limit,
                "next_offset": offset + limit if offset + limit < total else None,
                "previous_offset": max(0, offset - limit) if offset else None,
            },
        }

    result = snapshot()
    if follow and result["not_modified"] and wait_seconds:
        deadline = time.monotonic() + wait_seconds
        while time.monotonic() < deadline:
            time.sleep(min(0.25, max(0.0, deadline - time.monotonic())))
            result = snapshot()
            if not result["not_modified"]:
                break
    return result
