"""Provider readiness and discovery stay offline, explicit, and redacted."""

from __future__ import annotations

import asyncio
import io
import json
import urllib.error

import pytest

from seohead import cli
from seohead.data_sources import credentials, providers


def _isolate_credentials(monkeypatch, tmp_path):
    from seohead.data_sources import oauth

    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path)
    for provider_sources in providers._CREDENTIAL_SOURCES.values():
        for _path, env_var in provider_sources.values():
            monkeypatch.delenv(env_var, raising=False)
    monkeypatch.delenv("GSC_SERVICE_ACCOUNT_FILE", raising=False)


def _service_account(tmp_path):
    account = tmp_path / "gsc" / "service-account.json"
    account.parent.mkdir(exist_ok=True)
    account.write_text(
        json.dumps(
            {
                "type": "service_account",
                "client_email": "synthetic@example.test",
                "private_key": "synthetic-private-key-canary",
                "token_uri": "https://oauth2.googleapis.com/token",
            }
        ),
        encoding="utf-8",
    )
    account.chmod(0o600)
    return account


@pytest.mark.parametrize("operation", ["landing_pages", "page_views"])
@pytest.mark.parametrize("credential", ["service_account", "oauth_bearer", "both"])
def test_ga4_readiness_matches_existing_collection_credentials(
    monkeypatch, tmp_path, operation, credential
):
    from seohead.data_sources import ga4, gsc

    _isolate_credentials(monkeypatch, tmp_path)
    if credential in {"service_account", "both"}:
        _service_account(tmp_path)
    if credential in {"oauth_bearer", "both"}:
        monkeypatch.setenv("GA4_ACCESS_TOKEN", "synthetic-bearer-canary")
    token_requests = []

    def service_token(scope):
        token_requests.append(scope)
        return "synthetic-service-token-canary"

    monkeypatch.setattr(gsc, "service_account_access_token", service_token)
    readiness = providers.provider_readiness("ga4", operation)
    doctor = providers.sources_doctor()["providers"]["ga4"]
    verification = providers.provider_verify("ga4")

    assert readiness["state"] == doctor["readiness_state"] == "configured_unverified"
    assert doctor["state"] == "credential_present"
    assert verification["state"] == "verification_input_required"
    assert verification["authenticated_account"] is False
    assert readiness["credential_components"] == {
        "oauth_bearer": credential in {"oauth_bearer", "both"},
        "service_account": credential in {"service_account", "both"},
    }
    assert verification["permission_state"] == "unknown"
    assert readiness["permission_state"] == "not_verified"
    assert readiness["target_access"] == verification["target_access"] == "not_requested"
    assert readiness["verified"] is verification["verified"] is False
    assert token_requests == []  # Diagnostics never refresh or mint a token.
    public = json.dumps([readiness, doctor, verification])
    assert "canary" not in public and str(tmp_path) not in public
    assert "durable_oauth" not in readiness["credential_components"]

    request = {"property_id": "123", "start_date": "2026-10-01", "end_date": "2026-10-03"}
    if operation == "page_views":
        request["site_origin"] = "https://example.test"
    body = json.dumps(
        {
            "dimensionHeaders": [
                {"name": name} for name in ["date", "hostName", "pagePathPlusQueryString"]
            ],
            "metricHeaders": [{"name": "screenPageViews", "type": "TYPE_INTEGER"}],
            "rows": [],
            "rowCount": 0,
            "metadata": {"timeZone": "UTC"},
        }
    )
    result = providers.provider_collect("ga4", operation, request, transport=lambda *_: body)
    assert result["evidence"]["status"] == "complete"
    assert token_requests == ([ga4.READONLY_SCOPE] if credential == "service_account" else [])


@pytest.mark.parametrize("malformed", [False, True])
def test_ga4_missing_and_invalid_service_account_remain_unavailable(
    monkeypatch, tmp_path, malformed
):
    _isolate_credentials(monkeypatch, tmp_path)
    if malformed:
        _service_account(tmp_path).write_text('{"private_key":"synthetic-canary"', encoding="utf-8")
    report = providers.provider_readiness("ga4")
    assert report["state"] == ("invalid" if malformed else "missing")
    assert report["credential_components"] == {"oauth_bearer": False, "service_account": False}
    assert report["verified"] is False
    assert "synthetic-canary" not in json.dumps(report)
    if malformed:
        assert report["credential_sources"]["service_account"]["reason"] == "malformed_json"


def test_ga4_does_not_reuse_search_console_bearer_or_grant(monkeypatch, tmp_path):
    from seohead.data_sources import oauth

    _isolate_credentials(monkeypatch, tmp_path)
    monkeypatch.setenv("GSC_ACCESS_TOKEN", "synthetic-gsc-bearer")
    monkeypatch.setattr(oauth, "grant_available", lambda provider: provider == "gsc")
    assert providers.provider_readiness("gsc")["state"] == "configured_unverified"
    report = providers.provider_readiness("ga4")
    assert report["state"] == "missing"
    assert "durable_oauth" not in report["credential_components"]


def test_readiness_lists_operation_routes_and_never_infers_target_access(monkeypatch, tmp_path):
    _isolate_credentials(monkeypatch, tmp_path)
    monkeypatch.setattr(
        providers,
        "provider_verify",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(
            AssertionError("readiness must not make or trigger a provider verification")
        ),
    )

    report = providers.provider_readiness("gsc", "search_analytics")

    assert report["ok"] is True
    assert report["state"] == "missing"
    assert report["readiness_state"] == "missing"
    assert report["permission_state"] == "not_verified"
    assert report["target_access"] == "not_requested"
    assert report["verified"] is False
    operation = report["operation"]
    assert operation["name"] == "search_analytics"
    assert operation["state"] == "supported"
    assert operation["surface"] == "provider-collect"
    assert operation["input_schema"]["required"] == ["provider", "operation", "request"]
    assert operation["input_schema"]["properties"]["request"]["type"] == "object"


def test_credentials_show_redacted_environment_and_config_references(monkeypatch, tmp_path):
    _isolate_credentials(monkeypatch, tmp_path)
    monkeypatch.setenv("ARSENKIN_TOKEN", "synthetic-private-token")

    env_report = providers.provider_readiness("arsenkin")
    source = env_report["credential_sources"]["api_token"]
    assert env_report["state"] == "configured_unverified"
    assert source["state"] == "configured_unverified"
    assert source["source_reference"] == "env:ARSENKIN_TOKEN"
    assert "synthetic-private-token" not in json.dumps(env_report)
    assert "/Users/" not in json.dumps(env_report)

    monkeypatch.delenv("ARSENKIN_TOKEN")
    token_file = tmp_path / "arsenkin" / "token"
    token_file.parent.mkdir()
    token_file.write_text("synthetic-file-token", encoding="utf-8")
    file_report = providers.provider_readiness("arsenkin")
    source = file_report["credential_sources"]["api_token"]
    assert source["source_reference"] == "config:arsenkin/token"
    assert "synthetic-file-token" not in json.dumps(file_report)
    assert str(tmp_path) not in json.dumps(file_report)


def test_empty_credential_source_is_invalid_not_configured(monkeypatch, tmp_path):
    _isolate_credentials(monkeypatch, tmp_path)
    token_file = tmp_path / "crux" / "api_key"
    token_file.parent.mkdir()
    token_file.write_text("  \n", encoding="utf-8")

    report = providers.provider_readiness("crux")

    assert report["state"] == "invalid"
    assert report["credential_sources"]["api_key"]["state"] == "invalid"
    assert report["credential_sources"]["api_key"]["source_reference"] == "config:crux/api_key"
    assert report["target_access"] == "not_requested"
    assert report["verified"] is False


def test_invalid_gsc_service_account_is_named_without_echoing_contents(monkeypatch, tmp_path):
    _isolate_credentials(monkeypatch, tmp_path)
    account = tmp_path / "gsc" / "service-account.json"
    account.parent.mkdir()
    account.write_text('{"private_key":"synthetic-private-canary"', encoding="utf-8")
    account.chmod(0o600)

    report = providers.provider_readiness("gsc")

    assert report["state"] == "invalid"
    assert report["credential_sources"]["service_account"]["state"] == "invalid"
    assert report["credential_sources"]["service_account"]["source_reference"] == (
        "config:gsc/service-account.json"
    )
    assert report["target_access"] == "not_requested"
    assert "synthetic-private-canary" not in json.dumps(report)
    assert str(tmp_path) not in json.dumps(report)


def test_declared_but_unsupported_operation_is_explicit(monkeypatch, tmp_path):
    _isolate_credentials(monkeypatch, tmp_path)

    report = providers.provider_readiness("arsenkin", "serp_clustering")

    assert report["ok"] is False
    assert report["state"] == "unsupported"
    assert report["readiness_state"] == "missing"
    assert report["operation"]["state"] == "unsupported"
    assert report["operation"]["surface"] is None


def test_unknown_provider_and_invalid_operation_selectors_are_explicit():
    assert providers.provider_readiness("not-a-provider")["state"] == "unsupported"
    assert providers.provider_readiness("gsc", "")["state"] == "invalid"
    assert providers.provider_readiness(operation="search_analytics")["state"] == "invalid"


def test_operation_discovery_covers_registry_and_separates_surface_states(monkeypatch, tmp_path):
    _isolate_credentials(monkeypatch, tmp_path)
    report = providers.provider_readiness()
    registry = providers.provider_registry()["providers"]

    assert set(report["providers"]) == set(registry)
    for provider, row in report["providers"].items():
        discovered = {operation["name"]: operation for operation in row["operations"]}
        assert set(discovered) == set(registry[provider]["operations"])
        assert all(
            operation["state"] in {"supported", "dedicated", "dedicated_write", "unsupported"}
            for operation in discovered.values()
        )
    assert report["providers"]["arsenkin"]["operations"][0]["state"] == "dedicated"
    assert report["providers"]["indexnow"]["operations"][0]["state"] == "dedicated_write"
    assert report["providers"]["gsc"]["operations"][0]["input_schema"]["type"] == "object"
    assert report["providers"]["wayback"]["permission_state"] == "not_verified"
    assert report["providers"]["wayback"]["target_access"] == "not_requested"


def test_provider_verify_reports_insufficient_scope_and_does_not_echo_secret(monkeypatch, tmp_path):
    _isolate_credentials(monkeypatch, tmp_path)
    monkeypatch.setenv("GSC_ACCESS_TOKEN", "synthetic-secret-token")
    from seohead.data_sources import gsc

    monkeypatch.setattr(
        gsc,
        "discover_properties",
        lambda **_kwargs: {
            "ok": False,
            "state": "verification_failed",
            "status": 403,
            "error": "insufficient_scope: webmasters.readonly is required",
        },
    )

    result = providers.provider_verify("gsc")

    assert result["permission_state"] == "insufficient_scope"
    assert result["target_access"] == "not_requested"
    assert result["verified"] is False
    assert result["credential_sources"]["oauth_bearer"]["source_reference"] == (
        "env:GSC_ACCESS_TOKEN"
    )
    assert "synthetic-secret-token" not in json.dumps(result)


def test_provider_verify_requires_returned_target_to_claim_access(monkeypatch, tmp_path):
    _isolate_credentials(monkeypatch, tmp_path)
    monkeypatch.setenv("GSC_ACCESS_TOKEN", "synthetic-token")
    from seohead.data_sources import gsc

    monkeypatch.setattr(
        gsc,
        "discover_properties",
        lambda **_kwargs: {"ok": True, "properties": [{"site_url": "sc-domain:example.test"}]},
    )
    verified = providers.provider_verify("gsc", {"site_url": "sc-domain:example.test"})
    assert verified["permission_state"] == "verified"
    assert verified["target_access"] == "verified"
    assert verified["verified"] is True

    missing = providers.provider_verify("gsc", {"site_url": "sc-domain:other.test"})
    assert missing["permission_state"] == "not_granted"
    assert missing["target_access"] == "not_granted"
    assert missing["verified"] is False


def test_gsc_verification_error_redacts_echoed_bearer():
    from seohead.data_sources import gsc

    secret = "synthetic-bearer-canary"

    def fail_request(_method, url, _payload, _bearer):
        body = json.dumps({"error": {"message": f"invalid token {secret}"}}).encode()
        raise urllib.error.HTTPError(url, 401, "unauthorized", {}, io.BytesIO(body))

    result = gsc.discover_properties(token=secret, transport=fail_request)

    assert result["ok"] is False and result["status"] == 401
    assert secret not in json.dumps(result)


def test_bing_verification_error_does_not_echo_api_key(monkeypatch, tmp_path):
    _isolate_credentials(monkeypatch, tmp_path)
    secret = "synthetic-bing-key-canary"
    monkeypatch.setenv("BING_WEBMASTER_API_KEY", secret)
    from seohead.data_sources import bing_webmaster

    def fail_request(url):
        raise urllib.error.HTTPError(url, 403, "denied", {}, io.BytesIO(b""))

    result = bing_webmaster.collect("sites", site_url="", transport=fail_request)

    assert result["ok"] is False and result["status"] == 403
    assert secret not in json.dumps(result)


def test_cli_and_mcp_share_provider_readiness_state(monkeypatch, tmp_path, capsys):
    from seohead.mcp import handlers
    from seohead.mcp.mcp_server import build_server

    _isolate_credentials(monkeypatch, tmp_path)
    payload = '{"provider":"gsc","operation":"search_analytics"}'
    expected = {
        "ok": True,
        "format": "seohead.provider-readiness.v1",
        "state": "synthetic",
    }
    calls = []

    def fake_readiness(**kwargs):
        calls.append(kwargs)
        return expected

    monkeypatch.setattr(handlers, "provider_readiness", fake_readiness)
    monkeypatch.setitem(handlers.HANDLERS, "provider_readiness", fake_readiness)

    assert cli.main(["provider-readiness", "--input", payload]) == 0
    cli_result = json.loads(capsys.readouterr().out)
    server = build_server()
    asyncio.run(
        server.call_tool(
            "seo_provider_readiness", {"provider": "gsc", "operation": "search_analytics"}
        )
    )

    assert cli_result == expected
    assert calls == [
        {"provider": "gsc", "operation": "search_analytics"},
        {"provider": "gsc", "operation": "search_analytics"},
    ]


def test_arsenkin_verify_reads_limits_without_claiming_target_access(monkeypatch, tmp_path):
    _isolate_credentials(monkeypatch, tmp_path)
    monkeypatch.setenv("ARSENKIN_TOKEN", "synthetic-arsenkin-canary")
    from seohead.data_sources import arsenkin

    monkeypatch.setattr(arsenkin.ArsenkinClient, "limits", lambda self: 1234)

    result = providers.provider_verify("arsenkin")

    assert result["ok"] is True
    assert result["state"] == "authenticated"
    assert result["permission_state"] == "authenticated_account"
    assert result["target_access"] == "not_requested"
    assert result["verified"] is False
    assert "synthetic-arsenkin-canary" not in json.dumps(result)


def test_arsenkin_verify_maps_rejected_token_to_invalid(monkeypatch, tmp_path):
    _isolate_credentials(monkeypatch, tmp_path)
    monkeypatch.setenv("ARSENKIN_TOKEN", "synthetic-arsenkin-canary")
    from seohead.data_sources import arsenkin

    def rejected(self):
        raise arsenkin.ArsenkinError("401", "synthetic-arsenkin-canary echoed")

    monkeypatch.setattr(arsenkin.ArsenkinClient, "limits", rejected)

    result = providers.provider_verify("arsenkin")

    assert result["ok"] is False
    assert result["permission_state"] == "invalid"
    assert result["verified"] is False
    assert "synthetic-arsenkin-canary" not in json.dumps(result)


def test_pagespeed_verify_runs_an_explicit_sample_and_reports_elapsed_time(monkeypatch):
    monkeypatch.setattr(
        providers,
        "_credential_details",
        lambda _provider: ({"api_key": True}, {"api_key": {"state": "configured_unverified"}}),
    )
    monkeypatch.setattr(providers, "_readiness_state", lambda *_args: "configured_unverified")
    monkeypatch.setattr(
        providers,
        "provider_collect",
        lambda provider, operation, request, transport=None: {
            "ok": True,
            "provider": provider,
            "operation": operation,
            "samples": [{"url": request["urls"][0]}],
        },
    )

    result = providers.provider_verify("pagespeed", {"urls": ["https://example.test/"]})

    assert result["ok"] is True
    assert result["permission_state"] == "authenticated_account"
    assert result["response_time_ms"] >= 0
    assert result["quota_state"] == "unknown"


def test_yandex_cloud_verify_uses_only_the_unbilled_region_tree(monkeypatch):
    from seohead.data_sources import yandex_cloud

    monkeypatch.setattr(
        providers,
        "_credential_details",
        lambda _provider: (
            {"api_key": True, "folder_id": True},
            {
                "api_key": {"state": "configured_unverified"},
                "folder_id": {"state": "configured_unverified"},
            },
        ),
    )
    monkeypatch.setattr(providers, "_readiness_state", lambda *_args: "configured_unverified")

    class FakeWordstat:
        folder = "folder-test"

    monkeypatch.setattr(yandex_cloud, "Wordstat", FakeWordstat)
    calls = []

    def region_tree(url, payload):
        calls.append((url, payload))
        return 200, {"regions": [{"id": "225", "name": "Russia"}]}

    result = providers.provider_verify("yandex_cloud", transport=region_tree)

    assert result["ok"] is True
    assert result["authenticated_account"] is True
    assert result["verified_capabilities"] == ["wordstat.getRegionsTree"]
    assert "web_search" in result["unverified_capabilities"]
    assert calls == [
        (
            "https://searchapi.api.cloud.yandex.net/v2/wordstat/getRegionsTree",
            {"folderId": "folder-test"},
        )
    ]


def test_yandex_cloud_verification_does_not_confuse_permission_denial_with_network_failure(
    monkeypatch,
):
    from seohead.data_sources import yandex_cloud

    monkeypatch.setattr(
        providers,
        "_credential_details",
        lambda _provider: (
            {"api_key": True, "folder_id": True},
            {
                "api_key": {"state": "configured_unverified"},
                "folder_id": {"state": "configured_unverified"},
            },
        ),
    )
    monkeypatch.setattr(providers, "_readiness_state", lambda *_args: "configured_unverified")

    class FakeWordstat:
        folder = "folder-test"

    monkeypatch.setattr(yandex_cloud, "Wordstat", FakeWordstat)

    denied = providers.provider_verify(
        "yandex_cloud", transport=lambda *_args: (403, {"message": "denied"})
    )
    unavailable = providers.provider_verify(
        "yandex_cloud", transport=lambda *_args: (503, {"message": "maintenance"})
    )

    assert denied["permission_state"] == "not_granted"
    assert unavailable["permission_state"] == "verification_failed"
    assert denied["verified"] is False and unavailable["verified"] is False


def test_dataforseo_verify_uses_free_account_metadata_without_returning_it(monkeypatch):
    from seohead.data_sources.dataforseo import DataForSEOClient

    monkeypatch.setattr(
        providers,
        "_credential_details",
        lambda _provider: (
            {"login": True, "password": True},
            {
                "login": {"state": "configured_unverified"},
                "password": {"state": "configured_unverified"},
            },
        ),
    )
    monkeypatch.setattr(providers, "_readiness_state", lambda *_args: "configured_unverified")
    monkeypatch.setattr(
        DataForSEOClient,
        "balance",
        lambda self: {
            "env": "prod",
            "balance": 123.45,
            "limits": {"secretly detailed": "not for this response"},
            "prices": {"backlinks": "private account metadata"},
        },
    )

    result = providers.provider_verify("dataforseo_backlinks")

    assert result["ok"] is True
    assert result["quota_state"] == "reported"
    assert result["environment"] == "prod"
    assert result["verified_capabilities"] == ["appendix.user_data"]
    assert "backlinks subscription and collection access" in result["unverified_capabilities"]
    assert "123.45" not in json.dumps(result)
    assert "private account metadata" not in json.dumps(result)


def test_dataforseo_account_metadata_rejects_malformed_200_envelopes():
    from seohead.data_sources.dataforseo import DataForSEOError, _account_metadata_result

    valid = {
        "status_code": 20000,
        "tasks_count": 1,
        "tasks_error": 0,
        "tasks": [
            {
                "status_code": 20000,
                "result": [{"rates": {"serp": {}}, "price": {"serp": {}}}],
            }
        ],
    }
    assert _account_metadata_result(valid)["rates"] == {"serp": {}}

    malformed = {**valid, "tasks": [{"status_code": 20000, "result": [{}]}]}
    with pytest.raises(DataForSEOError, match="quota or pricing metadata"):
        _account_metadata_result(malformed)


def test_cloudflare_token_verification_does_not_claim_zone_permissions(monkeypatch):
    from seohead.data_sources import cloudflare

    monkeypatch.setattr(
        providers,
        "_credential_details",
        lambda _provider: (
            {"api_token": True},
            {"api_token": {"state": "configured_unverified"}},
        ),
    )
    monkeypatch.setattr(providers, "_readiness_state", lambda *_args: "configured_unverified")
    monkeypatch.setattr(
        cloudflare,
        "verify_token",
        lambda transport=None: {"ok": True, "token_status": "active", "status": 200},
    )

    result = providers.provider_verify("cloudflare", {"zone_id": "zone-test"})

    assert result["ok"] is True
    assert result["token_status"] == "active"
    assert result["target_access"] == "not_requested"
    assert result["verified"] is False
    assert "zone and account resource access" in result["unverified_capabilities"]


def test_gsc_verify_maps_invalid_grant_to_invalid_permission(monkeypatch):
    from seohead.data_sources import gsc

    monkeypatch.setattr(
        providers,
        "_credential_details",
        lambda _provider: (
            {"oauth_bearer": True},
            {"oauth_bearer": {"state": "configured_unverified"}},
        ),
    )
    monkeypatch.setattr(providers, "_readiness_state", lambda *_args: "configured_unverified")
    monkeypatch.setattr(
        gsc,
        "discover_properties",
        lambda transport=None: {
            "ok": False,
            "state": "not_configured",
            "error": "invalid_grant",
        },
    )

    result = providers.provider_verify("gsc")

    assert result["permission_state"] == "invalid"
    assert result["verified"] is False
