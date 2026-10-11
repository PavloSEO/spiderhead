"""Findings from versioned document markup, not inferred SF export columns."""

from __future__ import annotations

import json

from seohead.checks.markup import A11Y_CODES, AMP_CODES

from .context import AuditContext
from .normalize import norm_url

SCHEMA_CODES = (
    "STRUCTURED_DATA_SCHEMA_ERROR",
    "STRUCTURED_DATA_SCHEMA_WARNING",
    "STRUCTURED_DATA_RICH_RESULT_REQUIRED_MISSING",
)


def check_document_markup(ctx: AuditContext) -> None:
    """Read one active document per URL; unmeasured documents stay unavailable."""
    all_codes = (
        *AMP_CODES,
        "AMP_MISSING_HTML_ATTRIBUTE",
        *A11Y_CODES,
        *SCHEMA_CODES,
    )
    if ctx.scan_con is None:
        for code in all_codes:
            ctx.skip(code, "document-bound markup evidence requires a retained native crawl")
        return
    con = ctx.scan_con
    targets = {
        norm_url(str(page.metrics.get("_record", {}).get("amphtml")))
        for page in ctx.html_pages()
        if page.metrics.get("_record", {}).get("amphtml")
    }
    measured = set()
    unavailable = set()
    # Older captures may have no context row for an otherwise eligible page.
    # A measured sibling is not proof that those pages were checked too.
    for kind, codes in (("markup_evidence", A11Y_CODES), ("structured_evidence", SCHEMA_CODES)):
        missing = con.execute(
            "SELECT 1 FROM pages p LEFT JOIN documents d ON d.document_id=p.document_id "
            "LEFT JOIN context_items c ON c.kind=? AND c.item_key='page:'||p.url_id||':document:'||d.document_id||':representation:'||d.representation "
            "WHERE p.status_code BETWEEN 200 AND 299 AND lower(p.content_type) LIKE '%html%' "
            "AND c.item_key IS NULL LIMIT 1",
            (kind,),
        ).fetchone()
        if missing:
            unavailable.update(codes)
    # Static source markup owns AMP syntax; a serialized rendered DOM may repair
    # missing head/body tags and drop the original doctype.
    for row in con.execute(
        "SELECT u.url,c.payload_json FROM pages p JOIN urls u USING(url_id) "
        "LEFT JOIN documents d ON d.document_id=(SELECT MAX(newer.document_id) "
        "FROM documents newer WHERE newer.url_id=p.url_id AND newer.representation='static') "
        "LEFT JOIN context_items c ON c.kind='markup_evidence' "
        "AND c.item_key='page:'||d.url_id||':document:'||d.document_id||':representation:'||d.representation "
        "WHERE p.status_code BETWEEN 200 AND 299 AND lower(p.content_type) LIKE '%html%'"
    ):
        if row["payload_json"] is None:
            unavailable.update((*AMP_CODES, "AMP_MISSING_HTML_ATTRIBUTE"))
            continue
        payload = json.loads(row["payload_json"])
        facts = payload.get("facts")
        if payload["state"] == "unavailable" or not isinstance(facts, dict):
            unavailable.update((*AMP_CODES, "AMP_MISSING_HTML_ATTRIBUTE"))
            continue
        amp = facts["amp"]
        targeted = norm_url(row["url"]) in targets
        if not amp["declared"] and not targeted:
            continue
        measured.add("AMP_MISSING_HTML_ATTRIBUTE")
        provenance = {
            "document_id": payload["source_document_id"],
            "representation": "static",
            "method": "static_html_structure",
            "full_validator_state": amp["validator_state"],
            "full_validator_reason": amp["validator_reason"],
        }
        if not amp["declared"]:
            ctx.add("AMP_MISSING_HTML_ATTRIBUTE", target_url=row["url"], details=provenance)
            unavailable.update(AMP_CODES)
            continue
        measured.update(AMP_CODES)
        for error in amp["errors"]:
            ctx.add(error["code"], target_url=row["url"], details={**provenance, **error})

    for row in con.execute(
        "SELECT u.url,c.kind,c.payload_json FROM pages p JOIN urls u USING(url_id) "
        "JOIN documents d ON d.document_id=p.document_id "
        "JOIN context_items c ON c.item_key='page:'||d.url_id||':document:'||d.document_id||':representation:'||d.representation "
        "WHERE c.kind IN ('markup_evidence','structured_evidence') "
        "AND p.status_code BETWEEN 200 AND 299 AND lower(p.content_type) LIKE '%html%'"
    ):
        payload = json.loads(row["payload_json"])
        provenance = {
            "document_id": payload["source_document_id"],
            "representation": payload["representation"],
        }
        if row["kind"] == "markup_evidence":
            if payload["state"] == "unavailable":
                unavailable.update(A11Y_CODES)
                continue
            measured.update(A11Y_CODES)
            for issue in payload["facts"]["accessibility"]["issues"]:
                ctx.add(
                    issue["code"],
                    target_url=row["url"],
                    occurrences_count=issue["count"],
                    details={**provenance, "method": "static_html", **issue},
                )
        else:
            validation = payload.get("validation")
            if not isinstance(validation, dict):
                unavailable.update(SCHEMA_CODES)
                continue
            incomplete = payload["state"] in {"unavailable", "unsupported_context"}
            if incomplete:
                unavailable.update(SCHEMA_CODES)
            measured.update(SCHEMA_CODES[:2])
            for field, code in zip(
                ("entity_errors", "entity_warnings"), SCHEMA_CODES[:2], strict=True
            ):
                count = validation[field]
                # Scalar/empty JSON parses syntactically but defines no graph
                # entities. Its explicit structural failure is still a finding.
                if (
                    field == "entity_errors"
                    and not count
                    and payload["state"] == "structurally_invalid"
                ):
                    count = 1
                if count:
                    ctx.add(
                        code,
                        target_url=row["url"],
                        occurrences_count=count,
                        details={**provenance, "validation": validation},
                    )
            features = validation["feature_checks"]
            if incomplete:
                continue  # Required fields may be in unsupported RDFa graph carriers.
            if features:
                measured.add(SCHEMA_CODES[2])
            else:
                unavailable.add(SCHEMA_CODES[2])
            missing = [
                feature
                for feature in features
                if feature["local_requirements_state"] == "missing_required_properties"
            ]
            if missing:
                ctx.add(
                    SCHEMA_CODES[2],
                    target_url=row["url"],
                    details={
                        **provenance,
                        "features": missing,
                        "scope": "bundled local requirements; Google display is not guaranteed",
                    },
                )
    for code in all_codes:
        if code in unavailable or code not in measured:
            ctx.skip(
                code,
                "one or more native documents lack measured markup evidence"
                if code in unavailable
                else "no applicable document-bound native markup evidence",
            )
