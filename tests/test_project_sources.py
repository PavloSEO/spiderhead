"""Manual project-source bindings (#990, slice S1): offline, local-only, no provider calls."""

from __future__ import annotations

import json

import pytest

from seohead import cli
from seohead.mcp import handlers
from seohead.projects import bindings
from seohead.projects.workspace import create_project, open_project


def _project(tmp_path):
    project = tmp_path / "example"
    create_project(project, "https://example.test/")
    return project


def test_link_list_and_reopen_keep_the_binding_in_place(tmp_path):
    project = _project(tmp_path)
    created = bindings.link(project, "gsc", "sc-domain:example.test", label="Main property")
    assert created["created"] is True
    assert created["binding"]["origin"] == "manual"
    assert created["binding"]["status"] == "selected"

    listed = bindings.list_bindings(project)
    assert listed["count"] == 1
    assert listed["bindings"][0]["resource"] == "sc-domain:example.test"

    assert open_project(project)["ok"] is True
    assert bindings.list_bindings(project)["bindings"] == listed["bindings"]


def test_project_json_is_never_rewritten(tmp_path):
    project = _project(tmp_path)
    before = (project / "project.json").read_bytes()
    bindings.link(project, "ga4", "123456")
    assert (project / "project.json").read_bytes() == before


def test_cloudflare_zone_id_can_be_bound_without_provider_discovery(tmp_path):
    project = _project(tmp_path)

    linked = bindings.link(project, "cloudflare", "zone-123", label="Example zone")

    assert linked["binding"]["service"] == "cloudflare"
    assert linked["binding"]["resource"] == "zone-123"


def test_relinking_the_same_pair_is_idempotent(tmp_path):
    project = _project(tmp_path)
    bindings.link(project, "metrika", "42")
    again = bindings.link(project, "metrika", "42")
    assert again["created"] is False
    assert bindings.list_bindings(project)["count"] == 1


def test_unlink_removes_and_missing_pair_is_a_clear_error(tmp_path):
    project = _project(tmp_path)
    bindings.link(project, "bing", "https://example.test/")
    removed = bindings.unlink(project, "bing", "https://example.test/")
    assert removed["removed"]["service"] == "bing"
    assert bindings.list_bindings(project)["count"] == 0
    with pytest.raises(ValueError, match="no such source binding"):
        bindings.unlink(project, "bing", "https://example.test/")


def test_unknown_service_and_unsafe_resources_are_refused(tmp_path):
    project = _project(tmp_path)
    with pytest.raises(ValueError, match="unknown service"):
        bindings.link(project, "yandex", "1")
    with pytest.raises(ValueError, match="resource"):
        bindings.link(project, "gsc", "")
    with pytest.raises(ValueError, match="resource"):
        bindings.link(project, "gsc", "x" * 513)
    with pytest.raises(ValueError, match="resource"):
        bindings.link(project, "gsc", "line\nbreak")
    with pytest.raises(ValueError, match="label"):
        bindings.link(project, "gsc", "a", label="   ")
    assert not (project / "sources.json").exists()


def test_symlinked_sources_file_is_refused_on_read_and_write(tmp_path):
    project = _project(tmp_path)
    outside = tmp_path / "outside.json"
    outside.write_text("{}", encoding="utf-8")
    (project / "sources.json").symlink_to(outside)
    with pytest.raises(ValueError, match="unsafe"):
        bindings.list_bindings(project)
    with pytest.raises(ValueError, match=r"unsafe|symlink"):
        bindings.link(project, "gsc", "a")
    assert outside.read_text(encoding="utf-8") == "{}"


def test_invalid_project_is_refused_before_any_write(tmp_path):
    project = _project(tmp_path)
    (project / "project.json").write_text('{"version": 999}', encoding="utf-8")
    with pytest.raises(ValueError):
        bindings.link(project, "gsc", "a")
    assert not (project / "sources.json").exists()


def test_cli_and_handler_return_json_and_mcp_shares_the_core(tmp_path, capsys):
    project = _project(tmp_path)
    assert (
        cli.main(
            [
                "project-sources-link",
                "--directory",
                str(project),
                "--service",
                "gsc",
                "--resource",
                "https://example.test/",
            ]
        )
        == 0
    )
    linked = json.loads(capsys.readouterr().out)
    assert linked["created"] is True

    assert cli.main(["project", "sources-list", "--directory", str(project)]) == 0
    listed = json.loads(capsys.readouterr().out)
    assert set(listed) >= {"ok", "path", "count", "bindings"}
    assert listed["bindings"][0]["service"] == "gsc"

    direct = handlers.project_sources_list(str(project))
    assert direct["bindings"] == listed["bindings"]
