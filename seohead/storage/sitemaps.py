"""Closed context for the URL membership of selected expanded sitemap roots."""

from __future__ import annotations

import json
import sqlite3
from typing import Any

from . import ScanError, _dump, _insert, _url

KINDS = {"sitemap_declaration", "sitemap_declared_url", "sitemap_fetch_summary"}
MAX_SELECTED_ROOTS = 5000
MAX_ID = 2**63 - 1


def root_ids(con: sqlite3.Connection) -> set[int]:
    roots = set()
    for index, row in enumerate(
        con.execute("SELECT payload_json FROM context_items WHERE kind='sitemap_declaration'")
    ):
        if index >= MAX_SELECTED_ROOTS:
            raise ScanError("too many selected sitemap roots")
        value = json.loads(row[0])
        if not isinstance(value, dict) or type(value.get("sitemap_url_id")) is not int:
            raise ScanError("invalid selected sitemap root")
        sid = value["sitemap_url_id"]
        if not 1 <= sid <= MAX_ID or sid in roots:
            raise ScanError("selected sitemap roots must reference unique valid URLs")
        roots.add(sid)
    return roots


def validate_context(
    con: sqlite3.Connection,
    item: dict[str, Any],
    payload: Any,
    selected_roots: set[int] | None = None,
) -> None:
    kind = item["kind"]
    keys = {
        "sitemap_declaration": {"sitemap_url_id", "source", "ordinal"},
        "sitemap_declared_url": {"sitemap_url_id", "url_id", "ordinal"},
        "sitemap_fetch_summary": {"sitemap_url_id", "response_ids", "complete", "reason"},
    }[kind]
    if not isinstance(payload, dict) or set(payload) != keys:
        raise ScanError(f"{kind}: invalid sitemap context shape")
    sid = payload["sitemap_url_id"]
    if (
        type(sid) is not int
        or not 1 <= sid <= MAX_ID
        or con.execute("SELECT 1 FROM urls WHERE url_id=?", (sid,)).fetchone() is None
    ):
        raise ScanError(f"{kind}: invalid sitemap URL reference")
    if kind == "sitemap_declaration":
        ordinal = payload["ordinal"]
        if (
            type(ordinal) is not int
            or not 0 <= ordinal < MAX_SELECTED_ROOTS
            or item["item_key"] != f"ordinal:{ordinal}"
            or payload["source"] not in {"explicit", "robots", "fallback", "sitemap-only"}
        ):
            raise ScanError("invalid sitemap declaration source/order")
    elif kind == "sitemap_declared_url":
        ordinal, url_id = payload["ordinal"], payload["url_id"]
        if (
            type(ordinal) is not int
            or not 0 <= ordinal <= MAX_ID
            or type(url_id) is not int
            or not 1 <= url_id <= MAX_ID
            or item["item_key"] != f"sitemap:{sid}:ordinal:{ordinal}"
        ):
            raise ScanError("invalid sitemap member source/order")
        if con.execute("SELECT 1 FROM urls WHERE url_id=?", (url_id,)).fetchone() is None:
            raise ScanError("sitemap member references an unknown URL")
    else:
        if (
            item["item_key"] != f"url:{sid}"
            or type(payload["complete"]) is not bool
            or type(payload["reason"]) is not str
            or not isinstance(payload["response_ids"], list)
        ):
            raise ScanError("invalid sitemap expansion summary")
        if (item["completeness"] == "complete") != payload["complete"]:
            raise ScanError("sitemap expansion completeness disagrees")
        for response_id in payload["response_ids"]:
            if (
                type(response_id) is not int
                or not 1 <= response_id <= MAX_ID
                or con.execute(
                    "SELECT 1 FROM responses WHERE response_id=? AND purpose='sitemap'",
                    (response_id,),
                ).fetchone()
                is None
            ):
                raise ScanError("sitemap expansion references an unavailable response")
    if kind != "sitemap_declaration" and sid not in (
        root_ids(con) if selected_roots is None else selected_roots
    ):
        raise ScanError("sitemap membership has no selected root declaration")


def _context(
    kind: str, key: str, payload: dict[str, Any], *, complete: bool = True, reason: str = ""
) -> dict[str, str]:
    return {
        "kind": kind,
        "item_key": key,
        "payload_version": "scan_context.v1",
        "payload_json": _dump(payload),
        "completeness": "complete" if complete else "partial",
        "reason": reason,
    }


def declare(con: sqlite3.Connection, url: str, source: str, ordinal: int) -> int:
    from .native_context import put_context

    if (
        type(url) is not str
        or not url
        or type(source) is not str
        or source not in {"explicit", "robots", "fallback", "sitemap-only"}
        or type(ordinal) is not int
        or ordinal < 0
    ):
        raise ScanError("invalid selected sitemap root")
    existing = con.execute(
        "SELECT payload_json FROM context_items WHERE kind='sitemap_declaration' AND item_key=?",
        (f"ordinal:{ordinal}",),
    ).fetchone()
    count = con.execute(
        "SELECT COUNT(*) FROM context_items WHERE kind='sitemap_declaration'"
    ).fetchone()[0]
    if existing is None and (ordinal != count or count >= MAX_SELECTED_ROOTS):
        raise ScanError("sitemap declaration order must append a bounded source sequence")
    sid = _url(con, url)
    if existing is None and sid in root_ids(con):
        raise ScanError("selected sitemap roots must be unique")
    put_context(
        con,
        _context(
            "sitemap_declaration",
            f"ordinal:{ordinal}",
            {
                "sitemap_url_id": sid,
                "source": source,
                "ordinal": ordinal,
            },
            reason="selected expanded sitemap root",
        ),
    )
    return sid


def finish(con: sqlite3.Connection, sid: int, complete: bool, reason: str) -> None:
    item = _context(
        "sitemap_fetch_summary",
        f"url:{sid}",
        {
            "sitemap_url_id": sid,
            "response_ids": [],
            "complete": complete,
            "reason": reason,
        },
        complete=complete,
        reason=reason,
    )
    validate_context(con, item, json.loads(item["payload_json"]))
    previous = con.execute(
        "SELECT 1 FROM context_items WHERE kind='sitemap_fetch_summary' AND item_key=?",
        (item["item_key"],),
    ).fetchone()
    if previous:
        con.execute(
            "UPDATE context_items SET payload_json=?,completeness=?,reason=? WHERE kind='sitemap_fetch_summary' AND item_key=?",
            (item["payload_json"], item["completeness"], reason, item["item_key"]),
        )
    else:
        _insert(con, "context_items", item)
