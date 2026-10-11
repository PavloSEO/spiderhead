"""Mock-only tests for explicit local Logpush import and finite Instant Logs capture."""

import json
import threading

import pytest

from seohead.checks import logstore
from seohead.data_sources import cloudflare, cloudflare_logs
from seohead.projects import workspace

TOKEN = "synthetic-cloudflare-token"


def _project(tmp_path):
    return workspace.create_project(str(tmp_path / "project"), "https://example.test")


def _enterprise(monkeypatch):
    monkeypatch.setattr(cloudflare, "resolve_project_zone", lambda _: "zone-id-1")
    monkeypatch.setattr(
        cloudflare,
        "resolve_zone_id",
        lambda *args: {"id": "zone-id-1", "name": "example.test", "plan": "Enterprise Website"},
    )


def _record():
    return {
        "ClientIP": "203.0.113.1",
        "ClientRequestURI": "/page?secret=query-value",
        "EdgeStartTimestamp": "2026-10-12T00:00:10Z",
        "EdgeResponseStatus": 200,
        "ClientRequestMethod": "GET",
        "ClientRequestUserAgent": "Googlebot/2.1",
        "EdgeResponseBytes": 123,
    }


def test_logpush_file_import_is_local_bounded_and_redacts_records(tmp_path, monkeypatch):
    project = _project(tmp_path)
    monkeypatch.setattr(logstore, "RESERVE_BYTES", 0)
    source = tmp_path / "logpush.ndjson"
    source.write_text(json.dumps(_record()) + "\n", encoding="utf-8")
    result = cloudflare_logs.import_logpush_file(str(source), project["path"])
    assert result["ok"] and result["state"] == "complete"
    assert result["imported"] == 1
    assert "203.0.113.1" not in json.dumps(result)
    assert "secret=query-value" not in json.dumps(result)


def test_instant_logs_requires_confirmation_and_plan_before_post(tmp_path, monkeypatch):
    project = _project(tmp_path)
    monkeypatch.setattr(cloudflare, "resolve_project_zone", lambda _: "zone-id-1")
    monkeypatch.setattr(
        cloudflare,
        "resolve_zone_id",
        lambda *args: {"id": "zone-id-1", "name": "example.test", "plan": "Free Website"},
    )
    with pytest.raises(ValueError, match="confirm_live_capture"):
        cloudflare_logs.instant_logs_capture(
            project["path"],
            token=TOKEN,
            transport=lambda *args: pytest.fail("provider call"),
        )
    result = cloudflare_logs.instant_logs_capture(
        project["path"],
        confirm_live_capture=True,
        token=TOKEN,
        transport=lambda *args: pytest.fail("plan gate must precede POST"),
        websocket_factory=lambda *args, **kwargs: pytest.fail("no socket"),
    )
    assert result["state"] == "unavailable"
    assert result["plan"] == "unsupported"


def test_existing_instant_logs_session_is_never_replaced(tmp_path, monkeypatch):
    project = _project(tmp_path)
    _enterprise(monkeypatch)
    requests = []

    def transport(method, url, body, token):
        requests.append((method, url, body))
        return {"success": True, "result": [{"session_id": "foreign"}]}

    result = cloudflare_logs.instant_logs_capture(
        project["path"],
        confirm_live_capture=True,
        token=TOKEN,
        transport=transport,
        websocket_factory=lambda *args, **kwargs: pytest.fail("active session must prevent socket"),
    )
    assert result["state"] == "unavailable"
    assert len(requests) == 1
    assert requests[0][0] == "GET"


def test_instant_logs_capture_closes_owned_socket_and_imports_only_capped_events(
    tmp_path, monkeypatch
):
    project = _project(tmp_path)
    monkeypatch.setattr(logstore, "RESERVE_BYTES", 0)
    _enterprise(monkeypatch)
    session = "a" * 32
    requests = []

    def transport(method, url, body, token):
        requests.append((method, url, body))
        if method == "GET":
            return {"success": True, "result": []}
        return {
            "success": True,
            "result": {
                "destination_conf": f"wss://logs.cloudflare.com/instant-logs/ws/sessions/{session}"
            },
        }

    class Socket:
        closed = False

        def recv(self, *, timeout):
            return json.dumps(_record())

        def close(self):
            self.closed = True

    socket = Socket()
    ticks = iter((0.0, 0.1, 0.2, 0.3, 1.1, 1.2))
    result = cloudflare_logs.instant_logs_capture(
        project["path"],
        confirm_live_capture=True,
        max_seconds=1,
        max_events=1,
        token=TOKEN,
        transport=transport,
        websocket_factory=lambda url, **kwargs: socket,
        now=lambda: next(ticks),
    )
    assert result["ok"] and result["state"] == "partial"
    assert result["captured_events"] == 1
    assert result["stop_reason"] == "event_limit"
    assert socket.closed
    assert [item[0] for item in requests] == ["GET", "POST"]
    assert requests[1][2]["kind"] == "instant-logs"
    assert TOKEN not in json.dumps(result)
    assert "203.0.113.1" not in json.dumps(result)


def test_instant_logs_rejects_unpinned_websocket_endpoint(tmp_path, monkeypatch):
    project = _project(tmp_path)
    _enterprise(monkeypatch)
    requests = []

    def transport(method, url, body, token):
        requests.append(method)
        if method == "GET":
            return {"success": True, "result": []}
        return {
            "success": True,
            "result": {
                "destination_conf": "wss://evil.example/instant-logs/ws/sessions/" + "c" * 32
            },
        }

    result = cloudflare_logs.instant_logs_capture(
        project["path"],
        confirm_live_capture=True,
        token=TOKEN,
        transport=transport,
        websocket_factory=lambda *args, **kwargs: pytest.fail("unpinned endpoint must not connect"),
    )
    assert result["state"] == "failed"
    assert "expire after five idle minutes" in result["provider_session"]
    assert requests == ["GET", "POST"]


def test_instant_logs_byte_cap_closes_socket_without_importing_oversized_message(
    tmp_path, monkeypatch
):
    project = _project(tmp_path)
    _enterprise(monkeypatch)
    session = "d" * 32

    def transport(method, url, body, token):
        return {
            "success": True,
            "result": []
            if method == "GET"
            else {
                "destination_conf": f"wss://logs.cloudflare.com/instant-logs/ws/sessions/{session}"
            },
        }

    class Socket:
        closed = False

        def recv(self, *, timeout):
            return json.dumps(_record())

        def close(self):
            self.closed = True

    socket = Socket()
    ticks = iter((0.0, 0.1, 0.2))
    result = cloudflare_logs.instant_logs_capture(
        project["path"],
        confirm_live_capture=True,
        max_seconds=1,
        max_bytes=32,
        token=TOKEN,
        transport=transport,
        websocket_factory=lambda url, **kwargs: socket,
        now=lambda: next(ticks),
    )
    assert result["stop_reason"] == "byte_limit"
    assert result["captured_events"] == 0
    assert socket.closed


def test_instant_logs_cancel_closes_socket_and_does_not_touch_foreign_session(
    tmp_path, monkeypatch
):
    project = _project(tmp_path)
    _enterprise(monkeypatch)
    session = "b" * 32

    def transport(method, url, body, token):
        return {
            "success": True,
            "result": []
            if method == "GET"
            else {
                "destination_conf": f"wss://logs.cloudflare.com/instant-logs/ws/sessions/{session}"
            },
        }

    class Socket:
        closed = False

        def recv(self, *, timeout):
            raise AssertionError("pre-cancelled capture must not receive")

        def close(self):
            self.closed = True

    socket = Socket()
    cancelled = threading.Event()
    cancelled.set()
    result = cloudflare_logs.instant_logs_capture(
        project["path"],
        confirm_live_capture=True,
        token=TOKEN,
        transport=transport,
        websocket_factory=lambda url, **kwargs: socket,
        cancel_event=cancelled,
    )
    assert result["stop_reason"] == "cancelled"
    assert socket.closed
    assert result["provider_session"].startswith("socket closed")
