"""Offline, fact-backed priority preview and explicit coverage policy application."""

from __future__ import annotations

import copy
import json
from importlib import resources
from typing import Any

from .catalogue import load_catalogue
from .coverage import (
    FORMAT,
    FORMAT_V2,
    _hash,
    _now,
    _read,
    _set,
    _transaction,
    coverage_status,
)
from .workspace import _load

_PRIORITIES = {"P0", "P1", "P2"}


def _matches_fact(value: Any, choices: list[str]) -> bool:
    """Compare saved string facts case-insensitively; punctuation remains significant."""
    return type(value) is str and value.casefold() in {choice.casefold() for choice in choices}


def _policy(value: dict[str, Any] | None, catalogue: dict[str, Any]) -> dict[str, Any]:
    if value is None:
        value = json.loads(
            resources.files("seohead").joinpath("data/project_priorities.json").read_text()
        )
    if not isinstance(value, dict) or set(value) != {"format", "rules"}:
        raise ValueError("priority policy has unsupported fields")
    if value["format"] != "seohead.project-priorities.v1" or not isinstance(value["rules"], list):
        raise ValueError("unsupported priority policy")
    if len(value["rules"]) > 100:
        raise ValueError("priority policy has too many rules")
    seen = set()
    normalized = []
    for rule in value["rules"]:
        if not isinstance(rule, dict) or set(rule) != {"id", "facts", "priority", "items"}:
            raise ValueError("priority rule has unsupported fields")
        rule_id = rule["id"]
        if not isinstance(rule_id, str) or not rule_id or len(rule_id) > 128 or rule_id in seen:
            raise ValueError("priority rule ID is invalid or duplicated")
        seen.add(rule_id)
        if type(rule["priority"]) is not str or rule["priority"] not in _PRIORITIES:
            raise ValueError("priority rule has invalid priority")
        # An empty fact map is an explicit baseline rule.  It is not an absent
        # condition: it deliberately matches every initialized checklist.
        if not isinstance(rule["facts"], dict) or len(rule["facts"]) > 16:
            raise ValueError("priority rule facts are invalid")
        facts = {}
        for name, values in rule["facts"].items():
            if not isinstance(name, str) or not name or not isinstance(values, list) or not values:
                raise ValueError("priority rule facts are invalid")
            if any(type(item) is not str or not item or len(item) > 128 for item in values):
                raise ValueError("priority rule fact values are invalid")
            facts[name] = sorted(set(values))
        if not isinstance(rule["items"], list) or not rule["items"] or len(rule["items"]) > 100:
            raise ValueError("priority rule items are invalid")
        if any(type(item) is not str or item not in catalogue for item in rule["items"]):
            raise ValueError("priority rule references an unknown catalogue item")
        normalized.append(
            {
                "id": rule_id,
                "facts": facts,
                "priority": rule["priority"],
                "items": sorted(set(rule["items"])),
            }
        )
    return {"format": value["format"], "rules": normalized}


def _facts(project: dict[str, Any]) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    values: dict[str, Any] = {}
    snapshot = []
    for fact in project["facts"]:
        name = fact["name"]
        if name in values and values[name] != fact["value"]:
            raise ValueError(f"conflicting project fact: {name}")
        values[name] = fact["value"]
        snapshot.append(copy.deepcopy(fact))
    return values, sorted(snapshot, key=lambda fact: fact["name"])


def _decisions(
    document: dict[str, Any], policy: dict[str, Any], facts: dict[str, Any]
) -> list[dict[str, Any]]:
    matched: dict[str, list[dict[str, Any]]] = {}
    for rule in policy["rules"]:
        if all(_matches_fact(facts.get(name), values) for name, values in rule["facts"].items()):
            for item_id in rule["items"]:
                matched.setdefault(item_id, []).append(rule)
    decisions = []
    for item_id, item in sorted(document["items"].items()):
        definition = item["definition"]
        before = {
            "priority": definition["priority"],
            "priority_origin": definition["priority_origin"],
        }
        rules = matched.get(item_id, [])
        if definition["priority_origin"] == "operator":
            after = before
            reason = "operator priority preserved"
            consulted: list[str] = []
        elif not rules:
            after = (
                {"priority": "P1", "priority_origin": "default"}
                if definition["priority_origin"] == "policy"
                else before
            )
            reason = (
                "policy no longer matched saved facts"
                if definition["priority_origin"] == "policy"
                else "no policy rule matched saved facts"
            )
            consulted = []
        else:
            priorities = {rule["priority"] for rule in rules}
            if len(priorities) != 1:
                raise ValueError(f"conflicting priority rules for {item_id}")
            priority = priorities.pop()
            after = {"priority": priority, "priority_origin": "policy"}
            consulted = sorted({name for rule in rules for name in rule["facts"]})
            reason = "matched " + ", ".join(rule["id"] for rule in rules)
        decisions.append(
            {
                "id": item_id,
                "before": before,
                "after": after,
                "reason": reason,
                "consulted_facts": consulted,
            }
        )
    return decisions


def _receipt(
    policy: dict[str, Any], facts: list[dict[str, Any]], decisions: list[dict[str, Any]]
) -> dict[str, Any]:
    return {
        "policy": policy,
        "policy_hash": _hash(policy),
        "facts": facts,
        "decisions": decisions,
        "item_ids": sorted(decision["id"] for decision in decisions),
        "decision_fingerprint": [
            {
                "id": decision["id"],
                "after": decision["after"],
                "reason": decision["reason"],
                "consulted_facts": decision["consulted_facts"],
            }
            for decision in decisions
        ],
    }


def project_priorities(
    directory: str,
    policy: dict[str, Any] | None = None,
    apply: bool = False,
    expected_revision: int | None = None,
    full: bool = False,
) -> dict[str, Any]:
    """Preview or explicitly persist offline priority decisions from saved project facts."""
    if type(apply) is not bool:
        raise ValueError("apply must be a boolean")
    root, project = _load(directory)
    document = _read(root, project)
    if document is None:
        raise ValueError("initialize the checklist first")
    if apply and type(expected_revision) is not int:
        raise ValueError("expected_revision must be an integer when apply=true")
    if not apply and expected_revision is not None and type(expected_revision) is not int:
        raise ValueError("expected_revision must be an integer")
    catalogue = load_catalogue()
    selected = _policy(policy, catalogue)
    values, snapshot = _facts(project)
    decisions = _decisions(document, selected, values)
    receipt = _receipt(selected, snapshot, decisions)
    result = {
        "ok": True,
        "applied": False,
        "base_revision": document["revision"],
        "policy_hash": receipt["policy_hash"],
        "decisions": decisions,
        "checklist": coverage_status(directory),
    }
    if not full:
        # The default answer is bounded (#1398): counts, the changed decisions and the
        # item ids; the full per-check decision list is one --full away.
        changed = [d for d in decisions if d["before"] != d["after"]]
        result["decision_counts"] = {
            "total": len(decisions),
            "changed": len(changed),
            "unchanged": len(decisions) - len(changed),
        }
        result["changed_decisions"] = changed
        result["item_ids"] = receipt["item_ids"]
        del result["decisions"]
    if not apply:
        return result
    with _transaction(directory, expected_revision) as (_, locked_project, writable):
        current_values, current_snapshot = _facts(locked_project)
        current_decisions = _decisions(writable, selected, current_values)
        current_receipt = _receipt(selected, current_snapshot, current_decisions)
        history = writable.get("priority_policy", {}).get("applications", [])
        prior = history[-1].get("receipt") if history else None
        if prior and {
            key: prior.get(key) for key in ("policy_hash", "facts", "decision_fingerprint")
        } == {
            key: current_receipt[key] for key in ("policy_hash", "facts", "decision_fingerprint")
        }:
            return result
        for decision in current_decisions:
            definition = writable["items"][decision["id"]]["definition"]
            if decision["before"] != decision["after"]:
                _set(
                    writable["items"],
                    {
                        **definition,
                        "priority": decision["after"]["priority"],
                        "priority_origin": decision["after"]["priority_origin"],
                    },
                )
        writable["priority_policy"] = {
            "applications": [
                *history,
                {"applied_at": _now(), "receipt": current_receipt},
            ]
        }
        if writable["format"] == FORMAT:
            writable["format"] = FORMAT_V2
            writable["version"] = 2
        result["applied"] = True
        result["decisions"] = current_decisions
        result["policy_hash"] = current_receipt["policy_hash"]
    result["checklist"] = coverage_status(directory)
    result["base_revision"] = result["checklist"]["revision"] - 1
    return result
