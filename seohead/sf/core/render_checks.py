"""Raw-versus-rendered findings from retained browser captures."""

from __future__ import annotations

import json

from .context import AuditContext

RULE_CHECKS = {
    "noindex_nofollow_raw_only": ("NOINDEX_ONLY_IN_RAW_HTML", "NOFOLLOW_ONLY_IN_RAW_HTML"),
    "canonical_mismatch": ("CANONICAL_MISMATCH",),
    "canonical_only_after_render": ("CANONICAL_ONLY_IN_RENDERED_HTML",),
}
FIELD_CHECKS = {
    ("title", "rendered_only"): "PAGE_TITLE_ONLY_IN_RENDERED_HTML",
    ("title", "changed"): "PAGE_TITLE_UPDATED_BY_JAVASCRIPT",
    ("description", "rendered_only"): "META_DESCRIPTION_ONLY_IN_RENDERED_HTML",
    ("description", "changed"): "META_DESCRIPTION_UPDATED_BY_JAVASCRIPT",
    ("h1", "rendered_only"): "H1_ONLY_IN_RENDERED_HTML",
    ("h1", "changed"): "H1_UPDATED_BY_JAVASCRIPT",
}
ELEMENT_CHECKS = tuple(
    dict.fromkeys(
        [item for values in RULE_CHECKS.values() for item in values]
        + list(FIELD_CHECKS.values())
    )
)
CHECKS = (*ELEMENT_CHECKS, "RENDER_RESOURCE_BLOCKED")


def _element_findings(ctx: AuditContext, url: str, evidence: dict) -> bool:
    """Emit element differences; return whether the retained contract was malformed."""
    malformed = False
    for entry in evidence.get("rules", []):
        if not isinstance(entry, dict) or not isinstance(entry.get("diff"), dict):
            malformed = True
            continue
        diff = entry["diff"]
        rule = entry.get("rule")
        field = diff.get("field")
        kind = diff.get("kind")
        check_ids = list(RULE_CHECKS.get(rule, ()))
        if rule == "noindex_nofollow_raw_only":
            directives = str(diff.get("raw") or "").lower().replace(",", " ").split()
            check_ids = [
                code
                for token, code in (
                    ("noindex", "NOINDEX_ONLY_IN_RAW_HTML"),
                    ("nofollow", "NOFOLLOW_ONLY_IN_RAW_HTML"),
                )
                if token in directives
            ]
        field_check = FIELD_CHECKS.get((field, kind))
        if field_check:
            check_ids.append(field_check)
        for check_id in check_ids:
            ctx.add(
                check_id,
                target_url=url,
                details={
                    "field": field,
                    "raw": diff.get("raw"),
                    "rendered": diff.get("rendered"),
                },
            )
    return malformed


def _network_findings(ctx: AuditContext, url: str, evidence: dict) -> bool:
    """Emit network errors; return whether the retained bounded log was partial."""
    partial = evidence.get("state") == "partial"
    for resource in evidence.get("resources", []):
        if not isinstance(resource, dict) or not isinstance(resource.get("url"), str):
            partial = True
            continue
        ctx.add(
            "RENDER_RESOURCE_BLOCKED",
            target_url=resource["url"],
            details={
                "source_page": url,
                "resource_type": resource.get("resource_type"),
                "reason": resource.get("reason"),
                "status_code": resource.get("status_code"),
            },
        )
    return partial


def check_rendered_elements(ctx: AuditContext) -> None:
    """Emit differences and blocked requests only from retained rendered evidence."""
    if ctx.scan_con is None:
        for check_id in CHECKS:
            ctx.skip(check_id, "requires retained native rendered evidence")
        return
    rows = ctx.scan_con.execute(
        "SELECT u.url,d.renderer_json FROM pages p JOIN urls u ON u.url_id=p.url_id "
        "JOIN documents d ON d.document_id=p.document_id "
        "WHERE p.representation='rendered' AND d.renderer_json IS NOT NULL ORDER BY u.url"
    )
    element_measured = 0
    element_unavailable = 0
    element_partial = False
    network_measured = 0
    network_unavailable = 0
    network_partial = False
    for url, raw_json in rows:
        try:
            renderer = json.loads(raw_json)
            elements = renderer.get("raw_rendered_evidence")
            network = renderer.get("network_evidence")
        except (TypeError, ValueError, AttributeError):
            elements = None
            network = None
        if isinstance(elements, dict) and elements.get("state") == "complete":
            element_measured += 1
            element_partial |= _element_findings(ctx, url, elements)
        else:
            element_unavailable += 1
        if isinstance(network, dict) and network.get("state") in {"complete", "partial"}:
            network_measured += 1
            network_partial |= _network_findings(ctx, url, network)
        else:
            network_unavailable += 1
    if not element_measured:
        reason = (
            "no retained raw-versus-rendered element comparisons were available"
            if not element_unavailable
            else "retained raw-versus-rendered element comparisons were unavailable"
        )
        for check_id in ELEMENT_CHECKS:
            ctx.skip(check_id, reason)
    elif element_unavailable or element_partial:
        for check_id in ELEMENT_CHECKS:
            ctx.skip(check_id, "one or more raw-versus-rendered comparisons were partial or unavailable")
    if not network_measured:
        ctx.skip("RENDER_RESOURCE_BLOCKED", "no retained rendered network log was available")
    elif network_unavailable or network_partial:
        ctx.skip("RENDER_RESOURCE_BLOCKED", "one or more rendered network logs were partial or unavailable")
