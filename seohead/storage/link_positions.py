"""Streamed offline position derivation with a transactional document checkpoint."""

from __future__ import annotations

import json
from typing import Any

from . import ScanError
from .link_context import _derive


def position_status(position: str | None) -> str:
    return "classified" if position else "not_classified"


def validate_checkpoint(con, item, payload) -> None:
    """Bind resumable progress to the current derived scan and retained document."""
    fields = {
        "schema_version",
        "evidence_revision",
        "last_document_id",
        "documents",
        "filled",
        "unavailable_documents",
    }
    if (
        not isinstance(payload, dict)
        or set(payload) != fields
        or payload["schema_version"] != "links.position.v1"
        or any(
            type(payload[key]) is not int or payload[key] < 0 for key in fields - {"schema_version"}
        )
        or item["item_key"] != "checkpoint"
        or item["completeness"] not in {"complete", "partial"}
        or payload["unavailable_documents"] > payload["documents"]
    ):
        raise ScanError("invalid link position checkpoint")
    scan = con.execute(
        "SELECT source_kind,evidence_revision FROM scan WHERE singleton=1"
    ).fetchone()
    if (
        scan[0] != "reanalysis"
        or payload["evidence_revision"] != scan[1]
        or not con.execute(
            "SELECT 1 FROM documents WHERE document_id=?", (payload["last_document_id"],)
        ).fetchone()
    ):
        raise ScanError("link position checkpoint belongs to another derived revision or document")


def position_summary(con, *, count_cap: int = 100_000) -> dict[str, Any]:
    """Count a bounded prefix, keeping blanks separate from observed content links."""
    row = con.execute(
        "SELECT COUNT(*),SUM(position!='') FROM (SELECT position FROM links LIMIT ?)",
        (count_cap + 1,),
    ).fetchone()
    count, classified = row[0], row[1] or 0
    capped = count > count_cap
    state = (
        "partial"
        if capped or 0 < classified < count
        else "complete"
        if classified
        else "not_classified"
    )
    return {
        "state": state,
        "position_status": "classified" if state == "complete" else state,
        "has_positions": bool(classified) if classified or not capped else None,
        "observed_links": count,
        "classified": classified,
        "not_classified": count - classified,
        "capped": capped,
        "count_cap": count_cap,
        "scope": "retained hyperlink occurrences",
    }


def backfill_positions(
    scan,
    *,
    max_documents: int | None = None,
    max_body_bytes: int = 5 * 1024 * 1024,
    resume: bool = True,
) -> dict[str, Any]:
    """Fill blanks in a private derived writer, one document/commit at a time.

    Recalling this function on that writer continues after its durable checkpoint.
    Existing classified occurrences, missing bodies and identity mismatches are never
    replaced. The source capture and its recorded classification policy stay unchanged.
    """
    if max_documents is not None and (type(max_documents) is not int or max_documents < 1):
        raise ScanError("max_documents must be a positive integer")
    if type(max_body_bytes) is not int or not 1 <= max_body_bytes <= 8 * 1024 * 1024:
        raise ScanError("max_body_bytes must be 1..8388608")
    identity = scan.con.execute(
        "SELECT source_kind,evidence_revision FROM scan WHERE singleton=1"
    ).fetchone()
    if identity[0] != "reanalysis":
        raise ScanError("link position backfill requires a private derived scan")
    prior = scan.con.execute(
        "SELECT payload_json FROM context_items WHERE kind='links.position' AND item_key='checkpoint'"
    ).fetchone()
    progress = json.loads(prior[0]) if prior else {}
    if not resume or progress.get("evidence_revision") not in {identity[1], identity[1] - 1}:
        progress = {
            "schema_version": "links.position.v1",
            "evidence_revision": identity[1],
            "last_document_id": 0,
            "documents": 0,
            "filled": 0,
            "unavailable_documents": 0,
        }
    progress["evidence_revision"] = identity[1]
    # Documents is keyed by document_id; source-document lookups use its page/representation
    # index rather than scanning links for each document in a million-page capture.
    for count, document in enumerate(
        scan.con.execute(
            "SELECT document_id,url_id,representation FROM documents WHERE document_id>? ORDER BY document_id",
            (progress["last_document_id"],),
        ),
        1,
    ):
        document_id = document[0]
        links = [
            dict(row)
            for row in scan.con.execute(
                "SELECT l.*,s.url AS source_url,d.url AS destination_url FROM links l "
                "JOIN urls s ON s.url_id=l.source_url_id JOIN urls d ON d.url_id=l.destination_url_id "
                "WHERE l.source_url_id=? AND l.evidence_representation=? AND l.source_document_id=? "
                "ORDER BY l.ordinal,l.link_id LIMIT 20001",
                (document[1], document[2], document_id),
            )
        ]
        updates = []
        unavailable = False
        if any(not item["position"] for item in links):
            items, coverage = _derive(scan.con, document_id, links, max_body_bytes)
            unavailable = coverage["state"] == "unavailable"
            updates = [
                (item["placement"]["position"], item["link_id"])
                for item in items
                if item["state"] == "measured" and not item["stored_position"]
            ]
        next_progress = {
            **progress,
            "last_document_id": document_id,
            "documents": progress["documents"] + 1,
            "filled": progress["filled"] + len(updates),
            "unavailable_documents": progress["unavailable_documents"] + int(unavailable),
        }
        scan._begin()
        try:
            scan.con.executemany(
                "UPDATE links SET position=? WHERE link_id=? AND position=''", updates
            )
            scan.con.execute(
                "INSERT OR REPLACE INTO context_items VALUES ('links.position','checkpoint',"
                "'scan_context.v1',?,'partial','offline retained-document position backfill')",
                (json.dumps(next_progress, sort_keys=True),),
            )
            scan.con.commit()
        except BaseException:
            scan._rollback()
            raise
        progress = next_progress
        if max_documents is not None and count >= max_documents:
            break
    has_more = (
        scan.con.execute(
            "SELECT 1 FROM documents WHERE document_id>? LIMIT 1", (progress["last_document_id"],)
        ).fetchone()
        is not None
    )
    if progress["last_document_id"]:
        scan.con.execute(
            "UPDATE context_items SET completeness=?,payload_json=? "
            "WHERE kind='links.position' AND item_key='checkpoint'",
            ("partial" if has_more else "complete", json.dumps(progress, sort_keys=True)),
        )
        scan.con.commit()
    return {
        "step": "links.position",
        "state": "partial" if has_more else "complete",
        "network": "disabled",
        "has_more": has_more,
        **progress,
    }
