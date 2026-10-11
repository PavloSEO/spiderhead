"""Revision-aware conditional reads for issue #979/#953."""

from __future__ import annotations

import pytest

from seohead import cli
from seohead.projects.observer import observe, observe_activity
from seohead.projects.workspace import create_project


def test_project_activity_returns_small_not_modified_response(tmp_path):
    project = tmp_path / "activity"
    create_project(project, "https://example.test/")

    first = observe_activity(str(project))
    unchanged = observe_activity(str(project), since_revision=first["revision"])

    assert len(first["revision"]) == 64
    assert unchanged == {
        "ok": True,
        "revision": first["revision"],
        "not_modified": True,
    }


def test_project_observe_revision_changes_with_saved_snapshot_data(tmp_path):
    project = tmp_path / "observe"
    create_project(project, "https://example.test/")

    first = observe(str(project), scan_limit=1)
    unchanged = observe(str(project), scan_limit=1, since_revision=first["revision"])
    with (project / "log.md").open("a", encoding="utf-8") as stream:
        stream.write("\nA recorded synthetic change.\n")
    changed = observe(str(project), scan_limit=1, since_revision=first["revision"])

    assert unchanged["not_modified"] is True
    assert changed["not_modified"] is False
    assert changed["revision"] != first["revision"]


def test_conditional_revision_is_a_validated_sha256_token(tmp_path):
    project = tmp_path / "invalid-revision"
    create_project(project, "https://example.test/")

    with pytest.raises(ValueError, match="64-character"):
        observe_activity(str(project), since_revision="short")


def test_cli_observer_flags_map_the_revision_token(tmp_path):
    project = tmp_path / "cli"
    create_project(project, "https://example.test/")
    token = "a" * 64

    observe_args = cli.build_parser().parse_args(
        ["project-observe", "--directory", str(project), "--since-revision", token]
    )
    activity_args = cli.build_parser().parse_args(
        ["project-activity", "--directory", str(project), "--since-revision", token]
    )

    assert cli._build_kwargs(observe_args)[1]["since_revision"] == token
    assert cli._build_kwargs(activity_args)[1]["since_revision"] == token
