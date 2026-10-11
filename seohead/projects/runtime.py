"""Project crawl policy, admission, bounded preparation and addressable playbooks."""

from __future__ import annotations

import contextlib
import copy
import hashlib
import json
import math
import os
import re
import sqlite3
import uuid
from pathlib import Path
from typing import Any

from seohead.core.filesystem import atomic_write_bytes

from .catalogue import load_catalogue
from .coverage import _now, coverage_status, initialize_coverage, record_execution, update_item
from .workspace import _load, _target

POLICY_FORMAT = "seohead.project-crawl-policy.v1"
PREPARATION_FORMAT = "seohead.project-preparation.v1"
DEFAULT_POLICY = {
    "evidence_hash": {"max_bytes": 1024 * 1024 * 1024, "max_seconds": 5},
    "approval_thresholds": {"pages": 1000, "requests": 3000, "seconds": 600},
    "quick_crawl": {"pages": 50, "requests": 150, "seconds": 60},
    "crawl_overrides": {},
    "competitor_limit": 5,
}


def read_document(root: Path, name: str) -> dict | None:
    path = root / name
    if not os.path.lexists(path):
        return None
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 16 * 1024 * 1024:
        raise ValueError(f"unsafe or oversized {name}")
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise ValueError(f"{name} must contain an object")
    return data


def write_document(
    root: Path, name: str, data: dict, *, expected_revision: int | None = None
) -> None:
    """Publish one bounded project document with a retained previous revision."""
    content = json.dumps(data, ensure_ascii=False, indent=2, sort_keys=True, allow_nan=False) + "\n"
    if len(content.encode()) > 16 * 1024 * 1024:
        raise ValueError("project document exceeds its byte budget")
    lock = root / ("." + name + ".lock")
    try:
        fd = os.open(lock, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError as exc:
        raise ValueError(f"another writer owns {name}") from exc
    os.close(fd)
    try:
        previous = read_document(root, name)
        revision = previous.get("revision", 0) if previous else 0
        if expected_revision is not None and (
            type(expected_revision) is not int or revision != expected_revision
        ):
            raise ValueError("project document revision conflict")
        if previous is not None:
            backups = root / ".project-backups"
            if backups.is_symlink():
                raise ValueError("unsafe project backup directory")
            backups.mkdir(mode=0o700, exist_ok=True)
            backup = backups / f"{name}.{uuid.uuid4().hex}.json"
            descriptor = os.open(backup, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(descriptor, "wb") as stream:
                stream.write((root / name).read_bytes())
                stream.flush()
                os.fsync(stream.fileno())
        atomic_write_bytes(root / name, content.encode("utf-8"))
    finally:
        lock.unlink(missing_ok=True)


def _positive(value: Any, label: str) -> int:
    if type(value) is not int or not 1 <= value <= 1_000_000_000:
        raise ValueError(f"{label} must be a positive bounded integer")
    return value


def validate_policy(policy: Any) -> dict:
    from seohead.crawl.settings import load

    if not isinstance(policy, dict) or set(policy) not in (
        set(DEFAULT_POLICY),
        set(DEFAULT_POLICY) - {"evidence_hash"},
    ):
        raise ValueError("crawl policy has unsupported fields")
    result = copy.deepcopy(policy)
    result.setdefault("evidence_hash", copy.deepcopy(DEFAULT_POLICY["evidence_hash"]))
    hashing = result["evidence_hash"]
    if not isinstance(hashing, dict) or set(hashing) != {"max_bytes", "max_seconds"}:
        raise ValueError("evidence_hash requires max_bytes and max_seconds")
    for name, ceiling in (("max_bytes", 64 * 1024 * 1024 * 1024), ("max_seconds", 300)):
        if type(hashing[name]) is not int or not 1 <= hashing[name] <= ceiling:
            raise ValueError(f"evidence_hash.{name} must be an integer from 1 to {ceiling}")
    for section in ("approval_thresholds", "quick_crawl"):
        if not isinstance(result[section], dict) or set(result[section]) != {
            "pages",
            "requests",
            "seconds",
        }:
            raise ValueError(f"{section} requires pages, requests and seconds")
        for key, value in result[section].items():
            _positive(value, f"{section}.{key}")
    if not isinstance(result["crawl_overrides"], dict):
        raise ValueError("crawl_overrides must be a configuration object")
    # Validate with the collector's actual settings; policy never executes its text.
    load(overrides=result["crawl_overrides"])
    if _positive(result["competitor_limit"], "competitor_limit") > 20:
        raise ValueError("competitor_limit cannot exceed 20")
    return result


def project_policy(
    directory: str,
    policy: dict | None = None,
    apply: bool = False,
    expected_revision: int | None = None,
) -> dict:
    root, project = _load(directory)
    current = read_document(root, "crawl-policy.json")
    if current and (
        set(current) != {"format", "project_uuid", "revision", "policy"}
        or current["format"] != POLICY_FORMAT
        or current["project_uuid"] != project["project_uuid"]
        or type(current["revision"]) is not int
    ):
        raise ValueError("unsupported or mismatched project crawl policy")
    selected = validate_policy(
        policy if policy is not None else current["policy"] if current else DEFAULT_POLICY
    )
    revision = current["revision"] if current else 0
    if type(apply) is not bool:
        raise ValueError("apply must be a boolean")
    if apply:
        if type(expected_revision) is not int:
            raise ValueError("expected_revision is required to change project policy")
        document = {
            "format": POLICY_FORMAT,
            "project_uuid": project["project_uuid"],
            "revision": revision + 1,
            "policy": selected,
        }
        write_document(root, "crawl-policy.json", document, expected_revision=expected_revision)
        revision += 1
    return {"ok": True, "applied": apply, "revision": revision, "policy": selected}


def admission(directory: str, settings: dict, *, approved: bool = False) -> dict:
    """Bound requested work before the first request; missing bounds require approval."""
    if type(approved) is not bool:
        raise ValueError("approve_large_crawl must be a boolean")
    policy = project_policy(directory)["policy"]
    limits = settings["limits"]
    requested = {
        "pages": limits.get("max_urls"),
        "requests": limits.get("max_requests"),
        "seconds": limits.get("max_crawl_seconds"),
    }
    exceeded = []
    for key, maximum in policy["approval_thresholds"].items():
        value = requested[key]
        if (
            not isinstance(value, (int, float))
            or isinstance(value, bool)
            or not math.isfinite(value)
            or value <= 0
        ):
            exceeded.append(
                {
                    "budget": key,
                    "requested": value,
                    "threshold": maximum,
                    "reason": "request is not explicitly bounded",
                }
            )
        elif value > maximum:
            exceeded.append(
                {
                    "budget": key,
                    "requested": value,
                    "threshold": maximum,
                    "reason": "requested budget exceeds project threshold",
                }
            )
    return {
        "ok": not exceeded or approved,
        "approved": approved,
        "requested": requested,
        "exceeded": exceeded,
        "reason": "explicit budget override accepted"
        if approved
        else "within project thresholds"
        if not exceeded
        else "project admission requires approve_large_crawl=true",
    }


_HEADING = re.compile(r"^(#{1,6})\s+(.+?)\s*$")
_STEP = re.compile(r"^(?:#{2,6}\s+|\*\*)(\d+)\.\s+(.+?)\.?\*{0,2}\s*$")


def playbook_outline(content: str) -> dict:
    """Headings and numbered steps of a packaged playbook, read without running anything.

    Lines inside fenced code blocks are skipped, so a shell comment is never a heading.
    """
    headings: list[dict] = []
    steps: list[str] = []
    fenced = False
    for line in content.splitlines():
        if line.lstrip().startswith("```"):
            fenced = not fenced
            continue
        if fenced:
            continue
        heading = _HEADING.match(line)
        if heading:
            headings.append({"level": len(heading.group(1)), "title": heading.group(2)})
        step = _STEP.match(line)
        if step:
            steps.append(step.group(2).replace("*", "").strip())
    return {"headings": headings, "steps": steps}


def playbook_list(kind: str | None = None) -> dict:
    if kind not in {None, "skill", "scenario"}:
        raise ValueError("kind must be skill or scenario")
    entries = []
    for row in load_catalogue().values():
        if row["kind"] not in ({kind} if kind else {"skill", "scenario"}):
            continue
        entry = {key: value for key, value in row.items() if key != "content"}
        entry["steps"] = playbook_outline(row["content"])["steps"]
        entries.append(entry)
    return {"ok": True, "items": entries}


def playbook_show(name: str, kind: str = "skill") -> dict:
    if not isinstance(name, str) or kind not in {"skill", "scenario"}:
        raise ValueError("invalid playbook request")
    matches = [
        row
        for key, row in load_catalogue().items()
        if row["kind"] == kind
        and (key == name or key.removeprefix(kind + ":") == name or key.rsplit("/", 1)[-1] == name)
    ]
    if len(matches) != 1:
        raise ValueError("playbook name is missing or ambiguous; use its full catalogue ID")
    return {"ok": True, **matches[0], **playbook_outline(matches[0]["content"])}


def preparation_status(directory: str) -> dict:
    root, project = _load(directory)
    state = read_document(root, "preparation.json")
    if state is None:
        return {"state": "pending", "reason": "bounded project preparation has not run"}
    if (
        state.get("format") != PREPARATION_FORMAT
        or state.get("project_uuid") != project["project_uuid"]
        or not isinstance(state.get("steps"), dict)
        or not isinstance(state.get("competitors"), list)
    ):
        raise ValueError("invalid project preparation state")
    if state.get("state") == "running" and not (root / ".prepare.lock").exists():
        return {
            **state,
            "state": "interrupted",
            "reason": "preparation writer stopped before recording completion",
        }
    return state


def _candidate_url(value: str) -> str:
    """Validate a competitor URL like a target, but keep its host as entered (www included)."""
    from urllib.parse import urlsplit, urlunsplit

    _target(value)
    parts = urlsplit(value.strip())
    port = f":{parts.port}" if parts.port else ""
    return urlunsplit((parts.scheme.lower(), parts.hostname + port, parts.path or "/", "", ""))


def _candidate(value: Any) -> dict:
    if isinstance(value, str):
        return {
            "url": _candidate_url(value),
            "source": "operator supplied candidate",
            "observed_at": None,
        }
    if not isinstance(value, dict) or set(value) != {"url", "source", "observed_at"}:
        raise ValueError("competitor candidates require url, source and observed_at")
    if (
        not isinstance(value["source"], str)
        or not value["source"].strip()
        or len(value["source"]) > 512
    ):
        raise ValueError("competitor source is required")
    return {**value, "url": _candidate_url(value["url"])}


def _competitor_identity(url: str) -> str:
    """The core site identity a candidate URL resolves to; the stored URL keeps its www host."""
    return _target(url)


def _competitor_workspaces(root: Path, candidates: list[dict]) -> list[dict]:
    """Create or reuse one local workspace per candidate and return its shortlist rows."""
    from .workspace import create_project

    directory_root = root / "competitors"
    if directory_root.is_symlink():
        raise ValueError("unsafe competitor workspace directory")
    directory_root.mkdir(mode=0o700, exist_ok=True)
    rows = []
    for candidate in candidates:
        slug = hashlib.sha256(candidate["url"].encode()).hexdigest()[:16]
        child = directory_root / slug
        if not child.exists():
            create_project(
                child,
                candidate["url"],
                facts=[
                    {
                        "name": "candidate_source",
                        "value": candidate["source"],
                        "provenance": "project preparation",
                        "observed_at": candidate["observed_at"],
                    }
                ],
            )
        _, child_project = _load(child)
        if child_project["site"]["target"] != _competitor_identity(candidate["url"]):
            raise ValueError("competitor workspace site identity mismatch")
        initialize_coverage(child, template=None)
        rows.append(
            {
                **candidate,
                "directory": child.relative_to(root).as_posix(),
                "project_uuid": child_project["project_uuid"],
                "state": "candidate; audit not run",
            }
        )
    return rows


def add_competitors(directory: str, competitors: list) -> dict:
    """Record operator-supplied competitor candidates without running a preparation crawl."""
    root, project = _load(directory)
    policy = project_policy(directory)["policy"]
    if not isinstance(competitors, list) or not competitors:
        raise ValueError("competitors must be a non-empty list of candidates")
    candidates = [_candidate(value) for value in competitors]
    identities = [_competitor_identity(row["url"]) for row in candidates]
    if len(set(identities)) != len(identities):
        raise ValueError("duplicate competitor candidates")
    if project["site"]["target"] in identities:
        raise ValueError("the primary site cannot be its own competitor candidate")
    lock = root / ".prepare.lock"
    try:
        fd = os.open(lock, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError as exc:
        raise ValueError("project preparation is already running") from exc
    os.close(fd)
    try:
        current = read_document(root, "preparation.json")
        if current is not None and (
            current.get("format") != PREPARATION_FORMAT
            or current.get("project_uuid") != project["project_uuid"]
            or not isinstance(current.get("steps"), dict)
            or not isinstance(current.get("competitors"), list)
        ):
            raise ValueError("invalid project preparation state")
        existing = current["competitors"] if current else []
        known = {_competitor_identity(row["url"]) for row in existing}
        duplicates = sorted(
            row["url"]
            for row, identity in zip(candidates, identities, strict=True)
            if identity in known
        )
        if duplicates:
            raise ValueError("competitor already recorded: " + duplicates[0])
        if len(existing) + len(candidates) > policy["competitor_limit"]:
            raise ValueError("competitor candidate list exceeds the project limit")
        revision = current.get("revision", 0) if current else 0
        state = current or {
            "format": PREPARATION_FORMAT,
            "project_uuid": project["project_uuid"],
            "revision": 0,
            "started_at": _now(),
            "state": "partial",
            "steps": {},
            "competitors": [],
        }
        state["competitors"] = [*existing, *_competitor_workspaces(root, candidates)]
        state["steps"]["competitors"] = {
            "state": "run",
            "reason": "operator-supplied candidate shortlist; competitiveness is not verified",
            "count": len(state["competitors"]),
        }
        state["revision"] = revision + 1
        write_document(root, "preparation.json", state, expected_revision=revision)
        return {"ok": True, "competitors": state["competitors"], "preparation": state}
    finally:
        lock.unlink(missing_ok=True)


def list_competitors(directory: str) -> dict:
    """Read the recorded competitor candidates without changing them."""
    status = preparation_status(directory)
    return {"ok": True, "competitors": status.get("competitors", [])}


def remove_competitor(directory: str, url: str) -> dict:
    """Drop one candidate from the shortlist; its local workspace stays on disk, untouched."""
    root, _ = _load(directory)
    target = _competitor_identity(_candidate_url(url))
    lock = root / ".prepare.lock"
    try:
        fd = os.open(lock, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError as exc:
        raise ValueError("project preparation is already running") from exc
    os.close(fd)
    try:
        current = read_document(root, "preparation.json")
        if current is None or not isinstance(current.get("competitors"), list):
            raise ValueError("no competitor candidates are recorded")
        if not isinstance(current.get("steps"), dict):
            raise ValueError("invalid project preparation state")
        revision = current.get("revision", 0)
        kept = [row for row in current["competitors"] if _competitor_identity(row["url"]) != target]
        if len(kept) == len(current["competitors"]):
            raise ValueError("competitor is not recorded: " + url)
        removed = next(
            row for row in current["competitors"] if _competitor_identity(row["url"]) == target
        )
        current["competitors"] = kept
        current["steps"]["competitors"] = {
            "state": "run" if kept else "not_run",
            "reason": "operator-supplied candidate shortlist; competitiveness is not verified"
            if kept
            else "no supplied or authorized competitor source; no competitors invented",
            "count": len(kept),
        }
        current["revision"] = revision + 1
        write_document(root, "preparation.json", current, expected_revision=revision)
        return {
            "ok": True,
            "removed": removed["url"],
            "workspace_kept": removed.get("directory"),
            "competitors": kept,
        }
    finally:
        lock.unlink(missing_ok=True)


_CAP_STOPS = frozenset({"stopped_by_budget", "url_limit"})


def _has_saved_audit(path: Path) -> bool:
    """A scan holds an audit either inline or in its audit.v2 companion."""
    from seohead.storage import open_scan

    try:
        with contextlib.closing(open_scan(path)) as con:
            return (
                con.execute("SELECT EXISTS(SELECT 1 FROM audit WHERE singleton=1)").fetchone()[0]
                == 1
            )
    except (ValueError, OSError, sqlite3.Error):
        return False


def _audited_scans(root: Path, target: str):
    """Finished, audited native scans of this site identity, newest first."""
    from seohead.storage.history import list_scans

    for row in list_scans(root / "scans", limit=1000)["items"]:
        if row["lifecycle"] != "finished":
            continue
        try:
            same = _target(row["start_url"]) == target
        except (TypeError, ValueError):
            continue
        if same and (row["audit_bytes"] or _has_saved_audit(Path(row["path"]))):
            yield row


def _scan_url_cap(path: Path) -> int | None:
    from seohead.storage import open_scan

    try:
        with contextlib.closing(open_scan(path)) as con:
            config = json.loads(
                con.execute("SELECT config_json FROM scan WHERE singleton=1").fetchone()[0]
            )
        return config["limits"]["max_urls"]
    except (KeyError, TypeError, ValueError, OSError):
        return None


def _is_complete(row: dict) -> bool:
    return row["finish_reason"] == "finished" and not row["crawl_partial"]


def latest_project_scan(directory: str) -> Path:
    """Return the newest finished, audited, complete scan of the project's own site."""
    root, project = _load(directory)
    for row in _audited_scans(root, project["site"]["target"]):
        if _is_complete(row):
            return Path(row["path"])
    raise ValueError(
        "no finished, complete audited scan in this project; run project-prepare or a crawl first"
    )


def _reusable_scan(root: Path, target: str, max_urls: int | None) -> Path | None:
    """Newest saved scan that already answers this request, so preparation does not re-crawl.

    Without a requested cap any complete scan qualifies. With a cap, the scan must have been
    run under that exact cap and either finished inside it or stopped at it.
    """
    for row in _audited_scans(root, target):
        if max_urls is None:
            if _is_complete(row):
                return Path(row["path"])
        elif (_is_complete(row) or row["finish_reason"] in _CAP_STOPS) and _scan_url_cap(
            Path(row["path"])
        ) == max_urls:
            return Path(row["path"])
    return None


def prepare_project(
    directory: str,
    *,
    tools: dict,
    template: dict | None = None,
    competitors: list | None = None,
    approve_large_crawl: bool = False,
    producer_build: str | None = None,
    max_urls: int | None = None,
) -> dict:
    """Run the bounded native preparation path using explicitly injected shared tools.

    ``max_urls`` is the owner's URL cap for the preparation crawl. It replaces the quick
    default and, when set, a stop at that cap is reported as reaching the owner's limit.
    """
    from .priorities import project_priorities
    from .workspace import project_status

    if max_urls is not None and (type(max_urls) is not int or not 1 <= max_urls <= 1_000_000):
        raise ValueError("max_urls must be an integer from 1 to 1000000")
    root, project = _load(directory)
    policy = project_policy(directory)["policy"]
    if competitors is not None and (
        not isinstance(competitors, list) or len(competitors) > policy["competitor_limit"]
    ):
        raise ValueError("competitor candidate list exceeds the project limit")
    previous_preparation = preparation_status(directory)
    previous_candidates = [
        {key: row[key] for key in ("url", "source", "observed_at")}
        for row in previous_preparation.get("competitors", [])
    ]
    candidates = [
        _candidate(value)
        for value in (competitors if competitors is not None else previous_candidates)
    ]
    if len(candidates) > policy["competitor_limit"]:
        raise ValueError("saved competitor candidate list exceeds the current project limit")
    if len({row["url"] for row in candidates}) != len(candidates):
        raise ValueError("duplicate competitor candidates")
    if any(_target(row["url"]) == project["site"]["target"] for row in candidates):
        raise ValueError("the primary site cannot be its own competitor candidate")
    lock = root / ".prepare.lock"
    try:
        fd = os.open(lock, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError as exc:
        raise ValueError("project preparation is already running") from exc
    os.close(fd)
    state = {
        "format": PREPARATION_FORMAT,
        "project_uuid": project["project_uuid"],
        "revision": 1,
        "started_at": _now(),
        "state": "running",
        "steps": {},
        "competitors": [],
    }

    def save():
        previous = read_document(root, "preparation.json")
        state["revision"] = (previous.get("revision", 0) if previous else 0) + 1
        write_document(root, "preparation.json", state)

    try:
        initialize_coverage(root, template=template)
        for slug, title in (
            ("sitemap", "Review sitemap discovery and coverage"),
            ("crawl", "Review the bounded preparation crawl"),
            ("competitors", "Review competitor candidates"),
            ("plan", "Review the initial project plan"),
            ("delivery", "Review the final client deliverable"),
        ):
            item_id = "custom:preparation-" + slug
            current = coverage_status(root)
            if not any(row["id"] == item_id for row in current["items"]):
                update_item(
                    root,
                    {
                        "id": item_id,
                        "title": title,
                        "execution_kind": "deliverable" if slug == "delivery" else "manual",
                        "order": 0,
                    },
                    current["revision"],
                )
        save()
        quick = policy["quick_crawl"]
        pages = quick["pages"] if max_urls is None else max_urls
        # A requested cap scales the other budgets so the URL cap, not an unrelated budget, stops the crawl.
        budgets = {
            "pages": pages,
            "requests": quick["requests"]
            if max_urls is None
            else max(quick["requests"], 3 * pages),
            "seconds": quick["seconds"] if max_urls is None else max(quick["seconds"], 2 * pages),
        }
        overrides = {
            **policy["crawl_overrides"],
            "limits.max_urls": budgets["pages"],
            "limits.max_requests": budgets["requests"],
            "limits.max_crawl_seconds": budgets["seconds"],
            "sitemaps.auto_discover": True,
        }
        reused = _reusable_scan(root, project["site"]["target"], max_urls)
        state["steps"]["crawl"] = {
            "state": "running",
            "reason": "reusing latest suitable saved scan"
            if reused
            else "bounded native crawl requested",
            "budgets": budgets,
        }
        save()
        try:
            if reused is not None:
                result = {"ok": True, "scan": str(reused)}
            else:
                result = tools["crawl_site"](
                    project=str(root),
                    overrides=overrides,
                    approve_large_crawl=approve_large_crawl,
                    producer_build=producer_build,
                )
            if not isinstance(result, dict) or result.get("ok") is False:
                raise ValueError(
                    str(result.get("error", "crawl was unavailable"))
                    if isinstance(result, dict)
                    else "invalid crawl result"
                )
            scan = result.get("scan") or result.get("scan_path")
            if isinstance(scan, dict):
                scan = scan.get("path")
            if not isinstance(scan, str):
                raise ValueError("crawl did not return its retained scan path")
            source = Path(scan).resolve()
            if not source.is_relative_to(root / "scans") or not source.is_file():
                raise ValueError("crawl result is not a retained project scan")
            relative = source.relative_to(root).as_posix()
            from seohead.storage import open_scan

            from .evidence import audit_facts

            with contextlib.closing(open_scan(source)) as con:
                header = dict(con.execute("SELECT * FROM scan WHERE singleton=1").fetchone())
                run, coverage, fired = audit_facts(source, con)
            at_owner_cap = max_urls is not None and header["finish_reason"] in _CAP_STOPS
            state["steps"]["crawl"] = {
                "state": "partial" if header["crawl_partial"] and not at_owner_cap else "run",
                "artifact": relative,
                "scan_uuid": header["scan_uuid"],
                "partial": bool(header["crawl_partial"]) and not at_owner_cap,
                "reason": (
                    f"reached your URL limit ({max_urls})"
                    if at_owner_cap
                    else header["finish_reason"]
                ),
                "reused": reused is not None,
            }
            sitemap_skips = [
                row
                for row in run.get("checks_skipped", [])
                if str(row.get("id", "")).startswith("SITEMAP_")
            ]
            state["steps"]["sitemap"] = {
                "state": "partial" if sitemap_skips else "run",
                "artifact": relative,
                "reason": "saved sitemap coverage; unavailable checks remain explicit",
                "unavailable": sitemap_skips,
            }
            from .crawl_evidence import record_scan

            evidence = record_scan(root, source)
            executed = set(coverage.get("checks_silent_ids", []))
            executed.update(fired)
            unavailable = {
                row.get("id"): row.get("reason") for row in run.get("checks_skipped", [])
            }
            recording_errors = list(evidence.get("refused", []))
            for item in coverage_status(root)["items"]:
                if item["kind"] != "check" or item["complete"] or not item["enabled"]:
                    continue
                check_id = item["id"].removeprefix("check:")
                if check_id in executed:
                    entry = {
                        "status": "succeeded",
                        "reason": "Executed in the bounded preparation scan; see recorded measurement scope",
                        "artifact": relative,
                    }
                elif check_id in unavailable:
                    entry = {"status": "unavailable", "reason": str(unavailable[check_id])}
                else:
                    continue
                try:
                    record_execution(root, item["id"], entry, coverage_status(root)["revision"])
                except ValueError as exc:
                    recording_errors.append({"id": item["id"], "reason": str(exc)})
            if recording_errors:
                state["steps"]["crawl"]["recording_gaps"] = recording_errors
        except (ValueError, OSError) as exc:
            state["steps"]["crawl"] = {"state": "not_run", "reason": str(exc)}
            state["steps"]["sitemap"] = {
                "state": "not_run",
                "reason": "crawl source unavailable; sitemap coverage not established",
            }
        save()
        if candidates:
            state["competitors"].extend(_competitor_workspaces(root, candidates))
            state["steps"]["competitors"] = {
                "state": "run",
                "reason": "operator-supplied candidate shortlist; competitiveness is not verified",
                "count": len(candidates),
            }
        else:
            state["steps"]["competitors"] = {
                "state": "not_run",
                "reason": "no supplied or authorized competitor source; no competitors invented",
            }
        current = coverage_status(root)
        project_priorities(str(root), apply=True, expected_revision=current["revision"])
        plan_name = "initial-plan-" + uuid.uuid4().hex[:12] + ".md"
        state["steps"]["plan"] = {
            "state": "run",
            "artifact": "reports/" + plan_name,
            "reason": "initial work plan created; manual and deliverable acceptance remain pending",
        }
        lines = [
            "# Initial project plan",
            "",
            f"Target: {project['site']['target']}",
            "",
            "Preparation observations:",
        ]
        lines.extend(
            f"- {name}: {row['state']} — {row.get('reason', '')}"
            for name, row in state["steps"].items()
        )
        lines.extend(
            [
                "",
                "Review the preparation checklist, verify candidate competitors, and use project status for remaining work. This file does not establish a completed audit or client delivery.",
                "",
            ]
        )
        (root / "reports" / plan_name).write_text("\n".join(lines), encoding="utf-8")
        state["state"] = (
            "prepared"
            if all(row["state"] == "run" for row in state["steps"].values())
            else "partial"
        )
        state["finished_at"] = _now()
        save()
        return {"ok": True, "preparation": state, "status": project_status(str(root))}
    except (Exception, KeyboardInterrupt) as exc:
        state.update(
            state="interrupted" if isinstance(exc, KeyboardInterrupt) else "failed",
            reason=str(exc) or "preparation interrupted",
            finished_at=_now(),
        )
        with contextlib.suppress(OSError, ValueError):
            save()
        raise
    finally:
        lock.unlink(missing_ok=True)


def aggregate_coverage(
    directory: str,
    primary: dict,
    *,
    _coverage_views: dict | None = None,
    _verify_evidence: bool = True,
) -> dict:
    """Combine declared site checklists without assigning another site's evidence to a row."""
    root, project = _load(directory)
    views = {} if _coverage_views is None else _coverage_views
    views[str(root)] = primary
    preparation = preparation_status(directory)
    children = preparation.get("competitors", [])
    if not children:
        return primary
    sites = [
        {
            "directory": ".",
            "project_uuid": project["project_uuid"],
            "site": project["site"]["target"],
            "checklist": primary,
        }
    ]
    seen = {project["project_uuid"]}
    unavailable = []
    for child in children:
        relative = child.get("directory")
        if (
            not isinstance(relative, str)
            or Path(relative).is_absolute()
            or ".." in Path(relative).parts
            or not relative.startswith("competitors/")
        ):
            raise ValueError("unsafe competitor project reference")
        path = root / relative
        if path.is_symlink() or not path.resolve().is_relative_to(root):
            raise ValueError("unsafe competitor project reference")
        try:
            _, metadata = _load(path)
        except (OSError, ValueError) as exc:
            unavailable.append({"directory": relative, "state": "unavailable", "reason": str(exc)})
            continue
        if (
            metadata["project_uuid"] != child.get("project_uuid")
            or metadata["site"]["target"] != _competitor_identity(child.get("url"))
            or metadata["project_uuid"] in seen
        ):
            # A replaced or duplicated workspace no longer matches its preparation row: keep the
            # owner's checklist readable and report this competitor instead of failing the project.
            unavailable.append(
                {
                    "directory": relative,
                    "state": "unavailable",
                    "reason": "competitor project identity mismatch",
                }
            )
            continue
        seen.add(metadata["project_uuid"])
        child_key = str(path.resolve())
        if child_key not in views:
            views[child_key] = coverage_status(path, _verify_evidence=_verify_evidence)
        sites.append(
            {
                "directory": relative,
                "project_uuid": metadata["project_uuid"],
                "site": metadata["site"]["target"],
                "checklist": views[child_key],
            }
        )
    result = copy.deepcopy(primary)
    result["sites"] = [
        {key: value for key, value in site.items() if key != "checklist"}
        | {"counts": site["checklist"].get("counts"), "state": site["checklist"]["state"]}
        for site in sites
    ] + unavailable
    if any(site["checklist"]["state"] != "initialized" for site in sites):
        result.update(
            state="not_initialized",
            complete=False,
            counts_known=False,
            counts=None,
            by_kind={},
            items=[],
            views={key: [] for key in primary.get("views", {})},
            coverage=None,
            reason="one or more site checklists are not initialized",
        )
        return result
    result["counts"] = {
        key: sum(site["checklist"]["counts"][key] for site in sites) for key in primary["counts"]
    }
    if "coverage" in primary:
        axes = {}
        for name in primary["coverage"]:
            merged = dict(primary["coverage"][name])
            for site in sites[1:]:
                axis = site["checklist"]["coverage"][name]
                for key in ("numerator", "denominator", "measured_urls"):
                    if key in merged and key in axis:
                        merged[key] = (
                            None
                            if merged[key] is None or axis[key] is None
                            else merged[key] + axis[key]
                        )
                for key in ("excluded", "pending_exclusion", "not_agreed", "unfinished"):
                    if key in merged:
                        merged[key] += axis[key]
                for key in ("unverified_measurements",):
                    if key in merged:
                        merged[key] += axis[key]
                if "population_kind" in merged and (
                    axis["population_kind"] != merged["population_kind"]
                    or axis["population_name"] != merged["population_name"]
                ):
                    merged["population_kind"] = "mixed"
                    merged["population_name"] = None
                if axis["state"] != "measured" or not merged.get("denominator"):
                    merged["state"] = "unknown" if not merged.get("denominator") else "partial"
                    merged["reason"] = (
                        "one or more site checklists cannot justify the denominator"
                        if not merged.get("denominator")
                        else "one or more site checklists report a partial or unknown state"
                    )
            axes[name] = merged
        result["coverage"] = axes
        result["exclusions"] = [
            {**entry, "id": "site:" + site["project_uuid"] + "/" + entry["id"]}
            for site in sites
            for entry in site["checklist"].get("exclusions", [])
        ]
    result["by_kind"] = {
        kind: {
            key: sum(site["checklist"]["by_kind"][kind][key] for site in sites)
            for key in ("total", "complete")
        }
        for kind in primary["by_kind"]
    }
    result["items"] = []
    result["views"] = {key: [] for key in primary["views"]}
    for site in sites:
        prefix = "site:" + site["project_uuid"] + "/"
        for item in site["checklist"]["items"]:
            result["items"].append(
                {
                    **item,
                    "id": prefix + item["id"],
                    "item_id": item["id"],
                    "project_directory": site["directory"],
                    "project_uuid": site["project_uuid"],
                }
            )
        for name, ids in site["checklist"]["views"].items():
            result["views"][name].extend(prefix + item_id for item_id in ids)
    result["complete"] = all(site["checklist"]["complete"] for site in sites)
    result["counts_known"] = True
    return result


def resolve_item_scope(
    directory: str, item_id: str, *, _verify_evidence: bool = True
) -> tuple[str, str]:
    """Resolve only declared qualified site rows; unqualified IDs keep their local scope."""
    if not isinstance(item_id, str) or not item_id.startswith("site:"):
        return directory, item_id
    prefix, separator, local_id = item_id.partition("/")
    if not separator:
        raise ValueError("qualified item ID has no local ID")
    root, project = _load(directory)
    requested = prefix.removeprefix("site:")
    if requested == project["project_uuid"]:
        return str(root), local_id
    for child in preparation_status(directory).get("competitors", []):
        if child.get("project_uuid") == requested:
            # Reuse the complete validation before resolving a writable child.
            aggregate_coverage(
                directory,
                coverage_status(root, _verify_evidence=_verify_evidence),
                _verify_evidence=_verify_evidence,
            )
            return str(root / child["directory"]), local_id
    raise ValueError("item belongs to an undeclared project scope")
