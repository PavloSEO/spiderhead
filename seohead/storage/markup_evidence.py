"""Document-bound static AMP and accessibility observations, without SQL changes."""

from __future__ import annotations

import json
from typing import Any

from . import ScanError

KIND = "markup_evidence"
VERSION = "markup_evidence.v1"
REPRESENTATIONS = {"static", "rendered", "legacy_fragment"}


def context_item(
    *,
    page_url_id: int,
    source_document_id: int,
    representation: str,
    parsed: dict[str, Any] | None,
    unavailable_reason: str = "",
) -> dict[str, Any]:
    """Keep measured absence distinct from an unparsed document."""
    facts = parsed.get("markup") if isinstance(parsed, dict) else None
    available = isinstance(facts, dict) and not unavailable_reason
    payload = {
        "schema_version": VERSION,
        "page_url_id": page_url_id,
        "source_document_id": source_document_id,
        "representation": representation,
        "state": "complete" if available else "unavailable",
        "reason": "" if available else unavailable_reason or "document markup was not parsed",
        "facts": facts if available else None,
    }
    return {
        "kind": KIND,
        "item_key": f"page:{page_url_id}:document:{source_document_id}:representation:{representation}",
        "payload_version": "scan_context.v1",
        "payload_json": json.dumps(payload, sort_keys=True, separators=(",", ":")),
        "completeness": payload["state"],
        "reason": payload["reason"],
    }


def validate_context(con: Any, item: dict[str, Any], payload: Any) -> None:
    """Reject malformed facts and mismatched source-document identities."""
    from seohead.checks.markup import A11Y_CODES, AMP_CODES

    if (
        not isinstance(payload, dict)
        or set(payload)
        != {
            "schema_version",
            "page_url_id",
            "source_document_id",
            "representation",
            "state",
            "reason",
            "facts",
        }
        or payload["schema_version"] != VERSION
        or type(payload["page_url_id"]) is not int
        or payload["page_url_id"] < 1
        or type(payload["source_document_id"]) is not int
        or payload["source_document_id"] < 1
        or payload["representation"] not in REPRESENTATIONS
        or payload["state"] not in {"complete", "unavailable"}
        or not isinstance(payload["reason"], str)
        or payload["state"] != item["completeness"]
        or payload["reason"] != item["reason"]
    ):
        raise ScanError("markup evidence payload is invalid")
    facts = payload["facts"]
    if payload["state"] == "unavailable":
        if facts is not None or not payload["reason"]:
            raise ScanError("unavailable markup evidence requires a reason and no facts")
    else:
        if not isinstance(facts, dict) or set(facts) != {"amp", "accessibility"}:
            raise ScanError("markup evidence facts are invalid")
        amp, accessibility = facts["amp"], facts["accessibility"]
        if (
            not isinstance(amp, dict)
            or set(amp)
            != {
                "declared",
                "errors",
                "method",
                "validator_state",
                "validator_reason",
                "unavailable",
            }
            or amp.get("method") != "static_structure"
            or type(amp.get("declared")) is not bool
            or not isinstance(amp.get("errors"), list)
            or amp.get("validator_state") != "unavailable"
            or not isinstance(amp.get("validator_reason"), str)
            or not amp["validator_reason"]
            or any(
                not isinstance(error, dict) or error.get("code") not in AMP_CODES
                for error in amp["errors"]
            )
            or not isinstance(accessibility, dict)
            or set(accessibility) != {"method", "issues", "sample_limit", "unavailable"}
            or type(accessibility.get("sample_limit")) is not int
            or accessibility["sample_limit"] != 20
            or accessibility.get("method") != "static_html"
            or not isinstance(accessibility.get("issues"), list)
            or not isinstance(accessibility.get("unavailable"), list)
        ):
            raise ScanError("markup evidence measurements are invalid")
        for issue in accessibility["issues"]:
            if (
                not isinstance(issue, dict)
                or set(issue) != {"code", "count", "samples", "samples_truncated"}
                or issue.get("code") not in A11Y_CODES
                or type(issue.get("count")) is not int
                or issue["count"] < 1
                or not isinstance(issue.get("samples"), list)
                or len(issue["samples"]) > min(20, issue["count"])
                or type(issue.get("samples_truncated")) is not bool
                or issue["samples_truncated"] != (issue["count"] > len(issue["samples"]))
            ):
                raise ScanError("markup evidence issue is invalid")
    if (
        item["item_key"]
        != f"page:{payload['page_url_id']}:document:{payload['source_document_id']}:representation:{payload['representation']}"
        or not con.execute(
            "SELECT 1 FROM documents WHERE document_id=? AND url_id=? AND representation=?",
            (payload["source_document_id"], payload["page_url_id"], payload["representation"]),
        ).fetchone()
    ):
        raise ScanError("markup evidence binds the wrong document")
