"""Validate saved completion evidence without running checks or changing artifacts."""

from __future__ import annotations

import hashlib
import json
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

from .coverage import URL_ENUMERATION_LIMIT, _text


def artifact_path(root: Path, value: Any) -> Path:
    """Accept portable regular files inside the project, excluding its control files."""
    _text(value, "artifact", 1024)
    relative = Path(value)
    if relative.is_absolute() or ".." in relative.parts or not relative.parts:
        raise ValueError("artifact must be a portable project-relative path")
    if relative.parts[0] not in {"scans", "reports"}:
        raise ValueError("evidence belongs under scans/ or reports/")
    current = root
    for part in relative.parts:
        current /= part
        if current.is_symlink():
            raise ValueError("evidence must not traverse symlinks")
    if not current.is_file() or not current.resolve().is_relative_to(root):
        raise ValueError("evidence artifact is missing or unsafe")
    return current


def _digest(path: Path, *, deadline: float | None = None, max_bytes: int | None = None) -> str:
    """Hash in fixed-size blocks, optionally enforcing an explicit read budget."""
    if max_bytes is not None and path.stat().st_size > max_bytes:
        raise ValueError("evidence artifact exceeds the hashing byte budget")
    digest = hashlib.sha256()
    used = 0
    with path.open("rb") as stream:
        while True:
            if deadline is not None and time.monotonic() > deadline:
                raise ValueError("evidence hashing time budget exceeded")
            block = stream.read(1024 * 1024)
            if not block:
                break
            used += len(block)
            if max_bytes is not None and used > max_bytes:
                raise ValueError("evidence artifact exceeds the hashing byte budget")
            digest.update(block)
    return digest.hexdigest()


def artifact_identity(path: Path) -> list[int]:
    """Capture cheap replacement/change identity; this is not a content digest."""
    info = path.stat()
    return [info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns]


def _artifact_receipt(root: Path, reference: str) -> dict:
    path = artifact_path(root, reference)
    before = artifact_identity(path)
    digest = _digest(path)
    if artifact_identity(artifact_path(root, reference)) != before:
        raise ValueError("evidence changed while it was being recorded")
    return {
        "artifact": reference,
        "sha256": digest,
        "artifact_verification": {
            "sha256": digest,
            "identity": before,
            "verified_at": datetime.now(timezone.utc).isoformat(),
        },
    }


def audit_facts(path: Path, con: Any) -> tuple[dict, dict, set]:
    """Return run, check coverage and fired check IDs of a retained scan's audit.

    A streamed scan keeps its audit in the audit.v2 companion, so the legacy ``audit`` row is
    absent; the companion header and its issue collection are read instead, never a second copy.
    """
    row = con.execute("SELECT document_json FROM audit WHERE singleton=1").fetchone()
    if row is not None:
        audit = json.loads(row[0])
        fired = {item.get("check") for item in audit.get("issues", [])}
        header = audit
    else:
        from seohead.storage.audit_v2 import AuditV2Reader

        with AuditV2Reader(path) as reader:
            header = reader.header
            fired = {item.get("check") for item in reader.iter_collection("/issues")}
    return (
        header.get("run", {}),
        header.get("summary", {}).get("check_coverage", {}),
        fired,
    )


def _saved_check(root: Path, definition: dict, value: Any) -> dict:
    """Bind automatic completion to the check outcome and identity inside scan.v1."""
    from seohead.storage import open_scan

    from .catalogue import load_catalogue

    path = artifact_path(root, value)
    before_identity = artifact_identity(path)
    before = _digest(path)
    con = open_scan(path)
    try:
        scan = dict(con.execute("SELECT * FROM scan WHERE singleton=1").fetchone())
        operation = definition["operation"]
        check_id = operation.removeprefix("check:")
        run, coverage, fired = audit_facts(path, con)
        silent = set(coverage.get("checks_silent_ids", []))
        unavailable = {
            item.get("id")
            for key in ("checks_skipped", "checks_disabled")
            for item in run.get(key, [])
        }
        unavailable.update(coverage.get("checks_disabled_ids", []))
        if check_id in unavailable or check_id not in fired | silent:
            raise ValueError("saved artifact does not prove this check ran successfully")
        if run.get("crawl_valid") is False:
            raise ValueError("saved crawl is invalid")
        start = scan["start_url"] or run.get("start_url") or run.get("source")
        if (
            not isinstance(start, str)
            or urlsplit(start).netloc != urlsplit(definition["scope"]["site"]).netloc
        ):
            raise ValueError("evidence site does not match item scope")
        pages = {row[0] for row in con.execute("SELECT url FROM pages JOIN urls USING(url_id)")}
        requested = set(definition["scope"]["urls"])
        if definition["scope"]["template"] and not requested:
            raise ValueError("template measurements require explicit sample URLs")
        if requested and requested != pages:
            raise ValueError("evidence population must match the requested scope URLs")
        if not pages:
            raise ValueError("empty scan cannot complete a measurement")
        limited = bool(
            scan["crawl_partial"]
            or scan["corpus_partial"]
            or definition["scope"]["template"]
            or requested
        )
        result = {
            "artifact": value,
            "sha256": before,
            "site": definition["scope"]["site"],
            "scope": definition["scope"],
            "operation": operation,
            "operation_hash": load_catalogue()[operation]["definition_hash"],
            "source": {
                key: scan[key]
                for key in (
                    "scan_uuid",
                    "source_kind",
                    "writer_version",
                    "writer_revision",
                    "config_fingerprint",
                    "config_json",
                    "finish_reason",
                    "lifecycle",
                    "evidence_revision",
                )
                if key in scan
            },
            "observed_at": run.get("generated_at") or scan.get("created_at"),
            "measurement": {
                "state": "limited" if limited else "measured",
                "reason": "source is partial or scope is an explicit sample"
                if limited
                else "measured population in the saved scan; not a census of the whole site",
                "population": len(pages),
                "requested_urls": len(requested),
                "urls": sorted(pages)[:URL_ENUMERATION_LIMIT],
                "urls_enumerated": len(pages) <= URL_ENUMERATION_LIMIT,
                "crawl_partial": bool(scan["crawl_partial"]),
                "corpus_partial": bool(scan["corpus_partial"]),
            },
        }
    finally:
        con.close()
    if _digest(path) != before or artifact_identity(artifact_path(root, value)) != before_identity:
        raise ValueError("evidence changed while it was being recorded")
    result["artifact_verification"] = {
        "sha256": before,
        "identity": before_identity,
        "verified_at": datetime.now(timezone.utc).isoformat(),
    }
    return result


def validate_record(root: Path, definition: dict, record: Any) -> dict:
    if not isinstance(record, dict) or set(record) - {
        "status",
        "reason",
        "artifact",
        "evidence",
        "reviewer",
        "actor",
        "review",
        "signoff",
    }:
        raise ValueError("record has unsupported fields")
    status = record.get("status")
    if not isinstance(status, str) or status not in {
        "running",
        "failed",
        "unavailable",
        "succeeded",
        "not_applicable",
    }:
        raise ValueError("invalid execution status")
    reason = _text(record.get("reason"), "record reason")
    result = {"status": status, "reason": reason, "measurement": None}
    if "actor" in record:
        result["actor"] = _text(record["actor"], "record actor", 128)
    if status in {"running", "failed", "unavailable"}:
        if set(record) - {"status", "reason", "actor"}:
            raise ValueError("unfinished attempts record status, reason and optional actor only")
        return result
    if status == "not_applicable":
        # Applicability is an explicit specialist decision, never inferred from missing data.
        if set(record) - {"status", "reason", "reviewer", "artifact", "evidence", "actor"}:
            raise ValueError("applicability review records reason, reviewer and evidence basis")
        result["reviewer"] = _text(record.get("reviewer"), "reviewer", 128)
        if "artifact" in record:
            path = artifact_path(root, record["artifact"])
            if path.stat().st_size == 0:
                raise ValueError("empty artifact cannot support an applicability decision")
            result.update(_artifact_receipt(root, record["artifact"]))
        if "evidence" in record:
            result["evidence"] = _text(record["evidence"], "exclusion evidence", 512)
        if "artifact" not in result and "evidence" not in result:
            raise ValueError("applicability review requires an inspectable evidence basis")
        return result
    kind = definition["execution_kind"]
    if kind == "automatic":
        if set(record) - {"status", "reason", "artifact", "actor"}:
            raise ValueError("automatic evidence cannot be replaced by manual signoff")
        result.update(_saved_check(root, definition, record.get("artifact")))
    else:
        result["reviewer"] = _text(record.get("reviewer"), "reviewer", 128)
        if "signoff" in record and type(record["signoff"]) is not bool:
            raise ValueError("signoff must be a boolean")
        if kind == "manual" and record.get("signoff") is True:
            if "artifact" in record or "review" in record:
                raise ValueError("use either explicit signoff or reviewed evidence")
            result["signoff"] = True
        else:
            if record.get("review") != "approved":
                raise ValueError("completion requires approved artifact review")
            path = artifact_path(root, record.get("artifact"))
            if path.stat().st_size == 0:
                raise ValueError("empty artifact cannot complete a review")
            result.update(_artifact_receipt(root, record["artifact"]), review="approved")
        result["scope"] = definition["scope"]
        result["measurement"] = {
            "state": "not_measured",
            "reason": "specialist review or signoff; no automatic measurement is implied",
        }
    return result


def evidence_stale(
    root: Path, record: dict, catalogue: dict, digests: dict, *, verify_bytes: bool = True
) -> str:
    if "artifact" in record:
        try:
            artifact = record["artifact"]
            path = artifact_path(root, artifact)
            identity = artifact_identity(path)
            if verify_bytes:
                key = (artifact, *identity)
                if key not in digests:
                    digests[key] = _digest(path)
                if (
                    digests[key] != record["sha256"]
                    or artifact_identity(artifact_path(root, artifact)) != identity
                ):
                    return "evidence artifact changed"
            else:
                receipt = record.get("artifact_verification")
                if not isinstance(receipt, dict):
                    return "evidence unverified: no retained byte-verification metadata"
                if receipt.get("sha256") != record["sha256"] or receipt.get("identity") != identity:
                    return "evidence artifact metadata changed; explicit byte verification required"
        except (ValueError, OSError):
            return "evidence artifact is missing or unsafe"
    if "operation" in record:
        current = catalogue.get(record["operation"])
        if current is None or current["definition_hash"] != record["operation_hash"]:
            return "evidence operation definition changed"
    return ""
