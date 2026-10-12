"""Project journal: one write path for crawls, reports, checklist attempts and agent actions.

Every entry is a validated structured event in ``events.jsonl`` and one bullet in
``log.md``, the human narrative. Automatic hooks call ``note`` best-effort so that a
journal problem never fails the crawl, report or checklist step it describes.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from . import event_log


def _bullet(event: dict[str, Any], source: str, actor: str) -> str:
    text = " ".join(event["text"].split())
    return f"- {event['occurred_at']} · {source}/{actor} · {text}\n"


def record(directory: str | Path, *, source: str, actor: str, text: str) -> dict[str, Any]:
    """Append one event and mirror it to log.md. Raises ValueError for invalid input."""
    from .workspace import _load

    root, _project = _load(directory)
    from .runtime import project_write_lock

    with project_write_lock(root):
        event = event_log.append(root, source=source, actor=actor, text=text)["event"]
        with (root / "log.md").open("a", encoding="utf-8") as stream:
            stream.write(_bullet(event, source, actor))
    return {"ok": True, "event": event}


def note(directory: str | Path, *, source: str, actor: str, text: str) -> None:
    """Best-effort record for automatic hooks: a failure is swallowed, never raised."""
    try:
        record(directory, source=source, actor=actor, text=text)
    except (OSError, ValueError):
        return None
