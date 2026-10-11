"""Directory tree: page counts per URL path segment, bounded for large sites (#1004, S2).

A page is counted once at every directory above it, the root included. A path that
does not end in ``/`` names a file, so its last segment is not a directory. Query
strings and fragments are ignored; the caller passes URLs of one site.

Bounds are applied when the tree is read out, not while it is built, so the build
holds one node per distinct directory. ``max_depth`` cuts the walk, ``max_children``
keeps the busiest children per node and folds the rest into ``other_pages``.
"""

from __future__ import annotations

import json
import sqlite3
import time
from collections.abc import Iterable
from dataclasses import dataclass, field
from pathlib import Path
from urllib.parse import urlsplit

from . import open_scan_mode


@dataclass
class DirNode:
    name: str
    pages: int = 0
    children: dict[str, DirNode] = field(default_factory=dict)

    def to_dict(self, max_depth: int, max_children: int) -> dict:
        """Return this node as plain data, cut at ``max_depth`` and ``max_children``."""
        ranked = sorted(self.children.values(), key=lambda n: (-n.pages, n.name))
        if max_depth <= 0:
            kept, folded = [], ranked
        else:
            kept, folded = ranked[:max_children], ranked[max_children:]
        out = {
            "name": self.name,
            "pages": self.pages,
            "children": [c.to_dict(max_depth - 1, max_children) for c in kept],
        }
        if folded:
            out["other_children"] = len(folded)
            out["other_pages"] = sum(c.pages for c in folded)
        return out


def _directory_segments(url: str) -> list[str]:
    path = urlsplit(url).path
    parts = [p for p in path.split("/") if p]
    if parts and not path.endswith("/"):
        parts = parts[:-1]  # the last segment names a file, not a directory
    return parts


def build_directory_tree(
    urls: Iterable[str], *, max_depth: int = 3, max_children: int = 50
) -> dict:
    """Return the directory tree of ``urls`` as plain data, bounded by the given limits.

    ``ponytail: plain dict tree, built in memory | ceiling: one node per distinct
    directory of the whole site | upgrade: count per directory in SQL at query time
    when a 1M-URL scan exceeds the memory budget (S6 of #1004)``
    """
    if max_depth < 0 or max_children < 0:
        raise ValueError("max_depth and max_children must be zero or more")
    root = DirNode("/")
    for url in urls:
        node = root
        node.pages += 1
        for segment in _directory_segments(url):
            child = node.children.get(segment)
            if child is None:
                child = node.children[segment] = DirNode(segment)
            child.pages += 1
            node = child
    return root.to_dict(max_depth, max_children)


def scan_directory_tree(
    scan_path: str | Path,
    *,
    max_depth: int = 3,
    max_children: int = 50,
    max_nodes: int = 10_000,
    max_bytes: int = 1_048_576,
    timeout_seconds: float = 15.0,
) -> dict:
    """Build a bounded directory summary from stored URLs with SQLite-backed counts.

    Unlike ``build_directory_tree``, this read does not retain one Python node per site path.
    Distinct path counts live in a temporary table and only the requested output tree is read
    into memory. It never fetches URLs or mutates the scan.
    """
    if (
        type(max_depth) is not int
        or not 0 <= max_depth <= 20
        or type(max_children) is not int
        or not 0 <= max_children <= 500
        or type(max_nodes) is not int
        or not 1 <= max_nodes <= 10_000
        or type(max_bytes) is not int
        or not 4096 <= max_bytes <= 8 * 1024 * 1024
        or type(timeout_seconds) not in {int, float}
        or not 0.1 <= timeout_seconds <= 30
    ):
        raise ValueError(
            "invalid directory-tree budget; depth 0..20, children 0..500, "
            "nodes 1..10000, bytes 4096..8388608, timeout 0.1..30s"
        )
    try:
        con, validation = open_scan_mode(scan_path, require_audit=False, light=True)
    except (ValueError, OSError, sqlite3.Error) as exc:
        return {"state": "unavailable", "reason": str(exc), "tree": None}
    deadline = time.monotonic() + timeout_seconds
    con.set_progress_handler(lambda: int(time.monotonic() >= deadline), 10_000)
    try:
        source = con.execute(
            "SELECT scan_uuid,evidence_revision,lifecycle,finish_reason,crawl_partial,start_url "
            "FROM scan WHERE singleton=1"
        ).fetchone()
        if source is None or not source["start_url"]:
            return {
                "state": "unavailable",
                "reason": "scan start URL is unavailable",
                "tree": None,
                "validation": validation,
            }
        origin = urlsplit(source["start_url"]).netloc.casefold()
        con.execute("PRAGMA query_only=OFF")
        con.execute(
            "CREATE TEMP TABLE directory_counts(path TEXT PRIMARY KEY,parent TEXT NOT NULL,"
            "name TEXT NOT NULL,pages INTEGER NOT NULL) WITHOUT ROWID"
        )
        total = con.execute("SELECT COUNT(*) FROM pages").fetchone()[0]
        pending: dict[tuple[str, str], int] = {}
        included = excluded_origin = 0

        def flush() -> None:
            if not pending:
                return
            records = [
                (path, parent, name, count)
                for (parent, name), count in pending.items()
                for path in [f"{parent}/{name}" if parent else name]
            ]
            con.executemany(
                "INSERT INTO temp.directory_counts(path,parent,name,pages) VALUES(?,?,?,?) "
                "ON CONFLICT(path) DO UPDATE SET pages=pages+excluded.pages",
                records,
            )
            pending.clear()

        for row in con.execute(
            "SELECT u.url FROM pages p JOIN urls u USING(url_id) ORDER BY p.page_ordinal"
        ):
            if time.monotonic() >= deadline:
                raise sqlite3.OperationalError("interrupted")
            url = row[0]
            if urlsplit(url).netloc.casefold() != origin:
                excluded_origin += 1
                continue
            included += 1
            parent = ""
            for name in _directory_segments(url):
                key = (parent, name)
                pending[key] = pending.get(key, 0) + 1
                parent = f"{parent}/{name}" if parent else name
                if len(pending) >= 1_000:
                    flush()
        flush()
        con.execute(
            "CREATE INDEX temp.directory_counts_parent_rank "
            "ON directory_counts(parent,pages DESC,name ASC)"
        )
        con.execute("PRAGMA query_only=ON")
        remaining = max_nodes - 1
        remaining_bytes = max_bytes - 2_048
        truncated = False

        def node(path: str, name: str, pages: int, depth: int) -> dict:
            nonlocal remaining, remaining_bytes, truncated
            remaining_bytes -= len(name.encode("utf-8")) * 2 + 128
            row = con.execute(
                "SELECT COUNT(*),COALESCE(SUM(pages),0) FROM temp.directory_counts WHERE parent=?",
                (path,),
            ).fetchone()
            child_count = row[0]
            output = {"name": name, "pages": pages, "children": []}
            allowance = 0 if depth >= max_depth else min(max_children, remaining)
            children = con.execute(
                "SELECT path,name,pages FROM temp.directory_counts WHERE parent=? "
                "ORDER BY pages DESC,name ASC LIMIT ?",
                (path, allowance),
            ).fetchall()
            kept = []
            kept_paths = []
            for child_path, child_name, count in children:
                estimate = len(child_name.encode("utf-8")) * 2 + 128
                if remaining <= 0 or remaining_bytes < estimate:
                    truncated = True
                    break
                remaining -= 1
                kept_paths.append(child_path)
                kept.append(node(child_path, child_name, count, depth + 1))
            output["children"] = kept
            shown = len(kept)
            if child_count > shown:
                if kept_paths:
                    marks = ",".join("?" for _ in kept_paths)
                    folded = con.execute(
                        "SELECT COUNT(*),COALESCE(SUM(pages),0) FROM temp.directory_counts "
                        f"WHERE parent=? AND path NOT IN ({marks})",
                        (path, *kept_paths),
                    ).fetchone()
                else:
                    folded = con.execute(
                        "SELECT COUNT(*),COALESCE(SUM(pages),0) FROM temp.directory_counts "
                        "WHERE parent=?",
                        (path,),
                    ).fetchone()
                output["other_children"], output["other_pages"] = folded[0], folded[1]
                truncated = True
            return output

        tree = node("", "/", included, 0)
        result = {
            "state": "partial" if truncated or source["lifecycle"] != "finished" or source["crawl_partial"] else "available",
            "reason": "directory tree was bounded" if truncated else "",
            "source": {
                "scan_uuid": source["scan_uuid"],
                "evidence_revision": source["evidence_revision"],
                "lifecycle": source["lifecycle"],
            },
            "validation": validation,
            "scope": {"origin": origin, "total_pages": total, "included_pages": included,
                      "excluded_other_origins": excluded_origin},
            "max_depth": max_depth,
            "max_children": max_children,
            "max_nodes": max_nodes,
            "tree": tree,
        }
        if len(json.dumps(result, ensure_ascii=False).encode("utf-8")) > max_bytes:
            return {
                "state": "limit_reached",
                "reason": "output_byte_limit_exceeded",
                "source": result["source"],
                "validation": validation,
                "scope": result["scope"],
                "tree": None,
            }
        return result
    except sqlite3.OperationalError as exc:
        if "interrupt" not in str(exc).lower():
            raise
        return {
            "state": "limit_reached",
            "reason": "query_time_budget_exceeded",
            "validation": validation,
            "tree": None,
        }
    finally:
        con.set_progress_handler(None, 0)
        con.close()
