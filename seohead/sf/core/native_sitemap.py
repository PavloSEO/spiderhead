"""Offline sitemap/robots findings over cursor-backed retained scan evidence."""

from __future__ import annotations

import itertools
import json
from typing import Any
from urllib.parse import urlsplit

from seohead.checks.parser import robots_directives
from seohead.checks.robots import is_allowed, match_path
from seohead.storage.analysis_graph import selected_links_cte

from .context import AuditContext
from .normalize import norm_url

STATUS_CHECKS = (
    "SITEMAP_URL_3XX",
    "SITEMAP_URL_4XX_5XX",
    "SITEMAP_URL_NON_INDEXABLE",
    "SITEMAP_URL_NON_CANONICAL",
    "SITEMAP_URL_BLOCKED_ROBOTS",
)
COVERAGE_CHECKS = ("SITEMAP_ORPHAN", "URL_NOT_IN_SITEMAP", "SITEMAP_DESYNC")


def _robots(ctx: AuditContext) -> dict[str, Any] | None:
    row = ctx.scan_con.execute(
        "SELECT payload_json FROM context_items WHERE kind='robots_summary' AND item_key='run'"
    ).fetchone()
    if row is None:
        return None
    payload = json.loads(row[0])
    return payload if payload["fetch_state"] == "fetched" else None


def _blocked(robots: dict[str, Any] | None, target: str, start_url: str) -> bool:
    # Rules belong to the fetched origin, never a third-party CDN or other host.
    return bool(
        robots is not None
        and urlsplit(target).netloc.lower() == urlsplit(start_url).netloc.lower()
        and urlsplit(target).scheme.lower() == urlsplit(start_url).scheme.lower()
        and not is_allowed(robots["parsed"], match_path(target), robots["token"])
    )


def run_retained_sitemap(
    ctx: AuditContext, reconciliation: Any, *, start_url: str
) -> dict[str, Any]:
    """Emit full findings while retaining only twenty examples per summary bucket.

    The scan reader supplies a validated, budgeted SQLite snapshot. Missing or
    partial declarations cannot prove absence; positive conflicts still use the
    recorded members. Resources are judged only when their dependency was retained.
    """
    if ctx.scan_con is None:
        raise ValueError("retained sitemap checks require a saved scan")
    robots = _robots(ctx)
    summary: dict[str, Any] = {"evidence_source": "retained_scan", "offline": True}
    roots = [
        str(row[0])
        for row in ctx.scan_con.execute(
            "SELECT u.url FROM context_items c JOIN urls u "
            "ON u.url_id=json_extract(c.payload_json,'$.sitemap_url_id') "
            "WHERE c.kind='sitemap_declaration' ORDER BY json_extract(c.payload_json,'$.ordinal')"
        )
    ]
    summary.update(sitemap_url=roots[0] if roots else None, sitemap_urls=roots)
    for check in (
        "SITEMAP_TOO_MANY_URLS",
        "SITEMAP_TOO_LARGE",
        "SITEMAP_URL_DUPLICATED",
        "SITEMAP_STALE_LASTMOD",
    ):
        ctx.skip(
            check, "retained sitemap membership does not include protocol size or lastmod evidence"
        )
    if robots is None:
        ctx.skip("SITEMAP_NOT_IN_ROBOTS", "no retained fetched robots.txt policy")
    elif not robots["parsed"]["sitemaps"]:
        ctx.add(
            "SITEMAP_NOT_IN_ROBOTS",
            target_url=start_url,
            details={
                "reason": "retained robots.txt declares no sitemap",
                "evidence_source": "retained_scan",
            },
        )
    _run_robots(ctx, robots, start_url)
    if reconciliation is None or not reconciliation._names:
        reason = "no saved sitemap declarations"
        for check in (*STATUS_CHECKS, *COVERAGE_CHECKS):
            ctx.skip(check, reason)
        ctx.skip("SITEMAP_FETCH_INCOMPLETE", reason)
        return {**summary, "reconciliation_available": False, "reconciliation_reason": reason}

    unmeasured = 0
    for row in reconciliation.iter_declared_facts():
        target, status = row["url"], row["status_code"]
        details = {
            "in_sitemap": True,
            "source_url": row["sitemap_url"],
            "target_url": target,
            "status_code": status,
            "evidence_source": "retained_scan",
        }
        evidence = {"sitemap_url": row["sitemap_url"], "retained": True}
        if row["blocked"] or _blocked(robots, target, start_url):
            ctx.add(
                "SITEMAP_URL_BLOCKED_ROBOTS",
                target_url=target,
                status_code=status,
                details={**details, "reason": "blocked by retained robots.txt policy"},
                evidence=evidence,
            )
        if status is None:
            unmeasured += 1
            continue
        if 300 <= status < 400:
            ctx.add(
                "SITEMAP_URL_3XX",
                target_url=target,
                status_code=status,
                details={**details, "reason": "redirect", "redirect_url": row["redirect_url"]},
                evidence=evidence,
            )
        elif 400 <= status < 600:
            ctx.add(
                "SITEMAP_URL_4XX_5XX",
                target_url=target,
                status_code=status,
                details={**details, "reason": "client error" if status < 500 else "server error"},
                evidence=evidence,
            )
        elif 200 <= status < 300:
            if "noindex" in robots_directives(row["meta_robots"] or "", row["x_robots"] or ""):
                ctx.add(
                    "SITEMAP_URL_NON_INDEXABLE",
                    target_url=target,
                    status_code=status,
                    details={**details, "reason": "noindex", "indexability": "Non-Indexable"},
                    evidence=evidence,
                )
            if row["canonical"] and norm_url(target) != norm_url(row["canonical"]):
                ctx.add(
                    "SITEMAP_URL_NON_CANONICAL",
                    target_url=target,
                    status_code=status,
                    details={
                        **details,
                        "reason": "canonical points to another URL",
                        "canonical_url": row["canonical"],
                    },
                    evidence=evidence,
                )
    summary["sitemap_status_unmeasured"] = unmeasured
    if unmeasured:
        for check in STATUS_CHECKS[:4]:
            ctx.skip(check, f"{unmeasured} declared sitemap URL(s) have no retained response")
    if robots is None:
        ctx.skip("SITEMAP_URL_BLOCKED_ROBOTS", "no retained fetched robots.txt policy")
    if not reconciliation.available:
        ctx.add(
            "SITEMAP_FETCH_INCOMPLETE",
            target_url=start_url,
            details={"reason": reconciliation.reason, "evidence_source": "retained_scan"},
        )
        for check in COVERAGE_CHECKS:
            ctx.skip(check, reconciliation.reason)
        return {
            **summary,
            "reconciliation_available": False,
            "reconciliation_reason": reconciliation.reason,
        }

    summary.update(reconciliation.summary(), reconciliation_available=True)
    for name in (
        "in_sitemap_and_linked",
        "in_sitemap_not_linked",
        "linked_not_in_sitemap",
        "linked_not_comparable",
    ):
        summary[name] = list(itertools.islice(reconciliation.iter_bucket(name), 20))
        summary[name + "_count"] = reconciliation.count_bucket(name)
    summary["examples_limit"] = 20
    missing = 0
    for row in reconciliation.iter_missing_canonical_pages():
        target, status = row["url"], row["status_code"]
        if _blocked(robots, target, start_url):
            continue
        missing += 1
        ctx.add(
            "URL_NOT_IN_SITEMAP",
            target_url=target,
            status_code=status,
            details={
                "source_url": roots[0] if roots else None,
                "target_url": target,
                "status_code": status,
                "reason": "canonical indexable HTML page absent from retained sitemap",
                "evidence_source": "retained_scan",
            },
        )
    summary["canonical_pages_not_in_sitemap_count"] = missing
    # Incomplete graph evidence cannot prove that a page has no internal links.
    if reconciliation.crawl_partial:
        ctx.skip("SITEMAP_ORPHAN", "crawl is partial: internal link absence cannot be proven")
        ctx.skip(
            "SITEMAP_DESYNC", "crawl is partial: sitemap-versus-link-graph verdict cannot be proven"
        )
    else:
        orphaned = 0
        for row in reconciliation.iter_orphan_facts():
            target = row["url"]
            if norm_url(target) == norm_url(start_url):
                continue
            orphaned += 1
            ctx.add(
                "SITEMAP_ORPHAN",
                target_url=target,
                status_code=row["status_code"],
                details={
                    "in_sitemap": True,
                    "source_url": row["sitemap_url"],
                    "target_url": target,
                    "status_code": row["status_code"],
                    "reason": "no retained internal link",
                    "evidence_source": "retained_scan",
                },
            )
        linked_missing = summary["linked_not_in_sitemap_count"]
        summary["sitemap_orphan_findings_count"] = orphaned
        crawl_pct = round(
            100 * linked_missing / max(summary["in_sitemap_and_linked_count"] + linked_missing, 1),
            1,
        )
        sitemap_pct = round(100 * orphaned / max(reconciliation.counts["urls_in_sitemap"], 1), 1)
        if max(crawl_pct, sitemap_pct) >= ctx.thresholds["sitemap_desync_pct_warn"]:
            ctx.add(
                "SITEMAP_DESYNC",
                target_url=start_url,
                details={
                    "in_crawl_not_in_sitemap": linked_missing,
                    "in_sitemap_not_in_crawl": orphaned,
                    "crawl_not_in_sitemap_pct": crawl_pct,
                    "sitemap_not_in_crawl_pct": sitemap_pct,
                    "examples_missing_from_sitemap": summary["linked_not_in_sitemap"],
                    "examples_in_sitemap_not_crawled": summary["in_sitemap_not_linked"],
                    "evidence_source": "retained_scan",
                },
            )
    return summary


def _run_robots(ctx: AuditContext, robots: dict[str, Any] | None, start_url: str) -> None:
    con = ctx.scan_con
    cursor = con.execute(
        selected_links_cte("selected")
        + "SELECT src.url AS source,dst.url AS target,p.status_code,c.payload_json "
        "FROM selected l JOIN urls src ON src.url_id=l.source_url_id "
        "JOIN urls dst ON dst.url_id=l.destination_url_id "
        "LEFT JOIN pages p ON p.url_id=l.destination_url_id "
        "LEFT JOIN context_items c ON c.kind='robots_blocked_url' AND c.item_key='url:'||dst.url_id "
        "ORDER BY l.source_url_id,l.ordinal,l.link_id"
    )
    explicit = False
    for row in cursor:
        if urlsplit(row["target"]).netloc.lower() != urlsplit(start_url).netloc.lower():
            continue
        if row["payload_json"] is not None or _blocked(robots, row["target"], start_url):
            explicit = explicit or row["payload_json"] is not None
            ctx.add(
                "INTERNALLY_LINKED_ROBOTS_BLOCKED",
                target_url=row["target"],
                details={
                    "source_url": row["source"],
                    "target_url": row["target"],
                    "status_code": row["status_code"],
                    "reason": "internal link targets a robots-blocked URL",
                    "evidence_source": "retained_scan",
                },
                status_code=row["status_code"],
                locations=[{"source_url": row["source"], "destination_url": row["target"]}],
                evidence={"retained_robots": True},
            )
    for row in con.execute(
        "SELECT src.url AS source,dst.url AS target,r.kind,r.capture_state,r.reason "
        "FROM resource_refs r JOIN pages p ON p.url_id=r.page_url_id "
        "JOIN urls src ON src.url_id=r.page_url_id JOIN urls dst ON dst.url_id=r.resource_url_id "
        "WHERE r.representation=p.representation AND r.kind IN ('script','stylesheet') "
        "ORDER BY r.page_url_id,r.ordinal"
    ):
        if row["capture_state"] == "excluded_robots" or _blocked(robots, row["target"], start_url):
            ctx.add(
                "ROBOTS_BLOCKS_RESOURCES",
                target_url=row["source"],
                details={
                    "source_url": row["source"],
                    "target_url": row["target"],
                    "kind": row["kind"],
                    "reason": "rendering dependency blocked by retained robots.txt",
                    "capture_reason": row["reason"],
                    "evidence_source": "retained_scan",
                },
                evidence={"retained_resource_declaration": True},
            )
    if robots is None:
        if not explicit:
            ctx.skip("INTERNALLY_LINKED_ROBOTS_BLOCKED", "no retained fetched robots.txt policy")
        ctx.skip(
            "ROBOTS_BLOCKS_RESOURCES", "no retained fetched robots.txt policy or blocked dependency"
        )
    elif (
        con.execute("SELECT 1 FROM resource_refs LIMIT 1").fetchone() is None
        and con.execute(
            "SELECT 1 FROM context_items WHERE kind='resource_inventory' AND completeness='complete' LIMIT 1"
        ).fetchone()
        is None
    ):
        ctx.skip("ROBOTS_BLOCKS_RESOURCES", "no retained script or stylesheet dependency inventory")
