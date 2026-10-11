"""Offline boundaries for restricted OAuth grants and provider replay."""

from __future__ import annotations

import json
import socket
import threading
import time
import urllib.parse
from datetime import datetime, timedelta, timezone

import pytest

from seohead.data_sources import credentials, gsc, oauth, oauth_flow, providers
from seohead.storage.native_scan import NativeScan
from tests.test_native_capture import _claim
from tests.test_scan_native import _metadata, _record, _runtime

_SCOPE = "https://www.googleapis.com/auth/webmasters.readonly"


def _grant() -> dict[str, object]:
    return {
        "refresh_token": "synthetic-refresh-token",
        "client_id": "synthetic-client-id",
        "client_secret": "synthetic-client-secret",
        "scopes": [_SCOPE],
    }


def _scan(path):
    with NativeScan.create(path, **_metadata()) as scan:
        lease = _claim(scan)
        scan.commit_page(lease, _record(lease.url), runtime=_runtime())
        return scan.con.execute("SELECT scan_uuid FROM scan WHERE singleton=1").fetchone()[0]


def test_gsc_connect_requires_private_file_and_never_returns_secret_values(tmp_path, monkeypatch):
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path / "config")
    source = tmp_path / "grant.json"
    source.write_text(json.dumps(_grant()), encoding="utf-8")
    source.chmod(0o644)

    with pytest.raises(ValueError, match="private bounded"):
        oauth.manage_grant("gsc", "connect", str(source))

    source.chmod(0o600)
    connected = oauth.manage_grant("gsc", "connect", str(source))
    stored = tmp_path / "config" / "gsc" / "oauth.json"
    assert connected == {"ok": True, "configured": True, "access_verified": False}
    assert stored.stat().st_mode & 0o777 == 0o600

    monkeypatch.setattr(
        oauth,
        "refresh_access_token",
        lambda _provider: {
            "access_token": "synthetic-access-token",
            "scopes": [_SCOPE],
            "expires_in": 3600,
        },
    )
    refreshed = oauth.manage_grant("gsc", "refresh")
    returned = json.dumps({"connected": connected, "refreshed": refreshed})
    for secret in ("synthetic-refresh-token", "synthetic-client-secret", "synthetic-access-token"):
        assert secret not in returned
    assert refreshed == {
        "ok": True,
        "refreshed": True,
        "scopes": [_SCOPE],
        "expires_in": 3600,
        "property_access_verified": False,
    }


def test_gsc_uses_durable_refresh_only_after_bearer_lookup_fails(monkeypatch):
    monkeypatch.delenv("GSC_ACCESS_TOKEN", raising=False)
    monkeypatch.setattr(
        credentials,
        "gsc_access_token",
        lambda: (_ for _ in ()).throw(credentials.MissingCredential("no bearer")),
    )
    monkeypatch.setattr(oauth, "grant_available", lambda _provider: True)
    monkeypatch.setattr(gsc, "durable_oauth_token", lambda: {"access_token": "refreshed-bearer"})
    seen = {}

    result = gsc.search_analytics(
        "sc-domain:example.test",
        token=None,
        fetcher=lambda payload, bearer: (
            seen.update(payload=payload, bearer=bearer) or '{"rows": []}'
        ),
    )

    assert result["ok"] is True
    assert seen["bearer"] == "refreshed-bearer"
    assert "refreshed-bearer" not in json.dumps(result)


def test_failed_remote_revoke_keeps_the_private_local_grant(tmp_path, monkeypatch):
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path / "config")
    oauth.save_grant("gsc", _grant())
    stored = tmp_path / "config" / "gsc" / "oauth.json"
    before = stored.read_bytes()

    def fail_revoke(*_args, **_kwargs):
        raise OSError("synthetic offline failure")

    monkeypatch.setattr(oauth, "open_no_redirect", fail_revoke)
    with pytest.raises(ValueError, match="local grant preserved"):
        oauth.manage_grant("gsc", "revoke", confirm=True)

    assert stored.exists()
    assert stored.read_bytes() == before


def test_gsc_replay_maps_page_dimension_and_keeps_raw_join_private(tmp_path, monkeypatch):
    scan_path = tmp_path / "scan.sqlite"
    scan_uuid = _scan(scan_path)
    evidence_path = tmp_path / "gsc-private.json"
    evidence_path.write_text(
        json.dumps(
            {
                "evidence": {
                    "format": providers.EVIDENCE_FORMAT,
                    "provider": "gsc",
                    "period": "2026-01-01..2026-01-28",
                    "status": "complete",
                    "sampling": "unknown",
                },
                "result": {
                    "dimensions": ["query", "page"],
                    "rows": [
                        {"keys": ["synthetic query", "https://example.test/"], "clicks": 7},
                        {"keys": ["unkeyable", "/relative"], "clicks": 1},
                    ],
                },
            }
        ),
        encoding="utf-8",
    )
    evidence_path.chmod(0o600)
    out_dir = tmp_path / "private-joins"
    monkeypatch.setattr(
        socket,
        "create_connection",
        lambda *_args, **_kwargs: pytest.fail("provider replay must not open a network connection"),
    )

    replayed = providers.provider_replay(
        str(scan_path), str(evidence_path), str(out_dir), review_external_only=True
    )

    assert replayed["counts"] == {
        "pages": 1,
        "rows": 2,
        "joined": 1,
        "crawl_only": 0,
        "external_only": 0,
        "unkeyable_pages": 0,
        "unkeyable_rows": 1,
    }
    public = json.dumps(replayed)
    assert "example.test" not in public and "synthetic query" not in public
    artifact = json.loads(next(out_dir.glob("provider-*.json")).read_text(encoding="utf-8"))
    assert artifact["source"]["scan_uuid"] == scan_uuid
    assert artifact["join"]["joined"][0]["external"]["url"] == "https://example.test/"
    assert artifact["join"]["joined"][0]["external"]["keys"][0] == "synthetic query"


def _flow_file(tmp_path):
    return tmp_path / "config" / "gsc" / "oauth-flow.json"


def test_flow_record_is_private_and_holds_no_grant_material(tmp_path):
    path = _flow_file(tmp_path)
    now = datetime(2026, 10, 10, 12, 0, tzinfo=timezone.utc)

    record, state = oauth_flow.create_flow(path, now)

    assert path.stat().st_mode & 0o777 == 0o600
    stored = path.read_text(encoding="utf-8")
    assert state not in stored
    assert set(json.loads(stored)) == {
        "flow_id",
        "state_sha256",
        "status",
        "created_at",
        "expires_at",
        "connected_at",
        "token_expires_at",
    }
    assert record["status"] == "waiting"
    assert record["state_sha256"] == oauth_flow.hash_state(state)
    for secret in ("synthetic-refresh-token", "synthetic-client-secret"):
        assert secret not in stored


def test_flow_expires_after_ten_minutes_without_a_write(tmp_path):
    now = datetime(2026, 10, 10, 12, 0, tzinfo=timezone.utc)
    record, _state = oauth_flow.create_flow(_flow_file(tmp_path), now)

    assert oauth_flow.effective_status(record, now + timedelta(seconds=599)) == "waiting"
    assert oauth_flow.effective_status(record, now + timedelta(seconds=600)) == "expired"


def test_status_without_flow_keeps_the_pre_change_shape(tmp_path, monkeypatch):
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path / "config")

    assert oauth.manage_grant("gsc", "status") == {
        "ok": True,
        "configured": False,
        "access_verified": False,
    }


def test_cancel_marks_waiting_flow_and_repeat_cancel_is_idempotent(tmp_path, monkeypatch):
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path / "config")
    record, _state = oauth_flow.create_flow(_flow_file(tmp_path), datetime.now(timezone.utc))

    first = oauth.manage_grant("gsc", "cancel")
    second = oauth.manage_grant("gsc", "cancel")
    status = oauth.manage_grant("gsc", "status")

    assert first == {"ok": True, "flow": {"flow_id": record["flow_id"], "status": "cancelled"}}
    assert second == first
    assert status["flow"] == {
        "flow_id": record["flow_id"],
        "status": "cancelled",
        "expires_at": record["expires_at"],
        "connected_at": None,
        "token_expires_at": None,
    }
    assert status["access_verified"] is False


def test_cancel_does_not_revive_an_expired_flow(tmp_path, monkeypatch):
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path / "config")
    oauth_flow.create_flow(_flow_file(tmp_path), datetime(2000, 1, 1, tzinfo=timezone.utc))

    assert oauth.manage_grant("gsc", "cancel") == {
        "ok": True,
        "flow": {
            "flow_id": json.loads(_flow_file(tmp_path).read_text())["flow_id"],
            "status": "expired",
        },
    }


def test_cancel_without_flow_is_a_normalized_error(tmp_path, monkeypatch):
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path / "config")

    assert oauth.manage_grant("gsc", "cancel") == {"ok": False, "error": "no_active_flow"}


def test_symlinked_flow_record_is_refused(tmp_path, monkeypatch):
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path / "config")
    target = tmp_path / "elsewhere.json"
    target.write_text("{}", encoding="utf-8")
    path = _flow_file(tmp_path)
    path.parent.mkdir(parents=True)
    path.symlink_to(target)

    with pytest.raises(ValueError, match="symlinks"):
        oauth.manage_grant("gsc", "status")


def test_oauth_callback_state_is_one_time_and_expires_or_cancels(tmp_path):
    path = _flow_file(tmp_path)
    now = datetime(2026, 10, 10, 12, 0, tzinfo=timezone.utc)
    flow, state = oauth_flow.create_flow(path, now)

    assert oauth_flow.verify_callback_state(path, state, now)["flow_id"] == flow["flow_id"]
    oauth_flow.cancel_flow(path, now)
    with pytest.raises(ValueError, match="oauth_flow_not_waiting"):
        oauth_flow.verify_callback_state(path, state, now)

    expired_path = _flow_file(tmp_path / "expired")
    expired_path.parent.mkdir(parents=True)
    _, expired_state = oauth_flow.create_flow(expired_path, now)
    with pytest.raises(ValueError, match="oauth_flow_not_waiting"):
        oauth_flow.verify_callback_state(expired_path, expired_state, now + timedelta(seconds=600))
    other_path = _flow_file(tmp_path / "other")
    other_path.parent.mkdir(parents=True)
    oauth_flow.create_flow(other_path, now)
    with pytest.raises(ValueError, match="state_mismatch"):
        oauth_flow.verify_callback_state(other_path, "bad", now)


def test_callback_completion_records_verified_exchange_times(tmp_path):
    path = _flow_file(tmp_path)
    now = datetime(2026, 10, 10, 12, 0, tzinfo=timezone.utc)
    flow, state = oauth_flow.create_flow(path, now)
    oauth_flow.verify_callback_state(path, state, now)

    connected = oauth_flow.complete_flow(path, flow, now, 3600)

    assert connected["status"] == "connected"
    assert connected["connected_at"] == now.isoformat()
    assert connected["token_expires_at"] == (now + timedelta(seconds=3600)).isoformat()


def test_oauth_consent_start_and_callback_are_redacted_and_mockable(tmp_path, monkeypatch):
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path / "config")
    monkeypatch.setenv("GSC_OAUTH_CLIENT_ID", "synthetic-client.apps.example.test")
    monkeypatch.setenv("GSC_OAUTH_CLIENT_SECRET", "synthetic-client-secret")
    monkeypatch.setenv("GSC_OAUTH_REDIRECT_URI", "http://localhost:8765/oauth/callback")

    started = oauth.manage_grant("gsc", "start")
    query = urllib.parse.parse_qs(urllib.parse.urlsplit(started["authorization_url"]).query)
    observed = {}

    def exchange(payload):
        observed.update(payload)
        return {
            "access_token": "synthetic-access-token",
            "refresh_token": "synthetic-refresh-token",
            "expires_in": 3600,
            "scope": _SCOPE,
        }

    connected = oauth.manage_grant(
        "gsc",
        "callback",
        state=query["state"][0],
        code="synthetic-code",
        exchange_transport=exchange,
    )

    public = json.dumps({"started": started, "connected": connected})
    assert "synthetic-client-secret" not in public
    assert "synthetic-access-token" not in public
    assert "synthetic-refresh-token" not in public
    assert observed["code"] == "synthetic-code"
    assert connected["flow"]["status"] == "connected"
    assert connected["flow"]["token_expires_at"]


def test_invalid_grant_refresh_is_normalized_and_clears_connected_flow_state(tmp_path, monkeypatch):
    monkeypatch.setattr(oauth, "CONFIG_ROOT", tmp_path / "config")
    oauth.save_grant("gsc", _grant())
    path = _flow_file(tmp_path)
    now = datetime(2026, 10, 10, 12, 0, tzinfo=timezone.utc)
    flow, _state = oauth_flow.create_flow(path, now)
    oauth_flow.complete_flow(path, flow, now, 3600)

    with pytest.raises(credentials.MissingCredential, match="invalid_grant") as excinfo:
        oauth.refresh_access_token(
            "gsc",
            transport=lambda _payload: {
                "error": "invalid_grant",
                "error_description": "synthetic-secret-provider-response",
            },
        )

    assert "synthetic-secret-provider-response" not in str(excinfo.value)
    assert oauth_flow.load_flow(path)["status"] == "invalid"


def test_cancel_during_oauth_exchange_prevents_grant_persistence(tmp_path, monkeypatch):
    config_root = tmp_path / "config"
    monkeypatch.setattr(oauth, "CONFIG_ROOT", config_root)
    monkeypatch.setenv("GSC_OAUTH_CLIENT_ID", "synthetic-client.apps.example.test")
    monkeypatch.setenv("GSC_OAUTH_CLIENT_SECRET", "synthetic-client-secret")
    monkeypatch.setenv("GSC_OAUTH_REDIRECT_URI", "http://localhost:8765/oauth/callback")
    now = datetime.now(timezone.utc)
    path = config_root / "gsc" / "oauth-flow.json"
    flow, state = oauth_flow.create_flow(path, now)
    exchange_started = threading.Event()
    allow_exchange_to_finish = threading.Event()
    failures = []

    def exchange(_payload):
        exchange_started.set()
        if not allow_exchange_to_finish.wait(timeout=2):
            raise RuntimeError("test exchange timed out")
        return {
            "access_token": "synthetic-access-token",
            "refresh_token": "synthetic-refresh-token",
            "expires_in": 3600,
            "scope": _SCOPE,
        }

    def callback():
        try:
            oauth.manage_grant(
                "gsc", "callback", state=state, code="synthetic-code", exchange_transport=exchange
            )
        except Exception as exc:
            failures.append(exc)

    worker = threading.Thread(target=callback)
    worker.start()
    assert exchange_started.wait(timeout=1)
    cancelled = oauth.manage_grant("gsc", "cancel")
    allow_exchange_to_finish.set()
    worker.join(timeout=2)

    assert not worker.is_alive()
    assert cancelled["flow"]["status"] == "cancelled"
    assert failures and "oauth_flow_not_waiting" in str(failures[0])
    assert not (config_root / "gsc" / "oauth.json").exists()
    assert oauth_flow.load_flow(path)["flow_id"] == flow["flow_id"]


def test_concurrent_oauth_start_cannot_replace_flow_during_callback_commit(tmp_path, monkeypatch):
    config_root = tmp_path / "config"
    monkeypatch.setattr(oauth, "CONFIG_ROOT", config_root)
    monkeypatch.setenv("GSC_OAUTH_CLIENT_ID", "synthetic-client.apps.example.test")
    monkeypatch.setenv("GSC_OAUTH_CLIENT_SECRET", "synthetic-client-secret")
    monkeypatch.setenv("GSC_OAUTH_REDIRECT_URI", "http://localhost:8765/oauth/callback")
    flow_path = config_root / "gsc" / "oauth-flow.json"
    _, state = oauth_flow.create_flow(flow_path, datetime.now(timezone.utc))
    callback_has_lock = threading.Event()
    finish_verification = threading.Event()
    original_verify = oauth_flow.verify_callback_state
    calls = 0

    def pause_during_locked_verify(path, supplied_state, now):
        nonlocal calls
        calls += 1
        flow = original_verify(path, supplied_state, now)
        if calls == 2:
            callback_has_lock.set()
            if not finish_verification.wait(timeout=2):
                raise RuntimeError("test callback commit timed out")
        return flow

    monkeypatch.setattr(oauth_flow, "verify_callback_state", pause_during_locked_verify)
    monkeypatch.setattr(
        oauth,
        "_default_exchange",
        lambda _payload: {
            "access_token": "synthetic-access-token",
            "refresh_token": "synthetic-refresh-token",
            "expires_in": 3600,
            "scope": _SCOPE,
        },
    )
    callback_results = []
    callback_errors = []
    start_errors = []
    start_attempted = threading.Event()

    def callback():
        try:
            callback_results.append(oauth.manage_grant("gsc", "callback", state=state, code="code"))
        except Exception as exc:
            callback_errors.append(exc)

    def start_again():
        start_attempted.set()
        try:
            oauth.manage_grant("gsc", "start")
        except Exception as exc:
            start_errors.append(exc)

    callback_worker = threading.Thread(target=callback)
    callback_worker.start()
    assert callback_has_lock.wait(timeout=1)
    start_worker = threading.Thread(target=start_again)
    start_worker.start()
    assert start_attempted.wait(timeout=1)
    time.sleep(0.05)
    finish_verification.set()
    callback_worker.join(timeout=2)
    start_worker.join(timeout=2)

    assert not callback_worker.is_alive() and not start_worker.is_alive()
    assert not callback_errors
    assert callback_results[0]["flow"]["status"] == "connected"
    assert start_errors and "durable OAuth grant already exists" in str(start_errors[0])
    assert (config_root / "gsc" / "oauth.json").is_file()
