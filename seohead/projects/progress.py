"""Compact, bounded progress and next-action view for local projects."""

from __future__ import annotations

from pathlib import Path
from typing import Any

DEFAULT_LIMIT = 20
MAX_LIMIT = 100
MAX_OFFSET = 1_000_000
MAX_NEXT_ACTIONS = 5

_STATE_NAMES = (
    "completed",
    "remaining",
    "running",
    "blocked",
    "stale",
    "unavailable",
    "review",
    "deliverable",
    "excluded",
    "not_agreed",
    "not_run",
)


def _item_state(item: dict[str, Any]) -> str:
    """Project one checklist row onto a compact display state."""
    applicability = item.get("applicability")
    if applicability == "excluded":
        return "excluded"
    if applicability == "not_agreed":
        return "not_agreed"
    if item.get("stale"):
        return "stale"
    if item.get("attempt_status") == "running":
        return "running"
    if item.get("attempt_status") == "unavailable":
        return "unavailable"
    if item.get("blocked_by") or item.get("attempt_status") == "failed":
        return "blocked"
    if item.get("complete"):
        return "completed"
    if applicability == "pending_exclusion":
        return "review"
    if item.get("execution_kind") == "manual":
        # Never attempted is not waiting for review; only an attempt is.
        return "not_run" if item.get("attempt_status", "not_run") == "not_run" else "review"
    if item.get("execution_kind") == "deliverable":
        return "deliverable"
    return "remaining"


def _has_explicit_plans(
    directory: str | Path, checklist: dict[str, Any], coverage_views: dict | None = None
) -> tuple[bool, str | None]:
    """Require an agreed plan at every site included in an aggregated checklist."""
    sites = checklist.get("sites")
    if not isinstance(sites, list):
        return checklist.get("plan") is not None, None
    if not sites:
        return False, "the aggregated checklist contains no site scope"

    from .coverage import coverage_status

    root = Path(directory).resolve()
    for site in sites:
        relative = site.get("directory") if isinstance(site, dict) else None
        if not isinstance(relative, str):
            return False, "a site scope has no validated project directory"
        child = Path(relative)
        if child.is_absolute() or ".." in child.parts:
            return False, "a site scope points outside the project"
        if relative == ".":
            current = checklist
        else:
            target = root / child
            if target.is_symlink():
                return False, "a site scope points through a symbolic link"
            resolved = target.resolve()
            if not resolved.is_relative_to(root):
                return False, "a site scope points outside the project"
            try:
                current = (coverage_views or {}).get(str(resolved))
                if current is None:
                    current = coverage_status(resolved)
            except (OSError, ValueError):
                return False, "a declared site scope could not be read"
        if current.get("state") != "initialized" or current.get("plan") is None:
            return False, "every site needs an explicitly recorded audit plan"
    return True, None


def _task_completion(
    checklist: dict[str, Any], explicit_scope: bool, scope_reason: str | None
) -> dict[str, Any]:
    """Project the #794 axis without rebuilding its numerator or denominator."""
    axis = (checklist.get("coverage") or {}).get("audit_tasks")
    result = {
        "label": "Audit-task completion",
        "basis": None,
        "numerator": None,
        "denominator": None,
        "percent": None,
        "state": "unknown",
        "reason": None,
    }
    if not explicit_scope:
        result["reason"] = (
            scope_reason or "record an explicit audit plan before reporting a percentage"
        )
        return result
    if not isinstance(axis, dict):
        result["reason"] = checklist.get("reason") or "audit-task coverage is unavailable"
        return result
    result["basis"] = axis.get("basis")
    if axis.get("state") != "measured":
        result["reason"] = axis.get("reason") or "the agreed audit-task denominator is unavailable"
        return result
    numerator, denominator = axis.get("numerator"), axis.get("denominator")
    if (
        type(numerator) is not int
        or type(denominator) is not int
        or denominator <= 0
        or not 0 <= numerator <= denominator
    ):
        result["reason"] = axis.get("reason") or "the agreed audit-task ratio is undefined"
        return result
    result.update(
        numerator=numerator,
        denominator=denominator,
        percent=round(100 * numerator / denominator, 1),
        state="measured",
        reason=None,
    )
    return result


def _scope_summary(
    checklist: dict[str, Any], explicit_scope: bool, reason: str | None
) -> dict[str, Any]:
    plan = checklist.get("plan")
    population = plan.get("population") if isinstance(plan, dict) else None
    tasks = plan.get("tasks") if isinstance(plan, dict) else None
    return {
        "recorded_for_all_sites": explicit_scope,
        "site_count": len(checklist.get("sites", [])) or 1,
        "plan_revision": plan.get("revision") if isinstance(plan, dict) else None,
        "reviewer": plan.get("reviewer") if isinstance(plan, dict) else None,
        "population": (
            {
                "kind": population.get("kind"),
                "size": population.get("size"),
                "name": population.get("name"),
                "source": population.get("source"),
                "reason": population.get("reason"),
            }
            if isinstance(population, dict)
            else None
        ),
        "task_set": (
            {
                "kind": tasks.get("kind"),
                "selected_count": len(tasks.get("ids", []))
                if isinstance(tasks.get("ids"), list)
                else None,
                "source": tasks.get("source"),
            }
            if isinstance(tasks, dict)
            else None
        ),
        "reason": reason,
    }


def _next_action(item: dict[str, Any], state: str) -> dict[str, Any] | None:
    if state == "remaining":
        action = "run this agreed checklist task"
    elif state == "stale":
        action = "refresh the stale evidence and rerun the task"
    elif state == "unavailable":
        action = "resolve the unavailable input and retry the task"
    elif state == "not_run":
        action = "complete the agreed manual check and record its attempt"
    elif state == "review":
        action = "review the exclusion or complete the required signoff"
    elif state == "deliverable":
        action = "prepare the deliverable and record its approved review"
    elif state == "blocked" and not item.get("blocked_by"):
        action = "inspect the failed attempt and retry when its cause is resolved"
    else:
        return None
    return {
        "kind": "task",
        "id": item.get("id"),
        "title": item.get("title"),
        "state": state,
        "action": action,
        "reason": item.get("reason"),
        "blocked_by": item.get("blocked_by", []),
    }


def project_progress(
    directory: str | Path,
    *,
    limit: int = DEFAULT_LIMIT,
    offset: int = 0,
    _status: dict | None = None,
    _coverage_views: dict | None = None,
) -> dict[str, Any]:
    """Return a bounded project progress page using the project checklist's own coverage axes."""
    if type(limit) is not int or not 1 <= limit <= MAX_LIMIT:
        raise ValueError(f"limit must be an integer from 1 to {MAX_LIMIT}")
    if type(offset) is not int or not 0 <= offset <= MAX_OFFSET:
        raise ValueError(f"offset must be an integer from 0 to {MAX_OFFSET}")

    from .workspace import project_status

    status = project_status(directory) if _status is None else _status
    checklist = status["checklist"]
    rows = checklist.get("items", [])
    explicit_scope, scope_reason = _has_explicit_plans(directory, checklist, _coverage_views)
    states = [_item_state(item) for item in rows]
    state_counts = {name: sum(state == name for state in states) for name in _STATE_NAMES}
    counts = checklist.get("counts") or {}
    coverage = checklist.get("coverage") or {}
    url_axis = coverage.get("url_population") or {}

    next_actions: list[dict[str, Any]] = []
    if checklist.get("state") != "initialized":
        next_actions.append(
            {
                "kind": "scope",
                "id": "scope:initialize",
                "state": "review",
                "action": "initialize the checklist and record the agreed audit plan",
                "reason": checklist.get("reason") or "the project checklist is not initialized",
            }
        )
    elif not explicit_scope:
        next_actions.append(
            {
                "kind": "scope",
                "id": "scope:agree",
                "state": "review",
                "action": "record the agreed site population and applicable task set",
                "reason": scope_reason,
            }
        )
    elif url_axis.get("state") in {"unknown", "partial"}:
        next_actions.append(
            {
                "kind": "scope",
                "id": "scope:url-population",
                "state": "review",
                "action": "establish or verify URL-population coverage",
                "reason": url_axis.get("reason"),
            }
        )

    for item, state_name in zip(rows, states, strict=True):
        action = _next_action(item, state_name)
        if action is not None:
            next_actions.append(action)
        if len(next_actions) >= MAX_NEXT_ACTIONS:
            break

    total = len(rows)
    page = rows[offset : offset + limit]
    page_states = states[offset : offset + limit]
    items = [
        {
            "id": item.get("id"),
            "title": item.get("title"),
            "kind": item.get("execution_kind"),
            "state": state_name,
            "complete": bool(item.get("complete")),
            "stale": bool(item.get("stale")),
            "attempt_status": item.get("attempt_status"),
            "applicability": item.get("applicability"),
            "blocked_by": item.get("blocked_by", []),
            "reason": item.get("reason"),
        }
        for item, state_name in zip(page, page_states, strict=True)
    ]
    next_offset = offset + len(page) if offset + len(page) < total else None
    project = status.get("project", {})
    site = project.get("site", {}) if isinstance(project, dict) else {}
    return {
        "state": checklist.get("state", "unknown"),
        "project": {
            "site": site.get("host"),
            "label": site.get("label"),
        },
        "revision": checklist.get("revision"),
        "scope": _scope_summary(checklist, explicit_scope, scope_reason),
        "audit_task_completion": _task_completion(checklist, explicit_scope, scope_reason),
        "url_population": {
            key: url_axis.get(key)
            for key in (
                "basis",
                "numerator",
                "denominator",
                "state",
                "reason",
                "population_kind",
                "population_name",
            )
        }
        if url_axis
        else None,
        "audit_complete": bool(checklist.get("complete")),
        "counts": {
            key: counts.get(key)
            for key in (
                "total",
                "complete",
                "remaining",
                "stale",
                "excluded",
                "pending_exclusion",
                "not_agreed",
            )
        },
        "state_counts": state_counts,
        "next_actions": next_actions[:MAX_NEXT_ACTIONS],
        "items": items,
        "pagination": {
            "limit": limit,
            "offset": offset,
            "total": total,
            "next_offset": next_offset,
        },
    }
