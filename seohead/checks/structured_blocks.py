"""Per-block JSON-LD facts for one HTML document: position, status, graph findings, source.

The stored structured-evidence context keeps only hashes, so it cannot answer
"which block is broken, on which line, and what does it say". This module reads
one retained body and reports every live ``application/ld+json`` block the same
way the parser and the Schema.org graph check see it, without changing either.
"""

from __future__ import annotations

import json
from collections.abc import Mapping
from typing import Any

from bs4 import BeautifulSoup

from seohead.checks import schema_org
from seohead.checks.parser import document_base_url, live_jsonld_tags

# Bounds keep one page's answer small enough for an MCP response.
SOURCE_CHARS = 20_000
EXCERPT_CHARS = 200
VALUE_CHARS = 200
MAX_PROPERTIES = 100
MAX_BLOCKS = 25
MAX_MESSAGES = 100


def _preview(value: Any) -> str:
    text = value if isinstance(value, str) else json.dumps(value, ensure_ascii=False)
    return text if len(text) <= VALUE_CHARS else text[:VALUE_CHARS] + "…"


def _properties(node: Any, path: str, out: list[dict[str, Any]]) -> None:
    """Flatten a parsed block into dotted paths (``offers.price``, ``@graph[0].name``)."""
    if len(out) >= MAX_PROPERTIES:
        return
    if isinstance(node, dict):
        for key, value in node.items():
            if len(out) >= MAX_PROPERTIES:
                break
            child = f"{path}.{key}" if path else key
            if isinstance(value, (dict, list)) and value:
                _properties(value, child, out)
            else:
                out.append({"path": child, "value": _preview(value)})
    elif isinstance(node, list):
        for index, value in enumerate(node):
            if len(out) >= MAX_PROPERTIES:
                break
            child = f"{path}[{index}]"
            if isinstance(value, (dict, list)) and value:
                _properties(value, child, out)
            else:
                out.append({"path": child, "value": _preview(value)})
    else:
        out.append({"path": path, "value": _preview(node)})


def _unique_types(nodes: list[dict[str, Any]]) -> list[str]:
    found: list[str] = []
    for node in nodes:
        for name in schema_org._types_of(node):
            if name not in found:
                found.append(name)
    return found


def structured_blocks(
    html: str | None,
    *,
    base_url: str = "",
    checked_page: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    """Describe every live JSON-LD block of one document, in source order.

    ``state`` is ``absent`` when there are no blocks, ``malformed`` when any block
    fails to parse, otherwise the worst graph state: ``errors``, ``warnings`` or
    ``valid``. A block that parsed is still judged only by the bundled Schema.org
    graph check, which is offline and does not promise Google display.
    """
    if not isinstance(html, str):
        return {"state": "unavailable", "reason": "document body is unavailable", "blocks": []}
    # html.parser is the only bundled builder that records source lines; the
    # script and template handling used here is the same in both builders.
    soup = BeautifulSoup(html, features="html.parser")
    resolved_base = document_base_url(soup, base_url)
    parsed: list[dict[str, Any]] = []
    tags = live_jsonld_tags(soup)
    for ordinal, tag in enumerate(tags[:MAX_BLOCKS]):
        raw = (tag.string or tag.get_text() or "").strip()
        entry: dict[str, Any] = {
            "ordinal": ordinal,
            "line": tag.sourceline,
            "column": tag.sourcepos,
            "state": "parse_error",
            "types": [],
            "errors": 0,
            "warnings": 0,
            "error": None,
            "messages": [],
            "message_count": 0,
            "messages_truncated": False,
            "properties": [],
            "property_limit_reached": False,
            "source_json": None,
            "parsed_json": None,
            "source_truncated": False,
            "raw_excerpt": raw[:EXCERPT_CHARS],
        }
        nodes: list[dict[str, Any]] = []
        if not raw:
            entry["error"] = "block is empty"
        else:
            try:
                block = json.loads(raw)
            except (ValueError, TypeError) as exc:
                entry["error"] = str(exc)
            else:
                # Retain the actual JSON tree before the validator adds private
                # graph annotations to its independent flattened nodes.
                entry["parsed_json"] = block if len(raw) <= SOURCE_CHARS else None
                context, supported = schema_org._block_vocab(block)
                normalized = schema_org.normalize_jsonld_ids(block, resolved_base)
                schema_org._flatten(normalized, nodes)
                for node in nodes:
                    node.setdefault("_vocab_context", context)
                    node.setdefault("_vocab_supported", supported)
                entry["types"] = _unique_types(nodes)
                _properties(block, "", entry["properties"])
                entry["property_limit_reached"] = len(entry["properties"]) >= MAX_PROPERTIES
                source = json.dumps(block, ensure_ascii=False, indent=2)
                entry["source_truncated"] = len(source) > SOURCE_CHARS
                entry["source_json"] = source[:SOURCE_CHARS]
                entry["state"] = "valid"
                if not nodes:
                    entry["errors"] = 1
                    entry["messages"] = [
                        {
                            "path": "@root",
                            "level": "error",
                            "text": "Parsed JSON-LD block contains no entity definitions",
                        }
                    ]
        parsed.append(entry)

    # The caller may supply the one page-wide Schema.org result used by a
    # retained-detail endpoint. Standalone callers still get the same offline check.
    checked_page = (
        checked_page
        if isinstance(checked_page, Mapping)
        else schema_org.check_schema(html=html, document_base_url=resolved_base)
    )
    checked_entities = checked_page.get("entities") or []
    for entry in parsed:
        prefix = f"jsonld[{entry['ordinal'] + 1}]"
        for checked in checked_entities:
            if (
                not isinstance(checked, Mapping)
                or checked.get("format") != "jsonld"
                or not isinstance(checked.get("path"), str)
            ):
                continue
            path = checked["path"]
            if not (path == prefix or path.startswith((prefix + ".", prefix + "@", prefix + "["))):
                continue
            errors = checked.get("errors") or []
            warnings = checked.get("warnings") or []
            entry["errors"] += len(errors)
            entry["warnings"] += len(warnings)
            entry["message_count"] += len(errors) + len(warnings)
            entry["messages"] += [
                {"path": path[:512], "level": "error", "text": str(text)[:500]} for text in errors
            ] + [
                {"path": path[:512], "level": "warning", "text": str(text)[:500]}
                for text in warnings
            ]
            entry["messages_truncated"] = entry["message_count"] > MAX_MESSAGES
            entry["messages"] = entry["messages"][:MAX_MESSAGES]
        if entry["state"] == "valid" and entry["errors"]:
            entry["state"] = "errors"
        elif entry["state"] == "valid" and entry["warnings"]:
            entry["state"] = "warnings"

    if not parsed and not tags:
        state = "absent"
    elif any(entry["state"] == "parse_error" for entry in parsed):
        state = "malformed"
    elif any(entry["state"] == "errors" for entry in parsed):
        state = "errors"
    elif any(entry["state"] == "warnings" for entry in parsed):
        state = "warnings"
    else:
        state = "valid"
    truncated = len(tags) > MAX_BLOCKS
    reason = f"Showing the first {MAX_BLOCKS} of {len(tags)} JSON-LD blocks." if truncated else ""
    return {
        "state": state,
        "reason": reason,
        "block_count": len(tags),
        "blocks_truncated": truncated,
        "parse_error_count": sum(entry["state"] == "parse_error" for entry in parsed),
        "rules_version": schema_org.RICH_RESULTS_RULES_VERSION,
        "blocks": parsed,
    }
