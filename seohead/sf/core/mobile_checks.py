"""Mobile layout findings from explicitly captured native render evidence (#1016)."""

from __future__ import annotations

import json

from .context import AuditContext

CHECKS = (
    "MOBILE_CONTENT_OVERFLOW",
    "MOBILE_TEXT_TOO_SMALL",
    "MOBILE_TARGET_TOO_SMALL",
)


def check_mobile_render_layout(ctx: AuditContext) -> None:
    """Emit mobile findings only from retained, measured rendered-DOM geometry."""
    if ctx.scan_con is None:
        for check_id in CHECKS:
            ctx.skip(check_id, "requires retained native rendered mobile layout evidence")
        return
    con = ctx.scan_con
    rows = con.execute(
        "SELECT u.url,d.renderer_json FROM pages p JOIN urls u ON u.url_id=p.url_id "
        "JOIN documents d ON d.document_id=p.document_id "
        "WHERE p.representation='rendered' AND d.renderer_json IS NOT NULL ORDER BY u.url"
    )
    measured = 0
    partial = 0
    unavailable = 0
    for url, raw in rows:
        try:
            renderer = json.loads(raw)
            evidence = renderer.get("mobile_layout")
        except (TypeError, ValueError, AttributeError):
            evidence = None
        if not isinstance(evidence, dict):
            continue
        if evidence.get("state") not in {"complete", "partial"}:
            unavailable += 1
            continue
        measured += 1
        partial += evidence.get("state") == "partial"
        if evidence.get("content_overflow") is True:
            ctx.add(
                "MOBILE_CONTENT_OVERFLOW",
                target_url=url,
                details={
                    "overflow_px": evidence.get("content_overflow_px"),
                    "viewport_width_px": evidence.get("viewport_width_px"),
                    "document_width_px": evidence.get("document_width_px"),
                    "elements": evidence.get("details", {}).get("overflow_elements", []),
                },
            )
        if (evidence.get("small_text_count_lower_bound") or 0) > 0:
            ctx.add(
                "MOBILE_TEXT_TOO_SMALL",
                target_url=url,
                details={
                    "count_lower_bound": evidence["small_text_count_lower_bound"],
                    "threshold_px": evidence.get("thresholds", {}).get("small_text_below_px"),
                    "elements": evidence.get("details", {}).get("small_text_elements", []),
                },
            )
        if (evidence.get("small_target_count_lower_bound") or 0) > 0:
            ctx.add(
                "MOBILE_TARGET_TOO_SMALL",
                target_url=url,
                details={
                    "count_lower_bound": evidence["small_target_count_lower_bound"],
                    "threshold_px": evidence.get("thresholds", {}).get("small_target_below_px"),
                    "elements": evidence.get("details", {}).get("small_target_elements", []),
                },
            )
    reason = (
        "mobile layout was not requested for any rendered page"
        if not measured and not unavailable
        else "rendered mobile layout evidence was unavailable"
        if not measured
        else "one or more mobile measurements were partial or unavailable"
        if partial or unavailable
        else ""
    )
    if reason:
        for check_id in CHECKS:
            ctx.skip(check_id, reason)
