"""Parse a CSV of snippet edits (URL, title, description) into validated records.

First slice of #1010 child 7. Pure parsing: no network, no file I/O, no pixel
measurement. The CLI/MCP entry point and the export-as-task step are separate
slices and are not part of this module.

Rules: the header must name ``url`` and at least one of ``title`` / ``description``
(case-insensitive, ``;`` or ``,`` delimiter). An empty cell means "leave this
field unchanged". A row with no edited field, a non-http(s) URL, or a URL that
already appeared is reported as an error and skipped; the other rows still load.
"""

from __future__ import annotations

import csv
import io
import json
import math
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

MAX_CHARS = 5_000_000
EDITABLE = ("title", "description")
METRICS_SCHEMA = "snippet_metrics.v1"
TASKS_SCHEMA = "snippet_tasks.v1"


def parse_snippet_csv(text: str) -> dict[str, Any]:
    """Return ``{"edits": [...], "errors": [...]}``; raise ``ValueError`` on bad input."""
    if not isinstance(text, str):
        raise ValueError("snippet CSV must be text")
    if len(text) > MAX_CHARS:
        raise ValueError(f"snippet CSV is larger than {MAX_CHARS} characters")
    text = text.lstrip("﻿")
    first_line = text.split("\n", 1)[0]
    delimiter = ";" if first_line.count(";") > first_line.count(",") else ","
    reader = csv.reader(io.StringIO(text), delimiter=delimiter)
    try:
        header = [cell.strip().lower() for cell in next(reader)]
    except StopIteration:
        raise ValueError("snippet CSV is empty") from None
    if "url" not in header or not any(name in header for name in EDITABLE):
        raise ValueError("snippet CSV needs a 'url' column and 'title' or 'description'")

    edits: list[dict[str, Any]] = []
    errors: list[dict[str, Any]] = []
    seen: set[str] = set()
    for row in reader:
        line = reader.line_num
        if not any(cell.strip() for cell in row):
            continue
        cells = {name: (row[i].strip() if i < len(row) else "") for i, name in enumerate(header)}
        url = cells["url"]
        parts = urlsplit(url)
        if parts.scheme not in ("http", "https") or not parts.netloc:
            errors.append({"line": line, "message": "url must be an absolute http(s) URL"})
            continue
        if url in seen:
            errors.append({"line": line, "message": "duplicate url"})
            continue
        fields = {name: cells[name] for name in EDITABLE if cells.get(name)}
        if not fields:
            errors.append({"line": line, "message": "no title or description to change"})
            continue
        seen.add(url)
        edits.append({"line": line, "url": url, **fields})
    return {"edits": edits, "errors": errors}


def measure_text_pixels(text: str, *, font_path: str, font_size: int) -> int:
    """Measure text in a caller-supplied local font; this is not a search-engine preview.

    Font choice and size are explicit because Google and Yandex snippet typography and
    truncation policies are not interchangeable or a stable browser rendering contract.
    """
    if not isinstance(text, str):
        raise ValueError("snippet text must be a string")
    if not font_path or not isinstance(font_path, str):
        raise ValueError("font_path must name a local font file")
    if isinstance(font_size, bool) or not isinstance(font_size, int) or font_size < 1:
        raise ValueError("font_size must be a positive integer")
    from PIL import ImageFont

    try:
        font = ImageFont.truetype(font_path, font_size)
    except OSError as exc:
        raise ValueError("font_path could not be loaded") from exc
    return math.ceil(font.getlength(text))


def measure_snippet_edits(
    edits: list[dict[str, Any]], metrics: dict[str, dict[str, Any]]
) -> dict[str, Any]:
    """Add measured title/description widths for explicitly supplied font profiles.

    Each metric is ``{"font_path": ..., "font_size": ...}``. No profile names,
    thresholds, verdicts, or Google/Yandex equivalence are inferred here.
    """
    if not isinstance(metrics, dict) or not metrics:
        raise ValueError("metrics must contain at least one named font profile")
    measured: list[dict[str, Any]] = []
    for edit in edits:
        row = dict(edit)
        widths: dict[str, dict[str, int]] = {}
        for profile, spec in metrics.items():
            if not isinstance(profile, str) or not profile.strip() or not isinstance(spec, dict):
                raise ValueError("each metric profile needs a name and font settings")
            profile_widths: dict[str, int] = {}
            for field in EDITABLE:
                if field in row:
                    profile_widths[field] = measure_text_pixels(
                        row[field],
                        font_path=spec.get("font_path"),
                        font_size=spec.get("font_size"),
                    )
            widths[profile] = profile_widths
        row["pixel_widths"] = widths
        measured.append(row)
    return {"schema_version": METRICS_SCHEMA, "edits": measured}


def build_snippet_tasks(parsed: dict[str, Any]) -> dict[str, Any]:
    """Export imported edits as reviewable, unapplied task candidates."""
    edits = parsed.get("edits")
    errors = parsed.get("errors")
    if not isinstance(edits, list) or not isinstance(errors, list):
        raise ValueError("parsed snippet edits need edits and errors lists")
    tasks = [
        {
            "id": f"SNIPPET-{edit['line']}",
            "type": "snippet_edit_review",
            "state": "candidate",
            "target_url": edit["url"],
            "source_line": edit["line"],
            "edits": {field: edit[field] for field in EDITABLE if field in edit},
            "instruction": "Review the proposed title and description; apply only after approval.",
        }
        for edit in edits
    ]
    return {
        "schema_version": TASKS_SCHEMA,
        "summary": {"tasks": len(tasks), "rejected_rows": len(errors)},
        "tasks": tasks,
        "errors": errors,
    }


def write_snippet_tasks(tasks: dict[str, Any], out: str | Path) -> str:
    """Write a new task JSON file without replacing an existing file."""
    path = Path(out)
    if path.exists() or path.is_symlink():
        raise FileExistsError(f"snippet task output already exists: {path}")
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = json.dumps(tasks, ensure_ascii=False, indent=2) + "\n"
    with path.open("x", encoding="utf-8", newline="\n") as handle:
        handle.write(payload)
    return str(path)
