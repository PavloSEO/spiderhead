"""Checklist acceptance: evidence identity, editable history and concurrent writers."""

import json
import socket
from concurrent.futures import ThreadPoolExecutor
from threading import Barrier

import pytest

from seohead.projects.coverage import (
    coverage_status,
    initialize_coverage,
    record_execution,
    update_item,
)
from seohead.projects.workspace import create_project, project_status
from tests.test_scan_reanalysis_integration import _source


@pytest.fixture
def project(tmp_path):
    root = tmp_path / "shop"
    create_project(root, "https://example.test/")
    initialize_coverage(root)
    return root


def edit(root, **item):
    return update_item(root, item, coverage_status(root)["revision"])


def record(root, item_id, **entry):
    return record_execution(root, item_id, entry, coverage_status(root)["revision"])


def row(status, item_id):
    return next(item for item in status["items"] if item["id"] == item_id)


def test_ecommerce_history_and_completion_axes(project, monkeypatch):
    scope = {
        "site": "https://example.test/",
        "template": "product",
        "urls": ["https://example.test/"],
    }
    initial = coverage_status(project)["counts"]["remaining"]
    template = {
        "format": "seohead.checklist-template.v1",
        "items": [
            {
                "id": "custom:product-auto",
                "title": "Product response check",
                "execution_kind": "automatic",
                "operation": "check:BROKEN_PAGE_4XX",
                "scope": scope,
            },
            {
                "id": "custom:product-review",
                "title": "Review product purchase flow",
                "dependencies": ["custom:product-auto"],
                "scope": scope,
            },
        ],
    }
    initialize_coverage(project, template)
    assert coverage_status(project)["counts"]["remaining"] == initial + 2
    with pytest.raises(ValueError, match="dependencies"):
        record(
            project,
            "custom:product-review",
            status="succeeded",
            reason="Reviewed purchase flow",
            reviewer="Specialist",
            signoff=True,
        )
    _source(project / "scans/source.sqlite")
    original = (project / "scans/source.sqlite").read_bytes()
    monkeypatch.setattr(socket, "create_connection", lambda *a, **k: pytest.fail("network"))
    status = record(
        project,
        "custom:product-auto",
        status="succeeded",
        reason="Saved product sample checked",
        artifact="scans/source.sqlite",
    )
    measured = row(status, "custom:product-auto")
    assert measured["complete"] and measured["measurement"]["state"] == "limited"
    assert (project / "scans/source.sqlite").read_bytes() == original
    status = record(
        project,
        "custom:product-review",
        status="succeeded",
        reason="Reviewed purchase flow",
        reviewer="Specialist",
        signoff=True,
    )
    assert status["counts"]["remaining"] == initial
    status = edit(project, id="custom:product-review", title="Review updated purchase flow")
    assert row(status, "custom:product-review")["stale"]
    assert row(status, "custom:product-review")["definition_versions"] == 2
    assert row(status, "custom:product-review")["attempts"] == 1
    assert not row(status, "custom:product-auto")["stale"]
    status = record(
        project,
        "custom:product-review",
        status="not_applicable",
        reason="Purchase flow is out of the agreed scope",
        reviewer="Specialist",
        evidence="Agreed scope change order 2026-09-30",
    )
    assert status["counts"]["remaining"] == initial
    reviewed = row(status, "custom:product-review")
    assert reviewed["applicability"] == "excluded"
    assert reviewed["exclusion"]["basis"] == {"evidence": "Agreed scope change order 2026-09-30"}
    assert status["coverage"]["audit_tasks"]["excluded"] == 1
    assert project_status(project)["checklist"] == coverage_status(project)


def test_competitor_missing_failed_and_wrong_site_stay_pending(project):
    edit(
        project,
        id="custom:competitor",
        execution_kind="automatic",
        operation="check:BROKEN_PAGE_4XX",
        scope={"site": "https://competitor.test/", "template": None, "urls": []},
    )
    for status in ("failed", "unavailable", "running"):
        result = record(
            project, "custom:competitor", status=status, reason="Competitor source pending"
        )
        assert not row(result, "custom:competitor")["complete"]
    with pytest.raises(ValueError, match="missing"):
        record(
            project,
            "custom:competitor",
            status="succeeded",
            reason="Attempt",
            artifact="scans/missing.sqlite",
        )
    _source(project / "scans/primary.sqlite")
    with pytest.raises(ValueError, match="site"):
        record(
            project,
            "custom:competitor",
            status="succeeded",
            reason="Attempt",
            artifact="scans/primary.sqlite",
        )
    assert not row(coverage_status(project), "custom:competitor")["complete"]


def test_manual_and_deliverable_review_are_distinct(project):
    edit(project, id="custom:manual")
    edit(project, id="custom:delivery", execution_kind="deliverable")
    output = project / "reports/client.md"
    output.write_text("Reviewed synthetic client deliverable")
    with pytest.raises(ValueError, match="approved"):
        record(
            project,
            "custom:delivery",
            status="succeeded",
            reason="Created file",
            artifact="reports/client.md",
            reviewer="Specialist",
        )
    with pytest.raises(ValueError, match="approved"):
        record(
            project,
            "custom:delivery",
            status="succeeded",
            reason="Signed",
            reviewer="Specialist",
            signoff=True,
        )
    result = record(
        project,
        "custom:delivery",
        status="succeeded",
        reason="Reviewed package",
        artifact="reports/client.md",
        reviewer="Specialist",
        review="approved",
    )
    assert "custom:delivery" in result["views"]["deliverable_ready"]
    record(
        project,
        "custom:manual",
        status="succeeded",
        reason="Reviewed flow",
        reviewer="Specialist",
        signoff=True,
    )
    output.write_text("Changed after review")
    status = coverage_status(project)
    assert row(status, "custom:delivery")["stale"]
    assert row(status, "custom:manual")["complete"]


def test_concurrent_conflicts_preserve_successful_write(project):
    revision = coverage_status(project)["revision"]
    barrier = Barrier(2)

    def write(n):
        barrier.wait()
        try:
            update_item(project, {"id": f"custom:writer{n}"}, revision)
            return True
        except ValueError:
            return False

    with ThreadPoolExecutor(max_workers=2) as executor:
        results = list(executor.map(write, range(2)))
    assert sum(results) == 1
    status = coverage_status(project)
    assert status["revision"] == revision + 1
    assert sum(item["id"].startswith("custom:writer") for item in status["items"]) == 1


def test_interrupted_publication_keeps_previous_bytes(project, monkeypatch):
    before = (project / "coverage.json").read_bytes()

    def interrupted(*a):
        raise OSError("simulated interrupted publication")

    monkeypatch.setattr("seohead.projects.coverage.os.replace", interrupted)
    with pytest.raises(OSError):
        edit(project, id="custom:interrupted")
    assert (project / "coverage.json").read_bytes() == before
    assert not list(project.glob(".coverage-*"))
    assert not (project / ".coverage.lock").exists()


@pytest.mark.parametrize(
    "item",
    [
        {"id": "custom:../bad"},
        {"id": "custom:x", "execution_kind": "shell"},
        {"id": "custom:x", "operation": "rm -rf"},
        {"id": "custom:x", "priority": "P3"},
        {"id": "custom:x", "enabled": 1},
        {"id": "custom:x", "dependencies": ["custom:missing"]},
        {
            "id": "custom:x",
            "scope": {
                "site": "https://example.test/",
                "template": None,
                "urls": ["https://competitor.test/"],
            },
        },
    ],
)
def test_invalid_definitions_do_not_change_file(project, item):
    before = (project / "coverage.json").read_bytes()
    with pytest.raises(ValueError):
        update_item(project, item, coverage_status(project)["revision"])
    assert (project / "coverage.json").read_bytes() == before


def test_schema_identity_and_path_refusals(project):
    path = project / "coverage.json"
    original = json.loads(path.read_text())
    for key, value in [("version", 99), ("project_uuid", "other")]:
        path.write_text(json.dumps({**original, key: value}))
        with pytest.raises(ValueError):
            coverage_status(project)
    path.write_text(json.dumps(original))
    edit(project, id="custom:manual")
    for artifact in ("../outside", "/tmp/outside", "project.json", "reports/missing"):
        with pytest.raises(ValueError):
            record(
                project,
                "custom:manual",
                status="succeeded",
                reason="Review",
                artifact=artifact,
                reviewer="Specialist",
                review="approved",
            )


def test_definition_drift_and_new_entries_are_pending(project, monkeypatch):
    from seohead.projects import coverage

    record(
        project,
        "skill:workflow/control",
        status="succeeded",
        reason="Reviewed scope",
        reviewer="Specialist",
        signoff=True,
    )
    catalogue = coverage.load_catalogue()
    catalogue["skill:workflow/control"]["definition_hash"] = "b" * 64
    monkeypatch.setattr(coverage, "load_catalogue", lambda: catalogue)
    assert row(coverage_status(project), "skill:workflow/control")["stale"]
    result = initialize_coverage(project)
    assert row(result, "skill:workflow/control")["stale"]
    assert row(result, "skill:workflow/control")["attempts"] == 1


def test_disable_and_order_do_not_erase_history(project):
    edit(project, id="custom:x")
    record(
        project,
        "custom:x",
        status="succeeded",
        reason="Reviewed",
        reviewer="Specialist",
        signoff=True,
    )
    result = edit(project, id="custom:x", enabled=False, order=999)
    assert result["counts"]["disabled"] == 1
    assert row(result, "custom:x")["attempts"] == 1
    assert "custom:x" in result["views"]["remaining"]
    assert row(result, "custom:x")["applicability"] == "pending_exclusion"
    assert "custom:x" in result["views"]["pending_exclusion"]


def test_new_catalogue_entry_is_pending_before_reconcile(project, monkeypatch):
    from seohead.projects import coverage

    catalogue = coverage.load_catalogue()
    before = coverage_status(project)["counts"]["remaining"]
    original = (project / "coverage.json").read_bytes()
    catalogue["check:SYNTHETIC_NEW"] = {
        **catalogue["check:BROKEN_PAGE_4XX"],
        "id": "check:SYNTHETIC_NEW",
    }
    monkeypatch.setattr(coverage, "load_catalogue", lambda: catalogue)
    status = coverage_status(project)
    assert status["counts"]["remaining"] == before + 1
    assert row(status, "check:SYNTHETIC_NEW")["state"] == "not_run"
    assert (project / "coverage.json").read_bytes() == original


@pytest.mark.parametrize("value", [None, [], "bad", {"status": []}])
def test_malformed_execution_refuses_without_write(project, value):
    path = project / "coverage.json"
    original = path.read_bytes()
    with pytest.raises(ValueError):
        record_execution(
            project, "check:BROKEN_PAGE_4XX", value, coverage_status(project)["revision"]
        )
    assert path.read_bytes() == original


def test_malformed_history_and_definitions_refuse(project):
    edit(project, id="custom:manual")
    record(
        project,
        "custom:manual",
        status="succeeded",
        reason="Reviewed",
        reviewer="Specialist",
        signoff=True,
    )
    path = project / "coverage.json"
    original = path.read_text()
    for mutation in ("record", "definition", "approval"):
        doc = json.loads(original)
        item = doc["items"]["custom:manual"]
        if mutation == "record":
            item["records"][-1].pop("definition_hash")
        elif mutation == "definition":
            item["definition"]["execution_kind"] = []
            item["definitions"][-1]["definition"]["execution_kind"] = []
        else:
            item["records"][-1].pop("signoff")
        path.write_text(json.dumps(doc))
        with pytest.raises(ValueError):
            coverage_status(project)
    path.write_text(original)


def test_skipped_check_and_absent_template_never_complete(project):
    _source(project / "scans/source.sqlite")
    edit(
        project,
        id="custom:absent",
        execution_kind="automatic",
        operation="check:BROKEN_PAGE_4XX",
        scope={"site": "https://example.test/", "template": "product", "urls": []},
    )
    with pytest.raises(ValueError, match="sample URLs"):
        record(
            project,
            "custom:absent",
            status="succeeded",
            reason="Try source",
            artifact="scans/source.sqlite",
        )
    from seohead.storage import open_scan

    with open_scan(project / "scans/source.sqlite") as con:
        audit = json.loads(con.execute("SELECT document_json FROM audit").fetchone()[0])
    skipped = audit["run"]["checks_skipped"][0]["id"]
    with pytest.raises(ValueError, match="does not prove"):
        record(
            project,
            "check:" + skipped,
            status="succeeded",
            reason="Try source",
            artifact="scans/source.sqlite",
        )


@pytest.mark.parametrize("identifier", [[], {}, None])
def test_malformed_update_identifier_refuses_cleanly(project, identifier):
    before = (project / "coverage.json").read_bytes()
    with pytest.raises(ValueError):
        update_item(project, {"id": identifier}, coverage_status(project)["revision"])
    assert (project / "coverage.json").read_bytes() == before


def test_removed_builtin_can_be_explicitly_disabled_without_losing_history(project, monkeypatch):
    from seohead.projects import coverage

    item_id = "skill:workflow/control"
    record(
        project, item_id, status="succeeded", reason="Reviewed", reviewer="Specialist", signoff=True
    )
    catalogue = coverage.load_catalogue()
    del catalogue[item_id]
    monkeypatch.setattr(coverage, "load_catalogue", lambda: catalogue)
    status = edit(project, id=item_id, enabled=False)
    assert row(status, item_id)["attempts"] == 1
    assert row(status, item_id)["applicability"] == "pending_exclusion"
    assert item_id in status["views"]["pending_exclusion"]
    assert status["counts"]["disabled"] == 1


def test_passive_observer_does_not_rehash_large_retained_coverage_artifact(project, monkeypatch):
    from seohead.projects import evidence as evidence_core
    from seohead.projects.observer import checklist_page, observe, task_detail

    edit(project, id="custom:large-report", title="Review retained large report")
    path = project / "reports" / "large-report.txt"
    with path.open("wb") as stream:
        for _ in range(32):
            stream.write(b"x" * (1024 * 1024))
    assert path.stat().st_size == 32 * 1024 * 1024
    saved = record(
        project,
        "custom:large-report",
        status="succeeded",
        reason="Reviewed saved report",
        reviewer="Synthetic reviewer",
        review="approved",
        artifact="reports/large-report.txt",
    )
    assert row(saved, "custom:large-report")["complete"]
    coverage_before = (project / "coverage.json").read_bytes()
    monkeypatch.setattr(
        evidence_core,
        "_digest",
        lambda *args, **kwargs: pytest.fail("passive observer hashed artifact bytes"),
    )
    snapshot = observe(str(project))
    assert snapshot["progress"]["state_counts"]["completed"] >= 1
    listed = checklist_page(project, query="large report")["items"][0]
    assert listed["complete"] and listed["evidence_verification"]["state"] == "metadata_matches"
    assert task_detail(project, item_id="custom:large-report")["item"]["complete"]
    assert (project / "coverage.json").read_bytes() == coverage_before
    with path.open("ab") as stream:
        stream.write(b"changed")
    changed = checklist_page(project, query="large report")["items"][0]
    assert not changed["complete"] and changed["stale"]
    assert changed["evidence_verification"]["state"] == "stale"
    path.rename(path.with_suffix(".moved"))
    missing = task_detail(project, item_id="custom:large-report")["item"]
    assert not missing["complete"] and "missing" in missing["reason"]


def test_legacy_coverage_receipt_is_unverified_only_in_passive_mode(project):
    edit(project, id="custom:review", title="Review existing report")
    (project / "reports" / "review.md").write_text("Synthetic reviewed evidence")
    record(
        project,
        "custom:review",
        status="succeeded",
        reason="Reviewed saved report",
        reviewer="Synthetic reviewer",
        review="approved",
        artifact="reports/review.md",
    )
    path = project / "coverage.json"
    document = json.loads(path.read_text())
    del document["items"]["custom:review"]["records"][-1]["artifact_verification"]
    path.write_text(json.dumps(document))
    before = path.read_bytes()
    passive = coverage_status(project, _verify_evidence=False)
    assert not row(passive, "custom:review")["complete"]
    assert row(passive, "custom:review")["evidence_verification"]["state"] == "unverified"
    assert row(coverage_status(project), "custom:review")["complete"]
    assert path.read_bytes() == before


def test_passive_coverage_rejects_symlink_replacement_without_reading_it(
    project, tmp_path, monkeypatch
):
    from seohead.projects import evidence as evidence_core

    edit(project, id="custom:review", title="Review existing report")
    artifact = project / "reports" / "review.md"
    artifact.write_text("Synthetic reviewed evidence")
    record(
        project,
        "custom:review",
        status="succeeded",
        reason="Reviewed saved report",
        reviewer="Synthetic reviewer",
        review="approved",
        artifact="reports/review.md",
    )
    artifact.rename(artifact.with_suffix(".saved"))
    outside = tmp_path / "outside.md"
    outside.write_text("Outside project")
    artifact.symlink_to(outside)
    monkeypatch.setattr(
        evidence_core, "_digest", lambda *args, **kwargs: pytest.fail("outside artifact read")
    )
    result = row(coverage_status(project, _verify_evidence=False), "custom:review")
    assert result["stale"] and not result["complete"]


def test_explicit_coverage_still_rehashes_changed_receipt(project, monkeypatch):
    from seohead.projects import evidence as evidence_core

    edit(project, id="custom:review", title="Review existing report")
    artifact = project / "reports" / "review.md"
    artifact.write_text("Synthetic reviewed evidence")
    record(
        project,
        "custom:review",
        status="succeeded",
        reason="Reviewed saved report",
        reviewer="Synthetic reviewer",
        review="approved",
        artifact="reports/review.md",
    )
    artifact.write_text("Different retained evidence")
    calls = []
    original = evidence_core._digest

    def counted(path, **kwargs):
        calls.append(path)
        return original(path, **kwargs)

    monkeypatch.setattr(evidence_core, "_digest", counted)
    result = row(coverage_status(project), "custom:review")
    assert calls == [artifact]
    assert result["stale"] and not result["complete"]


def test_scoped_competitor_task_detail_never_rehashes_any_site_artifact(tmp_path, monkeypatch):
    from seohead.projects import evidence as evidence_core
    from seohead.projects.observer import task_detail
    from tests.test_project_observer_sites import _prepare_with_competitors

    root = tmp_path / "owner"
    prepared = _prepare_with_competitors(root)["preparation"]
    child = root / prepared["competitors"][0]["directory"]
    for site in (root, child):
        edit(site, id="custom:review", title="Review retained report")
        (site / "reports" / "review.md").write_text("Synthetic retained report")
        record(
            site,
            "custom:review",
            status="succeeded",
            reason="Reviewed",
            reviewer="Synthetic reviewer",
            review="approved",
            artifact="reports/review.md",
        )
    monkeypatch.setattr(
        evidence_core,
        "_digest",
        lambda *args, **kwargs: pytest.fail("scoped detail hashed a site artifact"),
    )
    identifier = "site:" + prepared["competitors"][0]["project_uuid"] + "/custom:review"
    detail = task_detail(root, item_id=identifier)
    assert detail["item"]["complete"]
    assert detail["item"]["evidence_verification"]["state"] == "metadata_matches"


def test_agent_review_leaves_a_deliverable_waiting_for_owner_completion(project):
    edit(project, id="custom:delivery", execution_kind="deliverable")
    (project / "reports/client.md").write_text("Reviewed synthetic client deliverable")
    result = record(
        project,
        "custom:delivery",
        status="succeeded",
        reason="Agent reviewed package",
        artifact="reports/client.md",
        reviewer="Claude Code",
        review="approved",
    )
    assert row(result, "custom:delivery")["state"] == "run"
    assert not row(result, "custom:delivery")["complete"]
    assert "custom:delivery" in result["views"]["waiting_for_manual_review"]
    assert "custom:delivery" not in result["views"]["deliverable_ready"]
    owner = record(
        project,
        "custom:delivery",
        status="succeeded",
        reason="Owner approved package",
        artifact="reports/client.md",
        reviewer="Pavel",
        review="approved",
    )
    assert row(owner, "custom:delivery")["complete"]
