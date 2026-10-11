"""Bounded local Logpush imports and explicitly started Instant Logs captures."""

from __future__ import annotations

import ipaddress
import json
import os
import re
import stat
import tempfile
import threading
import time
import urllib.parse
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable

from seohead.data_sources import cloudflare

MAX_INPUT_BYTES = 16 * 1024 * 1024
MAX_INPUT_ROWS = 10_000
MAX_LINE_BYTES = 65_536
MAX_CAPTURE_BYTES = 8 * 1024 * 1024
MAX_CAPTURE_EVENTS = 1_000
MAX_CAPTURE_SECONDS = 60
INSTANT_FIELDS = (
    "ClientIP",
    "ClientRequestURI",
    "EdgeStartTimestamp",
    "EdgeResponseStatus",
    "ClientRequestMethod",
    "ClientRequestUserAgent",
    "EdgeResponseBytes",
)
_SESSION_PATH = re.compile(r"^/instant-logs/ws/sessions/[a-fA-F0-9]{32}$")


def _apache_quote(value: Any) -> str:
    return urllib.parse.quote(
        str(value or "-"), safe="/?:=&%#+,;@!$'()*-._~"
    )


def _to_apache(row: dict[str, Any]) -> str:
    required = ("ClientIP", "ClientRequestURI", "EdgeStartTimestamp", "EdgeResponseStatus")
    if any(name not in row for name in required):
        raise ValueError("record is missing required HTTP request fields")
    address = str(row["ClientIP"])
    try:
        ipaddress.ip_address(address)
    except ValueError:
        raise ValueError("record has an invalid client IP") from None
    stamp_value = row["EdgeStartTimestamp"]
    if not isinstance(stamp_value, str):
        raise ValueError("record has an invalid edge timestamp")
    try:
        stamp = datetime.fromisoformat(stamp_value.replace("Z", "+00:00"))
    except ValueError:
        raise ValueError("record has an invalid edge timestamp") from None
    if stamp.tzinfo is None:
        raise ValueError("record timestamp must include a timezone")
    stamp = stamp.astimezone(timezone.utc)
    status = row["EdgeResponseStatus"]
    if type(status) is not int or not 100 <= status <= 599:
        raise ValueError("record has an invalid HTTP status")
    size = row.get("EdgeResponseBytes")
    if size is not None and (type(size) is not int or size < 0):
        raise ValueError("record has invalid response bytes")
    method = row.get("ClientRequestMethod") or "GET"
    agent = row.get("ClientRequestUserAgent") or "-"
    return (
        f'{address} - - [{stamp.strftime("%d/%b/%Y:%H:%M:%S +0000")}] '
        f'"{_apache_quote(method)} {_apache_quote(row["ClientRequestURI"])} HTTP/1.1" '
        f"{status} {size if size is not None else '-'} \"-\" \"{_apache_quote(agent)}\""
    )


def _write_project_import(project: str, lines: list[str], *, complete: bool) -> dict[str, Any]:
    if not lines:
        return {"ok": True, "state": "empty", "imported": 0, "complete": complete}
    from seohead.projects.workspace import open_project

    root = Path(open_project(project)["path"])
    directory = root / "logs"
    if directory.is_symlink() or (directory.exists() and not directory.is_dir()):
        raise ValueError("project logs directory is unsafe")
    directory.mkdir(mode=0o700, exist_ok=True)
    temporary_path = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            newline="\n",
            prefix=".cloudflare-import-",
            suffix=".log",
            dir=directory,
            delete=False,
        ) as handle:
            temporary_path = Path(handle.name)
            for line in lines:
                handle.write(line + "\n")
            if not complete:
                handle.write("\n")
        from seohead.checks.logstore import import_logs

        result = import_logs(str(temporary_path), project, max_lines=len(lines))
    finally:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)
    if not result.get("ok"):
        return {"ok": False, "state": "failed", "reason": result.get("error") or "local log import failed"}
    return {
        "ok": True,
        "state": "partial" if not complete or not result.get("complete") else "complete",
        "imported": result.get("lines", {}).get("parsed", 0),
        "complete": complete and bool(result.get("complete")),
        "log_import": result,
    }


def import_logpush_file(
    input_path: str,
    project: str,
    *,
    max_events: int = MAX_INPUT_ROWS,
) -> dict[str, Any]:
    """Convert a caller-supplied Logpush JSON Lines file into the local logstore."""
    if not project:
        raise ValueError("project is required for local Logpush retention")
    if type(max_events) is not int or not 1 <= max_events <= MAX_INPUT_ROWS:
        raise ValueError(f"max_events must be between 1 and {MAX_INPUT_ROWS}")
    path = Path(input_path)
    before = path.lstat()
    if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or before.st_size > MAX_INPUT_BYTES:
        raise ValueError("Logpush input must be an unaliased regular file no larger than 16 MiB")
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        if (
            not stat.S_ISREG(opened.st_mode)
            or opened.st_nlink != 1
            or opened.st_size > MAX_INPUT_BYTES
            or opened.st_ino != before.st_ino
            or opened.st_dev != before.st_dev
        ):
            raise ValueError("Logpush input changed while it was being opened")
        with os.fdopen(descriptor, "rb") as stream:
            raw = stream.read(MAX_INPUT_BYTES + 1)
        descriptor = -1
    finally:
        if descriptor >= 0:
            os.close(descriptor)
    if len(raw) > MAX_INPUT_BYTES:
        raise ValueError("Logpush input exceeds the 16 MiB byte limit")
    try:
        text = raw.decode("utf-8")
    except UnicodeError:
        raise ValueError("Logpush input must be UTF-8 JSON Lines") from None
    lines = [line for line in text.splitlines() if line]
    if len(lines) > max_events:
        raise ValueError("Logpush input exceeds max_events")
    parsed = []
    try:
        for line in lines:
            if len(line.encode("utf-8")) > MAX_LINE_BYTES:
                raise ValueError("Logpush JSON line exceeds the 64 KiB limit")
            row = json.loads(line)
            if not isinstance(row, dict):
                raise ValueError("Logpush line must contain a JSON object")
            parsed.append(_to_apache(row))
    except (json.JSONDecodeError, TypeError, ValueError) as exc:
        return {"ok": False, "state": "failed", "reason": str(exc)}
    return _write_project_import(project, parsed, complete=True)


def _websocket_connect():
    try:
        from websockets.sync.client import connect
    except ImportError:
        raise RuntimeError("optional websockets dependency is unavailable; install the cloudflare-stream extra") from None
    return connect


def instant_logs_capture(
    project: str,
    *,
    confirm_live_capture: bool = False,
    max_seconds: int = 10,
    max_events: int = 100,
    max_bytes: int = 1024 * 1024,
    sample: int = 10,
    token: str | None = None,
    transport: cloudflare.Transport | None = None,
    websocket_factory: Callable[..., Any] | None = None,
    cancel_event: threading.Event | None = None,
    now: Callable[[], float] = time.monotonic,
) -> dict[str, Any]:
    """Start one explicitly confirmed finite Instant Logs session and retain a bounded prefix."""
    if confirm_live_capture is not True:
        raise ValueError("confirm_live_capture=true is required to start an Instant Logs session")
    if not project:
        raise ValueError("project is required for local log retention")
    if type(max_seconds) is not int or not 1 <= max_seconds <= MAX_CAPTURE_SECONDS:
        raise ValueError(f"max_seconds must be between 1 and {MAX_CAPTURE_SECONDS}")
    if type(max_events) is not int or not 1 <= max_events <= MAX_CAPTURE_EVENTS:
        raise ValueError(f"max_events must be between 1 and {MAX_CAPTURE_EVENTS}")
    if type(max_bytes) is not int or not 1 <= max_bytes <= MAX_CAPTURE_BYTES:
        raise ValueError(f"max_bytes must be between 1 and {MAX_CAPTURE_BYTES}")
    if type(sample) is not int or not 1 <= sample <= 100:
        raise ValueError("sample must be between 1 and 100")
    if websocket_factory is None:
        try:
            websocket_factory = _websocket_connect()
        except RuntimeError as exc:
            return {"ok": False, "state": "unavailable", "reason": str(exc)}
    try:
        secret = token or cloudflare.read_token()
        zone_id = cloudflare.resolve_project_zone(project)
        info = cloudflare.resolve_zone_id(zone_id, secret, transport or cloudflare._default_transport)
    except (cloudflare.CloudflareError, OSError, ValueError):
        return {"ok": False, "state": "unavailable", "reason": "selected Cloudflare zone is unavailable"}
    plan = str(info.get("plan") or "").strip().casefold()
    if plan.startswith("business"):
        plan_state = "business"
    elif plan.startswith("enterprise"):
        plan_state = "enterprise"
    else:
        return {
            "ok": False,
            "state": "unavailable",
            "plan": "unknown" if not plan else "unsupported",
            "reason": "Instant Logs requires a confirmed Business or Enterprise zone plan",
        }
    api = transport or cloudflare._default_transport
    zone = urllib.parse.quote(info["id"], safe="")
    base = f"{cloudflare.API}/zones/{zone}/logpush/edge/jobs"
    try:
        existing = api("GET", base, None, secret)
    except Exception:
        return {"ok": False, "state": "unavailable", "plan": plan_state, "reason": "could not check for an active zone session"}
    active_jobs = existing.get("result") if isinstance(existing, dict) else None
    if not isinstance(existing, dict) or not existing.get("success") or not isinstance(active_jobs, list):
        return {"ok": False, "state": "unavailable", "plan": plan_state, "reason": "Instant Logs session list is unavailable"}
    if active_jobs:
        return {"ok": False, "state": "unavailable", "plan": plan_state, "reason": "a zone Instant Logs session is already active"}
    if cancel_event is not None and cancel_event.is_set():
        return {"ok": False, "state": "cancelled", "plan": plan_state, "reason": "capture cancelled before session creation"}
    body = {"fields": ",".join(INSTANT_FIELDS), "filter": "", "sample": sample, "kind": "instant-logs"}
    try:
        created = api("POST", base, body, secret)
    except Exception:
        return {"ok": False, "state": "failed", "plan": plan_state, "reason": "Instant Logs session creation failed"}
    result = created.get("result") if isinstance(created, dict) else None
    destination = result.get("destination_conf") if isinstance(result, dict) else None
    if not created.get("success") or not isinstance(destination, str):
        return {"ok": False, "state": "failed", "plan": plan_state, "reason": "Instant Logs did not return a WebSocket endpoint"}
    parsed = urllib.parse.urlsplit(destination)
    if (
        parsed.scheme != "wss"
        or parsed.hostname != "logs.cloudflare.com"
        or parsed.port not in (None, 443)
        or parsed.username
        or parsed.password
        or parsed.query
        or parsed.fragment
        or not _SESSION_PATH.fullmatch(parsed.path)
        or (
            result.get("session_id") is not None
            and parsed.path.rsplit("/", 1)[-1] != str(result.get("session_id"))
        )
        or (result.get("kind") not in (None, "instant-logs"))
    ):
        return {
            "ok": False,
            "state": "failed",
            "plan": plan_state,
            "reason": "Instant Logs returned an unexpected WebSocket endpoint",
            "provider_session": "created session will expire after five idle minutes; no stop endpoint exists",
        }
    ws = None
    records: list[str] = []
    captured_bytes = 0
    malformed = 0
    truncated = False
    stop_reason = "timeout"
    stream_error = None
    deadline = now() + max_seconds
    try:
        ws = websocket_factory(
            destination,
            open_timeout=min(5, max_seconds),
            close_timeout=2,
            max_size=max_bytes,
            max_queue=1,
            compression=None,
            ping_interval=None,
            proxy=None,
            server_hostname="logs.cloudflare.com",
        )
        while now() < deadline:
            if cancel_event is not None and cancel_event.is_set():
                stop_reason = "cancelled"
                break
            if len(records) >= max_events:
                stop_reason = "event_limit"
                truncated = True
                break
            remaining = deadline - now()
            try:
                message = ws.recv(timeout=max(0.05, min(1.0, remaining)))
            except TimeoutError:
                continue
            if message is None:
                stop_reason = "stream_closed"
                break
            if isinstance(message, bytes):
                message_bytes = len(message)
                if captured_bytes + message_bytes > max_bytes:
                    stop_reason = "byte_limit"
                    truncated = True
                    break
                captured_bytes += message_bytes
                malformed += 1
                continue
            if not isinstance(message, str):
                malformed += 1
                continue
            message_bytes = len(message.encode("utf-8"))
            if captured_bytes + message_bytes > max_bytes:
                stop_reason = "byte_limit"
                truncated = True
                break
            captured_bytes += message_bytes
            for line in message.splitlines():
                if not line:
                    continue
                if len(records) >= max_events:
                    stop_reason = "event_limit"
                    truncated = True
                    break
                try:
                    row = json.loads(line)
                    if not isinstance(row, dict):
                        raise ValueError
                    records.append(_to_apache(row))
                except (json.JSONDecodeError, TypeError, ValueError):
                    malformed += 1
            if stop_reason == "event_limit":
                break
        else:
            stop_reason = "timeout"
    except KeyboardInterrupt:
        stop_reason = "cancelled"
    except Exception:
        stream_error = "Instant Logs WebSocket capture failed"
        stop_reason = "stream_unavailable"
    finally:
        if ws is not None:
            try:
                ws.close()
            except Exception:
                pass
    if stream_error and not records:
        return {
            "ok": False,
            "state": "unavailable" if "optional websockets dependency" in stream_error else "failed",
            "plan": plan_state,
            "reason": stream_error,
            "stop_reason": stop_reason,
            "provider_session": "socket closed or connection unavailable; Cloudflare idle expiry is at most five minutes",
        }
    try:
        imported = _write_project_import(project, records, complete=False) if records else {
            "ok": True,
            "state": "empty",
            "imported": 0,
            "complete": False,
        }
    except Exception:
        return {
            "ok": False,
            "state": "failed",
            "plan": plan_state,
            "reason": "local logstore import failed after capture",
            "stop_reason": stop_reason,
            "provider_session": "socket is closed; Cloudflare auto-stops the owned session after five idle minutes",
        }
    if not imported.get("ok"):
        return {"ok": False, "state": "failed", "plan": plan_state, "reason": imported.get("reason")}
    return {
        "ok": True,
        "state": "partial",
        "plan": plan_state,
        "captured_events": len(records),
        "captured_bytes": captured_bytes,
        "malformed_events": malformed,
        "truncated": truncated,
        "stop_reason": stop_reason,
        "local_import": imported,
        "provider_session": "socket closed; Cloudflare auto-stops sessions after five idle minutes",
        "api_cleanup_endpoint": "not available for Instant Logs sessions",
    }
