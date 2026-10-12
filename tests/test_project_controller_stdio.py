"""A controller must persist human-note triage and work before it executes."""

from __future__ import annotations

import asyncio
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

pytest.importorskip("mcp")

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

from seohead.projects.inbox import unread_summary
from seohead.projects.workspace import create_project

ROOT = Path(__file__).resolve().parents[1]


def _payload(result):
    content = getattr(result, "structuredContent", None)
    if isinstance(content, dict):
        return content["result"] if set(content) == {"result"} else content
    return json.loads(result.content[0].text)


async def _controller_lifecycle(project: Path) -> dict:
    environment = {
        **os.environ,
        "SEOHEAD_MCP_CONSUMER_ID": "agent/controller",
        "SEOHEAD_MCP_PROJECT_ALLOWLIST": str(project),
    }
    params = StdioServerParameters(
        command=sys.executable,
        args=["-m", "seohead.mcp.mcp_server"],
        cwd=str(ROOT),
        env=environment,
    )
    async with (
        stdio_client(params) as (read, write),
        ClientSession(read, write) as session,
    ):
        await session.initialize()
        note = _payload(
            await session.call_tool(
                "seo_project_inbox_submit",
                {
                    "directory": str(project),
                    "text": "Please review this competitor before the audit.",
                    "kind": "note",
                    "author_role": "specialist",
                },
            )
        )
        initialized = _payload(
            await session.call_tool("seo_project_checklist_init", {"directory": str(project)})
        )
        task = _payload(
            await session.call_tool(
                "seo_project_checklist_update",
                {
                    "directory": str(project),
                    "expected_revision": initialized["revision"],
                    "item": {
                        "id": "custom:review-competitor",
                        "title": "Review the supplied competitor candidate",
                    },
                },
            )
        )
        running = _payload(
            await session.call_tool(
                "seo_project_checklist_record",
                {
                    "directory": str(project),
                    "item_id": "custom:review-competitor",
                    "expected_revision": task["revision"],
                    "record": {
                        "status": "running",
                        "reason": "Controller registered the note-backed review before analysis.",
                    },
                },
            )
        )
        triaged = _payload(
            await session.call_tool(
                "seo_project_inbox_triage",
                {
                    "directory": str(project),
                    "entry_id": note["entry"]["id"],
                    "actor": "agent/controller",
                    "outcome": {
                        "kind": "task",
                        "reason": "A durable review task now owns the human suggestion.",
                        "task_ids": ["custom:review-competitor"],
                    },
                },
            )
        )
        goal = _payload(
            await session.call_tool(
                "seo_project_inbox_submit",
                {
                    "directory": str(project),
                    "text": "Run the scoped synthetic audit.",
                    "kind": "proposed_goal",
                    "author_role": "agent",
                },
            )
        )
        refused_acceptance = await session.call_tool(
            "seo_project_inbox_goal",
            {
                "directory": str(project),
                "entry_id": goal["entry"]["id"],
                "state": "accepted",
                "expected_revision": goal["revision"],
            },
        )
        assert refused_acceptance.isError
        accepted_result = subprocess.run(
            [
                sys.executable,
                "-m",
                "seohead",
                "project-inbox-goal",
                "--directory",
                str(project),
                "--entry-id",
                goal["entry"]["id"],
                "--state",
                "accepted",
            ],
            cwd=ROOT,
            env=environment,
            check=True,
            capture_output=True,
            text=True,
        )
        accepted = json.loads(accepted_result.stdout)
        workflow = _payload(
            await session.call_tool(
                "seo_workflow_start",
                {
                    "directory": str(project),
                    "scenario_id": "scenario:full-audit",
                    "steps": ["custom:review-competitor"],
                    "context": {
                        "goal_id": accepted["entry"]["id"],
                        "prompt_reference": "skill:workflow/full-audit-v1",
                        "task_ids": ["custom:review-competitor"],
                        "competitors": ["https://competitor.test/"],
                    },
                },
            )
        )
        return {
            "notice": initialized["inbox_unread"],
            "triaged": triaged,
            "workflow": workflow,
            "running": running,
        }


def test_real_stdio_controller_persists_note_task_goal_and_workflow_without_consuming_notice(
    tmp_path,
):
    project = tmp_path / "project"
    create_project(project, "https://example.test/")

    result = asyncio.run(_controller_lifecycle(project))

    assert result["notice"]["count"] == 1
    assert result["triaged"]["entry"]["triage"][-1] == {
        "kind": "task",
        "reason": "A durable review task now owns the human suggestion.",
        "actor": "agent/controller",
        "recorded_at": result["triaged"]["entry"]["triage"][-1]["recorded_at"],
        "task_ids": ["custom:review-competitor"],
    }
    assert result["running"]["views"]["running"] == ["custom:review-competitor"]
    context = result["workflow"]["run"]["context"]
    assert context["task_ids"] == ["custom:review-competitor"]
    assert context["goal"]["id"].startswith("inbox:")
    assert context["prompt"]["id"] == "skill:workflow/full-audit-v1"
    assert unread_summary(project, consumer="agent/controller")["count"] == 2
