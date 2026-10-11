"""Search Console evidence as page-level crawl findings. Pure: no network, no registry.

Joins crawl pages to a normalized GSC document (``seohead.normalized-evidence.v1``) and
evaluates three checks. A check that cannot be decided is listed under ``_skipped`` with its
reason; an absent or skipped check is never reported as clean.
"""

from __future__ import annotations

from collections.abc import Iterable, Mapping
from typing import Any
from urllib.parse import urlsplit, urlunsplit

from seohead.checks.external_join import orphan_urls
from seohead.data_sources.evidence_join import join_evidence

GSC_ORPHAN = "GSC_ORPHAN"
NO_SEARCH_DATA = "NO_SEARCH_DATA"
NON_INDEXABLE_WITH_SEARCH_DATA = "NON_INDEXABLE_WITH_SEARCH_DATA"
CHECK_IDS = (GSC_ORPHAN, NO_SEARCH_DATA, NON_INDEXABLE_WITH_SEARCH_DATA)
INSPECTION_CHECKS = {
    "GSC_INDEXED_WITH_ISSUES": "URL is on Google, But Has Issues",
    "GSC_AMP_INVALID": "AMP URL is Invalid",
    "GSC_RICH_RESULTS_INVALID": "Rich Result is Invalid",
    "GSC_INDEXABLE_NOT_INDEXED": "Indexable URL Not Indexed",
    "GSC_USER_CANONICAL_NOT_SELECTED": "User-Declared Canonical Not Selected",
    "GSC_URL_NOT_ON_GOOGLE": "URL Is Not on Google",
}

NO_EVIDENCE_REASON = "no Search Console evidence"


def _public_url(value: str) -> str:
    parts = urlsplit(value)
    host = parts.hostname or ""
    if ":" in host and not host.startswith("["):
        host = f"[{host}]"
    try:
        port = parts.port
    except ValueError:
        port = None
    netloc = f"{host}:{port}" if port is not None else host
    return urlunsplit((parts.scheme, netloc, parts.path, "", ""))


def _indexable(page: Mapping[str, Any]) -> bool | None:
    flag = page.get("indexable")
    if isinstance(flag, bool):
        return flag
    state = page.get("indexability")
    if isinstance(state, str) and state.strip():
        return state.strip().lower() == "indexable"
    return None


def _impressions(rows: Iterable[Mapping[str, Any]]) -> float:
    total = 0.0
    for row in rows:
        entry = (row.get("metrics") or {}).get("impressions") or {}
        if entry.get("state") == "measured":
            total += entry["value"]
    return total


def _origin(key: str) -> str:
    parts = urlsplit(key)
    return f"{parts.scheme}://{parts.netloc}"


def evaluate(
    crawl_pages: Iterable[Mapping[str, Any]],
    gsc_document: Mapping[str, Any] | None,
    *,
    crawl_partial: bool,
    window_complete: bool,
) -> dict[str, Any]:
    """Return ``{check_id: [{"url", "details"}], "_skipped": {check_id: reason}}``."""
    pages = [dict(page) for page in crawl_pages]
    findings: dict[str, Any] = {check: [] for check in CHECK_IDS}
    if gsc_document is None:
        findings["_skipped"] = dict.fromkeys(CHECK_IDS, NO_EVIDENCE_REASON)
        return findings

    join = join_evidence(pages, dict(gsc_document))
    skipped: dict[str, str] = {}
    if crawl_partial or not window_complete:
        reason = "crawl is partial" if crawl_partial else "reporting window is incomplete"
        skipped[GSC_ORPHAN] = reason
        skipped[NO_SEARCH_DATA] = reason
    collection = (gsc_document.get("mapping") or {}).get("collection") or {}
    degraded = [flag for flag in ("sampled", "thresholded", "truncated") if collection.get(flag)]
    if collection.get("state") not in (None, "complete"):
        degraded.append(collection["state"])
    if degraded:
        skipped.setdefault(NO_SEARCH_DATA, f"collection coverage is {', '.join(degraded)}")
    unknown = sum(1 for page in pages if _indexable(page) is None)
    if unknown:
        reason = f"crawl indexability missing on {unknown} pages"
        skipped.setdefault(NO_SEARCH_DATA, reason)
        skipped.setdefault(NON_INDEXABLE_WITH_SEARCH_DATA, reason)

    if GSC_ORPHAN not in skipped:
        origins = sorted({_origin(entry["key"]) for entry in join["matched"] + join["crawl_only"]})
        if origins:
            impressions = {
                entry["key"]: _impressions([entry["row"]]) for entry in join["external_only"]
            }
            orphans = orphan_urls(
                {
                    "external_only": [{"url": key} for key, n in impressions.items() if n > 0],
                    "crawl_origins": origins,
                }
            )
            findings[GSC_ORPHAN] = [
                {"url": url, "details": {"impressions": impressions[url]}} for url in orphans
            ]
        else:
            skipped[GSC_ORPHAN] = "no crawled origin to scope orphans against"

    if NO_SEARCH_DATA not in skipped:
        findings[NO_SEARCH_DATA] = [
            {"url": entry["url"], "details": {"impressions": 0}}
            for entry in join["crawl_only"]
            if _indexable(entry["page"])
        ]

    if NON_INDEXABLE_WITH_SEARCH_DATA not in skipped:
        totals = {entry["key"]: _impressions(entry["rows"]) for entry in join["matched"]}
        seen: set[str] = set()
        for entry in join["matched"]:
            key = entry["key"]
            if key in seen or _indexable(entry["page"]) is not False or totals[key] <= 0:
                continue
            seen.add(key)
            findings[NON_INDEXABLE_WITH_SEARCH_DATA].append(
                {
                    "url": entry["url"],
                    "details": {"impressions": totals[key], "crawl_partial": crawl_partial},
                }
            )

    findings["_skipped"] = skipped
    return findings


def evaluate_inspections(
    crawl_pages: Iterable[Mapping[str, Any]],
    inspections: Iterable[Mapping[str, Any]],
    *,
    requested: int,
    deferred: int = 0,
) -> dict[str, Any]:
    """Map explicit, bounded URL Inspection responses to six positive sample findings.

    No unsampled URL is inferred clean. Empty check lists describe the inspected sample only.
    """
    pages = {page.get("url"): page for page in crawl_pages}
    rows = [dict(item) for item in inspections if isinstance(item, Mapping)]
    findings = {name: [] for name in INSPECTION_CHECKS}
    failed = 0
    for row in rows:
        status = row.get("index_status")
        if (
            row.get("state") != "complete"
            or not isinstance(row.get("url"), str)
            or not isinstance(status, Mapping)
            or status.get("verdict") not in {"PASS", "FAIL", "NEUTRAL"}
        ):
            failed += 1
            continue
        url = row["url"]
        amp = row.get("amp_result") or {}
        rich = row.get("rich_results_result") or {}
        verdict = status.get("verdict")
        amp_verdict = amp.get("verdict")
        rich_verdict = rich.get("verdict")

        def add(check: str, details: dict[str, Any]) -> None:
            findings[check].append(
                {
                    "url": url,
                    "details": {**details, "inspected_at": row.get("inspected_at")},
                }
            )

        if verdict == "PASS" and (amp_verdict == "FAIL" or rich_verdict == "FAIL"):
            add(
                "GSC_INDEXED_WITH_ISSUES",
                {"amp_verdict": amp_verdict, "rich_results_verdict": rich_verdict},
            )
        if amp_verdict == "FAIL":
            add("GSC_AMP_INVALID", {"amp_verdict": amp_verdict})
        if rich_verdict == "FAIL":
            add("GSC_RICH_RESULTS_INVALID", {"rich_results_verdict": rich_verdict})
        crawl_indexable = _indexable(pages.get(url, {}))
        if crawl_indexable is True and verdict in {"FAIL", "NEUTRAL"}:
            add(
                "GSC_INDEXABLE_NOT_INDEXED",
                {"verdict": verdict, "coverage_state": status.get("coverageState")},
            )
        user_canonical = status.get("userCanonical")
        google_canonical = status.get("googleCanonical")
        if (
            isinstance(user_canonical, str)
            and user_canonical
            and isinstance(google_canonical, str)
            and google_canonical
            and normalize_join_key(user_canonical) != normalize_join_key(google_canonical)
        ):
            add(
                "GSC_USER_CANONICAL_NOT_SELECTED",
                {
                    "user_canonical": user_canonical,
                    "google_canonical": google_canonical,
                    "safe_user_canonical": _public_url(user_canonical),
                    "safe_google_canonical": _public_url(google_canonical),
                },
            )
        if verdict in {"FAIL", "NEUTRAL"}:
            add(
                "GSC_URL_NOT_ON_GOOGLE",
                {"verdict": verdict, "coverage_state": status.get("coverageState")},
            )
    return {
        "scope": "bounded Search Console URL Inspection sample; not a site-wide census",
        "requested": requested,
        "inspected": len(rows) - failed,
        "failed": failed,
        "deferred": deferred,
        "sampled": bool(rows),
        "reason": "no retained URL Inspection sample" if not rows else None,
        "findings": findings,
        "sf_checks": dict(INSPECTION_CHECKS),
    }
