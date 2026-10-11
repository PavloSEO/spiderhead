"""Render the row-level coverage-gap reconciliation from the canonical map."""

from __future__ import annotations

import re
from collections import Counter
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MAP = ROOT / "docs" / "COVERAGE_GAPS.md"
OUT = ROOT / "docs" / "COVERAGE_GAPS_RECONCILIATION.md"
ROW = re.compile(
    r"^\| (?P<row_id>\d+\.\d+) \| (?P<name>.*?) \| (?P<checks>.*?) \| "
    r"(?P<value>.*?) \| (?P<mode>.*?) \| (?P<home>.*?) \|$"
)
BACKTICKED = re.compile(r"`([^`]+)`")

# Keep the excluded accessibility work outside the shipped static markup subset.
OUT_OF_SCOPE_ROWS = frozenset({"3.1", "3.2", "3.3", "3.6", "3.8", "3.9"})
VALUE_ORDER = {"**high**": 0, "high": 0, "medium": 1, "low": 2}
STATE_ORDER = {"missing": 0, "partial": 1}


@dataclass(frozen=True)
class Row:
    """One canonical six-column gap-map row."""

    row_id: str
    name: str
    checks: str
    value: str
    mode: str
    home: str


def rows() -> list[Row]:
    """Read every numbered map row, retaining the actual Mode column."""
    found = []
    for line in MAP.read_text(encoding="utf-8").splitlines():
        match = ROW.match(line)
        if match:
            found.append(Row(**match.groupdict()))
    return found


def status(row: Row) -> str:
    """Classify scope from the reviewed map state, never from its Value column."""
    lowered = row.mode.lower()
    if (
        row.row_id in OUT_OF_SCOPE_ROWS
        or "out of scope" in lowered
        or "consciously excluded" in lowered
    ):
        return "out_of_scope"
    if "partially done" in lowered or "mostly done" in lowered or "**partial**" in lowered:
        return "partial"
    if "**done**" in lowered:
        return "covered_registry_or_tool"
    return "missing"


def evidence_refs(row: Row) -> tuple[str, ...]:
    """Return the Mode cell's shipped check/tool references for validation."""
    return tuple(BACKTICKED.findall(row.mode))


def _row_key(row: Row) -> tuple[int, int]:
    major, minor = row.row_id.split(".", 1)
    return int(major), int(minor)


def _mode_rank(row: Row) -> int:
    lowered = row.mode.lower()
    if re.search(r"(?:^|\W)b\s*\(", lowered):
        return 0
    if "b+" in lowered:
        return 1
    if "a/live" in lowered or "a (" in lowered or "live `" in lowered:
        return 2
    return 3


def priority_rows(limit: int = 10) -> list[Row]:
    """Rank unresolved rows by the map's stated value, feasibility, then row id."""
    candidates = [row for row in rows() if status(row) in STATE_ORDER]
    return sorted(
        candidates,
        key=lambda row: (
            VALUE_ORDER[row.value.lower()],
            _mode_rank(row),
            STATE_ORDER[status(row)],
            _row_key(row),
        ),
    )[:limit]


def render() -> str:
    entries = rows()
    counts = Counter(status(row) for row in entries)
    lines = [
        "# Coverage gap reconciliation",
        "",
        "Generated from `docs/COVERAGE_GAPS.md` by `scripts/generate_coverage_gap_reconciliation.py`.",
        "A row is covered only when its actual Mode cell names a shipped check/tool; partial and out-of-scope are not readiness claims.",
        "",
        f"**{len(entries)} rows:** "
        + ", ".join(f"{key}={counts[key]}" for key in sorted(counts))
        + ".",
        "",
        "| Row | Name | Reconciled state |",
        "|---|---|---|",
    ]
    lines += [f"| {row.row_id} | {row.name} | {status(row)} |" for row in entries]
    lines += [
        "",
        "## Remaining priority order",
        "",
        "Rows below are generated from unresolved map rows: stated value first, then the declared B/B+/A feasibility, then row number. The order is a planning aid, not a product-readiness claim.",
        "",
    ]
    lines += [
        f"{index}. {row.name} ({row.row_id}) — {status(row)}; {row.mode}"
        for index, row in enumerate(priority_rows(), start=1)
    ]
    lines.append("")
    return "\n".join(lines)


if __name__ == "__main__":
    OUT.write_text(render(), encoding="utf-8")
