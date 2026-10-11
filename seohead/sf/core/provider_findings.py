"""Join retained project provider history to one SF audit without fetching sources.

Provider history is an independent measurement: collection gaps stay partial and measured
per-URL findings enter the normal registered issue stream. Typed source intervals and coverage stay
alongside those issues in the audit summary.
"""

from __future__ import annotations

import hashlib
from datetime import date, datetime, timedelta, timezone
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

MAX_PROVIDER_ROWS = 100_000
MAX_FINDING_URLS = 500
MAX_CRAWL_PAGES = 100_000
MAX_ISSUES_PER_PROVIDER_CHECK = 500

GSC_ANALYTICS_CHECKS = (
    "GSC_ORPHAN",
    "NO_SEARCH_DATA",
    "NON_INDEXABLE_WITH_SEARCH_DATA",
)
GSC_INSPECTION_CHECKS = (
    "GSC_INDEXED_WITH_ISSUES",
    "GSC_AMP_INVALID",
    "GSC_RICH_RESULTS_INVALID",
    "GSC_INDEXABLE_NOT_INDEXED",
    "GSC_USER_CANONICAL_NOT_SELECTED",
    "GSC_URL_NOT_ON_GOOGLE",
)
ANALYTICS_CHECKS = {
    "ga4": {
        "GA_ORPHAN_URL": "GA_ORPHAN_URL",
        "NO_GA_DATA": "NO_GA_DATA",
        "NON_INDEXABLE_WITH_GA_DATA": "NON_INDEXABLE_WITH_GA_DATA",
        "BOUNCE_RATE_ABOVE_70": "BOUNCE_RATE_ABOVE_70",
    },
    "metrika": {
        "GA_ORPHAN_URL": "METRIKA_ORPHAN_URL",
        "NO_GA_DATA": "NO_METRIKA_DATA",
        "NON_INDEXABLE_WITH_GA_DATA": "NON_INDEXABLE_WITH_METRIKA_DATA",
        "BOUNCE_RATE_ABOVE_70": "METRIKA_BOUNCE_RATE_ABOVE_70",
    },
}


def _period(source: str) -> tuple[str, str]:
    from seohead.data_sources.sources_db import _requested_period

    start, end, _available = _requested_period(source, None, None)
    return start, end


def _history(project_dir: str | Path, source: str, resource: str) -> dict[str, Any]:
    from seohead.data_sources import sources_db

    path = sources_db.db_path(project=str(project_dir))
    start, end = _period(source)
    rows = sources_db.query(
        path,
        source,
        resource=resource,
        start_date=start,
        end_date=end,
        limit=MAX_PROVIDER_ROWS,
    )
    status = sources_db.status(path)
    entry = next(
        (
            item
            for item in status["sources"]
            if item["source"] == source and item["resource"] == resource
        ),
        None,
    )
    complete_days = set((entry or {}).get("coverage", {}).get("complete", []))
    empty_days = set((entry or {}).get("coverage", {}).get("empty", []))
    first, last = date.fromisoformat(start), date.fromisoformat(end)
    expected = [(first + timedelta(days=index)).isoformat() for index in range((last-first).days+1)]
    coverage_complete = bool(entry) and all(day in complete_days or day in empty_days for day in expected)
    return {
        "period": {"from": start, "to": end},
        "rows": rows["rows"],
        "row_count": rows["returned"],
        "truncated": bool(rows.get("has_more")),
        "omitted_rows_lower_bound": 1 if rows.get("has_more") else 0,
        "coverage_complete": coverage_complete and not rows.get("has_more"),
        "days_complete": sum(day in complete_days for day in expected),
        "days_empty": sum(day in empty_days for day in expected),
        "days_expected": len(expected),
    }


def _origin(project_dir: str | Path) -> str | None:
    from seohead.projects.workspace import open_project

    target = open_project(project_dir)["project"]["site"]["target"]
    parts = urlsplit(target)
    return f"{parts.scheme}://{parts.netloc}" if parts.scheme and parts.netloc else None


def _history_source(bindings: list[dict[str, Any]], service: str) -> dict[str, Any] | None:
    matches = [
        item
        for item in bindings
        if item.get("service") == service
        and item.get("status") in {"selected", "available", "stale", "unavailable", "no_access"}
    ]
    return matches[0] if len(matches) == 1 else None


def _url(value: Any, origin: str | None) -> str | None:
    if not isinstance(value, str) or not value.strip():
        return None
    candidate = value.strip()
    parts = urlsplit(candidate)
    if parts.scheme in {"http", "https"} and parts.netloc:
        return candidate
    if origin and candidate.startswith("/"):
        return origin + candidate
    return None


def _public_url(value: str) -> str:
    """Hide query values and URL userinfo before provider findings reach issue output."""
    parts = urlsplit(value)
    host = parts.hostname or ""
    if ":" in host and not host.startswith("["):
        host = f"[{host}]"
    try:
        port = parts.port
    except ValueError:
        port = None
    netloc = f"{host}:{port}" if port is not None else host
    return f"{parts.scheme}://{netloc}{parts.path}"


def _url_id(value: str) -> str:
    return "url:v1:" + hashlib.sha256(value.encode("utf-8")).hexdigest()[:24]


def _analytics_rows(
    source: str, history: dict[str, Any], origin: str | None
) -> tuple[list[dict], str, str | None, int]:
    totals: dict[str, dict[str, Any]] = {}
    visit_column = "sessions" if source == "ga4" else "visits"
    unreadable = 0
    for row in history["rows"]:
        url = _url(row.get("landing_page"), origin)
        if url is None:
            unreadable += 1
            continue
        item = totals.setdefault(
            url,
            {
                "url": url,
                visit_column: 0.0,
                "visits_seen": False,
                "visits_unreadable": False,
                "bounce_weighted": 0.0,
                "bounce_weight": 0.0,
                "bounce_unreadable": False,
            },
        )
        visits = row.get(visit_column)
        if isinstance(visits, (int, float)) and not isinstance(visits, bool) and visits >= 0:
            item[visit_column] += visits
            item["visits_seen"] = True
            bounce = row.get("bounce_rate")
            if source == "metrika" and isinstance(bounce, (int, float)) and not isinstance(bounce, bool) and 0 <= bounce <= 1 and visits > 0:
                item["bounce_weighted"] += bounce * visits
                item["bounce_weight"] += visits
            elif source == "metrika" and visits > 0:
                item["bounce_unreadable"] = True
        else:
            item["visits_unreadable"] = True
            unreadable += 1
    rows = []
    for item in totals.values():
        row = {
            "url": item["url"],
            visit_column: (
                item[visit_column]
                if item["visits_seen"] and not item["visits_unreadable"]
                else None
            ),
        }
        if source == "metrika":
            row["bounce_rate"] = (
                item["bounce_weighted"] / item["bounce_weight"]
                if item["bounce_weight"] and not item["bounce_unreadable"]
                else None
            )
            if item["bounce_unreadable"]:
                unreadable += 1
        rows.append(row)
    bounce_column = (
        "bounce_rate"
        if source == "metrika"
        and any(row.get("bounce_rate") is not None for row in rows)
        else None
    )
    return rows, visit_column, bounce_column, unreadable


def _cap_findings(findings: dict[str, Any]) -> tuple[dict[str, Any], bool]:
    truncated = False
    result = {}
    for check, item in findings.items():
        if isinstance(item, list):
            if len(item) > MAX_FINDING_URLS:
                item = item[:MAX_FINDING_URLS]
                truncated = True
            result[check] = item
            continue
        if not isinstance(item, dict):
            result[check] = item
            continue
        urls = item.get("urls")
        if isinstance(urls, list) and len(urls) > MAX_FINDING_URLS:
            item = {
                **item,
                "urls": urls[:MAX_FINDING_URLS],
                "omitted_urls": len(urls) - MAX_FINDING_URLS,
            }
            for key in ("safe_urls", "url_ids"):
                values = item.get(key)
                if isinstance(values, list):
                    item[key] = values[:MAX_FINDING_URLS]
            truncated = True
        result[check] = item
    return result, truncated


def _gsc_findings(pages: list[dict[str, Any]], history: dict[str, Any], resource: str, crawl_partial: bool) -> dict[str, Any]:
    from seohead.checks.search_console_findings import evaluate
    from seohead.data_sources.evidence_import import MAPPING_FORMAT, normalize_inline

    manifest = {
        "format": MAPPING_FORMAT,
        "source": {
            "kind": "provider_evidence",
            "provider": "gsc",
            "operation": "search_analytics",
            "reporting_identity": resource,
            "privacy": "restricted",
            "search_engine": "google",
            "search_type": "web",
            "timezone": "America/Los_Angeles",
        },
        "period": {"start_date": history["period"]["from"], "end_date": history["period"]["to"]},
        "url": {"field": "page", "kind": "absolute"},
        "metrics": [
            {"name": name, "type": "number", "unit": unit}
            for name, unit in (("clicks", "count"), ("impressions", "count"), ("ctr", "ratio"), ("position", "position"))
        ],
        "dimensions": ["date", "query", "page"],
        "collection": {
            "state": "complete" if history["coverage_complete"] else "partial",
            "sampled": False,
            "thresholded": False,
            "truncated": history["truncated"],
            "reason": None if history["coverage_complete"] else "retained GSC coverage has gaps or is truncated",
        },
        "duplicate_policy": "mark",
        "row_shape": "flat",
    }
    evidence = normalize_inline(
        [
            {name: row.get(name) for name in ("page", "query", "date", "clicks", "impressions", "ctr", "position")}
            for row in history["rows"]
        ],
        manifest=manifest,
    )
    result = evaluate(
        pages,
        evidence,
        crawl_partial=crawl_partial,
        window_complete=history["coverage_complete"],
    )
    for values in result.values():
        if not isinstance(values, list):
            continue
        for item in values:
            if isinstance(item, dict) and isinstance(item.get("url"), str):
                item["safe_url"] = _public_url(item["url"])
                item["url_id"] = _url_id(item["url"])
    result["coverage"] = {key: value for key, value in history.items() if key != "rows"}
    result["findings"], result["truncated_findings"] = _cap_findings(result["findings"])
    return result


def _inspection_urls(pages: list[dict[str, Any]], existing: list[dict[str, Any]]) -> list[str]:
    seen = {item.get("url") for item in existing if item.get("state") == "complete"}
    candidates = sorted(
        {
            page["url"]
            for page in pages
            if isinstance(page.get("url"), str) and page["url"] not in seen
        }
    )
    if len(candidates) <= 50:
        return candidates
    return [candidates[index * (len(candidates) - 1) // 49] for index in range(50)]


def _gsc_inspections(
    pages: list[dict[str, Any]],
    *,
    project_dir: str | Path,
    site_url: str,
    collect: bool,
) -> dict[str, Any]:
    from seohead.checks.search_console_findings import evaluate_inspections
    from seohead.data_sources import gsc_inspection_history

    since = (datetime.now(timezone.utc).date() - timedelta(days=28)).isoformat() + "T00:00:00Z"
    try:
        cached = gsc_inspection_history.latest(project_dir, site_url, since=since)
        rows = cached["rows"]
    except (OSError, ValueError):
        rows = []
    collection_state = "cached" if rows else "not_requested"
    quota = None
    requests_made = 0
    if collect:
        wanted = _inspection_urls(pages, rows)
        reservation = gsc_inspection_history.reserve(project_dir, site_url, wanted)
        quota = {
            key: value
            for key, value in reservation.items()
            if key not in {"reservation_id", "site_url", "urls"}
        }
        if reservation["urls"]:
            from seohead.data_sources import gsc

            try:
                result = gsc.inspect_urls(site_url, reservation["urls"])
            except Exception:
                result = {
                    "ok": False,
                    "state": "failed",
                    "requests_made": reservation["reserved"],
                    "urls": [],
                }
            requests_made = int(result.get("requests_made") or 0)
            settlement = None
            settlement_failed = False
            try:
                if reservation["reservation_id"]:
                    settlement = gsc_inspection_history.settle(
                        project_dir,
                        reservation["reservation_id"],
                        result.get("urls") or [],
                        requests_made=requests_made,
                    )
            except (OSError, ValueError):
                settlement_failed = True
            if settlement_failed:
                collection_state = "history_write_failed"
            elif result.get("state") == "not_configured":
                collection_state = "not_configured"
            elif settlement and settlement.get("state") == "history_full":
                collection_state = "history_full"
            elif requests_made:
                collection_state = "collected" if result.get("state") == "complete" else "partial"
            else:
                collection_state = result.get("state") or "failed"
            try:
                rows = gsc_inspection_history.latest(project_dir, site_url, since=since)["rows"]
            except (OSError, ValueError):
                rows = result.get("urls") or []
        elif not wanted:
            collection_state = "cached"
        else:
            collection_state = "quota_limited"
    summary = evaluate_inspections(
        pages,
        rows,
        requested=(quota or {}).get("reserved", 0),
        deferred=(quota or {}).get("deferred", 0),
    )
    summary["history_urls"] = len(rows)
    crawl_urls = {
        page.get("url") for page in pages if isinstance(page.get("url"), str)
    }
    inspected_urls = {
        row.get("url")
        for row in rows
        if row.get("state") == "complete"
        and isinstance(row.get("index_status"), dict)
        and row["index_status"].get("verdict") in {"PASS", "FAIL", "NEUTRAL"}
        and row.get("url") in crawl_urls
    }
    summary["crawl_pages"] = len(crawl_urls)
    summary["crawl_urls_inspected"] = len(inspected_urls)
    summary["sample_population_complete"] = bool(crawl_urls) and inspected_urls == crawl_urls
    for values in summary.get("findings", {}).values():
        if not isinstance(values, list):
            continue
        for item in values:
            if isinstance(item, dict) and isinstance(item.get("url"), str):
                item["safe_url"] = _public_url(item["url"])
                item["url_id"] = _url_id(item["url"])
    summary["findings"], summary["truncated_findings"] = _cap_findings(summary["findings"])
    return {
        "state": collection_state,
        "coverage": "bounded URL sample; unsampled pages are unknown",
        "daily_budget": quota,
        "requests_made": requests_made,
        **summary,
    }


def _cloudflare_history(
    project_dir: str | Path,
    binding: dict[str, Any],
    *,
    as_of: datetime | None = None,
) -> dict[str, Any]:
    from seohead.data_sources.cloudflare_history import list_snapshots

    result = list_snapshots(
        project=str(project_dir),
        zone_id=binding["resource"],
        limit=1,
        offset=0,
        include_facts=True,
        facts_limit=MAX_FINDING_URLS,
    )
    if not result["rows"]:
        return {"state": "unavailable", "reason": "no retained Cloudflare aggregate snapshot"}
    snapshot = result["rows"][0]
    facts = snapshot.get("url_facts") or []
    if not isinstance(facts, list):
        facts = []
    today = (as_of or datetime.now(timezone.utc)).date()
    reasons = []
    observed = snapshot.get("observed_at")
    observed_date = None
    try:
        if not isinstance(observed, str):
            raise ValueError
        observed_stamp = datetime.fromisoformat(observed.replace("Z", "+00:00"))
        if observed_stamp.tzinfo is None:
            raise ValueError
        observed_date = observed_stamp.astimezone(timezone.utc).date()
        age_days = (today - observed_date).days
        if age_days < 0:
            reasons.append("snapshot timestamp is in the future")
        elif age_days > 28:
            reasons.append("latest Cloudflare snapshot is older than 28 days")
    except ValueError:
        age_days = None
        reasons.append("snapshot timestamp is missing or invalid")
    period = snapshot.get("period")
    try:
        start_date = date.fromisoformat(period["from"])
        end_date = date.fromisoformat(period["to"])
        native_period_days = (end_date - start_date).days + 1
        if native_period_days < 1:
            raise ValueError
        data_age_days = (today - end_date).days
        if data_age_days < 0:
            reasons.append("snapshot reporting interval ends in the future")
        elif data_age_days > 28:
            reasons.append("snapshot reporting interval ends more than 28 days ago")
    except (KeyError, TypeError, ValueError):
        native_period_days = None
        data_age_days = None
        reasons.append("snapshot has no valid native reporting interval")
    source_omitted = snapshot.get("url_facts_omitted", 0)
    source_omitted = source_omitted if type(source_omitted) is int and source_omitted >= 0 else 0
    page_omitted = snapshot.get("url_facts_page_omitted", 0)
    page_omitted = page_omitted if type(page_omitted) is int and page_omitted >= 0 else 0
    omitted = max(
        0,
        source_omitted + page_omitted + len(facts) - MAX_FINDING_URLS,
    )
    source_truncated = bool(snapshot.get("url_facts_source_truncated")) or source_omitted > 0
    if source_truncated or page_omitted or omitted:
        reasons.append("retained URL facts are truncated or paged")
    if snapshot.get("sampled") or snapshot.get("truncated_days"):
        reasons.append("retained Cloudflare collection was sampled or truncated")
    if snapshot.get("state") != "complete" and not reasons:
        reasons.append("retained Cloudflare snapshot is partial")
    state = "complete" if snapshot.get("state") == "complete" and not reasons else "partial"
    return {
        "state": state,
        "reason": "; ".join(reasons) if reasons else None,
        "observed_at": observed,
        "age_days": age_days,
        "data_age_days": data_age_days,
        "period": snapshot.get("period"),
        "period_days": native_period_days,
        "coverage": "one retained aggregate snapshot for this native interval; not a 28-day history",
        "sampled": snapshot.get("sampled"),
        "truncated_days": snapshot.get("truncated_days") or [],
        "url_facts": facts[:MAX_FINDING_URLS],
        "omitted_url_facts": omitted,
        "url_facts_truncated": source_truncated or bool(page_omitted) or omitted > 0,
        "fact_scope": "grouped host/path/day counts; no query string, client IP or sub-day timestamp",
    }


def project_provider_findings(
    pages: list[dict[str, Any]],
    *,
    project_dir: str | Path,
    crawl_partial: bool,
    crawl_pages_omitted: int = 0,
    inspect_gsc: bool = False,
) -> dict[str, Any]:
    """Return bounded URL findings from explicitly bound, already-retained history."""
    from seohead.projects.bindings import list_bindings

    try:
        bindings = list_bindings(project_dir)["bindings"]
        origin = _origin(project_dir)
    except (OSError, ValueError):
        return {"schema_version": "provider_findings.v1", "state": "unavailable", "reason": "project bindings are unavailable", "sources": {}}
    sources: dict[str, Any] = {}
    for provider, source_name in (("gsc", "gsc"), ("ga4", "ga4"), ("metrika", "metrika")):
        binding = _history_source(bindings, provider)
        if binding is None:
            sources[provider] = {"state": "not_configured", "reason": "no unique selected provider binding"}
            continue
        try:
            history = _history(project_dir, source_name, binding["resource"])
        except (OSError, ValueError):
            sources[provider] = {"state": "unavailable", "reason": "no readable retained provider history"}
            continue
        if provider == "gsc":
            result = _gsc_findings(pages, history, binding["resource"], crawl_partial)
            result["inspection"] = _gsc_inspections(
                pages,
                project_dir=project_dir,
                site_url=binding["resource"],
                collect=inspect_gsc,
            )
            result["state"] = "complete" if history["coverage_complete"] else "partial"
            sources[provider] = result
            continue
        rows, visits_column, bounce_column, unusable_rows = _analytics_rows(
            provider, history, origin
        )
        from seohead.checks.analytics_findings import analytics_findings
        from seohead.checks.external_join import join_external_data

        joined = join_external_data(pages, rows, url_column="url")
        evidence_complete = history["coverage_complete"] and unusable_rows == 0
        reason = (
            None
            if evidence_complete
            else "retained analytics coverage has gaps, truncation, or unusable URL/metric rows"
        )
        result = analytics_findings(
            pages,
            joined,
            partial=crawl_partial,
            evidence_complete=evidence_complete,
            evidence_reason=reason,
            empty_is_complete=history["coverage_complete"] and history["row_count"] == 0,
            visits_column=visits_column,
            bounce_column=bounce_column,
            bounce_reason=(
                "retained Metrika history contains no readable per-URL bounce values"
                if provider == "metrika" and bounce_column is None
                else None
            ),
        )
        if provider == "metrika":
            metrika_ids = {
                "GA_ORPHAN_URL": "METRIKA_ORPHAN_URL",
                "NO_GA_DATA": "NO_METRIKA_DATA",
                "NON_INDEXABLE_WITH_GA_DATA": "NON_INDEXABLE_WITH_METRIKA_DATA",
                "BOUNCE_RATE_ABOVE_70": "METRIKA_BOUNCE_RATE_ABOVE_70",
            }
            result["findings"] = {
                metrika_ids.get(check, check): finding
                for check, finding in result["findings"].items()
            }
        result["findings"], truncated = _cap_findings(result["findings"])
        for finding in result["findings"].values():
            if isinstance(finding, dict) and isinstance(finding.get("urls"), list):
                urls = finding["urls"]
                finding["safe_urls"] = [
                    _public_url(url) if isinstance(url, str) else url for url in urls
                ]
                finding["url_ids"] = [
                    _url_id(url) if isinstance(url, str) else None for url in urls
                ]
        result["coverage"] = {key: value for key, value in history.items() if key != "rows"}
        result["coverage"]["unusable_history_rows"] = unusable_rows
        result["truncated_findings"] = truncated
        result["state"] = "complete" if evidence_complete else "partial"
        sources[provider] = result
    cloudflare_binding = _history_source(bindings, "cloudflare")
    if cloudflare_binding is None:
        sources["cloudflare"] = {
            "state": "not_configured",
            "reason": "no unique selected Cloudflare zone binding",
        }
    else:
        try:
            sources["cloudflare"] = _cloudflare_history(project_dir, cloudflare_binding)
        except (OSError, ValueError):
            sources["cloudflare"] = {
                "state": "unavailable",
                "reason": "retained Cloudflare aggregate history could not be read",
            }
    states = [item.get("state") for item in sources.values()]
    configured_states = [state for state in states if state != "not_configured"]
    partial = crawl_partial or crawl_pages_omitted > 0
    return {
        "schema_version": "provider_findings.v1",
        "state": "unavailable"
        if not configured_states
        else "partial"
        if partial or any(state != "complete" for state in configured_states)
        else "complete",
        "period_days": 28,
        "period_scope": ["gsc", "ga4", "metrika"],
        "crawl_pages_omitted": crawl_pages_omitted,
        "source": "project-bound provider history",
        "network_requests": bool(inspect_gsc),
        "sources": sources,
    }


def emit_provider_issues(ctx: Any, evidence: dict[str, Any]) -> None:
    """Project measured source findings into the registered audit issue stream."""

    def skip(check_id: str, reason: str) -> None:
        if not ctx.settled(check_id):
            ctx.skip(check_id, reason)

    def add(check_id: str, url: Any, details: dict[str, Any], source: str) -> None:
        if not isinstance(url, str) or not url:
            return
        if not ctx.enabled(check_id):
            return
        ctx.add(
            check_id,
            target_url=url,
            details=details,
            source=source,
        )

    if evidence.get("state") == "unavailable" and not evidence.get("sources"):
        reason = evidence.get("reason") or "no validated project-bound provider history"
        for check_id in (*GSC_ANALYTICS_CHECKS, *GSC_INSPECTION_CHECKS):
            skip(check_id, reason)
        for mapping in ANALYTICS_CHECKS.values():
            for check_id in mapping.values():
                skip(check_id, reason)
        return

    sources = evidence.get("sources") if isinstance(evidence.get("sources"), dict) else {}
    gsc = sources.get("gsc") if isinstance(sources.get("gsc"), dict) else {}
    search = gsc.get("findings") if isinstance(gsc.get("findings"), dict) else {}
    search_skipped = search.get("_skipped") if isinstance(search.get("_skipped"), dict) else {}
    search_coverage = gsc.get("coverage") if isinstance(gsc.get("coverage"), dict) else {}
    source_detail = {
        "provider": "gsc",
        "operation": "search_analytics",
        "period": search_coverage.get("period"),
        "coverage": search_coverage,
        "evidence_scope": "retained project-bound Search Analytics history",
    }
    for check_id in GSC_ANALYTICS_CHECKS:
        found = search.get(check_id)
        found = found if isinstance(found, list) else []
        truncated = bool(search.get("truncated_findings"))
        for item in found[:MAX_ISSUES_PER_PROVIDER_CHECK]:
            if not isinstance(item, dict):
                continue
            details = item.get("details") if isinstance(item.get("details"), dict) else {}
            add(
                check_id,
                item.get("url"),
                {
                    **source_detail,
                    **details,
                    "safe_url": item.get("safe_url"),
                    "url_id": item.get("url_id"),
                    "evidence_truncated": truncated,
                },
                "retained GSC Search Analytics",
            )
        reason = search_skipped.get(check_id)
        if reason and not found:
            skip(check_id, str(reason))
        elif gsc.get("state") in {"unavailable", "not_configured"} and not found:
            skip(check_id, str(gsc.get("reason") or "Search Console history is unavailable"))
        elif gsc.get("state") != "complete" and not found:
            skip(
                check_id,
                "retained Search Console source coverage is partial; absence is not established",
            )

    inspection = gsc.get("inspection") if isinstance(gsc.get("inspection"), dict) else {}
    inspection_findings = (
        inspection.get("findings")
        if isinstance(inspection.get("findings"), dict)
        else {}
    )
    inspection_details = {
        "provider": "gsc",
        "operation": "url_inspection",
        "scope": inspection.get("scope"),
        "requests_made": inspection.get("requests_made"),
        "inspected": inspection.get("inspected"),
        "crawl_pages": inspection.get("crawl_pages"),
        "crawl_urls_inspected": inspection.get("crawl_urls_inspected"),
        "sample_population_complete": inspection.get("sample_population_complete"),
        "failed": inspection.get("failed"),
        "deferred": inspection.get("deferred"),
    }
    for check_id in GSC_INSPECTION_CHECKS:
        found = inspection_findings.get(check_id)
        found = found if isinstance(found, list) else []
        for item in found[:MAX_ISSUES_PER_PROVIDER_CHECK]:
            if not isinstance(item, dict):
                continue
            details = item.get("details") if isinstance(item.get("details"), dict) else {}
            add(
                check_id,
                item.get("url"),
                {
                    **inspection_details,
                    **details,
                    "safe_url": item.get("safe_url"),
                    "url_id": item.get("url_id"),
                },
                "sampled Search Console URL Inspection",
            )
        sample_incomplete = bool(
            not inspection.get("inspected")
            or inspection.get("failed")
            or inspection.get("deferred")
            or not inspection.get("sample_population_complete")
        )
        if not found and sample_incomplete:
            skip(
                check_id,
                str(
                    inspection.get("reason")
                    or "no complete Search Console URL Inspection sample covers this check"
                ),
            )

    for provider, mapping in ANALYTICS_CHECKS.items():
        data = sources.get(provider) if isinstance(sources.get(provider), dict) else {}
        findings = data.get("findings") if isinstance(data.get("findings"), dict) else {}
        coverage = data.get("coverage") if isinstance(data.get("coverage"), dict) else {}
        for source_check, issue_check in mapping.items():
            item = findings.get(source_check)
            item = item if isinstance(item, dict) else {}
            urls = item.get("urls") if isinstance(item.get("urls"), list) else []
            safe_urls = item.get("safe_urls") if isinstance(item.get("safe_urls"), list) else []
            url_ids = item.get("url_ids") if isinstance(item.get("url_ids"), list) else []
            detail = {
                "provider": provider,
                "period": coverage.get("period"),
                "coverage": coverage,
                "evidence_scope": f"retained project-bound {provider} landing-page history",
                "finding_state": item.get("state"),
                "unreadable_metrics": item.get("unreadable", 0),
                "evidence_truncated": bool(data.get("truncated_findings")),
            }
            for index, url in enumerate(urls[:MAX_ISSUES_PER_PROVIDER_CHECK]):
                add(
                    issue_check,
                    url,
                    {
                        **detail,
                        "safe_url": safe_urls[index] if index < len(safe_urls) else None,
                        "url_id": url_ids[index] if index < len(url_ids) else None,
                    },
                    f"retained {provider} history",
                )
            if (
                item.get("state") in {"withheld", "skipped", "partial"}
                or item.get("unreadable", 0)
            ) and not urls:
                skip(issue_check, str(item.get("reason") or f"{provider} evidence is incomplete"))
            elif data.get("state") in {"not_configured", "unavailable"} and not urls:
                skip(issue_check, str(data.get("reason") or f"{provider} history is unavailable"))
            elif data.get("state") != "complete" and not urls:
                skip(
                    issue_check,
                    f"retained {provider} coverage is partial; absence is not established",
                )
