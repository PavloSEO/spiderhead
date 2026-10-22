"""Portable checklist definitions, append-only history and offline completion views."""

from __future__ import annotations

import copy
import hashlib
import json
import os
import re
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from seohead.core.common import utc_iso_z as _now
from seohead.core.filesystem import atomic_write_bytes

from .catalogue import load_catalogue
from .workspace import _facts, _load, _target

FORMAT = "seohead.coverage.v1"
FORMAT_V2 = "seohead.coverage.v2"
FORMAT_V3 = "seohead.coverage.v3"
FORMAT_V4 = "seohead.coverage.v4"
MAX_BYTES = 32 * 1024 * 1024
URL_ENUMERATION_LIMIT = 10000
_ID = re.compile(
    r"(?:check:[A-Z][A-Z0-9_]*|skill:(?:workflow|general)/[a-z0-9_-]+|scenario:[a-z0-9_-]+|custom:[a-z][a-z0-9._/-]{0,127})\Z"
)


def _hash(value: Any) -> str:
    try:
        encoded = json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False)
    except (TypeError, ValueError) as exc:
        raise ValueError("coverage value is not canonical JSON") from exc
    return hashlib.sha256(encoded.encode()).hexdigest()


def _completion_hash(definition: dict) -> str:
    """Hash evidence-bearing definition fields, excluding scheduling choices."""
    return _hash(
        {
            key: value
            for key, value in definition.items()
            if key not in {"priority", "priority_origin", "order", "enabled", "assignee"}
            and not (
                key
                in {
                    "description",
                    "acceptance_criterion",
                    "finding_ref",
                    "method_ref",
                    "competitors",
                }
                and value is None
            )
            and not (key == "competitors" and value == [])
        }
    )


def _text(value: Any, name: str, limit: int = 2048) -> str:
    if not isinstance(value, str) or not value.strip() or len(value) > limit:
        raise ValueError(f"{name} must be nonempty text of at most {limit} characters")
    return value


def _identifier(value: Any) -> str:
    if not isinstance(value, str) or not _ID.fullmatch(value) or ".." in value:
        raise ValueError("invalid checklist item identifier")
    return value


def _urls(urls: Any, site: str, label: str) -> list[str]:
    if not isinstance(urls, list) or len(urls) > URL_ENUMERATION_LIMIT:
        raise ValueError(f"{label} must be a bounded list")
    from urllib.parse import urlsplit

    from seohead.recon.net import normalize_url

    for url in urls:
        if not isinstance(url, str) or len(url) > 2048 or normalize_url(url) != url:
            raise ValueError(f"{label} must be normalized absolute URLs")
        if urlsplit(url).netloc != urlsplit(site).netloc:
            raise ValueError(f"{label} belongs to another site")
    if len(set(urls)) != len(urls):
        raise ValueError(f"duplicate URL in {label}")
    return urls


def _scope(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != {"site", "template", "urls"}:
        raise ValueError("scope requires site, template and urls")
    site = _target(value["site"])
    template = value["template"]
    if template is not None:
        _text(template, "template", 128)
    return {"site": site, "template": template, "urls": _urls(value["urls"], site, "scope urls")}


_POPULATION_REQUIRED = ("kind", "source")
_POPULATION_DEFAULTS = {"size": None, "urls": [], "name": None, "reason": None, "templates": None}


def _population(value: Any, site: str, *, nested: bool = False) -> dict[str, Any]:
    """Validate one agreed URL population against the project site identity.

    Only ``kind`` and ``source`` are required; the other keys default to unset, so a sample
    needs just its kind, size, name and source. Missing and unknown keys are named in the error.
    """
    if not isinstance(value, dict):
        raise ValueError("population must be an object")
    allowed = {"kind", "size", "urls", "name", "source", "reason"} | (
        set() if nested else {"templates"}
    )
    missing = [key for key in _POPULATION_REQUIRED if key not in value]
    if missing:
        raise ValueError("population is missing required key(s): " + ", ".join(missing))
    unknown = sorted(set(value) - allowed, key=str)
    if unknown:
        raise ValueError(
            "population has unsupported key(s): "
            + ", ".join(map(str, unknown))
            + "; allowed: "
            + ", ".join(sorted(allowed))
        )
    value = {**_POPULATION_DEFAULTS, **value}
    kind = value["kind"]
    kinds = {"complete_set", "sample"} if nested else {"complete_set", "sample", "unknown"}
    if kind not in kinds:
        raise ValueError(
            "population kind must be complete_set, sample" + ("" if nested else " or unknown")
        )
    urls = _urls(value["urls"], site, "population urls")
    size = value["size"]
    name = value["name"]
    if kind == "unknown":
        if size is not None or urls or name is not None:
            raise ValueError("an unknown population has no size, urls or name")
        _text(value["reason"], "unknown population reason", 512)
    else:
        if urls:
            if size is not None and size != len(urls):
                raise ValueError("population size disagrees with its enumerated urls")
            size = len(urls)
        elif size is None:
            raise ValueError("population is missing required key(s): size (or urls)")
        if type(size) is not int or not 1 <= size <= 1_000_000_000:
            raise ValueError("population size must be a positive bounded integer")
        if kind == "sample":
            _text(name, "sample name", 128)
        elif name is not None:
            raise ValueError("only a sample population is named")
        if value["reason"] is not None:
            _text(value["reason"], "population reason", 512)
    _text(value["source"], "population source", 512)
    result = {
        "kind": kind,
        "size": size,
        "urls": urls,
        "name": name,
        "source": value["source"],
        "reason": value["reason"],
    }
    if not nested:
        templates = value["templates"]
        if templates is None:
            templates = {}
        if not isinstance(templates, dict) or len(templates) > 100:
            raise ValueError("population templates must be a bounded object")
        result["templates"] = {
            _text(key, "template population name", 128): _population(entry, site, nested=True)
            for key, entry in templates.items()
        }
        if size is not None:
            members = set(urls)
            for template_name, entry in result["templates"].items():
                if urls and not set(entry["urls"]) <= members:
                    raise ValueError(
                        f"template population {template_name!r} declares URLs outside "
                        "the enumerated site population"
                    )
                if entry["size"] > size:
                    raise ValueError(
                        f"template population {template_name!r} is larger than the "
                        "agreed site population"
                    )
                members |= set(entry["urls"])
            if len(members) > size:
                raise ValueError(
                    "template populations declare more URLs than the agreed site population"
                )
    return result


def _tasks(value: Any) -> dict[str, Any]:
    """Validate the agreed task set: the whole checklist or a sourced item selection."""
    if value is None or value == {"kind": "all_agreed"}:
        return {"kind": "all_agreed"}
    if not isinstance(value, dict) or set(value) != {"kind", "ids", "source"}:
        raise ValueError("task agreement is all_agreed or a sourced selection of item ids")
    if value["kind"] != "selection":
        raise ValueError("task agreement kind must be all_agreed or selection")
    ids = value["ids"]
    if not isinstance(ids, list) or not ids or len(ids) > 10000:
        raise ValueError("task selection must be a nonempty bounded list")
    for item_id in ids:
        _identifier(item_id)
    if len(set(ids)) != len(ids):
        raise ValueError("duplicate agreed task id")
    _text(value["source"], "task selection source", 512)
    return {"kind": "selection", "ids": sorted(ids), "source": value["source"]}


def _plan(value: Any, site: str) -> dict[str, Any]:
    """Validate the agreed audit scope recorded for a checklist."""
    if (
        not isinstance(value, dict)
        or not {"reviewer", "population"} <= set(value)
        or set(value) - {"reviewer", "population", "tasks"}
    ):
        raise ValueError("plan requires reviewer and population")
    return {
        "reviewer": _text(value["reviewer"], "plan reviewer", 128),
        "population": _population(value["population"], site),
        "tasks": _tasks(value.get("tasks")),
    }


def _stored_plan(value: Any, site: str) -> dict[str, Any]:
    if (
        not isinstance(value, dict)
        or not {"recorded_at", "revision", "reviewer", "site", "population"} <= set(value)
        or set(value) - {"recorded_at", "revision", "reviewer", "site", "population", "tasks"}
    ):
        raise ValueError("invalid audit scope plan")
    _text(value["recorded_at"], "plan recording time", 128)
    if type(value["revision"]) is not int or value["revision"] < 1:
        raise ValueError("invalid plan revision")
    _text(value["reviewer"], "plan reviewer", 128)
    if value["site"] != site:
        raise ValueError("plan site identity does not match the project")
    _population(value["population"], site)
    value["tasks"] = _tasks(value.get("tasks"))
    return value


def _definition(value: Any, catalogue: dict, *, historical: bool = False) -> dict:
    required = {
        "id",
        "title",
        "kind",
        "scope",
        "dependencies",
        "execution_kind",
        "priority",
        "priority_origin",
        "enabled",
        "order",
        "operation",
        "source_hash",
    }
    task_fields = {
        "assignee",
        "description",
        "acceptance_criterion",
        "finding_ref",
        "method_ref",
        "competitors",
    }
    if not isinstance(value, dict) or frozenset(value) not in {
        frozenset(required),
        frozenset(required | task_fields),
    }:
        raise ValueError("item definition has unsupported fields")
    value = copy.deepcopy(value)
    item_id = _identifier(value["id"])
    _text(value["title"], "title", 512)
    if (
        not isinstance(value["kind"], str)
        or value["kind"] not in {"check", "skill", "scenario", "custom"}
        or not item_id.startswith(value["kind"] + ":")
    ):
        raise ValueError("item kind does not match identifier")
    if not historical and value["kind"] != "custom" and item_id not in catalogue:
        raise ValueError("unknown built-in item")
    value["scope"] = _scope(value["scope"])
    if not isinstance(value["execution_kind"], str) or value["execution_kind"] not in {
        "automatic",
        "manual",
        "deliverable",
    }:
        raise ValueError("invalid execution kind")
    if not isinstance(value["priority"], str) or value["priority"] not in {"P0", "P1", "P2"}:
        raise ValueError("invalid priority")
    if not isinstance(value["priority_origin"], str) or value["priority_origin"] not in {
        "default",
        "operator",
        "policy",
    }:
        raise ValueError("invalid priority origin")
    if (
        type(value["enabled"]) is not bool
        or type(value["order"]) is not int
        or not 0 <= value["order"] <= 1000000
    ):
        raise ValueError("invalid enabled/order value")
    dependencies = value["dependencies"]
    if not isinstance(dependencies, list) or len(dependencies) > 1000:
        raise ValueError("dependencies must be a bounded list")
    for dep in dependencies:
        _identifier(dep)
    if item_id in dependencies or len(set(dependencies)) != len(dependencies):
        raise ValueError("duplicate or self dependency")
    operation = value["operation"]
    if value["execution_kind"] == "automatic":
        if (
            not isinstance(operation, str)
            or not operation.startswith("check:")
            or (
                not historical
                and (operation not in catalogue or catalogue[operation]["kind"] != "check")
            )
        ):
            raise ValueError("automatic items bind to a registered check")
    elif operation is not None:
        raise ValueError("manual/deliverable items do not execute operations")
    if value["source_hash"] is not None and (
        not isinstance(value["source_hash"], str)
        or not re.fullmatch("[a-f0-9]{64}", value["source_hash"])
    ):
        raise ValueError("invalid source definition hash")
    if task_fields <= set(value):
        for field in ("assignee", "description", "acceptance_criterion"):
            field_value = value[field]
            if field_value is not None:
                _text(field_value, field, 2048 if field != "assignee" else 128)
        finding_ref = value["finding_ref"]
        if finding_ref is not None:
            if not isinstance(finding_ref, dict) or set(finding_ref) != {"scan_id", "finding_id"}:
                raise ValueError("finding_ref requires scan_id and finding_id")
            _text(finding_ref["scan_id"], "finding_ref scan_id", 128)
            _text(finding_ref["finding_id"], "finding_ref finding_id", 256)
        method_ref = value["method_ref"]
        if method_ref is not None and (
            not isinstance(method_ref, str)
            or not method_ref.startswith(("skill:", "scenario:"))
            or len(method_ref) > 256
        ):
            raise ValueError("method_ref must identify a registered skill or scenario")
        competitors = value["competitors"]
        if not isinstance(competitors, list) or len(competitors) > 20:
            raise ValueError("competitors must be a bounded list of at most 20 sites")
        identities = set()
        from urllib.parse import urlsplit

        for competitor in competitors:
            if not isinstance(competitor, dict) or set(competitor) != {"url", "max_urls"}:
                raise ValueError("each competitor requires url and max_urls")
            url = _text(competitor["url"], "competitor URL", 2048)
            normalized = _target(url)
            parsed = urlsplit(normalized)
            if parsed.scheme not in {"http", "https"} or not parsed.hostname:
                raise ValueError("competitor URL must be an HTTP(S) site")
            primary = urlsplit(_target(value["scope"]["site"]))
            if parsed.netloc.casefold() == primary.netloc.casefold():
                raise ValueError("the task's primary site cannot also be a competitor")
            if normalized in identities:
                raise ValueError("duplicate competitor URL")
            identities.add(normalized)
            if (
                type(competitor["max_urls"]) is not int
                or not 1 <= competitor["max_urls"] <= 1_000_000
            ):
                raise ValueError("competitor max_urls must be 1..1,000,000")
    return value


def _dependencies(items: dict) -> None:
    done, active = set(), set()

    def visit(item_id: str) -> None:
        if item_id in active:
            raise ValueError("cyclic checklist dependency")
        if item_id in done:
            return
        if item_id not in items:
            raise ValueError(f"unknown dependency {item_id}")
        if len(active) >= 100:
            raise ValueError("dependency chain exceeds supported depth")
        active.add(item_id)
        for dep in items[item_id]["definition"]["dependencies"]:
            visit(dep)
        active.remove(item_id)
        done.add(item_id)

    for item_id in items:
        visit(item_id)


def _priority(value: Any, label: str) -> None:
    if type(value) is not str or value not in {"P0", "P1", "P2"}:
        raise ValueError(f"invalid {label} priority")


def _priority_origin(value: Any, label: str) -> None:
    if type(value) is not str or value not in {"default", "operator", "policy"}:
        raise ValueError(f"invalid {label} priority origin")


def _application_time(value: Any) -> None:
    _text(value, "priority policy application time", 128)
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as exc:
        raise ValueError("priority policy application time must be RFC3339 UTC") from exc
    if parsed.tzinfo is None or parsed.utcoffset() != timezone.utc.utcoffset(parsed):
        raise ValueError("priority policy application time must be RFC3339 UTC")


def _historical_policy(value: Any) -> None:
    if not isinstance(value, dict) or set(value) != {"format", "rules"}:
        raise ValueError("invalid priority policy receipt")
    if value["format"] != "seohead.project-priorities.v1" or not isinstance(value["rules"], list):
        raise ValueError("invalid priority policy receipt")
    if len(value["rules"]) > 100:
        raise ValueError("invalid priority policy receipt")
    ids = set()
    for rule in value["rules"]:
        if not isinstance(rule, dict) or set(rule) != {"id", "facts", "priority", "items"}:
            raise ValueError("invalid priority policy receipt")
        if (
            type(rule["id"]) is not str
            or not rule["id"]
            or len(rule["id"]) > 128
            or rule["id"] in ids
        ):
            raise ValueError("invalid priority policy receipt")
        ids.add(rule["id"])
        _priority(rule["priority"], "priority rule")
        facts = rule["facts"]
        if not isinstance(facts, dict) or len(facts) > 16:
            raise ValueError("invalid priority policy receipt")
        for name, values in facts.items():
            if (
                type(name) is not str
                or not re.fullmatch(r"[a-z][a-z0-9_]{0,63}", name)
                or not isinstance(values, list)
                or not values
                or len(values) > 100
                or any(type(item) is not str or not item or len(item) > 128 for item in values)
            ):
                raise ValueError("invalid priority policy receipt")
            if len(values) != len(set(values)):
                raise ValueError("invalid priority policy receipt")
        items = rule["items"]
        if not isinstance(items, list) or not items or len(items) > 100:
            raise ValueError("invalid priority policy receipt")
        if any(type(item) is not str for item in items) or len(items) != len(set(items)):
            raise ValueError("invalid priority policy receipt")
        for item_id in items:
            _identifier(item_id)


def _receipt_fingerprint(decisions: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return [
        {
            "id": decision["id"],
            "after": decision["after"],
            "reason": decision["reason"],
            "consulted_facts": decision["consulted_facts"],
        }
        for decision in decisions
    ]


def _priority_receipt(value: Any, item_ids: set[str]) -> None:
    fields = {"policy", "policy_hash", "facts", "decisions", "decision_fingerprint"}
    if not isinstance(value, dict) or set(value) not in (fields, fields | {"item_ids"}):
        raise ValueError("invalid priority policy receipt")
    _historical_policy(value["policy"])
    if type(value["policy_hash"]) is not str or value["policy_hash"] != _hash(value["policy"]):
        raise ValueError("invalid priority policy receipt")
    facts = _facts(value["facts"])
    if facts != value["facts"]:
        raise ValueError("invalid priority policy receipt")
    decisions = value["decisions"]
    if not isinstance(decisions, list) or len(decisions) > len(item_ids):
        raise ValueError("invalid priority policy receipt")
    seen = set()
    for decision in decisions:
        if not isinstance(decision, dict) or set(decision) != {
            "id",
            "before",
            "after",
            "reason",
            "consulted_facts",
        }:
            raise ValueError("invalid priority policy receipt")
        item_id = decision["id"]
        if type(item_id) is not str or item_id not in item_ids or item_id in seen:
            raise ValueError("invalid priority policy receipt")
        seen.add(item_id)
        for state in ("before", "after"):
            definition = decision[state]
            if not isinstance(definition, dict) or set(definition) != {
                "priority",
                "priority_origin",
            }:
                raise ValueError("invalid priority policy receipt")
            _priority(definition["priority"], "priority decision")
            _priority_origin(definition["priority_origin"], "priority decision")
        _text(decision["reason"], "priority decision reason", 512)
        consulted = decision["consulted_facts"]
        if (
            not isinstance(consulted, list)
            or len(consulted) > 16
            or consulted != sorted(set(consulted))
            or any(
                type(name) is not str or not re.fullmatch(r"[a-z][a-z0-9_]{0,63}", name)
                for name in consulted
            )
        ):
            raise ValueError("invalid priority policy receipt")
    if "item_ids" in value:
        scope = value["item_ids"]
        if (
            not isinstance(scope, list)
            or any(type(item) is not str for item in scope)
            or scope != sorted(seen)
        ):
            raise ValueError("priority policy receipt disagrees with its historical item scope")
    fingerprint = value["decision_fingerprint"]
    if fingerprint != _receipt_fingerprint(decisions):
        raise ValueError("invalid priority policy receipt")


def _read(root: Path, project: dict) -> dict | None:
    path = root / "coverage.json"
    if not os.path.lexists(path):
        return None
    if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_BYTES:
        raise ValueError("coverage.json is unsafe or exceeds its byte limit")
    try:
        document = json.loads(path.read_text())
    except (ValueError, OSError) as exc:
        raise ValueError("coverage.json is not valid JSON") from exc
    return _validate_document(document, project)


def _validate_document(document: dict, project: dict) -> dict:
    """Validate the complete document both when reopening and before publication."""
    if not isinstance(document, dict) or type(document.get("format")) is not str:
        raise ValueError("unsupported coverage document shape")
    expected_keys = (
        {"format", "version", "project_uuid", "revision", "items"}
        if document["format"] == FORMAT
        else {"format", "version", "project_uuid", "revision", "items", "priority_policy"}
        if document["format"] == FORMAT_V2
        else {
            "format",
            "version",
            "project_uuid",
            "revision",
            "items",
            "priority_policy",
        }
        if document["format"] == FORMAT_V3
        else {
            "format",
            "version",
            "project_uuid",
            "revision",
            "items",
            "priority_policy",
            "plans",
        }
        if document["format"] == FORMAT_V4
        else None
    )
    if expected_keys is None or type(document["version"]) is not int:
        raise ValueError("unsupported coverage document shape")
    if document["format"] in {FORMAT_V3, FORMAT_V4}:
        extra = set(document) - expected_keys
        if extra not in ({"plan"}, {"plans"}, set()):
            raise ValueError("unsupported coverage document shape")
        if "plan" in document:
            # Normalize the single-plan v3 shape into the append-only history.
            document["plans"] = [document.pop("plan")]
    elif set(document) != expected_keys:
        raise ValueError("unsupported coverage document shape")
    if (
        document["version"]
        != {
            FORMAT: 1,
            FORMAT_V2: 2,
            FORMAT_V3: 3,
            FORMAT_V4: 4,
        }[document["format"]]
    ):
        raise ValueError("unsupported coverage version")
    if document["project_uuid"] != project["project_uuid"]:
        raise ValueError("coverage project UUID mismatch")
    if (
        type(document["revision"]) is not int
        or document["revision"] < 1
        or not isinstance(document["items"], dict)
    ):
        raise ValueError("invalid coverage revision/items")
    if len(document["items"]) > 10000:
        raise ValueError("too many checklist items")
    for item_id, item in document["items"].items():
        _identifier(item_id)
        allowed_item_keys = {"definition", "definitions", "records"} | (
            {"comments"} if document["format"] == FORMAT_V4 else set()
        )
        if not isinstance(item, dict) or set(item) != allowed_item_keys:
            raise ValueError("invalid checklist history")
        if not isinstance(item["definition"], dict) or item["definition"].get("id") != item_id:
            raise ValueError("item identity mismatch")
        if not isinstance(item["definitions"], list) or not isinstance(item["records"], list):
            raise ValueError("invalid checklist history lists")
        if (
            not item["definitions"]
            or not isinstance(item["definitions"][-1], dict)
            or item["definitions"][-1].get("definition") != item["definition"]
        ):
            raise ValueError("current definition is absent from history")
        _definition(item["definition"], {}, historical=True)
        if document["format"] == FORMAT_V4:
            task_fields = {
                "assignee",
                "description",
                "acceptance_criterion",
                "finding_ref",
                "method_ref",
                "competitors",
            }
            if not task_fields <= set(item["definition"]):
                raise ValueError("v4 task metadata is incomplete")
            if not isinstance(item.get("comments"), list) or len(item["comments"]) > 10000:
                raise ValueError("invalid task comment history")
            for comment in item["comments"]:
                _task_comment_shape(comment)
        if document["format"] == FORMAT and item["definition"]["priority_origin"] == "policy":
            raise ValueError("v1 coverage cannot contain policy priority origins")
        hashes = {}
        for version in item["definitions"]:
            if not isinstance(version, dict) or set(version) != {"observed_at", "definition"}:
                raise ValueError("invalid definition history entry")
            _text(version["observed_at"], "definition observation time", 128)
            _definition(version["definition"], {}, historical=True)
            if version["definition"]["id"] != item_id:
                raise ValueError("historical item identity mismatch")
            if (
                document["format"] == FORMAT
                and version["definition"]["priority_origin"] == "policy"
            ):
                raise ValueError("v1 coverage cannot contain policy priority origins")
            hashes[_completion_hash(version["definition"])] = version["definition"]
        for record in item["records"]:
            _record_shape(record, hashes)
    if document["format"] in {FORMAT_V2, FORMAT_V3}:
        policy = document["priority_policy"]
        if (
            not isinstance(policy, dict)
            or set(policy) != {"applications"}
            or not isinstance(policy["applications"], list)
        ):
            raise ValueError("invalid priority policy history")
        for application in policy["applications"]:
            if not isinstance(application, dict) or set(application) != {"applied_at", "receipt"}:
                raise ValueError("invalid priority policy application")
            _application_time(application["applied_at"])
            _priority_receipt(application["receipt"], set(document["items"]))
    if document["format"] in {FORMAT_V3, FORMAT_V4}:
        plans = document["plans"]
        if (
            not isinstance(plans, list)
            or (document["format"] == FORMAT_V3 and not plans)
            or len(plans) > 10000
        ):
            raise ValueError("invalid audit scope plan history")
        revision = 0
        for entry in plans:
            _stored_plan(entry, project["site"]["target"])
            if entry["revision"] <= revision:
                raise ValueError("audit scope plan revisions must increase")
            revision = entry["revision"]
    _dependencies(document["items"])
    return document


def _task_comment_shape(value: Any) -> None:
    if not isinstance(value, dict) or set(value) not in (
        {"actor", "text", "created_at"},
        {"actor", "text", "created_at", "reference"},
    ):
        raise ValueError("invalid task comment")
    _text(value["actor"], "comment actor", 128)
    _text(value["text"], "comment text", 8000)
    _text(value["created_at"], "comment timestamp", 128)
    reference = value.get("reference")
    if reference is not None:
        if not isinstance(reference, dict) or reference.get("kind") not in {"scan", "finding"}:
            raise ValueError("comment reference must identify a scan or finding")
        expected = {"kind", "id"} if reference["kind"] == "scan" else {"kind", "scan_id", "id"}
        if set(reference) != expected:
            raise ValueError("comment reference has an invalid shape")
        _text(reference["id"], "comment reference id", 256)
        if reference["kind"] == "finding":
            _text(reference["scan_id"], "comment reference scan_id", 128)


def _affected_url_count(
    root: Path,
    definition: dict,
    measurement: dict | None,
    cache: dict[str, tuple[Path | None, str | None]],
    count_cache: dict[tuple[str, str], dict[str, Any]],
) -> dict[str, Any]:
    """Use retained affected-URL evidence or W07's saved issue selection, never the crawl total."""
    urls = (measurement or {}).get("affected_urls")
    if isinstance(urls, list) and all(isinstance(url, str) and url for url in urls):
        return {
            "count": len(set(urls)),
            "state": "measured",
            "scope_state": "measured_population",
            "source": "retained_task_measurement",
            "reason": None,
        }
    reference = definition.get("finding_ref")
    if isinstance(reference, dict):
        scan_id, finding_id = reference.get("scan_id"), reference.get("finding_id")
        if isinstance(scan_id, str) and isinstance(finding_id, str):
            scan_path, unavailable = cache.get(scan_id, (None, None))
            if scan_id not in cache:
                from seohead.storage.history import list_scans

                offset = 0
                while True:
                    page = list_scans(root / "scans", offset=offset, limit=100)
                    if page.get("errors"):
                        unavailable = "saved scan history contains unreadable entries"
                        break
                    found = next((row for row in page["items"] if row.get("uuid") == scan_id), None)
                    if found is not None:
                        scan_path = Path(found["path"])
                        break
                    offset += len(page["items"])
                    if not page["has_more"]:
                        unavailable = "referenced scan is not retained in this project"
                        break
                cache[scan_id] = (scan_path, unavailable)
            if scan_path is not None:
                check_id = finding_id.removeprefix("check:")
                if re.fullmatch(r"[a-z0-9][a-z0-9_.-]{0,99}", check_id, re.I):
                    count_key = (scan_id, check_id)
                    if count_key in count_cache:
                        return dict(count_cache[count_key])
                    try:
                        from seohead.storage.url_query import scan_url_query

                        selection = scan_url_query(
                            str(scan_path),
                            issue_check=check_id,
                            columns=["url"],
                            offset=0,
                            limit=1,
                            count_timeout_seconds=5.0,
                            max_bytes=4096,
                        )
                    except (OSError, ValueError) as exc:
                        unavailable = f"saved finding selection is unavailable: {exc}"
                    else:
                        count = selection.get("filtered_total")
                        if (
                            selection.get("ok") is True
                            and selection.get("filtered_total_state") == "exact"
                            and type(count) is int
                            and (selection.get("source") or {}).get("scan_uuid") == scan_id
                        ):
                            source = selection.get("source") or {}
                            scan_state = (
                                "partial"
                                if source.get("crawl_partial") or source.get("corpus_partial")
                                else "complete"
                            )
                            counted = {
                                "count": count,
                                "state": "measured",
                                "scope_state": scan_state,
                                "source": "saved_issue_selection",
                                "scan_uuid": scan_id,
                                "check_id": check_id,
                                "reason": None,
                            }
                            count_cache[count_key] = counted
                            return dict(counted)
                        unavailable = (
                            selection.get("reason")
                            or "saved issue count was capped, unavailable, or from a different scan"
                        )
                else:
                    unavailable = "finding_ref.finding_id is not a supported saved check identifier"
            return {
                "count": None,
                "state": "not_measured",
                "scope_state": "unknown",
                "source": "finding_ref",
                "reason": unavailable or "saved affected-URL evidence is unavailable",
            }
    return {
        "count": None,
        "state": "not_measured",
        "scope_state": "unknown",
        "source": "none",
        "reason": "the retained task measurement does not enumerate affected URLs",
    }


def _record_shape(record: Any, definition_hashes: dict) -> None:
    allowed = {
        "status",
        "reason",
        "measurement",
        "reviewer",
        "signoff",
        "artifact",
        "sha256",
        "review",
        "scope",
        "site",
        "operation",
        "operation_hash",
        "source",
        "observed_at",
        "definition_hash",
        "recorded_at",
        "evidence",
        "revision",
        "artifact_verification",
        "actor",
    }
    if (
        not isinstance(record, dict)
        or set(record) - allowed
        or not {"status", "reason", "measurement", "definition_hash", "recorded_at"} <= set(record)
    ):
        raise ValueError("invalid execution history entry")
    if not isinstance(record["status"], str) or record["status"] not in {
        "running",
        "failed",
        "unavailable",
        "succeeded",
        "not_applicable",
    }:
        raise ValueError("invalid execution history status")
    _text(record["reason"], "record reason")
    _text(record["recorded_at"], "record timestamp", 128)
    if "actor" in record:
        _text(record["actor"], "record actor", 128)
    if "evidence" in record:
        _text(record["evidence"], "exclusion evidence", 512)
    if "revision" in record and (type(record["revision"]) is not int or record["revision"] < 1):
        raise ValueError("invalid record revision")
    if (
        not isinstance(record["definition_hash"], str)
        or record["definition_hash"] not in definition_hashes
    ):
        raise ValueError("execution refers to an unknown definition")
    if record["measurement"] is not None and (
        not isinstance(record["measurement"], dict)
        or not isinstance(record["measurement"].get("state"), str)
        or record["measurement"].get("state") not in {"limited", "measured", "not_measured"}
    ):
        raise ValueError("invalid measurement record")
    if "artifact" in record:
        _text(record["artifact"], "artifact", 1024)
        if not isinstance(record.get("sha256"), str) or not re.fullmatch(
            "[a-f0-9]{64}", record["sha256"]
        ):
            raise ValueError("invalid evidence digest")
    if "artifact_verification" in record:
        receipt = record["artifact_verification"]
        if (
            "artifact" not in record
            or not isinstance(receipt, dict)
            or set(receipt) != {"sha256", "identity", "verified_at"}
            or not isinstance(receipt["sha256"], str)
            or not re.fullmatch("[a-f0-9]{64}", receipt["sha256"])
            or not isinstance(receipt["identity"], list)
            or len(receipt["identity"]) != 5
            or any(type(value) is not int for value in receipt["identity"])
        ):
            raise ValueError("invalid artifact verification receipt")
        _text(receipt["verified_at"], "artifact verification timestamp", 128)
        if datetime.fromisoformat(receipt["verified_at"].replace("Z", "+00:00")).tzinfo is None:
            raise ValueError("artifact verification timestamp must include timezone")
    if "operation" in record:
        _identifier(record["operation"])
        if not isinstance(record.get("operation_hash"), str) or not re.fullmatch(
            "[a-f0-9]{64}", record["operation_hash"]
        ):
            raise ValueError("invalid operation digest")
    if record["status"] in {"not_applicable", "succeeded"} and "operation" not in record:
        _text(record.get("reviewer"), "reviewer", 128)
    if record["status"] == "succeeded" and not (
        "artifact" in record or record.get("signoff") is True
    ):
        raise ValueError("successful history requires evidence or signoff")
    if record["status"] == "succeeded":
        definition = definition_hashes[record["definition_hash"]]
        if definition["execution_kind"] == "automatic":
            if (
                record.get("operation") != definition["operation"]
                or "artifact" not in record
                or record["measurement"] is None
            ):
                raise ValueError("automatic history is missing bound measurement evidence")
        elif (
            not (definition["execution_kind"] == "manual" and record.get("signoff") is True)
            and record.get("review") != "approved"
        ):
            raise ValueError("review history is missing approval")


def _busy_reason(root: Path, lock: Path) -> str:
    """Name the live holder of the writer lock; only an unexplained lock is a leftover."""
    from .run_observation import live_holder

    try:
        holder = live_holder(root)
    except ValueError:  # an unreadable collector log must not hide the lock holder check
        holder = None
    if holder is not None:
        return (
            f"coverage writer is busy: {holder} is running on this project; wait for it to finish"
        )
    try:
        pid = int(lock.read_text().strip())
        os.kill(pid, 0)
    except (OSError, ValueError):
        return (
            "coverage writer is busy; no live holder is recorded, so .coverage.lock is a "
            "leftover from an interrupted process"
        )
    return f"coverage writer is busy: process pid {pid} holds .coverage.lock"


@contextmanager
def _transaction(directory: str | Path, expected_revision: int | None):
    root, project = _load(directory)
    lock = root / ".coverage.lock"
    try:
        fd = os.open(lock, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError as exc:
        raise ValueError(_busy_reason(root, lock)) from exc
    try:
        os.write(fd, f"{os.getpid()}\n".encode())
        os.close(fd)
        document = _read(root, project)
        revision = document["revision"] if document else 0
        if expected_revision is not None and (
            type(expected_revision) is not int or expected_revision != revision
        ):
            raise ValueError(f"coverage revision conflict: current revision is {revision}")
        if document is None:
            document = {
                "format": FORMAT,
                "version": 1,
                "project_uuid": project["project_uuid"],
                "revision": 0,
                "items": {},
            }
        original = copy.deepcopy(document)
        yield root, project, document
        if document == original:
            return
        document["revision"] += 1
        _validate_document(document, project)
        content = json.dumps(document, indent=2, sort_keys=True, allow_nan=False) + "\n"
        if len(content.encode()) > MAX_BYTES:
            raise ValueError("coverage exceeds its byte limit")
        atomic_write_bytes(root / "coverage.json", content.encode("utf-8"))
    finally:
        lock.unlink(missing_ok=True)


def _set(items: dict, definition: dict) -> None:
    item_id = definition["id"]
    if item_id not in items:
        items[item_id] = {"definition": definition, "definitions": [], "records": []}
        if "assignee" in definition:
            items[item_id]["comments"] = []
    item = items[item_id]
    if not item["definitions"] or item["definition"] != definition:
        item["definitions"].append({"observed_at": _now(), "definition": copy.deepcopy(definition)})
        item["definition"] = definition


def _custom(value: Any, site: str, catalogue: dict, previous: dict | None = None) -> dict:
    if not isinstance(value, dict):
        raise ValueError("item must be an object")
    item_id = _identifier(value.get("id"))
    if not item_id.startswith("custom:") and previous is None:
        raise ValueError("new operator items require the custom namespace")
    if "priority_origin" in value:
        raise ValueError("priority origin is assigned by the checklist, not input")
    base = previous or {
        "id": item_id,
        "title": item_id,
        "kind": "custom",
        "scope": {"site": site, "template": None, "urls": []},
        "dependencies": [],
        "execution_kind": "manual",
        "priority": "P1",
        "priority_origin": "default",
        "enabled": True,
        "order": 0,
        "operation": None,
        "source_hash": None,
        "assignee": None,
        "description": None,
        "acceptance_criterion": None,
        "finding_ref": None,
        "method_ref": None,
        "competitors": [],
    }
    if set(value) - set(base):
        raise ValueError("unknown item definition fields")
    if previous and any(
        value.get(k, previous[k]) != previous[k] for k in ("id", "kind", "source_hash")
    ):
        raise ValueError("item identity/source is immutable")
    removed = previous is not None and previous["kind"] != "custom" and item_id not in catalogue
    if removed and set(value) - {"id", "enabled", "order", "priority", "title"}:
        raise ValueError("removed built-ins can only be disabled or relabeled")
    definition = {**base, **value}
    if "priority" in value:
        definition["priority_origin"] = "operator"
    return _definition(definition, catalogue, historical=removed)


def _builtin(item_id: str, entry: dict, order: int, site: str) -> dict:
    return {
        "id": item_id,
        "title": entry["title"],
        "kind": entry["kind"],
        "scope": {"site": site, "template": None, "urls": []},
        "dependencies": [],
        "execution_kind": "automatic" if entry["kind"] == "check" else "manual",
        "priority": "P1",
        "priority_origin": "default",
        "enabled": True,
        "order": order,
        "operation": item_id if entry["kind"] == "check" else None,
        "source_hash": entry["definition_hash"],
    }


def _materialize_builtin(document: dict, item_id: str, project: dict) -> None:
    """A goal-only checklist gains the catalogue rows only when one of them is actually touched."""
    catalogue = load_catalogue()
    if item_id not in document["items"] and item_id in catalogue:
        if document.get("format") == FORMAT_V4:
            definition = _builtin(
                item_id,
                catalogue[item_id],
                len(document["items"]),
                project["site"]["target"],
            )
            definition.update(
                {
                    "assignee": None,
                    "description": None,
                    "acceptance_criterion": None,
                    "finding_ref": None,
                    "method_ref": None,
                    "competitors": [],
                }
            )
            _set(document["items"], definition)
        else:
            _reconcile_builtins(document["items"], catalogue, project["site"]["target"])


def _reconcile_builtins(items: dict, catalogue: dict, site: str) -> None:
    """Add or refresh every built-in row; only checklist setup and on-demand touches call this."""
    for order, (item_id, entry) in enumerate(catalogue.items()):
        if item_id in items:
            definition = {
                **items[item_id]["definition"],
                "source_hash": entry["definition_hash"],
                "title": entry["title"],
            }
        else:
            definition = _builtin(item_id, entry, order, site)
        _set(items, _definition(definition, catalogue))


def initialize_coverage(
    directory: str | Path,
    template: dict | None = None,
    expected_revision: int | None = None,
    plan: dict | None = None,
) -> dict:
    """Initialize or reconcile built-ins, apply a template, and record the agreed audit scope."""
    catalogue = load_catalogue()
    if template is not None and (
        not isinstance(template, dict)
        or set(template) != {"format", "items"}
        or template["format"] != "seohead.checklist-template.v1"
        or not isinstance(template["items"], list)
    ):
        raise ValueError("unsupported checklist template")
    with _transaction(directory, expected_revision) as (_, project, document):
        items = document["items"]
        _reconcile_builtins(items, catalogue, project["site"]["target"])
        if document.get("format") == FORMAT_V4 or template is not None:
            _promote_v4(document)
        seen = set()
        for value in (template or {}).get("items", []):
            if not isinstance(value, dict):
                raise ValueError("invalid template item")
            _identifier(value.get("id"))
            if value.get("id") in seen:
                raise ValueError("invalid or duplicate template item")
            seen.add(value.get("id"))
            previous = items.get(value.get("id"), {}).get("definition")
            _set(items, _custom(value, project["site"]["target"], catalogue, previous))
        if plan is not None:
            stored = _plan(plan, project["site"]["target"])
            if stored["tasks"]["kind"] == "selection" and any(
                item_id not in items for item_id in stored["tasks"]["ids"]
            ):
                raise ValueError("agreed task ids must exist in the reconciled checklist")
            if document.get("format") != FORMAT_V4:
                document["format"] = FORMAT_V3
                document["version"] = 3
            document.setdefault("priority_policy", {"applications": []})
            agreement = {**stored, "site": project["site"]["target"]}
            current = (document.get("plans") or [None])[-1]
            if current is None or any(
                current.get(key) != value for key, value in agreement.items()
            ):
                document.setdefault("plans", []).append(
                    {
                        **stored,
                        "recorded_at": _now(),
                        "revision": document["revision"] + 1,
                        "site": project["site"]["target"],
                    }
                )
    return coverage_status(directory)


def _promote_v4(document: dict) -> None:
    """Add task metadata/comments while retaining every earlier checklist record."""
    defaults = {
        "assignee": None,
        "description": None,
        "acceptance_criterion": None,
        "finding_ref": None,
        "method_ref": None,
        "competitors": [],
    }
    for item in document["items"].values():
        item["definition"] = {**defaults, **item["definition"]}
        for version in item["definitions"]:
            version["definition"] = {**defaults, **version["definition"]}
        item.setdefault("comments", [])
    if document.get("format") != FORMAT_V4:
        document["format"] = FORMAT_V4
        document["version"] = 4
        document.setdefault("priority_policy", {"applications": []})
        document.setdefault("plans", [])


def ensure_goal_task(
    directory: str | Path, *, item_id: str, title: str, description: str | None = None
) -> None:
    """Create one manual custom task for an accepted goal; an existing task is kept as-is."""
    catalogue = load_catalogue()
    with _transaction(directory, None) as (_, project, document):
        _promote_v4(document)
        if item_id not in document["items"]:
            _set(
                document["items"],
                _custom(
                    {
                        "id": item_id,
                        "title": title,
                        "description": description,
                    },
                    project["site"]["target"],
                    catalogue,
                ),
            )


def update_item(directory: str | Path, item: dict, expected_revision: int) -> dict:
    """Add or edit one item, preserving definitions and result history."""
    if type(expected_revision) is not int:
        raise ValueError("expected_revision must be an integer")
    if not isinstance(item, dict):
        raise ValueError("item must be an object")
    _identifier(item.get("id"))
    catalogue = load_catalogue()
    with _transaction(directory, expected_revision) as (_, project, document):
        if document["revision"] == 0:
            raise ValueError("initialize the checklist first: run project-checklist-init")
        _promote_v4(document)
        _materialize_builtin(document, item.get("id"), project)
        previous = (
            document["items"].get(item.get("id"), {}).get("definition")
            if isinstance(item, dict)
            else None
        )
        _set(document["items"], _custom(item, project["site"]["target"], catalogue, previous))
    return coverage_status(directory)


def append_task_comment(
    directory: str | Path,
    *,
    item_id: str,
    text: str,
    actor: str,
    expected_revision: int,
    reference: dict[str, str] | None = None,
) -> dict:
    """Append an attributed owner/agent note with an optional typed evidence identity."""
    _identifier(item_id)
    _text(actor, "comment actor", 128)
    _text(text, "comment text", 8000)
    if type(expected_revision) is not int:
        raise ValueError("expected_revision must be an integer")
    entry = {"actor": actor, "text": text, "created_at": _now()}
    if reference is not None:
        entry["reference"] = copy.deepcopy(reference)
    _task_comment_shape(entry)
    with _transaction(directory, expected_revision) as (_, _project, document):
        if document["revision"] == 0:
            raise ValueError("initialize the checklist first: run project-checklist-init")
        _promote_v4(document)
        item = document["items"].get(item_id)
        if item is None or item["definition"]["kind"] != "custom":
            raise ValueError("comments apply to current custom tasks")
        item.setdefault("comments", []).append(entry)
        if len(item["comments"]) > 10000:
            raise ValueError("task comment history exceeds its bound")
    return coverage_status(directory)


def record_execution(
    directory: str | Path, item_id: str, record: dict, expected_revision: int
) -> dict:
    """Record an attempt or explicit applicability review; never execute an operation."""
    from .evidence import validate_record

    _identifier(item_id)
    if type(expected_revision) is not int:
        raise ValueError("expected_revision must be an integer")
    if not isinstance(record, dict):
        raise ValueError("record must be an object")
    with _transaction(directory, expected_revision) as (root, project, document):
        if "actor" in record:
            _promote_v4(document)
        _materialize_builtin(document, item_id, project)
        if item_id not in document["items"]:
            raise ValueError("unknown checklist item")
        item = document["items"][item_id]
        view = _status(root, document, load_catalogue())
        current = next(row for row in view["items"] if row["id"] == item_id)
        if record.get("status") == "succeeded" and current["blocked_by"]:
            raise ValueError("dependencies are not complete")
        if not item["definition"]["enabled"] and record.get("status") != "not_applicable":
            raise ValueError("item is disabled")
        if record.get("status") == "succeeded":
            _definition(item["definition"], load_catalogue())
            if (
                item["definition"]["kind"] != "custom"
                and item["definition"]["source_hash"]
                != load_catalogue()[item_id]["definition_hash"]
            ):
                raise ValueError("source definition changed; reconcile checklist first")
        validated = validate_record(root, item["definition"], record)
        item["records"].append(
            {
                **validated,
                "definition_hash": _completion_hash(item["definition"]),
                "recorded_at": _now(),
                "revision": document["revision"] + 1,
            }
        )
    from .journal import note

    note(
        directory,
        source="agent",
        actor="agent",
        text=f"checklist {item_id}: {record.get('status')}",
    )
    return coverage_status(directory)


def _disposition(item: dict, record: dict, stale_reason: str) -> tuple[str, dict | None]:
    """Resolve an item as agreed-applicable, reviewed-excluded, or pending a decision."""
    if record and record["status"] == "not_applicable":
        if stale_reason:
            return "pending_exclusion", {
                "reason": "exclusion record is stale; review and record the decision again"
            }
        basis = {}
        if "artifact" in record:
            basis["artifact"] = record["artifact"]
            basis["sha256"] = record.get("sha256")
        if "evidence" in record:
            basis["evidence"] = record["evidence"]
        if basis:
            return "excluded", {
                "reason": record["reason"],
                "reviewer": record.get("reviewer"),
                "basis": basis,
                "recorded_at": record["recorded_at"],
                "revision": record.get("revision"),
            }
        return "pending_exclusion", {
            "reason": "exclusion record lacks a verifiable evidence basis; review and record again"
        }
    if not item["definition"]["enabled"]:
        return "pending_exclusion", {
            "reason": "disabled without a reviewed exclusion; it stays inside the agreed denominator"
        }
    return "applicable", None


_AGENT_REVIEWER = re.compile(r"(agent|assistant|claude|codex|llm|ai)\b", re.IGNORECASE)


def _owner_decided(record: dict) -> bool:
    """Manual work counts as owner-complete only by owner signoff or a non-agent review.

    An agent may record that it reviewed an artifact; that leaves the item waiting for the
    owner rather than completing it. The reviewer name is the only identity the record holds.
    """
    if record.get("signoff") is True:
        return True
    return record.get("review") == "approved" and not _AGENT_REVIEWER.search(
        str(record.get("reviewer") or "")
    )


def _measured_urls(row: dict) -> tuple[set[str], bool]:
    """Return the URL set a fresh automatic measurement covered, and whether it is complete."""
    measurement = row["measurement"] or {}
    urls = measurement.get("urls")
    if isinstance(urls, list):
        return set(urls), bool(measurement.get("urls_enumerated", True))
    scope_urls = (row["scope"] or {}).get("urls") or []
    if scope_urls:
        return set(scope_urls), True
    return set(), not measurement.get("population")


def _eligibility(row: dict, population: dict | None) -> set[str] | None:
    """Return the agreed eligible URL set for a row, or None when membership is unverifiable."""
    if not population or population["kind"] == "unknown":
        return None
    template = row["scope"]["template"]
    entry = population.get("templates", {}).get(template) if template else None
    if entry is not None:
        return set(entry["urls"]) if entry["urls"] else None
    if population["urls"]:
        return set(population["urls"])
    # A size-only population cannot verify that an observed URL belongs to it.
    return None


def _task_axis(rows: list[dict], basis: str) -> dict:
    excluded = sum(row["applicability"] == "excluded" for row in rows)
    pending = sum(row["applicability"] == "pending_exclusion" for row in rows)
    not_agreed = sum(row["applicability"] == "not_agreed" for row in rows)
    denominator_rows = [
        row for row in rows if row["applicability"] in {"applicable", "pending_exclusion"}
    ]
    numerator = sum(row["complete"] for row in denominator_rows)
    denominator = len(denominator_rows)
    return {
        "basis": basis,
        "numerator": numerator,
        "denominator": denominator,
        "excluded": excluded,
        "pending_exclusion": pending,
        "not_agreed": not_agreed,
        "unfinished": denominator - numerator,
        "state": "measured" if denominator else "unknown",
        "reason": "enumerated checklist task set"
        if denominator
        else "no agreed applicable tasks remain in this set; the ratio is undefined, not 100%",
    }


def _url_axis(ordered: list[dict], plan: dict | None) -> dict:
    population = (plan or {}).get("population")
    measured_urls: set[str] = set()
    counted_urls: set[str] = set()
    unverified = 0
    for row in ordered:
        if (
            row["execution_kind"] != "automatic"
            or row["applicability"] == "excluded"
            or row["state"] != "run"
        ):
            continue
        observed, fully_enumerated = _measured_urls(row)
        measured_urls |= observed
        eligible = _eligibility(row, population)
        if eligible is not None:
            counted_urls |= observed & eligible
        if eligible is None or not fully_enumerated:
            unverified += 1
    reason = ""
    if not plan:
        state = "unknown"
        numerator = denominator = None
        reason = "no agreed URL population is recorded for this checklist"
    elif population["kind"] == "unknown":
        state = "unknown"
        numerator = denominator = None
        reason = population["reason"]
    else:
        denominator = population["size"]
        numerator = len(counted_urls)
        state = "measured" if not unverified else "partial"
        if unverified:
            reason = (
                f"{unverified} measurement(s) cannot be verified against the agreed "
                "population; numerator counts only verified eligible URLs"
            )
        elif population["kind"] == "sample":
            reason = "measured against the named sample, not the full site population"
        else:
            reason = "measured against the agreed site population"
    return {
        "basis": "eligible URLs covered by fresh check measurements over the agreed eligible "
        "URL population",
        "numerator": numerator,
        "denominator": denominator,
        "measured_urls": len(measured_urls),
        "unverified_measurements": unverified,
        "population_kind": population["kind"] if population else None,
        "population_name": population.get("name") if population else None,
        "state": state,
        "reason": reason,
    }


def _status(
    root: Path,
    document: dict,
    catalogue: dict,
    project: dict | None = None,
    *,
    _verify_evidence: bool = True,
) -> dict:
    from .evidence import evidence_stale

    document = copy.deepcopy(document)
    _, project = _load(root)
    for order, (item_id, entry) in enumerate(catalogue.items()):
        if item_id not in document["items"]:
            _set(document["items"], _builtin(item_id, entry, order, project["site"]["target"]))
    plans = document.get("plans") or []
    plan = plans[-1] if plans else None
    plan_revision = plan["revision"] if plan else 0
    agreement = (plan or {}).get("tasks") or {"kind": "all_agreed"}
    agreed = set(agreement["ids"]) if agreement["kind"] == "selection" else None
    rows = {}
    affected_count_cache: dict[str, tuple[Path | None, str | None]] = {}
    affected_result_cache: dict[tuple[str, str], dict[str, Any]] = {}
    digests = {}
    exclusions = []
    for item_id, item in document["items"].items():
        definition = item["definition"]
        record = item["records"][-1] if item["records"] else {}
        source_stale = definition["kind"] != "custom" and (
            item_id not in catalogue
            or definition["source_hash"] != catalogue[item_id]["definition_hash"]
        )
        stale_reason = ""
        if record and record["definition_hash"] != _completion_hash(definition):
            stale_reason = "item definition changed"
        elif record:
            stale_reason = evidence_stale(
                root, record, catalogue, digests, verify_bytes=_verify_evidence
            )
        if (
            record
            and not stale_reason
            and plan_revision
            and (record.get("revision") or 0) < plan_revision
        ):
            stale_reason = "execution predates the current agreed audit scope"
        if source_stale and (not record or record["status"] != "not_applicable"):
            # A success cannot outlive the source meaning it measured, but a
            # reviewed exclusion stays resolvable across catalogue churn.
            stale_reason = stale_reason or "source definition changed or was removed"
        state = "not_run"
        if record and not stale_reason:
            if record["status"] == "not_applicable":
                state = "not_applicable"
            elif record["status"] == "succeeded":
                state = "run"
        if agreed is not None and item_id not in agreed:
            applicability, applicability_detail = "not_agreed", None
        else:
            applicability, applicability_detail = _disposition(item, record, stale_reason)
        if applicability == "excluded":
            exclusions.append({"id": item_id, **applicability_detail})
        measurement = record.get("measurement") or {}
        partial_measurement = (
            definition["execution_kind"] == "automatic"
            and not definition["scope"]["urls"]
            and measurement.get("state") == "limited"
        )
        rows[item_id] = {
            "id": item_id,
            "title": definition["title"],
            "kind": definition["kind"],
            "scope": definition["scope"],
            "priority": definition["priority"],
            "priority_origin": definition["priority_origin"],
            "priority_reason": "not applied",
            "order": definition["order"],
            "enabled": definition["enabled"],
            "execution_kind": definition["execution_kind"],
            "state": state,
            "applicability": applicability,
            "applicability_reason": applicability_detail["reason"]
            if applicability == "pending_exclusion"
            else "outside the agreed task selection"
            if applicability == "not_agreed"
            else None,
            "exclusion": applicability_detail if applicability == "excluded" else None,
            "stale": bool(stale_reason) or source_stale,
            "reason": stale_reason or record.get("reason") or "not attempted",
            "attempt_status": record.get("status", "not_run"),
            "actor": record.get("actor") or record.get("reviewer"),
            "measurement": record.get("measurement"),
            "affected_urls": _affected_url_count(
                root, definition, measurement, affected_count_cache, affected_result_cache
            ),
            "task": {
                "assignee": definition.get("assignee"),
                "description": definition.get("description"),
                "acceptance_criterion": definition.get("acceptance_criterion"),
                "finding_ref": definition.get("finding_ref"),
                "method_ref": definition.get("method_ref"),
                "competitors": definition.get("competitors", []),
            },
            "evidence_verification": {
                "mode": "bytes" if _verify_evidence else "metadata_only",
                "state": "unverified"
                if "unverified:" in stale_reason
                else "stale"
                if stale_reason
                else "verified"
                if "artifact" in record and _verify_evidence
                else "metadata_matches"
                if "artifact" in record
                else "not_applicable",
                "verified_at": (record.get("artifact_verification") or {}).get("verified_at"),
            },
            "blocked_by": [],
            "complete": state == "run"
            and applicability == "applicable"
            and not partial_measurement
            and (definition["execution_kind"] == "automatic" or _owner_decided(record)),
            "definition_versions": len(item["definitions"]),
            "attempts": len(item["records"]),
        }
    visited = set()

    def resolve(item_id):
        if item_id in visited:
            return
        row = rows[item_id]
        for dep in document["items"][item_id]["definition"]["dependencies"]:
            resolve(dep)
            if row["applicability"] in {"excluded", "not_agreed"}:
                continue
            if (
                rows[dep]["applicability"] in {"applicable", "pending_exclusion", "not_agreed"}
                and not rows[dep]["complete"]
            ):
                row["blocked_by"].append(dep)
        if row["blocked_by"]:
            row["complete"] = False
        visited.add(item_id)

    for item_id in rows:
        resolve(item_id)
    ordered = sorted(rows.values(), key=lambda row: (row["order"], row["id"]))
    denominator_rows = [
        row for row in ordered if row["applicability"] in {"applicable", "pending_exclusion"}
    ]
    counts = {
        name: sum(row["state"] == name for row in denominator_rows)
        for name in ("run", "not_applicable", "not_run")
    }
    counts.update(
        total=len(denominator_rows),
        disabled=sum(not row["enabled"] for row in ordered),
        excluded=len(exclusions),
        pending_exclusion=sum(row["applicability"] == "pending_exclusion" for row in ordered),
        not_agreed=sum(row["applicability"] == "not_agreed" for row in ordered),
        complete=sum(row["complete"] for row in denominator_rows),
        stale=sum(row["stale"] for row in denominator_rows),
    )
    counts["remaining"] = counts["total"] - counts["complete"]
    views = {
        "running": [
            row["id"]
            for row in denominator_rows
            if row["attempt_status"] == "running" and not row["stale"]
        ],
        "blocked": [
            row["id"]
            for row in denominator_rows
            if row["blocked_by"] or row["attempt_status"] in {"failed", "unavailable"}
        ],
        "waiting_for_manual_review": [
            row["id"]
            for row in denominator_rows
            if not row["complete"] and row["execution_kind"] in {"manual", "deliverable"}
        ],
        "deliverable_ready": [
            row["id"]
            for row in denominator_rows
            if row["complete"] and row["state"] == "run" and row["execution_kind"] == "deliverable"
        ],
        "pending_exclusion": [
            row["id"] for row in ordered if row["applicability"] == "pending_exclusion"
        ],
        "not_agreed": [row["id"] for row in ordered if row["applicability"] == "not_agreed"],
        "remaining": [row["id"] for row in denominator_rows if not row["complete"]],
    }
    by_kind = {
        kind: {
            "total": sum(row["kind"] == kind for row in denominator_rows),
            "complete": sum(row["kind"] == kind and row["complete"] for row in denominator_rows),
        }
        for kind in ("check", "skill", "scenario", "custom")
    }
    applications = document.get("priority_policy", {}).get("applications", [])
    latest_policy = applications[-1] if applications else None
    if latest_policy is not None:
        for decision in latest_policy["receipt"]["decisions"]:
            if decision["id"] in rows:
                rows[decision["id"]]["priority_reason"] = (
                    "operator priority preserved"
                    if rows[decision["id"]]["priority_origin"] == "operator"
                    else decision["reason"]
                )
    saved_facts_state = "not_applied"
    if latest_policy is not None:
        current_facts = sorted(
            copy.deepcopy((project or _load(root)[1])["facts"]), key=lambda fact: fact["name"]
        )
        saved_facts_state = (
            "matches_current_project"
            if current_facts == latest_policy["receipt"]["facts"]
            else "changed_since_application"
        )
    coverage = {
        "audit_tasks": _task_axis(
            ordered, "applicable audit tasks completed over the agreed applicable task set"
        ),
        "checks": _task_axis(
            [row for row in ordered if row["execution_kind"] == "automatic"],
            "applicable automatic checks completed over the agreed applicable check set",
        ),
        "manual_review": _task_axis(
            [row for row in ordered if row["execution_kind"] == "manual"],
            "manual-review tasks completed over the agreed applicable manual-review set",
        ),
        "deliverable_review": _task_axis(
            [row for row in ordered if row["execution_kind"] == "deliverable"],
            "approved deliverables over the agreed applicable deliverable set",
        ),
        "url_population": _url_axis(ordered, plan),
    }
    coverage["checks"]["measured_urls"] = coverage["url_population"]["measured_urls"]
    tasks = coverage["audit_tasks"]
    url_population = coverage["url_population"]
    custom_tasks = [row for row in ordered if row["kind"] == "custom"]
    measured_affected = [
        row["affected_urls"]["count"]
        for row in custom_tasks
        if row["affected_urls"]["state"] == "measured"
    ]
    affected_unmeasured = len(custom_tasks) - len(measured_affected)
    task_kpis = {
        "scope": "primary_project",
        "total": len(custom_tasks),
        "complete": sum(row["complete"] for row in custom_tasks),
        "remaining": sum(not row["complete"] for row in custom_tasks),
        "assigned": sum(bool(row["task"]["assignee"]) for row in custom_tasks),
        "affected_urls": {
            "state": "measured"
            if custom_tasks and not affected_unmeasured
            else "partial"
            if measured_affected
            else "unavailable",
            "count": sum(measured_affected) if measured_affected else None,
            "measured_tasks": len(measured_affected),
            "unmeasured_tasks": affected_unmeasured,
            "reason": None
            if custom_tasks and not affected_unmeasured
            else "exact affected-URL evidence is unavailable for one or more custom tasks",
        },
    }
    population_finished = (
        url_population["state"] == "measured"
        and bool(url_population["denominator"])
        and url_population["numerator"] == url_population["denominator"]
    )
    return {
        "state": "initialized",
        "revision": document["revision"],
        "project_uuid": document["project_uuid"],
        "counts": counts,
        "task_kpis": task_kpis,
        "by_kind": by_kind,
        "views": views,
        "items": ordered,
        "coverage": coverage,
        "plan": plan,
        "plan_history": plans[:-1],
        "exclusions": exclusions,
        "priority_policy": {
            "state": "applied" if latest_policy else "not_applied",
            "policy_hash": latest_policy["receipt"]["policy_hash"] if latest_policy else None,
            "applied_at": latest_policy["applied_at"] if latest_policy else None,
            "facts_state": saved_facts_state,
        },
        "complete": bool(tasks["denominator"])
        and tasks["unfinished"] == 0
        and tasks["pending_exclusion"] == 0
        and population_finished,
    }


def coverage_status(directory: str | Path, *, _verify_evidence: bool = True) -> dict:
    """Read definitions and evidence without writes or network requests."""
    root, project = _load(directory)
    document = _read(root, project)
    if document is None:
        return {
            "state": "not_initialized",
            "reason": "coverage checklist is initialized by project checklist setup, not project creation",
        }
    return _status(root, document, load_catalogue(), project, _verify_evidence=_verify_evidence)
