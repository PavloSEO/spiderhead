"""Portable project workspace contracts for #648 child A / #678."""

from __future__ import annotations

import json
import shutil

import pytest

from seohead import cli
from seohead.projects import workspace
from seohead.projects.workspace import create_project, open_project, project_status


def test_create_open_status_and_move_preserve_relative_artifact_references(tmp_path):
    project = tmp_path / "example"
    created = create_project(
        project,
        "https://example.test/",
        facts=[{"provenance": "fixture", "observed_at": None, "value": "synthetic", "name": "cms"}],
        template_references=["ecommerce/product-card"],
        profile_references=["ecommerce/basic"],
    )
    assert created["project"]["artifact_directories"]["scans"] == "scans"
    assert created["project"]["project_uuid"]
    assert created["project"]["created_at"].endswith("Z")
    status = project_status(project)
    assert status["checklist"]["state"] == "not_initialized"
    assert "0/0" not in json.dumps(status)
    assert status["preparation"]["state"] == "pending"

    moved = tmp_path / "moved"
    shutil.move(project, moved)
    assert open_project(moved)["project"]["site"]["target"] == "https://example.test/"
    assert project_status(moved)["scans"]["items"] == []


def test_status_keeps_unmeasured_operational_fields_explicit(tmp_path):
    project = tmp_path / "status"
    create_project(project, "https://example.test/")

    status = project_status(project)

    assert status["summary"]["running_scan"]["running"] is None
    assert status["summary"]["running_scan"]["reason"]
    assert status["summary"]["open_tasks"]["count"] is None
    assert status["summary"]["open_tasks"]["reason"]
    assert status["summary"]["latest_scan"]["uuid"] is None
    assert status["summary"]["latest_scan"]["reason"]
    assert status["summary"]["folder_health"]["state"] == "ok"
    assert status["agent_state"]["connected"] is None
    assert status["agent_state"]["last_seen_at"] is None
    assert status["agent_state"]["reason"]
    assert status["storage"]["relative_path"] == "scans"
    assert status["storage"]["total_bytes"] is None
    assert status["storage"]["scan_count"] is None
    assert status["storage"]["reason"]


def test_status_counts_only_current_applicable_custom_tasks():
    result = workspace._open_tasks(
        {
            "state": "initialized",
            "items": [
                {"id": "custom:open", "complete": False, "stale": False},
                {"id": "custom:done", "complete": True, "stale": False},
                {"id": "custom:stale", "complete": False, "stale": True},
                {
                    "id": "custom:excluded",
                    "complete": False,
                    "stale": False,
                    "applicability": "excluded",
                },
                {"id": "check:missing-title", "complete": False, "stale": False},
            ],
        }
    )

    assert result["count"] == 1
    assert result["state"] == "measured"
    assert result["scope"] == "primary_project_custom_checklist"


def test_settings_overlay_is_returned_without_rewriting_project_manifest(tmp_path, monkeypatch):
    project = tmp_path / "settings"
    create_project(project, "https://example.test/", label="Manifest label")
    before = (project / "project.json").read_bytes()
    monkeypatch.setattr(
        workspace,
        "_settings",
        lambda _root: {"revision": 7, "settings": {"label": "Current label"}},
    )

    opened = open_project(project)
    status = project_status(project)

    assert opened["project"]["site"]["label"] == "Current label"
    assert opened["project"]["settings_revision"] == 7
    assert status["project"]["site"]["label"] == "Current label"
    assert status["project"]["settings_revision"] == 7
    assert (project / "project.json").read_bytes() == before


def test_creation_never_overwrites_and_conflicting_identity_refuses(tmp_path):
    project = tmp_path / "example"
    create_project(project, "https://example.test/")
    before = (project / "project.json").read_bytes()
    with pytest.raises(ValueError, match="already exists"):
        create_project(project, "https://other.test/")
    assert (project / "project.json").read_bytes() == before
    with pytest.raises(ValueError, match="conflicts"):
        open_project(project, expected_site="other.test")


@pytest.mark.parametrize(
    "mutate",
    [
        lambda project: (project / "scans").rmdir(),
        lambda project: (project / "project.json").write_text('{"version":999}', encoding="utf-8"),
    ],
)
def test_malformed_or_incomplete_projects_never_open_as_successful(tmp_path, mutate):
    project = tmp_path / "example"
    create_project(project, "https://example.test/")
    mutate(project)
    with pytest.raises(ValueError):
        open_project(project)


def test_references_and_facts_refuse_traversal_and_secrets(tmp_path):
    with pytest.raises(ValueError, match="relative"):
        create_project(tmp_path / "unsafe", "https://example.test/", template_references=["../run"])
    with pytest.raises(ValueError, match="credentials"):
        create_project(
            tmp_path / "secrets",
            "https://example.test/",
            facts=[
                {"name": "api_token", "value": "x", "provenance": "fixture", "observed_at": None}
            ],
        )


def test_project_path_rejects_symlink_escape(tmp_path):
    outside = tmp_path / "outside"
    outside.mkdir()
    link = tmp_path / "project-link"
    link.symlink_to(outside, target_is_directory=True)
    with pytest.raises(ValueError, match="already exists"):
        create_project(link, "https://example.test/")
    with pytest.raises(ValueError, match="traversal"):
        create_project(tmp_path / "nested" / ".." / "outside", "https://example.test/")


def test_facts_are_key_order_independent_and_preserve_scalar_values(tmp_path):
    facts = [
        {"value": "text", "observed_at": None, "name": "string", "provenance": "fixture"},
        {
            "provenance": "fixture",
            "name": "integer",
            "value": 7,
            "observed_at": "2026-01-02T03:04:05Z",
        },
        {"name": "boolean", "value": True, "provenance": "fixture", "observed_at": None},
    ]
    created = create_project(tmp_path / "facts", "https://example.test/", facts=facts)
    assert created["project"]["facts"] == [
        {"name": "string", "value": "text", "provenance": "fixture", "observed_at": None},
        {
            "name": "integer",
            "value": 7,
            "provenance": "fixture",
            "observed_at": "2026-01-02T03:04:05Z",
        },
        {"name": "boolean", "value": True, "provenance": "fixture", "observed_at": None},
    ]


def test_site_label_never_overrides_target_host_and_boolean_version_refuses(tmp_path):
    project = tmp_path / "labeled"
    create_project(project, "https://Example.test/", label="Client label")
    assert open_project(project, expected_site="example.test")["project"]["site"] == {
        "host": "example.test",
        "target": "https://example.test/",
        "label": "Client label",
    }
    document = json.loads((project / "project.json").read_text(encoding="utf-8"))
    document["version"] = True
    (project / "project.json").write_text(json.dumps(document), encoding="utf-8")
    with pytest.raises(ValueError, match="version"):
        open_project(project)


@pytest.mark.parametrize(
    "site",
    [
        {"id": 7, "target": "https://example.test/"},
        {"host": "example.test", "label": 7, "target": "https://example.test/"},
    ],
)
def test_unknown_or_wrongly_typed_site_identity_refuses_cleanly(tmp_path, site):
    project = tmp_path / "identity"
    create_project(project, "https://example.test/")
    document = json.loads((project / "project.json").read_text(encoding="utf-8"))
    document["site"] = site
    (project / "project.json").write_text(json.dumps(document), encoding="utf-8")
    with pytest.raises(ValueError, match="site identity"):
        open_project(project)


def test_failed_manifest_publish_never_creates_a_false_project(tmp_path, monkeypatch):
    project = tmp_path / "interrupted"

    def fail(*_args, **_kwargs):
        raise OSError("simulated publish interruption")

    monkeypatch.setattr(workspace, "_publish_manifest", fail)
    with pytest.raises(OSError, match="interruption"):
        create_project(project, "https://example.test/")
    assert not (project / "project.json").exists()
    with pytest.raises(ValueError, match=r"project\.json"):
        open_project(project)


def test_nested_and_flat_project_cli_aliases(tmp_path, capsys):
    project = tmp_path / "cli"
    assert (
        cli.main(
            ["project", "new", "--directory", str(project), "--target", "https://example.test/"]
        )
        == 0
    )
    assert cli.main(["project-status", "--directory", str(project)]) == 0
    assert cli.main(["project", "open", "--directory", str(project)]) == 0
    assert '"not_initialized"' in capsys.readouterr().out


@pytest.mark.parametrize(
    "name", ["api_key", "service_api_key", "apikey", "private_key", "credentials"]
)
def test_credential_fact_names_refuse_before_project_creation(tmp_path, name):
    project = tmp_path / "secrets"
    with pytest.raises(ValueError, match="credentials"):
        create_project(
            project,
            "https://example.test/",
            facts=[
                {"name": name, "value": "synthetic", "provenance": "operator", "observed_at": None}
            ],
        )
    assert not project.exists()


def test_keyword_fact_names_are_not_mistaken_for_credentials(tmp_path):
    result = create_project(
        tmp_path / "keywords",
        "https://example.test/",
        facts=[
            {"name": "keyword_count", "value": 5, "provenance": "operator", "observed_at": None}
        ],
    )
    assert result["project"]["facts"][0]["value"] == 5
