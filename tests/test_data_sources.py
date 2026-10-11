"""Offline tests for external data-source behavior around the API calls."""

from __future__ import annotations

import base64
import json
import os
import urllib.error
import urllib.parse
import urllib.request

import pytest

from seohead.data_sources import (
    arsenkin,
    credentials,
    crtsh,
    crux,
    indexnow,
    spend,
    wayback,
    yandex_cloud,
)
from seohead.data_sources import gsc as gsc_core

# --- Credentials -----------------------------------------------------------


def test_credential_from_env_wins(monkeypatch):
    monkeypatch.setenv("SOME_TOKEN", "  from-environment  ")
    assert credentials.read("missing/path", "SOME_TOKEN") == "from-environment"


def test_credential_missing_names_path_but_not_value(monkeypatch, tmp_path):
    monkeypatch.delenv("SOME_TOKEN", raising=False)
    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    with pytest.raises(credentials.MissingCredential) as exc:
        credentials.read("svc/token", "SOME_TOKEN")
    message = str(exc.value)
    assert "svc/token" in message and "SOME_TOKEN" in message


def test_credential_empty_file_is_missing(monkeypatch, tmp_path):
    monkeypatch.delenv("SOME_TOKEN", raising=False)
    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    (tmp_path / "svc").mkdir()
    (tmp_path / "svc" / "token").write_text("   \n", encoding="utf-8")
    with pytest.raises(credentials.MissingCredential):
        credentials.read("svc/token", "SOME_TOKEN")


def test_available_is_false_without_secret(monkeypatch, tmp_path):
    monkeypatch.delenv("NOPE", raising=False)
    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    assert credentials.available("nope/token", "NOPE") is False


# --- DataForSEO readiness (login AND password, issue #341) -----------------


def _clear_dataforseo_env(monkeypatch):
    monkeypatch.delenv("DATAFORSEO_LOGIN", raising=False)
    monkeypatch.delenv("DATAFORSEO_PASSWORD", raising=False)


def test_dataforseo_ready_requires_both_login_and_password(monkeypatch, tmp_path):
    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    _clear_dataforseo_env(monkeypatch)
    assert credentials.dataforseo_ready() == (False, {"login": False, "password": False})


def test_dataforseo_ready_is_false_with_login_only(monkeypatch, tmp_path):
    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    _clear_dataforseo_env(monkeypatch)
    monkeypatch.setenv("DATAFORSEO_LOGIN", "synthetic-login")
    assert credentials.dataforseo_ready() == (False, {"login": True, "password": False})


def test_dataforseo_ready_is_false_with_password_only(monkeypatch, tmp_path):
    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    _clear_dataforseo_env(monkeypatch)
    monkeypatch.setenv("DATAFORSEO_PASSWORD", "synthetic-password")
    assert credentials.dataforseo_ready() == (False, {"login": False, "password": True})


def test_dataforseo_ready_is_true_with_both_components(monkeypatch, tmp_path):
    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    _clear_dataforseo_env(monkeypatch)
    monkeypatch.setenv("DATAFORSEO_LOGIN", "synthetic-login")
    monkeypatch.setenv("DATAFORSEO_PASSWORD", "synthetic-password")
    assert credentials.dataforseo_ready() == (True, {"login": True, "password": True})


def test_dataforseo_ready_treats_blank_values_as_missing(monkeypatch, tmp_path):
    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    _clear_dataforseo_env(monkeypatch)
    monkeypatch.setenv("DATAFORSEO_LOGIN", "   ")
    monkeypatch.setenv("DATAFORSEO_PASSWORD", "synthetic-password")
    assert credentials.dataforseo_ready() == (False, {"login": False, "password": True})


def test_dataforseo_ready_never_exposes_the_secret_values(monkeypatch, tmp_path):
    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    _clear_dataforseo_env(monkeypatch)
    monkeypatch.setenv("DATAFORSEO_LOGIN", "super-secret-login")
    monkeypatch.setenv("DATAFORSEO_PASSWORD", "super-secret-password")
    ready, components = credentials.dataforseo_ready()
    serialized = json.dumps({"ready": ready, "components": components})
    assert "super-secret-login" not in serialized
    assert "super-secret-password" not in serialized


@pytest.mark.parametrize(
    ("ready", "components"),
    [
        (False, {"login": True, "password": False}),
        (False, {"login": False, "password": True}),
        (True, {"login": True, "password": True}),
    ],
    ids=["login_only", "password_only", "both"],
)
def test_sources_doctor_uses_shared_dataforseo_readiness(monkeypatch, tmp_path, ready, components):
    """The public doctor must use the two-component readiness decision, not login alone."""
    from seohead.mcp import handlers

    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    monkeypatch.setattr(credentials, "available", lambda *_args: False)
    monkeypatch.setattr(credentials, "dataforseo_ready", lambda: (ready, components))

    dataforseo = handlers.sources_doctor()["sources"]["dataforseo"]
    assert dataforseo["ready"] is ready
    assert dataforseo["components"] == components


# --- GSC readiness (bearer OR durable grant OR service account, issue #717) --

# Synthetic shape only: no usable key material, real email, or real token endpoint fields.
_SYNTHETIC_SERVICE_ACCOUNT = {
    "type": "service_account",
    "project_id": "synthetic-project",
    "private_key_id": "synthetic-key-id",
    "private_key": "synthetic-placeholder-not-a-real-key",
    "client_email": "synthetic-service-account@example.invalid",
    "client_id": "synthetic-client-id",
    "token_uri": "https://oauth2.googleapis.com/token",
}


def _clear_gsc_env(monkeypatch):
    monkeypatch.delenv("GSC_ACCESS_TOKEN", raising=False)
    monkeypatch.delenv("GSC_SERVICE_ACCOUNT_FILE", raising=False)


def _gsc_config_dir(tmp_path):
    path = tmp_path / "gsc"
    path.mkdir(exist_ok=True)
    return path


def _write_durable_grant(tmp_path):
    grant = _gsc_config_dir(tmp_path) / "oauth.json"
    grant.write_text(
        json.dumps(
            {
                "refresh_token": "synthetic-refresh-token",
                "client_id": "synthetic-client-id",
                "client_secret": "synthetic-client-secret",
                "scopes": ["https://www.googleapis.com/auth/webmasters.readonly"],
            }
        ),
        encoding="utf-8",
    )
    grant.chmod(0o600)
    return grant


def _write_service_account(tmp_path, raw_text=None):
    account = _gsc_config_dir(tmp_path) / "service-account.json"
    account.write_text(
        raw_text if raw_text is not None else json.dumps(_SYNTHETIC_SERVICE_ACCOUNT),
        encoding="utf-8",
    )
    account.chmod(0o600)
    return account


@pytest.mark.parametrize("component", ["oauth_bearer", "durable_oauth", "service_account"])
def test_sources_doctor_gsc_ready_with_any_working_credential(monkeypatch, tmp_path, component):
    """The legacy ``sources`` block follows provider components, not the bearer file alone."""
    from seohead.data_sources import oauth
    from seohead.mcp import handlers

    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path)
    _clear_gsc_env(monkeypatch)
    _gsc_config_dir(tmp_path)
    if component == "oauth_bearer":
        monkeypatch.setenv("GSC_ACCESS_TOKEN", "synthetic-bearer")
    elif component == "durable_oauth":
        _write_durable_grant(tmp_path)
    else:
        _write_service_account(tmp_path)

    doctor = handlers.sources_doctor()
    gsc = doctor["sources"]["gsc"]
    assert gsc["ready"] is True
    assert gsc["components"] == doctor["provider_status"]["gsc"]["credential_components"]
    assert gsc["components"][component] is True


def test_sources_doctor_gsc_not_ready_without_any_credential(monkeypatch, tmp_path):
    from seohead.data_sources import oauth
    from seohead.mcp import handlers

    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path)
    _clear_gsc_env(monkeypatch)

    doctor = handlers.sources_doctor()
    gsc = doctor["sources"]["gsc"]
    assert gsc["ready"] is False
    assert gsc["components"] == {
        "oauth_bearer": False,
        "service_account": False,
        "durable_oauth": False,
    }
    assert gsc["service_account_status"] == "missing"


@pytest.mark.parametrize(
    ("raw_text", "status"),
    [
        ("{not valid json", "malformed_json"),
        ('"just a string"', "unsupported_shape"),
        ("[]", "unsupported_shape"),
    ],
    ids=["invalid_json", "json_string", "json_array"],
)
def test_sources_doctor_gsc_service_account_malformed_document(
    monkeypatch, tmp_path, raw_text, status
):
    """A broken service-account file is not ready; the doctor reports a safe status enum."""
    from seohead.data_sources import oauth
    from seohead.mcp import handlers

    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path)
    _clear_gsc_env(monkeypatch)
    _write_service_account(tmp_path, raw_text=raw_text)

    doctor = handlers.sources_doctor()
    gsc = doctor["sources"]["gsc"]
    assert gsc["ready"] is False
    assert gsc["components"]["service_account"] is False
    assert gsc["service_account_status"] == status
    assert doctor["provider_status"]["gsc"]["service_account_status"] == status


@pytest.mark.parametrize(
    "document",
    [
        {},
        {**_SYNTHETIC_SERVICE_ACCOUNT, "type": "authorized_user"},
        {**_SYNTHETIC_SERVICE_ACCOUNT, "token_uri": "https://example.invalid/token"},
        {**_SYNTHETIC_SERVICE_ACCOUNT, "private_key": 42},
        {**_SYNTHETIC_SERVICE_ACCOUNT, "client_email": ""},
    ],
    ids=["empty_object", "wrong_type", "wrong_token_uri", "non_string_key", "empty_email"],
)
def test_sources_doctor_gsc_service_account_unsupported_shape(monkeypatch, tmp_path, document):
    from seohead.data_sources import oauth
    from seohead.mcp import handlers

    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path)
    _clear_gsc_env(monkeypatch)
    _write_service_account(tmp_path, raw_text=json.dumps(document))

    doctor = handlers.sources_doctor()
    gsc = doctor["sources"]["gsc"]
    assert gsc["ready"] is False
    assert gsc["components"]["service_account"] is False
    assert gsc["service_account_status"] == "unsupported_shape"


@pytest.mark.parametrize("missing_field", ["client_email", "private_key", "token_uri"])
def test_sources_doctor_gsc_service_account_missing_required_field(
    monkeypatch, tmp_path, missing_field
):
    from seohead.data_sources import oauth
    from seohead.mcp import handlers

    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path)
    _clear_gsc_env(monkeypatch)
    document = {
        key: value for key, value in _SYNTHETIC_SERVICE_ACCOUNT.items() if key != missing_field
    }
    _write_service_account(tmp_path, raw_text=json.dumps(document))

    gsc = handlers.sources_doctor()["sources"]["gsc"]
    assert gsc["ready"] is False
    assert gsc["components"]["service_account"] is False
    assert gsc["service_account_status"] == "unsupported_shape"


def test_sources_doctor_gsc_service_account_over_size_limit(monkeypatch, tmp_path):
    """A file past the bound is never parsed, never "ready" — but it is not malformed JSON."""
    from seohead.data_sources import oauth
    from seohead.mcp import handlers

    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path)
    _clear_gsc_env(monkeypatch)
    document = {**_SYNTHETIC_SERVICE_ACCOUNT, "padding": "x" * 70_000}
    _write_service_account(tmp_path, raw_text=json.dumps(document))

    gsc = handlers.sources_doctor()["sources"]["gsc"]
    assert gsc["ready"] is False
    assert gsc["components"]["service_account"] is False
    assert gsc["service_account_status"] == "too_large"


@pytest.mark.skipif(
    os.name == "nt" or getattr(os, "geteuid", lambda: -1)() == 0,
    reason="POSIX permission bits do not apply on Windows; root bypasses them",
)
def test_sources_doctor_gsc_service_account_owner_unreadable(monkeypatch, tmp_path):
    """A private file the owner cannot read is unreadable, not malformed JSON."""
    from seohead.data_sources import oauth
    from seohead.mcp import handlers

    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path)
    _clear_gsc_env(monkeypatch)
    account = _write_service_account(tmp_path)
    account.chmod(0o200)

    gsc = handlers.sources_doctor()["sources"]["gsc"]
    assert gsc["ready"] is False
    assert gsc["components"]["service_account"] is False
    assert gsc["service_account_status"] == "unreadable"


@pytest.mark.skipif(os.name == "nt", reason="POSIX permission bits do not apply on Windows")
def test_sources_doctor_gsc_service_account_group_readable_is_unsafe(monkeypatch, tmp_path):
    from seohead.data_sources import oauth
    from seohead.mcp import handlers

    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path)
    _clear_gsc_env(monkeypatch)
    account = _write_service_account(tmp_path)
    account.chmod(0o640)

    gsc = handlers.sources_doctor()["sources"]["gsc"]
    assert gsc["ready"] is False
    assert gsc["components"]["service_account"] is False
    assert gsc["service_account_status"] == "unsafe_file"


@pytest.mark.skipif(os.name == "nt", reason="POSIX symlinks are not portable on Windows")
def test_sources_doctor_gsc_service_account_symlink_is_unsafe(monkeypatch, tmp_path):
    from seohead.data_sources import oauth
    from seohead.mcp import handlers

    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path)
    _clear_gsc_env(monkeypatch)
    real = tmp_path / "real-service-account.json"
    real.write_text(json.dumps(_SYNTHETIC_SERVICE_ACCOUNT), encoding="utf-8")
    real.chmod(0o600)
    (_gsc_config_dir(tmp_path) / "service-account.json").symlink_to(real)

    gsc = handlers.sources_doctor()["sources"]["gsc"]
    assert gsc["ready"] is False
    assert gsc["service_account_status"] == "unsafe_file"


def test_sources_doctor_gsc_service_account_status_honors_env_override(monkeypatch, tmp_path):
    from seohead.data_sources import oauth
    from seohead.mcp import handlers

    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path / "other-config")
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path / "other-config")
    _clear_gsc_env(monkeypatch)
    account = _write_service_account(tmp_path)
    monkeypatch.setenv("GSC_SERVICE_ACCOUNT_FILE", str(account))

    gsc = handlers.sources_doctor()["sources"]["gsc"]
    assert gsc["ready"] is True
    assert gsc["components"]["service_account"] is True
    assert gsc["service_account_status"] == "configured_unverified"


def test_sources_doctor_gsc_never_exposes_service_account_contents(monkeypatch, tmp_path):
    from seohead.data_sources import oauth
    from seohead.mcp import handlers

    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path)
    _clear_gsc_env(monkeypatch)
    _write_service_account(tmp_path)

    serialized = json.dumps(handlers.sources_doctor())
    for marker in (
        "synthetic-service-account@example.invalid",
        "synthetic-placeholder-not-a-real-key",
        "synthetic-project",
        "synthetic-key-id",
    ):
        assert marker not in serialized


def test_gsc_service_account_document_returns_document_only_when_valid(monkeypatch, tmp_path):
    from seohead.data_sources import oauth

    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path)
    _clear_gsc_env(monkeypatch)
    account = _write_service_account(tmp_path)

    status, document = credentials.gsc_service_account_document()
    assert status == "configured_unverified"
    assert document["client_email"] == "synthetic-service-account@example.invalid"

    account.write_text("{broken", encoding="utf-8")
    assert credentials.gsc_service_account_document() == ("malformed_json", None)


def test_gsc_bearer_wins_over_service_account_file(monkeypatch, tmp_path):
    """Token precedence stays bearer, then durable grant, then service account."""
    from seohead.data_sources import oauth

    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path)
    _clear_gsc_env(monkeypatch)
    _write_service_account(tmp_path)
    monkeypatch.setenv("GSC_ACCESS_TOKEN", "synthetic-bearer")

    bearer, error = gsc_core._acquire_token(None)
    assert bearer == "synthetic-bearer"
    assert error is None


def test_gsc_durable_grant_wins_over_service_account(monkeypatch, tmp_path):
    from seohead.data_sources import oauth

    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path)
    _clear_gsc_env(monkeypatch)
    _write_durable_grant(tmp_path)
    _write_service_account(tmp_path)
    monkeypatch.setattr(
        gsc_core, "durable_oauth_token", lambda: {"access_token": "refreshed-bearer"}
    )

    bearer, error = gsc_core._acquire_token(None)
    assert bearer == "refreshed-bearer"
    assert error is None


def test_gsc_failed_grant_refresh_returns_before_service_account(monkeypatch, tmp_path):
    """A durable-grant refresh failure must not silently fall back to the service account."""
    from seohead.data_sources import oauth

    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path)
    _clear_gsc_env(monkeypatch)
    _write_durable_grant(tmp_path)
    _write_service_account(tmp_path)

    def fail_refresh():
        raise credentials.MissingCredential("synthetic refresh failure")

    monkeypatch.setattr(gsc_core, "durable_oauth_token", fail_refresh)

    bearer, error = gsc_core._acquire_token(None)
    assert bearer is None
    assert error == "stored OAuth grant refresh failed; reconnect or check the grant"


def test_gsc_malformed_service_account_reports_safe_error(monkeypatch, tmp_path):
    from seohead.data_sources import oauth

    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path)
    _clear_gsc_env(monkeypatch)
    _write_service_account(tmp_path, raw_text="{broken")

    bearer, error = gsc_core._acquire_token(None)
    assert bearer is None
    assert error == "OAuth bearer unavailable; GSC service-account JSON is not valid JSON"
    assert "{broken" not in error


def test_provider_verify_gsc_not_configured_reports_service_account_status(monkeypatch, tmp_path):
    from seohead.data_sources import oauth, providers

    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path)
    _clear_gsc_env(monkeypatch)
    _write_service_account(tmp_path, raw_text="{broken")

    result = providers.provider_verify("gsc")
    assert result["state"] == "not_configured"
    assert result["verified"] is False
    assert result["service_account_status"] == "malformed_json"
    assert "{broken" not in json.dumps(result)


# --- Spend journal ---------------------------------------------------------


@pytest.fixture()
def journal(monkeypatch, tmp_path):
    monkeypatch.setenv("SEOHEAD_SPEND_LOG", str(tmp_path / "spend.jsonl"))
    return tmp_path / "spend.jsonl"


def test_spend_records_and_sums_by_unit(journal):
    spend.record("arsenkin", "keyword_exact", cost=120, unit="limits", task_id=555, items=40)
    spend.record("arsenkin", "keyword_exact", cost=30, unit="limits", task_id=556, items=10)
    spend.record("yandex_cloud", "wordstat.topRequests", cost=1, unit="requests", items=1)

    report = spend.report()
    assert report["calls"] == 3
    assert report["by_source"]["arsenkin"]["limits"] == 150.0
    assert report["by_source"]["yandex_cloud"]["requests"] == 1.0
    assert report["by_operation"]["arsenkin.keyword_exact"]["limits"] == 150.0


def test_spend_keeps_task_ids_so_paid_results_can_be_refetched(journal):
    spend.record("arsenkin", "top", cost=10, task_id=1)
    spend.record("arsenkin", "top", cost=10, task_id=2)
    spend.record("yandex_cloud", "serp", cost=1)  # No task ID is available.
    assert spend.paid_task_ids("arsenkin") == [1, 2]
    assert spend.paid_task_ids("yandex_cloud") == []


def test_spend_survives_broken_line(journal):
    spend.record("arsenkin", "top", cost=5)
    with journal.open("a", encoding="utf-8") as handle:
        handle.write("this is not JSON\n")
    spend.record("arsenkin", "top", cost=5)
    assert spend.report()["calls"] == 2  # The malformed line is skipped without breaking the log.


def test_spend_report_since_filters_by_day(journal, monkeypatch):
    spend.record("arsenkin", "top", cost=5)
    rows = [json.loads(line) for line in journal.read_text(encoding="utf-8").splitlines()]
    old = dict(rows[0], at="2020-01-01T00:00:00")
    with journal.open("w", encoding="utf-8") as handle:
        handle.write(json.dumps(old, ensure_ascii=False) + "\n")
    assert spend.report()["calls"] == 1
    assert spend.report(since="2026-01-01")["calls"] == 0


def test_spend_report_on_missing_log_is_empty(monkeypatch, tmp_path):
    monkeypatch.setenv("SEOHEAD_SPEND_LOG", str(tmp_path / "missing.jsonl"))
    report = spend.report()
    assert report["calls"] == 0 and report["by_source"] == {}


def test_spend_csv_keeps_every_row_and_flags_uncertain_ones(journal, tmp_path):
    spend.record("arsenkin", "top", cost=5, task_id=7)
    spend.record("yandex_cloud", "serp", cost=1, extra={"charge_uncertain": True})
    out = tmp_path / "spend.csv"
    assert spend.write_csv(out) == 2
    lines = out.read_text(encoding="utf-8-sig").splitlines()
    assert lines[0] == "at;source;operation;cost;unit;items;task_id;uncertain"
    assert lines[1].split(";")[1:3] == ["arsenkin", "top"]
    assert lines[1].endswith(";7;False")
    assert lines[2].endswith(";;True")


def test_spend_csv_since_filters_rows(journal, tmp_path):
    spend.record("arsenkin", "top", cost=5)
    rows = [json.loads(line) for line in journal.read_text(encoding="utf-8").splitlines()]
    with journal.open("w", encoding="utf-8") as handle:
        handle.write(json.dumps(dict(rows[0], at="2020-01-01T00:00:00")) + "\n")
    assert spend.write_csv(tmp_path / "old.csv", since="2026-01-01") == 0


# --- Arsenkin rate limiting and usage accounting --------------------------


def test_rate_limiter_keeps_headroom_under_the_wall():
    limiter = arsenkin.RateLimiter(max_calls=30, period=60.0, safety=3)
    assert limiter.max_calls == 27  # Headroom prevents bursts of HTTP 429 responses.


def test_rate_limiter_never_drops_below_one():
    assert arsenkin.RateLimiter(max_calls=2, safety=10).max_calls == 1


@pytest.mark.parametrize(
    "data,expected",
    [
        ({"keywords": ["alpha", "beta", "gamma"]}, 3),
        ({"words": "alpha\nbeta\n\ngamma\n"}, 3),
        ({"urls": []}, 0),
        ({"unrelated": "value"}, 0),
    ],
)
def test_count_items_for_journal(data, expected):
    assert arsenkin._count_items(data) == expected


def test_refetch_is_get_so_paid_result_is_not_bought_twice():
    assert arsenkin.ArsenkinClient.refetch is arsenkin.ArsenkinClient.get


# --- Yandex Cloud normalization and SERP parsing --------------------------


# Cyrillic fixtures intentionally verify Russian case folding and yo-character normalization.
@pytest.mark.parametrize(
    "raw,expected",
    [
        ("  Тёплый   ПОЛ ", "теплый пол"),
        ("ЁЛКА", "елка"),
        ("", ""),
        (None, ""),
    ],
)
def test_normalize(raw, expected):
    assert yandex_cloud.normalize(raw) == expected


def test_parse_serp_extracts_position_url_domain_title():
    xml = (
        "<doc><url>https://www.example.com/a</url><title>First result</title></doc>"
        "<doc><url>https://search.example/x</url><domain>search.example</domain>"
        "<title>Second result</title></doc>"
    )
    docs = yandex_cloud.parse_serp(xml)
    assert [d["pos"] for d in docs] == [1, 2]
    assert docs[0]["domain"] == "example.com"  # The ``www`` prefix is removed.
    assert docs[0]["title"] == "First result"
    assert docs[1]["domain"] == "search.example"


def test_parse_serp_strips_highlight_tags_inside_title():
    xml = (
        "<doc><url>https://search.example/</url><title>buy<hlword>ing</hlword> a pump</title></doc>"
    )
    assert yandex_cloud.parse_serp(xml)[0]["title"] == "buying a pump"


def test_parse_serp_on_empty_input_is_empty_not_error():
    assert yandex_cloud.parse_serp("") == []


def test_serp_body_never_asks_for_sync_search():
    """Synchronous search is deliberately absent because it costs 16 times more."""
    # The Cyrillic query intentionally exercises the Russian Yandex search type.
    body = yandex_cloud._serp_body(
        "тест", "225", "SEARCH_TYPE_RU", 10, 1, "FAMILY_MODE_NONE", "folder-1"
    )
    assert body["responseFormat"] == "FORMAT_XML"
    assert body["query"]["queryText"] == "тест"
    assert not hasattr(yandex_cloud.WebSearch, "search_sync")


# --- Regions ---------------------------------------------------------------

# Cyrillic region names intentionally verify Yandex's Russian aliases and canonical names.


def test_region_lookup_understands_both_official_and_api_names():
    """The official and API-specific names resolve to the same federal district."""
    from seohead.data_sources import yandex_regions as regions

    assert regions.by_name("Поволжье") == "40"
    assert regions.by_name("Приволжский") == "40"
    assert regions.by_name("Дальневосточный") == regions.by_name("Дальний Восток") == "73"


def test_region_lookup_returns_none_for_unknown():
    from seohead.data_sources import yandex_regions as regions

    assert regions.by_name("Atlantis") is None


def test_vladivostok_city_is_not_the_district():
    """Code 75 is the city and 73 the district; mixing them distorts demand data."""
    from seohead.data_sources import yandex_regions as regions

    assert regions.CITIES["Владивосток"] == "75"
    assert regions.DISTRICTS["Дальний Восток"] == "73"


def test_every_district_alias_points_at_a_real_district():
    from seohead.data_sources import yandex_regions as regions

    assert all(target in regions.DISTRICTS for target in regions.DISTRICT_ALIASES.values())


# --- Yandex Metrica --------------------------------------------------------


def test_metrika_backoff_respects_retry_after_header():
    """A valid Retry-After value takes precedence over the local backoff formula."""
    from seohead.data_sources.metrika import MetrikaClient

    assert MetrikaClient._backoff(1, "5") == 5.0
    assert MetrikaClient._backoff(1, "600") == 60.0  # Never wait longer than one minute.
    assert MetrikaClient._backoff(3, None) == 4.0  # Fall back to exponential backoff.
    assert MetrikaClient._backoff(1, "not-a-number") == 1.0  # Ignore invalid headers.


def test_metrika_backoff_is_capped():
    from seohead.data_sources.metrika import MAX_BACKOFF, MetrikaClient

    assert MetrikaClient._backoff(20, None) == MAX_BACKOFF


@pytest.mark.parametrize(
    "payload,expected",
    [
        ('{"message": "Counter not found"}', "Counter not found"),
        ('{"errors": [{"message": "Invalid metric"}]}', "Invalid metric"),
        ('{"errors": ["Flat error string"]}', "Flat error string"),
        ("not JSON at all", "not JSON at all"),
        ("", "empty response"),
    ],
)
def test_metrika_error_message_is_extracted_from_api_answer(payload, expected):
    from seohead.data_sources.metrika import _api_message

    assert _api_message(payload) == expected


def test_metrika_error_carries_status():
    from seohead.data_sources.metrika import MetrikaError

    exc = MetrikaError(429, "Too many requests")
    assert exc.status == 429 and "429" in str(exc)


def test_metrika_url_drops_empty_params_but_keeps_zero():
    from seohead.data_sources.metrika import MetrikaClient

    # ``attempt`` is a synthetic stand-in: report ``offset`` is 1-based and ``0`` must never
    # reach ``stat/v1/data`` (#707), so this helper check uses a different zero-valued key.
    url = MetrikaClient._url(
        "stat/v1/data", {"limit": 100, "attempt": 0, "filters": "", "preset": None}
    )
    assert "limit=100" in url and "attempt=0" in url
    assert "filters" not in url and "preset" not in url


def test_metrika_reports_start_at_offset_one(monkeypatch):
    """The Reporting API is 1-based: ``offset=0`` is answered with 400 (#707)."""
    from seohead.data_sources.metrika import MetrikaClient

    urls: list[str] = []
    client = MetrikaClient.__new__(MetrikaClient)
    monkeypatch.setattr(client, "_request", lambda url, *a, **k: urls.append(url) or {"data": []})
    client.report({"ids": 1})
    client.by_time({"ids": 1})
    client.report({"ids": 1}, paginate=True)
    assert urls and all("offset=1" in url for url in urls)


def test_metrika_pagination_advances_one_based_cursor(monkeypatch):
    from seohead.data_sources import metrika

    monkeypatch.setattr(metrika, "PAGE_PAUSE", 0)
    offsets: list[int] = []

    def fake(url, *a, **k):
        offset = int(url.split("offset=")[1].split("&")[0])
        offsets.append(offset)
        rows = [{"dimensions": [], "metrics": [1]}] * (100 if offset == 1 else 5)
        return {"data": rows, "total_rows": 105}

    client = metrika.MetrikaClient.__new__(metrika.MetrikaClient)
    monkeypatch.setattr(client, "_request", fake)
    result = client.report({"ids": 1}, paginate=True)
    assert offsets == [1, 101] and len(result["data"]) == 105


def test_metrika_rejects_zero_based_offset_before_any_request(monkeypatch):
    """``offset`` below 1 fails locally instead of being forwarded to a 400 (#707)."""
    from seohead.data_sources.metrika import MetrikaClient

    client = MetrikaClient(token="synthetic")
    calls: list[str] = []
    monkeypatch.setattr(client, "_request", lambda url, *a, **k: calls.append(url))

    for kwargs in ({"offset": 0}, {"offset": -3}):
        with pytest.raises(ValueError, match="offset"):
            client.report({"ids": 1}, **kwargs)
        with pytest.raises(ValueError, match="offset"):
            client.report({"ids": 1}, paginate=True, **kwargs)
        with pytest.raises(ValueError, match="offset"):
            client.by_time({"ids": 1}, **kwargs)
    assert calls == []


def test_metrika_public_report_paths_never_send_a_zero_based_offset(monkeypatch, journal):
    """Every public report entry point defaults to the 1-based offset the API requires (#707).

    The double mimics the live contract—``offset < 1`` earns a 400—so on the broken revision
    each of these calls failed with ``must be greater than or equal to 1``.
    """
    from seohead.data_sources import metrika, providers
    from seohead.mcp import handlers

    monkeypatch.setenv("YANDEX_METRIKA_TOKEN", "synthetic")
    monkeypatch.setattr(metrika, "PAGE_PAUSE", 0)
    urls: list[str] = []

    def fake_request(self, url, *a, **k):
        urls.append(url)
        query = urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)
        if int(query["offset"][0]) < 1:
            raise metrika.MetrikaError(400, "must be greater than or equal to 1")
        return {
            "data": [{"dimensions": [{"name": "/"}], "metrics": [7]}],
            "total_rows": 1,
            "query": {"metrics": ["ym:s:visits"], "dimensions": ["ym:s:startURL"]},
        }

    monkeypatch.setattr(metrika.MetrikaClient, "_request", fake_request)

    client = metrika.MetrikaClient()
    assert client.report({"ids": 1, "metrics": "ym:s:visits"})["data"]
    assert client.report({"ids": 1, "metrics": "ym:s:visits"}, paginate=True)["data"]

    handled = handlers.metrika_report("1", "ym:s:visits", date1="2026-09-01", date2="2026-09-16")
    assert handled["ok"] is True

    collected = providers.provider_collect(
        "metrika",
        "aggregate_report",
        {
            "counter_id": "1",
            "metrics": "ym:s:visits",
            "date1": "2026-09-01",
            "date2": "2026-09-16",
        },
    )
    assert collected["evidence"]["status"] == "complete"
    assert urls and all("offset=" in url for url in urls)


def _metrika_paged_rows(url, total):
    """Serve ``min(limit, remaining)`` numbered rows for a 1-based ``offset`` URL."""
    query = urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)
    offset, limit = int(query["offset"][0]), int(query["limit"][0])
    rows = [{"metrics": [offset + i]} for i in range(min(limit, total - (offset - 1)))]
    return {"data": rows, "total_rows": total, "query": {}}


def test_metrika_pagination_covers_two_and_a_half_pages(monkeypatch, journal):
    """2.5 pages request offsets ``1, 1+size, 1+2*size`` and return every row once (#707)."""
    from seohead.data_sources import metrika

    monkeypatch.setattr(metrika, "PAGE_PAUSE", 0)
    requests: list[int] = []

    def fake(url, *a, **k):
        requests.append(int(urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)["offset"][0]))
        return _metrika_paged_rows(url, 250)

    client = metrika.MetrikaClient.__new__(metrika.MetrikaClient)
    monkeypatch.setattr(client, "_request", fake)
    result = client.report({"ids": 1}, paginate=True, limit=100)
    assert requests == [1, 101, 201]
    assert [row["metrics"][0] for row in result["data"]] == list(range(1, 251))


def test_metrika_pagination_total_rows_stop_counts_rows_before_offset(monkeypatch, journal):
    """``total_rows`` is absolute: with ``offset=51`` a 151-row report takes two pages (#707).

    Counting ``offset + len(rows)`` instead of ``offset - 1 + len(rows)`` would stop after the
    first page and silently drop the last row.
    """
    from seohead.data_sources import metrika

    monkeypatch.setattr(metrika, "PAGE_PAUSE", 0)
    requests: list[int] = []

    def fake(url, *a, **k):
        requests.append(int(urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)["offset"][0]))
        return _metrika_paged_rows(url, 151)

    client = metrika.MetrikaClient.__new__(metrika.MetrikaClient)
    monkeypatch.setattr(client, "_request", fake)
    result = client.report({"ids": 1}, paginate=True, limit=100, offset=51)
    assert requests == [51, 151]
    assert len(result["data"]) == 101


def test_metrika_pagination_row_cap_stop_counts_rows_before_offset(monkeypatch, journal):
    """The ``ROW_CAP`` stop also counts the rows skipped before ``offset`` (#707)."""
    from seohead.data_sources import metrika

    monkeypatch.setattr(metrika, "PAGE_PAUSE", 0)
    monkeypatch.setattr(metrika, "ROW_CAP", 151)
    requests: list[int] = []

    def fake(url, *a, **k):
        requests.append(int(urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)["offset"][0]))
        return _metrika_paged_rows(url, 10_000)

    client = metrika.MetrikaClient.__new__(metrika.MetrikaClient)
    monkeypatch.setattr(client, "_request", fake)
    result = client.report({"ids": 1}, paginate=True, limit=100, offset=51)
    # 150 absolute rows sit below the cap, so page two must still be fetched — but
    # it may ask only for the single row the budget still allows, not a full page.
    assert requests == [51, 151]
    assert len(result["data"]) == 101 and result["capped"] is True


def test_metrika_rows_to_records_pairs_dimensions_with_metrics():
    """Pair Metrica's parallel dimension and metric arrays without shifting columns."""
    from seohead.data_sources.metrika import rows_to_records

    report = {
        "query": {"dimensions": ["ym:s:startURL"], "metrics": ["ym:s:visits", "ym:s:users"]},
        "data": [
            {"dimensions": [{"name": "/blog"}], "metrics": [120, 90]},
            {"dimensions": [{"name": "/about"}], "metrics": [10, 8]},
        ],
    }
    assert rows_to_records(report) == [
        {"startURL": "/blog", "visits": 120, "users": 90},
        {"startURL": "/about", "visits": 10, "users": 8},
    ]


def test_metrika_rows_to_records_survives_missing_query_and_extra_columns():
    from seohead.data_sources.metrika import rows_to_records

    report = {"data": [{"dimensions": ["plain string"], "metrics": [1]}]}
    assert rows_to_records(report) == [{"dimension_0": "plain string", "metric_0": 1}]
    assert rows_to_records({}) == []


def test_metrika_row_cap_exists_so_a_typo_cannot_pull_a_million_rows():
    from seohead.data_sources import metrika

    assert metrika.ROW_CAP == 100_000
    assert metrika.PAGE_PAUSE > 0  # Paging without a pause can exhaust the request quota.


def test_metrika_paginated_failure_keeps_the_usage_already_made(monkeypatch, journal):
    """A page-two failure must not erase the request that page one already spent.

    Before the fix, the aggregate ``report.paginated`` spend row was written only after the
    whole loop finished, so an exception on a later page left the journal with no entry at
    all — hiding both the successful first page and the failing second attempt from anyone
    diagnosing an interrupted collection.
    """
    from seohead.data_sources import metrika

    client = metrika.MetrikaClient(token="synthetic")
    calls = {"count": 0}

    def fake_request(_url):
        calls["count"] += 1
        if calls["count"] == 1:
            return {"data": [{"metrics": [1]}] * 100, "total_rows": 300, "query": {}}
        raise metrika.MetrikaError(503, "synthetic outage")

    monkeypatch.setattr(client, "_request", fake_request)
    monkeypatch.setattr(metrika.time, "sleep", lambda _seconds: None)

    with pytest.raises(metrika.MetrikaError) as exc:
        client.report({"metrics": "ym:s:visits"}, paginate=True, limit=100)

    assert exc.value.status == 503
    assert calls["count"] == 2  # Both the successful page and the failing attempt ran.

    rows = spend.read_all()
    assert len(rows) == 1  # The interrupted collection still leaves usage in the journal.
    assert rows[0]["cost"] == 2  # One completed page plus the one that raised.
    assert rows[0]["items"] == 100  # Rows collected before the failure are not discarded.
    assert rows[0]["extra"]["outcome"] == "failed"


def test_metrika_total_failure_does_not_journal_a_fabricated_success(monkeypatch, journal):
    """A request that raises before returning anything must not log items:1 as if it succeeded.

    Before the fix, ``report(paginate=False)`` (and the other non-paginated methods) called
    ``spend.record`` before ``_request`` ran, so a totally failed call left the same shaped
    journal entry as a genuine one-item success. The refused request still consumed provider
    quota, so it does appear — as an explicit failure, the convention paginated failures
    already use — never as a measured success.
    """
    from seohead.data_sources import metrika

    client = metrika.MetrikaClient(token="synthetic")

    def fake_request(_url):
        raise metrika.MetrikaError(403, "invalid oauth_token")

    monkeypatch.setattr(client, "_request", fake_request)

    with pytest.raises(metrika.MetrikaError):
        client.report({"metrics": "ym:pv:pageviews"}, paginate=False)

    rows = spend.read_all()
    assert len(rows) == 1
    assert rows[0]["extra"]["outcome"] == "failed" and rows[0]["items"] == 0


def test_metrika_report_logs_actual_row_count_not_a_hardcoded_one(monkeypatch, journal):
    """A non-paginated report returning 500 rows must be journalled as items:500, not items:1."""
    from seohead.data_sources import metrika

    client = metrika.MetrikaClient(token="synthetic")
    monkeypatch.setattr(
        client,
        "_request",
        lambda _url: {"data": [{"metrics": [1]}] * 500, "query": {}},
    )

    result = client.report({"metrics": "ym:pv:pageviews"}, paginate=False, limit=500)

    assert len(result["data"]) == 500
    rows = spend.read_all()
    assert len(rows) == 1
    assert rows[0]["items"] == 500  # Negative control against the old hardcoded 1.


def test_metrika_counters_logs_zero_items_for_an_empty_list(monkeypatch, journal):
    """An empty counters list must be journalled as items:0, the genuine measured count."""
    from seohead.data_sources import metrika

    client = metrika.MetrikaClient(token="synthetic")
    monkeypatch.setattr(client, "_request", lambda _url: {"counters": []})

    assert client.counters() == []
    rows = spend.read_all()
    assert len(rows) == 1
    assert rows[0]["items"] == 0


def _metrika_query(url: str) -> dict:
    """Query parameters of a fake-captured Reporting API URL."""
    return dict(urllib.parse.parse_qsl(urllib.parse.urlsplit(url).query))


def test_metrika_too_complicated_period_is_retried_in_month_slices(monkeypatch, journal):
    """A refused long range is collected month by month and merged (#714)."""
    from datetime import date

    from seohead.data_sources import metrika

    client = metrika.MetrikaClient(token="synthetic")
    urls: list[str] = []

    def fake(url):
        urls.append(url)
        query = _metrika_query(url)
        days = (date.fromisoformat(query["date2"]) - date.fromisoformat(query["date1"])).days
        if days > 31:
            raise metrika.MetrikaError(400, "Query is too complicated")
        return {
            "data": [
                {"dimensions": [{"name": "/a"}], "metrics": [1, 10]},
                {"dimensions": [{"name": f"/{query['date1'][:7]}"}], "metrics": [2, 20]},
            ],
            "totals": [3, 30],
            "total_rows": 2,
            "sampled": False,
            "sample_share": 1.0,
            "query": {},
        }

    monkeypatch.setattr(client, "_request", fake)
    monkeypatch.setattr(metrika.time, "sleep", lambda _seconds: None)

    result = client.report(
        {
            "metrics": "ym:s:visits,ym:s:pageviews",
            "date1": "2026-01-15",
            "date2": "2026-03-10",
        }
    )

    spans = [(_metrika_query(u)["date1"], _metrika_query(u)["date2"]) for u in urls]
    assert spans == [
        ("2026-01-15", "2026-03-10"),  # refused as a whole
        ("2026-01-15", "2026-01-31"),
        ("2026-02-01", "2026-02-28"),
        ("2026-03-01", "2026-03-10"),
    ]
    by_name = {row["dimensions"][0]["name"]: row["metrics"] for row in result["data"]}
    assert by_name["/a"] == [3, 30]  # the same dimension value sums across the slices
    assert len(result["data"]) == 4 and result["total_rows"] == 4
    assert result["totals"] == [9, 90]
    assert result["accuracy_used"] == "full" and result["sampled"] is False
    assert len(result["split"]["periods"]) == 3
    query = result["query"]
    assert query["date1"] == "2026-01-15" and query["date2"] == "2026-03-10"


def test_metrika_too_complicated_slice_degrades_to_sampled_accuracy(monkeypatch, journal):
    """A slice that still refuses at ``full`` is resubmitted sampled, and the body says so."""
    from seohead.data_sources import metrika

    client = metrika.MetrikaClient(token="synthetic")
    calls: list[str] = []

    def fake(url):
        calls.append(url)
        if "accuracy=0.1" not in url:
            raise metrika.MetrikaError(400, "Query is too complicated")
        return {
            "data": [{"dimensions": [{"name": "/a"}], "metrics": [7]}],
            "totals": [7],
            "sampled": True,
            "sample_share": 0.1,
            "query": {},
        }

    monkeypatch.setattr(client, "_request", fake)
    monkeypatch.setattr(metrika.time, "sleep", lambda _seconds: None)

    result = client.report({"metrics": "ym:s:visits", "date1": "2026-01-01", "date2": "2026-03-31"})

    # The refused whole-range request, then per month: three refused full-accuracy
    # attempts and one sampled success.
    assert len(calls) == 1 + 3 * (metrika.COMPLEXITY_ATTEMPTS + 1)
    assert result["accuracy_used"] == 0.1 and result["sampled"] is True
    assert result["sample_share"] == 0.1
    assert all(p["accuracy"] == 0.1 for p in result["split"]["periods"])
    assert result["data"] == [{"dimensions": [{"name": "/a"}], "metrics": [21]}]
    assert result["totals"] == [21]


def test_metrika_too_complicated_period_that_cannot_be_split_reraises(monkeypatch, journal):
    """Shorthand dates such as ``month`` cannot be sliced; retries and sampling still run."""
    from seohead.data_sources import metrika

    client = metrika.MetrikaClient(token="synthetic")
    calls: list[str] = []

    def fake(url):
        calls.append(url)
        raise metrika.MetrikaError(400, "Query is too complicated")

    monkeypatch.setattr(client, "_request", fake)
    monkeypatch.setattr(metrika.time, "sleep", lambda _seconds: None)

    with pytest.raises(metrika.MetrikaError, match="too complicated"):
        client.report({"metrics": "ym:s:visits", "date1": "month", "date2": "today"})

    # The original attempt plus COMPLEXITY_ATTEMPTS at each of the two accuracy levels.
    assert len(calls) == 1 + 2 * metrika.COMPLEXITY_ATTEMPTS
    assert any("accuracy=0.1" in url for url in calls)


def test_metrika_non_complexity_error_is_not_split(monkeypatch, journal):
    """A genuine bad-query 400 must fail fast instead of being retried month by month."""
    from seohead.data_sources import metrika

    client = metrika.MetrikaClient(token="synthetic")
    calls = {"count": 0}

    def fake(_url):
        calls["count"] += 1
        raise metrika.MetrikaError(400, "dimension not found")

    monkeypatch.setattr(client, "_request", fake)
    with pytest.raises(metrika.MetrikaError, match="dimension not found"):
        client.report({"metrics": "ym:s:visits", "date1": "2026-01-01", "date2": "2026-03-31"})
    assert calls["count"] == 1


def test_metrika_split_reapplies_sort_and_limit_to_merged_rows(monkeypatch, journal):
    """A merged body must be ordered and windowed like one unsplit API answer."""
    from seohead.data_sources import metrika

    client = metrika.MetrikaClient(token="synthetic")

    def fake(url):
        query = _metrika_query(url)
        if query["date1"] == "2026-01-01" and query["date2"] == "2026-02-28":
            raise metrika.MetrikaError(400, "Query is too complicated")
        return {
            "data": [
                {"dimensions": [{"name": "/a"}], "metrics": [5]},
                {"dimensions": [{"name": "/b"}], "metrics": [40]},
                {"dimensions": [{"name": "/c"}], "metrics": [10]},
            ],
            "totals": [55],
            "total_rows": 3,
            "query": {},
        }

    monkeypatch.setattr(client, "_request", fake)
    monkeypatch.setattr(metrika.time, "sleep", lambda _seconds: None)

    result = client.report(
        {
            "metrics": "ym:s:visits",
            "date1": "2026-01-01",
            "date2": "2026-02-28",
            "sort": "-ym:s:visits",
        },
        limit=2,
    )
    assert [row["dimensions"][0]["name"] for row in result["data"]] == ["/b", "/c"]
    assert result["total_rows"] == 3  # the window does not shrink the merged set


def test_metrika_report_handler_reports_the_sampling_used(monkeypatch):
    from seohead.data_sources import metrika
    from seohead.mcp import handlers

    class _Client:
        def report(self, params, **_kwargs):
            return {
                "data": [],
                "totals": [0],
                "total_rows": 0,
                "sampled": True,
                "accuracy_used": 0.1,
                "split": {"reason": "Query is too complicated", "periods": []},
                "query": {"metrics": params["metrics"].split(","), "dimensions": []},
            }

    monkeypatch.setattr(metrika, "MetrikaClient", lambda *a, **k: _Client())
    out = handlers.metrika_report(counter_id="1", metrics="ym:s:visits")
    assert out["ok"] is True
    assert out["sampled"] is True and out["accuracy"] == 0.1
    assert out["split"]["reason"] == "Query is too complicated"


def _metrika_split_client(monkeypatch, fake):
    """A client whose transport is the given fake; sleeps are neutralized."""
    from seohead.data_sources import metrika

    client = metrika.MetrikaClient(token="synthetic")
    monkeypatch.setattr(client, "_request", fake)
    monkeypatch.setattr(metrika.time, "sleep", lambda _seconds: None)
    return client


def _slice_body(rows, totals=None, **fields):
    """A minimal synthetic Reporting API body for one month slice."""
    body = {
        "data": rows,
        "totals": totals if totals is not None else [0] * len(rows[0]["metrics"]),
        "total_rows": len(rows),
        "sampled": False,
        "sample_share": 1.0,
        "query": {},
    }
    body.update(fields)
    return body


@pytest.mark.parametrize(
    "metric",
    [
        "ym:s:users",  # distinct visitors overlap across months
        "ym:s:crossDeviceUsers",
        "ym:s:bounceRate",  # a percentage, not a count
        "ym:s:pageDepth",  # an average
        "ym:s:avgVisitDurationSeconds",
        "ym:s:goal12345visits",  # parameterized ids are unverifiable
    ],
)
def test_metrika_split_refuses_nonadditive_metrics(monkeypatch, journal, metric):
    """A unique-visitor, ratio, or average metric can never be summed over slices.

    The refusal happens before the first slice request: only the refused whole-range
    call may be spent on a query whose merged numbers would be wrong (#714).
    """
    from seohead.data_sources import metrika

    client = metrika.MetrikaClient(token="synthetic")
    calls: list[str] = []

    def fake(url):
        calls.append(url)
        raise metrika.MetrikaError(400, "Query is too complicated")

    monkeypatch.setattr(client, "_request", fake)
    monkeypatch.setattr(metrika.time, "sleep", lambda _seconds: None)

    with pytest.raises(metrika.MetrikaError) as exc:
        client.report(
            {
                "metrics": f"ym:s:visits,{metric}",
                "date1": "2026-01-01",
                "date2": "2026-03-31",
            }
        )

    message = str(exc.value)
    assert metric in message and "2026-01-01..2026-03-31" in message
    assert "too complicated" in message  # the split cause stays visible
    assert len(calls) == 1  # the policy applies before the first slice request


def test_metrika_split_month_slices_cover_each_day_once(monkeypatch, journal):
    """Slices align to calendar months, including leap February and a year boundary."""
    from seohead.data_sources import metrika

    client = metrika.MetrikaClient(token="synthetic")
    urls: list[str] = []

    def fake(url):
        urls.append(url)
        query = _metrika_query(url)
        # The fake refuses any range that crosses a month boundary.
        if query["date1"][:7] != query["date2"][:7]:
            raise metrika.MetrikaError(400, "Query is too complicated")
        return _slice_body([{"dimensions": [{"name": "/a"}], "metrics": [1]}], totals=[1])

    client = _metrika_split_client(monkeypatch, fake)
    result = client.report({"metrics": "ym:s:visits", "date1": "2024-01-31", "date2": "2024-03-01"})

    spans = [(_metrika_query(u)["date1"], _metrika_query(u)["date2"]) for u in urls]
    assert spans == [
        ("2024-01-31", "2024-03-01"),
        ("2024-01-31", "2024-01-31"),
        ("2024-02-01", "2024-02-29"),  # 2024 is a leap year
        ("2024-03-01", "2024-03-01"),
    ]
    covered = [(period["date1"], period["date2"]) for period in result["split"]["periods"]]
    assert covered == spans[1:]
    assert result["query"]["date1"] == "2024-01-31"
    assert result["query"]["date2"] == "2024-03-01"

    urls.clear()
    result = client.report({"metrics": "ym:s:visits", "date1": "2023-12-31", "date2": "2024-01-01"})
    assert [(period["date1"], period["date2"]) for period in result["split"]["periods"]] == [
        ("2023-12-31", "2023-12-31"),
        ("2024-01-01", "2024-01-01"),
    ]


def test_metrika_split_merges_additive_rows_and_totals(monkeypatch, journal):
    """The canonical additive case: shared keys sum, disjoint keys union, totals sum."""
    from seohead.data_sources import metrika

    bodies = {
        "2026-01": _slice_body(
            [
                {"dimensions": [{"name": "/a"}], "metrics": [10, 30]},
                {"dimensions": [{"name": "/b"}], "metrics": [1, 2]},
            ],
            totals=[11, 32],
        ),
        "2026-02": _slice_body(
            [
                {"dimensions": [{"name": "/a"}], "metrics": [5, 10]},
                {"dimensions": [{"name": "/c"}], "metrics": [20, 40]},
            ],
            totals=[25, 50],
        ),
    }

    def fake(url):
        query = _metrika_query(url)
        if query["date1"] == "2026-01-01" and query["date2"] == "2026-02-28":
            raise metrika.MetrikaError(400, "Query is too complicated")
        return bodies[query["date1"][:7]]

    client = _metrika_split_client(monkeypatch, fake)
    result = client.report(
        {
            "metrics": "ym:s:visits,ym:s:pageviews",
            "date1": "2026-01-01",
            "date2": "2026-02-28",
            "sort": "-ym:s:visits",
        }
    )

    by_name = {row["dimensions"][0]["name"]: row["metrics"] for row in result["data"]}
    assert by_name == {"/a": [15, 40], "/b": [1, 2], "/c": [20, 40]}
    assert result["totals"] == [36, 82] and result["total_rows"] == 3
    assert result["capped"] is False and result["incomplete"] is False
    assert result["min"] == [1, 2] and result["max"] == [20, 40]


def test_metrika_split_without_dimensions_sums_the_single_aggregate_row(monkeypatch, journal):
    """A dimensionless report is one aggregate row; the merge still sums it correctly."""
    from seohead.data_sources import metrika

    def fake(url):
        query = _metrika_query(url)
        if query["date1"][:7] != query["date2"][:7]:
            raise metrika.MetrikaError(400, "Query is too complicated")
        value = 10 if query["date1"][:7] == "2026-01" else 20
        return _slice_body([{"dimensions": [], "metrics": [value]}], totals=[value])

    client = _metrika_split_client(monkeypatch, fake)
    result = client.report({"metrics": "ym:s:visits", "date1": "2026-01-01", "date2": "2026-02-28"})
    assert result["data"] == [{"dimensions": [], "metrics": [30]}]
    assert result["totals"] == [30] and result["total_rows"] == 1


def test_metrika_split_missing_metric_cell_is_unavailable_not_zero(monkeypatch, journal):
    """A suppressed cell cannot be summed: it stays ``None`` and marks the body incomplete."""
    from seohead.data_sources import metrika

    bodies = {
        "2026-01": _slice_body(
            [{"dimensions": [{"name": "/a"}], "metrics": [10, None]}], totals=[10, None]
        ),
        "2026-02": _slice_body(
            [{"dimensions": [{"name": "/a"}], "metrics": [5, 10]}], totals=[5, 10]
        ),
    }

    def fake(url):
        query = _metrika_query(url)
        if query["date1"][:7] != query["date2"][:7]:
            raise metrika.MetrikaError(400, "Query is too complicated")
        return bodies[query["date1"][:7]]

    client = _metrika_split_client(monkeypatch, fake)
    result = client.report(
        {
            "metrics": "ym:s:visits,ym:s:pageviews",
            "date1": "2026-01-01",
            "date2": "2026-02-28",
        }
    )
    assert result["data"][0]["metrics"] == [15, None]  # not 10, and not silently summed
    assert result["totals"] == [15, None]
    assert result["incomplete"] is True
    assert "min" not in result and "max" not in result


def test_metrika_split_slice_without_totals_reports_none(monkeypatch, journal):
    """Missing slice totals make the merged totals unknown rather than partial sums."""
    from seohead.data_sources import metrika

    def fake(url):
        query = _metrika_query(url)
        if query["date1"][:7] != query["date2"][:7]:
            raise metrika.MetrikaError(400, "Query is too complicated")
        body = _slice_body([{"dimensions": [{"name": "/a"}], "metrics": [5]}], totals=[5])
        if query["date1"][:7] == "2026-02":
            del body["totals"]
        return body

    client = _metrika_split_client(monkeypatch, fake)
    result = client.report({"metrics": "ym:s:visits", "date1": "2026-01-01", "date2": "2026-02-28"})
    assert result["totals"] is None and result["incomplete"] is True
    assert result["data"][0]["metrics"] == [10]


def test_metrika_split_default_sort_is_first_metric_descending(monkeypatch, journal):
    """Without ``sort`` the API orders by the first metric; the merged set must too."""
    from seohead.data_sources import metrika

    def fake(url):
        query = _metrika_query(url)
        if query["date1"][:7] != query["date2"][:7]:
            raise metrika.MetrikaError(400, "Query is too complicated")
        if query["date1"][:7] == "2026-01":
            return _slice_body(
                [
                    {"dimensions": [{"name": "/a"}], "metrics": [10]},
                    {"dimensions": [{"name": "/b"}], "metrics": [1]},
                ],
                totals=[11],
            )
        return _slice_body(
            [
                {"dimensions": [{"name": "/a"}], "metrics": [5]},
                {"dimensions": [{"name": "/c"}], "metrics": [20]},
            ],
            totals=[25],
        )

    client = _metrika_split_client(monkeypatch, fake)
    result = client.report({"metrics": "ym:s:visits", "date1": "2026-01-01", "date2": "2026-02-28"})
    # Merged: /c=20, /a=15, /b=1 — the default metric sort, not slice insertion order.
    assert [row["dimensions"][0]["name"] for row in result["data"]] == ["/c", "/a", "/b"]

    windowed = client.report(
        {"metrics": "ym:s:visits", "date1": "2026-01-01", "date2": "2026-02-28"},
        offset=2,
        limit=2,
    )
    assert [row["dimensions"][0]["name"] for row in windowed["data"]] == ["/a", "/b"]


def test_metrika_split_sort_by_dimension_and_multiple_keys(monkeypatch, journal):
    """Dimension sort tokens and multi-key sorts apply to merged rows before windowing."""
    from seohead.data_sources import metrika

    def fake(url):
        query = _metrika_query(url)
        if query["date1"][:7] != query["date2"][:7]:
            raise metrika.MetrikaError(400, "Query is too complicated")
        if query["date1"][:7] == "2026-01":
            return _slice_body(
                [
                    {"dimensions": [{"name": "/c"}], "metrics": [10]},
                    {"dimensions": [{"name": "/a"}], "metrics": [10]},
                ],
                totals=[20],
            )
        return _slice_body([{"dimensions": [{"name": "/b"}], "metrics": [30]}], totals=[30])

    client = _metrika_split_client(monkeypatch, fake)
    params = {
        "metrics": "ym:s:visits",
        "dimensions": "ym:s:startURL",
        "date1": "2026-01-01",
        "date2": "2026-02-28",
    }

    result = client.report(dict(params, sort="ym:s:startURL"))
    assert [row["dimensions"][0]["name"] for row in result["data"]] == ["/a", "/b", "/c"]

    result = client.report(dict(params, sort="-ym:s:visits,ym:s:startURL"))
    assert [row["dimensions"][0]["name"] for row in result["data"]] == ["/b", "/a", "/c"]


def test_metrika_split_paginated_window_starts_at_the_global_offset(monkeypatch, journal):
    """``paginate=True, offset=2`` continues from global row 2 of the merged set (#714)."""
    from seohead.data_sources import metrika

    def fake(url):
        query = _metrika_query(url)
        offset = int(query.get("offset", "1"))
        if query["date1"] == "2026-01-01" and query["date2"] == "2026-02-28":
            raise metrika.MetrikaError(400, "Query is too complicated")
        if query["date1"][:7] == "2026-01":
            return _slice_body(
                [
                    {"dimensions": [{"name": "/b"}], "metrics": [30]},
                    {"dimensions": [{"name": "/a"}], "metrics": [10]},
                ],
                totals=[40],
            )
        assert offset == 1  # slices are always collected from their own first row
        return _slice_body([{"dimensions": [{"name": "/c"}], "metrics": [20]}], totals=[20])

    client = _metrika_split_client(monkeypatch, fake)
    params = {"metrics": "ym:s:visits", "date1": "2026-01-01", "date2": "2026-02-28"}

    result = client.report(params, paginate=True, offset=1)
    assert [row["dimensions"][0]["name"] for row in result["data"]] == ["/b", "/c", "/a"]

    result = client.report(params, paginate=True, offset=2)
    assert [row["dimensions"][0]["name"] for row in result["data"]] == ["/c", "/a"]


def test_metrika_split_enforces_one_global_row_cap(monkeypatch, journal):
    """The cap bounds rows downloaded, not rows kept: January's two rows exhaust a
    cap of two, so February is named unfetched instead of being requested."""
    from seohead.data_sources import metrika

    monkeypatch.setattr(metrika, "ROW_CAP", 2)
    urls: list[str] = []

    def fake(url):
        urls.append(url)
        query = _metrika_query(url)
        if query["date1"][:7] != query["date2"][:7]:
            raise metrika.MetrikaError(400, "Query is too complicated")
        prefix = "a" if query["date1"][:7] == "2026-01" else "c"
        return _slice_body(
            [
                {"dimensions": [{"name": f"/{prefix}1"}], "metrics": [10]},
                {"dimensions": [{"name": f"/{prefix}2"}], "metrics": [20]},
            ],
            totals=[30],
        )

    client = _metrika_split_client(monkeypatch, fake)
    result = client.report({"metrics": "ym:s:visits", "date1": "2026-01-01", "date2": "2026-02-28"})

    assert len(result["data"]) <= 2  # never more rows than the global cap
    assert result["capped"] is True and result["incomplete"] is True
    assert result["total_rows"] is None  # the full count is not a collected fact
    assert result["split"]["rows_dropped"] == 0  # February's rows were never seen
    assert result["split"]["unfetched"] == [{"date1": "2026-02-01", "date2": "2026-02-28"}]
    assert not any("date1=2026-02" in url for url in urls)


def test_metrika_split_repeated_keys_still_obey_the_global_download_cap(monkeypatch, journal):
    """Months repeating the same dimension keys cannot bypass the row budget.

    Before the fix, three identical two-row months downloaded six rows, kept two,
    and reported ``unfetched=None`` because each new row matched a stored key.
    """
    from seohead.data_sources import metrika

    monkeypatch.setattr(metrika, "ROW_CAP", 2)
    urls: list[str] = []
    served = {"rows": 0}

    def fake(url):
        urls.append(url)
        query = _metrika_query(url)
        if query["date1"][:7] != query["date2"][:7]:
            raise metrika.MetrikaError(400, "Query is too complicated")
        rows = [
            {"dimensions": [{"name": "/a"}], "metrics": [10]},
            {"dimensions": [{"name": "/b"}], "metrics": [20]},
        ]
        served["rows"] += len(rows)
        return _slice_body(rows, totals=[30])

    client = _metrika_split_client(monkeypatch, fake)
    result = client.report({"metrics": "ym:s:visits", "date1": "2026-01-01", "date2": "2026-03-31"})

    assert served["rows"] == 2  # January alone spent the budget — never six rows
    stat_urls = [u for u in urls if "stat/v1" in u]
    assert len(stat_urls) == 2  # the refused whole-range call plus January
    assert result["split"]["unfetched"] == [
        {"date1": "2026-02-01", "date2": "2026-02-28"},
        {"date1": "2026-03-01", "date2": "2026-03-31"},
    ]
    assert result["capped"] is True and result["incomplete"] is True
    assert result["total_rows"] is None and result["totals"] is None


def test_metrika_split_budget_spent_mid_slice_names_the_partial_span(monkeypatch, journal):
    """A slice truncated by the row budget leaves its own tail unread: that span is
    named ``partial`` alongside the never-requested later months."""
    from seohead.data_sources import metrika

    monkeypatch.setattr(metrika, "ROW_CAP", 2)

    def fake(url):
        query = _metrika_query(url)
        if query["date1"][:7] != query["date2"][:7]:
            raise metrika.MetrikaError(400, "Query is too complicated")
        month = query["date1"][:7]
        rows = [{"dimensions": [{"name": f"/{month}-{i}"}], "metrics": [i]} for i in range(3)]
        offset, page = int(query.get("offset", "1")), int(query["limit"])
        body = _slice_body(rows[offset - 1 : offset - 1 + page], totals=[6])
        body["total_rows"] = 3  # every month holds three rows, one page at a time
        return body

    client = _metrika_split_client(monkeypatch, fake)
    result = client.report({"metrics": "ym:s:visits", "date1": "2026-01-01", "date2": "2026-03-31"})

    assert result["split"]["unfetched"] == [
        {"date1": "2026-01-01", "date2": "2026-01-31", "partial": True},
        {"date1": "2026-02-01", "date2": "2026-02-28"},
        {"date1": "2026-03-01", "date2": "2026-03-31"},
    ]
    assert result["split"]["periods"][0]["capped"] is True
    assert len(result["data"]) == 2  # only the first page of January was collected
    assert result["capped"] is True and result["incomplete"] is True
    assert result["total_rows"] is None and result["totals"] is None


def test_metrika_split_recomputes_page_limit_from_the_remaining_budget(monkeypatch, journal):
    """A mid-slice budget remainder is requested, never a full extra page.

    With a cap of 150, January's 20 rows leave 130 for February: the second page
    may ask for only the 30 rows still allowed — asking for a whole page would
    pull 70 rows nobody can keep (#714).
    """
    from seohead.data_sources import metrika

    monkeypatch.setattr(metrika, "ROW_CAP", 150)
    month_totals = {"2026-01": 20, "2026-02": 300, "2026-03": 300}
    requests: list[tuple[str, int, int]] = []
    served = {"rows": 0}

    def fake(url):
        query = _metrika_query(url)
        if query["date1"][:7] != query["date2"][:7]:
            raise metrika.MetrikaError(400, "Query is too complicated")
        month = query["date1"][:7]
        total = month_totals[month]
        offset, limit = int(query.get("offset", "1")), int(query["limit"])
        requests.append((month, offset, limit))
        rows = [
            {"dimensions": [{"name": f"/{month}-{i}"}], "metrics": [i]}
            for i in range(offset - 1, min(offset - 1 + limit, total))
        ]
        served["rows"] += len(rows)
        return _slice_body(rows, totals=[total], total_rows=total)

    client = _metrika_split_client(monkeypatch, fake)
    result = client.report(
        {"metrics": "ym:s:visits", "date1": "2026-01-01", "date2": "2026-03-31"},
        paginate=True,
    )

    # February's second page asks for the 30-row remainder, not another 100.
    assert requests == [("2026-01", 1, 100), ("2026-02", 1, 100), ("2026-02", 101, 30)]
    assert served["rows"] == 150  # one global budget, never a row more
    assert result["split"]["unfetched"] == [
        {"date1": "2026-02-01", "date2": "2026-02-28", "partial": True},
        {"date1": "2026-03-01", "date2": "2026-03-31"},
    ]
    assert result["split"]["periods"][1]["capped"] is True
    assert len(result["data"]) == 150
    assert result["capped"] is True and result["incomplete"] is True
    assert result["total_rows"] is None and result["totals"] is None


def test_metrika_split_failed_partition_names_what_succeeded(monkeypatch, journal):
    """A partition that stays too complicated fails with full coverage provenance."""
    from seohead.data_sources import metrika

    def fake(url):
        query = _metrika_query(url)
        if query["date1"] == "2026-01-01" and query["date2"] == "2026-02-28":
            raise metrika.MetrikaError(400, "Query is too complicated")
        if query["date1"].startswith("2026-02"):
            raise metrika.MetrikaError(400, "Query is too complicated")
        return _slice_body([{"dimensions": [{"name": "/a"}], "metrics": [5]}], totals=[5])

    client = _metrika_split_client(monkeypatch, fake)
    with pytest.raises(metrika.MetrikaError) as exc:
        client.report({"metrics": "ym:s:visits", "date1": "2026-01-01", "date2": "2026-02-28"})

    message = str(exc.value)
    assert "2026-01-01..2026-02-28" in message  # the original range
    assert "2026-02-01..2026-02-28" in message  # the partition that failed
    assert "2026-01-01..2026-01-31" in message  # the partition that succeeded
    assert "accuracies tried: full, 0.1" in message  # the ladder that was exhausted


def test_metrika_split_reports_actual_sampling_not_requested_accuracy(monkeypatch, journal):
    """``sampled``/``sample_share`` come from the API bodies, never from the request."""
    from seohead.data_sources import metrika

    def fake(url):
        query = _metrika_query(url)
        if query["date1"][:7] != query["date2"][:7]:
            raise metrika.MetrikaError(400, "Query is too complicated")
        if query["date1"][:7] == "2026-01":
            return _slice_body(
                [{"dimensions": [{"name": "/a"}], "metrics": [10]}],
                totals=[10],
                sampled=False,
                sample_share=1.0,
            )
        return _slice_body(
            [{"dimensions": [{"name": "/b"}], "metrics": [20]}],
            totals=[20],
            sampled=True,
            sample_share=0.2,
            sample_size=1000,
            sample_space=5000,
        )

    client = _metrika_split_client(monkeypatch, fake)
    result = client.report({"metrics": "ym:s:visits", "date1": "2026-01-01", "date2": "2026-02-28"})

    assert result["sampled"] is True
    assert result["sample_share"] == 0.2  # the worst share actually used
    periods = result["split"]["periods"]
    assert periods[0]["sampled"] is False and periods[0]["accuracy"] == "full"
    assert periods[1]["sampled"] is True and periods[1]["sample_share"] == 0.2
    assert periods[1]["sample_size"] == 1000 and periods[1]["sample_space"] == 5000
    assert "sample_size" not in result  # slice-level values stay inside split.periods


def test_metrika_split_unsampled_body_after_sampled_request_is_honest(monkeypatch, journal):
    """A slice asked at ``accuracy=0.1`` that answers unsampled is reported unsampled."""
    from seohead.data_sources import metrika
    from seohead.mcp import handlers

    def fake(url):
        query = _metrika_query(url)
        if query.get("accuracy") == "0.1":
            return _slice_body(
                [{"dimensions": [{"name": "/a"}], "metrics": [5]}],
                totals=[5],
                sampled=False,
                sample_share=1.0,
            )
        raise metrika.MetrikaError(400, "Query is too complicated")

    client = _metrika_split_client(monkeypatch, fake)
    result = client.report({"metrics": "ym:s:visits", "date1": "2026-01-01", "date2": "2026-02-28"})
    assert result["accuracy_used"] == 0.1
    assert result["sampled"] is False  # the API said so, regardless of the request

    monkeypatch.setattr(metrika, "MetrikaClient", lambda *a, **k: client)
    out = handlers.metrika_report(
        counter_id="1", metrics="ym:s:visits", date1="2026-01-01", date2="2026-02-28"
    )
    assert out["sampled"] is False and out["accuracy"] == 0.1


def test_metrika_split_aggregates_sensitivity_and_keeps_slice_metadata(monkeypatch, journal):
    """``contains_sensitive_data`` is an OR over slices; slice metadata stays per-slice."""
    from seohead.data_sources import metrika

    def fake(url):
        query = _metrika_query(url)
        if query["date1"][:7] != query["date2"][:7]:
            raise metrika.MetrikaError(400, "Query is too complicated")
        if query["date1"][:7] == "2026-01":
            return _slice_body(
                [{"dimensions": [{"name": "/a"}], "metrics": [10]}],
                totals=[10],
                contains_sensitive_data=False,
                min=[10],
                max=[10],
                data_lag=60,
            )
        return _slice_body(
            [{"dimensions": [{"name": "/b"}], "metrics": [20]}],
            totals=[20],
            contains_sensitive_data=True,
            min=[20],
            max=[20],
            data_lag=30,
        )

    client = _metrika_split_client(monkeypatch, fake)
    result = client.report({"metrics": "ym:s:visits", "date1": "2026-01-01", "date2": "2026-02-28"})

    assert result["contains_sensitive_data"] is True  # OR, not the first slice's value
    assert result["min"] == [10] and result["max"] == [20]  # recomputed on merged rows
    assert "data_lag" not in result  # never copied from one slice to the whole body
    assert result["split"]["periods"][0]["data_lag"] == 60


def test_metrika_split_resolves_relative_dates_in_the_request_timezone(monkeypatch, journal):
    """``NdaysAgo``/``today`` resolve in the report timezone, never the host date."""
    from datetime import datetime, timezone

    from seohead.data_sources import metrika

    client = metrika.MetrikaClient(token="synthetic")
    urls: list[str] = []

    def fake(url):
        urls.append(url)
        query = _metrika_query(url)
        if "daysAgo" in query["date1"] or query["date1"] == "today":
            raise metrika.MetrikaError(400, "Query is too complicated")
        return _slice_body([{"dimensions": [{"name": "/a"}], "metrics": [1]}], totals=[1])

    monkeypatch.setattr(client, "_request", fake)
    monkeypatch.setattr(metrika.time, "sleep", lambda _seconds: None)
    # Frozen at 2026-10-02T21:30Z the host (Europe/Minsk) is already Oct 3, but the
    # report timezone -07:00 is still Oct 2 — the API interval must be Oct 1-2.
    monkeypatch.setattr(metrika, "_now", lambda: datetime(2026, 10, 2, 21, 30, tzinfo=timezone.utc))

    result = client.report(
        {
            "metrics": "ym:s:visits",
            "date1": "1daysAgo",
            "date2": "today",
            "timezone": "-07:00",
        }
    )

    spans = [
        (_metrika_query(u)["date1"], _metrika_query(u)["date2"]) for u in urls if "date1=" in u
    ]
    assert ("2026-10-01", "2026-10-02") in spans
    assert ("2026-10-02", "2026-10-03") not in spans
    assert result["split"]["requested"] == {"date1": "1daysAgo", "date2": "today"}
    assert result["split"]["resolved"] == {"date1": "2026-10-01", "date2": "2026-10-02"}


def test_metrika_split_uses_the_counter_timezone_when_not_requested(monkeypatch, journal):
    """Without ``timezone`` the counter's own zone (Management API) resolves the dates."""
    from datetime import datetime, timezone

    from seohead.data_sources import metrika

    client = metrika.MetrikaClient(token="synthetic")
    urls: list[str] = []

    def fake(url):
        urls.append(url)
        if "management" in url:
            # The real counter schema (management/v1/counter/{id}).
            return {
                "counter": {
                    "time_zone_name": "America/Los_Angeles",
                    "time_zone_offset": -420,
                }
            }
        query = _metrika_query(url)
        if query["date1"] == "1daysAgo":
            raise metrika.MetrikaError(400, "Query is too complicated")
        return _slice_body([{"dimensions": [{"name": "/a"}], "metrics": [1]}], totals=[1])

    monkeypatch.setattr(client, "_request", fake)
    monkeypatch.setattr(metrika.time, "sleep", lambda _seconds: None)
    monkeypatch.setattr(metrika, "_now", lambda: datetime(2026, 10, 2, 21, 30, tzinfo=timezone.utc))

    client.report(
        {
            "ids": "123",
            "metrics": "ym:s:visits",
            "date1": "1daysAgo",
            "date2": "today",
        }
    )

    assert any("management/v1/counter/123" in url for url in urls)
    spans = [
        (_metrika_query(u)["date1"], _metrika_query(u)["date2"]) for u in urls if "stat/v1" in u
    ]
    assert ("2026-10-01", "2026-10-02") in spans


def test_metrika_split_relative_dates_without_timezone_stay_unresolvable(monkeypatch, journal):
    """With no request or counter timezone, relative dates are retried as-is."""
    from datetime import datetime, timezone

    from seohead.data_sources import metrika

    client = metrika.MetrikaClient(token="synthetic")
    urls: list[str] = []

    def fake(url):
        urls.append(url)
        raise metrika.MetrikaError(400, "Query is too complicated")

    monkeypatch.setattr(client, "_request", fake)
    monkeypatch.setattr(metrika.time, "sleep", lambda _seconds: None)
    monkeypatch.setattr(metrika, "_now", lambda: datetime(2026, 10, 2, 21, 30, tzinfo=timezone.utc))

    with pytest.raises(metrika.MetrikaError, match="too complicated"):
        client.report({"metrics": "ym:s:visits", "date1": "1daysAgo", "date2": "today"})

    # No ids → no counter lookup → the host date is never substituted; the range is
    # retried verbatim on the accuracy ladder instead of being sliced wrongly.
    assert all("management" not in url for url in urls)
    assert all("date1=1daysAgo" in url for url in urls)


def test_metrika_split_journals_every_attempt_and_their_delays(monkeypatch, journal):
    """Retry counts, pauses, and spend rows match the attempts each partition needed."""
    from seohead.data_sources import metrika

    client = metrika.MetrikaClient(token="synthetic")
    calls: dict[str, int] = {}
    pauses: list[float] = []

    def fake(url):
        query = _metrika_query(url)
        calls[url] = calls.get(url, 0) + 1
        if query["date1"] == "2026-01-01" and query["date2"] == "2026-02-28":
            raise metrika.MetrikaError(400, "Query is too complicated")
        if query["date1"].startswith("2026-01"):
            # January answers on its third full-accuracy attempt.
            if calls[url] < 3:
                raise metrika.MetrikaError(400, "Query is too complicated")
            return _slice_body([{"dimensions": [{"name": "/a"}], "metrics": [5]}], totals=[5])
        # February refuses at full accuracy and only answers sampled.
        if query.get("accuracy") != "0.1":
            raise metrika.MetrikaError(400, "Query is too complicated")
        return _slice_body([{"dimensions": [{"name": "/b"}], "metrics": [7]}], totals=[7])

    monkeypatch.setattr(client, "_request", fake)
    monkeypatch.setattr(metrika.time, "sleep", lambda seconds: pauses.append(seconds))

    result = client.report({"metrics": "ym:s:visits", "date1": "2026-01-01", "date2": "2026-02-28"})

    periods = result["split"]["periods"]
    assert periods[0]["attempts"] == 3 and periods[0]["accuracy"] == "full"
    assert periods[1]["attempts"] == metrika.COMPLEXITY_ATTEMPTS + 1
    assert periods[1]["accuracy"] == 0.1
    # Complexity pauses grow 5s, 10s within each accuracy level; one inter-slice pause too.
    assert [p for p in pauses if p >= metrika.COMPLEXITY_PAUSE] == [5.0, 10.0, 5.0, 10.0]
    assert metrika.PAGE_PAUSE in pauses

    rows = spend.read_all()
    requested = sum(row["cost"] for row in rows if row["operation"].startswith("report"))
    assert requested == sum(calls.values())  # every attempted request is in the journal


def test_metrika_normal_request_is_a_single_operation_without_split(monkeypatch, journal):
    """A successful whole-range request stays one call and carries no ``split`` block."""
    from seohead.data_sources import metrika

    client = metrika.MetrikaClient(token="synthetic")
    calls: list[str] = []

    def fake(url):
        calls.append(url)
        return _slice_body([{"dimensions": [{"name": "/a"}], "metrics": [5]}], totals=[5])

    monkeypatch.setattr(client, "_request", fake)
    result = client.report({"metrics": "ym:s:visits", "date1": "2026-01-01", "date2": "2026-03-31"})
    assert len(calls) == 1 and "split" not in result
    assert result["data"][0]["metrics"] == [5]


def test_metrika_split_uses_counter_zone_offset_when_no_zone_name(monkeypatch, journal):
    """``time_zone_offset`` (minutes) is the documented fallback to ``time_zone_name``."""
    from datetime import datetime, timezone

    from seohead.data_sources import metrika

    client = metrika.MetrikaClient(token="synthetic")
    urls: list[str] = []

    def fake(url):
        urls.append(url)
        if "management" in url:
            return {"counter": {"time_zone_offset": -420}}  # -07:00, no IANA name
        query = _metrika_query(url)
        if query["date1"] == "1daysAgo":
            raise metrika.MetrikaError(400, "Query is too complicated")
        return _slice_body([{"dimensions": [{"name": "/a"}], "metrics": [1]}], totals=[1])

    monkeypatch.setattr(client, "_request", fake)
    monkeypatch.setattr(metrika.time, "sleep", lambda _seconds: None)
    # Host-local date is already Oct 3; at -07:00 the API's "today" is still Oct 2.
    monkeypatch.setattr(metrika, "_now", lambda: datetime(2026, 10, 2, 21, 30, tzinfo=timezone.utc))

    result = client.report(
        {"ids": "123", "metrics": "ym:s:visits", "date1": "1daysAgo", "date2": "today"}
    )

    spans = [
        (_metrika_query(u)["date1"], _metrika_query(u)["date2"]) for u in urls if "stat/v1" in u
    ]
    assert ("2026-10-01", "2026-10-02") in spans
    assert ("2026-10-02", "2026-10-03") not in spans
    assert result["split"]["resolved"] == {"date1": "2026-10-01", "date2": "2026-10-02"}


def test_metrika_split_without_counter_zone_leaves_relative_dates_unresolved(monkeypatch, journal):
    """A counter that reports no timezone never falls back to the host date."""
    from datetime import datetime, timezone

    from seohead.data_sources import metrika

    client = metrika.MetrikaClient(token="synthetic")
    urls: list[str] = []

    def fake(url):
        urls.append(url)
        if "management" in url:
            return {"counter": {"name": "synthetic"}}
        raise metrika.MetrikaError(400, "Query is too complicated")

    monkeypatch.setattr(client, "_request", fake)
    monkeypatch.setattr(metrika.time, "sleep", lambda _seconds: None)
    monkeypatch.setattr(metrika, "_now", lambda: datetime(2026, 10, 2, 21, 30, tzinfo=timezone.utc))

    with pytest.raises(metrika.MetrikaError, match="too complicated"):
        client.report(
            {"ids": "123", "metrics": "ym:s:visits", "date1": "1daysAgo", "date2": "today"}
        )

    assert any("management/v1/counter/123" in url for url in urls)
    # The zone could not be resolved, so the relative range is retried verbatim on the
    # accuracy ladder rather than being sliced against an invented date.
    report_urls = [u for u in urls if "stat/v1" in u]
    assert report_urls and all("date1=1daysAgo" in url for url in report_urls)


def test_metrika_split_cap_stops_fetching_and_names_unfetched_partitions(monkeypatch, journal):
    """Once the global raw-row budget is gone, later partitions are never requested —
    even when each month would return fresh dimension keys."""
    from seohead.data_sources import metrika

    monkeypatch.setattr(metrika, "ROW_CAP", 2)
    urls: list[str] = []

    def fake(url):
        urls.append(url)
        query = _metrika_query(url)
        if query["date1"][:7] != query["date2"][:7]:
            raise metrika.MetrikaError(400, "Query is too complicated")
        prefix = {"2026-01": "a", "2026-02": "c", "2026-03": "e"}[query["date1"][:7]]
        return _slice_body(
            [
                {"dimensions": [{"name": f"/{prefix}1"}], "metrics": [10]},
                {"dimensions": [{"name": f"/{prefix}2"}], "metrics": [20]},
            ],
            totals=[30],
        )

    client = _metrika_split_client(monkeypatch, fake)
    result = client.report({"metrics": "ym:s:visits", "date1": "2026-01-01", "date2": "2026-03-31"})

    assert len(result["data"]) <= 2  # never more rows than the global cap
    assert result["capped"] is True and result["incomplete"] is True
    assert result["total_rows"] is None
    assert result["totals"] is None  # February and March were never asked
    assert result["split"]["rows_dropped"] == 0  # nothing overfetched to drop
    assert len(result["split"]["periods"]) == 1  # January spent the whole budget
    # January's two rows exhausted the cap, so both later partitions stayed unfetched.
    assert result["split"]["unfetched"] == [
        {"date1": "2026-02-01", "date2": "2026-02-28"},
        {"date1": "2026-03-01", "date2": "2026-03-31"},
    ]
    assert not any("date1=2026-02" in url for url in urls)  # never requested
    assert not any("date1=2026-03" in url for url in urls)


def test_metrika_split_budget_spent_exactly_still_keeps_summed_totals(monkeypatch, journal):
    """All partitions fetched — even with the budget spent to the last row — means
    the reported API totals genuinely cover the whole period."""
    from seohead.data_sources import metrika

    monkeypatch.setattr(metrika, "ROW_CAP", 4)

    def fake(url):
        query = _metrika_query(url)
        if query["date1"][:7] != query["date2"][:7]:
            raise metrika.MetrikaError(400, "Query is too complicated")
        prefix = "a" if query["date1"][:7] == "2026-01" else "c"
        return _slice_body(
            [
                {"dimensions": [{"name": f"/{prefix}1"}], "metrics": [10]},
                {"dimensions": [{"name": f"/{prefix}2"}], "metrics": [20]},
            ],
            totals=[30],
        )

    client = _metrika_split_client(monkeypatch, fake)
    result = client.report({"metrics": "ym:s:visits", "date1": "2026-01-01", "date2": "2026-02-28"})

    assert result["split"]["unfetched"] is None  # every partition was asked
    assert result["totals"] == [60] and result["total_rows"] == 4
    assert result["capped"] is False and result["incomplete"] is False


def test_metrika_split_malformed_row_marks_incomplete_not_complete(monkeypatch, journal):
    """A row whose metric vector is missing cells is evidence, not a clean zero."""
    from seohead.data_sources import metrika

    bodies = {
        "2026-01": _slice_body([{"dimensions": [{"name": "/a"}], "metrics": []}], totals=[1]),
        "2026-02": _slice_body([{"dimensions": [{"name": "/b"}], "metrics": [2]}], totals=[2]),
    }

    def fake(url):
        query = _metrika_query(url)
        if query["date1"][:7] != query["date2"][:7]:
            raise metrika.MetrikaError(400, "Query is too complicated")
        return bodies[query["date1"][:7]]

    client = _metrika_split_client(monkeypatch, fake)
    result = client.report({"metrics": "ym:s:visits", "date1": "2026-01-01", "date2": "2026-02-28"})

    by_name = {row["dimensions"][0]["name"]: row["metrics"] for row in result["data"]}
    assert by_name == {"/a": [None], "/b": [2]}  # unavailable, not zero and not copied
    assert result["incomplete"] is True
    assert "min" not in result and "max" not in result


def test_metrika_split_unknown_slice_sampling_stays_unknown(monkeypatch, journal):
    """A slice without sampling fields leaves the whole-period state unreported."""
    from seohead.data_sources import metrika
    from seohead.mcp import handlers

    def fake(url):
        query = _metrika_query(url)
        if query["date1"][:7] != query["date2"][:7]:
            raise metrika.MetrikaError(400, "Query is too complicated")
        if query["date1"][:7] == "2026-01":
            return _slice_body(
                [{"dimensions": [{"name": "/a"}], "metrics": [10]}],
                totals=[10],
                sampled=False,
                sample_share=1.0,
            )
        body = _slice_body([{"dimensions": [{"name": "/b"}], "metrics": [20]}], totals=[20])
        del body["sampled"], body["sample_share"]  # the API did not say
        return body

    client = _metrika_split_client(monkeypatch, fake)
    result = client.report({"metrics": "ym:s:visits", "date1": "2026-01-01", "date2": "2026-02-28"})

    assert "sampled" not in result  # unknown — neither True nor an invented False
    assert "sample_share" not in result  # never invented from the request
    assert result["incomplete"] is True
    assert result["split"]["periods"][1]["sampled"] is None

    monkeypatch.setattr(metrika, "MetrikaClient", lambda *a, **k: client)
    out = handlers.metrika_report(
        counter_id="1", metrics="ym:s:visits", date1="2026-01-01", date2="2026-02-28"
    )
    assert out["sampled"] is None  # the handler must not coerce unknown to False


@pytest.mark.parametrize("accuracy", ["1", "high", "medium", 1, 0.5])
def test_metrika_split_accuracy_forms_reach_the_sampled_fallback(monkeypatch, journal, accuracy):
    """Every documented accuracy form above the last resort degrades to it.

    ``"1"``/``1`` equals ``full`` and ``high``/``medium`` are larger samples than the
    last-resort share, so each gets the sampled retry instead of exhausting attempts.
    """
    from seohead.data_sources import metrika

    client = metrika.MetrikaClient(token="synthetic")
    calls: list[str] = []

    def fake(url):
        calls.append(url)
        if "accuracy=0.1" not in url:
            raise metrika.MetrikaError(400, "Query is too complicated")
        return _slice_body([{"dimensions": [{"name": "/a"}], "metrics": [7]}], totals=[7])

    monkeypatch.setattr(client, "_request", fake)
    monkeypatch.setattr(metrika.time, "sleep", lambda _seconds: None)

    result = client.report(
        {
            "metrics": "ym:s:visits",
            "date1": "2026-01-01",
            "date2": "2026-02-28",
            "accuracy": accuracy,
        }
    )

    assert result["accuracy_used"] == 0.1
    assert all(p["accuracy"] == 0.1 for p in result["split"]["periods"])
    assert any("accuracy=0.1" in url for url in calls)


@pytest.mark.parametrize("accuracy", ["low", 0.05, "0.05"])
def test_metrika_split_preserves_a_caller_lower_sample(monkeypatch, journal, accuracy):
    """``low`` and shares at/below the last resort are never resampled upward."""
    from seohead.data_sources import metrika

    client = metrika.MetrikaClient(token="synthetic")
    calls: list[str] = []

    def fake(url):
        calls.append(url)
        raise metrika.MetrikaError(400, "Query is too complicated")

    monkeypatch.setattr(client, "_request", fake)
    monkeypatch.setattr(metrika.time, "sleep", lambda _seconds: None)

    with pytest.raises(metrika.MetrikaError) as exc:
        client.report(
            {
                "metrics": "ym:s:visits",
                "date1": "2026-01-01",
                "date2": "2026-02-28",
                "accuracy": accuracy,
            }
        )

    assert "accuracies tried" in str(exc.value)
    # The last-resort 0.1 share would ask for more data than the caller allowed.
    assert not any("accuracy=0.1" in url for url in calls)
    # The whole-range call plus January's retries at the caller's own setting; the
    # first partition that cannot be satisfied aborts the split.
    assert len(calls) == 1 + metrika.COMPLEXITY_ATTEMPTS


# --- Arsenkin task batches -------------------------------------------------


class _FakeClient:
    """Offline client double with controllable submission and polling failures."""

    def __init__(self, fail_on=(), fail_wait_on=()):
        self.fail_on, self.fail_wait_on = set(fail_on), set(fail_wait_on)
        self.set_calls = []

    def set_task(self, tools_name, data):
        label = data.get("label")
        self.set_calls.append(label)
        if label in self.fail_on:
            raise arsenkin.ArsenkinError("400", f"invalid task {label}")
        return {"task_id": 1000 + len(self.set_calls), "cost": 10, "raw": {}}

    def wait(self, task_id, **kwargs):
        if task_id in self.fail_wait_on:
            raise arsenkin.ArsenkinError("TIMEOUT", f"task {task_id} timed out")
        return {"result": {"task": task_id}}

    def get(self, task_id):
        return {"result": {"refetched": task_id}}


def _jobs(*labels):
    return [
        {"tools_name": "wordstat", "data": {"label": label}, "label": label} for label in labels
    ]


def test_batch_keeps_input_order():
    runner = arsenkin.BatchRunner(client=_FakeClient())
    results = runner.run(_jobs("alpha", "beta", "gamma", "delta"))
    assert [r["label"] for r in results] == ["alpha", "beta", "gamma", "delta"]


def test_batch_one_bad_job_does_not_kill_the_rest():
    """A mid-batch exception must not orphan results from already paid tasks."""
    runner = arsenkin.BatchRunner(client=_FakeClient(fail_on={"beta"}))
    results = runner.run(_jobs("alpha", "beta", "gamma"))
    assert "error" in results[1] and results[1]["code"] == "400"
    assert "result" in results[0] and "result" in results[2]


def test_batch_failed_wait_still_returns_task_id_because_it_is_paid():
    client = _FakeClient(fail_wait_on={1001})
    results = arsenkin.BatchRunner(client=client).run(_jobs("one"))
    assert results[0]["task_id"] == 1001  # Return the identifier for the paid task.
    assert results[0]["cost"] == 10
    assert "error" in results[0]


def test_batch_respects_the_five_task_api_ceiling():
    from seohead.data_sources.arsenkin import MAX_CONCURRENT

    assert MAX_CONCURRENT == 5
    runner = arsenkin.BatchRunner(client=_FakeClient())
    assert runner._max_concurrent == 5


def test_batch_refetch_is_free_and_goes_through_get():
    client = _FakeClient()
    assert arsenkin.BatchRunner(client=client).refetch(777) == {"result": {"refetched": 777}}


def test_batch_on_empty_list_is_empty():
    assert arsenkin.BatchRunner(client=_FakeClient()).run([]) == []


# --- DataForSEO: Google ----------------------------------------------------


@pytest.mark.parametrize(
    "country", ["RU", "ru", "Россия", "россия", "РФ", "Russia", "BY", "Беларусь", "belarus"]
)
def test_geo_guard_blocks_geos_dataforseo_does_not_have(country):
    """Block unsupported geographies before a paid request returns no data."""
    # Cyrillic aliases intentionally verify localized Russia and Belarus inputs.
    from seohead.data_sources.dataforseo import geo_guard

    blocked = geo_guard(country)
    assert blocked is not None
    assert blocked["ok"] is False
    assert blocked["unsupported_geo"] in {"RU", "BY"}
    assert blocked["use_instead"]  # Always recommend the appropriate alternative provider.


@pytest.mark.parametrize("country", ["US", "de", "India", None, ""])
def test_geo_guard_lets_supported_geo_through(country):
    from seohead.data_sources.dataforseo import geo_guard

    assert geo_guard(country) is None


@pytest.mark.parametrize("location_code,iso", [(2643, "RU"), (2112, "BY")])
def test_geo_guard_blocks_location_code_even_without_country(location_code, iso):
    """``location_code`` is the field actually sent on the wire; it must be checked on its own."""
    from seohead.data_sources.dataforseo import geo_guard

    blocked = geo_guard(None, location_code)
    assert blocked is not None
    assert blocked["ok"] is False
    assert blocked["unsupported_geo"] == iso


def test_geo_guard_lets_supported_location_code_through():
    from seohead.data_sources.dataforseo import geo_guard

    assert geo_guard(None, 2840) is None  # United States


@pytest.mark.parametrize("location_code", [2643, 2112])
@pytest.mark.parametrize(
    "func_name,args",
    [
        ("search_volume", (["buy apartment"],)),
        ("keyword_ideas", ("buy apartment",)),
        ("keyword_difficulty", (["buy apartment"],)),
        ("serp", ("buy apartment",)),
    ],
)
def test_blocked_location_code_never_reaches_the_network(
    monkeypatch, location_code, func_name, args
):
    """A Russia/Belarus ``location_code`` must be rejected before any provider function posts.

    Reproduces the issue exactly: ``country`` is never supplied, only the numeric geo-target
    that DataForSEO actually bills on.
    """
    from seohead.data_sources import dataforseo

    def fail_urlopen(*_args, **_kwargs):
        raise AssertionError("must not reach the network for a blocked geo target")

    monkeypatch.setattr(urllib.request, "urlopen", fail_urlopen)

    func = getattr(dataforseo, func_name)
    result = func(*args, location_code=location_code, country=None, env="prod", confirm_paid=True)
    assert result["ok"] is False
    assert result["unsupported_geo"] in {"RU", "BY"}


def test_default_environment_is_sandbox_so_nothing_is_charged_by_accident(monkeypatch):
    monkeypatch.delenv("DATAFORSEO_ENV", raising=False)
    from seohead.data_sources.dataforseo import SANDBOX_BASE, DataForSEOClient

    client = DataForSEOClient()
    assert client.env == "sandbox" and client.base == SANDBOX_BASE


def test_prod_requires_an_explicit_switch(monkeypatch):
    from seohead.data_sources.dataforseo import PROD_BASE, DataForSEOClient

    monkeypatch.setenv("DATAFORSEO_ENV", "prod")
    assert DataForSEOClient().base == PROD_BASE
    monkeypatch.delenv("DATAFORSEO_ENV")
    assert DataForSEOClient(env="prod").base == PROD_BASE


def test_task_items_survives_none_at_every_nesting_level():
    """Handle ``None`` at every level of the nested task-result-item response."""
    from seohead.data_sources.dataforseo import task_items

    assert task_items({}) == []
    assert task_items({"tasks": None}) == []
    assert task_items({"tasks": [{"result": None}]}) == []
    assert task_items({"tasks": [{"result": [{"items": None}]}]}) == []
    assert task_items({"tasks": [{"result": [{"items": []}]}]}) == []
    assert task_items({"tasks": [{"result": [{"items": [{"keyword": "alpha"}]}]}]}) == [
        {"keyword": "alpha"}
    ]


def test_task_items_merges_several_tasks():
    from seohead.data_sources.dataforseo import task_items

    body = {
        "tasks": [
            {"result": [{"items": [{"keyword": "alpha"}, {"keyword": "beta"}]}]},
            {"result": [{"items": [{"keyword": "gamma"}]}]},
        ]
    }
    assert [i["keyword"] for i in task_items(body)] == ["alpha", "beta", "gamma"]


def test_task_errors_reports_everything_except_success_code():
    from seohead.data_sources.dataforseo import task_errors

    body = {
        "tasks": [
            {"status_code": 20000, "status_message": "Ok."},
            {"status_code": 40501, "status_message": "Invalid Field: 'location_code'"},
        ]
    }
    errors = task_errors(body)
    assert len(errors) == 1 and "40501" in errors[0]


def test_endpoints_are_v3_and_live_where_expected():
    from seohead.data_sources.dataforseo import ENDPOINTS

    assert all(path.startswith("v3/") for path in ENDPOINTS.values())
    assert "live" in ENDPOINTS["search_volume"]


def test_error_message_comes_from_the_api_not_from_us():
    from seohead.data_sources.dataforseo import _message

    assert _message('{"status_message": "Payment Required."}') == "Payment Required."
    assert _message('{"tasks":[{"status_message":"Invalid Field"}]}') == "Invalid Field"
    assert _message("") == "empty response"


# --- Network-loss during a billed call must not retry blindly --------------
#
# Each fake ``urlopen`` would SUCCEED on a second attempt (see the ``len(calls)`` branch below),
# so a passing test proves the client stops after the lost response instead of getting lucky on
# a retry it never should have made.


def test_dataforseo_network_error_does_not_retry_and_logs_the_lost_attempt(monkeypatch, journal):
    monkeypatch.setenv("DATAFORSEO_LOGIN", "test-login")
    monkeypatch.setenv("DATAFORSEO_PASSWORD", "test-password")
    from seohead.data_sources import dataforseo

    calls = []

    def fake_urlopen(request, timeout=None):
        calls.append(request)
        raise urllib.error.URLError("simulated network failure")

    monkeypatch.setattr(dataforseo, "open_no_redirect", fake_urlopen)

    result = dataforseo.search_volume(
        ["buy apartment"], location_code=2840, country=None, env="prod", confirm_paid=True
    )

    assert result["ok"] is False
    assert len(calls) == 1  # The identical payload is never resent to the live endpoint.
    rows = spend.read_all()
    assert len(rows) == 1  # The lost attempt is recorded, not silently dropped.
    assert rows[0]["source"] == "dataforseo"
    assert rows[0]["cost"] == 0.0
    assert rows[0]["extra"]["attempt_failed"] == "network_error"
    # A lost response proves nothing about the charge: it must not count as a measured zero.
    assert rows[0]["extra"]["cost_unknown"] is True
    report = spend.report()
    assert "dataforseo" not in report["by_source"]
    assert len(report["uncertain"]) == 1


def test_dataforseo_malformed_response_still_creates_a_receipt(monkeypatch, journal):
    """Extends #306's receipt-after-deserialization fix to DataForSEO: a received malformed
    body must not leave the spend journal empty, and it must not claim a confirmed charge or a
    confirmed zero cost."""
    monkeypatch.setenv("DATAFORSEO_LOGIN", "test-login")
    monkeypatch.setenv("DATAFORSEO_PASSWORD", "test-password")
    from seohead.data_sources import dataforseo

    class MalformedResponse:
        status = 200

        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return False

        def read(self):
            return b"{malformed"

    calls = []

    def fake_urlopen(request, timeout=None):
        calls.append(request)
        return MalformedResponse()

    monkeypatch.setattr(dataforseo, "open_no_redirect", fake_urlopen)

    with pytest.raises(json.JSONDecodeError):
        dataforseo.search_volume(
            ["buy apartment"], location_code=2840, country=None, env="prod", confirm_paid=True
        )

    assert len(calls) == 1  # The request reached the provider exactly once.
    rows = spend.read_all()
    assert len(rows) == 1  # A receipt exists even though the body could not be parsed.
    entry = rows[0]
    assert entry["source"] == "dataforseo"
    assert entry["extra"]["response_received"] is True
    assert entry["extra"]["response_malformed"] is True
    assert entry["extra"]["charge_status"] == "unknown"
    assert entry["extra"]["cost_unknown"] is True

    # A confirmed zero-cost call for the same source is the neighbouring legitimate case: it
    # must stay in by_source, not get pulled into "uncertain" alongside the malformed receipt.
    spend.record("dataforseo", "search_volume.prod", cost=0.0, unit="usd", items=1)

    report = spend.report()
    assert report["uncertain"] == [entry]
    assert report["by_source"]["dataforseo"]["usd"] == 0.0
    assert report["calls"] == 2


def test_arsenkin_set_task_network_error_does_not_retry_and_logs_the_lost_attempt(
    monkeypatch, journal
):
    calls = []

    def fake_urlopen(request, timeout=None):
        calls.append(request)
        raise urllib.error.URLError("simulated network failure")

    monkeypatch.setattr(arsenkin, "open_no_redirect", fake_urlopen)

    client = arsenkin.ArsenkinClient(
        token="test-token", limiter=arsenkin.RateLimiter(max_calls=100)
    )
    with pytest.raises(arsenkin.ArsenkinError):
        client.set_task("keywords_frequency", {"keywords": ["alpha", "beta"]})

    assert len(calls) == 1  # /set is never resent once its response is lost.
    rows = spend.read_all()
    assert len(rows) == 1
    assert rows[0]["source"] == "arsenkin"
    assert rows[0]["cost"] == 0.0
    assert rows[0]["extra"]["attempt_failed"] == "network_error"


def test_arsenkin_read_only_endpoint_still_retries_on_network_error(monkeypatch):
    """Only ``/set`` is billed; ``/check`` is a read and stays safe to retry."""
    monkeypatch.setattr(arsenkin.time, "sleep", lambda _seconds: None)
    calls = []

    def fake_urlopen(request, timeout=None):
        calls.append(request)
        raise urllib.error.URLError("still failing, just proving a retry was attempted")

    monkeypatch.setattr(arsenkin, "open_no_redirect", fake_urlopen)

    client = arsenkin.ArsenkinClient(
        token="test-token", limiter=arsenkin.RateLimiter(max_calls=100)
    )
    with pytest.raises(arsenkin.ArsenkinError):
        client.check(123)

    assert len(calls) >= 2  # Idempotent reads keep retrying past one lost response.


def test_yandex_cloud_wordstat_top_network_error_does_not_retry_and_logs_the_lost_attempt(
    monkeypatch, journal
):
    calls = []

    def fake_urlopen(request, timeout=None, context=None):
        calls.append(request)
        raise urllib.error.URLError("simulated network failure")

    monkeypatch.setattr(yandex_cloud, "open_no_redirect", fake_urlopen)

    client = yandex_cloud.Wordstat(api_key="test-key", folder_id="test-folder")
    with pytest.raises(yandex_cloud.NetworkAmbiguousError):
        client.top("buy apartment")

    assert len(calls) == 1  # topRequests is never resent once its response is lost.
    rows = spend.read_all()
    assert len(rows) == 1
    assert rows[0]["source"] == "yandex_cloud"
    assert rows[0]["extra"]["attempt_failed"] == "network_error"


def test_yandex_cloud_websearch_submit_network_error_does_not_retry_and_logs_the_lost_attempt(
    monkeypatch, journal
):
    calls = []

    def fake_urlopen(request, timeout=None, context=None):
        calls.append(request)
        raise urllib.error.URLError("simulated network failure")

    monkeypatch.setattr(yandex_cloud, "open_no_redirect", fake_urlopen)

    client = yandex_cloud.WebSearch(api_key="test-key", folder_id="test-folder")
    with pytest.raises(yandex_cloud.NetworkAmbiguousError):
        client.search("buy apartment")

    assert len(calls) == 1  # searchAsync is never resent once its response is lost.
    rows = spend.read_all()
    assert len(rows) == 1
    assert rows[0]["source"] == "yandex_cloud"


def test_yandex_cloud_search_batch_isolates_a_lost_response_from_the_rest_of_the_batch(
    monkeypatch, journal
):
    """One query's lost response must not abort queries already queued in the same batch."""
    calls = []

    def fake_urlopen(request, timeout=None, context=None):
        calls.append(request)
        if len(calls) == 2:  # The second query's submission loses its response.
            raise urllib.error.URLError("simulated network failure")
        return _FakeSearchAsyncResponse(f"op-{len(calls)}")

    monkeypatch.setattr(yandex_cloud, "open_no_redirect", fake_urlopen)

    client = yandex_cloud.WebSearch(api_key="test-key", folder_id="test-folder")
    results = client.search_batch(["alpha", "beta", "gamma"], timeout=0)

    assert "beta" in results and "error" in results["beta"]  # Logged, not silently dropped.
    rows = spend.read_all()
    assert any(r.get("extra", {}).get("attempt_failed") == "network_error" for r in rows)


class _FakeSearchAsyncResponse:
    """Minimal context-manager double for a successful ``searchAsync`` submission."""

    status = 200

    def __init__(self, operation_id: str):
        self._body = json.dumps({"id": operation_id}).encode("utf-8")

    def __enter__(self):
        return self

    def __exit__(self, *exc_info):
        return False

    def read(self):
        return self._body


def test_yandex_cloud_search_batch_reports_a_rejected_submission_as_an_error_not_a_timeout(
    monkeypatch, journal
):
    """A provider rejection (HTTP 4xx) must never be billed or read back as a lost timeout.

    Before the fix, a non-200 submission was silently dropped from the returned mapping, and the
    caller's only signal was the query's absence — indistinguishable from an operation that was
    genuinely billed and simply timed out while polling.
    """

    def fake_urlopen(request, timeout=None, context=None):
        raise _make_http_error(400, '{"message": "invalid query"}')

    monkeypatch.setattr(yandex_cloud, "open_no_redirect", fake_urlopen)

    client = yandex_cloud.WebSearch(api_key="test-key", folder_id="test-folder")
    results = client.search_batch(["synthetic invalid query"], timeout=0)

    entry = results["synthetic invalid query"]
    assert entry["status"] == "rejected"
    assert "400" in entry["error"]
    assert entry["docs"] == []
    assert "operation_id" not in entry

    rows = spend.read_all()
    assert len(rows) == 1
    assert rows[0]["cost"] == 0  # A rejected submission was never billed.
    assert rows[0]["extra"]["operation_ids"] == {}  # No operation exists to recover.


def test_serp_fetch_never_calls_a_rejected_query_billed(monkeypatch, journal):
    """serp_fetch's note must not claim a rejected query's operation is in the spend journal."""
    from seohead.mcp import handlers

    def fake_urlopen(request, timeout=None, context=None):
        raise _make_http_error(400, '{"message": "invalid query"}')

    monkeypatch.setattr(yandex_cloud, "open_no_redirect", fake_urlopen)
    real_websearch = yandex_cloud.WebSearch
    monkeypatch.setattr(
        yandex_cloud,
        "WebSearch",
        lambda: real_websearch(api_key="test-key", folder_id="test-folder"),
    )

    result = handlers.serp_fetch(query="synthetic invalid query")

    assert result["not_returned"] == []  # A rejection is an error, not a timeout.
    assert result["note"] is None
    assert result["results"]["synthetic invalid query"]["status"] == "rejected"


def test_yandex_cloud_search_batch_dedupes_exact_duplicate_queries_before_billing(
    monkeypatch, journal
):
    """A repeated query string must be billed once and its one result must stay retrievable.

    Before the fix, each duplicate created its own paid operation, but both wrote into the same
    dict key: the later completion silently overwrote the former, hiding one paid result forever
    while the ledger showed two charges with no operation id to recover either from.
    """
    calls = []

    def fake_urlopen(request, timeout=None, context=None):
        calls.append(request)
        if request.get_method() == "GET":
            return _FakeOperationDoneResponse("https://example.com/", "result")
        return _FakeSearchAsyncResponse("only-operation")

    monkeypatch.setattr(yandex_cloud, "open_no_redirect", fake_urlopen)
    monkeypatch.setattr(yandex_cloud.time, "sleep", lambda _seconds: None)

    client = yandex_cloud.WebSearch(api_key="test-key", folder_id="test-folder")
    results = client.search_batch(["same query", "same query"])

    submissions = [c for c in calls if c.get_method() == "POST"]
    assert len(submissions) == 1  # The duplicate never reaches the provider a second time.
    assert list(results) == ["same query"]
    assert results["same query"]["operation_id"] == "only-operation"

    rows = spend.read_all()
    assert len(rows) == 1
    assert rows[0]["cost"] == 1  # Billed exactly once for the one operation created.
    assert rows[0]["extra"]["operation_ids"] == {"same query": "only-operation"}


def test_serp_fetch_reconciles_requested_and_returned_for_duplicate_queries(monkeypatch, journal):
    """requested/returned must reconcile exactly, including when the input repeats a query."""
    from seohead.mcp import handlers

    def fake_urlopen(request, timeout=None, context=None):
        if request.get_method() == "GET":
            return _FakeOperationDoneResponse("https://example.com/", "result")
        return _FakeSearchAsyncResponse("only-operation")

    monkeypatch.setattr(yandex_cloud, "open_no_redirect", fake_urlopen)
    monkeypatch.setattr(yandex_cloud.time, "sleep", lambda _seconds: None)
    real_websearch = yandex_cloud.WebSearch
    monkeypatch.setattr(
        yandex_cloud,
        "WebSearch",
        lambda: real_websearch(api_key="test-key", folder_id="test-folder"),
    )

    result = handlers.serp_fetch(queries=["same query", "same query"])

    assert result["requested"] == 1
    assert result["returned"] == 1
    assert result["not_returned"] == []


class _FakeOperationDoneResponse:
    """Minimal context-manager double for a completed ``searchAsync`` operation."""

    status = 200

    def __init__(self, url: str, title: str):
        xml = f"<doc><url>{url}</url><title>{title}</title></doc>"
        body = {"done": True, "response": {"rawData": base64.b64encode(xml.encode()).decode()}}
        self._body = json.dumps(body).encode("utf-8")

    def __enter__(self):
        return self

    def __exit__(self, *exc_info):
        return False

    def read(self):
        return self._body


# --- CLI list parsing ------------------------------------------------------


@pytest.mark.parametrize(
    "raw,expected",
    [
        ("alpha,beta,gamma", ["alpha", "beta", "gamma"]),
        ("  alpha , beta ", ["alpha", "beta"]),
        ("single", ["single"]),
        ("", None),
        (None, None),
        ("alpha,,beta", ["alpha", "beta"]),
    ],
)
def test_split_list_plain(raw, expected):
    from seohead.cli import _split_list

    assert _split_list(raw) == expected


def test_split_list_keeps_comma_inside_quotes():
    """A quoted comma must not split one query into two paid requests."""
    from seohead.cli import _split_list

    assert _split_list("'CDM pumps — specifications, selection, and prices'") == [
        "CDM pumps — specifications, selection, and prices"
    ]
    assert _split_list("'first, with a comma','second'") == ["first, with a comma", "second"]
    assert _split_list('"alpha, beta",gamma') == ["alpha, beta", "gamma"]


# --- Wayback Machine CDX (keyless, issue #97) -------------------------------


_CDX_HEADER = ["urlkey", "timestamp", "original", "mimetype", "statuscode", "digest", "length"]


def test_wayback_history_parses_a_recorded_cdx_response():
    """Fixture shape recorded from a real ``.../cdx/search/cdx?...&output=json`` response."""
    body = json.dumps(
        [
            _CDX_HEADER,
            [
                "com,example)/",
                "20200101000000",
                "https://example.com/",
                "text/html",
                "200",
                "ABCD1234",
                "1024",
            ],
            [
                "com,example)/",
                "20230601000000",
                "https://example.com/",
                "text/html",
                "404",
                "EFGH5678",
                "512",
            ],
        ]
    )
    result = wayback.history("https://example.com/", fetcher=lambda url: body)
    assert result["ok"] is True
    assert result["count"] == 2
    assert result["snapshots"][0]["statuscode"] == "200"
    assert result["snapshots"][1]["statuscode"] == "404"
    assert result["snapshots"][1]["archived_url"] == (
        "https://web.archive.org/web/20230601000000/https://example.com/"
    )


def test_wayback_history_builds_the_query_from_optional_filters():
    captured = {}

    def fetcher(url):
        captured["url"] = url
        return ""

    wayback.history(
        "https://example.com/", limit=5, from_date="2024", to_date="20260101", fetcher=fetcher
    )
    assert "url=https%3A%2F%2Fexample.com%2F" in captured["url"]
    assert "limit=5" in captured["url"]
    assert "from=2024" in captured["url"]
    assert "to=20260101" in captured["url"]


def test_wayback_history_empty_response_is_not_an_error():
    """No snapshot at all is a fact, not a failure: the CDX server returns a fully empty body."""
    result = wayback.history("https://example.com/never-archived", fetcher=lambda url: "")
    assert result == {
        "ok": True,
        "url": "https://example.com/never-archived",
        "count": 0,
        "snapshots": [],
    }


def test_wayback_history_non_json_response_is_reported_not_raised():
    result = wayback.history("https://example.com/", fetcher=lambda url: "<html>error</html>")
    assert result["ok"] is False
    assert "not JSON" in result["error"]


def test_wayback_history_empty_array_is_not_an_error():
    """The documented empty-result shape: a JSON array with nothing in it."""
    result = wayback.history("https://example.com/never-archived", fetcher=lambda url: "[]")
    assert result == {
        "ok": True,
        "url": "https://example.com/never-archived",
        "count": 0,
        "snapshots": [],
    }


def test_wayback_history_recognized_header_without_rows_is_not_an_error():
    """A valid CDX header can legitimately be the whole non-empty response."""
    result = wayback.history(
        "https://example.com/never-archived", fetcher=lambda url: json.dumps([_CDX_HEADER])
    )
    assert result == {
        "ok": True,
        "url": "https://example.com/never-archived",
        "count": 0,
        "snapshots": [],
    }


def test_wayback_history_json_object_error_payload_is_reported_not_silent():
    """A synthetically injected object error payload must not read as zero snapshots."""
    body = json.dumps({"error": "synthetic provider failure"})
    result = wayback.history("https://example.com/", fetcher=lambda url: body)
    assert result["ok"] is False
    assert "url" in result and result["url"] == "https://example.com/"
    assert "count" not in result


def test_wayback_history_malformed_non_empty_array_is_reported_not_silent():
    """A one-element array whose entry is not a header row must not read as zero snapshots."""
    body = json.dumps([{"error": "synthetic provider failure"}])
    result = wayback.history("https://example.com/", fetcher=lambda url: body)
    assert result["ok"] is False
    assert "count" not in result


def test_wayback_history_error_shaped_string_array_is_reported_not_silent():
    """A list of strings is only a header when it names the required CDX fields."""
    body = json.dumps([["error", "synthetic provider failure"]])
    result = wayback.history("https://example.com/", fetcher=lambda url: body)
    assert result["ok"] is False
    assert "count" not in result


def test_wayback_history_network_error_is_reported_not_raised():
    def fetcher(url):
        raise urllib.error.URLError("simulated network failure")

    result = wayback.history("https://example.com/", fetcher=fetcher)
    assert result["ok"] is False
    assert "request failed" in result["error"]


def test_wayback_history_requires_a_url():
    with pytest.raises(ValueError):
        wayback.history("")


# --- Certificate Transparency / crt.sh (keyless, issue #97) -----------------


def test_crtsh_subdomains_parses_a_recorded_response():
    """Fixture shape recorded from a real ``crt.sh/?q=%.example.com&output=json`` response."""
    body = json.dumps(
        [
            {"common_name": "example.com", "name_value": "example.com\nwww.example.com"},
            {"common_name": "*.app.example.com", "name_value": "*.app.example.com"},
            {"common_name": "unrelated-domain.test", "name_value": "unrelated-domain.test"},
        ]
    )
    result = crtsh.subdomains("example.com", fetcher=lambda url: body)
    assert result["ok"] is True
    assert result["subdomains"] == ["app.example.com", "example.com", "www.example.com"]
    assert result["count"] == 3


def test_crtsh_subdomains_empty_response_is_not_an_error():
    result = crtsh.subdomains("example.com", fetcher=lambda url: "")
    assert result == {"ok": True, "domain": "example.com", "count": 0, "subdomains": []}


def test_crtsh_subdomains_non_json_response_is_reported_not_raised():
    """crt.sh serves an HTML page under load instead of its JSON API; that must not read as zero."""
    result = crtsh.subdomains("example.com", fetcher=lambda url: "<html>overloaded</html>")
    assert result["ok"] is False
    assert "overloaded" in result["error"] or "not JSON" in result["error"]


def test_crtsh_subdomains_empty_array_is_not_an_error():
    """The documented empty-result shape: a JSON array with nothing in it."""
    result = crtsh.subdomains("example.com", fetcher=lambda url: "[]")
    assert result == {"ok": True, "domain": "example.com", "count": 0, "subdomains": []}


def test_crtsh_subdomains_json_object_error_payload_is_reported_not_silent():
    """A synthetically injected object error payload must not read as zero subdomains."""
    body = json.dumps({"error": "synthetic provider failure"})
    result = crtsh.subdomains("example.com", fetcher=lambda url: body)
    assert result["ok"] is False
    assert "count" not in result


def test_crtsh_subdomains_malformed_non_empty_array_is_reported_not_silent():
    """An array entry with neither expected field must not read as zero subdomains."""
    body = json.dumps([{"error": "synthetic provider failure"}])
    result = crtsh.subdomains("example.com", fetcher=lambda url: body)
    assert result["ok"] is False
    assert "count" not in result


def test_crtsh_subdomains_network_error_is_reported_not_raised():
    def fetcher(url):
        raise urllib.error.URLError("simulated network failure")

    result = crtsh.subdomains("example.com", fetcher=fetcher)
    assert result["ok"] is False


def test_crtsh_subdomains_requires_a_domain():
    with pytest.raises(ValueError):
        crtsh.subdomains("")


# --- Google Search Console (credential-gated skeleton, issue #97) ----------


def test_gsc_search_analytics_missing_credential_never_reaches_the_network(monkeypatch, tmp_path):
    monkeypatch.delenv("GSC_ACCESS_TOKEN", raising=False)
    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)

    def fail_fetcher(payload, token):
        raise AssertionError("must not reach the network without a token")

    result = gsc_core.search_analytics(
        "sc-domain:example.com",
        start_date="2026-01-01",
        end_date="2026-01-31",
        fetcher=fail_fetcher,
    )
    assert result == {
        "ok": False,
        "error": (
            f"credential not found: store it in {tmp_path / 'gsc' / 'access_token'} or set "
            "$GSC_ACCESS_TOKEN. See docs/SETUP.md for how to obtain a Search Console OAuth token."
        ),
    }


def test_gsc_search_analytics_parses_a_recorded_response():
    body = json.dumps(
        {
            "rows": [
                {
                    "keys": ["technical seo"],
                    "clicks": 12,
                    "impressions": 400,
                    "ctr": 0.03,
                    "position": 8.4,
                },
            ]
        }
    )
    result = gsc_core.search_analytics(
        "sc-domain:example.com",
        start_date="2026-01-01",
        end_date="2026-01-31",
        token="fake-token",
        fetcher=lambda payload, token: body,
    )
    assert result["ok"] is True
    assert result["count"] == 1
    assert result["rows"][0]["clicks"] == 12
    assert result["rows"][0]["keys"] == ["technical seo"]


def test_gsc_inspect_url_parses_a_recorded_response():
    body = json.dumps(
        {
            "inspectionResult": {
                "indexStatusResult": {
                    "verdict": "PASS",
                    "coverageState": "Submitted and indexed",
                    "indexingState": "INDEXING_ALLOWED",
                    "googleCanonical": "https://example.com/",
                    "userCanonical": "https://example.com/",
                }
            }
        }
    )
    result = gsc_core.inspect_url(
        "sc-domain:example.com",
        "https://example.com/",
        token="fake-token",
        fetcher=lambda payload, token: body,
    )
    assert result["ok"] is True
    assert result["coverage_state"] == "Submitted and indexed"
    assert result["verdict"] == "PASS"


def _make_http_error(code: int, body: str) -> urllib.error.HTTPError:
    import io

    return urllib.error.HTTPError(
        url="https://example.invalid",
        code=code,
        msg="error",
        hdrs=None,  # type: ignore[arg-type]
        fp=io.BytesIO(body.encode()),
    )


def test_gsc_http_error_extracts_the_api_message_without_leaking_the_token():
    def fetcher(payload, token):
        raise _make_http_error(403, json.dumps({"error": {"message": "no permission"}}))

    result = gsc_core.search_analytics(
        "sc-domain:example.com",
        start_date="2026-01-01",
        end_date="2026-01-31",
        token="secret-bearer-token",
        fetcher=fetcher,
    )
    assert result["ok"] is False
    assert result["status"] == 403
    assert "permission" in result["error"]
    assert "secret-bearer-token" not in result["error"]


def test_gsc_default_date_range_is_a_completed_inclusive_28_day_pacific_window():
    from datetime import date, datetime, timedelta

    start, end = gsc_core.default_date_range()
    expected_end = datetime.now(gsc_core.PACIFIC).date() - timedelta(days=1)
    expected_start = expected_end - timedelta(days=27)
    assert (date.fromisoformat(start), date.fromisoformat(end)) == (expected_start, expected_end)


def test_gsc_search_analytics_resolves_the_legacy_relative_labels_to_iso_dates():
    """The public CLI/MCP default (``28daysAgo``/``today``) must resolve to the documented
    Pacific-Time window instead of reaching the outbound payload unresolved."""
    captured = {}

    def fetcher(payload, token):
        captured.update(payload)
        return json.dumps({"rows": []})

    result = gsc_core.search_analytics(
        "sc-domain:example.com",
        start_date="28daysAgo",
        end_date="today",
        token="fake-token",
        fetcher=fetcher,
    )
    assert result["ok"] is True
    expected_start, expected_end = gsc_core.default_date_range()
    assert captured["startDate"] == expected_start
    assert captured["endDate"] == expected_end
    assert result["period"] == f"{expected_start}..{expected_end}"


def test_gsc_search_analytics_omitted_dates_also_resolve_to_iso_dates():
    captured = {}

    def fetcher(payload, token):
        captured.update(payload)
        return json.dumps({"rows": []})

    result = gsc_core.search_analytics("sc-domain:example.com", token="fake-token", fetcher=fetcher)
    assert result["ok"] is True
    assert captured["startDate"] == result["period"].split("..")[0]
    assert captured["endDate"] == result["period"].split("..")[1]


def test_gsc_search_analytics_passes_explicit_valid_iso_dates_through_unchanged():
    captured = {}

    def fetcher(payload, token):
        captured.update(payload)
        return json.dumps({"rows": []})

    result = gsc_core.search_analytics(
        "sc-domain:example.com",
        start_date="2026-01-01",
        end_date="2026-01-31",
        token="fake-token",
        fetcher=fetcher,
    )
    assert result["ok"] is True
    assert captured == {
        "startDate": "2026-01-01",
        "endDate": "2026-01-31",
        "dimensions": ["query"],
        "rowLimit": 1000,
    }
    assert result["period"] == "2026-01-01..2026-01-31"


@pytest.mark.parametrize(
    ("start_date", "end_date"),
    [
        ("2026-13-01", "2026-01-31"),  # Not a real calendar date.
        ("2026-01-31", "2026-01-01"),  # Reversed range.
        ("01/01/2026", "2026-01-31"),  # Wrong format.
    ],
)
def test_gsc_search_analytics_rejects_invalid_or_reversed_dates_without_calling_the_fetcher(
    start_date, end_date
):
    def fail_fetcher(payload, token):
        raise AssertionError("must not reach the network with an invalid date range")

    result = gsc_core.search_analytics(
        "sc-domain:example.com",
        start_date=start_date,
        end_date=end_date,
        token="fake-token",
        fetcher=fail_fetcher,
    )
    assert result["ok"] is False
    assert "error" in result


def test_gsc_query_handler_rejects_an_unknown_mode():
    from seohead.mcp.handlers import gsc_query

    with pytest.raises(ValueError):
        gsc_query(site_url="sc-domain:example.com", mode="bogus")


def test_gsc_query_handler_requires_inspection_url_for_inspect_mode():
    from seohead.mcp.handlers import gsc_query

    with pytest.raises(ValueError):
        gsc_query(site_url="sc-domain:example.com", mode="inspect_url")


# --- Chrome UX Report / CrUX (credential-gated skeleton, issue #97) --------


def test_crux_query_missing_credential_never_reaches_the_network(monkeypatch, tmp_path):
    monkeypatch.delenv("CRUX_API_KEY", raising=False)
    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)

    def fail_fetcher(payload, api_key):
        raise AssertionError("must not reach the network without an API key")

    result = crux.query(url="https://example.com/", fetcher=fail_fetcher)
    assert result["ok"] is False
    assert "crux/api_key" in result["error"]


def test_crux_query_parses_a_recorded_response():
    body = json.dumps(
        {
            "record": {
                "key": {"url": "https://example.com/", "formFactor": "PHONE"},
                "collectionPeriod": {"firstDate": {"year": 2026, "month": 1, "day": 1}},
                "metrics": {
                    "largest_contentful_paint": {"percentiles": {"p75": 2100}},
                    "cumulative_layout_shift": {"percentiles": {"p75": "0.05"}},
                },
            }
        }
    )
    result = crux.query(
        url="https://example.com/",
        form_factor="PHONE",
        api_key="fake-key",
        fetcher=lambda p, k: body,
    )
    assert result["ok"] is True
    assert result["form_factor"] == "PHONE"
    assert result["metrics"]["largest_contentful_paint"]["p75"] == 2100


def test_crux_query_404_means_no_data_not_a_failure():
    def fetcher(payload, api_key):
        raise _make_http_error(404, json.dumps({"error": {"message": "not found"}}))

    result = crux.query(origin="https://tiny-site.example", api_key="fake-key", fetcher=fetcher)
    assert result["ok"] is True
    assert result["state"] == "no_field_data"
    assert result["target"] == "https://tiny-site.example"
    assert result["target_kind"] == "origin"
    assert result["metrics"] == {}
    assert result["assessment"]["overall"] == "unavailable"


def test_crux_query_requires_exactly_one_of_url_or_origin():
    with pytest.raises(ValueError):
        crux.query(api_key="fake-key")
    with pytest.raises(ValueError):
        crux.query(url="https://example.com/", origin="https://example.com", api_key="fake-key")


def test_crux_query_key_travels_in_a_header_not_the_request_url():
    """The key must never be able to leak through a URL echoed into a log or an exception."""
    import inspect

    source = inspect.getsource(crux._post)
    assert "X-goog-api-key" in source
    assert "?" not in source.split("urllib.request.Request(")[1].split(",")[0]


# --- IndexNow (credential-gated skeleton, issue #97) ------------------------


def test_indexnow_submit_missing_credential_never_reaches_the_network(monkeypatch, tmp_path):
    monkeypatch.delenv("INDEXNOW_KEY", raising=False)
    monkeypatch.setattr(credentials, "CONFIG_ROOT", tmp_path)

    def fail_fetcher(payload):
        raise AssertionError("must not reach the network without a key")

    result = indexnow.submit(["https://example.com/a"], host="example.com", fetcher=fail_fetcher)
    assert result["ok"] is False
    assert "indexnow/key" in result["error"]


def test_indexnow_submit_success_names_google_as_not_adopted():
    result = indexnow.submit(
        ["https://example.com/a", "https://example.com/b"],
        host="example.com",
        key="fake-key",
        fetcher=lambda payload: (200, ""),
    )
    assert result["ok"] is True
    assert result["submitted"] == 2
    assert result["not_adopted_by"] == ["Google"]


def test_indexnow_submit_rejects_an_oversized_batch_before_touching_credentials_or_network():
    def fail_fetcher(payload):
        raise AssertionError("must not reach the network over the batch limit")

    urls = [f"https://example.com/{i}" for i in range(indexnow.MAX_URLS_PER_BATCH + 1)]
    result = indexnow.submit(urls, host="example.com", key="fake-key", fetcher=fail_fetcher)
    assert result["ok"] is False
    assert "10000" in result["error"] or "10,000" in result["error"]


def test_indexnow_submit_reports_the_documented_status_message():
    result = indexnow.submit(
        ["https://example.com/a"],
        host="example.com",
        key="fake-key",
        fetcher=lambda payload: (403, ""),
    )
    assert result["ok"] is False
    assert "key" in result["error"]


def test_indexnow_submit_network_error_is_reported_not_raised():
    def fetcher(payload):
        raise urllib.error.URLError("simulated network failure")

    result = indexnow.submit(
        ["https://example.com/a"], host="example.com", key="fake-key", fetcher=fetcher
    )
    assert result["ok"] is False


def test_indexnow_submit_requires_urls_and_host():
    with pytest.raises(ValueError):
        indexnow.submit([], host="example.com")
    with pytest.raises(ValueError):
        indexnow.submit(["https://example.com/a"], host="")
