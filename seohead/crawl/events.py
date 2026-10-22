"""Bounded, redacted crawl event records with explicit timestamp and coverage state."""

from __future__ import annotations

import copy
import re
from datetime import datetime, timezone
from typing import Any

from seohead.core.common import utc_iso_z

FORMAT = "seohead.crawl-event.v1"
MAX_EVENTS = 10_000
MAX_PAYLOAD_BYTES = 4_096
EVENT_TYPES = frozenset(
    {
        "queue",
        "request",
        "retry",
        "cache",
        "throttle",
        "render",
        "circuit",
        "budget",
        "checkpoint",
        "stop",
        "provider_enrichment",
    }
)
_PAYLOAD_KEYS = {
    "queue": {"queue_ordinal", "url_id", "depth", "reason", "count"},
    "request": {"queue_ordinal", "url_id", "attempt", "method", "state"},
    "retry": {"queue_ordinal", "url_id", "attempt", "reason", "delay_ms"},
    "cache": {"queue_ordinal", "url_id", "state"},
    "throttle": {"delay_ms", "concurrency", "state", "previous_delay_ms"},
    "render": {"url_id", "representation", "state"},
    "circuit": {"kind", "state", "streak"},
    "budget": {"kind", "limit", "used"},
    "checkpoint": {"state", "queued", "inflight"},
    "stop": {"reason"},
    "provider_enrichment": {"provider", "operation", "state", "rows"},
}
_SENSITIVE = re.compile(r"(?:token|secret|password|credential|authorization|cookie)", re.I)
_IDENTIFIER_SENSITIVE = {"url", "query", "filter", "host", "site_url", "target_url"}
_UNSET = object()


def _timestamp(value: Any) -> tuple[str | None, str]:
    if value is _UNSET:
        return utc_iso_z(), "known"
    if value is None:
        return None, "unknown"
    if not isinstance(value, str):
        raise ValueError("event timestamp must be RFC3339 UTC text or null")
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as exc:
        raise ValueError("event timestamp must be RFC3339 UTC text or null") from exc
    if parsed.tzinfo is None or parsed.utcoffset() != timezone.utc.utcoffset(parsed):
        raise ValueError("event timestamp must be RFC3339 UTC text or null")
    return value, "known"


def _payload(event_type: str, value: Any) -> dict[str, Any]:
    if (
        event_type not in EVENT_TYPES
        or not isinstance(value, dict)
        or set(value) - _PAYLOAD_KEYS[event_type]
    ):
        raise ValueError("event payload does not match its event type")
    clean = {}
    for key, item in value.items():
        if _SENSITIVE.search(key) or key in _IDENTIFIER_SENSITIVE:
            clean[key] = "[redacted]"
        elif item is None or type(item) in {str, int, float, bool}:
            if type(item) is str and len(item) > 512:
                raise ValueError("event payload text exceeds its bound")
            clean[key] = item
        else:
            raise ValueError("event payload values must be bounded scalar facts")
    import json

    if len(json.dumps(clean, sort_keys=True, allow_nan=False).encode()) > MAX_PAYLOAD_BYTES:
        raise ValueError("event payload exceeds its byte bound")
    return clean


def validate(event: Any) -> dict[str, Any]:
    """Validate one serializable event without fabricating timestamps or source detail."""
    if not isinstance(event, dict) or set(event) != {
        "format",
        "sequence",
        "event_type",
        "occurred_at",
        "timestamp_state",
        "payload",
    }:
        raise ValueError("event has unsupported fields")
    if event["format"] != FORMAT or type(event["sequence"]) is not int or event["sequence"] < 1:
        raise ValueError("event format or sequence is invalid")
    occurred_at, timestamp_state = _timestamp(event["occurred_at"])
    if event["timestamp_state"] != timestamp_state:
        raise ValueError("event timestamp state disagrees with its timestamp")
    return {
        "format": FORMAT,
        "sequence": event["sequence"],
        "event_type": event["event_type"],
        "occurred_at": occurred_at,
        "timestamp_state": timestamp_state,
        "payload": _payload(event["event_type"], event["payload"]),
    }


class EventSink:
    """In-memory bounded event sink; a writer may persist each accepted event transactionally."""

    def __init__(self, *, cap: int = MAX_EVENTS, writer: Any = None) -> None:
        if type(cap) is not int or not 1 <= cap <= MAX_EVENTS:
            raise ValueError(f"event cap must be an integer from 1 to {MAX_EVENTS}")
        self.cap = cap
        self.writer = writer
        self.events: list[dict[str, Any]] = []
        self.dropped = 0

    def emit(
        self, event_type: str, payload: dict[str, Any], *, occurred_at: Any = _UNSET
    ) -> dict[str, Any] | None:
        """Append a typed record, or record omitted coverage when the bounded cap is reached."""
        if len(self.events) >= self.cap:
            self.dropped += 1
            return None
        timestamp, timestamp_state = _timestamp(occurred_at)
        event = validate(
            {
                "format": FORMAT,
                "sequence": len(self.events) + 1,
                "event_type": event_type,
                "occurred_at": timestamp,
                "timestamp_state": timestamp_state,
                "payload": payload,
            }
        )
        if self.writer is not None:
            self.writer(event)
        self.events.append(event)
        return copy.deepcopy(event)

    def coverage(self) -> dict[str, Any]:
        return {
            "state": "partial" if self.dropped else "complete",
            "captured": len(self.events),
            "dropped": self.dropped,
            "cap": self.cap,
        }

    def snapshot(self) -> dict[str, Any]:
        return {
            "format": "seohead.crawl-timeline.v1",
            "events": copy.deepcopy(self.events),
            "coverage": self.coverage(),
        }
