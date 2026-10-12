#!/usr/bin/env python3
"""Read-only final latency acceptance for URL pages, facets and issue counters.

Runs fixed page reads of 100 rows, all facets and whole-scan issue counters against an existing
scan of at least one million URLs. Page reads and issue counters have a 200 ms target; facets
have a 300 ms target. Facets are reported per group: exact counts include measured zeroes, while
unsupported groups remain explicitly unavailable. The scan is read-only and hashed before/after.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import platform
import sqlite3
import statistics
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from seohead import __version__  # noqa: E402
from seohead.storage import url_query  # noqa: E402

PAGE_ROWS = 100
BUDGET_MS = 200.0
FACET_BUDGET_MS = 300.0
MIN_URLS = 1_000_000
COLUMNS = ["url", "status_code", "title", "word_count"]
REQUIRED_EXACT_FACETS = {
    "all",
    "status_2xx",
    "status_3xx",
    "status_4xx",
    "status_5xx",
    "title_missing",
}

# Each case: name, filters, sort, offset. Sort by url is the indexed path; status filter
# plus url sort covers a filtered page; the deep offset shows the cost of skipping rows.
CASES: list[tuple[str, list[dict[str, object]] | None, str | None, int]] = [
    ("first_page_sort_url", None, "url", 0),
    ("first_page_unsorted", None, None, 0),
    ("deep_page_sort_url", None, "url", 500_000),
    (
        "filtered_status_404_sort_url",
        [{"column": "status_code", "op": "eq", "value": 404}],
        "url",
        0,
    ),
]


def _digest(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _total_urls(path: Path) -> int:
    con = sqlite3.connect(f"file:{path.absolute()}?mode=ro", uri=True)
    try:
        return int(con.execute("SELECT COUNT(*) FROM urls").fetchone()[0])
    finally:
        con.close()


def _measure(
    path: str,
    filters,
    sort,
    offset: int,
    repeat: int,
    *,
    facets: str | None = None,
    include_check_counts: bool = False,
    budget_ms: float = BUDGET_MS,
) -> dict[str, object]:
    times_ms: list[float] = []
    last: dict[str, object] = {}
    for _ in range(repeat):
        started = time.perf_counter()
        last = url_query.scan_url_query(
            path,
            filters=filters,
            sort=sort,
            columns=COLUMNS,
            offset=offset,
            limit=PAGE_ROWS,
            facets=facets,
            include_check_counts=include_check_counts,
        )
        times_ms.append((time.perf_counter() - started) * 1000)
    median = statistics.median(times_ms)
    facet_states = last.get("facet_states") if facets is not None else None
    facet_values = last.get("facets") if facets is not None else None
    facet_states_valid = None
    if facets is not None:
        facet_states_valid = (
            isinstance(facet_states, dict)
            and isinstance(facet_values, dict)
            and set(facet_states) == set(facet_values)
            and set(facet_states) >= REQUIRED_EXACT_FACETS
            and all(
                facet_states[name] == "exact" and type(facet_values[name]) is int
                for name in REQUIRED_EXACT_FACETS
            )
            and all(
                (state == "exact" and type(facet_values[name]) is int)
                or (state == "unavailable" and facet_values[name] is None)
                for name, state in facet_states.items()
            )
        )
    return {
        "ok": bool(last.get("ok")),
        "reason_code": last.get("reason_code"),
        "rows": len(last.get("rows", []) or []),
        "median_ms": round(median, 2),
        "min_ms": round(min(times_ms), 2),
        "max_ms": round(max(times_ms), 2),
        "within_budget": bool(last.get("ok")) and median <= budget_ms,
        "budget_ms": budget_ms,
        **(
            {"facet_states": facet_states, "facet_states_valid": facet_states_valid}
            if facets is not None
            else {}
        ),
        "result_state": (last.get("issue_summary") or {}).get(
            "state", last.get("facets_state", "exact")
        ),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--scan", type=Path, required=True, help="saved scan file (read-only)")
    parser.add_argument("--repeat", type=int, default=5)
    parser.add_argument("--min-urls", type=int, default=MIN_URLS)
    args = parser.parse_args()
    if args.repeat < 1:
        parser.error("--repeat must be at least 1")
    if args.min_urls < 1:
        parser.error("--min-urls must be at least 1")
    if not args.scan.is_file():
        parser.error("--scan must name an existing scan file")

    before = _digest(args.scan)
    started = time.perf_counter()
    cases = {
        name: _measure(str(args.scan), filters, sort, offset, args.repeat)
        for name, filters, sort, offset in CASES
    }
    cases["all_facets"] = _measure(
        str(args.scan), None, "url", 0, args.repeat, facets="all", budget_ms=FACET_BUDGET_MS
    )
    cases["whole_scan_issue_counters"] = _measure(
        str(args.scan), None, "url", 0, args.repeat, include_check_counts=True
    )
    after = _digest(args.scan)
    unchanged = before == after
    report = {
        "format": "seohead.profile-url-query.v1",
        "seohead_version": __version__,
        "python": platform.python_version(),
        "sqlite": sqlite3.sqlite_version,
        "urls": _total_urls(args.scan),
        "minimum_urls": args.min_urls,
        "page_rows": PAGE_ROWS,
        "budget_ms": BUDGET_MS,
        "facet_budget_ms": FACET_BUDGET_MS,
        "required_python": "3.15.0",
        "python_matches": platform.python_version() == "3.15.0",
        "repeat": args.repeat,
        "scan_unchanged": unchanged,
        "cases": cases,
        "wall_seconds": round(time.perf_counter() - started, 3),
    }
    print(json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True))
    ok = (
        unchanged
        and report["urls"] >= args.min_urls
        and report["python_matches"]
        and all(case["within_budget"] for case in cases.values())
        and cases["all_facets"]["facet_states_valid"] is True
        and cases["whole_scan_issue_counters"]["result_state"] == "exact"
    )
    return 0 if ok else 3


if __name__ == "__main__":
    raise SystemExit(main())
