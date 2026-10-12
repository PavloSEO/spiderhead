"""Offline state, revocation and client registration contracts (#929)."""

from __future__ import annotations

import asyncio
import json
from pathlib import Path

import pytest

from seohead import cli
from seohead.mcp import mcp_control as control


@pytest.fixture(autouse=True)
def isolated_home(tmp_path, monkeypatch):
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("SEOHEAD_CONFIG_DIR", str(tmp_path / "core"))
    monkeypatch.setenv("CODEX_HOME", str(tmp_path / ".codex"))
    monkeypatch.delenv("XDG_CONFIG_HOME", raising=False)
    monkeypatch.delenv("APPDATA", raising=False)


def test_default_status_never_creates_files(tmp_path):
    status = control.status()
    assert status["enabled"] and status["profile"] == "full"
    assert status["tools"] > 0
    assert all(not v["registered"] for v in status["clients"].values())
    assert not list(tmp_path.iterdir())


def test_state_roundtrip_and_audit():
    assert cli.main(["mcp", "disable", "--json"]) == 0
    assert not control.status()["enabled"]
    assert (
        cli.main(["mcp", "enable", "--profile", "router", "--actor", "SEOHEAD Desktop", "--json"])
        == 0
    )
    state = control.read_state()
    assert state["by"] == "SEOHEAD Desktop" and state["changed_at"]
    assert len(state["history"]) == 2
    assert control.status()["tools"] == 4


def test_disabled_startup_without_sdk(capsys):
    control.set_state(False)
    from seohead.mcp.mcp_server import main

    assert main() == 1
    assert control.DISABLED in capsys.readouterr().err


def test_live_server_revoked_before_handler(monkeypatch):
    pytest.importorskip("mcp")
    from mcp.server.fastmcp.exceptions import ToolError

    from seohead.mcp import handlers
    from seohead.mcp.mcp_server import build_server

    calls = []
    monkeypatch.setattr(handlers, "tool_catalog", lambda **kw: calls.append(kw) or {"ok": True})
    server = build_server()
    control.set_state(False)
    with pytest.raises(ToolError, match="disabled"):
        asyncio.run(server.call_tool("seo_tool_catalog", {}))
    assert not calls
    control.set_state(True, profile="router")
    asyncio.run(server.call_tool("seo_tool_catalog", {}))
    assert len(calls) == 1
    assert {t.name for t in asyncio.run(server.list_tools())} == {
        "seo_inspect_url",
        "seo_audit_workflow",
        "seo_tool_catalog",
        "seo_doctor",
    }
    with pytest.raises(ToolError, match="profile"):
        asyncio.run(server.call_tool("seo_parse", {"url": "https://example.test"}))


@pytest.mark.parametrize("client", control.CLIENTS)
def test_plan_permission_backup_preserve_and_restore(client, tmp_path):
    path = control.client_path(client)
    path.parent.mkdir(parents=True, exist_ok=True)
    raw = (
        b'# Foreign comment\nforeign_key = "private-placeholder"\n[mcp_servers.other]\ncommand = "other"\n'
        if client == "codex"
        else b'{"foreign_key":"private-placeholder","mcpServers":{"other":{"command":"other"}}}\n'
    )
    path.write_bytes(raw)
    before_files = set(tmp_path.rglob("*"))
    plan = control.install(client, dry_run=True, command="/opt/seohead")
    assert "private-placeholder" not in json.dumps(plan)
    assert set(tmp_path.rglob("*")) == before_files
    assert path.read_bytes() == raw
    with pytest.raises(ValueError, match="--yes"):
        control.install(client)
    result = control.install(
        client,
        yes=True,
        command="/opt/seohead",
        expected_sha256=plan["sha256"],
        backup_path=plan["backup"],
    )
    assert result["backup"] == plan["backup"]
    assert Path(result["backup"]).read_bytes() == raw
    assert control.status()["clients"][client]["registered"]
    current = path.read_bytes()
    if client == "codex":
        assert b"# Foreign comment" in current
    with pytest.raises(ValueError, match="--yes"):
        control.uninstall(client)
    control.uninstall(client, yes=True)
    assert path.read_bytes() == raw


@pytest.mark.parametrize("client", control.CLIENTS)
def test_uninstall_preserves_later_foreign_changes(client):
    result = control.install(client, yes=True, command="/opt/seohead")
    path = Path(result["file"])
    doc, key = control._parse(client, path.read_bytes())
    doc["later"] = "kept"
    path.write_bytes(control._serialize(client, doc))
    control.uninstall(client, yes=True)
    restored, _ = control._parse(client, path.read_bytes())
    assert restored["later"] == "kept" and "seohead" not in restored.get(key, {})


def test_changed_preview_invalid_and_symlink_configs(tmp_path):
    path = control.client_path("claude-code")
    plan = control.install("claude-code", dry_run=True)
    path.write_text("{}")
    with pytest.raises(ValueError, match="changed"):
        control.install("claude-code", yes=True, expected_sha256=plan["sha256"])
    path.write_text('{"secret":"hidden-placeholder"')
    with pytest.raises(ValueError) as caught:
        control.install("claude-code", yes=True)
    assert "hidden-placeholder" not in str(caught.value)
    path.unlink()
    path.symlink_to(tmp_path / "target")
    with pytest.raises(ValueError, match="symlink"):
        control.install("claude-code", yes=True)


def test_corrupt_backup_and_edited_own_entry_refused():
    result = control.install("cursor", yes=True)
    path = Path(result["file"])
    before = path.read_bytes()
    Path(result["backup"]).write_bytes(b"corrupted")
    with pytest.raises(ValueError, match="hash"):
        control.uninstall("cursor", yes=True)
    assert path.read_bytes() == before


def test_existing_own_entry_is_only_replaced_with_permission_and_can_be_restored():
    path = control.client_path("claude-code")
    raw = b'{"mcpServers":{"seohead":{"env":{"TOKEN":"hidden-placeholder"}},"other":{"command":"kept"}}}'
    path.write_bytes(raw)
    plan = control.install("claude-code", dry_run=True)
    assert plan["operation"] == "replace" and plan["changed"]
    assert "hidden-placeholder" not in json.dumps(plan)
    assert path.read_bytes() == raw
    with pytest.raises(ValueError, match="--yes"):
        control.install("claude-code")
    result = control.install(
        "claude-code", yes=True, expected_sha256=plan["sha256"], backup_path=plan["backup"]
    )
    assert "hidden-placeholder" not in json.dumps(result)
    assert json.loads(path.read_bytes())["mcpServers"]["other"]["command"] == "kept"
    control.uninstall("claude-code", yes=True)
    assert path.read_bytes() == raw


@pytest.mark.parametrize("client", control.CLIENTS)
def test_restoring_an_initially_missing_config_restores_absence(client):
    path = control.client_path(client)
    assert not path.exists()
    control.install(client, yes=True, command="/opt/seohead")
    assert path.exists()
    control.uninstall(client, yes=True)
    assert not path.exists()


def test_status_lists_profiles_with_registry_counts_and_actor_label():
    status = control.status()
    profiles = {item["id"]: item for item in status["profiles"]}
    assert set(profiles) == {"full", "audit", "quick-check", "infra", "router"}
    assert profiles["full"]["tools"] == status["tools"]
    assert profiles["audit"]["label"] == "Аудит"
    from seohead.mcp.mcp_profiles import profile_tools
    from seohead.mcp.tool_reference import load_seo_tools, load_sf_tools

    names = {t.name for t in [*load_seo_tools(), *load_sf_tools()]}
    for name in ("audit", "quick-check", "router"):
        assert profiles[name]["tools"] == len(names & profile_tools(name)) < len(names)
    assert status["by_label"] == "по умолчанию"
    control.set_state(True, profile="audit", actor="SEOHEAD Desktop")
    status = control.status()
    assert status["tools"] == profiles["audit"]["tools"]
    assert status["by_label"].startswith("SEOHEAD Desktop · сегодня ")
    assert control.changed_label("CLI", "2026-10-07T15:05:00+00:00").startswith("CLI · 07.10 ")


def test_backups_list_client_path_time_reason_and_hash(capsys):
    assert control.backups()["backups"] == []
    control.client_path("cursor").parent.mkdir(parents=True)
    control.client_path("cursor").write_text("{}")
    control.install("cursor", yes=True, command="/opt/seohead")
    control.install("codex", yes=True, command="/opt/seohead")
    items = control.backups("cursor")["backups"]
    assert len(items) == 1
    item = items[0]
    assert item["client"] == "cursor" and item["reason"] == "install" and item["verified"]
    assert Path(item["path"]).is_file() and item["created_at"].endswith("+00:00")
    assert len(control.backups()["backups"]) == 2
    assert cli.main(["mcp", "backups", "--client", "cursor", "--json"]) == 0
    assert json.loads(capsys.readouterr().out)["backups"][0]["sha256"] == item["sha256"]
