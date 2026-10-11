"""Cloudflare edge analytics as an aggregated stand-in for server access logs (issues #1026, #1009).

Many sites sit behind Cloudflare, where origin logs are incomplete or absent. The GraphQL
Analytics API exposes ``httpRequestsAdaptiveGroups``: request counts grouped by path, status,
user agent, verified-bot category and cache status. That is enough to answer "which bots hit which
URLs with which codes, per day", but it is **not a raw log**:

* rows are grouped counts, not individual requests: no IPs, no timestamps below one day, so the
  reverse-DNS bot verification of ``log-analyze`` is not possible;
* the dataset is adaptive (sampled under load); on low-traffic zones ``sampleInterval`` is 1;
* the account-level dataset is read-only and, on the Free plan, reaches back 32 days.

The result mirrors the ``log-analyze`` summary (families, bots, status, sections, top paths, daily)
and is labelled ``source: cloudflare-aggregated`` so nobody mistakes it for raw logs.

The token is read from ``$CLOUDFLARE_API_TOKEN`` or the file named by
``$SEOHEAD_CLOUDFLARE_TOKEN_FILE`` (default ``~/.config/cloudflare/api-token``). It needs only
read scopes (Account Analytics Read) and is never printed, logged, or put into an error message.
Only GET requests and POSTs to the GraphQL endpoint (a read) are issued.
"""

from __future__ import annotations

import json
import os
import time
import urllib.error
import urllib.parse
import urllib.request
from collections import Counter, defaultdict
from collections.abc import Callable
from datetime import date, datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from seohead.data_sources import spend
from seohead.data_sources.http import open_no_redirect

API = "https://api.cloudflare.com/client/v4"
GRAPHQL = f"{API}/graphql"
TIMEOUT = 60
MAX_RESPONSE_BYTES = 16 * 1024 * 1024
PAGE_SIZE = 10000  # the dataset's maxPageSize on the Free plan
MAX_DAYS = 32  # the dataset's maxDuration on the Free plan (2764800 s)
RETRIES = 3
MAX_TRACKED_PATHS = 5000
MAX_URL_FACTS = 50000
SOURCE = "cloudflare-aggregated"
NOTE = (
    "Aggregated Cloudflare edge analytics, not raw server logs: grouped counts per day, "
    "no client IPs, so bot authenticity (reverse DNS) cannot be verified; adaptive sampling "
    "applies on busy zones."
)

QUERY = """
query($account: String!, $zone: String!, $from: Time!, $to: Time!, $limit: Int!) {
  viewer { accounts(filter: {accountTag: $account}) {
    httpRequestsAdaptiveGroups(limit: $limit, orderBy: [count_DESC],
        filter: {zoneTag: $zone, datetime_geq: $from, datetime_lt: $to}) {
      count
      avg { sampleInterval }
      dimensions {
        date userAgent verifiedBotCategory clientRequestHTTPHost clientRequestPath
        edgeResponseStatus originResponseStatus cacheStatus
      }
    }
  } }
}
"""

# method, url, JSON body or None, token -> parsed JSON object
Transport = Callable[[str, str, "dict[str, Any] | None", str], "dict[str, Any]"]


class CloudflareError(RuntimeError):
    """A Cloudflare call failed; the message never contains the token."""


def read_token() -> str:
    env = os.environ.get("CLOUDFLARE_API_TOKEN", "").strip()
    if env:
        return env
    path = Path(
        os.environ.get("SEOHEAD_CLOUDFLARE_TOKEN_FILE") or "~/.config/cloudflare/api-token"
    ).expanduser()
    try:
        value = path.read_text(encoding="utf-8").strip()
    except OSError:
        value = ""
    if not value:
        raise CloudflareError(
            f"Cloudflare token not found: set $CLOUDFLARE_API_TOKEN or store it in {path} "
            "(or name the file in $SEOHEAD_CLOUDFLARE_TOKEN_FILE)"
        )
    return value


def _default_transport(method: str, url: str, body: dict | None, token: str) -> dict[str, Any]:
    data = json.dumps(body).encode("utf-8") if body is not None else None
    for attempt in range(RETRIES + 1):
        request = urllib.request.Request(
            url,
            data=data,
            method=method,
            headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
        )
        try:
            with open_no_redirect(request, timeout=TIMEOUT) as response:
                raw = response.read(MAX_RESPONSE_BYTES + 1)
        except urllib.error.HTTPError as exc:
            retryable = exc.code == 429 or exc.code >= 500
            if retryable and attempt < RETRIES:
                try:
                    wait = float(exc.headers.get("Retry-After") or 0)
                except ValueError:
                    wait = 0
                time.sleep(min(max(wait, 2**attempt), 30))
                continue
            raise CloudflareError(f"Cloudflare API returned HTTP {exc.code}") from None
        except (urllib.error.URLError, TimeoutError) as exc:
            if attempt < RETRIES:
                time.sleep(2**attempt)
                continue
            raise CloudflareError(f"Cloudflare API request failed: {exc}") from None
        if len(raw) > MAX_RESPONSE_BYTES:
            raise CloudflareError("Cloudflare response exceeded the size budget")
        try:
            return json.loads(raw)
        except ValueError:
            raise CloudflareError("Cloudflare returned a non-JSON response") from None
    raise CloudflareError("Cloudflare API retries exhausted")


def _errors(payload: dict[str, Any]) -> str:
    items = payload.get("errors") or []
    return "; ".join(str(e.get("message", e))[:200] for e in items if e)


def resolve_zone(zone: str, token: str, transport: Transport) -> dict[str, Any]:
    """Look a zone up by name; returns id, account id, plan and status."""
    query = urllib.parse.urlencode({"name": zone})
    payload = transport("GET", f"{API}/zones?{query}", None, token)
    if not payload.get("success"):
        raise CloudflareError(f"zone lookup failed: {_errors(payload)}")
    rows = payload.get("result") or []
    if not rows:
        raise CloudflareError(f"zone {zone!r} is not visible to this token")
    row = rows[0]
    return {
        "id": row["id"],
        "name": row["name"],
        "account": (row.get("account") or {}).get("id"),
        "plan": (row.get("plan") or {}).get("name"),
        "status": row.get("status"),
    }


def _parse_day(value: str, name: str) -> date:
    try:
        return date.fromisoformat(value)
    except ValueError:
        raise ValueError(f"{name} must be YYYY-MM-DD, got {value!r}") from None


def _days(since: date, until: date) -> list[date]:
    return [since + timedelta(days=i) for i in range((until - since).days + 1)]


def _classify(ua: str, category: str) -> tuple[str, str]:
    from seohead.checks.logs import detect_bot

    bot = detect_bot(ua)
    if bot:
        return bot["family"], bot["name"]
    if category:
        return "other", f"Verified bot: {category}"
    return "human", "human"


def traffic(
    zone: str,
    *,
    since: str | None = None,
    until: str | None = None,
    transport: Transport | None = None,
    token: str | None = None,
    today: date | None = None,
) -> dict[str, Any]:
    """Summarise bot and human traffic for a zone over an inclusive UTC date range.

    Defaults to the last 7 days ending today. One GraphQL query is made per day; a day that fills
    the dataset's row cap is split into halves (down to one hour) so rows are not silently lost,
    and only a one-hour window that still fills the cap is reported in ``truncated_days``.
    """
    if not zone:
        raise ValueError("zone required (zone name, e.g. example.com)")
    today = today or datetime.now(timezone.utc).date()
    end = _parse_day(until, "until") if until else today
    start = _parse_day(since, "since") if since else end - timedelta(days=6)
    if start > end:
        raise ValueError("since must not be later than until")
    if (today - start).days >= MAX_DAYS:
        raise ValueError(
            f"since is older than the {MAX_DAYS}-day history this dataset keeps on the Free plan"
        )
    if (end - start).days + 1 > MAX_DAYS:
        raise ValueError(f"the range may span at most {MAX_DAYS} days")

    send = transport or _default_transport
    try:
        secret = token or read_token()
        info = resolve_zone(zone, secret, send)
        if not info["account"]:
            raise CloudflareError("the zone has no account id")

        families: dict[str, Counter] = defaultdict(Counter)
        status: dict[str, Counter] = defaultdict(Counter)
        sections: dict[str, Counter] = defaultdict(Counter)
        paths: dict[str, Counter] = defaultdict(Counter)
        cache: dict[str, Counter] = defaultdict(Counter)
        edge_vs_origin: Counter = Counter()
        bot_url_status: dict[str, Counter] = defaultdict(Counter)
        categories: dict[str, Counter] = defaultdict(Counter)
        bots_daily: dict[str, Counter] = defaultdict(Counter)
        hosts: Counter = Counter()
        daily: Counter = Counter()
        url_facts: dict[tuple, int] = {}
        url_facts_truncated = False
        url_facts_omitted = 0
        truncated_days: list[str] = []
        sampled = False
        queries = 0

        from seohead.checks.logs import _section

        def fetch(frm: datetime, to: datetime, day: date) -> list[dict[str, Any]]:
            """One window; a window that fills the row cap is halved (down to one hour)."""
            nonlocal queries
            variables = {
                "account": info["account"],
                "zone": info["id"],
                "from": frm.strftime("%Y-%m-%dT%H:%M:%SZ"),
                "to": to.strftime("%Y-%m-%dT%H:%M:%SZ"),
                "limit": PAGE_SIZE,
            }
            payload = send("POST", GRAPHQL, {"query": QUERY, "variables": variables}, secret)
            queries += 1
            if payload.get("errors"):
                raise CloudflareError(f"GraphQL error: {_errors(payload)}")
            accounts = ((payload.get("data") or {}).get("viewer") or {}).get("accounts") or []
            found = (accounts[0] if accounts else {}).get("httpRequestsAdaptiveGroups") or []
            if len(found) < PAGE_SIZE:
                return found
            if to - frm <= timedelta(hours=1):
                if day.isoformat() not in truncated_days:
                    truncated_days.append(day.isoformat())
                return found
            mid = frm + (to - frm) / 2
            return fetch(frm, mid, day) + fetch(mid, to, day)

        for day in _days(start, end):
            midnight = datetime(day.year, day.month, day.day, tzinfo=timezone.utc)
            rows = fetch(midnight, midnight + timedelta(days=1), day)
            for row in rows:
                dims = row.get("dimensions") or {}
                count = int(row.get("count") or 0)
                if (row.get("avg") or {}).get("sampleInterval", 1) not in (1, None):
                    sampled = True
                ua = dims.get("userAgent") or ""
                category = dims.get("verifiedBotCategory") or ""
                family, name = _classify(ua, category)
                path = dims.get("clientRequestPath") or "/"
                code = int(dims.get("edgeResponseStatus") or 0)
                origin = int(dims.get("originResponseStatus") or 0)
                day_key = dims.get("date") or day.isoformat()

                # Preserve the measured edge/origin/cache dimensions at URL grain. This is
                # still grouped analytics, not a request log: no IP or sub-day timestamp exists.
                fact_key = (
                    dims.get("clientRequestHTTPHost") or "unknown",
                    path,
                    day_key,
                    family,
                    name,
                    category or None,
                    code,
                    origin or None,
                    dims.get("cacheStatus") or "unknown",
                )
                if fact_key in url_facts:
                    url_facts[fact_key] += count
                elif len(url_facts) < MAX_URL_FACTS:
                    url_facts[fact_key] = count
                else:
                    url_facts_truncated = True
                    url_facts_omitted += 1

                families[family][name] += count
                status[family][code] += count
                sections[family][_section(path)] += count
                tracked = paths[family]
                if path in tracked or len(tracked) < MAX_TRACKED_PATHS:
                    tracked[path] += count
                cache[family][dims.get("cacheStatus") or "unknown"] += count
                edge_vs_origin[f"{code}/{origin or 'none'}"] += count
                daily[day_key] += count
                hosts[dims.get("clientRequestHTTPHost") or "unknown"] += count
                if family != "human":
                    bot_url_status[name][f"{path}\t{code}"] += count
                    bots_daily[name][day_key] += count
                    if category:
                        categories[name][category] += count
    except CloudflareError as exc:
        return {"ok": False, "source": SOURCE, "zone": zone, "error": str(exc)}

    # Aggregated, free provider: still counted so the ledger shows how often the API is used.
    spend.record("cloudflare", "graphql-traffic", cost=0.0, unit="requests", items=queries)

    result: dict[str, Any] = {
        "ok": True,
        "source": SOURCE,
        "note": NOTE,
        "zone": info["name"],
        "plan": info["plan"],
        "period": {"from": start.isoformat(), "to": end.isoformat()},
        "queries": queries,
        "sampled": sampled,
        "truncated_days": truncated_days,
        "requests": sum(daily.values()),
        "by_family": {f: dict(c.most_common(20)) for f, c in families.items()},
        "bots": [
            {
                "name": n,
                "hits": sum(days.values()),
                "verified_bot_categories": dict(categories.get(n, {})),
            }
            for n, days in sorted(bots_daily.items(), key=lambda kv: -sum(kv[1].values()))[:40]
        ],
        "status_by_family": {f: dict(sorted(c.items())) for f, c in status.items()},
        "sections_by_family": {f: dict(c.most_common(20)) for f, c in sections.items()},
        "top_paths_by_family": {f: dict(c.most_common(15)) for f, c in paths.items()},
        "cache_by_family": {f: dict(c.most_common()) for f, c in cache.items()},
        "edge_origin_status": dict(edge_vs_origin.most_common(20)),
        "hosts": dict(hosts.most_common(20)),
        "daily": dict(sorted(daily.items())),
        "url_facts": [
            {
                "host": key[0],
                "path": key[1],
                "date": key[2],
                "bot_family": key[3],
                "bot_name": key[4],
                "verified_bot_category": key[5],
                "edge_status": key[6],
                "origin_status": key[7],
                "cache_status": key[8],
                "edge_redirect": 300 <= key[6] < 400,
                "requests": count,
            }
            for key, count in sorted(url_facts.items())
        ],
        "url_facts_truncated": url_facts_truncated,
        "url_facts_omitted": url_facts_omitted,
        "bots_daily": {n: dict(sorted(c.items())) for n, c in bots_daily.items()},
        "bot_url_status": {
            n: [
                {"path": k.split("\t")[0], "status": int(k.split("\t")[1]), "hits": v}
                for k, v in c.most_common(25)
            ]
            for n, c in bot_url_status.items()
        },
        "verification": {
            "checked": False,
            "reason": "Aggregated edge data carries no client IPs; reverse-DNS verification "
            "is impossible. verified_bot_categories is Cloudflare's own verdict where present.",
        },
    }
    result["findings"] = _findings(result)
    return result


def _findings(r: dict[str, Any]) -> list[str]:
    out = [r["note"]]
    if r["truncated_days"]:
        out.append(
            f"Row cap reached on {', '.join(r['truncated_days'])}: low-volume rows were cut off"
        )
    fams = r["by_family"]
    if r["requests"] == 0:
        out.append("No requests in the period: the zone may not be proxied or has no traffic")
        return out
    for wanted in ("googlebot", "yandexbot", "bingbot"):
        if wanted not in fams:
            out.append(f"{wanted} does not appear in the period")
    if "ai" in fams:
        out.append(f"AI crawlers made {sum(fams['ai'].values())} requests")
    for family, codes in r["status_by_family"].items():
        if family == "human":
            continue
        errors = sum(c for s, c in codes.items() if s >= 400)
        total = sum(codes.values()) or 1
        if errors / total > 0.1:
            out.append(
                f"{family}: {errors} of {total} requests returned errors ({errors / total:.0%})"
            )
    return out
