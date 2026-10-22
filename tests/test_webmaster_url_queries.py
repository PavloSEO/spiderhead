import asyncio
import io
import json
import urllib.error

from seohead import cli
from seohead.data_sources import yandex_webmaster as wm
from seohead.mcp import handlers


def test_url_query_route_uses_post_filters_and_keeps_rows_separate():
    calls = []

    def send(method, url, payload, token):
        calls.append((method, url, payload, token))
        if method == "GET":
            return json.dumps({"hosts": [{"host_id": "https:example.test:443"}]})
        if payload["text_indicator"] == "URL":
            return json.dumps(
                {
                    "text_indicator_to_statistics": [
                        {"text_indicator": {"value": "https://example.test/a"}}
                    ]
                }
            )
        return json.dumps(
            {
                "text_indicator_to_statistics": [
                    {
                        "text_indicator": {"value": "pump"},
                        "statistics": [
                            {"date": "2026-10-01", "field": "IMPRESSIONS", "value": 4},
                            {"date": "2026-10-01", "field": "CLICKS", "value": 1},
                        ],
                    }
                ]
            }
        )

    result = wm.url_queries(
        "https:example.test:443",
        url="https://example.test/a",
        token="synthetic",
        user_id="7",
        transport=send,
    )
    assert result["ok"] and result["rows"][0]["query"] == "pump"
    assert result["rows"][0]["daily"][0]["ctr"] == 0.25
    assert [call[0] for call in calls] == ["GET", "POST", "POST"]
    assert all("query-analytics/list" in call[1] for call in calls[1:])
    assert calls[1][2]["filters"]["text_filters"][0]["operation"] == "TEXT_MATCH"


def test_url_query_failures_do_not_echo_token():
    result = wm.url_queries("h", token="canary", user_id="7", transport=lambda *_: "not json")
    assert result["ok"] is False and "canary" not in json.dumps(result)


def test_url_queries_paginate_with_explicit_cap():
    def send(_method, _url, payload, _token):
        if payload is None:
            return json.dumps({"hosts": [{"host_id": "h"}]})
        if payload["text_indicator"] == "URL":
            return json.dumps(
                {
                    "count": 2,
                    "text_indicator_to_statistics": [
                        {"text_indicator": {"value": f"https://example.test/{payload['offset']}"}}
                    ],
                }
            )
        return json.dumps(
            {
                "count": 1,
                "text_indicator_to_statistics": [
                    {
                        "text_indicator": {"value": "pump"},
                        "statistics": [{"date": "2026-10-01", "field": "IMPRESSIONS", "value": 1}],
                    }
                ],
            }
        )

    result = wm.url_queries("h", max_urls=2, token="t", user_id="7", transport=send)
    assert result["returned_urls"] == 2 and result["state"] == "complete"


def test_query_route_retries_rate_limits_and_returns_only_provider_error_code():
    calls = 0

    def send(_method, _url, _payload, _token):
        nonlocal calls
        if _payload is None:
            return json.dumps({"hosts": [{"host_id": "h"}]})
        calls += 1
        if calls < 3:
            raise urllib.error.HTTPError("https://api.test", 429, "rate", {}, io.BytesIO(b"{}"))
        raise urllib.error.HTTPError(
            "https://api.test", 403, "denied", {}, io.BytesIO(b'{"error_code":"HOST_NOT_FOUND"}')
        )

    result = wm.url_queries("h", token="canary", user_id="7", transport=send)
    assert result == {"ok": False, "state": "failed", "error_code": "HOST_NOT_FOUND"}
    assert calls == 3 and "canary" not in json.dumps(result)


def test_cli_url_query_flags_reach_the_shared_handler(monkeypatch):
    captured = {}
    monkeypatch.setitem(
        handlers.HANDLERS,
        "webmaster_url_queries",
        lambda **kwargs: captured.update(kwargs) or {"ok": True},
    )
    assert (
        cli.main(
            [
                "webmaster-url-queries",
                "--host-id",
                "h",
                "--url",
                "https://example.test/a",
                "--max-urls",
                "2",
                "--start-date",
                "2026-10-01",
                "--end-date",
                "2026-10-03",
            ]
        )
        == 0
    )
    assert captured == {
        "host_id": "h",
        "url": "https://example.test/a",
        "max_urls": 2,
        "start_date": "2026-10-01",
        "end_date": "2026-10-03",
    }


def test_mcp_date_options_forward_to_the_same_handler(monkeypatch):
    captured = {}

    def fake(**kwargs):
        captured.update(kwargs)
        return {"ok": True}

    monkeypatch.setattr(handlers, "webmaster_url_queries", fake)
    from seohead.mcp.mcp_server import build_server

    tool = build_server()._tool_manager.get_tool("seo_webmaster_url_queries")
    assert asyncio.run(
        tool.run({"host_id": "h", "start_date": "2026-10-01", "end_date": "2026-10-03"})
    ) == {"ok": True}
    assert captured["start_date"] == "2026-10-01" and captured["end_date"] == "2026-10-03"


def test_local_date_filter_marks_unobserved_days_without_zero_filling():
    def send(_method, _url, payload, _token):
        if payload is None:
            return json.dumps({"hosts": [{"host_id": "h"}]})
        if payload["text_indicator"] == "URL":
            return json.dumps(
                {
                    "text_indicator_to_statistics": [
                        {"text_indicator": {"value": "https://example.test/a"}}
                    ]
                }
            )
        from datetime import date, timedelta

        observed = (date.today() - timedelta(days=1)).isoformat()
        return json.dumps(
            {
                "text_indicator_to_statistics": [
                    {
                        "text_indicator": {"value": "pump"},
                        "statistics": [{"date": observed, "field": "IMPRESSIONS", "value": 1}],
                    }
                ]
            }
        )

    from datetime import date, timedelta

    today = date.today()
    observed = (today - timedelta(days=1)).isoformat()  # inside the provider's rolling window
    edges = [today.isoformat(), (today - timedelta(days=2)).isoformat()]
    result = wm.url_queries(
        "h",
        url="https://example.test/a",
        start_date=edges[1],
        end_date=edges[0],
        token="t",
        user_id="7",
        transport=send,
    )
    assert result["coverage"] == {
        "requested_days": sorted(edges + [observed]),
        "observed_days": [observed],
        "unobserved_days": sorted(edges),
        "state": "partial_or_unknown",
    }


def test_unlisted_host_is_not_granted_without_query_request():
    result = wm.url_queries(
        "missing", token="t", user_id="7", transport=lambda *_args: json.dumps({"hosts": []})
    )
    assert result == {"ok": False, "state": "not_granted", "host_id": "missing"}
