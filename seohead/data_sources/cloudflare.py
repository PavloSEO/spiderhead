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
read scopes (Zone Read and Account Analytics Read) and is never printed, logged, or put into an
error message.
Only GET requests and POSTs to the GraphQL endpoint (a read) are issued.
"""

from __future__ import annotations

import ipaddress
import json
import os
import stat
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from collections import Counter, defaultdict
from collections.abc import Callable
from datetime import date, datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from seohead.data_sources import credentials, spend
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
MAX_TOKEN_BYTES = 8192
MAX_RULES = 2000
RULE_PHASES = (
    "http_request_redirect",
    "http_request_dynamic_redirect",
    "http_request_firewall_custom",
)
AI_SETTINGS = ("search_for_agents", "redirects_for_ai_training")
MAX_CONFIG_ITEMS = 100
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
        if len(env.encode("utf-8")) > MAX_TOKEN_BYTES or any(ord(char) < 32 for char in env):
            raise CloudflareError("Cloudflare API token environment value is invalid")
        return env
    path = Path(
        os.environ.get("SEOHEAD_CLOUDFLARE_TOKEN_FILE") or "~/.config/cloudflare/api-token"
    ).expanduser()
    try:
        link_info = path.lstat()
        is_reparse_point = bool(
            getattr(link_info, "st_file_attributes", 0)
            & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0)
        )
        if (
            stat.S_ISLNK(link_info.st_mode)
            or is_reparse_point
            or not stat.S_ISREG(link_info.st_mode)
        ):
            raise CloudflareError("Cloudflare token file must be a regular file, not a link")
        flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
        descriptor = os.open(path, flags)
        info = os.fstat(descriptor)
        post_open = path.lstat()
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_nlink != 1
            or info.st_size > MAX_TOKEN_BYTES
            or not credentials.is_private_mode(info.st_mode)
            or stat.S_ISLNK(post_open.st_mode)
            or bool(
                getattr(post_open, "st_file_attributes", 0)
                & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0)
            )
            or (link_info.st_ino and info.st_ino != link_info.st_ino)
            or (link_info.st_dev and info.st_dev != link_info.st_dev)
            or (info.st_ino and info.st_ino != post_open.st_ino)
            or (info.st_dev and info.st_dev != post_open.st_dev)
        ):
            os.close(descriptor)
            raise CloudflareError("Cloudflare token file must be a private bounded regular file")
        with os.fdopen(descriptor, "rb") as stream:
            raw = stream.read(MAX_TOKEN_BYTES + 1)
        if len(raw) > MAX_TOKEN_BYTES:
            raise CloudflareError("Cloudflare token file exceeds the byte limit")
        value = raw.decode("utf-8").strip()
    except FileNotFoundError:
        value = ""
    except CloudflareError:
        raise
    except (OSError, UnicodeError):
        raise CloudflareError("Cloudflare token file is unreadable or invalid") from None
    if len(value.encode("utf-8")) > MAX_TOKEN_BYTES or any(ord(char) < 32 for char in value):
        raise CloudflareError("Cloudflare token file is invalid")
    if not value:
        raise CloudflareError(
            f"Cloudflare token not found: set $CLOUDFLARE_API_TOKEN or store it in {path} "
            "(or name the file in $SEOHEAD_CLOUDFLARE_TOKEN_FILE)"
        )
    return value


def verify_token(*, token: str | None = None, transport: Transport | None = None) -> dict[str, Any]:
    """Verify Cloudflare token lifecycle only; this does not establish zone scopes."""
    try:
        secret = token or read_token()
    except CloudflareError:
        return {
            "ok": False,
            "token_status": None,
            "error": "Cloudflare API token is not configured",
        }
    try:
        payload = (transport or _default_transport)(
            "GET", f"{API}/user/tokens/verify", None, secret
        )
    except (CloudflareError, urllib.error.URLError, TimeoutError, OSError):
        return {
            "ok": False,
            "token_status": None,
            "error": "Cloudflare token verification request failed",
        }
    if not payload.get("success") or not isinstance(payload.get("result"), dict):
        return {
            "ok": False,
            "token_status": None,
            "error": "Cloudflare token verification returned an invalid response",
        }
    result = payload["result"]
    state = result.get("status")
    if state not in {"active", "disabled", "expired"}:
        return {
            "ok": False,
            "token_status": None,
            "error": "Cloudflare token verification returned an unknown status",
        }
    return {"ok": state == "active", "token_status": state}


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


def resolve_project_zone(project: str) -> str:
    """Resolve one manually selected Cloudflare zone ID without provider access."""
    from seohead.projects.bindings import list_bindings

    result = list_bindings(project)
    matches = [
        item
        for item in result["bindings"]
        if item["service"] == "cloudflare" and item["status"] == "selected"
    ]
    if len(matches) != 1:
        raise CloudflareError("project must have exactly one selected Cloudflare zone binding")
    return matches[0]["resource"]


def resolve_zone_id(zone_id: str, token: str, transport: Transport) -> dict[str, Any]:
    """Resolve a bound zone ID through Cloudflare's read-only zone endpoint."""
    payload = transport("GET", f"{API}/zones/{urllib.parse.quote(zone_id, safe='')}", None, token)
    if not payload.get("success"):
        raise CloudflareError(f"zone lookup failed: {_errors(payload)}")
    row = payload.get("result")
    if not isinstance(row, dict) or row.get("id") != zone_id:
        raise CloudflareError("bound Cloudflare zone is not visible to this token")
    return {
        "id": row["id"],
        "name": row["name"],
        "account": (row.get("account") or {}).get("id"),
        "plan": (row.get("plan") or {}).get("name"),
        "status": row.get("status"),
    }


def _zone_setting(zone_id: str, setting: str, token: str, transport: Transport) -> dict[str, Any]:
    try:
        payload = transport(
            "GET",
            f"{API}/zones/{urllib.parse.quote(zone_id, safe='')}/settings/{setting}",
            None,
            token,
        )
    except (CloudflareError, urllib.error.URLError, TimeoutError, OSError):
        return {"state": "unavailable", "reason": "zone settings read failed"}
    result = payload.get("result") if isinstance(payload, dict) else None
    if (
        not isinstance(payload, dict)
        or not payload.get("success")
        or not isinstance(result, dict)
        or result.get("id") != setting
    ):
        return {"state": "unavailable", "reason": "zone setting is unavailable or not readable"}
    setting_value = result.get("value")
    if isinstance(setting_value, str):
        setting_value = setting_value[:256]
    elif setting_value is not None and type(setting_value) not in {bool, int, float}:
        setting_value = None
    return {
        "state": "measured",
        "value": setting_value,
        "editable": result.get("editable"),
        "modified_on": result.get("modified_on"),
    }


def _ruleset(phase: str, zone_id: str, token: str, transport: Transport) -> dict[str, Any]:
    url = (
        f"{API}/zones/{urllib.parse.quote(zone_id, safe='')}/rulesets/phases/"
        f"{urllib.parse.quote(phase, safe='')}/entrypoint"
    )
    try:
        payload = transport("GET", url, None, token)
    except (CloudflareError, urllib.error.URLError, TimeoutError, OSError):
        return {"state": "unavailable", "reason": "ruleset read failed", "rules": []}
    value = payload.get("result") if isinstance(payload, dict) else None
    rules = value.get("rules") if isinstance(value, dict) else None
    if (
        not isinstance(payload, dict)
        or not payload.get("success")
        or not isinstance(rules, list)
        or not all(isinstance(rule, dict) for rule in rules)
    ):
        return {
            "state": "unavailable",
            "reason": "ruleset is unavailable or not readable",
            "rules": [],
        }
    return {
        "state": "partial" if len(rules) > MAX_RULES else "complete",
        "rules": [
            {
                key: (
                    rule.get(key)[:1000]
                    if key == "expression" and isinstance(rule.get(key), str)
                    else rule.get(key)[:200]
                    if key == "description" and isinstance(rule.get(key), str)
                    else rule.get(key)
                )
                for key in ("id", "description", "action", "enabled", "expression", "ref")
                if key in rule
            }
            for rule in rules[:MAX_RULES]
        ],
        "truncated": len(rules) > MAX_RULES,
        "modified_on": value.get("last_updated"),
    }


def _logpush_jobs(zone_id: str, token: str, transport: Transport) -> dict[str, Any]:
    url = f"{API}/zones/{urllib.parse.quote(zone_id, safe='')}/logpush/jobs?per_page={MAX_CONFIG_ITEMS}&page=1"
    try:
        payload = transport("GET", url, None, token)
    except (CloudflareError, urllib.error.URLError, TimeoutError, OSError):
        return {"state": "unavailable", "reason": "Logpush jobs read failed", "jobs": []}
    if (
        not isinstance(payload, dict)
        or not payload.get("success")
        or not isinstance(payload.get("result"), list)
    ):
        return {
            "state": "unavailable",
            "reason": "Logpush jobs are unavailable or not readable",
            "jobs": [],
        }
    rows = payload["result"]
    if not all(isinstance(row, dict) for row in rows):
        return {"state": "unavailable", "reason": "Logpush jobs response is malformed", "jobs": []}
    info = payload.get("result_info") or {}
    total = info.get("total_count") if isinstance(info, dict) else None
    return {
        "state": "partial"
        if len(rows) >= MAX_CONFIG_ITEMS or (isinstance(total, int) and total > len(rows))
        else "complete",
        "total": total if isinstance(total, int) else None,
        "jobs": [
            {
                key: row.get(key)
                for key in (
                    "id",
                    "dataset",
                    "enabled",
                    "kind",
                    "frequency",
                    "max_upload_bytes",
                    "max_upload_records",
                )
                if key in row
            }
            for row in rows[:MAX_CONFIG_ITEMS]
        ],
        "destinations_omitted": True,
        "reason": "This read lists configured jobs only; destination data must be read from the customer-owned target.",
    }


def configuration(
    zone_id: str, *, token: str | None = None, transport: Transport | None = None
) -> dict[str, Any]:
    """Read a bounded subset of zone AI settings, redirects and custom WAF rules."""
    secret = token or read_token()
    send = transport or _default_transport
    settings = {name: _zone_setting(zone_id, name, secret, send) for name in AI_SETTINGS}
    rulesets = {phase: _ruleset(phase, zone_id, secret, send) for phase in RULE_PHASES}
    logpush = _logpush_jobs(zone_id, secret, send)
    states = (
        [item["state"] for item in settings.values()]
        + [item["state"] for item in rulesets.values()]
        + [logpush["state"]]
    )
    return {
        "state": "complete"
        if all(state == "measured" or state == "complete" for state in states)
        else "partial",
        "source": "Cloudflare zone settings and Rulesets API; read-only",
        "zone_settings": settings,
        "rulesets": rulesets,
        "logpush": logpush,
        "ai_crawler_policy_note": (
            "AI Crawl Control allow/block actions are represented by Cloudflare custom WAF rules. "
            "These are configured rules, not proof that a rule matched a URL or request."
        ),
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
    zone: str | None = None,
    *,
    project: str | None = None,
    include_configuration: bool = False,
    since: str | None = None,
    until: str | None = None,
    transport: Transport | None = None,
    token: str | None = None,
    today: date | None = None,
) -> dict[str, Any]:
    """Summarise bot and human traffic for a zone over an inclusive UTC date range.

    Exactly one of ``zone`` (a zone name) or ``project`` (one selected Cloudflare zone binding)
    is required. Defaults to the last 7 days ending today. One GraphQL query is made per day; a day
    that fills the dataset's row cap is split into halves (down to one hour) so rows are not
    silently lost, and only a one-hour window that still fills the cap is reported in
    ``truncated_days``.
    """
    if bool(zone) == bool(project):
        raise ValueError("pass exactly one of zone or project")
    if type(include_configuration) is not bool:
        raise ValueError("include_configuration must be a boolean")
    requested_zone = zone
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
        if project:
            requested_zone = resolve_project_zone(project)
            info = resolve_zone_id(requested_zone, secret, send)
        else:
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
        return {"ok": False, "source": SOURCE, "zone": requested_zone, "error": str(exc)}

    # Aggregated, free provider: still counted so the ledger shows how often the API is used.
    spend.record(
        "cloudflare",
        "graphql-traffic",
        cost=0.0,
        unit="requests",
        items=queries
        + 1
        + (len(AI_SETTINGS) + len(RULE_PHASES) + 1 if include_configuration else 0),
    )

    result: dict[str, Any] = {
        "ok": True,
        "source": SOURCE,
        "note": NOTE,
        "zone_id": info["id"],
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
    result["state"] = "partial" if sampled or truncated_days or url_facts_truncated else "complete"
    if include_configuration:
        result["configuration"] = configuration(info["id"], token=secret, transport=send)
        result["configuration_requests"] = len(AI_SETTINGS) + len(RULE_PHASES) + 1
    if project:
        try:
            from seohead.data_sources.cloudflare_history import save_snapshot

            result["history"] = save_snapshot(result, project=project)
        except (OSError, ValueError):
            result["history"] = {
                "state": "unavailable",
                "reason": "the aggregate result could not be added to project history",
            }
    return result


def _raw_transport(url: str, token: str) -> bytes:
    request = urllib.request.Request(
        url,
        method="GET",
        headers={"Authorization": f"Bearer {token}", "Accept": "application/x-ndjson"},
    )
    with open_no_redirect(request, timeout=TIMEOUT) as response:
        raw = response.read(MAX_RESPONSE_BYTES + 1)
    if len(raw) > MAX_RESPONSE_BYTES:
        raise CloudflareError("Cloudflare Logpull response exceeded the 16 MiB limit")
    return raw


def _apache_quote(value: Any) -> str:
    from urllib.parse import quote

    return quote(str(value or "-"), safe="/?:=&%#+,;@!$'()*-._~")


def _logpull_datetime(value: str, name: str) -> datetime:
    if not isinstance(value, str):
        raise ValueError(f"{name} must be an RFC3339 UTC timestamp")
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        raise ValueError(f"{name} must be an RFC3339 UTC timestamp") from None
    if parsed.tzinfo is None or parsed.utcoffset() != timedelta(0):
        raise ValueError(f"{name} must be an RFC3339 UTC timestamp")
    return parsed.astimezone(timezone.utc)


def logpull(
    *,
    project: str | None = None,
    zone: str | None = None,
    start: str,
    end: str,
    count: int = 5000,
    token: str | None = None,
    transport: Transport | None = None,
    raw_transport: Callable[[str, str], bytes] | None = None,
    now: datetime | None = None,
) -> dict[str, Any]:
    """Import one explicit, bounded Enterprise Logpull interval through the shared log store."""
    if not project or zone is not None:
        raise ValueError("pass a project with one selected Cloudflare zone binding")
    if type(count) is not int or not 1 <= count <= 10_000:
        raise ValueError("count must be between 1 and 10000")
    start_at = _logpull_datetime(start, "start")
    end_at = _logpull_datetime(end, "end")
    current = now or datetime.now(timezone.utc)
    if start_at >= end_at or end_at - start_at > timedelta(hours=1):
        raise ValueError("Logpull interval must be ordered and at most one hour")
    if start_at < current - timedelta(days=7):
        raise ValueError("Logpull start must be within the last seven days")
    if end_at > current - timedelta(minutes=5):
        raise ValueError("Logpull end must be at least five minutes old")

    send = transport or _default_transport
    try:
        secret = token or read_token()
        zone_id = resolve_project_zone(project)
        info = resolve_zone_id(zone_id, secret, send)
        if not str(info.get("plan") or "").casefold().startswith("enterprise"):
            return {
                "ok": False,
                "state": "unavailable",
                "provider": "cloudflare",
                "zone": info["name"],
                "reason": "Cloudflare Logpull is available on Enterprise plans only",
            }
        field_payload = send(
            "GET",
            f"{API}/zones/{urllib.parse.quote(info['id'], safe='')}/logs/received/fields",
            None,
            secret,
        )
        fields_result = field_payload.get("result") if isinstance(field_payload, dict) else None
        if isinstance(fields_result, dict):
            available = set(fields_result)
        elif isinstance(fields_result, list):
            available = {
                item.get("key") if isinstance(item, dict) else item
                for item in fields_result
                if (isinstance(item, dict) and item.get("key")) or isinstance(item, str)
            }
        else:
            available = set()
        required = ("ClientIP", "ClientRequestURI", "EdgeStartTimestamp", "EdgeResponseStatus")
        if (
            not isinstance(field_payload, dict)
            or not field_payload.get("success")
            or not set(required) <= available
        ):
            return {
                "ok": False,
                "state": "unavailable",
                "provider": "cloudflare",
                "zone": info["name"],
                "reason": "Cloudflare Logpull fields are unavailable to this token or plan",
            }
        optional = (
            "ClientRequestMethod",
            "ClientRequestUserAgent",
            "EdgeResponseBytes",
        )
        selected = [*required, *(name for name in optional if name in available)]
        query = urllib.parse.urlencode(
            {
                "start": start_at.strftime("%Y-%m-%dT%H:%M:%SZ"),
                "end": end_at.strftime("%Y-%m-%dT%H:%M:%SZ"),
                "count": count,
                "fields": ",".join(selected),
                "timestamps": "rfc3339",
            }
        )
        endpoint = f"{API}/zones/{urllib.parse.quote(info['id'], safe='')}/logs/received?{query}"
        raw = (raw_transport or _raw_transport)(endpoint, secret)
    except CloudflareError as exc:
        return {
            "ok": False,
            "state": "failed",
            "provider": "cloudflare",
            "error": str(exc),
        }
    except (urllib.error.URLError, TimeoutError, OSError):
        return {
            "ok": False,
            "state": "failed",
            "provider": "cloudflare",
            "error": "Cloudflare Logpull request failed",
        }

    if isinstance(raw, str):
        encoded = raw.encode("utf-8")
        raw_text = raw
    elif isinstance(raw, bytes):
        encoded = raw
        try:
            raw_text = raw.decode("utf-8")
        except UnicodeError:
            return {
                "ok": False,
                "state": "failed",
                "provider": "cloudflare",
                "error": "Cloudflare Logpull returned invalid UTF-8",
            }
    else:
        return {
            "ok": False,
            "state": "failed",
            "provider": "cloudflare",
            "error": "Cloudflare Logpull returned an invalid response",
        }
    if len(encoded) > MAX_RESPONSE_BYTES:
        return {
            "ok": False,
            "state": "failed",
            "provider": "cloudflare",
            "error": "Cloudflare Logpull response exceeded the 16 MiB limit",
        }

    lines = [line for line in raw_text.splitlines() if line]
    if len(lines) > count:
        return {
            "ok": False,
            "state": "failed",
            "provider": "cloudflare",
            "error": "Cloudflare Logpull exceeded the requested row limit",
        }
    converted = []
    try:
        for line in lines:
            row = json.loads(line)
            if not isinstance(row, dict):
                raise ValueError("invalid log row")
            ip = str(row.get("ClientIP") or "-")
            try:
                ipaddress.ip_address(ip)
            except ValueError:
                raise ValueError("invalid client IP") from None
            uri = _apache_quote(row.get("ClientRequestURI") or "/")
            method = _apache_quote(row.get("ClientRequestMethod") or "GET")
            agent = _apache_quote(row.get("ClientRequestUserAgent") or "-")
            status = int(row["EdgeResponseStatus"])
            if not 100 <= status <= 599:
                raise ValueError("invalid HTTP status")
            stamp = datetime.fromisoformat(str(row["EdgeStartTimestamp"]).replace("Z", "+00:00"))
            if stamp.tzinfo is None:
                raise ValueError("timestamp lacks a timezone")
            stamp = stamp.astimezone(timezone.utc)
            size = row.get("EdgeResponseBytes")
            size = str(int(size)) if size is not None else "-"
            converted.append(
                f"{ip} - - [{stamp.strftime('%d/%b/%Y:%H:%M:%S +0000')}] "
                f'"{method} {uri} HTTP/1.1" {status} {size} "-" "{agent}"'
            )
    except (KeyError, TypeError, ValueError, OverflowError):
        return {
            "ok": False,
            "state": "failed",
            "provider": "cloudflare",
            "error": "Cloudflare Logpull returned malformed HTTP request rows",
        }
    if not converted:
        return {
            "ok": True,
            "state": "empty",
            "provider": "cloudflare",
            "zone": info["name"],
            "period": {"from": start_at.isoformat(), "to": end_at.isoformat()},
            "returned": 0,
            "complete": True,
            "reason": "no Cloudflare edge log rows in the requested interval",
        }

    import_result = None
    if project:
        from seohead.projects.workspace import open_project

        root = Path(open_project(project)["path"])
        logs_dir = root / "logs"
        if logs_dir.is_symlink() or (logs_dir.exists() and not logs_dir.is_dir()):
            raise ValueError("project logs directory is unsafe")
        logs_dir.mkdir(mode=0o700, exist_ok=True)
        temporary_path = None
        try:
            with tempfile.NamedTemporaryFile(
                mode="w",
                encoding="utf-8",
                newline="\n",
                prefix=".cloudflare-logpull-",
                suffix=".log",
                dir=logs_dir,
                delete=False,
            ) as handle:
                temporary_path = Path(handle.name)
                for line in converted:
                    handle.write(line + "\n")
                if len(converted) == count:
                    handle.write(
                        "\n"
                    )  # Sentinel makes the shared importer mark the capped prefix partial.
            from seohead.checks.logstore import import_logs

            import_result = import_logs(str(temporary_path), project, max_lines=count)
        finally:
            if temporary_path is not None:
                temporary_path.unlink(missing_ok=True)
    else:
        return {
            "ok": False,
            "state": "invalid",
            "provider": "cloudflare",
            "error": "Cloudflare Logpull requires project so IP records stay in the local log store",
        }
    if not import_result or not import_result.get("ok"):
        return {
            "ok": False,
            "state": "failed",
            "provider": "cloudflare",
            "zone": info["name"],
            "error": (import_result or {}).get("error")
            or "Cloudflare log rows could not be retained",
        }
    truncated = len(converted) == count
    return {
        "ok": True,
        "state": "partial" if truncated or not import_result.get("complete") else "complete",
        "provider": "cloudflare",
        "zone": info["name"],
        "period": {"from": start_at.isoformat(), "to": end_at.isoformat()},
        "returned": len(converted),
        "complete": not truncated and bool(import_result.get("complete")),
        "truncated": truncated,
        "log_import": import_result,
        "dns_verification": "not requested",
    }


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
