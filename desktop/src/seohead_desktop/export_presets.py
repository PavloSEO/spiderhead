"""Export presets over the core ``scan-url-query`` command: built-ins plus optional ``export_presets.json`` in the project.

A preset is ``{id, label, columns, preset?, filters?}``: ``preset`` is a core filter set (``scan-url-query --preset``),
``filters`` an extra list of ``{column, op, value}``. A project file overrides a built-in with the same id or adds new ones.
"""

from __future__ import annotations

import json
import re
from pathlib import Path

CONFIG_NAME = "export_presets.json"
CHECK = "issues_check"  # needs a check id from the user; the core takes it as --issue-check
CUSTOM = "custom"
_NAME = re.compile(r"[a-z][a-z0-9_]{0,63}\Z")

BUILTIN = (
    {
        "id": "gsc_urls",
        "label": "Search Console URL list (200, indexable)",
        "columns": ["url"],
        "filters": [
            {"column": "status_code", "op": "eq", "value": 200},
            {"column": "indexable", "op": "eq", "value": True},
        ],
    },
    {
        "id": "redirect_map",
        "label": "Redirect map (3xx)",
        "columns": ["url", "status_code", "redirect_url", "final_url"],
        "preset": "status_3xx",
    },
    {
        "id": "content_inventory",
        "label": "Content inventory",
        "columns": ["url", "status_code", "title", "meta_description", "h1", "word_count", "crawl_depth"],
    },
    {
        "id": "indexability",
        "label": "Indexability list",
        "columns": ["url", "status_code", "indexable", "meta_robots", "x_robots", "canonical"],
    },
    {
        "id": "images_missing_alt",
        "label": "Pages with images missing alt",
        "columns": ["url", "images_total", "images_missing_alt_attr"],
        "preset": "images_missing_alt",
    },
    {"id": CHECK, "label": "Issues for one check (enter check ID)", "columns": ["url", "status_code", "title"]},
    {
        "id": CUSTOM,
        "label": "Custom columns (all pages)",
        "columns": ["url", "status_code", "title", "h1", "word_count", "indexable"],
    },
)


def clean_columns(value):
    """Column names from a comma-separated string or list; ``None`` when any name is not a plain identifier."""
    items = value.split(",") if isinstance(value, str) else value
    if not isinstance(items, list):
        return None
    names = [str(item).strip() for item in items if str(item).strip()]
    return names if names and all(_NAME.match(name) for name in names) else None


def _valid(item):
    if not isinstance(item, dict) or not isinstance(item.get("id"), str) or not item["id"]:
        return None
    columns = clean_columns(item.get("columns"))
    filters = item.get("filters", [])
    if columns is None or not isinstance(filters, list) or not all(isinstance(f, dict) for f in filters):
        return None
    out = {"id": item["id"], "label": str(item.get("label") or item["id"]), "columns": columns, "filters": filters}
    if isinstance(item.get("preset"), str):
        out["preset"] = item["preset"]
    return out


def load_presets(project_dir):
    """Built-ins overridden/extended by the project file; unreadable or invalid entries are skipped."""
    presets = {p["id"]: dict(p) for p in BUILTIN}
    try:
        data = json.loads((Path(project_dir) / CONFIG_NAME).read_text(encoding="utf-8"))
    except (OSError, ValueError, TypeError):
        data = None
    for item in (data.get("presets") if isinstance(data, dict) else None) or []:
        valid = _valid(item)
        if valid:
            presets[valid["id"]] = valid
    return list(presets.values())


def query_arguments(scan_path, preset, out, fmt, columns=None, check=None):
    """``scan-url-query`` argv exporting every matching row to ``out`` (csv or xlsx)."""
    args = [
        "scan-url-query", "--scan", scan_path, "--export", str(out),
        "--export-format", fmt, "--columns", ",".join(columns or preset["columns"]),
    ]  # fmt: skip
    if preset.get("preset"):
        args += ["--preset", preset["preset"]]
    if preset.get("filters"):
        args += ["--filters", json.dumps(preset["filters"])]
    if check:
        args += ["--issue-check", check]
    return args


def list_files(project_dir):
    """Files agents or the user put into the project ``exports``/``reports`` folders, newest first."""
    found = []
    for name in ("exports", "reports"):
        folder = Path(project_dir) / name
        try:
            found += [p for p in folder.rglob("*") if p.is_file() and not p.name.startswith(".")]
        except OSError:
            continue
    found.sort(key=lambda p: p.stat().st_mtime, reverse=True)
    return found[:200]
