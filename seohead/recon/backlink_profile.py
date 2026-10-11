"""Merge explicit backlink evidence without fetching or guessing an index population.

CSV exports, provider samples and offline Common Crawl WAT/graph extracts have different
coverage. URL pairs are deduplicated, but each observation retains its own provenance and
rel tokens. A missing link in a later sample is never treated as a lost link.
"""

from __future__ import annotations

import csv
import json
import math
import os
import re
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from urllib.parse import urljoin, urlsplit, urlunsplit

from seohead.core.filesystem import stage_bytes
from seohead.recon.net import normalize_domain, normalize_url, registrable_domain

MAX_ROWS = 100_000
MAX_IMPORT_BYTES = 64 * 1024 * 1024
SOURCES = frozenset(
    {
        "generic",
        "gsc",
        "ahrefs",
        "majestic",
        "moz",
        "semrush",
        "dataforseo",
        "yandex_webmaster",
        "bing_webmaster",
        "common_crawl",
    }
)
_ALIASES = {
    "source_url": (
        "source_url",
        "url_from",
        "referring page url",
        "sourceurl",
        "source url",
        "source",
        "linking page",
        "linking page url",
        "referring page",
        "url",
    ),
    "target_url": (
        "target_url",
        "url_to",
        "destination_url",
        "target url",
        "targeturl",
        "target page",
        "target page url",
        "target",
        "destination",
    ),
    "anchor": ("anchor", "anchor text", "anchortext", "link text"),
    "rel": ("rel", "link rel", "rel tokens"),
    "nofollow": ("nofollow", "no follow", "is_nofollow", "flagnoffollow", "flagnofollow"),
    "sponsored": ("sponsored", "is_sponsored", "flagsponsored"),
    "ugc": ("ugc", "is_ugc", "flagugc"),
    "follow": ("follow", "dofollow", "is_dofollow"),
    "first_seen": (
        "first_seen_link",
        "first_seen",
        "first seen",
        "firstseen",
        "first indexed date",
        "firstindexeddate",
        "discovery_date",
        "date",
    ),
    "last_seen": (
        "last_visited",
        "last_seen",
        "last seen",
        "lastseen",
        "last check",
        "lastcheckdate",
        "source_last_access_date",
        "lastseendate",
    ),
    "lost_at": ("lost_at", "lost", "lost date", "date lost", "lost_date", "datelost"),
    "state": ("state", "link status", "status", "is_lost", "deleted", "deletedate", "flagdeleted"),
    "source_domain": (
        "source_domain",
        "source domain",
        "referring domain",
        "linking site",
        "domain",
        "sourcehost",
        "site",
    ),
    "target_domain": ("target_domain", "target domain", "targethost"),
}
_METRICS = {
    "provider_spam_score": ("spam_score", "backlink_spam_score", "spam score"),
    "domain_rating": ("domain_rating", "domain_rating_source", "domain rating", "dr"),
    "domain_authority": ("domain_authority", "domain authority", "da"),
    "trust_flow": ("trust flow", "trustflow", "trust_flow", "sourcetrustflow"),
    "citation_flow": ("citation flow", "citationflow", "citation_flow", "sourcecitationflow"),
    "external_links": (
        "external_links",
        "external links",
        "external links count",
        "external_links_count",
        "outboundlinks",
        "outbound links",
        "links_external",
        "sourceoutlinksexternal",
    ),
}


def _url(value: Any) -> str | None:
    if not isinstance(value, str) or not value.strip():
        return None
    value = normalize_url(value)
    if not value or any(ord(char) < 32 or char.isspace() for char in value):
        raise ValueError("backlink URL must be an absolute HTTP(S) URL without credentials")
    parsed = urlsplit(value)
    host = parsed.hostname or ""
    if not normalize_domain(host):
        raise ValueError("backlink URL must have a valid hostname")
    try:
        port = parsed.port
        host = host.encode("idna").decode("ascii").lower()
    except (ValueError, UnicodeError) as exc:
        raise ValueError("invalid backlink URL authority") from exc
    authority = host if port in (None, 80 if parsed.scheme == "http" else 443) else f"{host}:{port}"
    return urlunsplit((parsed.scheme.lower(), authority, parsed.path or "/", parsed.query, ""))


def _bool(value: Any) -> bool | None:
    if isinstance(value, bool):
        return value
    if value is None or str(value).strip() == "":
        return None
    if str(value).strip().lower() in {"1", "true", "yes", "y"}:
        return True
    if str(value).strip().lower() in {"0", "false", "no", "n"}:
        return False
    raise ValueError("link attribute flags must be boolean")


def _date(value: Any) -> str | None:
    if value is None or str(value).strip().lower() in {"", "-", "n/a", "null"}:
        return None
    raw = str(value).strip()
    try:
        return datetime.fromisoformat(raw.replace("Z", "+00:00")).date().isoformat()
    except ValueError:
        try:
            return datetime.strptime(raw, "%d/%m/%Y").date().isoformat()
        except ValueError as exc:
            raise ValueError("link dates must be ISO dates or DD/MM/YYYY") from exc


def _mapped(row: dict, columns: dict[str, str] | None = None) -> dict:
    if columns is not None and (
        not isinstance(columns, dict)
        or not all(
            isinstance(key, str) and isinstance(value, str) for key, value in columns.items()
        )
    ):
        raise ValueError("columns must map normalized field names to CSV headers")
    lowered = {str(key).strip().lower(): value for key, value in row.items()}
    result = {}
    for field, aliases in {**_ALIASES, **_METRICS}.items():
        keys = (columns[field].lower(),) if columns and field in columns else aliases
        for key in keys:
            if key in lowered:
                result[field] = lowered[key]
                break
    return result


def _observation(row: dict, source: str, observed_at: str, columns: dict | None) -> dict:
    fields = _mapped(row, columns)
    source_url = _url(fields.get("source_url"))
    target_url = _url(fields.get("target_url"))
    if not source_url:
        raise ValueError("backlink row requires source_url; aggregate exports are domain evidence")
    rel: list[str] | None = None
    if "rel" in fields:
        value = fields["rel"]
        if isinstance(value, list):
            if not all(isinstance(token, str) for token in value):
                raise ValueError("rel must contain string tokens")
            rel = sorted({token.lower() for token in value if token})
        elif isinstance(value, str):
            rel = sorted(set(re.split(r"[\s,;]+", value.lower().strip())) - {""})
        elif value is not None:
            raise ValueError("rel must be a token string or list")
    flags = {key: _bool(fields.get(key)) for key in ("nofollow", "sponsored", "ugc")}
    follow = _bool(fields.get("follow"))
    rel_complete = rel is not None
    qualification_complete = rel_complete or all(value is not None for value in flags.values())
    positive = {key for key, value in flags.items() if value is True}
    if rel is None and (positive or qualification_complete or follow is True):
        rel = sorted(positive)
        qualification_complete = qualification_complete or follow is True
    if rel is not None:
        rel = sorted(set(rel) | positive)
    lost_at = _date(fields.get("lost_at"))
    if source == "ahrefs" and _bool(row.get("is_lost")) is True and not lost_at:
        lost_at = _date(row.get("last_seen"))
    state = str(fields.get("state", "")).strip().lower()
    if state in {"true", "1", "lost", "deleted", "removed"} or lost_at:
        state = "lost"
    elif state in {"false", "0", "live", "active", "new"}:
        state = "live"
    elif state in {"", "unknown", "-"}:
        state = "unknown"
    else:
        # Some exports use a deletion date in the status column.
        lost_at, state = _date(state), "lost"
    first_seen, last_seen = _date(fields.get("first_seen")), _date(fields.get("last_seen"))
    if first_seen and last_seen and first_seen > last_seen:
        raise ValueError("first_seen must not be after last_seen")
    metrics = {}
    for field in _METRICS:
        value = fields.get(field)
        if value is None or str(value).strip() in {"", "-", "N/A"}:
            continue
        try:
            number = float(str(value).replace(",", ""))
        except ValueError as exc:
            raise ValueError("provider link metrics must be numeric") from exc
        if not math.isfinite(number) or number < 0:
            raise ValueError("provider link metrics must be finite and nonnegative")
        metrics[field] = number
    return {
        "source_url": source_url,
        "target_url": target_url,
        "source": source,
        "observed_at": observed_at,
        "anchor": str(fields.get("anchor") or ""),
        "rel": rel,
        "rel_complete": rel_complete,
        "qualification_complete": qualification_complete,
        "observed_follow": follow,
        "first_seen": first_seen,
        "last_seen": last_seen,
        "lost_at": lost_at,
        "state": state,
        "metrics": metrics,
    }


def _read_csv(path: Path) -> list[dict]:
    with path.open("rb") as stream:
        data = stream.read(MAX_IMPORT_BYTES + 1)
    if len(data) > MAX_IMPORT_BYTES:
        raise ValueError("backlink import exceeds 64 MiB")
    text = data.decode("utf-16" if data.startswith((b"\xff\xfe", b"\xfe\xff")) else "utf-8-sig")
    try:
        dialect = csv.Sniffer().sniff(text[:8192], delimiters=",;\t")
    except csv.Error:
        dialect = csv.excel
    import io

    reader = csv.DictReader(io.StringIO(text), dialect=dialect)
    if reader.fieldnames is None:
        raise ValueError("CSV import requires a header")
    rows = []
    for row in reader:
        if len(rows) >= MAX_ROWS:
            raise ValueError("backlink import exceeds 100000 rows")
        if None in row:
            raise ValueError("CSV row contains more columns than its header")
        rows.append(row)
    return rows


def _wat_rows(path: Path) -> list[dict]:
    if path.stat().st_size > MAX_IMPORT_BYTES:
        raise ValueError("Common Crawl WAT extract exceeds 64 MiB")
    rows = []
    with path.open(encoding="utf-8") as stream:
        for line in stream:
            envelope = json.loads(line)["Envelope"]
            donor = envelope["WARC-Header-Metadata"]["WARC-Target-URI"]
            links = envelope["Payload-Metadata"]["HTTP-Response-Metadata"]["HTML-Metadata"]["Links"]
            if not isinstance(links, list):
                raise ValueError("Common Crawl WAT Links must be a list")
            for link in links:
                if link.get("path", "").lower() != "a@/href":
                    continue
                rows.append(
                    {
                        "source_url": donor,
                        "target_url": urljoin(donor, link["url"]),
                        "anchor": link.get("text", ""),
                    }
                )
                if len(rows) > MAX_ROWS:
                    raise ValueError("Common Crawl WAT extract exceeds 100000 links")
    return rows


def fetch_backlink_source(source: str, domain: str, request: dict, *, transport=None) -> dict:
    """Explicit free-source request; the profile builder never invokes this function."""
    if source == "yandex_webmaster":
        from seohead.data_sources import yandex_webmaster

        operation = request.get("operation", "external_links_samples")
        if operation not in {"external_links_samples", "external_links_history"}:
            raise ValueError("unsupported Yandex backlink source operation")
        result = yandex_webmaster.collect(
            operation,
            **{key: value for key, value in request.items() if key != "operation"},
            transport=transport,
        )
        return {"source": source, "result": result, "scope": "verified property samples"}
    if source == "bing_webmaster":
        from seohead.data_sources import bing_webmaster

        operation = request.get("operation", "url_links")
        if operation not in {"url_links", "links"}:
            raise ValueError("unsupported Bing backlink source operation")
        result = bing_webmaster.collect(
            operation,
            **{key: value for key, value in request.items() if key != "operation"},
            transport=transport,
        )
        if operation == "url_links" and result.get("ok"):
            result["rows"] = [{**row, "target_url": request["link"]} for row in result["rows"]]
        return {"source": source, "result": result, "scope": "verified property samples"}
    raise ValueError("free backlink source must be yandex_webmaster or bing_webmaster")


def _source_rows(item: dict) -> tuple[list[dict], dict]:
    result = item.get("result", item)
    if not isinstance(result, dict):
        raise ValueError("source result must be an object")
    state = result.get("state", "partial")
    if result.get("ok") is False or state in {"failed", "skipped", "not_configured", "unavailable"}:
        return [], {
            "state": state,
            "complete": False,
            "reason": result.get("error", result.get("reason")),
        }
    operation = result.get("operation", item.get("operation"))
    if operation in {
        "backlinks_summary",
        "history",
        "domain_pages_summary",
        "external_links_history",
    } or (item.get("source") == "bing_webmaster" and operation == "links"):
        return [], {
            "state": "unavailable",
            "complete": False,
            "reason": "source returned aggregate metrics without URL pairs",
            "aggregate": result.get("data", result.get("items", result.get("rows"))),
        }
    rows = result.get("rows", result.get("items"))
    data = result.get("data")
    if rows is None and isinstance(data, dict):
        rows = data.get("links", data.get("items"))
    if rows is None and isinstance(data, list):
        rows = data
    if rows is None:
        return [], {
            "state": "unavailable",
            "complete": False,
            "reason": "source returned aggregate metrics without URL pairs",
            "aggregate": data,
        }
    if not isinstance(rows, list) or not all(isinstance(row, dict) for row in rows):
        raise ValueError("source rows must be a list of objects")
    if len(rows) > MAX_ROWS:
        raise ValueError("source exceeds 100000 rows")
    complete = item.get("complete", state == "complete") is True and not result.get(
        "truncated", False
    )
    return rows, {
        "state": "complete" if complete else "partial",
        "complete": complete,
        "rows_supplied": True,
    }


def _spam(observations: list[dict]) -> dict:
    """Transparent triage heuristic; never a search-engine penalty diagnosis."""
    reasons, score = [], 0.0
    metrics = [entry["metrics"] for entry in observations]
    reported = [entry["provider_spam_score"] for entry in metrics if "provider_spam_score" in entry]
    if reported:
        score = min(100.0, max(reported))
        reasons.append("provider_reported_spam_score")
    if any(entry.get("external_links", 0) >= 1000 for entry in metrics):
        score = max(score, 30.0)
        reasons.append("at_least_1000_external_links")
    if any(
        re.search(r"\b(?:casino|viagra|payday loan)\b", entry["anchor"], re.I)
        for entry in observations
    ):
        score = min(100.0, score + 30.0)
        reasons.append("spam_topic_anchor_requires_context")
    return {
        "score": score if reasons else None,
        "reasons": reasons,
        "kind": "review_heuristic",
        "penalty_evidence": False,
    }


def _merge(observations: list[dict], target_status: dict | None) -> list[dict]:
    pairs: dict[tuple, list] = {}
    for observation in observations:
        key = (observation["source_url"], observation["target_url"])
        bucket = pairs.setdefault(key, [])
        if observation not in bucket:
            bucket.append(observation)
    rows = []
    for (donor, target), entries in sorted(
        pairs.items(), key=lambda pair: (pair[0][0], pair[0][1] or "")
    ):
        first = [entry["first_seen"] for entry in entries if entry["first_seen"]]
        last = [entry["last_seen"] for entry in entries if entry["last_seen"]]
        rel_values = {tuple(entry["rel"]) for entry in entries if entry["rel"] is not None}
        rel = sorted({token for tokens in rel_values for token in tokens}) if rel_values else None
        latest: dict[str, dict] = {}
        for entry in sorted(entries, key=lambda entry: entry["observed_at"]):
            latest[entry["source"]] = entry
        states = {entry["state"] for entry in latest.values()} - {"unknown"}
        state = next(iter(states)) if len(states) == 1 else "unknown"
        categories = [token for token in ("nofollow", "sponsored", "ugc") if rel and token in rel]
        qualification_complete = any(entry["qualification_complete"] for entry in entries)
        follow_values = {
            entry["observed_follow"] for entry in entries if entry["observed_follow"] is not None
        }
        for entry in entries:
            if set(entry["rel"] or []) & {"nofollow", "sponsored", "ugc"}:
                follow_values.add(False)
            elif entry["qualification_complete"]:
                follow_values.add(True)
        rows.append(
            {
                "source_url": donor,
                "target_url": target,
                "referring_domain": registrable_domain(urlsplit(donor).hostname or ""),
                "anchors": sorted({entry["anchor"] for entry in entries}),
                "rel": rel,
                "rel_conflict": len(rel_values) > 1,
                "qualification_complete": qualification_complete,
                "unknown_rel_observations": sum(entry["rel"] is None for entry in entries),
                "link_types": categories or (["normal"] if qualification_complete else ["unknown"]),
                "follow": next(iter(follow_values))
                if len(follow_values) == 1 and len(rel_values) <= 1
                else None,
                "follow_conflict": len(follow_values) > 1,
                "state": state,
                "state_conflict": len(states) > 1,
                "first_seen": min(first) if first else None,
                "last_seen": max(last) if last else None,
                "lost_at": max(
                    (entry["lost_at"] for entry in entries if entry["lost_at"]), default=None
                ),
                "sources": sorted({entry["source"] for entry in entries}),
                "observations": entries,
                "spam": _spam(entries),
                "target_status": (target_status or {}).get(target),
            }
        )
    return rows


def _summary(rows: list[dict], domain_edges: list[dict]) -> dict:
    anchors = Counter(anchor for row in rows for anchor in row["anchors"])
    domains = Counter(row["referring_domain"] for row in rows)
    for edge in domain_edges:
        domains.setdefault(edge["source_domain"], 0)
    return {
        "backlinks": len(rows),
        "referring_domains": len(domains),
        "live": sum(row["state"] == "live" for row in rows),
        "lost": sum(row["state"] == "lost" for row in rows),
        "unknown_state": sum(row["state"] == "unknown" for row in rows),
        "link_types": dict(Counter(kind for row in rows for kind in row["link_types"])),
        "target_404": sum(row["target_status"] == 404 for row in rows),
        "target_redirect": sum(
            type(row["target_status"]) is int and 300 <= row["target_status"] < 400 for row in rows
        ),
        "anchors": [
            {"anchor": anchor, "backlinks": count} for anchor, count in anchors.most_common()
        ],
        "domains": [
            {"domain": domain, "backlinks": count} for domain, count in sorted(domains.items())
        ],
    }


def build_backlink_profile(
    domain: str,
    *,
    imports: list[dict] | None = None,
    evidence: list[dict] | None = None,
    db: str | None = None,
    project: str | None = None,
    observed_at: str | None = None,
    state: str | None = None,
    follow: str | None = None,
    target_prefix: str | None = None,
    min_spam_score: float | None = None,
    max_spam_score: float | None = None,
    first_seen_from: str | None = None,
    first_seen_to: str | None = None,
    one_per_domain: bool = False,
    target_status: dict | None = None,
    limit: int = 1000,
    offset: int = 0,
) -> dict:
    """Analyze only explicitly supplied imports/evidence, optionally saving a local snapshot.

    ``imports`` entries require source/path and optionally a normalized-field -> CSV-header
    ``columns`` map. Common Crawl accepts ``format=wat`` or ``format=domain_edges``; domain
    extracts require source_domain/target_domain headers. CSV population coverage defaults to
    partial; ``complete=true`` only means the supplied export scope, never the whole web.
    """
    host = normalize_domain(domain)
    if not host:
        raise ValueError("domain must be a valid hostname or HTTP(S) URL")
    if db and project:
        raise ValueError("pass db or project, not both")
    if (
        type(limit) is not int
        or not 1 <= limit <= MAX_ROWS
        or type(offset) is not int
        or offset < 0
    ):
        raise ValueError("limit must be 1..100000 and offset must be nonnegative")
    if state not in {None, "live", "lost", "unknown"} or follow not in {
        None,
        "normal",
        "nofollow",
        "sponsored",
        "ugc",
        "unknown",
    }:
        raise ValueError("unsupported link state or rel filter")
    for score in (min_spam_score, max_spam_score):
        if score is not None and (
            isinstance(score, bool) or not math.isfinite(score) or not 0 <= score <= 100
        ):
            raise ValueError("spam score filters must be in 0..100")
    if (
        min_spam_score is not None
        and max_spam_score is not None
        and min_spam_score > max_spam_score
    ):
        raise ValueError("min_spam_score must not exceed max_spam_score")
    start, end = _date(first_seen_from), _date(first_seen_to)
    if start and end and start > end:
        raise ValueError("first_seen_from must not exceed first_seen_to")
    stamp = observed_at or datetime.now(timezone.utc).isoformat(timespec="microseconds")
    parsed_stamp = datetime.fromisoformat(stamp.replace("Z", "+00:00"))
    if parsed_stamp.tzinfo is None:
        raise ValueError("observed_at must include a timezone")
    stamp = parsed_stamp.astimezone(timezone.utc).isoformat(timespec="microseconds")
    observations, coverage, domain_edges, aggregates = [], [], [], []
    items = list(evidence or [])
    for item in imports or []:
        source = item.get("source")
        path = Path(item["path"])
        rows = (
            _wat_rows(path)
            if source == "common_crawl" and item.get("format") == "wat"
            else _read_csv(path)
        )
        items.append(
            {
                **item,
                "rows": rows,
                "state": "complete" if item.get("complete") is True else "partial",
            }
        )
    for item in items:
        source = item.get("source")
        if source not in SOURCES:
            raise ValueError("unsupported backlink source")
        rows, info = _source_rows(item)
        info.update(
            source=source,
            scope=item.get("scope", "supplied export or provider sample"),
            input_rows=len(rows),
            rejected_rows=0,
            out_of_scope_rows=0,
            unknown_targets=0,
        )
        info["whole_web_complete"] = False
        for row in rows:
            fields = _mapped(row, item.get("columns"))
            if not fields.get("source_url") and fields.get("source_domain"):
                donor = normalize_domain(str(fields["source_domain"]))
                target = normalize_domain(
                    str(fields.get("target_domain") or (host if source != "common_crawl" else ""))
                )
                if donor and (target == host or target.endswith("." + host)):
                    edge = {
                        "source_domain": donor,
                        "target_domain": target,
                        "source": source,
                        "granularity": "domain",
                        "observed_at": stamp,
                    }
                    if edge not in domain_edges:
                        domain_edges.append(edge)
                else:
                    info["rejected_rows"] += 1
                continue
            if not fields.get("source_url") and fields.get("target_url"):
                try:
                    target = _url(fields["target_url"])
                except ValueError:
                    info["rejected_rows"] += 1
                    continue
                if normalize_domain(target or "") != host:
                    info["out_of_scope_rows"] += 1
                    continue
                aggregates.append(
                    {
                        "source": source,
                        "target_url": target,
                        "granularity": "target_aggregate",
                        "metrics": {
                            str(key): value
                            for key, value in row.items()
                            if str(key).lower()
                            in {"external links", "linking sites", "linked pages"}
                        },
                    }
                )
                continue
            try:
                observation = _observation(row, source, stamp, item.get("columns"))
            except (TypeError, ValueError):
                info["rejected_rows"] += 1
                continue
            target = observation["target_url"]
            target_host = normalize_domain(target or "")
            if target and target_host != host and not target_host.endswith("." + host):
                info["out_of_scope_rows"] += 1
                continue
            if not target:
                info["unknown_targets"] += 1
            observations.append(observation)
        if info["rejected_rows"] or info["unknown_targets"]:
            info.update(state="partial", complete=False)
        coverage.append(info)
    if len(observations) > MAX_ROWS:
        raise ValueError("merged backlink profile exceeds 100000 observations")
    normalized_status = {_url(key): value for key, value in (target_status or {}).items()}
    all_rows = _merge(observations, normalized_status)
    measured = any(
        item.get("rows_supplied")
        and (
            item["input_rows"] == 0
            or item["input_rows"] > item["rejected_rows"] + item["out_of_scope_rows"]
        )
        for item in coverage
    )
    history = None
    if (db or project) and measured:
        from seohead.data_sources.backlink_history import save_snapshot

        history = save_snapshot(
            host, all_rows, coverage, domain_edges, db=db, project=project, observed_at=stamp
        )
    filtered = []
    seen = set()
    for row in all_rows:
        score = row["spam"]["score"]
        if state and row["state"] != state:
            continue
        if follow and follow not in row["link_types"]:
            continue
        if target_prefix and not (row["target_url"] or "").startswith(target_prefix):
            continue
        if min_spam_score is not None and (score is None or score < min_spam_score):
            continue
        if max_spam_score is not None and (score is None or score > max_spam_score):
            continue
        if start and (not row["first_seen"] or row["first_seen"] < start):
            continue
        if end and (not row["first_seen"] or row["first_seen"] > end):
            continue
        if one_per_domain and row["referring_domain"] in seen:
            continue
        seen.add(row["referring_domain"])
        filtered.append(row)
    summary = _summary(filtered, domain_edges)
    return {
        "ok": measured,
        "format": "seohead.backlink-profile.v1",
        "domain": host,
        "observed_at": stamp,
        "state": "unavailable"
        if not measured
        else "partial"
        if not coverage or any(not item["complete"] for item in coverage)
        else "complete",
        "coverage": coverage,
        "reason": None if measured else "no measured backlink evidence in the requested scope",
        "whole_web_complete": False,
        "scope": "explicit source evidence; absent rows do not prove lost links",
        "summary": None
        if not measured
        else {key: value for key, value in summary.items() if key not in {"anchors", "domains"}},
        "anchors": summary["anchors"],
        "referring_domains": summary["domains"],
        "domain_edges": domain_edges,
        "aggregate_rows": aggregates,
        "unfiltered_backlinks": len(all_rows),
        "rows": filtered[offset : offset + limit],
        "history": history,
        "pagination": {
            "offset": offset,
            "limit": limit,
            "total": len(filtered),
            "has_more": offset + limit < len(filtered),
        },
    }


def export_disavow(
    *, domains: list[str] | None = None, urls: list[str] | None = None, output: str | None = None
) -> dict:
    """Generate an explicit reviewed selection, without uploading or auto-selecting toxicity."""
    normalized = sorted({normalize_domain(domain) for domain in domains or []})
    if "" in normalized:
        raise ValueError("disavow domains must be valid hostnames")
    normalized_urls = {_url(url) for url in urls or []}
    if None in normalized_urls:
        raise ValueError("disavow URLs must be nonempty")
    normalized_urls = sorted(normalized_urls)
    lines = ["# Explicit reviewed selection; no search-engine submission was made."]
    lines.extend("domain:" + domain for domain in normalized)
    lines.extend(normalized_urls)
    text = "\n".join(lines) + "\n"
    if output:
        target = Path(output)
        staged = stage_bytes(target.parent, ".disavow-", text.encode(), mode=0o600)
        try:
            os.link(staged, target)  # Exclusive publication: never overwrite an existing list.
        finally:
            Path(staged).unlink(missing_ok=True)
    return {
        "ok": True,
        "text": text,
        "domains": len(normalized),
        "urls": len(normalized_urls),
        "output": output,
        "submitted": False,
    }
