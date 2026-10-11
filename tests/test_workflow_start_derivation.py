"""workflow-start derives the prompt and goal task from an accepted goal; refusals name the fix."""

from __future__ import annotations

import contextlib
import io
import json

import pytest

from seohead.cli import build_parser, main
from seohead.projects.coverage import (
    coverage_status,
    initialize_coverage,
    record_execution,
    update_item,
)
from seohead.projects.execution import start
from seohead.projects.inbox import goal_task_id, set_goal_state, submit
from seohead.projects.workspace import create_project


@pytest.fixture
def project(tmp_path):
    root = tmp_path / "project"
    create_project(root, "https://example.test/")
    initialize_coverage(root)
    return root


def _accepted_goal(root) -> str:
    proposed = submit(
        root,
        text="Audit the synthetic site.",
        kind="proposed_goal",
        references=["goal:synthetic"],
    )
    set_goal_state(
        root,
        entry_id=proposed["entry"]["id"],
        state="accepted",
        expected_revision=proposed["revision"],
    )
    return proposed["entry"]["id"]


def _custom_task(root, item_id="custom:worklist"):
    current = coverage_status(root)
    update_item(root, {"id": item_id, "title": "Worklist"}, current["revision"])
    return item_id


def test_goal_alone_derives_prompt_and_the_goal_task(project):
    goal = _accepted_goal(project)
    run = start(
        project,
        scenario_id="scenario:full-audit",
        steps=["check:TITLE_MISSING"],
        context={"goal_id": goal},
    )["run"]

    assert run["context"]["goal"]["id"] == goal
    assert run["context"]["prompt"]["id"] == "skill:workflow/full-audit-v1"
    assert run["context"]["task_ids"] == [goal_task_id(goal)]


def test_explicit_prompt_and_tasks_still_win_over_derivation(project):
    goal = _accepted_goal(project)
    task = _custom_task(project)
    run = start(
        project,
        scenario_id="scenario:full-audit",
        steps=["check:TITLE_MISSING"],
        context={
            "goal_id": goal,
            "prompt_reference": "skill:workflow/full-audit-v1",
            "task_ids": [task],
        },
    )["run"]

    assert run["context"]["task_ids"] == [task]


def test_missing_goal_names_the_owner_command(project):
    with pytest.raises(ValueError, match=r"requires goal_id.*inbox-goal --directory DIR"):
        start(project, scenario_id="scenario:full-audit", steps=["check:TITLE_MISSING"], context={})


def test_unaccepted_goal_names_the_acceptance_command(project):
    proposed = submit(project, text="Not yet accepted.", kind="proposed_goal")
    entry = proposed["entry"]["id"]
    with pytest.raises(ValueError, match=rf"--entry-id {entry} --state accepted"):
        start(
            project,
            scenario_id="scenario:full-audit",
            steps=["check:TITLE_MISSING"],
            context={"goal_id": entry},
        )


def test_completed_goal_task_is_refused_with_a_clear_reason(project):
    goal = _accepted_goal(project)
    task = goal_task_id(goal)
    record_execution(
        project,
        task,
        {"status": "succeeded", "reason": "done", "reviewer": "Lead", "signoff": True},
        coverage_status(project)["revision"],
    )
    assert next(i for i in coverage_status(project)["items"] if i["id"] == task)["complete"]
    with pytest.raises(ValueError, match="missing, stale or complete; pass task_ids"):
        start(
            project,
            scenario_id="scenario:full-audit",
            steps=["check:TITLE_MISSING"],
            context={"goal_id": goal},
        )


def test_unknown_context_field_lists_the_allowed_fields(project):
    goal = _accepted_goal(project)
    with pytest.raises(ValueError, match="allowed: goal_id, prompt_reference"):
        start(
            project,
            scenario_id="scenario:full-audit",
            steps=["check:TITLE_MISSING"],
            context={"goal_id": goal, "tasks": []},
        )


def test_cli_goal_id_flag_starts_the_run(project, capsys):
    goal = _accepted_goal(project)
    output = io.StringIO()
    with contextlib.redirect_stdout(output):
        code = main(
            [
                "workflow-start",
                "--directory",
                str(project),
                "--goal-id",
                goal,
                "--input",
                json.dumps(
                    {"scenario_id": "scenario:full-audit", "steps": ["check:TITLE_MISSING"]}
                ),
            ]
        )
    assert code == 0
    run = json.loads(output.getvalue())["run"]
    assert run["context"]["task_ids"] == [goal_task_id(goal)]


def test_help_documents_the_derivation_and_owner_steps(capsys):
    parser = build_parser()
    with pytest.raises(SystemExit):
        parser.parse_args(["workflow-start", "--help"])
    text = capsys.readouterr().out
    assert "--goal-id" in text
    assert "skill:workflow/full-audit-v1" in text
    assert "inbox-goal --directory DIR --entry-id ENTRY --state accepted" in text
