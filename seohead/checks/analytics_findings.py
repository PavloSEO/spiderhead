"""Web-analytics findings read from an offline join (#1024).

:mod:`seohead.checks.external_join` says which crawled pages have an analytics
row and which rows name no crawled page. This module reads that result as four
crawl findings. It is pure: it takes the crawl pages and the join result and
returns plain data. It never calls a provider.

Each finding states its own precondition. A value that cannot be read is
counted as ``unreadable`` and never becomes a zero. A finding that asserts a
negative over the whole site is withheld on a partial crawl, the same rule
``seohead.sf.core.aggregate`` applies to orphan findings.
"""

from __future__ import annotations

import math
from collections.abc import Iterable, Mapping
from typing import Any

from seohead.checks.external_join import orphan_urls

SCHEMA_VERSION = "analytics_findings.v1"
BOUNCE_THRESHOLD = 70.0

ORPHAN_URL = "GA_ORPHAN_URL"
NO_DATA = "NO_GA_DATA"
NON_INDEXABLE_WITH_DATA = "NON_INDEXABLE_WITH_GA_DATA"
BOUNCE_ABOVE = "BOUNCE_RATE_ABOVE_70"


def parse_percent(value: Any) -> float | None:
    """Read a bounce rate as a percentage in 0..100, or ``None`` when unreadable.

    ``"65"``, ``"65%"`` and ``"65,5 %"`` are percentages. A decimal number not
    above 1 (``"0.65"``) is a fraction. A bare integer is always a percentage,
    so ``"1"`` means 1 %, not 100 %.
    """
    if value is None:
        return None
    text = "".join(str(value).split()).replace(",", ".")
    is_percent = text.endswith("%")
    if is_percent:
        text = text[:-1]
    try:
        number = float(text)
    except ValueError:
        return None
    if not math.isfinite(number) or number < 0:
        return None
    if is_percent or "." not in text or number > 1:
        return number if number <= 100 else None
    return number * 100


def parse_count(value: Any) -> float | None:
    """Read a non-negative visit count, or ``None`` when unreadable."""
    if value is None:
        return None
    text = "".join(str(value).split())
    try:
        number = float(text)
    except ValueError:
        return None
    return number if math.isfinite(number) and number >= 0 else None


def _is_indexable(page: Mapping[str, Any]) -> bool | None:
    """``True``/``False`` from the crawl's Indexability column, ``None`` if absent."""
    value = page.get("Indexability", page.get("indexability"))
    if value is None:
        return None
    text = str(value).strip().lower()
    if text == "indexable":
        return True
    if text == "non-indexable":
        return False
    return None


def _finding(
    state: str, reason: str, urls: Iterable[str] = (), unreadable: int = 0
) -> dict[str, Any]:
    return {
        "state": state,
        "reason": reason,
        "urls": sorted(set(urls)),
        "unreadable": unreadable,
    }


def analytics_findings(
    pages: Iterable[Mapping[str, Any]],
    join_result: Mapping[str, Any],
    *,
    partial: bool,
    evidence_complete: bool = True,
    evidence_reason: str | None = None,
    empty_is_complete: bool = False,
    visits_column: str | None = None,
    bounce_column: str | None = None,
    bounce_reason: str | None = None,
) -> dict[str, Any]:
    """Return the four analytics findings, each with a state and a reason.

    ``state`` is ``reported`` (the check ran; ``urls`` holds its hits),
    ``withheld`` (the check ran but its claim is unsound for this crawl), or
    ``skipped`` (the check had no input or no readable value; ``reason`` says why).
    A provider history explicitly declaring complete empty days can set
    ``empty_is_complete``; an empty offline file remains a skip by default.
    """
    pages_by_url = {page.get("url"): page for page in pages}
    joined = join_result.get("joined") or []
    rows_total = (join_result.get("summary") or {}).get("rows", 0)

    if partial:
        orphans = _finding(
            "withheld",
            "crawl is partial; an external-only URL cannot be proven an orphan",
        )
    elif not evidence_complete:
        orphans = _finding(
            "withheld",
            evidence_reason or "analytics history coverage is incomplete",
        )
    else:
        orphans = _finding(
            "reported",
            "crawl completed; external-only same-origin URLs",
            orphan_urls(join_result),
        )

    if partial:
        no_data = _finding(
            "withheld", "crawl is partial; absence of analytics data cannot be established"
        )
    elif not evidence_complete:
        no_data = _finding(
            "withheld", evidence_reason or "analytics history coverage is incomplete"
        )
    elif rows_total == 0 and not empty_is_complete:
        no_data = _finding(
            "skipped", "analytics input has no rows; a no-data claim would not be meaningful"
        )
    else:
        verdicts = [
            (url, _is_indexable(pages_by_url.get(url, {})))
            for url in join_result.get("crawl_only") or []
        ]
        no_data = _finding(
            "reported",
            "indexable crawled pages with no analytics row",
            (url for url, indexable in verdicts if indexable is True),
            unreadable=sum(1 for _, indexable in verdicts if indexable is None),
        )

    if visits_column is None:
        non_indexable = _finding("skipped", "visits_column not given")
    else:
        hits: list[str] = []
        unreadable = 0
        for entry in joined:
            indexable = _is_indexable(entry["page"])
            visits = parse_count(entry["external"].get(visits_column))
            if visits is None or indexable is None:
                unreadable += 1
            elif indexable is False and visits > 0:
                hits.append(entry["url"])
        non_indexable = _finding(
            "reported",
            "non-indexable crawled pages with visits above zero",
            hits,
            unreadable=unreadable,
        )

    if bounce_column is None:
        bounce = _finding(
            "skipped",
            bounce_reason
            or "bounce_column not given; per-URL bounce needs a per-URL export (#990, #984)",
        )
    else:
        hits = []
        unreadable = 0
        for entry in joined:
            rate = parse_percent(entry["external"].get(bounce_column))
            if rate is None:
                unreadable += 1
            elif rate > BOUNCE_THRESHOLD:
                hits.append(entry["url"])
        bounce = _finding(
            "reported",
            f"matched pages with bounce rate above {BOUNCE_THRESHOLD:g}%",
            hits,
            unreadable=unreadable,
        )

    return {
        "schema_version": SCHEMA_VERSION,
        "findings": {
            ORPHAN_URL: orphans,
            NO_DATA: no_data,
            NON_INDEXABLE_WITH_DATA: non_indexable,
            BOUNCE_ABOVE: bounce,
        },
    }
