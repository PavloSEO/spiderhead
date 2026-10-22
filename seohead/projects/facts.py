"""Record project stack facts, keeping operator decisions above detected evidence.

The priority policy already reads ``project.json`` facts, but nothing could write
one after ``project new``: a stack had to be typed in by hand. This closes that
link. An operator-entered fact is a decision and a detected fact is evidence, so a
detection never overwrites an operator's value, and both keep saying which they are
through the ``provenance`` and ``observed_at`` fields the fact schema already has.
"""

from __future__ import annotations

from typing import Any

from .coverage import _now
from .workspace import _facts, _load

# ``provenance`` is the only field that carries a source, so the operator/detected
# distinction has to survive a round trip through project.json inside it. A detection
# stamps this prefix; supplied facts are refused if they claim it.
DETECTED_PREFIX = "detected by tech-detect"
# One fact per tech-detect category the packaged priority policy consults. The fact
# name is the category name, so there is no translation table to drift.
DETECTED_CATEGORIES = ("cms", "framework")
# The versioned label a meta generator tag contributes is registered as a second CMS
# under the same category; it is the same product, not a second candidate.
_GENERATOR_EVIDENCE = "meta name=generator"
_MAX_PROVENANCE = 512
_MAX_VALUE = 2048


def _is_detected(fact: dict[str, Any]) -> bool:
    return str(fact.get("provenance", "")).startswith(DETECTED_PREFIX)


def _supplied(value: Any) -> list[dict[str, Any]]:
    """Validate operator-supplied facts with the same schema project creation uses."""
    facts = _facts(value)
    claimed = sorted({fact["name"] for fact in facts if _is_detected(fact)})
    if claimed:
        raise ValueError(
            "supplied facts must not claim detection provenance: " + ", ".join(claimed)
        )
    names = [fact["name"] for fact in facts]
    if len(set(names)) != len(names):
        raise ValueError("supplied facts must not repeat a name")
    return facts


def _detect(tools: dict[str, Any], target: str) -> dict[str, Any]:
    """Fetch one page through the shared tools and read the stack off it.

    Two requests at most, robots.txt first: the single-page recon tools carry the
    repository's pinning transport, and reading the rules before the page is what
    keeps this bounded step inside the crawl contract the rest of the project obeys.
    An unreadable robots.txt is a refusal to fetch, not a permission.
    """
    for name in ("robots_check", "tech_detect"):
        if not callable(tools.get(name)):
            raise ValueError(f"detection requires an injected {name} tool")
    report: dict[str, Any] = {
        "state": "unavailable",
        "url": target,
        "observed_at": _now(),
        "request_budget": 2,
        "facts": [],
        "unavailable": [],
    }
    from seohead.checks.robots import match_path

    robots = tools["robots_check"](url=target, paths=[match_path(target)])
    if not isinstance(robots, dict) or not robots.get("ok"):
        error = robots.get("error") if isinstance(robots, dict) else "invalid robots result"
        report["reason"] = f"robots.txt could not be read, so nothing was fetched: {error}"
        return report
    if any(not row.get("allowed") for row in robots.get("path_checks") or []):
        report["reason"] = f"robots.txt disallows fetching {target}"
        return report
    detected = tools["tech_detect"](url=target)
    if not isinstance(detected, dict) or not detected.get("ok"):
        error = detected.get("error") if isinstance(detected, dict) else "invalid detection result"
        report["reason"] = f"tech-detect was unavailable: {error}"
        return report
    by_category = detected.get("by_category")
    by_category = by_category if isinstance(by_category, dict) else {}
    for name in DETECTED_CATEGORIES:
        rows = [
            row
            for row in by_category.get(name) or []
            if isinstance(row, dict) and isinstance(row.get("name"), str) and row["name"]
        ]
        candidates = [row for row in rows if row.get("evidence") != _GENERATOR_EVIDENCE] or rows
        if not candidates:
            report["unavailable"].append(
                {"name": name, "reason": f"no {name} signature matched the fetched page"}
            )
        elif len(candidates) > 1:
            named = ", ".join(sorted(row["name"] for row in candidates))
            report["unavailable"].append(
                {
                    "name": name,
                    "reason": f"{len(candidates)} {name} candidates matched ({named}); not guessed",
                }
            )
        else:
            evidence = candidates[0].get("evidence")
            evidence = evidence if isinstance(evidence, str) and evidence else "signature match"
            report["facts"].append(
                {
                    "name": name,
                    "value": candidates[0]["name"][:_MAX_VALUE],
                    "provenance": f"{DETECTED_PREFIX} at {target}: {evidence}"[:_MAX_PROVENANCE],
                    "observed_at": report["observed_at"],
                }
            )
    report["state"] = "partial" if report["unavailable"] else "run"
    report["reason"] = "one page fetched after reading robots.txt"
    return report


def _replace(facts: list[dict[str, Any]], fact: dict[str, Any]) -> None:
    """Keep one entry per name in its original position; a fact is a current value."""
    positions = [index for index, row in enumerate(facts) if row["name"] == fact["name"]]
    if not positions:
        facts.append(fact)
        return
    facts[positions[0]] = fact
    for index in reversed(positions[1:]):
        del facts[index]


def _merge(
    current: list[dict[str, Any]],
    supplied: list[dict[str, Any]],
    detected: list[dict[str, Any]],
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    result = [dict(fact) for fact in current]
    changes: list[dict[str, Any]] = []
    for fact in supplied:
        previous = next((row for row in result if row["name"] == fact["name"]), None)
        _replace(result, fact)
        changes.append(
            {
                "name": fact["name"],
                "action": "recorded" if previous is None else "updated",
                "value": fact["value"],
                "provenance": fact["provenance"],
                "reason": "operator-entered decision",
            }
        )
    for fact in detected:
        previous = next((row for row in result if row["name"] == fact["name"]), None)
        if previous is not None and not _is_detected(previous):
            changes.append(
                {
                    "name": fact["name"],
                    "action": "kept_operator",
                    "value": previous["value"],
                    "provenance": previous["provenance"],
                    "detected_value": fact["value"],
                    "reason": "an operator-entered fact is a decision; detection is evidence",
                }
            )
            continue
        if previous is None:
            action = "recorded"
        elif previous["value"] == fact["value"]:
            action = "confirmed"
        else:
            action = "updated"
        _replace(result, fact)
        changes.append(
            {
                "name": fact["name"],
                "action": action,
                "value": fact["value"],
                "provenance": fact["provenance"],
                "reason": "detected evidence",
            }
        )
    return result, changes


def project_facts(
    directory: str,
    facts: list[dict[str, Any]] | None = None,
    detect: bool = False,
    apply: bool = False,
    *,
    tools: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Preview or explicitly record project facts, optionally from one tech-detect run.

    ``detect`` is the only thing that makes a request: without it this is offline, and
    nothing here runs implicitly from another command. ``apply`` is what writes; the
    default remains an inspectable preview, like the other project policy surfaces.
    """
    if type(detect) is not bool or type(apply) is not bool:
        raise ValueError("detect and apply must be booleans")
    root, project = _load(directory)
    supplied = _supplied(facts) if facts is not None else []
    if not supplied and not detect:
        # No input asked for the saved facts (#1400): an empty list for a new project is
        # the honest answer, same contract as the other project read commands.
        raw = project["facts"]
        saved = (
            sorted(
                raw,
                key=lambda fact: str(fact.get("name", "")),
            )
            if isinstance(raw, list)
            else []
        )
        return {
            "ok": True,
            "state": "saved",
            "facts": saved,
            "count": len(saved),
        }
    detection = (
        _detect(tools or {}, project["site"]["target"])
        if detect
        else {
            "state": "not_run",
            "reason": "detection was not requested; no request was made",
            "facts": [],
            "unavailable": [],
        }
    )
    merged, changes = _merge(project["facts"], supplied, detection["facts"])
    validated = _facts(merged)
    result = {
        "ok": True,
        "applied": False,
        "path": str(root),
        "facts": validated,
        "changes": changes,
        "detection": detection,
        "reason": "preview only; pass apply=true to record these facts",
    }
    if validated == project["facts"]:
        result["reason"] = "saved facts already match; nothing to record"
        return result
    if not apply:
        return result
    from .runtime import write_document

    write_document(root, "project.json", {**project, "facts": validated})
    result["applied"] = True
    result["reason"] = "facts recorded; apply project priorities to act on them"
    return result
