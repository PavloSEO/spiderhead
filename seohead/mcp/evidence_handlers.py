"""Read-only projections and explicit saved-scan mutation boundaries."""

from __future__ import annotations

import json
from typing import Any


def scan_evidence(
    input_path: str, section: str = "capabilities", limit: int = 1000, offset: int = 0
) -> dict[str, Any]:
    from seohead.storage import open_scan

    if type(limit) is not int or type(offset) is not int or not 1 <= limit <= 10000 or offset < 0:
        raise ValueError("limit must be 1..10000 and offset nonnegative")
    if section not in {
        "capabilities",
        "corpus",
        "structured",
        "routes",
        "resources",
        "timeline",
        "relations",
        "browser",
        "extraction",
    }:
        raise ValueError("unknown saved evidence section")
    con = open_scan(input_path, require_audit=False)
    try:
        if section == "relations":
            from seohead.storage.discovery_ledger import read

            result = read(con, limit=limit, offset=offset)
        elif section == "resources":
            from seohead.storage.resource_graph import read

            result = read(con, limit=limit, offset=offset)
        elif section == "timeline":
            from seohead.storage.events import timeline

            result = timeline(con, limit=limit)
        elif section == "routes":
            from seohead.storage.rendered_routes import read

            result = read(con)
        elif section in {"corpus", "structured", "browser", "extraction"}:
            kinds = {
                "corpus": ("content_evidence",),
                "structured": ("structured_evidence", "language_evidence"),
                "browser": ("browser_artifacts",),
                "extraction": ("extraction_rule_evidence",),
            }[section]
            placeholders = ",".join("?" for _ in kinds)
            total = con.execute(
                f"SELECT COUNT(*) FROM context_items WHERE kind IN ({placeholders})", kinds
            ).fetchone()[0]
            saved = con.execute(
                f"SELECT kind,item_key,payload_json,completeness,reason FROM context_items WHERE kind IN ({placeholders}) ORDER BY kind,item_key LIMIT ? OFFSET ?",
                (*kinds, limit, offset),
            ).fetchall()
            result = {
                "state": "recorded" if total else "unavailable",
                "reason": "" if total else "requested evidence was not retained",
                "total": total,
                "offset": offset,
                "limit": limit,
                "has_more": offset + len(saved) < total,
                "items": [
                    {
                        "kind": row["kind"],
                        "key": row["item_key"],
                        "state": row["completeness"],
                        "reason": row["reason"],
                        "observation": json.loads(row["payload_json"]),
                    }
                    for row in saved
                ],
            }
        else:
            row = con.execute("SELECT document_json FROM audit WHERE singleton=1").fetchone()
            if row is None:
                result = {"state": "unavailable", "reason": "no saved audit capability projection"}
            else:
                audit = json.loads(row[0])
                from seohead.sf.core.evidence_contract import attach_contract

                identity = con.execute("SELECT scan_uuid FROM scan WHERE singleton=1").fetchone()[0]
                result = attach_contract(audit, scan_uuid=identity, con=con)["summary"][
                    "evidence_contract"
                ]
        return {"ok": True, "section": section, "evidence": result}
    finally:
        con.close()


def scan_extract(
    input_path: str,
    rules: list[dict[str, Any]],
    url: str | None = None,
    representation: str = "static",
    limit: int = 100,
) -> dict[str, Any]:
    """Apply bounded declarative rules to retained bodies without a fetch or mutation."""
    from seohead.checks.extraction_rules import evaluate, validate_rules
    from seohead.checks.parser import parse_html
    from seohead.storage import ScanError, open_scan
    from seohead.storage.bodies import read_document

    if (
        representation not in {"static", "rendered", "legacy_fragment"}
        or type(limit) is not int
        or not 1 <= limit <= 1000
    ):
        raise ValueError("invalid representation or page limit")
    selected = validate_rules(rules)
    con = open_scan(input_path, require_audit=False)
    try:
        where = "d.representation=?"
        parameters: list[Any] = [representation]
        if url is not None:
            where += " AND u.url=?"
            parameters.append(url)
        rows = con.execute(
            "SELECT d.*,u.url FROM documents d JOIN urls u USING(url_id) WHERE "
            + where
            + " ORDER BY document_id LIMIT ?",
            (*parameters, limit + 1),
        ).fetchall()
        items = []
        output_bytes = 0
        output_limited = False
        for row in rows[:limit]:
            html = None
            reason = ""
            try:
                html = read_document(con, row["document_id"], max_decoded_bytes=8 * 1024 * 1024)
            except ScanError as exc:
                reason = str(exc)
            parsed = parse_html(html, row["url"]) if html is not None else None
            result = evaluate(
                html=html, parsed=parsed, rules=selected, representation=representation
            )
            if reason:
                result["reason"] = reason
            item = {
                "url": row["url"],
                "document_id": row["document_id"],
                "representation": representation,
                "result": result,
            }
            size = len(json.dumps(item, ensure_ascii=False).encode("utf-8"))
            if output_bytes + size > 8 * 1024 * 1024:
                output_limited = True
                break
            items.append(item)
            output_bytes += size
        return {
            "ok": True,
            "items": items,
            "truncated": len(rows) > limit or output_limited,
            "output_limit_bytes": 8 * 1024 * 1024,
            "scope": "retained complete bodies only; no network or artifact mutation",
        }
    finally:
        con.close()


def scan_fragment_links(
    input_path: str,
    *,
    offset: int = 0,
    limit: int = 100,
    state: str | None = None,
    representation: str | None = None,
) -> dict[str, Any]:
    """Evaluate fragment anchors over retained complete HTML/DOM, offline.

    Only ``body_state='complete'`` HTML documents are measured; a missing,
    truncated, unsupported, failed or budget-exhausted body is reported as a
    named ``skipped`` occurrence or unavailable source -- never as a broken
    fragment.
    """
    from seohead.storage import open_scan
    from seohead.storage.fragment_links import evaluate, paginate

    if type(limit) is not int or type(offset) is not int or not 1 <= limit <= 10_000 or offset < 0:
        raise ValueError("limit must be 1..10000 and offset nonnegative")
    if state is not None and state not in {"resolved", "missing", "skipped"}:
        raise ValueError("state must be resolved, missing or skipped")
    if representation is not None and representation not in {
        "static",
        "rendered",
        "legacy_fragment",
    }:
        raise ValueError("representation must be static, rendered or legacy_fragment")
    con = open_scan(input_path, require_audit=False)
    try:
        result = paginate(
            evaluate(con),
            offset=offset,
            limit=limit,
            state=state,
            representation=representation,
        )
        result["ok"] = True
        result["scope"] = (
            "retained complete HTML/DOM only; missing or incomplete evidence "
            "is skipped by name, never reported as a broken fragment"
        )
        return result
    finally:
        con.close()


def scan_structured_blocks(
    input_path: str,
    url: str,
    representation: str = "static",
    variant_key: str | None = None,
) -> dict[str, Any]:
    """Read bounded exact-URL retained schema details without network access.

    The existing ``blocks`` member remains the JSON-LD source view. ``schema_detail``
    adds per-format shared-validator results and the saved retained validation contract.
    """
    import json

    from seohead.checks.schema_org import MAX_HTML_BYTES, check_schema
    from seohead.checks.structured_blocks import structured_blocks
    from seohead.storage import ScanError, open_scan
    from seohead.storage.bodies import read_page_document_content
    from seohead.storage.fragment_links import _document_final_url
    from seohead.storage.structured_evidence import validate_context

    if not isinstance(url, str) or not url or len(url) > 8192:
        raise ValueError("url must be nonempty exact retained URL text of at most 8192 characters")
    if representation not in {"static", "rendered", "legacy_fragment"}:
        raise ValueError("representation must be static, rendered or legacy_fragment")
    if variant_key is not None and (not isinstance(variant_key, str) or len(variant_key) > 2048):
        raise ValueError("variant_key must be at most 2048 characters or null")
    con = open_scan(input_path, require_audit=False)
    try:
        document = read_page_document_content(
            con,
            url,
            representation=representation,
            variant_key=variant_key,
            max_decoded_bytes=MAX_HTML_BYTES,
        )
        document_state = document.get("state")
        document_id = document.get("document_id")
        row = None
        if type(document_id) is int:
            row = con.execute(
                "SELECT d.* FROM documents d JOIN urls u USING(url_id) "
                "WHERE d.document_id=? AND u.url=? AND d.representation=?",
                (document_id, url, representation),
            ).fetchone()

        scan_row = con.execute(
            "SELECT scan_uuid,evidence_revision FROM scan WHERE singleton=1"
        ).fetchone()
        scan_uuid = document.get("scan_uuid") or (scan_row[0] if scan_row is not None else None)
        evidence_revision = document.get("evidence_revision") or (
            scan_row[1] if scan_row is not None else None
        )
        page_url_id = row["url_id"] if row is not None else None
        evidence_key = (
            f"page:{page_url_id}:document:{document_id}:representation:{representation}"
            if page_url_id is not None and document_id is not None
            else None
        )

        def retained_payload(kind: str) -> dict[str, Any] | None:
            if evidence_key is None:
                return None
            saved = con.execute(
                "SELECT kind,item_key,payload_version,payload_json,completeness,reason "
                "FROM context_items WHERE kind=? AND item_key=?",
                (kind, evidence_key),
            ).fetchone()
            if saved is None:
                return None
            item = dict(saved)
            try:
                payload = json.loads(item["payload_json"])
            except json.JSONDecodeError:
                return {
                    "state": "unavailable",
                    "reason": "retained evidence payload is not valid JSON",
                }
            try:
                validate_context(con, item, payload)
            except ScanError as exc:
                return {
                    "state": "unavailable",
                    "reason": f"retained evidence failed contract validation: {str(exc)[:300]}",
                }
            return payload

        structured = retained_payload("structured_evidence")
        language = retained_payload("language_evidence")
        provenance = {
            "url": url,
            "scan_uuid": scan_uuid,
            "evidence_revision": evidence_revision,
            "document_id": document_id,
            "representation": representation,
            "variant_key": document.get("variant_key", variant_key),
        }
        if document_state != "available" or row is None:
            reason = str(document.get("reason") or "retained document body is unavailable")
            state = "not_found" if reason == "document_not_in_corpus" else "unavailable"
            return {
                "ok": True,
                "state": state,
                "source_state": "unavailable",
                "reason": reason,
                **provenance,
                "content_type": document.get("content_type"),
                "raw_body_sha256": document.get("raw_body_sha256"),
                "sha256": document.get("sha256"),
                "structured_evidence": _structured_evidence_summary(structured),
                "language_evidence": _language_evidence_summary(language),
                "schema_detail": {
                    "state": structured.get("state", "unavailable") if structured else "unavailable",
                    "reason": structured.get("reason", reason) if structured else reason,
                    "analysis_state": "unavailable",
                    "validation": _bounded_validation(
                        structured.get("validation") if structured else None
                    ),
                    "rules_version": None,
                    "entities": [],
                    "entity_count": 0,
                    "formats": {},
                },
            }

        html = str(document.pop("text"))
        base_url = _document_final_url(con, dict(row), url)
        checked = check_schema(html=html, document_base_url=base_url)
        blocks = structured_blocks(html, base_url=base_url, checked_page=checked)
        structured_summary = _structured_evidence_summary(structured)
        language_summary = _language_evidence_summary(language)
        entities = checked.get("entities") or []
        entity_count = len(entities)
        entity_details = _schema_entity_details(entities)
        coverage = checked.get("markup_coverage") or []
        coverage_by_format = {
            item.get("format"): item
            for item in coverage
            if isinstance(item, dict) and isinstance(item.get("format"), str)
        }
        coverage_details = [
            {
                **{key: value for key, value in item.items() if key != "reasons"},
                "reasons": [str(value)[:500] for value in (item.get("reasons") or [])[:20]],
                "reasons_truncated": len(item.get("reasons") or []) > 20,
            }
            for item in coverage[:20]
            if isinstance(item, dict)
        ]
        formats = {
            "jsonld": {
                "state": (
                    "partial" if blocks.get("blocks_truncated") else
                    "absent" if blocks.get("state") == "absent" else
                    "checked"
                ),
                "result_state": blocks.get("state", "unavailable"),
                "reason": blocks.get("reason", ""),
                "block_count": blocks.get("block_count", 0),
                "blocks_truncated": blocks.get("blocks_truncated", False),
                "entity_count": sum(item.get("format") == "jsonld" for item in entities if isinstance(item, dict)),
                "entities": [item for item in entity_details if item["format"] == "jsonld"],
                "entities_truncated": sum(item.get("format") == "jsonld" for item in entities if isinstance(item, dict))
                > sum(item.get("format") == "jsonld" for item in entity_details),
            },
            "microdata": {
                **coverage_by_format.get("microdata", {"status": "unavailable", "reasons": []}),
                "state": coverage_by_format.get("microdata", {}).get("status", "unavailable"),
                "reason": "; ".join(
                    str(item)[:500]
                    for item in (coverage_by_format.get("microdata", {}).get("reasons") or [])[:20]
                ),
                "entity_count": sum(item.get("format") == "microdata" for item in entities if isinstance(item, dict)),
                "entities": [item for item in entity_details if item["format"] == "microdata"],
                "entities_truncated": sum(item.get("format") == "microdata" for item in entities if isinstance(item, dict))
                > sum(item.get("format") == "microdata" for item in entity_details),
            },
            "rdfa": {
                **coverage_by_format.get("rdfa", {"status": "unavailable", "reasons": []}),
                "state": coverage_by_format.get("rdfa", {}).get("status", "unavailable"),
                "reason": "; ".join(
                    str(item)[:500]
                    for item in (coverage_by_format.get("rdfa", {}).get("reasons") or [])[:20]
                ),
                "entity_count": sum(item.get("format") == "rdfa" for item in entities if isinstance(item, dict)),
                "entities": [item for item in entity_details if item["format"] == "rdfa"],
                "entities_truncated": sum(item.get("format") == "rdfa" for item in entities if isinstance(item, dict))
                > sum(item.get("format") == "rdfa" for item in entity_details),
            },
        }
        partial_reasons = [
            str(reason)[:500]
            for item in coverage
            if isinstance(item, dict)
            for reason in item.get("reasons") or []
        ]
        if blocks.get("blocks_truncated"):
            partial_reasons.append(str(blocks.get("reason") or "JSON-LD block details were truncated")[:500])
        if entity_count > len(entity_details):
            partial_reasons.append("Schema entity details were truncated to the retained response limit")
        if any(item.get("entities_truncated") for item in formats.values() if isinstance(item, dict)):
            partial_reasons.append("Per-format entity details were truncated to the retained response limit")
        saved_validation = structured.get("validation") if structured else None
        schema_state = structured.get("state", "unavailable") if structured else "unavailable"
        schema_reason = structured.get("reason", "retained structured evidence was not stored") if structured else "retained structured evidence was not stored"
        analysis_state = (
            "partial" if partial_reasons else
            "malformed" if checked.get("parse_errors") else
            "checked"
        )
        result = {
            "ok": True,
            "state": blocks.get("state", "unavailable"),
            "source_state": "available",
            "reason": blocks.get("reason", ""),
            **provenance,
            "content_type": document.get("content_type"),
            "raw_body_sha256": document.get("raw_body_sha256"),
            "sha256": document.get("sha256"),
            "encoding": document.get("encoding"),
            "decoder": document.get("decoder"),
            "total_chars": document.get("total_chars"),
            "lines": document.get("lines"),
            "block_count": blocks.get("block_count", 0),
            "blocks_truncated": blocks.get("blocks_truncated", False),
            "scope": "retained complete body only; local offline schema validation; no network or mutation",
            "blocks": blocks.get("blocks", []),
            "structured_evidence": structured_summary,
            "language_evidence": language_summary,
            "schema_detail": {
                "state": schema_state,
                "reason": schema_reason,
                "analysis_state": analysis_state,
                "validation": _bounded_validation(saved_validation),
                "rules_version": checked.get("rules_version"),
                "entity_count": entity_count,
                "entities_truncated": entity_count > len(entity_details),
                "entities": entity_details,
                "markup_counts": checked.get("markup_counts", {}),
                "markup_coverage": coverage_details,
                "partial_reasons": list(dict.fromkeys(partial_reasons))[:20],
                "parse_errors": [str(item)[:500] for item in (checked.get("parse_errors") or [])[:100]],
                "graph": checked.get("graph", {}),
                "local_eligibility": _bounded_local_eligibility(
                    checked.get("rich_results") or []
                ),
                "local_eligibility_count": len(checked.get("rich_results") or []),
                "local_eligibility_truncated": len(checked.get("rich_results") or []) > 200,
                "formats": formats,
                "google_display": "unknown",
            },
        }
        return result
    finally:
        con.close()


_SCHEMA_DETAIL_LIMIT = 200
_SCHEMA_MESSAGE_LIMIT = 20
_SCHEMA_EVIDENCE_LIMIT = 100


def _bounded_evidence(payload: dict[str, Any] | None, *, language: bool = False) -> dict[str, Any]:
    if not isinstance(payload, dict):
        return {"state": "unavailable", "reason": "retained evidence was not stored"}
    if language:
        declarations = payload.get("declarations") or []
        return {
            "schema_version": payload.get("schema_version"),
            "state": payload.get("state", "unavailable"),
            "reason": payload.get("reason", ""),
            "html_lang": payload.get("html_lang"),
            "declaration_count": len(declarations),
            "declarations": [
                {
                    key: str(value)[:2048] if isinstance(value, str) else value
                    for key, value in item.items()
                }
                for item in declarations[:_SCHEMA_EVIDENCE_LIMIT]
                if isinstance(item, dict)
            ],
            "declarations_truncated": len(declarations) > _SCHEMA_EVIDENCE_LIMIT,
        }
    bounded = {
        "schema_version": payload.get("schema_version"),
        "state": payload.get("state", "unavailable"),
        "reason": payload.get("reason", ""),
        "page_url_id": payload.get("page_url_id"),
        "source_document_id": payload.get("source_document_id"),
        "representation": payload.get("representation"),
        "validation": payload.get("validation"),
        "projection": "bounded retained evidence",
    }
    for key in ("blocks", "nodes", "edges"):
        values = payload.get(key) or []
        bounded[key] = [
            {
                name: (
                    [str(value)[:2048] for value in value[:32]]
                    if isinstance(value, list)
                    else str(value)[:2048]
                    if isinstance(value, str)
                    else value
                )
                for name, value in item.items()
            }
            for item in values[:_SCHEMA_EVIDENCE_LIMIT]
            if isinstance(item, dict)
        ]
        bounded[f"{key}_count"] = len(values)
        bounded[f"{key}_truncated"] = len(values) > _SCHEMA_EVIDENCE_LIMIT
    return bounded


def _structured_evidence_summary(payload: dict[str, Any] | None) -> dict[str, Any]:
    return _bounded_evidence(payload)


def _language_evidence_summary(payload: dict[str, Any] | None) -> dict[str, Any]:
    return _bounded_evidence(payload, language=True)


def _bounded_validation(validation: Any) -> dict[str, Any]:
    if not isinstance(validation, dict):
        return {"state": "unavailable", "reason": "retained validation was not stored"}
    checks = validation.get("feature_checks") or []
    return {
        **{key: value for key, value in validation.items() if key != "feature_checks"},
        "feature_check_count": len(checks),
        "feature_checks": [
            {
                **{key: value for key, value in item.items() if key not in {
                    "missing_required", "missing_required_any_of"
                }},
                "missing_required": [str(value)[:256] for value in (item.get("missing_required") or [])[:50]],
                "missing_required_any_of": [
                    [str(value)[:256] for value in group[:50]]
                    for group in (item.get("missing_required_any_of") or [])[:20]
                    if isinstance(group, list)
                ],
            }
            for item in checks[:200]
            if isinstance(item, dict)
        ],
        "feature_checks_truncated": len(checks) > 200,
    }


def _bounded_local_eligibility(items: Any) -> list[dict[str, Any]]:
    if not isinstance(items, list):
        return []
    result = []
    for item in items[:200]:
        if not isinstance(item, dict):
            continue
        result.append(
            {
                **{
                    key: value
                    for key, value in item.items()
                    if key not in {"missing_required", "missing_required_any_of"}
                },
                "missing_required": [
                    str(value)[:256] for value in (item.get("missing_required") or [])[:50]
                ],
                "missing_required_any_of": [
                    [str(value)[:256] for value in group[:50]]
                    for group in (item.get("missing_required_any_of") or [])[:20]
                    if isinstance(group, list)
                ],
            }
        )
    return result


def _schema_entity_details(entities: Any) -> list[dict[str, Any]]:
    if not isinstance(entities, list):
        return []
    result = []
    for entity in entities[:_SCHEMA_DETAIL_LIMIT]:
        if not isinstance(entity, dict):
            continue
        errors = entity.get("errors") or []
        warnings = entity.get("warnings") or []
        result.append(
            {
                "path": str(entity.get("path") or "")[:512],
                "id": str(entity["id"])[:2048] if entity.get("id") is not None else None,
                "format": str(entity.get("format") or "")[:32],
                "types": [str(value)[:256] for value in (entity.get("types") or [])[:32]],
                "errors": [str(value)[:500] for value in errors[:_SCHEMA_MESSAGE_LIMIT]],
                "warnings": [str(value)[:500] for value in warnings[:_SCHEMA_MESSAGE_LIMIT]],
                "error_count": len(errors),
                "warning_count": len(warnings),
                "messages_truncated": (
                    len(errors) > _SCHEMA_MESSAGE_LIMIT or len(warnings) > _SCHEMA_MESSAGE_LIMIT
                ),
            }
        )
    return result
