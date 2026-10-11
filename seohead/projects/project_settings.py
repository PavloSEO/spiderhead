"""Revisioned project labels and owner-reviewed agent setting proposals."""

from __future__ import annotations

import copy
import functools
import re
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from .action_log import record as record_action
from .runtime import project_policy, project_write_lock, read_document, write_document
from .workspace import _load

SETTINGS_FILE = "project-settings.json"
PROPOSALS_FILE = "settings-proposals.json"
SETTINGS_FORMAT = "seohead.project-settings.v1"
PROPOSALS_FORMAT = "seohead.project-settings-proposals.v1"
_LABEL = re.compile(r"[^\x00-\x1f\x7f]{1,128}\Z")
_MAX_PROPOSALS = 100


def _label(value: Any) -> str:
    if type(value) is not str or not _LABEL.fullmatch(value) or not value.strip():
        raise ValueError("label must be non-empty text of at most 128 characters")
    return value.strip()


def _utc() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def _serialized_project_write(function):
    """Keep compound project setting mutations inside the shared writer boundary."""

    @functools.wraps(function)
    def wrapped(directory: str | Path, *args: Any, **kwargs: Any) -> Any:
        root, _project = _load(directory)
        with project_write_lock(root):
            return function(directory, *args, **kwargs)

    return wrapped


def _settings(directory: str | Path) -> tuple[Path, dict[str, Any], dict[str, Any] | None]:
    root, project = _load(directory)
    current = read_document(root, SETTINGS_FILE)
    if current is not None and (
        set(current)
        != {"format", "version", "project_uuid", "revision", "settings", "last_operation_id"}
        or current["format"] != SETTINGS_FORMAT
        or type(current["version"]) is not int
        or current["version"] != 1
        or current["project_uuid"] != project["project_uuid"]
        or type(current["revision"]) is not int
        or current["revision"] < 1
        or not isinstance(current["settings"], dict)
        or set(current["settings"]) != {"label"}
        or type(current["settings"].get("label")) is not str
        or not _LABEL.fullmatch(current["settings"].get("label", ""))
        or not current["settings"].get("label", "").strip()
        or current["last_operation_id"] is not None
        and (
            type(current["last_operation_id"]) is not str
            or not re.fullmatch(r"[0-9a-f]{32}", current["last_operation_id"])
        )
    ):
        raise ValueError("project settings have an unsupported shape")
    return root, project, current


def read_settings(directory: str | Path) -> dict[str, Any]:
    """Read project settings; legacy projects start at revision zero."""
    _root, project, current = _settings(directory)
    return {
        "ok": True,
        "revision": current["revision"] if current else 0,
        "operation_id": current["last_operation_id"] if current else None,
        "settings": copy.deepcopy(
            current["settings"] if current else {"label": project["site"]["label"]}
        ),
    }


@_serialized_project_write
def update_label(
    directory: str | Path,
    label: str,
    *,
    expected_revision: int,
    client: str = "cli",
    operation_id: str | None = None,
    log_action: bool = True,
) -> dict[str, Any]:
    """Update the project label with an optimistic revision check."""
    if type(client) is not str or client not in {"cli", "mcp", "system"}:
        raise ValueError("client must be a supported interface")
    if type(expected_revision) is not int or expected_revision < 0:
        raise ValueError("expected_revision must be a nonnegative integer")
    label = _label(label)
    if operation_id is None:
        operation_id = uuid.uuid4().hex
    if type(operation_id) is not str or not re.fullmatch(r"[0-9a-f]{32}", operation_id):
        raise ValueError("operation_id must be a UUID hex value")
    if type(log_action) is not bool:
        raise ValueError("log_action must be a boolean")
    root, project, current = _settings(directory)
    revision = current["revision"] if current else 0
    if expected_revision != revision:
        raise ValueError(f"project settings revision conflict: current revision is {revision}")
    pending_action = None
    if log_action:
        pending_action = record_action(
            root,
            client=client,
            action="project.settings.update",
            outcome="pending",
            detail="Project label update pending; terminal result is reserved.",
            operation_id=operation_id,
        )["action"]
    document = {
        "format": SETTINGS_FORMAT,
        "version": 1,
        "project_uuid": project["project_uuid"],
        "revision": revision + 1,
        "settings": {"label": label},
        "last_operation_id": operation_id,
    }
    try:
        write_document(root, SETTINGS_FILE, document, expected_revision=revision)
    except (OSError, ValueError):
        if pending_action is not None:
            from .action_log import resolve_pending

            resolve_pending(
                root,
                pending_action["sequence"],
                outcome="stale",
                detail="Project label update did not apply.",
                operation_id=operation_id,
            )
        raise
    result = read_settings(root)
    if pending_action is not None:
        from .action_log import resolve_pending

        try:
            resolve_pending(
                root,
                pending_action["sequence"],
                outcome="applied",
                detail=f"Updated project label at revision {result['revision']}.",
                operation_id=operation_id,
            )
        except (OSError, ValueError):
            result["audit_warning"] = "Label was saved; the action row remains pending."
    return result


def _proposal_document(
    directory: str | Path,
) -> tuple[Path, dict[str, Any], dict[str, Any] | None]:
    root, project = _load(directory)
    current = read_document(root, PROPOSALS_FILE)
    if current is not None and (
        set(current) != {"format", "version", "project_uuid", "revision", "proposals"}
        or current["format"] != PROPOSALS_FORMAT
        or type(current["version"]) is not int
        or current["version"] != 1
        or current["project_uuid"] != project["project_uuid"]
        or type(current["revision"]) is not int
        or current["revision"] < 1
        or not isinstance(current["proposals"], list)
        or len(current["proposals"]) > _MAX_PROPOSALS
    ):
        raise ValueError("settings proposal store has an unsupported shape")
    if current is not None:
        for proposal in current["proposals"]:
            if not isinstance(proposal, dict) or set(proposal) != {
                "id",
                "created_at",
                "client",
                "status",
                "kind",
                "base_revision",
                "changes",
                "decided_at",
                "action_sequence",
                "decision",
            }:
                raise ValueError("settings proposal has an unsupported shape")
            if (
                type(proposal["id"]) is not str
                or not re.fullmatch(r"[0-9a-f]{32}", proposal["id"])
                or type(proposal["client"]) is not str
                or proposal["client"] not in {"cli", "mcp", "system"}
                or type(proposal["status"]) is not str
                or proposal["status"]
                not in {"pending", "applying", "applied", "denied", "stale"}
                or type(proposal["kind"]) is not str
                or proposal["kind"] not in {"project_settings", "crawl_policy"}
                or type(proposal["base_revision"]) is not int
                or proposal["base_revision"] < 0
                or type(proposal["action_sequence"]) is not int
                or proposal["action_sequence"] < 1
                or proposal["decision"] is not None
                and (
                    type(proposal["decision"]) is not str
                    or proposal["decision"] not in {"approve", "deny"}
                )
            ):
                raise ValueError("settings proposal metadata is invalid")
            if not isinstance(proposal["changes"], dict):
                raise ValueError("settings proposal changes must be an object")
            changes = _validate_proposal(proposal["changes"])
            expected_kind = "project_settings" if "label" in changes else "crawl_policy"
            if expected_kind != proposal["kind"]:
                raise ValueError("settings proposal kind does not match its changes")
            for stamp_name in ("created_at", "decided_at"):
                stamp = proposal[stamp_name]
                if stamp is None and stamp_name == "decided_at":
                    continue
                try:
                    parsed = datetime.fromisoformat(stamp.replace("Z", "+00:00"))
                except (AttributeError, ValueError) as exc:
                    raise ValueError("settings proposal timestamps must be RFC3339 UTC") from exc
                if parsed.tzinfo is None or parsed.utcoffset() != timezone.utc.utcoffset(parsed):
                    raise ValueError("settings proposal timestamps must be RFC3339 UTC")
            if (proposal["status"] in {"pending", "applying"}) != (proposal["decided_at"] is None):
                raise ValueError("settings proposal decision time does not match its status")
            if proposal["status"] == "pending" and proposal["decision"] is not None:
                raise ValueError("pending settings proposal cannot have a decision")
            if proposal["status"] == "applying" and proposal["decision"] != "approve":
                raise ValueError("applying settings proposal must be approved")
            if proposal["status"] in {"applied", "denied", "stale"} and proposal["decision"] is None:
                raise ValueError("decided settings proposal must retain its decision")
    return root, project, current


def _validate_proposal(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict) or len(value) != 1:
        raise ValueError("proposal must change exactly one supported setting")
    if "label" in value:
        return {"label": _label(value["label"])}
    if "crawl_policy" in value:
        from .runtime import validate_policy

        return {"crawl_policy": validate_policy(value["crawl_policy"])}
    raise ValueError("proposal supports label or crawl_policy settings only")


@_serialized_project_write
def propose_settings(
    directory: str | Path,
    changes: dict[str, Any],
    *,
    client: str = "mcp",
    expected_revision: int,
) -> dict[str, Any]:
    """Record one agent proposal; it never applies settings or authorizes execution."""
    if type(client) is not str or client not in {"cli", "mcp", "system"}:
        raise ValueError("client must be a supported interface")
    if type(expected_revision) is not int or expected_revision < 0:
        raise ValueError("expected_revision must be a nonnegative integer")
    root, project, current = _proposal_document(directory)
    revision = current["revision"] if current else 0
    if expected_revision != revision:
        raise ValueError(f"proposal revision conflict: current revision is {revision}")
    proposals = copy.deepcopy(current["proposals"] if current else [])
    if len(proposals) >= _MAX_PROPOSALS:
        raise ValueError("project settings proposal queue is full")
    changes = _validate_proposal(changes)
    kind = "project_settings" if "label" in changes else "crawl_policy"
    if kind == "project_settings":
        base_revision = read_settings(root)["revision"]
    else:
        base_revision = project_policy(str(root))["revision"]
    proposal = {
        "id": uuid.uuid4().hex,
        "created_at": _utc(),
        "client": client,
        "status": "pending",
        "kind": kind,
        "base_revision": base_revision,
        "changes": changes,
        "decided_at": None,
        "decision": None,
    }
    action = record_action(
        root,
        client=client,
        action="project.settings.propose",
        outcome="pending",
        detail="Settings proposal pending; terminal review result is reserved.",
        proposal_id=proposal["id"],
    )["action"]
    proposal["action_sequence"] = action["sequence"]
    proposals.append(proposal)
    document = {
        "format": PROPOSALS_FORMAT,
        "version": 1,
        "project_uuid": project["project_uuid"],
        "revision": revision + 1,
        "proposals": proposals,
    }
    try:
        write_document(root, PROPOSALS_FILE, document, expected_revision=revision)
    except (OSError, ValueError):
        from .action_log import resolve_pending

        resolve_pending(
            root,
            action["sequence"],
            outcome="refused",
            detail="Settings proposal was not persisted.",
            proposal_id=proposal["id"],
        )
        raise
    return {"ok": True, "revision": document["revision"], "proposal": proposal}


def list_proposals(directory: str | Path, *, offset: int = 0, limit: int = 50) -> dict[str, Any]:
    if type(offset) is not int or offset < 0 or type(limit) is not int or not 1 <= limit <= 100:
        raise ValueError("offset must be nonnegative and limit must be 1..100")
    _root, _project, current = _proposal_document(directory)
    proposals = current["proposals"] if current else []
    items = list(reversed(proposals))[offset : offset + limit]
    total = len(proposals)
    return {
        "ok": True,
        "revision": current["revision"] if current else 0,
        "total": total,
        "items": items,
        "pagination": {
            "total": total,
            "offset": offset,
            "limit": limit,
            "next_offset": offset + limit if offset + limit < total else None,
        },
    }


@_serialized_project_write
def review_proposal(
    directory: str | Path,
    proposal_id: str,
    decision: str,
    *,
    expected_revision: int,
    client: str = "cli",
) -> dict[str, Any]:
    """Apply or deny one pending proposal through the owner CLI workflow."""
    if type(client) is not str or client not in {"cli", "mcp", "system"}:
        raise ValueError("client must be a supported interface")
    if type(decision) is not str or decision not in {"approve", "deny"}:
        raise ValueError("decision must be approve or deny")
    if type(expected_revision) is not int or expected_revision < 0:
        raise ValueError("expected_revision must be nonnegative")
    root, project, current = _proposal_document(directory)
    revision = current["revision"] if current else 0
    proposals = copy.deepcopy(current["proposals"] if current else [])
    proposal = next((item for item in proposals if item["id"] == proposal_id), None)
    if proposal is None:
        raise ValueError("settings proposal does not exist")
    if revision != expected_revision and not (
        proposal["status"] in {"applying", "applied", "denied", "stale"}
        and proposal["decision"] == decision
    ):
        raise ValueError(f"proposal revision conflict: current revision is {revision}")
    if proposal["status"] in {"applied", "denied", "stale"}:
        if proposal["decision"] != decision:
            raise ValueError("settings proposal already has a different terminal decision")
        from .action_log import resolve_pending

        result = {"ok": True, "revision": revision, "proposal": proposal}
        try:
            resolve_pending(
                root,
                proposal["action_sequence"],
                outcome=proposal["status"],
                detail=_review_detail(proposal["status"], proposal["kind"]),
                proposal_id=proposal_id,
            )
        except (OSError, ValueError):
            result["audit_warning"] = "Proposal status is saved; its action row remains pending."
        return result
    changes = proposal["changes"]
    if proposal["kind"] == "project_settings":
        current_settings = read_settings(root)
        current_revision = current_settings["revision"]
        current_value = current_settings["settings"]["label"]
        current_operation = current_settings["operation_id"]
        desired_value = changes["label"]
    else:
        current_policy = project_policy(str(root))
        current_revision = current_policy["revision"]
        current_value = current_policy["policy"]
        current_operation = current_policy["operation_id"]
        desired_value = changes["crawl_policy"]

    if current_operation == proposal_id:
        status = "applied"
    elif decision == "deny":
        status = "denied"
    elif current_value == desired_value and current_revision >= proposal["base_revision"]:
        status = "applied"
    elif current_revision != proposal["base_revision"]:
        status = "stale"
    else:
        if proposal["status"] != "applying":
            proposal["status"] = "applying"
            proposal["decision"] = "approve"
            marker = {
                "format": PROPOSALS_FORMAT,
                "version": 1,
                "project_uuid": project["project_uuid"],
                "revision": revision + 1,
                "proposals": proposals,
            }
            write_document(root, PROPOSALS_FILE, marker, expected_revision=revision)
            revision += 1
        if proposal["kind"] == "project_settings":
            update_label(
                root,
                desired_value,
                expected_revision=proposal["base_revision"],
                client=client,
                operation_id=proposal_id,
                log_action=False,
            )
        else:
            project_policy(
                str(root),
                policy=desired_value,
                apply=True,
                expected_revision=proposal["base_revision"],
                operation_id=proposal_id,
            )
        status = "applied"

    proposal["status"] = status
    proposal["decision"] = decision
    proposal["decided_at"] = _utc()
    document = {
        "format": PROPOSALS_FORMAT,
        "version": 1,
        "project_uuid": project["project_uuid"],
        "revision": revision + 1,
        "proposals": proposals,
    }
    write_document(root, PROPOSALS_FILE, document, expected_revision=revision)
    from .action_log import resolve_pending

    result = {"ok": True, "revision": document["revision"], "proposal": proposal}
    try:
        resolve_pending(
            root,
            proposal["action_sequence"],
            outcome=status,
            detail=_review_detail(status, proposal["kind"]),
            proposal_id=proposal_id,
        )
    except (OSError, ValueError):
        result["audit_warning"] = "Proposal status is saved; its action row remains pending."
    return result


def _review_detail(status: str, kind: str) -> str:
    verb = {"applied": "approved", "denied": "denied", "stale": "marked stale"}[status]
    return f"Owner {verb} {kind} proposal."


def delete_project(directory: str | Path, *, client: str = "cli") -> dict[str, Any]:
    """Move a project into a sibling Trash directory, preserving all its artifacts."""
    import os

    from .action_log import record as record_action
    from .action_log import resolve_pending, resolve_pending_root
    from .run_observation import live_holder
    from .runtime import project_write_lock

    if type(client) is not str or client not in {"cli", "mcp", "system"}:
        raise ValueError("client must be a supported interface")
    root, project = _load(directory)
    trash = root.parent / "Trash"
    if trash.is_symlink():
        raise ValueError("project Trash path is unsafe")
    destination: Path | None = None
    with project_write_lock(root):
        pending_action = record_action(
            root,
            client=client,
            action="project.delete",
            outcome="pending",
            detail="Project deletion requested; terminal outcome will replace this row.",
        )["action"]
        try:
            owner = live_holder(root)
            if owner is not None:
                resolve_pending(
                    root,
                    pending_action["sequence"],
                    outcome="refused",
                    detail="Project delete refused: active run.",
                )
                raise ValueError(f"project has an active run: {owner}")
            trash.mkdir(mode=0o700, exist_ok=True)
            if not trash.is_dir():
                resolve_pending(
                    root,
                    pending_action["sequence"],
                    outcome="refused",
                    detail="Project delete refused: Trash path is not a directory.",
                )
                raise ValueError("project Trash path is not a directory")
            destination = trash / (
                f"{root.name}-{datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')}-"
                f"{uuid.uuid4().hex[:8]}"
            )
            os.replace(root, destination)
        except (OSError, ValueError) as exc:
            if root.exists():
                try:
                    resolve_pending(
                        root,
                        pending_action["sequence"],
                        outcome="refused",
                        detail=f"Project delete refused: {type(exc).__name__}.",
                    )
                except ValueError:
                    pass
            raise
    assert destination is not None
    result = {
        "ok": True,
        "deleted": True,
        "project_uuid": project["project_uuid"],
        "trash_path": str(destination),
        "exports_retained": True,
    }
    try:
        resolve_pending_root(
            destination,
            pending_action["sequence"],
            outcome="deleted",
            detail="Moved to sibling Trash; scans and exports retained.",
        )
    except (OSError, ValueError):
        result["audit_warning"] = "Project was moved; its action row remains pending."
    return result
