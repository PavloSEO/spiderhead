"""Issue #979: `seohead version --json` reports core identity without paths or secrets."""

from __future__ import annotations

import json

import pytest

from seohead import cli
from seohead._build.provenance import PackagedProvenance
from seohead.cli import COMMANDS
from seohead.core import core_info as core_info_module
from seohead.core.core_info import CORE_INFO_FORMAT, core_info


def test_core_info_shape_and_types():
    info = core_info(COMMANDS)
    assert info["format"] == CORE_INFO_FORMAT
    assert isinstance(info["package_version"], str) and info["package_version"]
    assert info["revision"] is None or len(info["revision"]) == 40
    assert set(info) == {
        "format",
        "contract_version",
        "package_version",
        "revision",
        "build",
        "supported_tools",
        "desktop_compatibility",
        "project",
        "ledger",
        "scan_formats",
        "commands",
    }
    assert info["scan_formats"] == ["scan.v1", "scan.v2"]
    assert "parse" in info["commands"]
    assert info["contract_version"] == CORE_INFO_FORMAT
    assert info["build"]["state"] in {"verified", "unavailable"}
    assert info["build"]["built_at"] is None or info["build"]["built_at"].endswith("Z")
    assert info["supported_tools"]["cli"] == list(COMMANDS)
    assert info["desktop_compatibility"]["policy"] == "exact_core_revision"
    assert info["desktop_compatibility"]["compatible"] is None


def test_core_info_has_no_absolute_paths():
    text = json.dumps(core_info(COMMANDS))
    assert "/Users/" not in text and "/home/" not in text and "C:\\" not in text


def test_revision_is_none_without_manifest(monkeypatch):
    def missing(*_a, **_k):
        raise FileNotFoundError("no manifest")

    monkeypatch.setattr("seohead._build.provenance.packaged_provenance", missing)
    assert core_info_module._revision() is None


def test_build_time_and_supported_mcp_tools_are_exposed(monkeypatch):
    monkeypatch.setattr(
        "seohead._build.provenance.packaged_provenance",
        lambda: PackagedProvenance("3.0.0", "a" * 40, "2026-10-12T12:00:00Z"),
    )
    info = core_info(COMMANDS, ["seo_doctor"])
    assert info["build"] == {
        "state": "verified",
        "revision": "a" * 40,
        "built_at": "2026-10-12T12:00:00Z",
        "reason": None,
    }
    assert info["supported_tools"]["mcp"] == ["seo_doctor"]


def test_desktop_compatibility_uses_the_pinned_revision(monkeypatch):
    revision = "a" * 40
    monkeypatch.setattr(
        "seohead._build.provenance.packaged_provenance",
        lambda: PackagedProvenance("3.0.0", revision, "2026-10-12T12:00:00Z"),
    )

    compatible = core_info(COMMANDS, required_core_revision=revision)
    incompatible = core_info(COMMANDS, required_core_revision="b" * 40)

    assert compatible["desktop_compatibility"]["compatible"] is True
    assert incompatible["desktop_compatibility"]["compatible"] is False
    with pytest.raises(ValueError, match="full lowercase Git SHA"):
        core_info(COMMANDS, required_core_revision="short")


def test_version_json_command(capsys):
    assert cli.main(["version", "--json"]) == 0
    out = json.loads(capsys.readouterr().out)
    assert out["format"] == CORE_INFO_FORMAT


def test_version_text_command(capsys):
    assert cli.main(["version"]) == 0
    assert capsys.readouterr().out.startswith("seohead ")
