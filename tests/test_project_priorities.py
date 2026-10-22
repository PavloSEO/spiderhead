"""Offline priority policy previews and explicit v2 applications."""

from __future__ import annotations

import json

import pytest

from seohead.projects.coverage import coverage_status, initialize_coverage, update_item
from seohead.projects.priorities import project_priorities
from seohead.projects.workspace import create_project


@pytest.fixture
def project(tmp_path):
    root = tmp_path / "project"
    create_project(
        root,
        "https://example.test/",
        facts=[
            {
                "name": "framework",
                "value": "react",
                "provenance": "operator",
                "observed_at": None,
            }
        ],
    )
    initialize_coverage(root)
    return root


def test_preview_reads_b_v1_without_mutating_it(project):
    path = project / "coverage.json"
    before = path.read_bytes()
    preview = project_priorities(str(project), full=True)

    decision = next(
        item for item in preview["decisions"] if item["id"] == "skill:workflow/js-render-check"
    )
    assert decision["after"] == {"priority": "P0", "priority_origin": "policy"}
    assert decision["consulted_facts"] == ["framework"]
    assert preview["applied"] is False
    assert path.read_bytes() == before


def test_packaged_baseline_applies_without_saved_facts(tmp_path):
    root = tmp_path / "no-facts"
    create_project(root, "https://example.test/")
    initialize_coverage(root)

    preview = project_priorities(str(root), full=True)
    critical = next(
        item for item in preview["decisions"] if item["id"] == "check:CANONICAL_MISSING"
    )
    stylistic = next(item for item in preview["decisions"] if item["id"] == "check:URL_UNDERSCORES")
    unchanged = next(item for item in preview["decisions"] if item["id"] == "check:URL_HAS_PARAMS")

    assert critical["after"] == {"priority": "P0", "priority_origin": "policy"}
    assert stylistic["after"] == {"priority": "P2", "priority_origin": "policy"}
    assert unchanged["after"] == {"priority": "P1", "priority_origin": "default"}
    assert critical["consulted_facts"] == []


def test_apply_publishes_v2_and_unchanged_reapply_preserves_bytes(project):
    revision = coverage_status(project)["revision"]
    result = project_priorities(str(project), full=True, apply=True, expected_revision=revision)
    assert result["applied"] is True
    saved = json.loads((project / "coverage.json").read_text())
    assert (saved["format"], saved["version"]) == ("seohead.coverage.v2", 2)
    assert (
        saved["priority_policy"]["applications"][-1]["receipt"]["facts"][0]["observed_at"] is None
    )
    status = coverage_status(project)
    row = next(item for item in status["items"] if item["id"] == "skill:workflow/js-render-check")
    assert (row["priority"], row["priority_origin"], row["priority_reason"]) == (
        "P0",
        "policy",
        "matched javascript-framework",
    )
    before = (project / "coverage.json").read_bytes()
    repeated = project_priorities(str(project), full=True, apply=True, expected_revision=status["revision"])
    assert repeated["applied"] is False
    assert (project / "coverage.json").read_bytes() == before


def test_operator_priority_survives_policy_application(project):
    revision = coverage_status(project)["revision"]
    update_item(project, {"id": "skill:workflow/js-render-check", "priority": "P1"}, revision)
    revision = coverage_status(project)["revision"]

    project_priorities(str(project), full=True, apply=True, expected_revision=revision)

    row = next(
        item
        for item in coverage_status(project)["items"]
        if item["id"] == "skill:workflow/js-render-check"
    )
    assert (row["priority"], row["priority_origin"], row["priority_reason"]) == (
        "P1",
        "operator",
        "operator priority preserved",
    )


def test_status_hides_old_policy_reason_after_an_operator_override(project):
    project_priorities(
        str(project), apply=True, expected_revision=coverage_status(project)["revision"]
    )
    update_item(
        project,
        {"id": "skill:workflow/js-render-check", "priority": "P1"},
        coverage_status(project)["revision"],
    )

    status = coverage_status(project)
    row = next(item for item in status["items"] if item["id"] == "skill:workflow/js-render-check")

    assert row["priority_reason"] == "operator priority preserved"
    assert status["priority_policy"]["facts_state"] == "matches_current_project"

    manifest = json.loads((project / "project.json").read_text())
    manifest["facts"][0]["value"] = "Vue.js"
    (project / "project.json").write_text(json.dumps(manifest))
    assert coverage_status(project)["priority_policy"]["facts_state"] == "changed_since_application"


def test_v1_rejects_v2_history_and_policy_origins(project):
    path = project / "coverage.json"
    original = json.loads(path.read_text())

    path.write_text(json.dumps({**original, "priority_policy": {"applications": []}}))
    with pytest.raises(ValueError, match="unsupported coverage document shape"):
        coverage_status(project)

    document = json.loads(json.dumps(original))
    document["items"]["check:BROKEN_PAGE_4XX"]["definition"]["priority_origin"] = "policy"
    document["items"]["check:BROKEN_PAGE_4XX"]["definitions"][-1]["definition"][
        "priority_origin"
    ] = "policy"
    path.write_text(json.dumps(document))
    with pytest.raises(ValueError, match="v1 coverage cannot contain policy"):
        coverage_status(project)


@pytest.mark.parametrize(
    "mutate",
    [
        lambda receipt: receipt.__setitem__("decisions", [{}]),
        lambda receipt: receipt.__setitem__("policy_hash", "0" * 64),
        lambda receipt: receipt["policy"]["rules"][0].__setitem__("priority", []),
    ],
)
def test_v2_rejects_malformed_saved_receipts(project, mutate):
    project_priorities(
        str(project), apply=True, expected_revision=coverage_status(project)["revision"]
    )
    path = project / "coverage.json"
    document = json.loads(path.read_text())
    mutate(document["priority_policy"]["applications"][-1]["receipt"])
    path.write_text(json.dumps(document))

    with pytest.raises(ValueError):
        coverage_status(project)


def test_changed_policy_can_return_a_policy_managed_entry_to_default(project):
    project_priorities(
        str(project), apply=True, expected_revision=coverage_status(project)["revision"]
    )

    project_priorities(
        str(project),
        policy={"format": "seohead.project-priorities.v1", "rules": []},
        apply=True,
        expected_revision=coverage_status(project)["revision"],
    )

    row = next(
        item
        for item in coverage_status(project)["items"]
        if item["id"] == "skill:workflow/js-render-check"
    )
    assert (row["priority"], row["priority_origin"], row["priority_reason"]) == (
        "P1",
        "default",
        "policy no longer matched saved facts",
    )


def test_apply_refuses_stale_revision_and_malformed_policy_without_writing(project):
    path = project / "coverage.json"
    before = path.read_bytes()
    with pytest.raises(ValueError, match="revision conflict"):
        project_priorities(str(project), full=True, apply=True, expected_revision=999)
    with pytest.raises(ValueError, match="unknown catalogue item"):
        project_priorities(
            str(project),
            {
                "format": "seohead.project-priorities.v1",
                "rules": [
                    {
                        "id": "bad",
                        "facts": {"framework": ["react"]},
                        "priority": "P0",
                        "items": ["check:UNKNOWN"],
                    }
                ],
            },
        )
    assert path.read_bytes() == before


def test_interrupted_policy_publication_preserves_v1_bytes(project, monkeypatch):
    from seohead.projects import coverage

    path = project / "coverage.json"
    before = path.read_bytes()
    monkeypatch.setattr(
        coverage.os, "replace", lambda *_args: (_ for _ in ()).throw(OSError("stop"))
    )

    with pytest.raises(OSError, match="stop"):
        project_priorities(
            str(project), apply=True, expected_revision=coverage_status(project)["revision"]
        )

    assert path.read_bytes() == before


@pytest.mark.parametrize(
    ("fact", "item_id"),
    [
        ({"name": "framework", "value": "Next"}, "skill:workflow/js-render-check"),
        ({"name": "framework", "value": "Next.js"}, "skill:workflow/js-render-check"),
        ({"name": "framework", "value": "Nextjs"}, "skill:workflow/js-render-check"),
        ({"name": "framework", "value": "Vue.js"}, "skill:workflow/js-render-check"),
        ({"name": "framework", "value": "Nuxt.js"}, "skill:workflow/js-render-check"),
        ({"name": "framework", "value": "rEaCt"}, "skill:workflow/js-render-check"),
        ({"name": "cms", "value": "WordPress"}, "skill:workflow/security-audit"),
        ({"name": "cms", "value": "WORDPRESS"}, "scenario:url-hygiene"),
        ({"name": "cms", "value": "1\u0421-Битрикс"}, "scenario:url-hygiene"),
        ({"name": "site_type", "value": "publisher"}, "check:SITEMAP_DESYNC"),
    ],
)
def test_packaged_policy_uses_saved_php_cms_and_publisher_facts(tmp_path, fact, item_id):
    root = tmp_path / item_id.replace(":", "-").replace("/", "-")
    create_project(
        root,
        "https://example.test/",
        facts=[{**fact, "provenance": "operator", "observed_at": None}],
    )
    initialize_coverage(root)

    decision = next(
        item for item in project_priorities(str(root), full=True)["decisions"] if item["id"] == item_id
    )

    assert decision["after"] == {"priority": "P0", "priority_origin": "policy"}


def test_prepared_project_accepts_new_task_without_rebinding_historical_priority_scope(tmp_path):
    import subprocess
    import sys
    from pathlib import Path

    from seohead.projects.observer import checklist_page, observe, task_detail
    from seohead.projects.workspace import project_status
    from tests.test_project_observer_sites import _prepare_with_competitors

    root = tmp_path / "project"
    _prepare_with_competitors(root)
    initialize_coverage(root)
    saved = json.loads((root / "coverage.json").read_text())
    historical = saved["priority_policy"]["applications"]
    assert historical
    updated = update_item(
        root,
        {"id": "custom:observe", "title": "Observe prepared project", "order": 999},
        coverage_status(root)["revision"],
    )
    item = next(row for row in updated["items"] if row["id"] == "custom:observe")
    assert item["priority"] == "P1" and item["priority_origin"] == "default"
    assert item["attempt_status"] == "not_run" and not item["complete"]
    assert (
        json.loads((root / "coverage.json").read_text())["priority_policy"]["applications"]
        == historical
    )
    assert project_status(root)["checklist"]["state"] == "initialized"
    assert checklist_page(root, query="Observe prepared project")["pagination"]["total"] == 1
    assert (
        task_detail(root, item_id="custom:observe")["item"]["title"] == "Observe prepared project"
    )
    assert any(
        row["id"].endswith("custom:observe") for row in observe(str(root))["active_tasks"]["items"]
    )
    script = "from seohead.projects.coverage import coverage_status; import json,sys; print(json.dumps(coverage_status(sys.argv[1])['state']))"
    reopened = subprocess.check_output(
        [sys.executable, "-c", script, str(root)],
        cwd=Path(__file__).resolve().parents[1],
        text=True,
    )
    assert json.loads(reopened) == "initialized"


def test_legacy_priority_receipt_stays_readable_after_addition(project):
    project_priorities(
        str(project), apply=True, expected_revision=coverage_status(project)["revision"]
    )
    path = project / "coverage.json"
    saved = json.loads(path.read_text())
    for application in saved["priority_policy"]["applications"]:
        application["receipt"].pop("item_ids", None)
    historical = saved["priority_policy"]["applications"]
    path.write_text(json.dumps(saved))
    update_item(
        project, {"id": "custom:later", "title": "Later task"}, coverage_status(project)["revision"]
    )
    assert json.loads(path.read_text())["priority_policy"]["applications"] == historical
    assert any(row["id"] == "custom:later" for row in coverage_status(project)["items"])


def test_receipt_declared_scope_detects_removed_historical_decision(project):
    from seohead.projects.coverage import _receipt_fingerprint

    project_priorities(
        str(project), apply=True, expected_revision=coverage_status(project)["revision"]
    )
    path = project / "coverage.json"
    saved = json.loads(path.read_text())
    receipt = saved["priority_policy"]["applications"][-1]["receipt"]
    receipt["decisions"].pop()
    receipt["decision_fingerprint"] = _receipt_fingerprint(receipt["decisions"])
    path.write_text(json.dumps(saved))
    with pytest.raises(ValueError, match="historical item scope"):
        coverage_status(project)


def test_transaction_rejects_invalid_candidate_before_atomic_publication(project):
    from seohead.projects.coverage import _transaction

    project_priorities(
        str(project), apply=True, expected_revision=coverage_status(project)["revision"]
    )
    path = project / "coverage.json"
    before = path.read_bytes()
    with (
        pytest.raises(ValueError, match="invalid priority policy receipt"),
        _transaction(project, coverage_status(project)["revision"]) as (_, _, document),
    ):
        document["priority_policy"]["applications"][-1]["receipt"]["policy_hash"] = "0" * 64
    assert path.read_bytes() == before
    assert coverage_status(project)["state"] == "initialized"
    assert not (project / ".coverage.lock").exists()
