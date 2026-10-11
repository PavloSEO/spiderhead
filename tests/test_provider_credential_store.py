"""Offline tests for the write-only credential storage primitives (issue #965, slice 1)."""

from __future__ import annotations

import os
import stat

import pytest

from seohead.data_sources import credentials

SECRET = "sk-test-0123456789-do-not-leak"


@pytest.fixture
def config_root(monkeypatch, tmp_path):
    root = tmp_path / "config"
    root.mkdir()
    monkeypatch.setattr(credentials, "CONFIG_ROOT", root)
    return root


def _mode(path) -> int:
    return stat.S_IMODE(os.stat(path).st_mode)


def test_write_creates_private_file_and_directory(config_root):
    credentials.write_secret("arsenkin/token", SECRET)

    target = config_root / "arsenkin" / "token"
    assert target.read_text(encoding="utf-8") == SECRET
    assert _mode(target) == 0o600
    assert _mode(config_root / "arsenkin") == 0o700
    assert list((config_root / "arsenkin").glob(".credential-*")) == []


def test_write_tightens_existing_directory_mode(config_root):
    parent = config_root / "arsenkin"
    parent.mkdir(mode=0o755)
    os.chmod(parent, 0o755)

    credentials.write_secret("arsenkin/token", SECRET)

    assert _mode(parent) == 0o700


def test_set_refuses_existing_file_and_keeps_old_bytes(config_root):
    credentials.write_secret("arsenkin/token", "old-value")

    with pytest.raises(FileExistsError) as excinfo:
        credentials.write_secret("arsenkin/token", SECRET)

    assert (config_root / "arsenkin" / "token").read_text(encoding="utf-8") == "old-value"
    assert SECRET not in str(excinfo.value)


def test_replace_requires_existing_file(config_root):
    with pytest.raises(FileNotFoundError):
        credentials.write_secret("arsenkin/token", SECRET, replace=True)

    assert not (config_root / "arsenkin" / "token").exists()


def test_replace_overwrites_existing_file(config_root):
    credentials.write_secret("arsenkin/token", "old-value")

    credentials.write_secret("arsenkin/token", SECRET, replace=True)

    target = config_root / "arsenkin" / "token"
    assert target.read_text(encoding="utf-8") == SECRET
    assert _mode(target) == 0o600


def test_failed_replace_keeps_old_bytes_and_removes_staged_file(config_root, monkeypatch):
    credentials.write_secret("arsenkin/token", "old-value")

    def boom(*_args, **_kwargs):
        raise OSError("simulated rename failure")

    monkeypatch.setattr(credentials.os, "replace", boom)
    with pytest.raises(OSError) as excinfo:
        credentials.write_secret("arsenkin/token", SECRET, replace=True)

    target = config_root / "arsenkin" / "token"
    assert target.read_text(encoding="utf-8") == "old-value"
    assert list(target.parent.glob(".credential-*")) == []
    assert SECRET not in str(excinfo.value)


def test_symlinked_file_is_refused(config_root, tmp_path):
    victim = tmp_path / "elsewhere"
    victim.write_text("keep", encoding="utf-8")
    (config_root / "arsenkin").mkdir()
    (config_root / "arsenkin" / "token").symlink_to(victim)

    with pytest.raises(ValueError):
        credentials.write_secret("arsenkin/token", SECRET, replace=True)
    with pytest.raises(ValueError):
        credentials.revoke_secret("arsenkin/token")

    assert victim.read_text(encoding="utf-8") == "keep"


def test_symlinked_parent_directory_is_refused(config_root, tmp_path):
    real_dir = tmp_path / "real"
    real_dir.mkdir()
    (config_root / "arsenkin").symlink_to(real_dir, target_is_directory=True)

    with pytest.raises(ValueError):
        credentials.write_secret("arsenkin/token", SECRET)

    assert list(real_dir.iterdir()) == []


def test_revoke_removes_file_and_reports_absence(config_root):
    credentials.write_secret("arsenkin/token", SECRET)

    assert credentials.revoke_secret("arsenkin/token") is True
    assert not (config_root / "arsenkin" / "token").exists()
    assert credentials.revoke_secret("arsenkin/token") is False


def test_empty_value_is_rejected_without_writing(config_root):
    with pytest.raises(ValueError):
        credentials.write_secret("arsenkin/token", "")

    assert not (config_root / "arsenkin" / "token").exists()


def test_provider_credential_contract_returns_only_reference_and_timestamp(config_root, monkeypatch):
    from seohead.data_sources import providers

    monkeypatch.delenv("ARSENKIN_TOKEN", raising=False)
    result = providers.manage_credential("arsenkin", "api_token", "set", value=SECRET)

    assert result["source_reference"] == "config:arsenkin/token"
    assert result["updated_at"]
    assert SECRET not in repr(result)
    status = providers.manage_credential("arsenkin", "api_token")
    assert status["state"] == "configured_unverified"
    assert SECRET not in repr(status)


def test_provider_credential_replacement_and_revoke_require_explicit_actions(config_root):
    from seohead.data_sources import providers

    providers.manage_credential("arsenkin", "api_token", "set", value="old")
    with pytest.raises(FileExistsError):
        providers.manage_credential("arsenkin", "api_token", "set", value=SECRET)
    replaced = providers.manage_credential(
        "arsenkin", "api_token", "set", value=SECRET, replace=True
    )
    assert replaced["replaced"] is True
    with pytest.raises(ValueError, match="confirm=true"):
        providers.manage_credential("arsenkin", "api_token", "revoke")
    revoked = providers.manage_credential(
        "arsenkin", "api_token", "revoke", confirm=True
    )
    assert revoked["revoked"] is True


def test_cli_credential_write_reads_stdin_without_echoing_value(config_root, monkeypatch, capsys):
    from io import StringIO

    from seohead import cli

    monkeypatch.delenv("ARSENKIN_TOKEN", raising=False)
    monkeypatch.setattr("sys.stdin", StringIO(SECRET))
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
    output = capsys.readouterr()
    assert SECRET not in output.out + output.err
    assert (config_root / "arsenkin" / "token").read_text(encoding="utf-8") == SECRET
