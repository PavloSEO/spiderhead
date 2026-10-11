"""Public handler wrappers for local project workspace operations."""

from __future__ import annotations

from typing import Any

from seohead.projects.coverage import initialize_coverage, record_execution, update_item
from seohead.projects.progress import project_progress as _project_progress
from seohead.projects.workspace import create_project, open_project, project_status


def project_new(
    directory: str,
    target: str,
    label: str | None = None,
    facts: list[dict[str, Any]] | None = None,
    template_references: list[str] | None = None,
    profile_references: list[str] | None = None,
) -> dict[str, Any]:
    return create_project(
        directory,
        target,
        label=label,
        facts=facts,
        template_references=template_references,
        profile_references=profile_references,
    )


def project_open(directory: str, expected_site: str | None = None) -> dict[str, Any]:
    return open_project(directory, expected_site=expected_site)


def _with_inbox_notice(
    result: dict[str, Any], directory: str, consumer: str | None
) -> dict[str, Any]:
    """Add only this project's unread summary without consuming it."""
    if consumer is None:
        return result
    from seohead.projects.inbox import unread_goal_tasks, unread_summary

    return {
        **result,
        "inbox_unread": unread_summary(directory, consumer=consumer),
        "owner_tasks_unread": unread_goal_tasks(directory, consumer=consumer),
    }


def project_basic_status(directory: str, consumer: str | None = None) -> dict[str, Any]:
    return _with_inbox_notice(project_status(directory), directory, consumer)


def project_progress(
    directory: str, limit: int = 20, offset: int = 0, consumer: str | None = None
) -> dict[str, Any]:
    """Return a bounded project progress page without running or changing work."""
    return _with_inbox_notice(
        _project_progress(directory, limit=limit, offset=offset), directory, consumer
    )


def project_inbox_submit(
    directory: str,
    text: str,
    kind: str = "note",
    references: list[str] | None = None,
    author_role: str = "specialist",
    expected_revision: int | None = None,
) -> dict[str, Any]:
    from seohead.projects.inbox import submit

    return submit(
        directory,
        text=text,
        kind=kind,
        references=references,
        author_role=author_role,
        expected_revision=expected_revision,
    )


def project_inbox_list(
    directory: str,
    consumer: str,
    offset: int = 0,
    limit: int = 20,
    include_acknowledged: bool = True,
) -> dict[str, Any]:
    from seohead.projects.inbox import list_entries

    return list_entries(
        directory,
        consumer=consumer,
        offset=offset,
        limit=limit,
        include_acknowledged=include_acknowledged,
    )


def project_inbox_read(
    directory: str, consumer: str, entry_ids: list[str], expected_revision: int | None = None
) -> dict[str, Any]:
    from seohead.projects.inbox import mark_read

    return mark_read(
        directory, consumer=consumer, entry_ids=entry_ids, expected_revision=expected_revision
    )


def project_inbox_acknowledge(
    directory: str, consumer: str, entry_ids: list[str], expected_revision: int | None = None
) -> dict[str, Any]:
    from seohead.projects.inbox import acknowledge

    return acknowledge(
        directory, consumer=consumer, entry_ids=entry_ids, expected_revision=expected_revision
    )


def project_inbox_goal(
    directory: str, entry_id: str, state: str, expected_revision: int | None = None
) -> dict[str, Any]:
    from seohead.projects.inbox import set_goal_state

    return set_goal_state(
        directory, entry_id=entry_id, state=state, expected_revision=expected_revision
    )


def project_inbox_triage(
    directory: str,
    entry_id: str,
    outcome: dict[str, Any],
    actor: str,
    expected_revision: int | None = None,
) -> dict[str, Any]:
    """Append an explicit note outcome without accepting it or starting work."""
    from seohead.projects.inbox import triage

    return triage(
        directory,
        entry_id=entry_id,
        outcome=outcome,
        actor=actor,
        expected_revision=expected_revision,
    )


def project_inbox_unread(directory: str, consumer: str, limit: int = 10) -> dict[str, Any]:
    from seohead.projects.inbox import unread_summary

    return unread_summary(directory, consumer=consumer, limit=limit)


def project_event_append(directory: str, source: str, actor: str, text: str) -> dict[str, Any]:
    from seohead.projects.journal import record

    return record(directory, source=source, actor=actor, text=text)


def project_event_page(
    directory: str,
    offset: int = 0,
    limit: int = 50,
    source: str | None = None,
    query: str = "",
) -> dict[str, Any]:
    from seohead.projects.event_log import page

    return page(directory, offset=offset, limit=limit, source=source, query=query)


def project_observe(
    directory: str,
    consumer: str | None = None,
    scan_limit: int = 20,
    run_offset: int = 0,
    run_limit: int = 20,
) -> dict[str, Any]:
    from seohead.projects.observer import observe

    return observe(
        directory,
        consumer=consumer,
        scan_limit=scan_limit,
        run_offset=run_offset,
        run_limit=run_limit,
    )


def project_facts(
    directory: str,
    facts: list[dict[str, Any]] | None = None,
    detect: bool = False,
    apply: bool = False,
    tools: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Preview or record project facts; a detection runs only when explicitly requested."""
    from seohead.projects.facts import project_facts as core

    return core(directory, facts=facts, detect=detect, apply=apply, tools=tools)


def project_checklist_init(
    directory: str,
    template: dict | None = None,
    expected_revision: int | None = None,
    plan: dict | None = None,
) -> dict[str, Any]:
    """Create or reconcile a project's local checklist without running any item."""
    return initialize_coverage(
        directory, template=template, expected_revision=expected_revision, plan=plan
    )


def project_checklist_update(directory: str, item: dict, expected_revision: int) -> dict[str, Any]:
    """Update one checklist definition without executing it."""
    from seohead.projects.runtime import resolve_item_scope

    if not isinstance(item, dict):
        raise ValueError("item must be an object")
    target, local_id = resolve_item_scope(directory, item.get("id"))
    return update_item(target, item={**item, "id": local_id}, expected_revision=expected_revision)


def project_checklist_record(
    directory: str, item_id: str, record: dict, expected_revision: int
) -> dict[str, Any]:
    """Record supplied evidence for one item without running its operation."""
    from seohead.projects.runtime import resolve_item_scope

    target, local_id = resolve_item_scope(directory, item_id)
    return record_execution(
        target, item_id=local_id, record=record, expected_revision=expected_revision
    )


def project_task_comment(
    directory: str,
    item_id: str,
    text: str,
    actor: str,
    expected_revision: int,
    reference: dict[str, str] | None = None,
) -> dict[str, Any]:
    """Append an attributed note with an optional typed scan/finding identity."""
    from seohead.projects.coverage import append_task_comment
    from seohead.projects.runtime import resolve_item_scope

    target, local_id = resolve_item_scope(directory, item_id)
    return append_task_comment(
        target,
        item_id=local_id,
        text=text,
        actor=actor,
        expected_revision=expected_revision,
        reference=reference,
    )


def project_view_list(directory: str) -> dict[str, Any]:
    from seohead.projects.finding_views import list_views

    return list_views(directory)


def project_view_show(directory: str, name: str) -> dict[str, Any]:
    from seohead.projects.finding_views import show_view

    return show_view(directory, name)


def project_view_save(directory: str, view: dict, expected_revision: int) -> dict[str, Any]:
    from seohead.projects.finding_views import save_view

    return save_view(directory, view, expected_revision=expected_revision)


def project_view_delete(directory: str, name: str, expected_revision: int) -> dict[str, Any]:
    from seohead.projects.finding_views import delete_view

    return delete_view(directory, name, expected_revision=expected_revision)


def project_view_rename(
    directory: str, name: str, new_name: str, expected_revision: int
) -> dict[str, Any]:
    from seohead.projects.finding_views import rename_view

    return rename_view(directory, name, new_name, expected_revision=expected_revision)


def project_priorities(
    directory: str,
    policy: dict | None = None,
    apply: bool = False,
    expected_revision: int | None = None,
) -> dict[str, Any]:
    """Preview or explicitly apply an offline priority policy."""
    from seohead.projects.priorities import project_priorities as core

    return core(directory, policy=policy, apply=apply, expected_revision=expected_revision)
