"""Pending browser-consent flow record for GSC grants.

The record holds lifecycle metadata only: no grant material, no client secret, no
authorization code. The raw ``state`` is returned to the caller once and stored as a
SHA-256 digest, so the file alone cannot complete a callback.
"""

from __future__ import annotations

import hashlib
import json
import secrets
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from seohead.core.filesystem import atomic_write_bytes
from seohead.data_sources.credentials import is_private_mode

FLOW_TTL = timedelta(seconds=600)
_BASE_KEYS = {"flow_id", "state_sha256", "status", "created_at", "expires_at"}
_EXTRA_KEYS = {"connected_at", "token_expires_at"}
_STATES = {"waiting", "connected", "expired", "cancelled", "revoked"}


def hash_state(state: str) -> str:
    return hashlib.sha256(state.encode("utf-8")).hexdigest()


def _iso(value: datetime) -> str:
    return value.astimezone(timezone.utc).isoformat()


def _check_path(path: Path) -> None:
    if path.parent.is_symlink() or path.is_symlink():
        raise ValueError("OAuth storage must not use symlinks")


def _write(path: Path, record: dict[str, Any]) -> None:
    atomic_write_bytes(
        path, json.dumps(record, sort_keys=True).encode("utf-8"), prefix=".oauth-flow-", mode=0o600
    )


def create_flow(path: Path, now: datetime) -> tuple[dict[str, Any], str]:
    """Start a waiting flow, replacing any earlier pending one.

    Returns the stored record and the raw state, which is never written to disk.
    """
    _check_path(path)
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    state = secrets.token_urlsafe(24)
    record = {
        "flow_id": secrets.token_urlsafe(16),
        "state_sha256": hash_state(state),
        "status": "waiting",
        "created_at": _iso(now),
        "expires_at": _iso(now + FLOW_TTL),
        "connected_at": None,
        "token_expires_at": None,
    }
    _write(path, record)
    return record, state


def load_flow(path: Path) -> dict[str, Any] | None:
    _check_path(path)
    if not path.exists():
        return None
    if not path.is_file() or not is_private_mode(path.stat().st_mode) or path.stat().st_size > 4096:
        raise ValueError("OAuth flow record must be a private bounded regular file")
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise ValueError("OAuth flow record is unreadable") from exc
    if (
        not isinstance(value, dict)
        or frozenset(value) not in {frozenset(_BASE_KEYS), frozenset(_BASE_KEYS | _EXTRA_KEYS)}
        or value.get("status") not in _STATES
        or not isinstance(value.get("flow_id"), str)
        or not 1 <= len(value["flow_id"]) <= 64
        or not isinstance(value.get("state_sha256"), str)
        or len(value["state_sha256"]) != 64
    ):
        raise ValueError("OAuth flow record is invalid")
    try:
        datetime.fromisoformat(value["created_at"])
        datetime.fromisoformat(value["expires_at"])
        for name in _EXTRA_KEYS:
            if value.get(name) is not None:
                datetime.fromisoformat(value[name])
    except (TypeError, ValueError) as exc:
        raise ValueError("OAuth flow record is invalid") from exc
    return {
        "connected_at": None,
        "token_expires_at": None,
        **value,
    }


def effective_status(flow: dict[str, Any], now: datetime) -> str:
    if flow["status"] == "waiting" and now >= datetime.fromisoformat(flow["expires_at"]):
        return "expired"
    return flow["status"]


def cancel_flow(path: Path, now: datetime) -> dict[str, Any] | None:
    """Mark a waiting flow cancelled. Repeated cancels are no-ops returning the same status."""
    flow = load_flow(path)
    if flow is None:
        return None
    status = effective_status(flow, now)
    if status == "waiting":
        flow["status"] = status = "cancelled"
        _write(path, flow)
    return {**flow, "status": status}


def verify_callback_state(path: Path, state: str, now: datetime) -> dict[str, Any]:
    """Validate one-time callback correlation and reject cancelled or expired flows."""
    flow = load_flow(path)
    if flow is None:
        raise ValueError("no_active_flow")
    if effective_status(flow, now) != "waiting":
        raise ValueError("oauth_flow_not_waiting")
    if not isinstance(state, str) or not 1 <= len(state) <= 512 or not secrets.compare_digest(
        flow["state_sha256"], hash_state(state)
    ):
        raise ValueError("oauth_state_mismatch")
    return flow


def complete_flow(path: Path, flow: dict[str, Any], now: datetime, expires_in: int) -> dict[str, Any]:
    """Mark a flow connected only after its caller has completed a verified token exchange."""
    if type(expires_in) is not int or not 1 <= expires_in <= 86400:
        raise ValueError("OAuth token expiry is outside the supported range")
    current = load_flow(path)
    if (
        current is None
        or current["flow_id"] != flow.get("flow_id")
        or effective_status(current, now) != "waiting"
    ):
        raise ValueError("oauth_flow_not_waiting")
    flow = current
    flow["status"] = "connected"
    flow["connected_at"] = _iso(now)
    flow["token_expires_at"] = _iso(now + timedelta(seconds=expires_in))
    _write(path, flow)
    return flow


def mark_revoked(path: Path) -> dict[str, Any] | None:
    """Mark stored flow metadata revoked after explicit local disconnect or remote revoke."""
    flow = load_flow(path)
    if flow is None:
        return None
    flow["status"] = "revoked"
    _write(path, flow)
    return flow
