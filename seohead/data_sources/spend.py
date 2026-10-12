"""Track paid-provider usage: what was charged, for which operation, and when.

A local ledger is necessary even when a provider exposes account history. Informal estimates can
drift from actual usage, and a parsing failure must not hide a charge. Two rules follow:

1. **Record a charge as soon as the provider returns its cost**, before parsing the result. A
   parser failure must not turn a paid response into an untraceable expense.
2. **Record the task identifier as well.** Arsenkin results can be fetched again by ``task_id``
   without paying twice. The ledger is therefore also an index of already-paid tasks.

The format is JSONL, one line per call. Appending a line is atomic, so an interrupted process does
not corrupt prior entries. The default path is ``~/.config/seohead/spend.jsonl`` and can be
overridden with ``SEOHEAD_SPEND_LOG``.
"""

from __future__ import annotations

import json
import os
import time
from collections import defaultdict
from collections.abc import Iterator
from contextlib import contextmanager
from contextvars import ContextVar
from pathlib import Path
from typing import Any

_CONTEXT: ContextVar[dict | None] = ContextVar("seohead_spend_context", default=None)


@contextmanager
def context(**tags: Any) -> Iterator[None]:
    """Attach ``tags`` to the ``extra`` of every entry recorded inside the block.

    A pipeline such as ``seohead semantics`` uses this to attribute provider charges to its
    project and stage without each provider client knowing about the caller.
    """
    token = _CONTEXT.set({**(_CONTEXT.get() or {}), **tags})
    try:
        yield
    finally:
        _CONTEXT.reset(token)


def log_path() -> Path:
    override = os.environ.get("SEOHEAD_SPEND_LOG")
    if override:
        return Path(override).expanduser()
    return Path(os.path.expanduser("~/.config/seohead/spend.jsonl"))


def record(
    source: str,
    operation: str,
    *,
    cost: float | None = 0.0,
    unit: str = "limits",
    task_id: Any | None = None,
    items: int = 0,
    extra: dict | None = None,
) -> dict:
    """Record one charge and return the stored entry.

    ``cost`` is the amount charged in the provider's own ``unit``: Arsenkin uses limit credits,
    while Yandex Cloud uses requests. The ledger deliberately does not convert usage to money.
    Prices change; the log must remain an accurate record of measured provider units.
    """
    entry = {
        "at": time.strftime("%Y-%m-%dT%H:%M:%S"),
        "source": source,
        "operation": operation,
        "cost": None if cost is None else cost,
        "unit": unit,
        "items": items,
    }
    if task_id is not None:
        entry["task_id"] = task_id
    tags = _CONTEXT.get()
    if tags:
        extra = {**tags, **(extra or {})}
    if extra:
        entry["extra"] = extra

    path = log_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(entry, ensure_ascii=False) + "\n")
    return entry


def read_all() -> list[dict]:
    path = log_path()
    if not path.exists():
        return []
    rows = []
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            row = json.loads(line)
            # Earlier writers encoded an unmeasured cost as zero plus an uncertainty flag.
            # Normalize that shape only while reading; the append-only journal is untouched.
            if isinstance(row, dict) and _is_uncertain(row) and row.get("cost") == 0:
                row["cost"] = None
            rows.append(row)
        except ValueError:
            continue  # Preserve usable history even when one JSONL line is malformed.
    return rows


def _is_uncertain(row: dict) -> bool:
    """A receipt whose cost or charge status is not actually known.

    A response can be received and still be unusable: a malformed body, for instance, proves a
    request reached the provider without proving what it cost. Such a row must never be folded
    into ``by_source``/``by_operation``/``by_day`` alongside confirmed zero-cost calls — that
    would silently relabel "unmeasured" as "measured and free". Callers set either flag on
    ``extra`` for this: ``cost_unknown`` or ``charge_uncertain`` (used
    by Yandex Cloud) both mean the same thing.
    """
    extra = row.get("extra") or {}
    return bool(extra.get("cost_unknown") or extra.get("charge_uncertain"))


def report(since: str | None = None) -> dict:
    """Summarize usage by provider, operation, and day.

    ``since`` is an inclusive ``YYYY-MM-DD`` date. Rows with an uncertain cost or charge status
    (see :func:`_is_uncertain`) are kept out of the cost totals and listed separately under
    ``uncertain``, so a receipt that only proves a request reached the provider never counts as a
    measured zero-cost call.
    """
    rows = [r for r in read_all() if not since or r.get("at", "")[:10] >= since]
    uncertain = [r for r in rows if _is_uncertain(r)]
    measured = [r for r in rows if not _is_uncertain(r)]

    by_source: dict[str, dict[str, float]] = defaultdict(lambda: defaultdict(float))
    by_operation: dict[str, dict[str, float]] = defaultdict(lambda: defaultdict(float))
    by_day: dict[str, dict[str, float]] = defaultdict(lambda: defaultdict(float))

    for row in measured:
        unit = row.get("unit", "limits")
        cost = float(row.get("cost") or 0)
        by_source[row.get("source", "?")][unit] += cost
        by_operation[f"{row.get('source', '?')}.{row.get('operation', '?')}"][unit] += cost
        by_day[row.get("at", "")[:10]][unit] += cost

    return {
        "ok": True,
        "calls": len(rows),
        "since": since,
        "by_source": {k: dict(v) for k, v in by_source.items()},
        "by_operation": {k: dict(v) for k, v in by_operation.items()},
        "by_day": {k: dict(v) for k, v in sorted(by_day.items())},
        "uncertain": uncertain,
        "log": str(log_path()),
    }


def paid_task_ids(source: str) -> list[Any]:
    """Return paid task IDs so callers can fetch results without paying again."""
    return [
        r["task_id"]
        for r in read_all()
        if r.get("source") == source and r.get("task_id") is not None
    ]


CSV_COLUMNS = ("at", "source", "operation", "cost", "unit", "items", "task_id", "uncertain")


def iter_rows(since: str | None = None) -> Iterator[dict]:
    """Yield one flat row per journal entry, in journal order, for CSV export.

    Unlike :func:`report`, rows are not aggregated, and uncertain receipts are kept with
    ``uncertain`` set, so a spreadsheet shows them rather than hiding them.
    """
    for row in read_all():
        if since and row.get("at", "")[:10] < since:
            continue
        yield {
            "at": row.get("at", ""),
            "source": row.get("source", ""),
            "operation": row.get("operation", ""),
            "cost": row.get("cost"),
            "unit": row.get("unit", "limits"),
            "items": row.get("items", 0),
            "task_id": "" if row.get("task_id") is None else row["task_id"],
            "uncertain": _is_uncertain(row),
        }


def write_csv(path: str | Path, since: str | None = None) -> int:
    """Write :func:`iter_rows` to ``path`` as CSV and return the number of rows written.

    Uses the ``;`` delimiter and UTF-8 BOM, as the other CSV exports in ``seohead.reports`` do.
    """
    import csv

    from seohead.reports import neutralize_formula

    count = 0
    with Path(path).expanduser().open("w", encoding="utf-8-sig", newline="") as handle:
        writer = csv.writer(handle, delimiter=";")
        writer.writerow(CSV_COLUMNS)
        for row in iter_rows(since=since):
            writer.writerow([neutralize_formula(row[c]) for c in CSV_COLUMNS])
            count += 1
    return count
