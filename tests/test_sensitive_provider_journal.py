"""The CLI and MCP handler journal must omit provider secrets and OAuth callback values."""

from __future__ import annotations

import json
from datetime import datetime, timezone
from io import StringIO

from seohead import cli
from seohead.data_sources import oauth, oauth_flow


def _entries(path):
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]


def test_cli_credential_value_is_redacted_in_the_actual_handler_journal(
    monkeypatch, tmp_path, capsys
):
    secret = "synthetic-provider-secret"
    log_path = tmp_path / "runs.jsonl"
    monkeypatch.setenv("SEOHEAD_RUN_LOG", str(log_path))
    monkeypatch.delenv("ARSENKIN_TOKEN", raising=False)
    monkeypatch.setattr("seohead.data_sources.credentials.CONFIG_ROOT", tmp_path / "config")
    monkeypatch.setattr("sys.stdin", StringIO(secret))

    assert cli.main(
        [
            "provider-credential",
            "--provider",
            "arsenkin",
            "--component",
            "api_token",
            "--action",
            "set",
            "--value-stdin",
        ]
    ) == 0

    result_output = capsys.readouterr().out
    journal_text = log_path.read_text(encoding="utf-8")
    entry = next(item for item in _entries(log_path) if item["tool"] == "provider_credential")
    assert secret not in result_output + journal_text
    assert entry["arguments"]["value"] == "[redacted]"


def test_cli_oauth_callback_state_and_code_are_redacted_in_journal(
    monkeypatch, tmp_path, capsys
):
    state = "synthetic-callback-state"
    code = "synthetic-authorization-code"
    log_path = tmp_path / "runs.jsonl"
    config_root = tmp_path / "config"
    monkeypatch.setenv("SEOHEAD_RUN_LOG", str(log_path))
    monkeypatch.setattr(oauth, "CONFIG_ROOT", config_root)
    monkeypatch.setenv("GSC_OAUTH_CLIENT_ID", "client-id.apps.example.test")
    monkeypatch.setenv("GSC_OAUTH_CLIENT_SECRET", "synthetic-client-secret")
    monkeypatch.setenv("GSC_OAUTH_REDIRECT_URI", "http://localhost:8765/oauth/callback")
    _, state = oauth_flow.create_flow(
        config_root / "gsc" / "oauth-flow.json", datetime.now(timezone.utc)
    )
    monkeypatch.setattr(
        oauth,
        "_default_exchange",
        lambda _payload: {
            "access_token": "synthetic-access-token",
            "refresh_token": "synthetic-refresh-token",
            "expires_in": 3600,
            "scope": "https://www.googleapis.com/auth/webmasters.readonly",
        },
    )
    monkeypatch.setattr("sys.stdin", StringIO(json.dumps({"state": state, "code": code})))

    assert cli.main(
        ["provider-auth", "--provider", "gsc", "--action", "callback", "--callback-stdin"]
    ) == 0

    result_output = capsys.readouterr().out
    journal_text = log_path.read_text(encoding="utf-8")
    entry = next(item for item in _entries(log_path) if item["tool"] == "provider_auth")
    for sensitive in (
        state,
        code,
        "synthetic-client-secret",
        "synthetic-access-token",
        "synthetic-refresh-token",
    ):
        assert sensitive not in result_output + journal_text
    assert entry["arguments"]["state"] == "[redacted]"
    assert entry["arguments"]["code"] == "[redacted]"
