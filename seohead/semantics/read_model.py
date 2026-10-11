"""Offline cluster, landing and rule-evidence read models for a semantic core."""

from __future__ import annotations

import json
import re
import sqlite3
from collections import defaultdict
from datetime import date, datetime, timedelta, timezone
from urllib.parse import urlsplit

from seohead.semantics import config
from seohead.semantics.norm import normalize

MAX_PAGE_SIZE = 100
MAX_INSIGHT_ROWS = 100_000
MAX_INSIGHT_DAYS = 800
SOURCE_METRICS = {
    "gsc": ("clicks", "impressions", "ctr", "position"),
    "metrika": ("visits", "users", "bounce_rate"),
    "webmaster": ("shows", "clicks", "show_position", "click_position"),
}
RULES = (
    {
        "rule_id": "landing.unassigned",
        "threshold": {"field": "landing_url", "operator": "is_null"},
        "evidence": ["semantic_core.clusters.landing_url", "landing_candidates"],
        "recommendation": "Review the measured candidate URLs and assign the cluster landing page.",
    },
    {
        "rule_id": "search.impressions_without_clicks",
        "threshold": {"field": "clicks", "operator": "equals", "value": 0, "requires": "impressions > 0"},
        "evidence": ["gsc.clicks", "gsc.impressions", "webmaster.clicks", "webmaster.shows"],
        "recommendation": "Review the cluster landing page and search-result targeting for measured impressions without clicks.",
    },
)


def _cluster(store, reference):
    if str(reference).isdecimal():
        row = store.db.execute("SELECT * FROM clusters WHERE id=?", (int(reference),)).fetchone()
    else:
        row = store.db.execute("SELECT * FROM clusters WHERE name=?", (reference,)).fetchone()
    if row is None:
        raise ValueError(f"cluster not found: {reference}")
    return row


def clusters(store, *, offset=0, limit=50):
    _page(offset, limit)
    cluster_columns = {row["name"] for row in store.db.execute("PRAGMA table_info(clusters)")}
    unavailable_cluster_fields = [
        field for field in ("intent", "source", "coherence", "landing_url")
        if field not in cluster_columns
    ]
    rows = store.cluster_rows(offset=offset, limit=limit + 1)
    items = []
    for row in rows[:limit]:
        item = dict(row)
        item["unavailable_fields"] = unavailable_cluster_fields
        try:
            candidate = store.db.execute(
                "SELECT score FROM landing_candidates WHERE cluster_id=? AND url=?",
                (item["id"], item.get("landing_url") or ""),
            ).fetchone()
        except sqlite3.OperationalError:
            candidate = None
        item["landing_score"] = candidate[0] if candidate else None
        item["landing_score_unavailable_reason"] = None if candidate else "no stored score for the selected landing URL"
        items.append(item)
    return {
        "format": "seohead.semantics.clusters.v1",
        "items": items,
        "offset": offset,
        "limit": limit,
        "total": store.db.execute("SELECT COUNT(*) FROM clusters").fetchone()[0],
        "has_more": len(rows) > limit,
    }


def cluster_show(store, reference):
    cluster = _cluster(store, reference)
    aggregate_row = store.cluster_row(cluster["id"])
    aggregate = dict(aggregate_row) if aggregate_row else None
    try:
        candidates = [
            {**dict(row), "evidence": json.loads(row["evidence_json"])}
            for row in store.db.execute(
                "SELECT url,score,evidence_json,computed_at FROM landing_candidates "
                "WHERE cluster_id=? ORDER BY score DESC, url LIMIT 25",
                (cluster["id"],),
            )
        ]
    except sqlite3.OperationalError:
        candidates = []
    cluster_data = dict(cluster)
    cluster_columns = {row["name"] for row in store.db.execute("PRAGMA table_info(clusters)")}
    cluster_data.setdefault("landing_url", None)
    return {
        "format": "seohead.semantics.cluster.v1",
        "cluster": cluster_data,
        "landing_url_unavailable_reason": "landing URLs are unavailable in this legacy database" if "landing_url" not in cluster_columns else None,
        "aggregate": aggregate,
        "landing_candidates": candidates,
        "landing_candidates_unavailable_reason": None if candidates else "candidate scores have not been computed from retained SERP evidence",
    }


def cluster_queries(store, reference, *, offset=0, limit=50, project=None):
    _page(offset, limit)
    cluster = _cluster(store, reference)
    total = store.db.execute(
        "SELECT COUNT(*) FROM phrases WHERE cluster_id=? AND status IN ('kept','review')",
        (cluster["id"],),
    ).fetchone()[0]
    available = {row["name"] for row in store.db.execute("PRAGMA table_info(phrases)")}
    fields = ("norm", "base", "quoted", "exact", "impr", "pos_y", "pos_g", "status", "route", "group_name", "subgroup", "url", "comment")
    present = [field for field in fields if field in available]
    rows = store.db.execute(
        f"SELECT {','.join(present)} FROM phrases WHERE cluster_id=? AND status IN ('kept','review') "
        "ORDER BY base DESC,id LIMIT ? OFFSET ?",
        (cluster["id"], limit + 1, offset),
    ).fetchall()
    items = []
    has_receipts = store.db.execute(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='serp_fetches'"
    ).fetchone()
    receipt_time_expr = "fetched_at" if has_receipts and "fetched_at" in {
        row["name"] for row in store.db.execute("PRAGMA table_info(serp_fetches)")
    } else "NULL AS fetched_at"
    configured_region = None
    if project is not None:
        configured_region = str((config.load(project).get("regions") or ["225"])[0])
    for row in rows[:limit]:
        item = {**{field: None for field in fields}, **dict(row)}
        receipt = None
        any_receipt = False
        if has_receipts:
            any_receipt = bool(store.db.execute(
                "SELECT 1 FROM serp_fetches WHERE phrase_norm=? AND top=10 LIMIT 1",
                (item["norm"],),
            ).fetchone())
            if configured_region:
                receipt = store.db.execute(
                    f"SELECT region,top,state,raw_json,{receipt_time_expr} FROM serp_fetches "
                    "WHERE phrase_norm=? AND region=? AND top=10",
                    (item["norm"], configured_region),
                ).fetchone()
            else:
                receipt = store.db.execute(
                    f"SELECT region,top,state,raw_json,{receipt_time_expr} FROM serp_fetches "
                    "WHERE phrase_norm=? AND top=10 ORDER BY region LIMIT 1",
                    (item["norm"],),
                ).fetchone()
        legacy_fetched = None
        if not any_receipt:
            legacy_fetched = store.db.execute(
                "SELECT MAX(fetched_at) FROM serp WHERE phrase_norm=?", (item["norm"],)
            ).fetchone()[0]
        results = []
        if receipt and receipt["state"] == "complete" and receipt["raw_json"]:
            try:
                results = json.loads(receipt["raw_json"]).get("docs", [])[:10]
            except (TypeError, ValueError):
                results = []
        elif not receipt and not any_receipt:
            results = [dict(result) for result in store.db.execute(
                "SELECT pos,url,domain,title FROM serp WHERE phrase_norm=? ORDER BY pos LIMIT 10",
                (item["norm"],),
            )]
        item["yandex_top10"] = {
            "results": results,
            "region": receipt["region"] if receipt else None,
            "fetched_at": receipt["fetched_at"] if receipt else legacy_fetched,
            "state": receipt["state"] if receipt else ("legacy_cache" if results else "unavailable"),
            "unavailable_reason": None if results else (
                f"no retained SERP receipt for configured region {configured_region}"
                if any_receipt and not receipt
                else f"SERP receipt state is {receipt['state']}" if receipt
                else "no retained Yandex TOP-10 evidence"
            ),
            "region_unavailable_reason": "legacy SERP rows do not retain region" if results and not receipt else None,
            "fetch_date_unavailable_reason": "this retained receipt predates fetch timestamps" if receipt and not receipt["fetched_at"] else None,
        }
        items.append(item)
    return {
        "format": "seohead.semantics.cluster-queries.v1",
        "cluster": {"id": cluster["id"], "name": cluster["name"]},
        "items": items,
        "unavailable_fields": [field for field in fields if field not in available],
        "offset": offset,
        "limit": limit,
        "total": total,
        "has_more": len(rows) > limit,
    }


def refresh_landing_candidates(store, own_domain):
    """Persist URL coverage ratios only when the project's own domain is known."""
    own = (own_domain or "").lower().removeprefix("www.").rstrip(".")
    if not own:
        store.db.execute("DELETE FROM landing_candidates")
        store.commit()
        return {"state": "unavailable", "reason": "project domain is not configured", "clusters": 0}
    store.db.execute("DELETE FROM landing_candidates")
    clusters_rows = store.db.execute(
        "SELECT id FROM clusters ORDER BY id"
    ).fetchall()
    for cluster in clusters_rows:
        phrases = store.db.execute(
            "SELECT p.norm FROM phrases p WHERE p.cluster_id=? AND p.status IN ('kept','review')",
            (cluster["id"],),
        ).fetchall()
        total = len(phrases)
        if not total:
            continue
        candidates: dict[str, dict] = {}
        for phrase in phrases:
            for row in store.db.execute(
                "SELECT url,pos,domain FROM serp WHERE phrase_norm=? AND pos BETWEEN 1 AND 10",
                (phrase[0],),
            ):
                parsed = urlsplit(row["url"] or "")
                host = (row["domain"] or parsed.hostname or "").lower().removeprefix("www.").rstrip(".")
                if host != own and not host.endswith("." + own):
                    continue
                item = candidates.setdefault(row["url"], {"phrases": set(), "positions": []})
                item["phrases"].add(phrase[0])
                if row["pos"] is not None:
                    item["positions"].append(row["pos"])
        for url, evidence in candidates.items():
            score = len(evidence["phrases"]) / total
            details = {
                "method": "serp_phrase_coverage.v1",
                "matched_phrases": len(evidence["phrases"]),
                "cluster_phrases": total,
                "positions": evidence["positions"],
                "score_interpretation": "share of cluster phrases where this own-domain URL appeared in retained top 10",
            }
            store.db.execute(
                "INSERT OR REPLACE INTO landing_candidates(cluster_id,url,score,evidence_json,computed_at) "
                "VALUES(?,?,?,?,?)",
                (
                    cluster["id"], url, score,
                    json.dumps(details, ensure_ascii=False),
                    datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z"),
                ),
            )
    store.commit()
    return {"state": "available", "domain": own, "clusters": len(clusters_rows)}


def _page(offset, limit):
    if type(offset) is not int or offset < 0:
        raise ValueError("offset must be a non-negative integer")
    if type(limit) is not int or not 1 <= limit <= MAX_PAGE_SIZE:
        raise ValueError(f"limit must be between 1 and {MAX_PAGE_SIZE}")


def _period(value):
    match = re.fullmatch(r"(\d{4}-\d{2}-\d{2})\.\.(\d{4}-\d{2}-\d{2})", value or "")
    if not match:
        raise ValueError("period must use YYYY-MM-DD..YYYY-MM-DD")
    start, end = (date.fromisoformat(part) for part in match.groups())
    if start > end:
        raise ValueError("period start must not be after period end")
    if (end - start).days > MAX_INSIGHT_DAYS:
        raise ValueError(f"period must not exceed {MAX_INSIGHT_DAYS} days")
    return start, end


def _week(value):
    day = date.fromisoformat(value)
    return (day - timedelta(days=day.weekday())).isoformat()


def _source_rows(root, source, start, end):
    path = root / "sources.sqlite"
    if not path.is_file():
        return [], {"state": "unavailable", "reason": "project sources.sqlite is missing", "resources": []}
    if not (root / "project.json").is_file():
        return [], {"state": "unavailable", "reason": "project source bindings are unavailable for a standalone semantic core", "resources": []}
    from seohead.data_sources import sources_db
    from seohead.projects.bindings import list_bindings

    bindings = list_bindings(root)["bindings"]
    resources = sorted({item["resource"] for item in bindings if item["service"] == source})
    if not resources:
        return [], {"state": "unavailable", "reason": "no resource is bound to this project", "resources": []}
    rows, capped = [], False
    for resource in resources:
        remaining = MAX_INSIGHT_ROWS - len(rows)
        if remaining <= 0:
            capped = True
            break
        result = sources_db.query(
            path,
            source,
            resource=resource,
            start_date=start.isoformat(),
            end_date=end.isoformat(),
            limit=remaining,
        )
        rows.extend(result["rows"])
        capped |= result["has_more"]
    status = sources_db.status(path)
    ledger = {
        (entry["source"], entry["resource"]): entry
        for entry in status["sources"]
        if entry["source"] == source and entry["resource"] in resources
    }
    return rows, {
        "state": "partial" if capped else "available",
        "reason": "provider row limit reached; values may be incomplete" if capped else None,
        "resources": resources,
        "coverage": {resource: ledger.get((source, resource), {}).get("coverage", {}) for resource in resources},
        "last_fetched_at": {
            resource: ledger.get((source, resource), {}).get("last_fetched_at") for resource in resources
        },
    }


def _matches(source, row, phrases, landing):
    if source == "gsc":
        page = row.get("page")
        known = {landing}
        known.update(phrase.get("url") for phrase in phrases if phrase.get("url"))
        query = normalize(row.get("query"))
        query_match = bool(query and any(query == phrase["norm"] for phrase in phrases))
        page_match = bool(page and page in known)
        return query_match and (not page or page_match) if query else page_match
    if source == "webmaster":
        query = normalize(row.get("query"))
        return bool(query and any(query == phrase["norm"] for phrase in phrases))
    if source == "metrika":
        page = row.get("landing_page")
        known = {landing}
        known.update(phrase.get("url") for phrase in phrases if phrase.get("url"))
        known_paths = {urlsplit(value).path for value in known if value and "://" in value}
        if page in known or page in known_paths:
            return True
        parsed = urlsplit(landing or "")
        return bool(page and parsed.path and page == parsed.path)
    return False


def _metric_value(source, metric, rows):
    values = [row[metric] for row in rows if row.get(metric) is not None]
    if not values:
        return {"value": None, "unavailable_reason": "no matched, complete provider measurements for this week"}
    if metric == "users" and source == "metrika":
        if len(rows) == 1 and len(values) == 1:
            return {"value": values[0], "aggregation": "single_observation"}
        return {"value": None, "unavailable_reason": "Metrika users are non-additive across landing pages and traffic sources"}
    if metric in {"position", "show_position", "click_position", "ctr", "bounce_rate"}:
        weight_name = "impressions" if source == "gsc" else "shows" if source == "webmaster" and metric == "show_position" else "clicks" if source == "webmaster" else "visits"
        weighted = [(row.get(metric), row.get(weight_name)) for row in rows if row.get(metric) is not None and row.get(weight_name) is not None]
        if not weighted or sum(weight for _, weight in weighted) == 0:
            if len(rows) == 1 and len(values) == 1:
                return {"value": values[0], "aggregation": "single_observation"}
            return {"value": None, "unavailable_reason": f"{metric} is non-additive and has no measured {weight_name} weights"}
        weight_total = sum(weight for _, weight in weighted)
        return {"value": sum(value * weight for value, weight in weighted) / weight_total, "aggregation": f"weighted_by_{weight_name}"}
    return {"value": sum(values), "aggregation": "sum_of_measured_rows"}


def insights(store, project, period, sources):
    start, end = _period(period)
    selected = list(dict.fromkeys(sources))
    unknown = set(selected) - set(SOURCE_METRICS)
    if unknown:
        raise ValueError("sources must be selected from gsc, metrika, webmaster")
    directory = config.data_dir(project)
    root = directory.parent if directory.name == "semantics" else directory
    provider_rows, source_status = {}, {}
    for source in selected:
        try:
            provider_rows[source], source_status[source] = _source_rows(root, source, start, end)
        except (OSError, ValueError, sqlite3.Error) as error:
            provider_rows[source] = []
            source_status[source] = {
                "state": "unavailable",
                "reason": f"local provider history is unreadable: {error}",
                "resources": [],
            }
    clusters = []
    phrase_columns = {row["name"] for row in store.db.execute("PRAGMA table_info(phrases)")}
    cluster_columns = {row["name"] for row in store.db.execute("PRAGMA table_info(clusters)")}
    url_expr = "url" if "url" in phrase_columns else "NULL AS url"
    landing_column_available = "landing_url" in cluster_columns
    for cluster in store.db.execute("SELECT * FROM clusters ORDER BY id"):
        landing_url = cluster["landing_url"] if landing_column_available else None
        phrases = [
            dict(row)
            for row in store.db.execute(
                f"SELECT norm,{url_expr} FROM phrases WHERE cluster_id=? AND status IN ('kept','review')",
                (cluster["id"],),
            )
        ]
        signals = {}
        for source in selected:
            grouped = defaultdict(list)
            for row in provider_rows[source]:
                if not _matches(source, row, phrases, landing_url):
                    continue
                if row.get("coverage_state") != "complete":
                    continue
                grouped[_week(row["date"])].append(row)
            weeks, cursor = [], start - timedelta(days=start.weekday())
            while cursor <= end:
                matched = grouped.get(cursor.isoformat(), [])
                metrics = {
                    metric: (
                        {"value": None, "unavailable_reason": source_status[source]["reason"]}
                        if source_status[source]["state"] == "partial"
                        else _metric_value(source, metric, matched)
                    )
                    for metric in SOURCE_METRICS[source]
                }
                weeks.append({
                    "week_start": cursor.isoformat(),
                    "row_count": len(matched),
                    "coverage_state": "partial" if source_status[source]["state"] == "partial" else "complete" if matched else "unavailable",
                    "unavailable_reason": source_status[source]["reason"] if source_status[source]["state"] == "partial" else None if matched else "no matched rows; absence is not a measured zero",
                    "metrics": metrics,
                })
                cursor += timedelta(days=7)
            signals[source] = {
                "source_state": source_status[source]["state"],
                "source_unavailable_reason": source_status[source]["reason"],
                "weeks": weeks,
            }
        try:
            candidates = [
                dict(row)
                for row in store.db.execute(
                    "SELECT url,score,computed_at FROM landing_candidates "
                    "WHERE cluster_id=? ORDER BY score DESC,url LIMIT 5",
                    (cluster["id"],),
                )
            ]
        except sqlite3.OperationalError:
            candidates = []
        cluster_result = {"id": cluster["id"], "name": cluster["name"], "landing_url": landing_url, "signals": signals}
        cluster_result["landing_url_state"] = "measured" if landing_column_available else "unavailable"
        cluster_result["landing_url_unavailable_reason"] = None if landing_column_available else "landing URLs are unavailable in this legacy database"
        cluster_result["landing_candidates"] = candidates
        cluster_result["landing_candidates_unavailable_reason"] = None if candidates else "no stored candidate scores for this cluster"
        cluster_result["rules"] = _evaluate_rules(cluster_result)
        clusters.append(cluster_result)
    return {
        "format": "seohead.semantics.insights.v1",
        "period": {"start": start.isoformat(), "end": end.isoformat()},
        "sources": source_status,
        "rules": list(RULES),
        "clusters": clusters,
        "read_only": True,
        "provider_calls_made": 0,
    }


def _evaluate_rules(cluster):
    results = []
    if cluster.get("landing_url_state") == "unavailable":
        results.append({
            "rule_id": "landing.unassigned",
            "state": "unavailable",
            "evidence": None,
            "unavailable_reason": cluster["landing_url_unavailable_reason"],
        })
    elif cluster["landing_url"]:
        results.append({"rule_id": "landing.unassigned", "state": "not_matched", "evidence": {"landing_url": cluster["landing_url"]}})
    else:
        results.append({
            "rule_id": "landing.unassigned",
            "state": "matched",
            "threshold": RULES[0]["threshold"],
            "evidence": {
                "landing_url": None,
                "candidate_urls": cluster["landing_candidates"],
                "candidates_unavailable_reason": cluster["landing_candidates_unavailable_reason"],
            },
            "recommendation": RULES[0]["recommendation"],
        })
    measured = []
    for source in ("gsc", "webmaster"):
        signal = cluster["signals"].get(source)
        if not signal:
            continue
        click_key, impression_key = ("clicks", "impressions") if source == "gsc" else ("clicks", "shows")
        for week in signal["weeks"]:
            clicks = week["metrics"][click_key]["value"]
            impressions = week["metrics"][impression_key]["value"]
            if clicks is not None and impressions is not None:
                measured.append({"source": source, "week_start": week["week_start"], "clicks": clicks, "impressions": impressions})
    if any(row["impressions"] > 0 and row["clicks"] == 0 for row in measured):
        results.append({
            "rule_id": "search.impressions_without_clicks",
            "state": "matched",
            "threshold": RULES[1]["threshold"],
            "evidence": [row for row in measured if row["impressions"] > 0 and row["clicks"] == 0],
            "recommendation": RULES[1]["recommendation"],
        })
    elif measured:
        results.append({"rule_id": "search.impressions_without_clicks", "state": "not_matched", "evidence": measured})
    else:
        results.append({"rule_id": "search.impressions_without_clicks", "state": "unavailable", "evidence": None, "unavailable_reason": "click and impression measurements are unavailable for this period"})
    return results


def create_insight_tasks(
    project,
    period,
    sources,
    selections,
    *,
    expected_revision=None,
    dry_run=False,
    confirm=False,
    insight=None,
    mcp_call=False,
):
    """Preview or add explicitly selected matched rules to the existing project checklist."""
    directory = config.data_dir(project)
    root = directory.parent if directory.name == "semantics" else directory
    if not (root / "project.json").is_file():
        raise ValueError("insight tasks require a SEOHEAD project workspace")
    from seohead.projects.coverage import coverage_status, update_item

    checklist = coverage_status(root, _verify_evidence=False)
    if checklist.get("state") == "not_initialized":
        raise ValueError("initialize the project checklist before creating insight tasks")
    revision = checklist.get("revision")
    if expected_revision is not None and expected_revision != revision:
        raise ValueError(f"checklist revision changed: expected {expected_revision}, current {revision}")
    if not dry_run and type(expected_revision) is not int:
        raise ValueError("expected_revision is required for an insight task write")
    if not dry_run and not confirm:
        raise ValueError("pass confirm=true to write selected insight tasks")
    if not dry_run and mcp_call:
        from seohead.projects.permissions import require_mcp_allowed

        require_mcp_allowed("project.checklist.update")
    insight = insight or insights_from_project(project, period, sources)
    by_cluster = {str(item["id"]): item for item in insight["clusters"]}
    selected, seen = [], set()
    for item in selections:
        if not isinstance(item, dict) or set(item) != {"rule_id", "cluster_id"}:
            raise ValueError("each selection requires rule_id and cluster_id")
        rule_id, cluster_id = item["rule_id"], str(item["cluster_id"])
        key = (rule_id, cluster_id)
        if key in seen:
            continue
        seen.add(key)
        cluster = by_cluster.get(cluster_id)
        if cluster is None:
            raise ValueError(f"cluster not found in insight result: {cluster_id}")
        rule = next((row for row in cluster["rules"] if row["rule_id"] == rule_id), None)
        if rule is None or rule["state"] != "matched":
            state = rule["state"] if rule else "unknown_rule"
            raise ValueError(f"selected rule {rule_id} is not matched for cluster {cluster_id}: {state}")
        slug = re.sub(r"[^a-z0-9._/-]+", "-", rule_id.lower()).strip("-./")
        task_id = f"custom:semantic-{slug}-cluster-{cluster_id}"
        rule_evidence = rule.get("evidence")
        if isinstance(rule_evidence, list):
            evidence = {
                "items": rule_evidence[:5],
                "item_count": len(rule_evidence),
                "omitted_count": max(0, len(rule_evidence) - 5),
            }
        elif isinstance(rule_evidence, dict) and isinstance(rule_evidence.get("candidate_urls"), list):
            candidates = rule_evidence["candidate_urls"]
            evidence = {
                **rule_evidence,
                "candidate_urls": [
                    {"url": str(item.get("url", ""))[:256], "score": item.get("score")}
                    for item in candidates[:3]
                ],
                "candidate_count": len(candidates),
                "candidates_omitted": max(0, len(candidates) - 3),
            }
        else:
            evidence = rule_evidence
        evidence = {
            "rule_id": rule_id,
            "cluster_id": cluster["id"],
            "cluster": str(cluster["name"])[:256],
            "period": insight["period"],
            "sources": list(sources),
            "evidence": evidence,
        }
        selected.append(
            {
                "id": task_id,
                "title": f"Review {rule_id.replace('.', ' ')}: {str(cluster['name'])[:450]}",
                "description": json.dumps(evidence, ensure_ascii=False, sort_keys=True),
                "acceptance_criterion": rule.get("recommendation") or "Review the recorded evidence and document the decision.",
                "evidence": evidence,
            }
        )
    if not selected:
        raise ValueError("select at least one matched rule and cluster pair")
    if dry_run:
        return {
            "contract": "seohead.semantics.operation.v1",
            "mode": "preview",
            "format": "seohead.semantics.insight-tasks.v1",
            "state": "preview",
            "expected_revision": revision,
            "tasks": selected,
            "written": 0,
        }
    written, current_revision = [], revision
    for task in selected:
        definition = {key: value for key, value in task.items() if key != "evidence"}
        updated = update_item(root, definition, current_revision)
        current_revision = updated["revision"]
        written.append(task["id"])
    action_log_state = "unavailable"
    action_log_reason = "project action journal is unavailable"
    try:
        from seohead.projects.action_log import record_interface_action

        record_interface_action(
            root,
            action="project.checklist.update",
            outcome="applied",
            detail=f"Created or updated {len(written)} explicitly selected semantic insight checklist task(s)",
        )
        action_log_state, action_log_reason = "recorded", None
    except (ImportError, OSError, ValueError) as error:
        action_log_state, action_log_reason = "failed", str(error)
    return {
        "contract": "seohead.semantics.operation.v1",
        "mode": "write",
        "format": "seohead.semantics.insight-tasks.v1",
        "state": "written",
        "revision": current_revision,
        "tasks": written,
        "written": len(written),
        "executed": 0,
        "action_log_state": action_log_state,
        "action_log_unavailable_reason": action_log_reason,
    }


def insights_from_project(project, period, sources):
    # Store is needed only for the local semantic SQLite read; re-open without migration.
    from seohead.semantics.runner import _store

    with _store(project, readonly=True) as store:
        return insights(store, project, period, sources)


def execute_read(store, project, stage, args):
    if stage == "clusters":
        result = clusters(store, offset=getattr(args, "offset", 0), limit=getattr(args, "limit", 50))
    elif stage == "cluster-show":
        result = cluster_show(store, args.cluster)
    elif stage == "cluster-queries":
        result = cluster_queries(store, args.cluster, offset=args.offset, limit=args.limit, project=project)
    elif stage == "set-landing":
        cluster = _cluster(store, args.cluster)
        url = args.url.strip() or None
        if url and ("\n" in url or "\r" in url or "\x00" in url):
            raise ValueError("landing URL must not contain control characters")
        store.set_landing(cluster["id"], url)
        store.commit()
        result = {"format": "seohead.semantics.landing-update.v1", "cluster_id": cluster["id"], "landing_url": url, "audit_recorded": True}
    elif stage == "insights":
        result = insights(store, project, args.period, [s.strip() for s in args.sources.split(",") if s.strip()])
    elif stage == "insights-tasks":
        source_list = [s.strip() for s in args.sources.split(",") if s.strip()]
        insight = insights(store, project, args.period, source_list)
        result = create_insight_tasks(
            project,
            args.period,
            source_list,
            args.selections,
            expected_revision=getattr(args, "expected_revision", None),
            dry_run=getattr(args, "dry_run", False),
            confirm=getattr(args, "confirm", False),
            insight=insight,
            mcp_call=getattr(args, "_mcp_call", False),
        )
    else:
        raise ValueError(f"unsupported semantic read command: {stage}")
    return result
