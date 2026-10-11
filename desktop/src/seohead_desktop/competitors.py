"""Competitor intake and state for the Desktop: pure helpers over bounded core data (no Qt, no I/O).

Writing goes through the core (``seo_project_competitors_add``); scans go through the owned scan queue. This module
only parses what the operator pasted, builds the core arguments and derives each competitor's state from the
observer snapshot. A competitor is never reported as audited unless a finished retained scan exists for it.
"""

from __future__ import annotations

import re
from urllib.parse import urlsplit

MAX_TEXT = 64 * 1024
MAX_URL = 2048
DEFAULT_URL_CAP = 50
MAX_URL_CAP = 1000  # above this the core asks for explicit approval of a large crawl; Desktop does not ask for it
# Core default approval thresholds (pages 1000, requests 3000, seconds 600). A competitor scan stays inside them, so it
# starts without approval. Zero would mean unbounded, and the core refuses an unbounded request as a large crawl.
REQUESTS_PER_PAGE = 3
MAX_CRAWL_SECONDS = 600
_SPLIT = re.compile(r"[\s,;]+")
_FINISHED = {"finished"}


def parse_candidates(text: str, *, existing=(), limit: int = 5) -> dict:
    """Split pasted text into accepted HTTP(S) candidates and explicit refusals.

    Tokens are separated by whitespace, commas or semicolons. A bare host gets ``https://``; the URL is otherwise kept
    exactly as entered (no ``www`` stripping). ``existing`` URLs and the project limit are checked here so the operator
    sees the refusal before the core is called.
    """
    if not isinstance(text, str):
        return {"accepted": [], "refused": [("", "clipboard is not text")]}
    if len(text) > MAX_TEXT:
        return {"accepted": [], "refused": [("", "the pasted text is too large")]}
    known = {_key(url) for url in existing}
    accepted: list[str] = []
    refused: list[tuple[str, str]] = []
    for token in (part for part in _SPLIT.split(text.strip()) if part):
        url, reason = _normalize(token)
        if reason:
            refused.append((token[:200], reason))
        elif _key(url) in known or any(_key(url) == _key(item) for item in accepted):
            refused.append((url, "already in the list"))
        elif len(known) + len(accepted) >= limit:
            refused.append((url, f"the project limit is {limit} competitors"))
        else:
            accepted.append(url)
    return {"accepted": accepted, "refused": refused}


def _key(url: str) -> tuple:
    """Identity of a site address: the core stores the same URL with a trailing slash and without ``www``."""
    parsed = urlsplit(url)
    host = (parsed.hostname or "").lower().removeprefix("www.")
    return parsed.scheme, host, parsed.path.rstrip("/") or "/", parsed.query


def _normalize(token: str) -> tuple[str | None, str | None]:
    if len(token) > MAX_URL or any(ord(char) < 32 for char in token):
        return None, "not a bounded URL"
    candidate = token if "://" in token else "https://" + token
    try:
        parsed = urlsplit(candidate)
        _ = parsed.port  # malformed ports raise ValueError
    except ValueError:
        return None, "not a valid URL"
    if parsed.scheme not in {"http", "https"}:
        return None, "only http and https URLs can be scanned"
    if not parsed.hostname or "." not in parsed.hostname:
        return None, "the host name is missing or incomplete"
    if parsed.username or parsed.password or parsed.fragment:
        return None, "credentials and fragments are not accepted"
    return candidate, None


def candidate_arguments(directory: str, urls, *, source: str, observed_at: str, consumer: str) -> dict:
    """Arguments for ``seo_project_competitors_add``; each candidate carries its provenance."""
    return {
        "directory": directory,
        "competitors": [{"url": url, "source": source, "observed_at": observed_at} for url in urls],
        "consumer": consumer,
    }


def scan_budget(cap: int) -> dict:
    """Explicit request and time budgets for one competitor scan, inside the core default thresholds."""
    return {"limits.max_requests": cap * REQUESTS_PER_PAGE, "limits.max_crawl_seconds": MAX_CRAWL_SECONDS}


def valid_url_cap(value) -> int | None:
    """The per-site URL cap as a bounded integer, or None when it is not one."""
    if type(value) is not int or not 1 <= value <= MAX_URL_CAP:
        return None
    return value


def competitor_rows(sites) -> list[dict]:
    """Competitor rows from one observer snapshot; state comes from retained finished scans only."""
    rows = []
    for site in sites if isinstance(sites, list) else []:
        if not isinstance(site, dict) or site.get("role") != "competitor":
            continue
        target = (site.get("site") or {}).get("target") or ""
        candidate = site.get("candidate") or {}
        scans = ((site.get("scans") or {}).get("items")) or []
        finished = [item for item in scans if isinstance(item, dict) and item.get("lifecycle") in _FINISHED]
        latest = finished[0] if finished else None
        rows.append(
            {
                "url": target,
                "host": (urlsplit(target).hostname or target) if target else "—",
                "directory": site.get("directory"),
                "project_uuid": site.get("project_uuid"),
                "candidate_state": candidate.get("state") or "—",
                "state": state_text(candidate, latest, scans),
                "scan_count": len(finished),
                "latest_finished_at": latest.get("finished_at") if latest else None,
                "partial": bool(latest and latest.get("crawl_partial")),
            }
        )
    return rows


def state_text(candidate: dict, latest: dict | None, scans: list) -> str:
    """Plain state: finished scans win over the candidate note; an unfinished scan is said to be unfinished."""
    if latest is not None:
        if latest.get("crawl_partial"):
            return "scanned, partial"
        return "scanned"
    if any(isinstance(item, dict) and item.get("lifecycle") == "running" for item in scans):
        return "scan running"
    return (candidate or {}).get("state") or "candidate; audit not run"

