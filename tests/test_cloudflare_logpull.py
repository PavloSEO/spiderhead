"""Mocked Cloudflare Logpull reads feed the shared local server-log store."""

import json
from datetime import datetime, timezone

import pytest

from seohead.data_sources import cloudflare
from seohead.projects import workspace

TOKEN = "synthetic-cloudflare-token"


def _project(tmp_path):
    return workspace.create_project(str(tmp_path / "project"), "https://example.test")


def test_logpull_rejects_non_project_retention_and_bad_windows_before_network():
    with pytest.raises(ValueError, match="pass a project"):
        cloudflare.logpull(
            zone="example.test",
            start="2026-10-12T00:00:00Z",
            end="2026-10-12T00:01:00Z",
            token=TOKEN,
            transport=lambda *args: pytest.fail("network"),
        )
    with pytest.raises(ValueError, match="at most one hour"):
        cloudflare.logpull(
            project="project.test",
            start="2026-10-12T00:00:00Z",
            end="2026-10-12T02:00:00Z",
            now=datetime(2026, 10, 12, 3, tzinfo=timezone.utc),
            token=TOKEN,
            transport=lambda *args: pytest.fail("network"),
        )


def test_logpull_plan_gate_does_not_request_fields_or_logs(monkeypatch, tmp_path):
    project = _project(tmp_path)
    monkeypatch.setattr(
        cloudflare,
        "resolve_project_zone",
        lambda _: "zone-id-1",
    )
    monkeypatch.setattr(
        cloudflare,
        "resolve_zone_id",
        lambda *args: {"id": "zone-id-1", "name": "example.test", "plan": "Free Website"},
    )
    result = cloudflare.logpull(
        project=project["path"],
        start="2026-10-12T00:00:00Z",
        end="2026-10-12T00:01:00Z",
        now=datetime(2026, 10, 12, 1, tzinfo=timezone.utc),
        token=TOKEN,
        transport=lambda *args: pytest.fail("unexpected provider call"),
    )
    assert result["state"] == "unavailable"
    assert "Enterprise" in result["reason"]


def test_enterprise_logpull_retains_through_logstore_without_returning_ips(monkeypatch, tmp_path):
    from seohead.checks import logstore

    project = _project(tmp_path)
    monkeypatch.setattr(logstore, "RESERVE_BYTES", 0)
    monkeypatch.setattr(cloudflare, "resolve_project_zone", lambda _: "zone-id-1")
    monkeypatch.setattr(
        cloudflare,
        "resolve_zone_id",
        lambda *args: {
            "id": "zone-id-1",
            "name": "example.test",
            "plan": "Enterprise Website",
        },
    )
    fields = [
        "ClientIP",
        "ClientRequestURI",
        "EdgeStartTimestamp",
        "EdgeResponseStatus",
        "ClientRequestMethod",
        "ClientRequestUserAgent",
        "EdgeResponseBytes",
    ]
    requests = []

    def send(method, url, body, token):
        requests.append(url)
        assert token == TOKEN
        assert method == "GET"
        if url.endswith("/logs/received/fields"):
            return {"success": True, "result": {name: "field" for name in fields}}
        pytest.fail("raw Logpull must use the bounded raw transport")

    raw = json.dumps(
        {
            "ClientIP": "203.0.113.1",
            "ClientRequestURI": "/page?x=1",
            "EdgeStartTimestamp": "2026-10-12T00:00:10Z",
            "EdgeResponseStatus": 200,
            "ClientRequestMethod": "GET",
            "ClientRequestUserAgent": "Googlebot/2.1",
            "EdgeResponseBytes": 123,
        }
    )
    result = cloudflare.logpull(
        project=project["path"],
        start="2026-10-12T00:00:00Z",
        end="2026-10-12T00:01:00Z",
        count=5,
        now=datetime(2026, 10, 12, 1, tzinfo=timezone.utc),
        token=TOKEN,
        transport=send,
        raw_transport=lambda url, token: raw.encode("utf-8"),
    )
    assert result["ok"] and result["state"] == "complete"
    assert result["log_import"]["lines"]["parsed"] == 1
    assert TOKEN not in json.dumps(result)
    assert "203.0.113.1" not in json.dumps(result)
    assert len(requests) == 1
