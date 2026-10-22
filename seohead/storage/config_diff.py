"""Read-only comparison of two saved crawl configurations (issue #1529).

Recurring audits change configuration over time, and a results difference
between two scans cannot be attributed without knowing whether the settings
moved too. This module reads each scan's recorded effective configuration --
nothing is re-crawled, nothing is written -- and reports a field-by-field diff
with both values shown, over the same dotted setting paths
``crawl-describe-settings`` and the run manifest use.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from seohead.crawl.settings import _flatten
from seohead.storage import ScanError, open_scan
from seohead.storage.native_scan import read_effective_config


def diff_configs(before: dict[str, Any], after: dict[str, Any]) -> dict[str, Any]:
    """Field-by-field diff of two effective crawl configurations.

    Paths are the dotted setting paths of the flattened configuration; a
    free-form map (``http.headers``) is one leaf, as in the manifest. Values are
    reported exactly as recorded on each side.
    """
    flat_before = _flatten(before or {})
    flat_after = _flatten(after or {})
    changed = [
        {"path": path, "before": flat_before[path], "after": flat_after[path]}
        for path in sorted(set(flat_before) & set(flat_after))
        if flat_before[path] != flat_after[path]
    ]
    added = [
        {"path": path, "after": value}
        for path, value in sorted(flat_after.items())
        if path not in flat_before
    ]
    removed = [
        {"path": path, "before": value}
        for path, value in sorted(flat_before.items())
        if path not in flat_after
    ]
    return {
        "changed": changed,
        "added": added,
        "removed": removed,
        "identical": not (changed or added or removed),
        "settings_compared": len(set(flat_before) | set(flat_after)),
    }


def _recorded_config(scan_path: str | Path) -> dict[str, Any]:
    path = Path(scan_path)
    if not path.is_file():
        raise ScanError(f"scan file does not exist: {path}")
    with open_scan(path, require_audit=False) as con:
        return read_effective_config(con)


def scan_config_diff(before_path: str | Path, after_path: str | Path) -> dict[str, Any]:
    """Diff the recorded effective configurations of two saved scans, read-only."""
    before = _recorded_config(before_path)
    after = _recorded_config(after_path)
    result = diff_configs(before, after)
    result.update(
        {
            "before": str(Path(before_path)),
            "after": str(Path(after_path)),
        }
    )
    return result
