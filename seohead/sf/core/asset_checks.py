"""Asset findings from retained native evidence or supplied per-URL Lighthouse JSON.

No collection occurs here. SQLite cursors bound memory independently of page count;
Lighthouse imports have explicit file/count limits and never include local paths.
"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
from typing import Any

from seohead.checks.lab_coverage import UNUSED_MIN_CHARS, UNUSED_RATIO_THRESHOLD
from seohead.storage import ScanError, _loads
from seohead.storage.resources import media_matches

from .context import AuditContext

TRACE_CHECKS = {
    "LCP_DISCOVERY": "lcp-discovery-insight",
    "NETWORK_DEPENDENCY_TREE": "network-dependency-tree-insight",
    "LAYOUT_SHIFT_CULPRITS": "cls-culprits-insight",
    "IMAGE_DELIVERY": "image-delivery-insight",
    "FORCED_REFLOW": "forced-reflow-insight",
    "JS_EXECUTION_TIME": "bootup-time",
}
RESOURCE_CHECKS = (
    "HEAVY_PAGE_WEIGHT",
    "RESOURCE_OVER_2MB",
    "ASSET_SHORT_CACHE",
    "FONT_DISPLAY_MISSING",
    "UNMINIFIED_CSS_JS",
    "DUPLICATED_JS",
    "LEGACY_JS",
)
RENDER_CHECKS = ("IMG_INTRINSIC_OVERSIZED", "IMG_UPSCALED", "UNUSED_CSS_JS")
MAX_LIGHTHOUSE_BYTES = 8 * 1024 * 1024
MAX_LIGHTHOUSE_REPORTS = 2000


def _read_json(value: Any) -> dict[str, Any] | None:
    try:
        result = json.loads(value) if isinstance(value, str) else value
    except (ValueError, TypeError, RecursionError):
        return None
    return result if isinstance(result, dict) else None


_HTML_WHERE = "p.status_code BETWEEN 200 AND 299 AND lower(p.content_type) LIKE '%html%'"
_SOURCE_KINDS = {"script", "stylesheet"}
_RAW_DOCUMENT_JOIN = (
    " LEFT JOIN (SELECT d.url_id,MAX(d.document_id) AS document_id FROM documents d "
    "JOIN context_items c ON c.kind='resource_inventory' AND c.item_key='document:'||d.document_id "
    "WHERE d.representation='static' GROUP BY d.url_id) raw "
    "ON raw.url_id=p.url_id AND p.representation!='static' "
)
_ACTIVE_DOCUMENT_WHERE = "(d.document_id=p.document_id OR d.document_id=raw.document_id)"


def _pages(ctx: AuditContext):
    """Stream every eligible page, including ones with no renderer document."""
    if ctx.scan_con is not None:
        yield from ctx.scan_con.execute(
            "SELECT u.url,CASE WHEN p.representation='static' THEN p.size_bytes ELSE b.decoded_bytes END,"
            "d.representation,d.renderer_json FROM pages p JOIN urls u ON u.url_id=p.url_id "
            "LEFT JOIN documents d ON d.document_id=p.document_id "
            + _RAW_DOCUMENT_JOIN
            + "LEFT JOIN documents sd ON sd.document_id=raw.document_id LEFT JOIN bodies b ON b.sha256=sd.body_sha256 "
            "WHERE " + _HTML_WHERE
        )
    else:
        for page in ctx.html_pages():
            yield page.url, None, None, None


def _eligible_pages(ctx: AuditContext) -> int:
    if ctx.scan_con is not None:
        return ctx.scan_con.execute("SELECT COUNT(*) FROM pages p WHERE " + _HTML_WHERE).fetchone()[
            0
        ]
    return len(ctx.html_pages())


def _measurement(ctx, code, unit, eligible, measured, partial, reason):
    unavailable = eligible - measured if eligible is not None else None
    state = (
        "unavailable"
        if not measured and (eligible is None or eligible > 0)
        else "partial"
        if partial or unavailable is None or unavailable > 0
        else "complete"
    )
    ctx.asset_measurements[code] = {
        "unit": unit,
        "eligible": eligible,
        "measured": measured,
        "partial": partial,
        "unavailable": unavailable,
        "state": state,
        "reason": reason if state != "complete" else "",
    }
    if state != "complete":
        # skip refuses fired checks; the scalar summary still preserves their partial scope.
        ctx.skip(code, reason)


def _asset_evidence(representation, renderer_json):
    if representation != "rendered":
        return {}
    settings = _read_json((_read_json(renderer_json) or {}).get("settings")) or {}
    return _read_json(settings.get("asset_evidence")) or {}


def check_lighthouse_reports(ctx: AuditContext) -> None:
    eligible = _eligible_pages(ctx)
    reports = ctx.config.get("input", {}).get("lighthouse_reports", {})
    measured = dict.fromkeys(TRACE_CHECKS, 0)
    reasons = set()
    if not isinstance(reports, dict) or len(reports) > MAX_LIGHTHOUSE_REPORTS:
        reasons.add(
            "per-URL Lighthouse input exceeds the 2000-report bound or is not a URL/path object"
        )
        reports = {}
    for url, filename in reports.items():
        page = ctx.page_by_url.get(url)
        if page is None or not page.is_html or not page.is_2xx:
            reasons.add("Lighthouse input names a URL absent from the eligible crawl pages")
            continue
        try:
            with Path(filename).open("rb") as f:
                raw = f.read(MAX_LIGHTHOUSE_BYTES + 1)
            if len(raw) > MAX_LIGHTHOUSE_BYTES:
                raise ValueError("report exceeds byte budget")
            report = _loads(raw, "supplied Lighthouse report")
            if (
                not isinstance(report, dict)
                or report.get("finalUrl") != url
                or report.get("runtimeError")
            ):
                raise ValueError("Lighthouse finalUrl does not match the crawl URL")
            audits = report["audits"]
            if not isinstance(audits, dict):
                raise ValueError("audits are unavailable")
        except (OSError, ValueError, KeyError, TypeError, ScanError):
            reasons.add(
                "supplied Lighthouse report is unreadable, oversized, malformed or belongs to another URL"
            )
            continue
        for check, ident in TRACE_CHECKS.items():
            audit = audits.get(ident)
            if not isinstance(audit, dict):
                reasons.add(f"supplied Lighthouse report has no {ident} result")
                continue
            score = audit.get("score")
            if type(score) not in {int, float} or not 0 <= score <= 1 or audit.get("errorMessage"):
                reasons.add(f"Lighthouse {ident} was not measured")
                continue
            if score < 1:
                details = audit.get("details")
                try:
                    encoded = json.dumps(details, ensure_ascii=True, allow_nan=False)
                except (ValueError, TypeError, RecursionError):
                    reasons.add(
                        f"Lighthouse {ident} details could not be retained within this runtime"
                    )
                    continue
                ctx.add(
                    check,
                    target_url=url,
                    status_code=page.status_code,
                    details={
                        "source": "supplied-lighthouse",
                        "audit_id": ident,
                        "score": score,
                        "lighthouse_version": value
                        if isinstance(value := report.get("lighthouseVersion"), str)
                        and len(value) <= 128
                        else None,
                        "fetch_time": value
                        if isinstance(value := report.get("fetchTime"), str) and len(value) <= 128
                        else None,
                        "input_sha256": hashlib.sha256(raw).hexdigest(),
                        "details": details if len(encoded) <= 64 * 1024 else None,
                        "details_omitted": len(encoded) > 64 * 1024,
                        "reason": "the supplied Lighthouse lab audit reported an optimization opportunity",
                    },
                )
            measured[check] += 1
    reason = "; ".join(sorted(reasons)) or (
        "not fully measured by this native crawl: supply input.lighthouse_reports with matching per-URL Lighthouse JSON for every eligible HTML page; no trace was collected for missing reports"
    )
    for check in TRACE_CHECKS:
        _measurement(ctx, check, "pages", eligible, measured[check], 0, reason)


def check_render_assets(ctx: AuditContext) -> None:
    measured = dict.fromkeys(RENDER_CHECKS, 0)
    partial = dict.fromkeys(RENDER_CHECKS, 0)
    eligible = 0
    for url, _, representation, renderer_json in _pages(ctx):
        eligible += 1
        evidence = _asset_evidence(representation, renderer_json)
        layout = _read_json(evidence.get("layout")) or {}
        if layout.get("state") in {"complete", "partial"}:
            for code in RENDER_CHECKS[:2]:
                measured[code] += 1
                partial[code] += layout["state"] == "partial"
            for key, code in (
                ("oversized", "IMG_INTRINSIC_OVERSIZED"),
                ("upscaled", "IMG_UPSCALED"),
            ):
                for item in (layout.get("image_sizing") or {}).get(key, []):
                    ctx.add(
                        code,
                        target_url=item["src"],
                        details={
                            **item,
                            "source_page": url,
                            "source": "native-render-layout",
                            "min_encoded_bytes": layout.get("min_encoded_bytes"),
                            "oversized_ratio": layout.get("oversized_ratio"),
                            "coverage": layout["state"],
                            "reason": "intrinsic width disagrees with the displayed device-pixel width",
                        },
                    )
        coverage = _read_json(evidence.get("coverage")) or {}
        if coverage.get("state") not in {"complete", "partial"}:
            continue
        measured["UNUSED_CSS_JS"] += 1
        summaries = [coverage.get(kind) or {} for kind in ("js", "css")]
        partial["UNUSED_CSS_JS"] += coverage["state"] == "partial" or any(
            s.get("skipped") for s in summaries
        )
        for kind, summary in zip(("js", "css"), summaries, strict=True):
            for item in summary.get("resources", []):
                if (
                    item["unused_ratio"] >= UNUSED_RATIO_THRESHOLD
                    and item["unused_chars"] >= UNUSED_MIN_CHARS
                ):
                    ctx.add(
                        "UNUSED_CSS_JS",
                        target_url=url,
                        details={
                            **item,
                            "kind": kind,
                            "source": "native-render-coverage",
                            "unit": coverage.get("unit"),
                            "coverage": coverage["state"],
                            "reason": "source was unused in the recorded navigation window; later interactions were not tested",
                        },
                    )
    for code in RENDER_CHECKS:
        _measurement(
            ctx,
            code,
            "pages",
            eligible,
            measured[code],
            partial[code],
            "not fully measured by this native crawl: retained render layout or Chromium coverage is absent, unavailable or bounded for eligible HTML pages",
        )


def _check_page_weight(ctx):
    eligible = measured = partial = 0
    for url, html_size, representation, renderer_json in _pages(ctx):
        eligible += 1
        weight = _read_json(_asset_evidence(representation, renderer_json).get("weight")) or {}
        state = weight.get("state")
        size = weight.get("bytes")
        if state in {"complete", "partial"} and type(size) is int and size >= 0:
            measured += 1
            partial += state == "partial"
            details = {
                **weight,
                "source": "native-render-responses",
                "size_state": "complete" if state == "complete" else "lower_bound",
            }
        elif type(html_size) is int and html_size >= 0:
            # Declared srcset, CSS media and nomodule alternatives are inventory,
            # not proof that the browser transferred them during this navigation.
            measured += 1
            partial += 1
            size = html_size
            details = {
                "source": "native-html-response",
                "bytes": size,
                "html_bytes": size,
                "image_bytes": None,
                "font_bytes": None,
                "third_party_bytes": None,
                "unit": "captured HTML bytes",
                "scope": "HTML response only; runtime assets unavailable",
                "size_state": "lower_bound",
            }
        else:
            continue
        limit = ctx.thresholds.get("page_weight_max_bytes", 2 * 1024 * 1024)
        if size > limit:
            ctx.add(
                "HEAVY_PAGE_WEIGHT",
                target_url=url,
                details={
                    **details,
                    "threshold_bytes": limit,
                    "reason": "observed navigation bytes exceed the page budget"
                    if details["source"] == "native-render-responses"
                    else "captured HTML alone exceeds the page budget; runtime asset weight was not measured",
                },
            )
    _measurement(
        ctx,
        "HEAVY_PAGE_WEIGHT",
        "pages",
        eligible,
        measured,
        partial,
        "not fully measured by this native crawl: runtime response weight is absent or bounded; captured HTML alone is a lower bound",
    )


def check_resource_assets(ctx: AuditContext) -> None:
    _check_page_weight(ctx)
    con = ctx.scan_con
    codes = RESOURCE_CHECKS[1:]
    eligible = dict.fromkeys(codes, 0)
    measured = dict.fromkeys(codes, 0)
    partial = dict.fromkeys(codes, 0)
    range_response = False
    tables = (
        {r[0] for r in con.execute("SELECT name FROM sqlite_master WHERE type='table'")}
        if con is not None
        else set()
    )
    known = {"resource_graph_occurrences", "resource_graph_fetches"} <= tables
    if known:
        # Missing declaration coverage makes even the eligible resource count unknown.
        unknown = con.execute(
            "SELECT COUNT(*) FROM pages p LEFT JOIN documents selected ON selected.document_id=p.document_id "
            + _RAW_DOCUMENT_JOIN
            + "LEFT JOIN documents d ON d.document_id=p.document_id OR d.document_id=raw.document_id "
            "LEFT JOIN context_items c ON c.kind='resource_graph_coverage' "
            "AND c.item_key='document:'||d.document_id WHERE "
            + _HTML_WHERE
            + " AND (selected.document_id IS NULL OR c.completeness IS NULL OR c.completeness!='complete' OR "
            "CASE WHEN json_valid(c.payload_json) THEN COALESCE(json_extract(c.payload_json,'$.state') IN ('complete','empty') "
            "AND json_extract(c.payload_json,'$.omitted')=0,0) ELSE 0 END=0)"
        ).fetchone()[0]
        columns = {r[1] for r in con.execute("PRAGMA table_info(resource_graph_fetches)")}
        delivery = "f.delivery_json" if "delivery_json" in columns else "NULL"
        # Start with active declarations so budget/excluded references without fetch
        # rows remain in the denominator. The cursor and scalar counters stay bounded.
        declarations = (
            "SELECT o.resolved_url,MIN(u.url) AS source,"
            "CASE o.kind WHEN 'module' THEN 'script' WHEN 'css_import' THEN 'stylesheet' ELSE o.kind END AS kind "
            "FROM resource_graph_occurrences o JOIN pages p ON p.url_id=o.page_url_id "
            "JOIN urls u ON u.url_id=p.url_id JOIN documents d ON d.document_id=o.source_document_id "
            + _RAW_DOCUMENT_JOIN
            + "WHERE "
            + _HTML_WHERE
            + " AND "
            + _ACTIVE_DOCUMENT_WHERE
            + " GROUP BY o.resolved_url,CASE o.kind WHEN 'module' THEN 'script' WHEN 'css_import' THEN 'stylesheet' ELSE o.kind END"
        )
        rows = con.execute(
            "SELECT o.resolved_url,o.source,o.kind,f.status_code,f.bytes_received,f.state,f.content_type,"
            + delivery
            + " FROM ("
            + declarations
            + ") o LEFT JOIN resource_graph_fetches f ON f.resolved_url=o.resolved_url"
        )
        for resource, source, kind, status, size, state, media, delivery_json in rows:
            # Earlier scans classified every retained 2xx response as complete.
            # Reanalysis must not mistake a 206 fragment for whole-resource evidence.
            if status == 206 and state in {"complete", "partial"}:
                range_response = True
                state = "partial"
            applicable = ["RESOURCE_OVER_2MB", "ASSET_SHORT_CACHE"]
            if kind in _SOURCE_KINDS:
                applicable.append("UNMINIFIED_CSS_JS")
            if kind == "stylesheet":
                applicable.append("FONT_DISPLAY_MISSING")
            if kind == "script":
                applicable.extend(("LEGACY_JS", "DUPLICATED_JS"))
            for code in applicable:
                eligible[code] += 1
            if state not in {"complete", "partial"}:
                continue
            details = {
                "source": "native-resource-graph",
                "source_page": source,
                "resource_url": resource,
                "kind": kind,
                "status_code": status,
                "bytes": size,
                "size_state": "complete" if state == "complete" else "lower_bound",
            }
            if type(size) is int and size >= 0:
                measured["RESOURCE_OVER_2MB"] += 1
                partial["RESOURCE_OVER_2MB"] += state == "partial"
                limit = ctx.thresholds.get("resource_max_bytes", 2 * 1024 * 1024)
                if size > limit:
                    ctx.add(
                        "RESOURCE_OVER_2MB",
                        target_url=resource,
                        details={
                            **details,
                            "threshold_bytes": limit,
                            "reason": "captured resource bytes exceed the configured resource budget",
                        },
                    )
            info = _read_json(delivery_json) or {}
            cache = _read_json(info.get("cache")) or {}
            if type(cache.get("ok")) is bool:
                measured["ASSET_SHORT_CACHE"] += 1
                partial["ASSET_SHORT_CACHE"] += state == "partial"
                if not cache["ok"]:
                    ctx.add(
                        "ASSET_SHORT_CACHE",
                        target_url=resource,
                        status_code=status,
                        details={**details, **cache},
                    )
            if (
                state != "complete"
                or info.get("source_state") != "complete"
                or not media_matches(kind, media or "")
            ):
                continue
            if kind == "stylesheet" and isinstance(info.get("font_display"), list):
                measured["FONT_DISPLAY_MISSING"] += 1
                if info["font_display"]:
                    ctx.add(
                        "FONT_DISPLAY_MISSING",
                        target_url=resource,
                        details={
                            **details,
                            "font_faces": info["font_display"],
                            "font_faces_count": info.get(
                                "font_display_count", len(info["font_display"])
                            ),
                            "font_faces_omitted": info.get("font_display_omitted", 0),
                            "reason": "observed @font-face lacks swap, fallback or optional font-display",
                        },
                    )
            if kind in _SOURCE_KINDS and type(info.get("unminified")) is bool:
                measured["UNMINIFIED_CSS_JS"] += 1
                if info["unminified"]:
                    ctx.add(
                        "UNMINIFIED_CSS_JS",
                        target_url=resource,
                        details={
                            **details,
                            "reason": "retained source does not look minified (whitespace/line-length heuristic)",
                        },
                    )
            if kind == "script":
                if type(info.get("legacy_js")) is bool:
                    measured["LEGACY_JS"] += 1
                    if info["legacy_js"]:
                        ctx.add(
                            "LEGACY_JS",
                            target_url=resource,
                            details={
                                **details,
                                "reason": "retained script contains a legacy/polyfill marker (heuristic)",
                            },
                        )
                if isinstance(info.get("sha256"), str):
                    measured["DUPLICATED_JS"] += 1
        if "delivery_json" in columns:
            rows = con.execute(
                "SELECT u.url,json_extract(f.delivery_json,'$.sha256'),MIN(o.resolved_url),COUNT(DISTINCT o.resolved_url) "
                "FROM resource_graph_occurrences o JOIN resource_graph_fetches f ON f.resolved_url=o.resolved_url "
                "JOIN pages p ON p.url_id=o.page_url_id JOIN urls u ON u.url_id=p.url_id "
                "JOIN documents d ON d.document_id=o.source_document_id "
                + _RAW_DOCUMENT_JOIN
                + "WHERE "
                + _HTML_WHERE
                + " AND "
                + _ACTIVE_DOCUMENT_WHERE
                + " AND o.kind IN ('script','module') AND f.state='complete' AND f.status_code!=206 AND json_valid(f.delivery_json) "
                "AND json_extract(f.delivery_json,'$.source_state')='complete' AND json_extract(f.delivery_json,'$.sha256') IS NOT NULL "
                "AND lower(trim(substr(f.content_type,1,instr(f.content_type||';',';')-1))) IN "
                "('text/javascript','application/javascript','text/ecmascript','application/ecmascript','application/x-javascript') "
                "GROUP BY o.page_url_id,json_extract(f.delivery_json,'$.sha256') HAVING COUNT(DISTINCT o.resolved_url)>1"
            )
            for url, digest, resource, count in rows:
                ctx.add(
                    "DUPLICATED_JS",
                    target_url=url,
                    details={
                        "source": "native-resource-graph",
                        "sha256": digest,
                        "resource_sample": resource,
                        "distinct_resources": count,
                        "reason": "different script URLs on this page contain identical captured source bytes",
                    },
                )
    else:
        unknown = True
    for code in codes:
        _measurement(
            ctx,
            code,
            "resources",
            None if unknown else eligible[code],
            measured[code],
            partial[code],
            "not fully measured by this native crawl: declared-resource evidence is absent, unavailable, partial or omits declarations; enable scan.v2 resources.fetch within explicit graph budgets"
            + (
                "; HTTP 206 responses contain only byte ranges; whole-resource source was not measured"
                if range_response
                else ""
            ),
        )
