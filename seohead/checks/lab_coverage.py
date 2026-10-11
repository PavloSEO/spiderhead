"""Unused CSS and JavaScript from rendered-page coverage payloads.

Pure analysis, no browser and no network. It takes the lists returned by
Playwright's ``page.coverage.stop_js_coverage()`` and ``stop_css_coverage()``
and reports how much of each resource never executed (JS) or never applied
(CSS). Starting and stopping coverage on a rendered page is a separate step.

Units are character offsets into the source text as the browser reports them.
For ASCII-heavy minified sources this approximates bytes; the fields are named
``*_chars`` so the unit is never mistaken for bytes.

Public API:
    analyze_js_coverage(entries, *, ratio_threshold, min_chars) -> dict
    analyze_css_coverage(entries, *, ratio_threshold, min_chars) -> dict
"""

from __future__ import annotations

from typing import Any

# Thresholds for reporting a resource as mostly unused. They are this module's
# choices, not an external standard: a resource must be at least half unused
# and carry a meaningful amount of dead text before it is flagged.
UNUSED_RATIO_THRESHOLD = 0.5
UNUSED_MIN_CHARS = 20_000

_USED = 1
_UNUSED = 0


def _paint(text_len: int, ranges: list[tuple[int, int, bool]]) -> bytearray:
    """Mark every character as used or unused; smaller (inner) ranges win.

    Ranges are painted largest first, so a nested range overwrites the range
    around it, matching how precise coverage nests function blocks. Characters
    no range mentions stay unused.
    """
    flags = bytearray([_UNUSED]) * text_len
    for start, end, used in sorted(ranges, key=lambda r: r[1] - r[0], reverse=True):
        start = max(0, start)
        end = min(text_len, end)
        if end > start:
            flags[start:end] = bytes([_USED if used else _UNUSED]) * (end - start)
    return flags


def _measure(
    entries: list[dict[str, Any]],
    ranges_for: Any,
    text_for: Any,
    *,
    ratio_threshold: float,
    min_chars: int,
) -> dict[str, Any]:
    resources: list[dict[str, Any]] = []
    skipped: list[dict[str, str]] = []
    findings: list[str] = []
    for entry in entries:
        url = str(entry.get("url") or "(inline)")
        text = text_for(entry)
        if not isinstance(text, str):
            skipped.append({"url": url, "reason": "coverage payload has no source text"})
            continue
        # CDP offsets count UTF-16 code units, including both halves of an astral character.
        total = len(text.encode("utf-16-le", "surrogatepass")) // 2
        if total == 0:
            skipped.append({"url": url, "reason": "resource has no source text"})
            continue
        flags = _paint(total, ranges_for(entry))
        unused = flags.count(_UNUSED)
        ratio = round(unused / total, 4)
        resources.append(
            {"url": url, "total_chars": total, "unused_chars": unused, "unused_ratio": ratio}
        )
        if ratio >= ratio_threshold and unused >= min_chars:
            findings.append(
                f"{url} is {ratio:.0%} unused ({unused} of {total} characters never used)"
            )
    total_chars = sum(r["total_chars"] for r in resources)
    unused_chars = sum(r["unused_chars"] for r in resources)
    return {
        "resources": resources,
        "total_chars": total_chars,
        "unused_chars": unused_chars,
        "findings": findings,
        "skipped": skipped,
    }


def _js_ranges(entry: dict[str, Any]) -> list[tuple[int, int, bool]]:
    ranges: list[tuple[int, int, bool]] = []
    for function in entry.get("functions") or []:
        for item in function.get("ranges") or []:
            ranges.append(
                (
                    int(item["startOffset"]),
                    int(item["endOffset"]),
                    int(item.get("count", 0)) > 0,
                )
            )
    return ranges


def _css_ranges(entry: dict[str, Any]) -> list[tuple[int, int, bool]]:
    # CSS coverage lists only the ranges that applied, so every listed range is used.
    return [(int(item["start"]), int(item["end"]), True) for item in entry.get("ranges") or []]


def analyze_js_coverage(
    entries: list[dict[str, Any]],
    *,
    ratio_threshold: float = UNUSED_RATIO_THRESHOLD,
    min_chars: int = UNUSED_MIN_CHARS,
) -> dict[str, Any]:
    """Measure unused JavaScript from ``stop_js_coverage()`` entries.

    A character is used when the innermost function range covering it ran at
    least once. Entries without a ``source`` string are skipped with a reason.
    """
    return _measure(
        entries,
        _js_ranges,
        lambda entry: entry.get("source"),
        ratio_threshold=ratio_threshold,
        min_chars=min_chars,
    )


def analyze_css_coverage(
    entries: list[dict[str, Any]],
    *,
    ratio_threshold: float = UNUSED_RATIO_THRESHOLD,
    min_chars: int = UNUSED_MIN_CHARS,
) -> dict[str, Any]:
    """Measure unused CSS from ``stop_css_coverage()`` entries.

    Entries without a ``text`` string are skipped with a reason.
    """
    return _measure(
        entries,
        _css_ranges,
        lambda entry: entry.get("text"),
        ratio_threshold=ratio_threshold,
        min_chars=min_chars,
    )
