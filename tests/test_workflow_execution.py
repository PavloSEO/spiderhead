from __future__ import annotations

import asyncio
import contextlib
import hashlib
import io
import json

import pytest

from seohead.projects.coverage import coverage_status, initialize_coverage, update_item
from seohead.projects.execution import checkpoint, execute, resume, start, status
from seohead.projects.inbox import set_goal_state, submit
from seohead.projects.workspace import create_project


def _evidence(project, reference: str) -> list[dict[str, str]]:
    path = project / reference
    if not path.exists():
        path.write_bytes(b'{"source":"synthetic workflow evidence"}')
    return [{"reference": reference, "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}]


def _accepted_context(project) -> dict[str, object]:
    task_id = "custom:controller-worklist"
    current = coverage_status(project)
    if not any(row["id"] == task_id for row in current["items"]):
        update_item(
            project,
            {"id": task_id, "title": "Synthetic controller worklist"},
            expected_revision=current["revision"],
        )
    proposed = submit(
        project,
        text="Complete the recorded synthetic full audit.",
        kind="proposed_goal",
        references=["goal:synthetic-full-audit", "task:handoff"],
    )
    set_goal_state(
        project,
        entry_id=proposed["entry"]["id"],
        state="accepted",
        expected_revision=proposed["revision"],
    )
    return {
        "goal_id": proposed["entry"]["id"],
        "prompt_reference": "skill:workflow/full-audit-v1",
        "task_ids": [task_id],
        "competitors": ["https://competitor.test/"],
    }


def _cli_json(*argv: str) -> dict:
    from seohead.cli import main

    output = io.StringIO()
    with contextlib.redirect_stdout(output):
        assert main(list(argv)) == 0
    return json.loads(output.getvalue())


def test_registered_steps_checkpoint_and_resume(tmp_path):
    project = tmp_path / "project"
    create_project(project, "https://example.test/")
    initialize_coverage(project)
    rows = status(project)
    assert rows["next_action"] is None
    steps = ["check:TITLE_MISSING", "check:DESC_MISSING"]
    run = start(
        project,
        scenario_id="scenario:full-audit",
        steps=steps,
        expected_revision=0,
        context=_accepted_context(project),
    )
    first = checkpoint(
        project,
        run_id=run["run"]["id"],
        step_id=steps[0],
        state="succeeded",
        evidence=_evidence(project, "reports/title.json"),
        expected_revision=run["revision"],
    )
    assert status(project)["next_action"] == steps[1]
    done = checkpoint(
        project,
        run_id=run["run"]["id"],
        step_id=steps[1],
        state="succeeded",
        evidence=_evidence(project, "reports/description.json"),
        expected_revision=first["revision"],
    )
    assert done["run"]["state"] == "completed"


def test_execution_status_is_readable_before_checklist_initialization(tmp_path):
    project = tmp_path / "project"
    create_project(project, "https://example.test/")
    before = (project / "project.json").read_bytes()
    result = status(project)
    assert result["runs"] == []
    assert result["next_action"] is None
    assert (project / "project.json").read_bytes() == before


def test_local_executor_records_each_registered_outcome(tmp_path):
    project = tmp_path / "project"
    create_project(project, "https://example.test/")
    initialize_coverage(project)
    steps = ["check:TITLE_MISSING"]
    result = execute(
        project,
        scenario_id="scenario:full-audit",
        steps=steps,
        outcomes=[
            {
                "id": steps[0],
                "state": "succeeded",
                "evidence": _evidence(project, "scans/scan.sqlite"),
            }
        ],
        context=_accepted_context(project),
    )
    assert result["run"]["state"] == "completed"


def test_second_session_resumes_only_the_interrupted_registered_step(tmp_path):
    project = tmp_path / "project"
    create_project(project, "https://example.test/")
    initialize_coverage(project)
    steps = ["check:TITLE_MISSING", "check:DESC_MISSING"]
    started = start(
        project,
        scenario_id="scenario:full-audit",
        steps=steps,
        context=_accepted_context(project),
    )
    first = checkpoint(
        project,
        run_id=started["run"]["id"],
        step_id=steps[0],
        state="succeeded",
        evidence=_evidence(project, "reports/own-site.json"),
        phase="own-site-capture",
        expected_revision=started["revision"],
    )
    interrupted = checkpoint(
        project,
        run_id=started["run"]["id"],
        step_id=steps[1],
        state="interrupted",
        expected_revision=first["revision"],
    )
    recovered = resume(
        project, run_id=started["run"]["id"], expected_revision=interrupted["revision"]
    )
    assert recovered["next_action"] == steps[1]
    assert recovered["run"]["steps"][0]["state"] == "succeeded"
    assert recovered["run"]["context"]["own_site"] == "https://example.test/"
    assert recovered["run"]["context"]["goal"]["references"] == [
        "goal:synthetic-full-audit",
        "task:handoff",
    ]
    assert recovered["run"]["context"]["prompt"]["id"] == "skill:workflow/full-audit-v1"
    assert recovered["run"]["phase"] == "own-site-capture"


def test_manual_step_needs_an_explicit_review_before_completion(tmp_path):
    project = tmp_path / "project"
    create_project(project, "https://example.test/")
    initialize_coverage(project)
    started = start(
        project,
        scenario_id="scenario:full-audit",
        steps=["skill:workflow/control"],
        context=_accepted_context(project),
    )
    with pytest.raises(ValueError, match="explicit approved review"):
        checkpoint(
            project,
            run_id=started["run"]["id"],
            step_id="skill:workflow/control",
            state="succeeded",
            evidence=_evidence(project, "reports/review.md"),
            expected_revision=started["revision"],
        )


def test_deliverable_step_needs_an_explicit_review_before_completion(tmp_path):
    project = tmp_path / "project"
    create_project(project, "https://example.test/")
    initialize_coverage(
        project,
        template={
            "format": "seohead.checklist-template.v1",
            "items": [
                {
                    "id": "custom:deliverable",
                    "title": "Synthetic approved report",
                    "execution_kind": "deliverable",
                }
            ],
        },
    )
    started = start(
        project,
        scenario_id="scenario:full-audit",
        steps=["custom:deliverable"],
        context=_accepted_context(project),
    )
    with pytest.raises(ValueError, match="explicit approved review"):
        checkpoint(
            project,
            run_id=started["run"]["id"],
            step_id="custom:deliverable",
            state="succeeded",
            evidence=_evidence(project, "reports/deliverable.pdf"),
            expected_revision=started["revision"],
        )


def test_successful_checkpoint_requires_exact_evidence(tmp_path):
    project = tmp_path / "project"
    create_project(project, "https://example.test/")
    initialize_coverage(project)
    started = start(
        project,
        scenario_id="scenario:full-audit",
        steps=["check:TITLE_MISSING"],
        context=_accepted_context(project),
    )
    with pytest.raises(ValueError, match="exact evidence"):
        checkpoint(
            project,
            run_id=started["run"]["id"],
            step_id="check:TITLE_MISSING",
            state="succeeded",
            expected_revision=started["revision"],
        )


def test_start_refuses_duplicate_registered_steps(tmp_path):
    project = tmp_path / "project"
    create_project(project, "https://example.test/")
    initialize_coverage(project)
    with pytest.raises(ValueError, match="unique nonempty"):
        start(
            project,
            scenario_id="scenario:full-audit",
            steps=["check:TITLE_MISSING", "check:TITLE_MISSING"],
            context=_accepted_context(project),
        )


def test_start_requires_an_accepted_goal_and_registered_prompt(tmp_path):
    project = tmp_path / "project"
    create_project(project, "https://example.test/")
    initialize_coverage(project)
    with pytest.raises(ValueError, match="accepted goal_id, prompt_reference, and task_ids"):
        start(
            project,
            scenario_id="scenario:full-audit",
            steps=["check:TITLE_MISSING"],
        )


def test_start_requires_current_incomplete_custom_tasks(tmp_path):
    project = tmp_path / "project"
    create_project(project, "https://example.test/")
    initialize_coverage(project)
    context = _accepted_context(project)
    context["task_ids"] = ["custom:missing-worklist"]
    with pytest.raises(ValueError, match="current incomplete custom checklist tasks"):
        start(
            project,
            scenario_id="scenario:full-audit",
            steps=["check:TITLE_MISSING"],
            context=context,
        )


def test_stale_prompt_reopens_the_earliest_step_without_losing_prior_evidence(tmp_path):
    project = tmp_path / "project"
    create_project(project, "https://example.test/")
    initialize_coverage(project)
    started = start(
        project,
        scenario_id="scenario:full-audit",
        steps=["check:TITLE_MISSING", "check:DESC_MISSING"],
        context=_accepted_context(project),
    )
    first = checkpoint(
        project,
        run_id=started["run"]["id"],
        step_id="check:TITLE_MISSING",
        state="succeeded",
        evidence=_evidence(project, "reports/title-before.json"),
        expected_revision=started["revision"],
    )
    interrupted = checkpoint(
        project,
        run_id=started["run"]["id"],
        step_id="check:DESC_MISSING",
        state="interrupted",
        expected_revision=first["revision"],
    )
    path = project / "execution.json"
    saved = json.loads(path.read_text(encoding="utf-8"))
    saved["runs"][0]["context"]["prompt"]["definition_hash"] = "0" * 64
    path.write_text(json.dumps(saved), encoding="utf-8")
    pending = status(project)
    assert pending["runs"][0]["state"] == "stale"
    assert pending["next_action"] == "check:TITLE_MISSING"
    recovered = resume(
        project,
        run_id=started["run"]["id"],
        expected_revision=interrupted["revision"],
    )
    assert recovered["run"]["state"] == "running"
    assert recovered["next_action"] == "check:TITLE_MISSING"
    assert recovered["run"]["steps"][0]["stale_evidence"] == _evidence(
        project, "reports/title-before.json"
    )
    assert recovered["run"]["steps"][1]["state"] == "pending"


def test_stale_checklist_dependency_requires_checklist_reconciliation(tmp_path):
    project = tmp_path / "project"
    create_project(project, "https://example.test/")
    initialize_coverage(project)
    started = start(
        project,
        scenario_id="scenario:full-audit",
        steps=["check:TITLE_MISSING"],
        context=_accepted_context(project),
    )
    interrupted = checkpoint(
        project,
        run_id=started["run"]["id"],
        step_id="check:TITLE_MISSING",
        state="interrupted",
        expected_revision=started["revision"],
    )
    path = project / "coverage.json"
    saved = json.loads(path.read_text(encoding="utf-8"))
    definition = saved["items"]["check:TITLE_MISSING"]["definition"]
    definition["source_hash"] = "0" * 64
    saved["items"]["check:TITLE_MISSING"]["definitions"][-1]["definition"] = definition
    path.write_text(json.dumps(saved), encoding="utf-8")
    assert status(project)["runs"][0]["state"] == "stale"
    with pytest.raises(ValueError, match="reconcile stale checklist dependencies"):
        resume(
            project,
            run_id=started["run"]["id"],
            expected_revision=interrupted["revision"],
        )


def test_two_agent_cli_and_mcp_handoff_persists_goal_prompt_versions_and_evidence(tmp_path):
    pytest.importorskip("mcp")
    from seohead.mcp.mcp_server import build_server

    project = tmp_path / "project"
    create_project(project, "https://example.test/")
    initialize_coverage(project)
    context = _accepted_context(project)
    steps = ["check:TITLE_MISSING", "check:DESC_MISSING"]
    agent_one = _cli_json(
        "workflow-start",
        "--directory",
        str(project),
        "--input",
        json.dumps({"scenario_id": "scenario:full-audit", "steps": steps, "context": context}),
    )
    server = build_server(profile="full")
    agent_two_status = asyncio.run(
        server.call_tool("seo_workflow_status", {"directory": str(project)})
    )[1]
    assert agent_two_status["next_action"] == steps[0]
    agent_two = asyncio.run(
        server.call_tool(
            "seo_workflow_checkpoint",
            {
                "directory": str(project),
                "run_id": agent_one["run"]["id"],
                "step_id": steps[0],
                "state": "succeeded",
                "evidence": _evidence(project, "reports/title-agent-two.json"),
                "phase": "own-site-capture",
                "expected_revision": agent_one["revision"],
            },
        )
    )[1]
    interrupted = _cli_json(
        "workflow-checkpoint",
        "--directory",
        str(project),
        "--input",
        json.dumps(
            {
                "run_id": agent_one["run"]["id"],
                "step_id": steps[1],
                "state": "interrupted",
                "expected_revision": agent_two["revision"],
            }
        ),
    )
    resumed = asyncio.run(
        server.call_tool(
            "seo_workflow_resume",
            {
                "directory": str(project),
                "run_id": agent_one["run"]["id"],
                "expected_revision": interrupted["revision"],
            },
        )
    )[1]
    assert resumed["next_action"] == steps[1]
    assert resumed["run"]["steps"][0]["state"] == "succeeded"
    assert resumed["run"]["steps"][0]["evidence"] == _evidence(
        project, "reports/title-agent-two.json"
    )
    assert resumed["run"]["context"]["goal"]["id"] == context["goal_id"]
    assert resumed["run"]["context"]["prompt"]["definition_hash"]
    assert resumed["run"]["scenario"]["definition_hash"]


def _one_step_run(tmp_path, *, step="check:TITLE_MISSING"):
    project = tmp_path / "project"
    create_project(project, "https://example.test/")
    initialize_coverage(project)
    result = start(
        project, scenario_id="scenario:full-audit", steps=[step], context=_accepted_context(project)
    )
    return project, result


@pytest.mark.parametrize(
    "reference", ["reports/missing.json", "../other/report.json", "/etc/passwd", "project.json"]
)
def test_success_refuses_missing_or_outside_evidence_without_changing_checkpoint(
    tmp_path, reference
):
    project, started = _one_step_run(tmp_path)
    before = (project / "execution.json").read_bytes()
    with pytest.raises(ValueError, match=r"evidence|artifact"):
        checkpoint(
            project,
            run_id=started["run"]["id"],
            step_id="check:TITLE_MISSING",
            state="succeeded",
            evidence=[{"reference": reference, "sha256": "a" * 64}],
            expected_revision=started["revision"],
        )
    assert (project / "execution.json").read_bytes() == before
    assert status(project)["runs"][0]["state"] == "running"


def test_success_rejects_mismatched_digest_and_symlink_target_before_read(tmp_path, monkeypatch):
    from seohead.projects import evidence as evidence_core

    project, started = _one_step_run(tmp_path)
    record = _evidence(project, "reports/measured.json")
    record[0]["sha256"] = "0" * 64
    with pytest.raises(ValueError, match="does not match"):
        checkpoint(
            project,
            run_id=started["run"]["id"],
            step_id="check:TITLE_MISSING",
            state="succeeded",
            evidence=record,
            expected_revision=started["revision"],
        )
    outside = tmp_path / "outside.json"
    outside.write_bytes(b"outside")
    (project / "reports" / "linked.json").symlink_to(outside)
    monkeypatch.setattr(
        evidence_core, "_digest", lambda *a, **k: pytest.fail("outside file was opened")
    )
    with pytest.raises(ValueError, match="symlinks"):
        checkpoint(
            project,
            run_id=started["run"]["id"],
            step_id="check:TITLE_MISSING",
            state="succeeded",
            evidence=[
                {
                    "reference": "reports/linked.json",
                    "sha256": hashlib.sha256(b"outside").hexdigest(),
                }
            ],
            expected_revision=started["revision"],
        )


@pytest.mark.parametrize("change", ["missing", "changed"])
def test_completed_evidence_becomes_stale_and_resume_preserves_old_receipt(tmp_path, change):
    project, started = _one_step_run(tmp_path)
    records = _evidence(project, "reports/checked.json")
    completed = checkpoint(
        project,
        run_id=started["run"]["id"],
        step_id="check:TITLE_MISSING",
        state="succeeded",
        evidence=records,
        expected_revision=started["revision"],
    )
    path = project / records[0]["reference"]
    if change == "missing":
        path.rename(path.with_suffix(".moved"))
    else:
        path.write_bytes(b"different measured bytes")
    before = (project / "execution.json").read_bytes()
    current = status(project)["runs"][0]
    assert current["state"] == "stale" and current["recorded_state"] == "completed"
    assert "retained evidence unavailable" in current["stale_dependencies"][0]["reason"]
    assert (project / "execution.json").read_bytes() == before
    recovered = resume(
        project, run_id=started["run"]["id"], expected_revision=completed["revision"]
    )
    step = recovered["run"]["steps"][0]
    assert step["state"] == "pending" and step["stale_evidence"] == records
    assert recovered["run"]["state"] == "running"


def test_manual_approval_does_not_authorize_phantom_artifact(tmp_path):
    project, started = _one_step_run(tmp_path, step="skill:workflow/control")
    with pytest.raises(ValueError, match="missing or unsafe"):
        checkpoint(
            project,
            run_id=started["run"]["id"],
            step_id="skill:workflow/control",
            state="succeeded",
            evidence=[{"reference": "reports/phantom.md", "sha256": "a" * 64}],
            review={"actor": "Synthetic reviewer", "state": "approved", "reason": "Reviewed"},
            expected_revision=started["revision"],
        )


@pytest.mark.parametrize("state", ["failed", "unavailable", "skipped", "interrupted"])
def test_non_success_checkpoint_keeps_missing_evidence_semantics(tmp_path, state):
    project, started = _one_step_run(tmp_path)
    result = checkpoint(
        project,
        run_id=started["run"]["id"],
        step_id="check:TITLE_MISSING",
        state=state,
        evidence=[{"reference": "reports/missing.json", "sha256": "a" * 64}],
        expected_revision=started["revision"],
    )
    assert result["run"]["steps"][0]["state"] == state
    assert result["run"]["state"] == ("interrupted" if state == "interrupted" else "blocked")


def test_verification_budget_cannot_accept_or_reset_unverified_evidence(tmp_path, monkeypatch):

    project, started = _one_step_run(tmp_path)
    records = _evidence(project, "reports/checked.json")
    completed = checkpoint(
        project,
        run_id=started["run"]["id"],
        step_id="check:TITLE_MISSING",
        state="succeeded",
        evidence=records,
        expected_revision=started["revision"],
    )
    before = (project / "execution.json").read_bytes()
    from seohead.projects.runtime import project_policy

    policy = project_policy(str(project))["policy"]
    policy["evidence_hash"]["max_bytes"] = 1
    project_policy(str(project), policy=policy, apply=True, expected_revision=0)
    result = status(project)
    assert result["runs"][0]["state"] == "stale"
    assert "byte budget" in result["runs"][0]["stale_dependencies"][0]["reason"]
    with pytest.raises(ValueError, match="verification budget"):
        resume(project, run_id=started["run"]["id"], expected_revision=completed["revision"])
    assert (project / "execution.json").read_bytes() == before


def test_artifact_mutating_during_hash_cannot_complete(tmp_path, monkeypatch):
    from seohead.projects import evidence as evidence_core

    project, started = _one_step_run(tmp_path)
    records = _evidence(project, "reports/checked.json")
    original = evidence_core._digest

    def mutate(path, **kwargs):
        digest = original(path, **kwargs)
        path.write_bytes(b"replaced during read")
        return digest

    monkeypatch.setattr(evidence_core, "_digest", mutate)
    with pytest.raises(ValueError, match="changed while hashing"):
        checkpoint(
            project,
            run_id=started["run"]["id"],
            step_id="check:TITLE_MISSING",
            state="succeeded",
            evidence=records,
            expected_revision=started["revision"],
        )
    assert status(project)["runs"][0]["state"] == "running"


def test_streamed_evidence_hash_deadline_and_reuse_are_explicit(tmp_path, monkeypatch):
    import time

    from seohead.projects import evidence as evidence_core
    from seohead.projects import execution

    project, _started = _one_step_run(tmp_path)
    records = _evidence(project, "reports/checked.json")
    path = project / records[0]["reference"]
    with pytest.raises(ValueError, match="time budget"):
        evidence_core._digest(path, deadline=time.monotonic() - 1)
    calls = []
    original = evidence_core._digest

    def counted(path, **kwargs):
        calls.append(path)
        return original(path, **kwargs)

    monkeypatch.setattr(evidence_core, "_digest", counted)
    digests = {}
    execution._evidence(records, root=project, digests=digests)
    execution._evidence(records, root=project, digests=digests)
    assert len(calls) == 1
    path.write_bytes(b"replaced")
    with pytest.raises(ValueError, match="does not match"):
        execution._evidence(records, root=project, digests=digests)
    assert len(calls) == 2


def test_success_checkpoint_reuses_one_hash_for_the_response_projection(tmp_path, monkeypatch):
    from seohead.projects import evidence as evidence_core

    project, started = _one_step_run(tmp_path)
    records = _evidence(project, "reports/checked.json")
    original = evidence_core._digest
    deadlines = []

    def counted(path, **kwargs):
        deadlines.append(kwargs["deadline"])
        return original(path, **kwargs)

    monkeypatch.setattr(evidence_core, "_digest", counted)
    result = checkpoint(
        project,
        run_id=started["run"]["id"],
        step_id="check:TITLE_MISSING",
        state="succeeded",
        evidence=records,
        expected_revision=started["revision"],
    )
    assert result["run"]["state"] == "completed"
    assert len(deadlines) == 1


def test_passive_observer_only_checks_retained_verification_metadata(tmp_path, monkeypatch):
    from seohead.projects import evidence as evidence_core
    from seohead.projects.observer import observe

    project, started = _one_step_run(tmp_path)
    records = _evidence(project, "reports/checked.json")
    checkpoint(
        project,
        run_id=started["run"]["id"],
        step_id="check:TITLE_MISSING",
        state="succeeded",
        evidence=records,
        expected_revision=started["revision"],
    )
    before = (project / "execution.json").read_bytes()
    monkeypatch.setattr(
        evidence_core, "_digest", lambda *a, **k: pytest.fail("passive workflow rehashed evidence")
    )
    run = observe(str(project))["execution"]["runs"][0]
    assert run["state"] == "completed"
    assert run["evidence_verification_mode"] == "metadata_only"
    assert run["steps"][0]["evidence_status"]["state"] == "metadata_matches"
    assert (project / "execution.json").read_bytes() == before
    (project / records[0]["reference"]).write_bytes(b"changed retained evidence")
    changed = observe(str(project))["execution"]["runs"][0]
    assert changed["state"] == "stale"
    assert changed["steps"][0]["evidence_status"]["state"] == "stale"


def test_legacy_success_without_receipt_is_unverified_in_passive_view(tmp_path):
    from seohead.projects.observer import observe

    project, started = _one_step_run(tmp_path)
    records = _evidence(project, "reports/checked.json")
    checkpoint(
        project,
        run_id=started["run"]["id"],
        step_id="check:TITLE_MISSING",
        state="succeeded",
        evidence=records,
        expected_revision=started["revision"],
    )
    path = project / "execution.json"
    saved = json.loads(path.read_text())
    del saved["runs"][0]["steps"][0]["evidence_verification"]
    path.write_text(json.dumps(saved))
    passive = observe(str(project))["execution"]["runs"][0]
    assert passive["state"] == "stale"
    assert passive["steps"][0]["evidence_status"]["state"] == "unverified"
    explicit = status(project)["runs"][0]
    assert explicit["state"] == "completed" and explicit["evidence_verification_mode"] == "bytes"
    assert explicit["steps"][0]["evidence_status"]["state"] == "verified"


def test_old_project_policy_defaults_are_normalized_without_rewriting(tmp_path):
    from seohead.projects.runtime import project_policy

    project, _started = _one_step_run(tmp_path)
    old = project_policy(str(project))["policy"]
    del old["evidence_hash"]
    project_policy(str(project), policy=old, apply=True, expected_revision=0)
    path = project / "crawl-policy.json"
    saved = json.loads(path.read_text())
    del saved["policy"]["evidence_hash"]
    path.write_text(json.dumps(saved))
    before = path.read_bytes()
    assert project_policy(str(project))["policy"]["evidence_hash"] == {
        "max_bytes": 1073741824,
        "max_seconds": 5,
    }
    assert path.read_bytes() == before


def test_explicit_large_hash_policy_reaches_verifier_without_large_fixture(tmp_path, monkeypatch):
    from seohead.projects import evidence as evidence_core
    from seohead.projects.runtime import project_policy

    project, started = _one_step_run(tmp_path)
    records = _evidence(project, "reports/checked.json")
    policy = project_policy(str(project))["policy"]
    policy["evidence_hash"] = {"max_bytes": 32 * 1024**3, "max_seconds": 120}
    project_policy(str(project), policy=policy, apply=True, expected_revision=0)
    original = evidence_core._digest
    budgets = []

    def measured(path, **kwargs):
        budgets.append(kwargs["max_bytes"])
        return original(path, **kwargs)

    monkeypatch.setattr(evidence_core, "_digest", measured)
    result = checkpoint(
        project,
        run_id=started["run"]["id"],
        step_id="check:TITLE_MISSING",
        state="succeeded",
        evidence=records,
        expected_revision=started["revision"],
    )
    assert result["run"]["state"] == "completed"
    assert budgets == [32 * 1024**3]


@pytest.mark.parametrize(
    "key,value",
    [
        ("max_bytes", 0),
        ("max_bytes", 64 * 1024**3 + 1),
        ("max_bytes", True),
        ("max_seconds", 301),
        ("max_seconds", 0),
    ],
)
def test_hash_policy_rejects_unbounded_or_ambiguous_limits(tmp_path, key, value):
    from seohead.projects.runtime import project_policy

    project, _started = _one_step_run(tmp_path)
    policy = project_policy(str(project))["policy"]
    policy["evidence_hash"][key] = value
    with pytest.raises(ValueError, match="evidence_hash"):
        project_policy(str(project), policy=policy, apply=True, expected_revision=0)


def test_blocked_run_lets_a_later_step_skip_a_failed_earlier_step(tmp_path):
    from seohead.projects.catalogue import load_catalogue

    project = tmp_path / "project"
    create_project(project, "https://example.test/")
    initialize_coverage(project)
    first, second = [item for item, entry in load_catalogue().items() if entry["kind"] == "check"][
        :2
    ]
    started = start(
        project,
        scenario_id="scenario:full-audit",
        steps=[first, second],
        context=_accepted_context(project),
    )
    run_id = started["run"]["id"]
    failed = checkpoint(
        project,
        run_id=run_id,
        step_id=first,
        state="failed",
        evidence=[],
        expected_revision=started["revision"],
    )
    assert failed["run"]["state"] == "blocked"
    skipped = checkpoint(
        project,
        run_id=run_id,
        step_id=second,
        state="skipped",
        evidence=[],
        expected_revision=failed["revision"],
    )
    assert [item["state"] for item in skipped["run"]["steps"]] == ["failed", "skipped"]
    assert skipped["run"]["state"] == "blocked"


def test_later_step_still_waits_for_a_pending_earlier_step(tmp_path):
    from seohead.projects.catalogue import load_catalogue

    project = tmp_path / "project"
    create_project(project, "https://example.test/")
    initialize_coverage(project)
    first, second = [item for item, entry in load_catalogue().items() if entry["kind"] == "check"][
        :2
    ]
    started = start(
        project,
        scenario_id="scenario:full-audit",
        steps=[first, second],
        context=_accepted_context(project),
    )
    with pytest.raises(ValueError, match="registered order"):
        checkpoint(
            project,
            run_id=started["run"]["id"],
            step_id=second,
            state="skipped",
            evidence=[],
            expected_revision=started["revision"],
        )
