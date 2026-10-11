"""Offline tests for the Cloudflare aggregated-traffic source; the API is mocked."""

from __future__ import annotations

import json
from datetime import date, datetime, timedelta

import pytest

from seohead.data_sources import cloudflare

TOKEN = "test-token-not-real"
ZONE = {
    "success": True,
    "result": [
        {
            "id": "z1",
            "name": "example.com",
            "plan": {"name": "Free Website"},
            "status": "active",
            "account": {"id": "a1"},
        }
    ],
}


def _row(count, path, ua, status=200, cache="hit", category="", origin=0, day="2026-10-08"):
    return {
        "count": count,
        "avg": {"sampleInterval": 1},
        "dimensions": {
            "date": day,
            "userAgent": ua,
            "verifiedBotCategory": category,
            "clientRequestPath": path,
            "edgeResponseStatus": status,
            "originResponseStatus": origin,
            "cacheStatus": cache,
        },
    }


GOOGLE = "Mozilla/5.0 (Linux; Android 6.0.1; Nexus 5X) (compatible; Googlebot/2.1)"
HUMAN = "Mozilla/5.0 (Macintosh) Chrome/133"
CLAUDE = "Mozilla/5.0 (compatible; ClaudeBot/1.0)"


def _transport(rows_by_call, calls):
    def send(method, url, body, token):
        assert token == TOKEN
        calls.append((method, url))
        if method == "GET":
            return ZONE
        assert url == cloudflare.GRAPHQL
        rows = rows_by_call(body["variables"])
        return {"data": {"viewer": {"accounts": [{"httpRequestsAdaptiveGroups": rows}]}}}

    return send


def test_traffic_builds_log_analyze_shaped_summary(tmp_path, monkeypatch):
    monkeypatch.setenv("SEOHEAD_SPEND_LOG", str(tmp_path / "spend.jsonl"))
    rows = [
        _row(10, "/a", GOOGLE, 200, "hit", "Search Engine Crawler"),
        _row(2, "/gone", GOOGLE, 404, "dynamic", "Search Engine Crawler"),
        _row(5, "/a", CLAUDE, 200, "miss", "AI Crawler"),
        _row(40, "/", HUMAN, 301, "none"),
        _row(3, "/x", "SomethingElse/1", 200, "hit", "Accessibility"),
    ]
    calls = []
    result = cloudflare.traffic(
        "example.com",
        since="2026-10-08",
        until="2026-10-08",
        transport=_transport(lambda v: rows, calls),
        token=TOKEN,
    )
    assert result["ok"] and result["source"] == "cloudflare-aggregated"
    assert result["plan"] == "Free Website" and result["requests"] == 60
    assert result["by_family"]["googlebot"] == {"Googlebot": 12}
    assert result["by_family"]["ai"] == {"ClaudeBot (Anthropic)": 5}
    assert result["by_family"]["other"] == {"Verified bot: Accessibility": 3}
    assert result["status_by_family"]["googlebot"] == {200: 10, 404: 2}
    assert result["top_paths_by_family"]["googlebot"] == {"/a": 10, "/gone": 2}
    assert result["cache_by_family"]["googlebot"] == {"hit": 10, "dynamic": 2}
    assert result["daily"] == {"2026-10-08": 60}
    assert {
        "host": "unknown",
        "path": "/gone",
        "date": "2026-10-08",
        "bot_family": "googlebot",
        "bot_name": "Googlebot",
        "verified_bot_category": "Search Engine Crawler",
        "edge_status": 404,
        "origin_status": None,
        "cache_status": "dynamic",
        "edge_redirect": False,
        "requests": 2,
    } in result["url_facts"]
    assert result["url_facts_truncated"] is False
    assert result["bots_daily"]["ClaudeBot (Anthropic)"] == {"2026-10-08": 5}
    assert {"path": "/gone", "status": 404, "hits": 2} in result["bot_url_status"]["Googlebot"]
    assert result["verification"]["checked"] is False
    assert any("not raw server logs" in f for f in result["findings"])
    assert any("googlebot: 2 of 12" in f for f in result["findings"])
    assert (tmp_path / "spend.jsonl").read_text().count("cloudflare") == 1
    assert TOKEN not in repr(result)


def test_full_day_is_split_until_rows_fit(tmp_path, monkeypatch):
    monkeypatch.setenv("SEOHEAD_SPEND_LOG", str(tmp_path / "s.jsonl"))
    monkeypatch.setattr(cloudflare, "PAGE_SIZE", 2)
    seen = []

    def rows(v):
        seen.append((v["from"], v["to"]))
        span = datetime.fromisoformat(v["to"][:-1]) - datetime.fromisoformat(v["from"][:-1])
        if span > timedelta(hours=6):
            return [_row(1, "/a", GOOGLE), _row(1, "/b", GOOGLE)]  # full -> must split
        return [_row(1, "/a", GOOGLE)]

    result = cloudflare.traffic(
        "example.com",
        since="2026-10-08",
        until="2026-10-08",
        transport=_transport(rows, []),
        token=TOKEN,
    )
    assert result["queries"] > 1 and result["requests"] > 0


def test_one_hour_window_that_stays_full_is_reported(tmp_path, monkeypatch):
    monkeypatch.setenv("SEOHEAD_SPEND_LOG", str(tmp_path / "s.jsonl"))
    monkeypatch.setattr(cloudflare, "PAGE_SIZE", 1)
    result = cloudflare.traffic(
        "example.com",
        since="2026-10-08",
        until="2026-10-08",
        transport=_transport(lambda v: [_row(1, "/a", GOOGLE)], []),
        token=TOKEN,
    )
    assert result["truncated_days"] == ["2026-10-08"]


def test_url_fact_population_is_hard_bounded(tmp_path, monkeypatch):
    monkeypatch.setenv("SEOHEAD_SPEND_LOG", str(tmp_path / "spend.jsonl"))
    monkeypatch.setattr(cloudflare, "MAX_URL_FACTS", 1)
    rows = [_row(1, "/a", GOOGLE), _row(2, "/b", GOOGLE)]
    result = cloudflare.traffic(
        "example.com",
        since="2026-10-08",
        until="2026-10-08",
        transport=_transport(lambda v: rows, []),
        token=TOKEN,
    )
    assert len(result["url_facts"]) == 1
    assert result["url_facts_truncated"] is True
    assert result["url_facts_omitted"] == 1


def test_graphql_error_is_reported_without_the_token():
    def send(method, url, body, token):
        if method == "GET":
            return ZONE
        return {"data": None, "errors": [{"message": "does not have permission"}]}

    result = cloudflare.traffic("example.com", transport=send, token=TOKEN)
    assert result["ok"] is False and "permission" in result["error"]
    assert TOKEN not in result["error"]


def test_unknown_zone_fails_cleanly():
    result = cloudflare.traffic(
        "nope.example",
        token=TOKEN,
        transport=lambda m, u, b, t: {"success": True, "result": []},
    )
    assert result["ok"] is False and "not visible" in result["error"]


@pytest.mark.parametrize(
    "since,until",
    [("2026-10-09", "2026-10-01"), ("2025-01-01", "2026-10-09"), ("bad", None)],
)
def test_bad_ranges_are_rejected(since, until):
    with pytest.raises(ValueError):
        cloudflare.traffic(
            "example.com",
            since=since,
            until=until,
            today=date(2026, 10, 10),
            token=TOKEN,
            transport=lambda *a: {},
        )


def test_token_is_read_from_env_then_file(tmp_path, monkeypatch):
    monkeypatch.delenv("CLOUDFLARE_API_TOKEN", raising=False)
    f = tmp_path / "t"
    f.write_text("from-file\n")
    f.chmod(0o600)
    monkeypatch.setenv("SEOHEAD_CLOUDFLARE_TOKEN_FILE", str(f))
    assert cloudflare.read_token() == "from-file"
    monkeypatch.setenv("CLOUDFLARE_API_TOKEN", "from-env")
    assert cloudflare.read_token() == "from-env"
    monkeypatch.delenv("CLOUDFLARE_API_TOKEN")
    f.write_text("")
    with pytest.raises(cloudflare.CloudflareError) as exc:
        cloudflare.read_token()
    assert "from-file" not in str(exc.value)


def test_token_file_rejects_symlinks_and_group_readable_files(tmp_path, monkeypatch):
    monkeypatch.delenv("CLOUDFLARE_API_TOKEN", raising=False)
    source = tmp_path / "source"
    source.write_text("synthetic-token")
    source.chmod(0o600)
    link = tmp_path / "link"
    link.symlink_to(source)
    monkeypatch.setenv("SEOHEAD_CLOUDFLARE_TOKEN_FILE", str(link))
    with pytest.raises(cloudflare.CloudflareError):
        cloudflare.read_token()

    link.unlink()
    source.chmod(0o644)
    monkeypatch.setenv("SEOHEAD_CLOUDFLARE_TOKEN_FILE", str(source))
    with pytest.raises(cloudflare.CloudflareError):
        cloudflare.read_token()


def test_verify_token_reports_lifecycle_without_claiming_zone_permissions():
    result = cloudflare.verify_token(
        token=TOKEN,
        transport=lambda method, url, body, token: {
            "success": True,
            "result": {
                "id": "private-token-id",
                "status": "active",
                "expires_on": "2026-12-01T00:00:00Z",
            },
        },
    )
    assert result == {"ok": True, "token_status": "active"}
    assert "private-token-id" not in repr(result)


def test_project_binding_resolves_cloudflare_zone_id_before_graphql(monkeypatch, tmp_path):
    from seohead.projects import bindings

    (tmp_path / "project.json").write_text("{}", encoding="utf-8")
    seen = []
    monkeypatch.setattr(
        bindings,
        "list_bindings",
        lambda project: {
            "bindings": [
                {
                    "service": "cloudflare",
                    "resource": "zone-id-1",
                    "status": "selected",
                }
            ]
        },
    )

    def send(method, url, body, token):
        seen.append((method, url))
        assert token == TOKEN
        if method == "GET":
            assert url == f"{cloudflare.API}/zones/zone-id-1"
            return {
                "success": True,
                "result": {
                    "id": "zone-id-1",
                    "name": "example.com",
                    "account": {"id": "account-1"},
                    "plan": {"name": "Free Website"},
                    "status": "active",
                },
            }
        return {"data": {"viewer": {"accounts": [{"httpRequestsAdaptiveGroups": []}]}}}

    result = cloudflare.traffic(
        project=str(tmp_path),
        since="2026-10-08",
        until="2026-10-08",
        transport=send,
        token=TOKEN,
    )
    assert result["ok"] and result["zone"] == "example.com"
    assert result["history"]["state"] == "stored"
    assert seen[0][1].endswith("/zones/zone-id-1")


def test_project_binding_requires_one_selected_cloudflare_zone(monkeypatch):
    from seohead.projects import bindings

    monkeypatch.setattr(bindings, "list_bindings", lambda project: {"bindings": []})
    result = cloudflare.traffic(
        project="project.test", token=TOKEN, transport=lambda *args: pytest.fail("network")
    )
    assert result["ok"] is False
    assert "exactly one selected Cloudflare zone binding" in result["error"]


def test_configuration_flag_reads_zone_ai_settings_and_bounded_rulesets(tmp_path, monkeypatch):
    monkeypatch.setenv("SEOHEAD_SPEND_LOG", str(tmp_path / "spend.jsonl"))
    calls = []

    def send(method, url, body, token):
        calls.append((method, url))
        assert token == TOKEN
        if url.endswith("/zones?name=example.com"):
            return ZONE
        if "/settings/" in url:
            setting = url.rsplit("/", 1)[-1]
            return {
                "success": True,
                "result": {"id": setting, "value": "on", "editable": True},
            }
        if "/rulesets/phases/" in url:
            phase = url.split("/phases/", 1)[1].split("/", 1)[0]
            return {
                "success": True,
                "result": {
                    "phase": phase,
                    "last_updated": "2026-10-01T00:00:00Z",
                    "rules": [
                        {
                            "id": f"rule-{phase}",
                            "description": "synthetic policy",
                            "action": "block",
                            "enabled": True,
                            "expression": 'http.request.uri.path eq "/private"',
                        }
                    ],
                },
            }
        if "/logpush/jobs?" in url:
            return {
                "success": True,
                "result": [
                    {
                        "id": 12,
                        "dataset": "http_requests",
                        "enabled": True,
                        "destination_conf": "must-not-be-returned",
                    }
                ],
                "result_info": {"total_count": 1},
            }
        assert url == cloudflare.GRAPHQL
        return {"data": {"viewer": {"accounts": [{"httpRequestsAdaptiveGroups": []}]}}}

    result = cloudflare.traffic(
        "example.com",
        since="2026-10-08",
        until="2026-10-08",
        include_configuration=True,
        transport=send,
        token=TOKEN,
    )
    assert result["ok"]
    assert result["configuration"]["state"] == "complete"
    assert result["configuration"]["zone_settings"]["search_for_agents"]["value"] == "on"
    assert (
        result["configuration"]["rulesets"]["http_request_firewall_custom"]["rules"][0]["action"]
        == "block"
    )
    assert result["configuration"]["logpush"]["jobs"][0]["dataset"] == "http_requests"
    assert "destination_conf" not in json.dumps(result)
    assert result["configuration_requests"] == len(cloudflare.AI_SETTINGS) + len(
        cloudflare.RULE_PHASES
    )
    assert len(calls) == 1 + result["configuration_requests"] + result["queries"]
