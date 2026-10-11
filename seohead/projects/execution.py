"""Durable, registered workflow checkpoints for interruption-safe local work."""

from __future__ import annotations

import copy
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from .coverage import coverage_status
from .runtime import read_document, write_document
from .workspace import _load as _load_workspace

FORMAT = "seohead.workflow-execution.v1"
NAME = "execution.json"
_TERMINAL = {"completed", "cancelled"}
_STEP_STATES = {"succeeded", "failed", "unavailable", "skipped", "interrupted"}
EVIDENCE_HASH_SECONDS = 5.0
EVIDENCE_HASH_BYTES = 1024 * 1024 * 1024


def _text(value: Any, name: str, maximum: int = 512) -> str:
    if not isinstance(value, str) or not value.strip() or len(value) > maximum:
        raise ValueError(f"{name} must be nonempty text of at most {maximum} characters")
    return value.strip()


def _load(directory: str | Path) -> tuple[Path, dict[str, Any], dict[str, Any]]:
    root, project = _load_workspace(directory)
    document = read_document(root, NAME)
    if document is None:
        document = {
            "format": FORMAT,
            "revision": 0,
            "project_uuid": project["project_uuid"],
            "runs": [],
        }
    if document.get("format") != FORMAT or document.get("project_uuid") != project["project_uuid"]:
        raise ValueError("execution checkpoint belongs to another project or format")
    if not isinstance(document.get("runs"), list) or len(document["runs"]) > 10_000:
        raise ValueError("execution checkpoint has invalid run history")
    return root, project, document


def _save(root: Path, document: dict[str, Any], expected_revision: int) -> None:
    if document["revision"] != expected_revision:
        raise ValueError("execution revision conflict")
    document["revision"] += 1
    write_document(root, NAME, document)


def _catalogue_hash(item_id: str) -> str | None:
    from .catalogue import load_catalogue

    entry = load_catalogue().get(item_id)
    return entry.get("definition_hash") if entry else None


def _accepted_goal(directory: str | Path, value: Any) -> dict[str, Any]:
    """Resolve an already accepted inbox goal without copying its mutable text."""
    goal_id = _text(value, "goal_id", 256)
    from .inbox import list_entries

    offset = 0
    while True:
        page = list_entries(
            directory,
            consumer="workflow-handoff",
            offset=offset,
            limit=100,
        )
        for entry in page["entries"]:
            if entry["id"] != goal_id:
                continue
            if entry["kind"] != "proposed_goal" or entry["goal_state"] != "accepted":
                raise ValueError("workflow goal_id must identify an accepted proposed goal")
            return {"id": entry["id"], "references": entry["references"]}
        next_offset = page["pagination"]["next_offset"]
        if next_offset is None:
            break
        offset = next_offset
    raise ValueError("workflow goal_id is not present in this project inbox")


def _prompt_reference(value: Any) -> dict[str, str]:
    prompt_id = _text(value, "prompt_reference", 512)
    from .catalogue import load_catalogue

    entry = load_catalogue().get(prompt_id)
    if entry is None or entry["kind"] != "skill":
        raise ValueError("prompt_reference must identify a registered skill")
    return {"id": prompt_id, "definition_hash": entry["definition_hash"]}


def _task_ids(value: Any, rows: dict[str, dict[str, Any]]) -> list[str]:
    if (
        not isinstance(value, list)
        or not value
        or len(value) > 20
        or len(value) != len(set(value))
        or any(type(item) is not str or not item.startswith("custom:") for item in value)
    ):
        raise ValueError("workflow task_ids must be unique current custom checklist tasks")
    if any(item not in rows or rows[item]["stale"] or rows[item]["complete"] for item in value):
        raise ValueError("workflow task_ids must be current incomplete custom checklist tasks")
    return value


def _context(
    value: Any, directory: str | Path, project: dict[str, Any], rows: dict[str, dict[str, Any]]
) -> dict[str, Any]:
    if value is None:
        raise ValueError(
            "workflow context requires accepted goal_id, prompt_reference, and task_ids"
        )
    if not isinstance(value, dict) or set(value) - {
        "goal_id",
        "prompt_reference",
        "task_ids",
        "competitors",
        "phase",
    }:
        raise ValueError("workflow context has unsupported fields")
    if {"goal_id", "prompt_reference", "task_ids"} - set(value):
        raise ValueError(
            "workflow context requires accepted goal_id, prompt_reference, and task_ids"
        )
    competitors = value.get("competitors", [])
    if (
        not isinstance(competitors, list)
        or len(competitors) > 20
        or len(set(competitors)) != len(competitors)
        or any(
            not isinstance(url, str) or not url.startswith(("http://", "https://"))
            for url in competitors
        )
    ):
        raise ValueError("workflow competitors must be a unique bounded HTTP(S) list")
    result = {
        "own_site": project["site"]["target"],
        "competitors": competitors,
        "goal": _accepted_goal(directory, value["goal_id"]),
        "prompt": _prompt_reference(value["prompt_reference"]),
        "task_ids": _task_ids(value["task_ids"], rows),
    }
    if "phase" in value:
        result["phase"] = _text(value["phase"], "phase", 128)
    return result


def _hash_limits(root: Path) -> dict:
    from .runtime import project_policy

    return project_policy(str(root))["policy"]["evidence_hash"]


def _artifact_identity(root: Path, reference: str) -> tuple:
    from .evidence import artifact_path

    path = artifact_path(root, reference)
    info = path.stat()
    return (str(path), info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def _verification_receipt(root: Path, records: list[dict], digests: dict) -> dict:
    artifacts = []
    for record in records:
        identity = _artifact_identity(root, record["reference"])
        if digests.get(identity) != record["sha256"]:
            raise ValueError("evidence artifact changed before recording its verification receipt")
        artifacts.append({**record, "identity": list(identity[1:])})
    return {"verified_at": datetime.now(timezone.utc).isoformat(), "artifacts": artifacts}


def _verify_receipt_metadata(root: Path, step: dict) -> None:
    """A passive observer checks prior proof identity without claiming a fresh byte read."""
    records = _evidence(step.get("evidence"))
    receipt = step.get("evidence_verification")
    if (
        not records
        or not isinstance(receipt, dict)
        or not isinstance(receipt.get("verified_at"), str)
        or not isinstance(receipt.get("artifacts"), list)
    ):
        raise ValueError("unverified: no retained byte-verification receipt")
    try:
        stamp = datetime.fromisoformat(receipt["verified_at"].replace("Z", "+00:00"))
    except ValueError as exc:
        raise ValueError("unverified: verification receipt timestamp is invalid") from exc
    if stamp.tzinfo is None:
        raise ValueError("unverified: verification receipt timestamp has no timezone")
    saved = {}
    for item in receipt["artifacts"]:
        if (
            not isinstance(item, dict)
            or set(item) != {"reference", "sha256", "identity"}
            or not isinstance(item["reference"], str)
            or not isinstance(item["identity"], list)
            or len(item["identity"]) != 5
            or any(type(value) is not int for value in item["identity"])
        ):
            raise ValueError("unverified: verification receipt identity is invalid")
        saved[item["reference"]] = item
    if len(saved) != len(records):
        raise ValueError("unverified: verification receipt does not cover the evidence")
    for record in records:
        prior = saved.get(record["reference"], {})
        if prior.get("sha256") != record["sha256"] or prior.get("identity") != list(
            _artifact_identity(root, record["reference"])[1:]
        ):
            raise ValueError(
                "evidence artifact metadata changed; explicit byte verification required"
            )


def _evidence(
    records: list[dict] | None,
    *,
    root: Path | None = None,
    digests: dict | None = None,
    deadline: float | None = None,
    max_bytes: int | None = None,
) -> list[dict]:
    records = records or []
    if not isinstance(records, list) or len(records) > 20:
        raise ValueError("evidence must be a list of at most 20 records")
    result = []
    digests = {} if digests is None else digests
    limits = (
        _hash_limits(root)
        if root is not None and (deadline is None or max_bytes is None)
        else {"max_seconds": EVIDENCE_HASH_SECONDS, "max_bytes": EVIDENCE_HASH_BYTES}
    )
    deadline = time.monotonic() + limits["max_seconds"] if deadline is None else deadline
    max_bytes = limits["max_bytes"] if max_bytes is None else max_bytes
    for record in records:
        if not isinstance(record, dict) or set(record) != {"reference", "sha256"}:
            raise ValueError("evidence records require reference and sha256")
        reference = _text(record["reference"], "evidence reference")
        digest = record["sha256"]
        if (
            not isinstance(digest, str)
            or len(digest) != 64
            or any(c not in "0123456789abcdef" for c in digest)
        ):
            raise ValueError("evidence sha256 must be a lowercase 64-character digest")
        if root is not None:
            from .evidence import _digest, artifact_path

            path = artifact_path(root, reference)
            before = path.stat()
            key = (
                str(path),
                before.st_dev,
                before.st_ino,
                before.st_size,
                before.st_mtime_ns,
                before.st_ctime_ns,
            )
            if key not in digests:
                measured = _digest(path, deadline=deadline, max_bytes=max_bytes)
                after = artifact_path(root, reference).stat()
                if (
                    before.st_dev,
                    before.st_ino,
                    before.st_size,
                    before.st_mtime_ns,
                    before.st_ctime_ns,
                ) != (
                    after.st_dev,
                    after.st_ino,
                    after.st_size,
                    after.st_mtime_ns,
                    after.st_ctime_ns,
                ):
                    raise ValueError("evidence artifact changed while hashing")
                digests[key] = measured
            if digests[key] != digest:
                raise ValueError("evidence artifact sha256 does not match retained bytes")
        result.append({"reference": reference, "sha256": digest})
    if len({record["reference"] for record in result}) != len(result):
        raise ValueError("evidence references must be unique within a checkpoint")
    return result


def _review(value: Any, execution_kind: str, state: str) -> dict[str, str] | None:
    if state != "succeeded" or execution_kind == "automatic":
        return None
    if not isinstance(value, dict) or set(value) != {"actor", "state", "reason"}:
        raise ValueError("manual and deliverable steps require an explicit approved review")
    if value["state"] != "approved":
        raise ValueError("manual and deliverable steps require an approved review")
    return {
        "actor": _text(value["actor"], "review actor", 128),
        "state": "approved",
        "reason": _text(value["reason"], "review reason", 512),
    }


def _rows(root: Path, view: dict | None = None) -> dict[str, dict[str, Any]]:
    if view is None:
        view = coverage_status(root)
    if view.get("state") == "not_initialized":
        return {}
    items = view.get("items")
    if not isinstance(items, list) or any(
        not isinstance(row, dict) or "id" not in row for row in items
    ):
        raise ValueError("initialized coverage status has invalid checklist items")
    return {row["id"]: row for row in items}


def _stale_dependencies(
    run: dict[str, Any],
    rows: dict[str, dict[str, Any]],
    *,
    root: Path | None = None,
    digests: dict | None = None,
    deadline: float | None = None,
    max_bytes: int | None = None,
    verify_evidence: bool = True,
) -> list[dict[str, str]]:
    """Return every changed dependency in deterministic recovery order."""
    stale: list[dict[str, str]] = []
    seen: set[str] = set()
    digests = {} if digests is None else digests
    limits = (
        _hash_limits(root)
        if root is not None and (deadline is None or max_bytes is None)
        else {"max_seconds": EVIDENCE_HASH_SECONDS, "max_bytes": EVIDENCE_HASH_BYTES}
    )
    deadline = time.monotonic() + limits["max_seconds"] if deadline is None else deadline
    max_bytes = limits["max_bytes"] if max_bytes is None else max_bytes

    def add(item_id: str, reason: str) -> None:
        if item_id not in seen:
            stale.append({"id": item_id, "reason": reason})
            seen.add(item_id)

    scenario_id = run["scenario"]["id"]
    scenario = rows.get(scenario_id)
    if scenario is None:
        add(scenario_id, "registered scenario or skill was removed")
    elif scenario["stale"]:
        add(scenario_id, scenario["reason"])
    elif _catalogue_hash(scenario_id) != run["scenario"].get("definition_hash"):
        add(scenario_id, "scenario or skill definition changed")
    prompt = run.get("context", {}).get("prompt")
    if prompt is not None and _catalogue_hash(prompt["id"]) != prompt.get("definition_hash"):
        add(prompt["id"], "registered prompt skill definition changed")
    for step in run["steps"]:
        if root is not None and step.get("state") == "succeeded":
            try:
                if verify_evidence:
                    records = _evidence(
                        step.get("evidence"),
                        root=root,
                        digests=digests,
                        deadline=deadline,
                        max_bytes=max_bytes,
                    )
                    if not records:
                        raise ValueError("successful workflow checkpoint has no retained evidence")
                else:
                    _verify_receipt_metadata(root, step)
            except (ValueError, OSError) as exc:
                add(step["id"], f"retained evidence unavailable: {exc}")
        row = rows.get(step["id"])
        if row is None:
            add(step["id"], "registered checklist step was removed")
        elif row["stale"]:
            add(step["id"], row["reason"])
        elif _catalogue_hash(step["id"]) != step.get("definition_hash"):
            add(step["id"], "registered step definition changed")
    return stale


def _reopen_index(run: dict[str, Any], stale: list[dict[str, str]]) -> int | None:
    """Find the first step whose result cannot remain valid after a change."""
    if not stale:
        return None
    affected = {item["id"] for item in stale}
    prompt = run.get("context", {}).get("prompt") or {}
    if run["scenario"]["id"] in affected or prompt.get("id") in affected:
        return 0
    return next((index for index, step in enumerate(run["steps"]) if step["id"] in affected), 0)


def _refresh_current_definition_hashes(
    run: dict[str, Any], rows: dict[str, dict[str, Any]]
) -> None:
    """Bind an explicitly reopened run to current registered definitions.

    A stale checklist row is deliberately not refreshed here: its own evidence
    has to be reconciled through the checklist first.  A prompt or a catalogue
    definition without a stale checklist row can be rebound only because the
    recovery has already reset its affected steps to pending.
    """
    scenario = rows.get(run["scenario"]["id"])
    if scenario is not None and not scenario["stale"]:
        run["scenario"]["definition_hash"] = _catalogue_hash(run["scenario"]["id"])
    prompt = run.get("context", {}).get("prompt")
    if prompt is not None:
        prompt["definition_hash"] = _catalogue_hash(prompt["id"])
    for step in run["steps"]:
        row = rows.get(step["id"])
        if row is not None and not row["stale"]:
            step["definition_hash"] = _catalogue_hash(step["id"])


def _stale_checklist_dependency(run: dict[str, Any], rows: dict[str, dict[str, Any]]) -> bool:
    """A stale checklist has to reconcile its own evidence before a workflow resumes."""
    identities = [run["scenario"]["id"], *(step["id"] for step in run["steps"])]
    return any(rows.get(item_id, {}).get("stale") for item_id in identities)


def _public_run(
    run: dict[str, Any],
    rows: dict[str, dict[str, Any]],
    *,
    root: Path | None = None,
    digests: dict | None = None,
    deadline: float | None = None,
    max_bytes: int | None = None,
    verify_evidence: bool = True,
) -> dict[str, Any]:
    item = copy.deepcopy(run)
    stale = _stale_dependencies(
        item,
        rows,
        root=root,
        digests=digests,
        deadline=deadline,
        max_bytes=max_bytes,
        verify_evidence=verify_evidence,
    )
    item["stale_dependencies"] = stale
    item["evidence_verification_mode"] = "bytes" if verify_evidence else "metadata_only"
    for step in item["steps"]:
        if step.get("state") == "succeeded":
            failure = next((row["reason"] for row in stale if row["id"] == step["id"]), None)
            step["evidence_status"] = {
                "state": "unverified"
                if failure and "unverified:" in failure
                else "unavailable"
                if failure and "budget" in failure
                else "stale"
                if failure
                else "verified"
                if verify_evidence
                else "metadata_matches",
                "verified_at": datetime.now(timezone.utc).isoformat()
                if verify_evidence and not failure
                else (step.get("evidence_verification") or {}).get("verified_at"),
                "observed_at": datetime.now(timezone.utc).isoformat(),
                "reason": failure,
            }
    next_action = next((step["id"] for step in item["steps"] if step["state"] == "pending"), None)
    reopened = _reopen_index(item, stale)
    if reopened is not None:
        item["recorded_state"] = item["state"]
        item["state"] = "stale"
        next_action = item["steps"][reopened]["id"]
    item["next_action"] = next_action
    return item


def start(
    directory: str | Path,
    *,
    scenario_id: str,
    steps: list[str],
    expected_revision: int = 0,
    context: dict[str, Any] | None = None,
) -> dict[str, Any]:
    root, project, document = _load(directory)
    if document["revision"] != expected_revision:
        raise ValueError("execution revision conflict")
    scenario_id = _text(scenario_id, "scenario_id")
    if (
        not isinstance(steps, list)
        or not steps
        or len(steps) > 200
        or len(set(steps)) != len(steps)
    ):
        raise ValueError("steps must be a unique nonempty list of at most 200 checklist ids")
    rows = _rows(root)
    scenario = rows.get(scenario_id)
    if scenario is None or scenario["kind"] not in {"scenario", "skill"}:
        raise ValueError("scenario_id must be a registered project scenario or skill")
    if any(step not in rows for step in steps):
        raise ValueError("every workflow step must be an existing checklist identity")
    run = {
        "id": f"run:{uuid.uuid4()}",
        "attempt": 1
        + sum(existing["scenario"]["id"] == scenario_id for existing in document["runs"]),
        "scenario": {"id": scenario_id, "definition_hash": _catalogue_hash(scenario_id)},
        "context": _context(context, root, project, rows),
        "phase": (context or {}).get("phase", "registered"),
        "steps": [
            {
                "id": step,
                "definition_hash": _catalogue_hash(step),
                "kind": rows[step]["kind"],
                "execution_kind": rows[step]["execution_kind"],
                "state": "pending",
                "evidence": [],
            }
            for step in steps
        ],
        "state": "running",
    }
    document["runs"].append(run)
    _save(root, document, expected_revision)
    return {
        "ok": True,
        "revision": document["revision"],
        "run": _public_run(run, rows, root=root),
        "next_action": steps[0],
    }


def checkpoint(
    directory: str | Path,
    *,
    run_id: str,
    step_id: str,
    state: str,
    evidence: list[dict] | None = None,
    expected_revision: int,
    review: dict[str, Any] | None = None,
    phase: str | None = None,
) -> dict[str, Any]:
    root, _, document = _load(directory)
    if document["revision"] != expected_revision:
        raise ValueError("execution revision conflict")
    if state not in _STEP_STATES:
        raise ValueError("checkpoint state is invalid")
    run = next((item for item in document["runs"] if item["id"] == run_id), None)
    # A blocked run has a non-success step that the operator may skip past: later steps proceed.
    if run is None or run["state"] not in {"running", "blocked"}:
        raise ValueError("workflow run is not running")
    step = next((item for item in run["steps"] if item["id"] == step_id), None)
    if step is None or step["state"] != "pending":
        raise ValueError("workflow step is not pending")
    if any(item["state"] == "pending" for item in run["steps"][: run["steps"].index(step)]):
        raise ValueError("workflow steps must be checkpointed in registered order")
    rows = _rows(root)
    digests: dict = {}
    limits = _hash_limits(root)
    deadline = time.monotonic() + limits["max_seconds"]
    stale = _stale_dependencies(
        run, rows, root=root, digests=digests, deadline=deadline, max_bytes=limits["max_bytes"]
    )
    current = rows.get(step_id)
    if state == "succeeded" and (
        stale or current is None or current["stale"] or current["blocked_by"]
    ):
        raise ValueError(
            "stale or blocked dependencies must be reconciled before a successful checkpoint"
        )
    records = _evidence(
        evidence,
        root=root if state == "succeeded" else None,
        digests=digests,
        deadline=deadline,
        max_bytes=limits["max_bytes"],
    )
    if state == "succeeded" and not records:
        raise ValueError("successful workflow checkpoints require exact evidence")
    step.update(state=state, evidence=records)
    if state == "succeeded":
        step["evidence_verification"] = _verification_receipt(root, records, digests)
    approved = _review(review, step["execution_kind"], state)
    if approved:
        step["review"] = approved
    if phase is not None:
        run["phase"] = _text(phase, "phase", 128)
    if state == "succeeded" and all(item["state"] == "succeeded" for item in run["steps"]):
        run["state"] = "completed"
    elif state != "succeeded":
        run["state"] = "interrupted" if state == "interrupted" else "blocked"
    _save(root, document, expected_revision)
    public = _public_run(
        run, rows, root=root, digests=digests, deadline=deadline, max_bytes=limits["max_bytes"]
    )
    return {
        "ok": True,
        "revision": document["revision"],
        "run": public,
        "next_action": public["next_action"],
    }


def resume(directory: str | Path, *, run_id: str, expected_revision: int) -> dict[str, Any]:
    """Deliberately reopen the first non-successful step after a handoff or interruption."""
    root, _, document = _load(directory)
    if document["revision"] != expected_revision:
        raise ValueError("execution revision conflict")
    run = next((item for item in document["runs"] if item["id"] == run_id), None)
    if run is None or run["state"] not in {"blocked", "interrupted", "completed"}:
        raise ValueError("workflow run is not resumable")
    rows = _rows(root)
    digests: dict = {}
    limits = _hash_limits(root)
    deadline = time.monotonic() + limits["max_seconds"]
    public = _public_run(
        run, rows, root=root, digests=digests, deadline=deadline, max_bytes=limits["max_bytes"]
    )
    if any("budget" in item["reason"] for item in public["stale_dependencies"]):
        raise ValueError("workflow evidence verification budget exceeded; retry before resuming")
    stale_index = _reopen_index(run, public["stale_dependencies"])
    if stale_index is not None:
        if _stale_checklist_dependency(run, rows):
            raise ValueError("reconcile stale checklist dependencies before resuming workflow")
        for step in run["steps"][stale_index:]:
            if step["evidence"]:
                step["stale_evidence"] = step["evidence"]
            step.update(state="pending", evidence=[])
            step.pop("review", None)
            step.pop("evidence_verification", None)
        _refresh_current_definition_hashes(run, rows)
        retry = run["steps"][stale_index]
    else:
        retry = next((step for step in run["steps"] if step["state"] != "succeeded"), None)
    if retry is None:
        raise ValueError("completed workflow run cannot be resumed")
    if stale_index is None:
        retry.update(state="pending", evidence=[])
        retry.pop("review", None)
        retry.pop("evidence_verification", None)
    run["state"] = "running"
    _save(root, document, expected_revision)
    return {
        "ok": True,
        "revision": document["revision"],
        "run": _public_run(
            run, rows, root=root, digests=digests, deadline=deadline, max_bytes=limits["max_bytes"]
        ),
        "next_action": retry["id"],
    }


def status(
    directory: str | Path, *, _coverage: dict | None = None, _verify_evidence: bool = True
) -> dict[str, Any]:
    root, _, document = _load(directory)
    rows = _rows(root, _coverage)
    digests: dict = {}
    limits = _hash_limits(root)
    deadline = time.monotonic() + limits["max_seconds"]
    runs = [
        _public_run(
            run,
            rows,
            root=root,
            digests=digests,
            deadline=deadline,
            max_bytes=limits["max_bytes"],
            verify_evidence=_verify_evidence,
        )
        for run in document["runs"]
    ]
    active = next((run for run in reversed(runs) if run["state"] == "running"), None)
    resumable = next(
        (run for run in reversed(runs) if run["state"] in {"blocked", "interrupted", "stale"}),
        None,
    )
    return {
        "ok": True,
        "revision": document["revision"],
        "runs": runs,
        "next_action": active["next_action"]
        if active
        else resumable["next_action"]
        if resumable
        else None,
        "resumable_run": resumable["id"] if resumable else None,
    }


def execute(
    directory: str | Path,
    *,
    scenario_id: str,
    steps: list[str],
    outcomes: list[dict],
    context: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Checkpoint supplied local outcomes; this helper executes no commands or network work."""
    started = start(
        directory,
        scenario_id=scenario_id,
        steps=steps,
        context=context,
        expected_revision=status(directory)["revision"],
    )
    run, revision = started["run"], started["revision"]
    if not isinstance(outcomes, list) or [item.get("id") for item in outcomes] != steps:
        raise ValueError("outcomes must cover registered steps once in declared order")
    for outcome in outcomes:
        result = checkpoint(
            directory,
            run_id=run["id"],
            step_id=outcome.get("id"),
            state=outcome.get("state"),
            evidence=outcome.get("evidence"),
            review=outcome.get("review"),
            phase=outcome.get("phase"),
            expected_revision=revision,
        )
        revision = result["revision"]
        if result["run"]["state"] != "running":
            return result
    return status(directory)
