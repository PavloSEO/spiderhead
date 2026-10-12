"""Read bounded, sanitized summaries from explicitly retained provider artifacts."""

from __future__ import annotations

import hashlib
import json
import os
import re
import stat
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit, urlunsplit

from seohead.data_sources.credentials import is_private_mode

MAX_ARTIFACT_BYTES = 16 * 1024 * 1024
MAX_SAMPLES = 25
MAX_AUDITS_PER_SAMPLE = 100
_ARTIFACT_REF = re.compile(r"^local-artifact:([0-9a-f]{64})$")


def _public_url(value: Any) -> str | None:
    """Retain the page identity while dropping query/fragment data that may contain tokens."""
    if not isinstance(value, str):
        return None
    try:
        parts = urlsplit(value)
        port = parts.port
    except ValueError:
        return None
    if parts.scheme not in {"http", "https"} or not parts.hostname:
        return None
    host = parts.hostname
    if port is not None:
        host = f"{host}:{port}"
    if ":" in host and not host.startswith("["):
        host = f"[{host}]"
    return urlunsplit((parts.scheme, host, parts.path or "/", "", ""))


def _small_text(value: Any, limit: int = 200) -> str | None:
    if not isinstance(value, str):
        return None
    text = " ".join(value.split())
    text = re.sub(r"(https?://[^\s?#]+)[?#][^\s]+", r"\1", text)
    return text[:limit]


def read_pagespeed_artifact(
    artifact_ref: str,
    artifact_dir: str,
    *,
    offset: int = 0,
    limit: int = 25,
) -> dict[str, Any]:
    """Return a field-allowlisted PSI sample page; never return the saved raw envelope."""
    match = _ARTIFACT_REF.fullmatch(artifact_ref or "")
    if not match:
        raise ValueError("artifact_ref must be a local provider artifact reference")
    if (
        type(offset) is not int
        or offset < 0
        or type(limit) is not int
        or not 1 <= limit <= MAX_SAMPLES
    ):
        raise ValueError("offset must be nonnegative and limit must be 1..25")
    root = Path(artifact_dir)
    if root.is_symlink() or not root.is_dir():
        raise ValueError("artifact_dir must be an existing regular directory")
    root = root.resolve(strict=True)
    if not is_private_mode(root.stat().st_mode):
        raise ValueError("artifact_dir must be private")
    if hasattr(os, "getuid") and root.stat().st_uid != os.getuid():
        raise ValueError("artifact_dir must be owned by the current user")
    path = root / f"provider-{match.group(1)[:16]}.json"
    try:
        info = path.lstat()
    except OSError:
        raise ValueError("provider artifact was not found") from None
    if (
        stat.S_ISLNK(info.st_mode)
        or not stat.S_ISREG(info.st_mode)
        or info.st_nlink != 1
        or info.st_size > MAX_ARTIFACT_BYTES
        or not is_private_mode(info.st_mode)
        or (hasattr(os, "getuid") and info.st_uid != os.getuid())
    ):
        raise ValueError("provider artifact is unsafe or exceeds its byte limit")
    resolved = path.resolve(strict=True)
    if resolved.parent != root:
        raise ValueError("provider artifact escapes artifact_dir")
    descriptor = os.open(resolved, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        if (
            not stat.S_ISREG(opened.st_mode)
            or opened.st_nlink != 1
            or opened.st_size > MAX_ARTIFACT_BYTES
            or opened.st_ino != info.st_ino
            or opened.st_dev != info.st_dev
            or (hasattr(os, "getuid") and opened.st_uid != os.getuid())
        ):
            raise ValueError("provider artifact changed while it was being opened")
        with os.fdopen(descriptor, "rb") as stream:
            raw = stream.read(MAX_ARTIFACT_BYTES + 1)
        descriptor = -1
    finally:
        if descriptor >= 0:
            os.close(descriptor)
    if len(raw) > MAX_ARTIFACT_BYTES:
        raise ValueError("provider artifact exceeds its byte limit")
    try:
        stored = json.loads(raw)
    except (UnicodeError, ValueError):
        raise ValueError("provider artifact is not valid JSON") from None
    if (
        not isinstance(stored, dict)
        or set(stored) != {"evidence", "result"}
        or not isinstance(stored["evidence"], dict)
        or stored["evidence"].get("format") != "seohead.provider-evidence.v1"
        or stored["evidence"].get("provider") != "pagespeed"
        or stored["evidence"].get("operation") not in {"mobile_samples", "desktop_samples"}
        or not isinstance(stored["result"], dict)
    ):
        raise ValueError("artifact is not a retained PageSpeed collection")
    digest = hashlib.sha256(
        json.dumps(stored, sort_keys=True, default=str).encode("utf-8")
    ).hexdigest()
    if digest != match.group(1):
        raise ValueError("provider artifact reference does not match its content")
    result = stored["result"]
    samples = result.get("samples")
    if (
        not isinstance(samples, list)
        or len(samples) > MAX_SAMPLES
        or any(not isinstance(sample, dict) for sample in samples)
    ):
        raise ValueError("PageSpeed artifact has an invalid sample list")
    selected = []
    for sample in samples[offset : offset + limit]:
        categories = sample.get("categories") or {}
        safe_categories = {}
        if isinstance(categories, dict):
            for name, category in list(categories.items())[:16]:
                if not isinstance(category, dict):
                    continue
                score = category.get("score")
                if score is not None and (
                    isinstance(score, bool) or not isinstance(score, int | float)
                ):
                    score = None
                safe_categories[str(name)[:64]] = {
                    "score": score,
                    "title": _small_text(category.get("title"), 100),
                }
        audits = sample.get("audits") or {}
        safe_audits = {}
        if isinstance(audits, dict):
            for name, audit in list(sorted(audits.items()))[:MAX_AUDITS_PER_SAMPLE]:
                if not isinstance(audit, dict):
                    continue
                score = audit.get("score")
                numeric = audit.get("numeric_value")
                safe_audits[str(name)[:100]] = {
                    "score": score if score is None or type(score) in {int, float} else None,
                    "numeric_value": numeric
                    if numeric is None or type(numeric) in {int, float}
                    else None,
                    "display_value": _small_text(audit.get("display_value")),
                    "score_display_mode": _small_text(audit.get("score_display_mode"), 40),
                }
        selected.append(
            {
                "url": _public_url(sample.get("url")),
                "strategy": sample.get("strategy")
                if sample.get("strategy") in {"mobile", "desktop"}
                else None,
                "state": sample.get("state")
                if sample.get("state") in {"complete", "failed"}
                else "unavailable",
                "final_url": _public_url(sample.get("final_url")),
                "fetch_time": _small_text(sample.get("fetch_time"), 40),
                "retrieved_at": _small_text(sample.get("fetch_time"), 40),
                "lighthouse_version": _small_text(sample.get("lighthouse_version"), 40),
                "categories": safe_categories,
                "audits": safe_audits,
                "lab_only": True,
            }
        )
    return {
        "schema_version": "pagespeed-artifact-summary.v1",
        "provider": "pagespeed",
        "operation": stored["evidence"]["operation"],
        "retrieved_at": _small_text(stored["evidence"].get("retrieved_at"), 40),
        "total": len(samples),
        "offset": offset,
        "limit": limit,
        "has_more": offset + len(selected) < len(samples),
        "samples": selected,
        "lab_only": True,
    }
