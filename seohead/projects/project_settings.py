"""Revisioned project labels and owner-reviewed agent setting proposals."""

from __future__ import annotations

import copy
import functools
import os
import re
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from seohead.core import filesystem
from .action_log import record as record_action
from .runtime import project_policy, read_document, write_document
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


def _serialized_proposal_write(function):
    """Serialize proposal creation/review around the separate settings-store writes."""
    @functools.wraps(function)
    def wrapped(directory: str | Path, *args: Any, **kwargs: Any) -> Any:
        root, _project = _load(directory)
        path = root / ".settings-proposals-operation.lock"
        descriptor = None
        try:
            descriptor = filesystem.open_lock(path)
            filesystem.lock_exclusive(descriptor)
        except OSError as exc:
            if descriptor is not None:
                os.close(descriptor)
            raise ValueError("settings proposal writer is busy or locking is unavailable") from exc
        try:
            return function(directory, *args, **kwargs)
        finally:
            try:
                filesystem.unlock(descriptor)
            finally:
                os.close(descriptor)

    return wrapped


def _settings(directory: str | Path) -> tuple[Path, dict[str, Any], dict[str, Any] | None]:
    root, project = _load(directory)
    current = read_document(root, SETTINGS_FILE)
    if current is not None and (
        set(current) != {"format", "version", "project_uuid", "revision", "settings"}
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
    ):
        raise ValueError("project settings have an unsupported shape")
    return root, project, current


def read_settings(directory: str | Path) -> dict[str, Any]:
    """Read project settings; legacy projects start at revision zero."""
    _root, project, current = _settings(directory)
    return {
        "ok": True,
        "revision": current["revision"] if current else 0,
        "settings": copy.deepcopy(
            current["settings"] if current else {"label": project["site"]["label"]}
        ),
    }


def update_label(
    directory: str | Path,
    label: str,
    *,
    expected_revision: int,
    client: str = "cli",
) -> dict[str, Any]:
    """Update the project label with an optimistic revision check."""
    if type(client) is not str or client not in {"cli", "mcp", "system"}:
        raise ValueError("client must be a supported interface")
    if type(expected_revision) is not int or expected_revision < 0:
        raise ValueError("expected_revision must be a nonnegative integer")
    label = _label(label)
    root, project, current = _settings(directory)
    revision = current["revision"] if current else 0
    if expected_revision != revision:
        raise ValueError(f"project settings revision conflict: current revision is {revision}")
    record_action(
        root,
        client=client,
        action="project.settings.update",
        outcome="pending",
        detail="Project label update requested",
    )
    document = {
        "format": SETTINGS_FORMAT,
        "version": 1,
        "project_uuid": project["project_uuid"],
        "revision": revision + 1,
        "settings": {"label": label},
    }
    try:
        write_document(root, SETTINGS_FILE, document, expected_revision=revision)
    except ValueError:
        record_action(
            root,
            client=client,
            action="project.settings.update",
            outcome="stale",
            detail="Project settings changed before the label update was applied",
        )
        raise
    result = read_settings(root)
    try:
        record_action(
            root,
            client=client,
            action="project.settings.update",
            outcome="applied",
            detail=f"Updated project label at revision {result['revision']}",
        )
    except (OSError, ValueError):
        pass
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
            }:
                raise ValueError("settings proposal has an unsupported shape")
            if (
                type(proposal["id"]) is not str
                or not re.fullmatch(r"[0-9a-f]{32}", proposal["id"])
                or type(proposal["client"]) is not str
                or proposal["client"] not in {"cli", "mcp", "system"}
                or type(proposal["status"]) is not str
                or proposal["status"] not in {"pending", "applied", "denied", "stale"}
                or type(proposal["kind"]) is not str
                or proposal["kind"] not in {"project_settings", "crawl_policy"}
                or type(proposal["base_revision"]) is not int
                or proposal["base_revision"] < 0
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
            if (proposal["status"] == "pending") != (proposal["decided_at"] is None):
                raise ValueError("settings proposal decision time does not match its status")
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


@_serialized_proposal_write
def propose_settings(
    directory: str | Path,
    changes: dict[str, Any],
    *,
    client: str = "mcp",
    expected_revision: int | None = None,
) -> dict[str, Any]:
    """Record one agent proposal; it never applies settings or authorizes execution."""
    if type(client) is not str or client not in {"cli", "mcp", "system"}:
        raise ValueError("client must be a supported interface")
    root, project, current = _proposal_document(directory)
    revision = current["revision"] if current else 0
    if expected_revision is not None:
        if type(expected_revision) is not int or expected_revision < 0:
            raise ValueError("expected_revision must be a nonnegative integer")
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
    }
    proposals.append(proposal)
    document = {
        "format": PROPOSALS_FORMAT,
        "version": 1,
        "project_uuid": project["project_uuid"],
        "revision": revision + 1,
        "proposals": proposals,
    }
    write_document(root, PROPOSALS_FILE, document, expected_revision=revision)
    record_action(
        root,
        client=client,
        action="project.settings.propose",
        outcome="pending",
        detail=f"Proposed {kind} change",
        proposal_id=proposal["id"],
    )
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


@_serialized_proposal_write
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
    if revision != expected_revision:
        raise ValueError(f"proposal revision conflict: current revision is {revision}")
    proposals = copy.deepcopy(current["proposals"] if current else [])
    proposal = next((item for item in proposals if item["id"] == proposal_id), None)
    if proposal is None or proposal["status"] != "pending":
        raise ValueError("settings proposal does not exist or is no longer pending")
    outcome = "denied"
    if decision == "approve":
        changes = proposal["changes"]
        if proposal["kind"] == "project_settings":
            current_settings = read_settings(root)
            if current_settings["revision"] != proposal["base_revision"]:
                proposal["status"] = "stale"
                outcome = "stale"
            else:
                update_label(
                    root,
                    changes["label"],
                    expected_revision=proposal["base_revision"],
                    client="cli",
                )
                proposal["status"] = "applied"
                outcome = "applied"
        else:
            current_policy = project_policy(str(root))
            if current_policy["revision"] != proposal["base_revision"]:
                proposal["status"] = "stale"
                outcome = "stale"
            else:
                project_policy(
                    str(root),
                    policy=changes["crawl_policy"],
                    apply=True,
                    expected_revision=proposal["base_revision"],
                )
                proposal["status"] = "applied"
                outcome = "applied"
    else:
        proposal["status"] = "denied"
    proposal["decided_at"] = _utc()
    document = {
        "format": PROPOSALS_FORMAT,
        "version": 1,
        "project_uuid": project["project_uuid"],
        "revision": revision + 1,
        "proposals": proposals,
    }
    write_document(root, PROPOSALS_FILE, document, expected_revision=revision)
    review_text = (
        "approved" if outcome == "applied" else "denied" if outcome == "denied" else "stale"
    )
    record_action(
        root,
        client=client,
        action="project.settings.review",
        outcome=outcome,
        detail=f"Owner {review_text} {proposal['kind']} proposal",
        proposal_id=proposal_id,
    )
    return {"ok": True, "revision": document["revision"], "proposal": proposal}


def delete_project(directory: str | Path, *, client: str = "cli") -> dict[str, Any]:
    """Move a project into a sibling Trash directory, preserving all its artifacts."""
    import os

    from .action_log import record_root
    from .run_observation import live_holder

    if type(client) is not str or client not in {"cli", "mcp", "system"}:
        raise ValueError("client must be a supported interface")
    root, project = _load(directory)
    trash = root.parent / "Trash"
    if trash.is_symlink():
        raise ValueError("project Trash path is unsafe")
    record_action(
        root,
        client=client,
        action="project.delete",
        outcome="pending",
        detail="Project delete requested",
    )
    lock_path = root / ".run-observation.json.lock"
    try:
        lock_fd = os.open(lock_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except OSError as exc:
        record_action(
            root,
            client=client,
            action="project.delete",
            outcome="refused",
            detail=f"Project is busy or could not be locked for deletion: {type(exc).__name__}",
        )
        raise
    destination: Path | None = None
    try:
        owner = live_holder(root)
        if owner is not None:
            record_action(
                root,
                client=client,
                action="project.delete",
                outcome="refused",
                detail="Project has an active run",
            )
            raise ValueError(f"project has an active run: {owner}")
        trash.mkdir(mode=0o700, exist_ok=True)
        if not trash.is_dir():
            record_action(
                root,
                client=client,
                action="project.delete",
                outcome="refused",
                detail="Project Trash path is not a directory",
            )
            raise ValueError("project Trash path is not a directory")
        destination = trash / (
            f"{root.name}-{datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')}-"
            f"{uuid.uuid4().hex[:8]}"
        )
        os.replace(root, destination)
    except OSError as exc:
        record_action(
            root,
            client=client,
            action="project.delete",
            outcome="refused",
            detail=f"Project could not be moved to Trash: {type(exc).__name__}",
        )
        raise
    finally:
        os.close(lock_fd)
        if root.exists():
            lock_path.unlink(missing_ok=True)
        elif destination is not None:
            (destination / lock_path.name).unlink(missing_ok=True)
    assert destination is not None
    try:
        record_root(
            destination,
            client=client,
            action="project.delete",
            outcome="deleted",
            detail="Project moved to sibling Trash; scans and exports retained",
        )
    except (OSError, ValueError):
        pass
    return {
        "ok": True,
        "deleted": True,
        "project_uuid": project["project_uuid"],
        "trash_path": str(destination),
        "exports_retained": True,
    }
