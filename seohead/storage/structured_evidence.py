"""Versioned structured-data and language-declaration evidence from one document."""

from __future__ import annotations

import hashlib
import json
import re
from typing import Any

from bs4 import BeautifulSoup

from . import ScanError

OUTER_VERSION = "scan_context.v1"
STRUCTURED_KIND = "structured_evidence"
LANGUAGE_KIND = "language_evidence"
STRUCTURED_VERSION = "structured_evidence.v2"
STRUCTURED_LEGACY_VERSION = "structured_evidence.v1"
LANGUAGE_VERSION = "language_evidence.v1"
REPRESENTATIONS = {"static", "rendered", "legacy_fragment"}


def _hash(value: Any) -> str:
    return hashlib.sha256(
        json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    ).hexdigest()


def _nodes(value: Any) -> list[dict[str, Any]]:
    stack, result = [value], []
    while stack:
        current = stack.pop()
        if isinstance(current, dict):
            result.append(current)
            stack.extend(reversed(list(current.values())))
        elif isinstance(current, list):
            stack.extend(reversed(current))
    return result


def _has_schema_rdfa(soup: BeautifulSoup) -> bool:
    """Recognize Schema.org RDFa properties as retained structured markup."""
    from seohead.checks.parser import is_inert_template_content
    from seohead.checks.schema_org import MAX_MARKUP_NODES

    def schema_namespace(value: str) -> bool:
        return bool(re.match(r"^https?://(?:www\.)?schema\.org(?:/|$)", value.strip(), re.I))

    for tag in soup.find_all(True, limit=MAX_MARKUP_NODES):
        if not any(tag.has_attr(attribute) for attribute in ("typeof", "property", "rel", "rev")):
            continue
        if is_inert_template_content(tag):
            continue
        if tag.has_attr("typeof"):
            return True
        terms = []
        for attribute in ("property", "rel", "rev"):
            value = tag.get(attribute) or []
            terms.extend(value if isinstance(value, list) else str(value).split())
        if any(term.startswith("schema:") or schema_namespace(term) for term in terms):
            return True
        ancestors = [tag, *tag.parents]
        if any(
            node.has_attr("vocab") and schema_namespace(str(node.get("vocab") or ""))
            for node in ancestors
        ):
            return True
        prefix_text = " ".join(
            str(node.get("prefix") or "") for node in ancestors if hasattr(node, "get")
        )
        prefix_map = {
            name: namespace for name, namespace in re.findall(r"(\w[\w-]*):\s+(\S+)", prefix_text)
        }
        if any(
            ":" in term and schema_namespace(prefix_map.get(term.split(":", 1)[0], ""))
            for term in terms
        ):
            return True
    return False


def _validation(html: str | None, base_url: str | None = None) -> dict[str, Any]:
    """Run only bundled, offline graph checks; do not promise Google display."""
    base = {
        "syntax_state": "unavailable",
        "structural_state": "unavailable",
        "feature_eligibility_state": "unknown",
        "unsupported_context_normalization": False,
        "entity_errors": 0,
        "entity_warnings": 0,
        "feature_checks": [],
    }
    if not isinstance(html, str):
        return base
    from seohead.checks.schema_org import check_schema

    checked = check_schema(html=html, document_base_url=base_url)
    parse_errors = checked.get("parse_errors") or []
    entities = checked.get("entities") or []
    vocabularies = checked.get("vocabularies") or []
    unsupported = any(
        isinstance(item, dict) and item.get("supported") is False for item in vocabularies
    )
    partial_markup = any(
        isinstance(item, dict) and item.get("status") == "partial"
        for item in checked.get("markup_coverage") or []
    )
    entity_errors = sum(
        len(item.get("errors") or []) for item in entities if isinstance(item, dict)
    )
    entity_warnings = sum(
        len(item.get("warnings") or []) for item in entities if isinstance(item, dict)
    )
    rich_results = checked.get("rich_results") or []
    feature_checks = []
    for item in rich_results:
        if not isinstance(item, dict):
            continue
        feature_checks.append(
            {
                "type": str(item.get("type") or ""),
                "feature": str(item.get("google_feature") or ""),
                "local_requirements_state": (
                    "met" if item.get("eligible") else "missing_required_properties"
                ),
                "missing_required": list(item.get("missing_required") or []),
                "missing_required_any_of": list(item.get("missing_required_any_of") or []),
                "rules_version": str(item.get("rules_version") or ""),
            }
        )
    structural_state = (
        "malformed"
        if parse_errors
        else "unsupported_context"
        if unsupported
        else "unavailable"
        if partial_markup
        else "structurally_invalid"
        if not entities or entity_errors
        else "valid"
    )
    return {
        "syntax_state": "malformed" if parse_errors else "parsed",
        "structural_state": structural_state,
        "feature_eligibility_state": "locally_checked" if feature_checks else "unknown",
        "unsupported_context_normalization": unsupported,
        "entity_errors": entity_errors,
        "entity_warnings": entity_warnings,
        "feature_checks": feature_checks,
    }


def structured_payload(
    *,
    page_url_id: int,
    source_document_id: int,
    representation: str,
    parsed: dict[str, Any] | None,
    html: str | None = None,
) -> dict[str, Any]:
    """Capture syntax, graph and local feature-check states without body excerpts."""
    if (
        type(page_url_id) is not int
        or type(source_document_id) is not int
        or representation not in REPRESENTATIONS
    ):
        raise ValueError("structured evidence identity is invalid")
    if not isinstance(parsed, dict):
        return {
            "schema_version": STRUCTURED_VERSION,
            "page_url_id": page_url_id,
            "source_document_id": source_document_id,
            "representation": representation,
            "state": "unavailable",
            "reason": "captured document was not parsed",
            "blocks": [],
            "nodes": [],
            "edges": [],
            "validation": _validation(None),
        }
    valid = list(parsed.get("jsonld") or [])
    invalid = list(parsed.get("jsonld_invalid") or [])
    blocks, nodes, edges = [], [], []
    for ordinal, block in enumerate(valid):
        blocks.append(
            {
                "ordinal": ordinal,
                "parser_ordinal": ordinal,
                "state": "parsed",
                "hash": _hash(block),
            }
        )
        for node_ordinal, node in enumerate(_nodes(block)):
            node_id = node.get("@id") if isinstance(node.get("@id"), str) else None
            types = node.get("@type")
            type_values = (
                [types]
                if isinstance(types, str)
                else sorted(t for t in types if isinstance(t, str))
                if isinstance(types, list)
                else []
            )
            nodes.append(
                {
                    "block_ordinal": ordinal,
                    "ordinal": node_ordinal,
                    "id": node_id,
                    "types": type_values,
                }
            )
            for property_name, value in node.items():
                values = value if isinstance(value, list) else [value]
                for target in values:
                    if isinstance(target, dict) and isinstance(target.get("@id"), str):
                        edges.append(
                            {
                                "block_ordinal": ordinal,
                                "source_id": node_id,
                                "property": property_name,
                                "target_id": target["@id"],
                            }
                        )
    for offset, item in enumerate(invalid, start=len(valid)):
        source_ordinal = item.get("index") if isinstance(item, dict) else None
        blocks.append(
            {
                "ordinal": offset,
                "parser_ordinal": source_ordinal if type(source_ordinal) is int else None,
                "state": "malformed",
                "hash": _hash(item),
            }
        )
    validation = _validation(html, parsed.get("document_base_url"))
    if invalid:
        validation["syntax_state"] = "malformed"
    # Non-JSON-LD formats are validated by the same bundled vocabulary. They
    # must not read as absent merely because the JSON-LD block list is empty.
    non_json_markup = False
    from seohead.checks.schema_org import MAX_HTML_BYTES

    if isinstance(html, str) and len(html.encode("utf-8", errors="replace")) <= MAX_HTML_BYTES:
        soup = BeautifulSoup(html, features="lxml")
        from seohead.checks.parser import is_inert_template_content

        non_json_markup = any(
            not is_inert_template_content(tag)
            for tag in soup.find_all(attrs={"itemscope": True})
            + soup.find_all(attrs={"typeof": True})
        ) or _has_schema_rdfa(soup)
    state = (
        "malformed"
        if invalid
        else validation["structural_state"]
        if validation["structural_state"] in {"unavailable", "unsupported_context"}
        else "absent"
        if not valid and not non_json_markup
        else validation["structural_state"]
    )
    reasons = {
        "malformed": "JSON-LD syntax failures captured",
        "structurally_invalid": "Schema.org graph validation found structural errors",
        "unsupported_context": "one or more JSON-LD contexts could not be normalized as Schema.org",
        "unavailable": "one or more markup constructs require unsupported structural normalization"
        if isinstance(html, str)
        else "captured HTML was unavailable for structural validation",
        "absent": "",
        "valid": "",
    }
    return {
        "schema_version": STRUCTURED_VERSION,
        "page_url_id": page_url_id,
        "source_document_id": source_document_id,
        "representation": representation,
        "state": state,
        "reason": reasons.get(state, "structural validation is unavailable"),
        "blocks": blocks,
        "nodes": nodes,
        "edges": edges,
        "validation": validation,
    }


def language_payload(
    *,
    page_url_id: int,
    source_document_id: int,
    representation: str,
    parsed: dict[str, Any] | None,
    html: str | None,
) -> dict[str, Any]:
    """Capture raw/resolved hreflang declarations as directed source evidence."""
    if (
        type(page_url_id) is not int
        or type(source_document_id) is not int
        or representation not in REPRESENTATIONS
    ):
        raise ValueError("language evidence identity is invalid")
    if not isinstance(parsed, dict):
        return {
            "schema_version": LANGUAGE_VERSION,
            "page_url_id": page_url_id,
            "source_document_id": source_document_id,
            "representation": representation,
            "state": "unavailable",
            "reason": "captured document was not parsed",
            "html_lang": None,
            "declarations": [],
        }
    declarations = []
    for ordinal, alternate in enumerate(parsed.get("hreflang") or []):
        if not isinstance(alternate, dict):
            continue
        declarations.append(
            {
                "ordinal": ordinal,
                "lang": str(alternate.get("lang") or ""),
                "raw_href": str(alternate.get("raw_href") or ""),
                "target": str(alternate.get("url") or ""),
                "state": "declared" if alternate.get("url") else "malformed",
            }
        )
    return {
        "schema_version": LANGUAGE_VERSION,
        "page_url_id": page_url_id,
        "source_document_id": source_document_id,
        "representation": representation,
        "state": "declared" if declarations else "absent",
        "reason": "",
        "html_lang": (
            str(BeautifulSoup(html, features="lxml").find("html").get("lang") or "")
            if isinstance(html, str)
            and BeautifulSoup(html, features="lxml").find("html") is not None
            else None
        ),
        "declarations": declarations,
    }


def context_items(structured: dict[str, Any], language: dict[str, Any]) -> list[dict[str, Any]]:
    """Return closed context envelopes for the existing atomic writer hook."""
    return [_item(STRUCTURED_KIND, structured), _item(LANGUAGE_KIND, language)]


def _item(kind: str, payload: dict[str, Any]) -> dict[str, Any]:
    return {
        "kind": kind,
        "item_key": f"page:{payload['page_url_id']}:document:{payload['source_document_id']}:representation:{payload['representation']}",
        "payload_version": OUTER_VERSION,
        "payload_json": json.dumps(payload, sort_keys=True, separators=(",", ":")),
        "completeness": "unavailable" if payload["state"] == "unavailable" else "complete",
        "reason": payload["reason"],
    }


def validate_context(con: Any, item: dict[str, Any], payload: Any) -> None:
    """Validate closed payload shape and exact document/page/representation binding."""
    kind = item.get("kind")
    version = (
        STRUCTURED_VERSION
        if kind == STRUCTURED_KIND
        else LANGUAGE_VERSION
        if kind == LANGUAGE_KIND
        else None
    )
    if (
        version is None
        or item.get("payload_version") != OUTER_VERSION
        or not isinstance(payload, dict)
    ):
        raise ScanError("structured or language evidence context is invalid")
    required = {
        "schema_version",
        "page_url_id",
        "source_document_id",
        "representation",
        "state",
        "reason",
    }
    if kind == STRUCTURED_KIND:
        required |= {"blocks", "nodes", "edges"}
        if payload.get("schema_version") == STRUCTURED_VERSION:
            required.add("validation")
        elif payload.get("schema_version") != STRUCTURED_LEGACY_VERSION:
            raise ScanError("structured evidence payload has unsupported version")
    else:
        required |= {"html_lang", "declarations"}
    if set(payload) != required or (
        kind != STRUCTURED_KIND and payload["schema_version"] != version
    ):
        raise ScanError("structured or language evidence payload has unsupported fields")
    if (
        type(payload["page_url_id"]) is not int
        or type(payload["source_document_id"]) is not int
        or payload["representation"] not in REPRESENTATIONS
    ):
        raise ScanError("structured or language evidence identity is invalid")
    if payload["state"] not in {
        "eligible",
        "valid",
        "malformed",
        "structurally_invalid",
        "unsupported_context",
        "absent",
        "declared",
        "unavailable",
    } or not isinstance(payload["reason"], str):
        raise ScanError("structured or language evidence state is invalid")
    if kind == STRUCTURED_KIND and payload["schema_version"] == STRUCTURED_VERSION:
        validation = payload["validation"]
        if (
            not isinstance(validation, dict)
            or set(validation)
            != {
                "syntax_state",
                "structural_state",
                "feature_eligibility_state",
                "unsupported_context_normalization",
                "entity_errors",
                "entity_warnings",
                "feature_checks",
            }
            or validation["syntax_state"] not in {"parsed", "malformed", "unavailable"}
            or validation["structural_state"]
            not in {
                "valid",
                "malformed",
                "structurally_invalid",
                "unsupported_context",
                "unavailable",
            }
            or validation["feature_eligibility_state"] not in {"locally_checked", "unknown"}
            or type(validation["unsupported_context_normalization"]) is not bool
            or type(validation["entity_errors"]) is not int
            or type(validation["entity_warnings"]) is not int
            or not isinstance(validation["feature_checks"], list)
        ):
            raise ScanError("structured evidence validation state is invalid")
    if (
        item["item_key"]
        != f"page:{payload['page_url_id']}:document:{payload['source_document_id']}:representation:{payload['representation']}"
    ):
        raise ScanError("structured or language evidence key is invalid")
    if not con.execute(
        "SELECT 1 FROM documents WHERE document_id=? AND url_id=? AND representation=?",
        (payload["source_document_id"], payload["page_url_id"], payload["representation"]),
    ).fetchone():
        raise ScanError("structured or language evidence binds the wrong document")


def read(con: Any, *, streaming: bool = False) -> dict[str, Any]:
    """Read typed evidence as lists or bounded re-iterable retained collections."""
    from .content_evidence import ContextRows

    output = {
        "structured": ContextRows(con, STRUCTURED_KIND),
        "language": ContextRows(con, LANGUAGE_KIND),
    }
    return output if streaming else {key: list(rows) for key, rows in output.items()}
