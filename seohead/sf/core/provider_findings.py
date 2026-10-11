"""Join retained project provider history to one SF audit without fetching sources.

Provider history is an independent measurement: collection gaps stay partial, zero stays measured,
and the result lives beside (rather than inside) the technical issue score.
"""

from __future__ import annotations

from datetime import date, datetime, timedelta, timezone
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

MAX_PROVIDER_ROWS = 100_000
MAX_FINDING_URLS = 500
MAX_CRAWL_PAGES = 100_000


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


def _analytics_rows(source: str, history: dict[str, Any], origin: str | None) -> tuple[list[dict], str, str | None]:
    totals: dict[str, dict[str, Any]] = {}
    visit_column = "sessions" if source == "ga4" else "visits"
    for row in history["rows"]:
        url = _url(row.get("landing_page"), origin)
        if url is None:
            continue
        item = totals.setdefault(url, {"url": url, visit_column: 0.0, "bounce_weighted": 0.0, "bounce_weight": 0.0})
        visits = row.get(visit_column)
        if isinstance(visits, (int, float)) and not isinstance(visits, bool) and visits >= 0:
            item[visit_column] += visits
            bounce = row.get("bounce_rate")
            if source == "metrika" and isinstance(bounce, (int, float)) and not isinstance(bounce, bool) and 0 <= bounce <= 1 and visits > 0:
                item["bounce_weighted"] += bounce * visits
                item["bounce_weight"] += visits
    rows = []
    for item in totals.values():
        row = {"url": item["url"], visit_column: item[visit_column]}
        if source == "metrika":
            row["bounce_rate"] = (
                item["bounce_weighted"] / item["bounce_weight"]
                if item["bounce_weight"]
                else None
            )
        rows.append(row)
    return rows, visit_column, "bounce_rate" if source == "metrika" else None


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
            item = {**item, "urls": urls[:MAX_FINDING_URLS], "omitted_urls": len(urls)-MAX_FINDING_URLS}
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
    summary["findings"], summary["truncated_findings"] = _cap_findings(summary["findings"])
    return {
        "state": collection_state,
        "coverage": "bounded URL sample; unsampled pages are unknown",
        "daily_budget": quota,
        "requests_made": requests_made,
        **summary,
    }


def _cloudflare_history(project_dir: str | Path, binding: dict[str, Any]) -> dict[str, Any]:
    from seohead.data_sources.cloudflare_history import list_snapshots

    result = list_snapshots(
        project=str(project_dir), zone_id=binding["resource"], limit=1, offset=0
    )
    if not result["rows"]:
        return {"state": "unavailable", "reason": "no retained Cloudflare aggregate snapshot"}
    snapshot = result["rows"][0]
    facts = snapshot.get("url_facts") or []
    omitted = max(0, len(facts) - MAX_FINDING_URLS)
    return {
        "state": snapshot.get("state", "partial"),
        "observed_at": snapshot.get("observed_at"),
        "period": snapshot.get("period"),
        "sampled": snapshot.get("sampled"),
        "truncated_days": snapshot.get("truncated_days") or [],
        "url_facts": facts[:MAX_FINDING_URLS],
        "omitted_url_facts": omitted,
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
        rows, visits_column, bounce_column = _analytics_rows(provider, history, origin)
        from seohead.checks.analytics_findings import analytics_findings
        from seohead.checks.external_join import join_external_data

        joined = join_external_data(pages, rows, url_column="url")
        reason = None if history["coverage_complete"] else "retained analytics coverage has gaps or is truncated"
        result = analytics_findings(
            pages,
            joined,
            partial=crawl_partial,
            evidence_complete=history["coverage_complete"],
            evidence_reason=reason,
            visits_column=visits_column,
            bounce_column=bounce_column,
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
        result["coverage"] = {key: value for key, value in history.items() if key != "rows"}
        result["findings"], truncated = _cap_findings(result["findings"])
        result["truncated_findings"] = truncated
        result["state"] = "complete" if history["coverage_complete"] else "partial"
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
        "crawl_pages_omitted": crawl_pages_omitted,
        "source": "project-bound provider history",
        "network_requests": bool(inspect_gsc),
        "sources": sources,
    }
