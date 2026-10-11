"""DataForSEO client for Google volume, keyword ideas, difficulty, and SERPs.

Yandex Wordstat and Arsenkin cover Yandex and the Russian-language search market. Projects
outside that market, including English-language, Indian, and Gulf-region sites, primarily depend
on Google data. This client adds that demand layer to the technical evidence collected elsewhere.

**Provider boundaries are strict.** DataForSEO serves Google and international markets. Yandex
and Russian-language markets use :mod:`yandex_cloud` and :mod:`arsenkin`. Do not mix these paths:
DataForSEO does not support locations in Russia or Belarus across its services. The geographic
guard therefore prevents Russian or Belarusian requests
from reaching the network and returns suitable alternatives instead. Without the guard, a paid
request can return an empty result, charging the account for no usable data.

**Sandbox is the default.** Responses use the provider's schema but contain synthetic data and do
not incur charges. Production mode requires the explicit ``DATAFORSEO_ENV=prod`` setting or
``env="prod"`` argument. The client never switches automatically: pipelines should be built and
validated in the sandbox, while spending money must be a deliberate decision.
"""

from __future__ import annotations

import base64
import json
import math
import os
import re
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import date, timedelta

from seohead.data_sources import spend, spend_gate
from seohead.data_sources.credentials import (
    MissingCredential,
    dataforseo_login,
    dataforseo_password,
)
from seohead.data_sources.http import open_no_redirect

PROD_BASE = "https://api.dataforseo.com/"
SANDBOX_BASE = "https://sandbox.dataforseo.com/"
SOURCE = "dataforseo"
TIMEOUT = 120
RETRIES = 3

# These countries are unavailable, not merely poorly supported. Russian aliases below are
# functional input data and intentionally remain localized.
UNSUPPORTED = {
    "RU": "Russia is unavailable in Labs, Google Ads, and SERP",
    "BY": "Belarus locations are unavailable across DataForSEO services",
}
COUNTRY_ALIASES = {
    "россия": "RU",
    "russia": "RU",
    "russian federation": "RU",
    "рф": "RU",
    "беларусь": "BY",
    "белоруссия": "BY",
    "belarus": "BY",
    "рб": "BY",  # noqa: RUF001 - Functional Russian abbreviation for Belarus.
}
# ``location_code`` is the field DataForSEO actually bills on; ``country`` is only advisory text
# a caller may never fill in. These are the country-level Google Ads geo-target IDs for Russia
# and Belarus (the two markets DataForSEO excludes).
UNSUPPORTED_LOCATION_CODES: dict[int, str] = {
    2643: "RU",
    2112: "BY",
}
FALLBACK_TOOLS = [
    "Yandex Wordstat via Yandex Cloud for volume and expansion (keywords-expand)",
    "Arsenkin for exact !W volume and SERP clustering (keywords-exact)",
    "Yandex SERP via serp-fetch",
]

# Google endpoints currently required by the public toolkit.
ENDPOINTS = {
    "search_volume": "v3/keywords_data/google_ads/search_volume/live",
    "keyword_ideas": "v3/dataforseo_labs/google/keyword_ideas/live",
    "keyword_difficulty": "v3/dataforseo_labs/google/bulk_keyword_difficulty/live",
    "serp": "v3/serp/google/organic/live/advanced",
    "backlinks_summary": "v3/backlinks/summary/live",
    "backlinks": "v3/backlinks/backlinks/live",
    "referring_domains": "v3/backlinks/referring_domains/live",
    "backlinks_history": "v3/backlinks/history/live",
    "domain_pages_summary": "v3/backlinks/domain_pages_summary/live",
}


class DataForSEOError(RuntimeError):
    def __init__(self, status: int, message: str):
        super().__init__(f"DataForSEO {status}: {message}")
        self.status = status
        self.message = message


def _rejection(iso: str) -> dict:
    return {
        "ok": False,
        "unsupported_geo": iso,
        "error": f"DataForSEO does not cover {iso}: {UNSUPPORTED[iso]}",
        "use_instead": FALLBACK_TOOLS,
    }


def geo_guard(country: str | None, location_code: int | None = None) -> dict | None:
    """Return a rejection for unsupported geographies, or ``None`` when a call is allowed.

    This is a cost guard, not defensive overreach. Without it, a Russian request can reach the
    paid API, incur a charge, and return an empty list. ``location_code`` is checked first
    because it is the field actually transmitted in the request body; ``country`` is only an
    advisory string that a caller can supply a numeric geo-target without ever filling in.
    """
    if location_code is not None:
        try:
            code = int(location_code)
        except (TypeError, ValueError):
            code = None
        if code in UNSUPPORTED_LOCATION_CODES:
            return _rejection(UNSUPPORTED_LOCATION_CODES[code])
    if not country:
        return None
    iso = COUNTRY_ALIASES.get(country.strip().lower(), country.strip().upper())
    if iso not in UNSUPPORTED:
        return None
    return _rejection(iso)


class DataForSEOClient:
    def __init__(
        self, login: str | None = None, password: str | None = None, env: str | None = None
    ):
        self.env = (env or os.environ.get("DATAFORSEO_ENV") or "sandbox").lower()
        if self.env not in {"sandbox", "prod"}:
            raise ValueError("env must be sandbox or prod")
        self.base = PROD_BASE if self.env == "prod" else SANDBOX_BASE
        self._login = login
        self._password = password

    @property
    def _auth(self) -> str:
        login = self._login or dataforseo_login()
        password = self._password or dataforseo_password()
        return "Basic " + base64.b64encode(f"{login}:{password}".encode()).decode()

    def post(self, endpoint: str, payload: list[dict], *, operation: str | None = None) -> dict:
        """POST to an endpoint; DataForSEO always expects a task list, even for one task.

        An HTTP error response (429/5xx) means the provider replied, so retrying it is the
        ordinary safe case and is unchanged. A network-level exception means the response was
        lost, not that the request never arrived: with no idempotency key on this endpoint, the
        provider may already have created and billed the task, so it is not retried. The attempt
        is logged before the exception is raised, so a lost response is not an untracked charge.
        """
        url = self.base + endpoint.lstrip("/")
        data = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        label = operation or endpoint
        attempt = 0
        while True:
            attempt += 1
            request = urllib.request.Request(
                url,
                data=data,
                method="POST",
                headers={"Authorization": self._auth, "Content-Type": "application/json"},
            )
            try:
                # The request URL is built from the fixed HTTPS provider base.
                with open_no_redirect(request, timeout=TIMEOUT) as response:
                    raw = response.read().decode("utf-8")
                    try:
                        return json.loads(raw)
                    except ValueError:
                        # The response was received — a receipt must exist even though it
                        # cannot be parsed, so a real charge is never silently untracked. This
                        # is neither a confirmed charge nor a confirmed zero cost, so cost stays
                        # unmeasured and both flags say so explicitly.
                        spend.record(
                            SOURCE,
                            label,
                            cost=0.0,
                            unit="usd",
                            extra={
                                "response_received": True,
                                "response_malformed": True,
                                "charge_status": "unknown",
                                "cost_unknown": True,
                                "status": response.status,
                            },
                        )
                        raise
            except urllib.error.HTTPError as exc:
                text = exc.read().decode("utf-8", "replace")
                if exc.code in (429, 500, 502, 503) and attempt <= RETRIES:
                    time.sleep(2**attempt)
                    continue
                raise DataForSEOError(exc.code, _message(text)) from None
            except (urllib.error.URLError, TimeoutError) as exc:
                spend.record(
                    SOURCE,
                    label,
                    cost=0.0,
                    unit="usd",
                    extra={
                        "attempt_failed": "network_error",
                        "detail": str(exc),
                        # The response was lost, so the charge is unknown, not a measured zero.
                        "charge_status": "unknown",
                        "cost_unknown": True,
                    },
                )
                raise DataForSEOError(
                    0, f"network error, response lost; task may already be billed: {exc}"
                ) from None

    def balance(self) -> dict:
        """Return account balance; the sandbox value is synthetic by design."""
        url = self.base + "v3/appendix/user_data"
        request = urllib.request.Request(url, headers={"Authorization": self._auth})
        # The request URL is built from the fixed HTTPS provider base.
        with open_no_redirect(request, timeout=TIMEOUT) as response:
            body = json.loads(response.read().decode("utf-8"))
        info = ((body.get("tasks") or [{}])[0].get("result") or [{}])[0]
        return {
            "env": self.env,
            "balance": (info.get("money") or {}).get("balance"),
            "limits": info.get("rates"),
            "prices": info.get("price"),
        }


def _message(raw: str) -> str:
    try:
        body = json.loads(raw)
    except ValueError:
        return raw[:300] or "empty response"
    if isinstance(body, dict):
        if body.get("status_message"):
            return str(body["status_message"])
        tasks = body.get("tasks")
        if isinstance(tasks, list) and tasks and isinstance(tasks[0], dict):
            return str(tasks[0].get("status_message") or raw[:300])
    return raw[:300]


def task_items(body: dict) -> list[dict]:
    """Flatten ``tasks[].result[].items[]`` into one list.

    DataForSEO has three nested levels, and each level may contain ``None`` instead of a list: a
    task can finish without a result, and a result can contain no items. Direct indexing through
    ``[0]["result"][0]["items"]`` fails on those valid responses.
    """
    items: list[dict] = []
    for task in body.get("tasks") or []:
        for result in task.get("result") or []:
            if isinstance(result, dict):
                if "items" in result:
                    nested = result["items"]
                    if isinstance(nested, list):
                        items.extend(x for x in nested if isinstance(x, dict))
                else:
                    items.append(result)
    return items


def _status_failed(status: object) -> bool:
    """Return whether a provider status is present and does not confirm success."""
    if status is None:
        return False
    try:
        return int(status) != 20000
    except (TypeError, ValueError):
        return True


def task_errors(body: dict) -> list[str]:
    """Return failed request and task messages; status code 20000 is success."""
    errors = []
    response_code = body.get("status_code")
    if _status_failed(response_code):
        errors.append(f"{response_code}: {body.get('status_message')}")
    for task in body.get("tasks") or []:
        code = task.get("status_code")
        if _status_failed(code):
            errors.append(f"{code}: {task.get('status_message')}")
    if not body.get("tasks") and not errors:
        errors.append("provider response contains no task evidence")
    return errors


def tasks_all_failed(body: dict) -> bool:
    """True when the provider rejected the request or every task failed.

    A successful response may contain no matching items, but a non-success top-level status means
    no task result can be treated as evidence of that success.
    """
    response_code = body.get("status_code")
    if _status_failed(response_code):
        return True
    tasks = body.get("tasks") or []
    if not tasks:
        return True
    return all(_status_failed(t.get("status_code")) or t.get("status_code") is None for t in tasks)


def _run(
    client: DataForSEOClient, kind: str, payload: list[dict], items_count: int
) -> tuple[list[dict], list[str], float, bool]:
    body = client.post(ENDPOINTS[kind], payload, operation=f"{kind}.{client.env}")
    if not isinstance(body, dict):
        spend.record(
            SOURCE,
            f"{kind}.{client.env}",
            cost=0.0,
            unit="usd",
            items=items_count,
            extra={
                "env": client.env,
                "response_received": True,
                "response_malformed": True,
                "cost_unknown": True,
                "charge_status": "unknown",
            },
        )
        raise DataForSEOError(0, "provider response must be an object; charge is unknown")
    raw_cost = body.get("cost")
    cost = (
        float(raw_cost)
        if isinstance(raw_cost, (int, float))
        and not isinstance(raw_cost, bool)
        and math.isfinite(raw_cost)
        and raw_cost >= 0
        else None
    )
    spend.record(
        SOURCE,
        f"{kind}.{client.env}",
        cost=cost if cost is not None else 0.0,
        unit="usd",
        items=items_count,
        extra={
            "env": client.env,
            "cost_unknown": cost is None,
            "charge_status": "unknown" if cost is None else "measured",
        },
    )
    return task_items(body), task_errors(body), cost, tasks_all_failed(body)


def estimate(
    operation: str,
    *,
    query_count: int = 1,
    row_limit: int = 0,
    cache_hits: int = 0,
    prices: dict | None = None,
    env: str | None = None,
    query: str | None = None,
) -> dict:
    """Estimate a bounded call using a supplied appendix/user_data price snapshot.

    No credential lookup or network request happens here. Provider prices omit additional paid
    parameters; this adapter transmits none of them. Unknown pricing stays unavailable.
    """
    if operation not in ENDPOINTS:
        raise ValueError("unsupported DataForSEO operation")
    for name, value in (
        ("query_count", query_count),
        ("row_limit", row_limit),
        ("cache_hits", cache_hits),
    ):
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            raise ValueError(f"{name} must be a non-negative integer")
    if cache_hits > query_count:
        raise ValueError("cache_hits cannot exceed query_count")
    requested_env = (env or os.environ.get("DATAFORSEO_ENV") or "sandbox").lower()
    if requested_env not in {"sandbox", "prod"}:
        raise ValueError("env must be sandbox or prod")
    result = {
        "operation": operation,
        "env": requested_env,
        "currency": "USD",
        "query_count": query_count,
        "cache_hits": cache_hits,
        "cache_basis": "caller_declared",
        "billable_queries": query_count - cache_hits,
        "row_limit": row_limit,
        "cost_usd": None,
        "state": "unavailable",
        "reason": "a current appendix/user_data price snapshot is required",
    }
    if requested_env == "sandbox" or query_count == cache_hits:
        result.update(
            cost_usd=0.0, state="complete", reason="sandbox or cached data incurs no new charge"
        )
        return result
    serp_pages = max(1, math.ceil(row_limit / 10)) if operation == "serp" else 1
    search_multiplier = 1
    if operation == "serp":
        if query is None:
            result["reason"] = "the SERP query is required to account for search-operator pricing"
            return result
        if not isinstance(query, str):
            raise ValueError("query must be a string")
        if re.search(
            r"\b(?:allinanchor|allintext|allintitle|allinurl|cache|define|definition|filetype|id|inanchor|info|intext|intitle|inurl|link|site):",
            urllib.parse.unquote_plus(query),
            re.I,
        ):
            search_multiplier = 5
        result.update(
            billable_serp_pages_per_query=serp_pages, search_operator_multiplier=search_multiplier
        )
    node = prices or {}
    for part in ENDPOINTS[operation].split("/")[1:]:
        node = node.get(part, {}) if isinstance(node, dict) else {}
    rates = node.get("priority_normal", []) if isinstance(node, dict) else []
    if not isinstance(rates, list) or not rates:
        return result
    per_request = per_result = 0.0
    for rate in rates:
        if not isinstance(rate, dict):
            return result
        kind = str(rate.get("cost_type", "")).replace("_", "")
        cost = rate.get("cost")
        if (
            isinstance(cost, bool)
            or not isinstance(cost, (int, float))
            or not math.isfinite(cost)
            or cost < 0
        ):
            return result
        if kind == "perrequest":
            per_request += cost
        elif kind == "perresult":
            per_result += cost
        else:
            result["reason"] = "the supplied price uses an unsupported billing unit"
            return result
    try:
        total = (
            (per_request * serp_pages + per_result * row_limit)
            * search_multiplier
            * (query_count - cache_hits)
        )
    except OverflowError:
        total = float("inf")
    if not math.isfinite(total):
        result["reason"] = "supplied pricing or volume exceeds a finite quote"
        return result
    result.update(
        cost_usd=round(total, 10),
        state="complete",
        reason="upper bound from supplied provider base prices",
        pricing_source="appendix/user_data",
    )
    return result


def _preflight(
    operation: str,
    *,
    env: str | None,
    confirm_paid: bool,
    spend_ceiling_usd: float | None,
    prices: dict | None,
    row_limit: int,
    require_confirmation: bool = False,
    query: str | None = None,
) -> tuple[dict, dict | None]:
    quote = estimate(operation, row_limit=row_limit, prices=prices, env=env, query=query)
    if spend_ceiling_usd is not None and (
        isinstance(spend_ceiling_usd, bool)
        or not isinstance(spend_ceiling_usd, (int, float))
        or not math.isfinite(spend_ceiling_usd)
        or spend_ceiling_usd < 0
    ):
        raise ValueError("spend_ceiling_usd must be a finite non-negative number")
    if quote["env"] == "prod" or require_confirmation:
        refusal = spend_gate.confirmation_required(confirm_paid, operation, quote)
        if refusal:
            return quote, refusal
    if spend_ceiling_usd is not None:
        if quote["cost_usd"] is None:
            return quote, {
                "ok": False,
                "state": "unavailable",
                "reason": "cannot enforce spend ceiling without current pricing",
                "estimate": quote,
            }
        if quote["cost_usd"] > spend_ceiling_usd:
            return quote, {
                "ok": False,
                "state": "spend_limit_exceeded",
                "reason": "estimated cost exceeds spend_ceiling_usd before request",
                "estimate": quote,
            }
    return quote, None


def _call(
    kind: str,
    env: str | None,
    payload: list[dict],
    items_count: int,
    *,
    confirm_paid: bool = False,
    spend_ceiling_usd: float | None = None,
    prices: dict | None = None,
    row_limit: int = 0,
) -> tuple[DataForSEOClient, list[dict], list[str], float, bool] | dict:
    """Gate before constructing a client; never send a production call implicitly."""
    quote, refusal = _preflight(
        kind,
        env=env,
        confirm_paid=confirm_paid,
        spend_ceiling_usd=spend_ceiling_usd,
        prices=prices,
        row_limit=row_limit,
        query=payload[0].get("keyword") if kind == "serp" else None,
    )
    if refusal:
        return refusal
    try:
        client = DataForSEOClient(env=quote["env"])
        items, errors, cost, failed = _run(client, kind, payload, items_count)
    except MissingCredential as exc:
        return {"ok": False, "state": "not_configured", "error": str(exc)}
    except DataForSEOError as exc:
        return {"ok": False, "state": "failed", "error": exc.message, "status": exc.status}
    if spend_ceiling_usd is not None and cost is not None and cost > spend_ceiling_usd:
        return {
            "ok": False,
            "state": "partial",
            "error": "provider-reported cost exceeds declared ceiling",
            "cost_usd": cost,
            "estimate": quote,
        }
    return client, items, errors, cost, failed


# --- operations -------------------------------------------------------------


def search_volume(
    keywords: list[str],
    *,
    location_code: int = 2840,
    language: str = "en",
    country: str | None = None,
    env: str | None = None,
    confirm_paid: bool = False,
    spend_ceiling_usd: float | None = None,
    prices: dict | None = None,
) -> dict:
    """Return Google search volume for keywords; ``location_code`` 2840 is the United States."""
    blocked = geo_guard(country, location_code)
    if blocked:
        return blocked
    result = _call(
        "search_volume",
        env,
        [
            {
                "keywords": list(keywords),
                "location_code": location_code,
                "language_code": language,
            }
        ],
        len(keywords),
        confirm_paid=confirm_paid,
        spend_ceiling_usd=spend_ceiling_usd,
        prices=prices,
        row_limit=len(keywords),
    )
    if isinstance(result, dict):
        return result
    client, items, errors, cost, failed = result
    return {
        "ok": not failed,
        "env": client.env,
        "cost_usd": cost,
        "errors": errors,
        "keywords": [
            {
                "phrase": i.get("keyword"),
                "volume": i.get("search_volume"),
                "cpc": i.get("cpc"),
                "competition": i.get("competition"),
            }
            for i in items
        ],
    }


def keyword_ideas(
    seed: str,
    *,
    location_code: int = 2840,
    language: str = "en",
    limit: int = 100,
    country: str | None = None,
    env: str | None = None,
    confirm_paid: bool = False,
    spend_ceiling_usd: float | None = None,
    prices: dict | None = None,
) -> dict:
    """Expand a seed phrase, analogous to Wordstat's left column but for Google."""
    blocked = geo_guard(country, location_code)
    if blocked:
        return blocked
    result = _call(
        "keyword_ideas",
        env,
        [
            {
                "keywords": [seed],
                "location_code": location_code,
                "language_code": language,
                "limit": int(limit),
            }
        ],
        1,
        confirm_paid=confirm_paid,
        spend_ceiling_usd=spend_ceiling_usd,
        prices=prices,
        row_limit=limit,
    )
    if isinstance(result, dict):
        return result
    client, items, errors, cost, failed = result
    return {
        "ok": not failed,
        "env": client.env,
        "seed": seed,
        "cost_usd": cost,
        "errors": errors,
        "found": len(items),
        "keywords": [
            {
                "phrase": i.get("keyword"),
                "volume": ((i.get("keyword_info") or {}).get("search_volume")),
                "difficulty": ((i.get("keyword_properties") or {}).get("keyword_difficulty")),
            }
            for i in items
        ],
    }


def keyword_difficulty(
    keywords: list[str],
    *,
    location_code: int = 2840,
    language: str = "en",
    country: str | None = None,
    env: str | None = None,
    confirm_paid: bool = False,
    spend_ceiling_usd: float | None = None,
    prices: dict | None = None,
) -> dict:
    """Return bulk keyword difficulty: the estimated effort required to reach top results."""
    blocked = geo_guard(country, location_code)
    if blocked:
        return blocked
    result = _call(
        "keyword_difficulty",
        env,
        [
            {
                "keywords": list(keywords),
                "location_code": location_code,
                "language_code": language,
            }
        ],
        len(keywords),
        confirm_paid=confirm_paid,
        spend_ceiling_usd=spend_ceiling_usd,
        prices=prices,
        row_limit=len(keywords),
    )
    if isinstance(result, dict):
        return result
    client, items, errors, cost, failed = result
    return {
        "ok": not failed,
        "env": client.env,
        "cost_usd": cost,
        "errors": errors,
        "keywords": [
            {"phrase": i.get("keyword"), "difficulty": i.get("keyword_difficulty")} for i in items
        ],
    }


def serp(
    query: str,
    *,
    location_code: int = 2840,
    language: str = "en",
    depth: int = 10,
    country: str | None = None,
    env: str | None = None,
    confirm_paid: bool = False,
    spend_ceiling_usd: float | None = None,
    prices: dict | None = None,
) -> dict:
    """Return the live Google organic results that currently rank for a query."""
    if type(depth) is not int or not 1 <= depth <= 200:
        raise ValueError("depth must be between 1 and 200")
    blocked = geo_guard(country, location_code)
    if blocked:
        return blocked
    result = _call(
        "serp",
        env,
        [
            {
                "keyword": query,
                "location_code": location_code,
                "language_code": language,
                "depth": int(depth),
            }
        ],
        1,
        confirm_paid=confirm_paid,
        spend_ceiling_usd=spend_ceiling_usd,
        prices=prices,
        row_limit=depth,
    )
    if isinstance(result, dict):
        return result
    client, items, errors, cost, failed = result
    organic = [i for i in items if i.get("type") == "organic"] or items
    return {
        "ok": not failed,
        "env": client.env,
        "query": query,
        "cost_usd": cost,
        "errors": errors,
        "docs": [
            {
                "pos": i.get("rank_absolute"),
                "url": i.get("url"),
                "domain": i.get("domain"),
                "title": i.get("title"),
            }
            for i in organic
        ],
    }


def backlink_call(
    operation: str,
    target: str,
    *,
    limit: int = 100,
    offset: int = 0,
    enabled: bool = False,
    account_eligible: bool = False,
    production_approved: bool = False,
    cost_approved: bool = False,
    confirm_paid: bool = False,
    spend_ceiling_usd: float | None = None,
    cache_key: str | None = None,
    env: str | None = None,
    prices: dict | None = None,
    client: DataForSEOClient | None = None,
    date_from: str | None = None,
    date_to: str | None = None,
) -> dict:
    """Run one explicitly selected bounded backlink-index operation, never auto-paginate."""
    if operation not in {
        "backlinks_summary",
        "backlinks",
        "referring_domains",
        "backlinks_history",
        "domain_pages_summary",
    }:
        raise ValueError("unsupported backlink operation")
    if not target or not isinstance(target, str):
        raise ValueError("target required")
    if isinstance(limit, bool) or not isinstance(limit, int) or not 1 <= limit <= 1000:
        raise ValueError("limit must be between 1 and 1000")
    if isinstance(offset, bool) or not isinstance(offset, int) or offset < 0:
        raise ValueError("offset must be a non-negative integer")
    if not enabled:
        return {
            "ok": False,
            "state": "skipped",
            "reason": "backlink adapter is disabled by default",
        }
    if not account_eligible or not cache_key:
        return {
            "ok": False,
            "state": "skipped",
            "reason": "account eligibility and an explicit local cache key are required",
        }
    requested_env = (env or os.environ.get("DATAFORSEO_ENV") or "sandbox").lower()
    if client is not None and client.env != requested_env:
        raise ValueError("injected client environment must match requested environment")
    if requested_env == "prod" and (not production_approved or not cost_approved):
        return {
            "ok": False,
            "state": "skipped",
            "reason": "production backlink calls require explicit production and cost approval",
        }
    history_dates = None
    if operation == "backlinks_history":
        end_date = date.fromisoformat(date_to) if date_to else date.today()
        start_date = date.fromisoformat(date_from) if date_from else end_date - timedelta(days=30)
        if start_date < date(2019, 1, 1) or start_date > end_date or end_date > date.today():
            raise ValueError("history dates must be ordered within 2019-01-01 and today")
        months = (end_date.year - start_date.year) * 12 + end_date.month - start_date.month + 1
        if months > limit:
            raise ValueError("history date range exceeds the declared row limit")
        history_dates = {"date_from": start_date.isoformat(), "date_to": end_date.isoformat()}
    row_limit = 1 if operation in {"backlinks_summary", "domain_pages_summary"} else limit
    quote, refusal = _preflight(
        operation,
        env=requested_env,
        confirm_paid=confirm_paid,
        spend_ceiling_usd=spend_ceiling_usd,
        prices=prices,
        row_limit=row_limit,
        require_confirmation=True,
    )
    if refusal:
        return refusal
    payload = {"target": target}
    if history_dates:
        payload.update(history_dates)
    if operation in {"backlinks", "referring_domains"}:
        payload.update(limit=limit, offset=offset)
    elif operation == "backlinks_summary":
        payload.update(internal_list_limit=0, backlinks_limit=0)
    try:
        source = client or DataForSEOClient(env=requested_env)
        items, errors, cost, failed = _run(source, operation, [payload], 1)
    except MissingCredential as exc:
        return {"ok": False, "state": "not_configured", "error": str(exc)}
    except DataForSEOError as exc:
        return {"ok": False, "state": "failed", "error": exc.message, "status": exc.status}
    if spend_ceiling_usd is not None and cost is not None and cost > spend_ceiling_usd:
        return {
            "ok": False,
            "state": "partial",
            "error": "provider-reported cost exceeds the declared spend ceiling",
            "cost_usd": cost,
            "estimate": quote,
        }
    return {
        "ok": not failed,
        "state": "complete" if not failed else "failed",
        "env": source.env,
        "operation": operation,
        "api_version": "v3",
        "cache_key": cache_key,
        "cost_usd": cost,
        "errors": errors,
        "estimate": quote,
        "items": items,
        "summary": items if operation == "backlinks_summary" else None,
        "coverage": {"bounded": True, "limit": limit, "offset": offset, "auto_paginated": False},
    }


def backlinks_summary(target: str, **kwargs) -> dict:
    """Return one optional backlink summary with explicit confirmation."""
    return backlink_call("backlinks_summary", target, **kwargs)


def backlinks(target: str, **kwargs) -> dict:
    """Return one bounded page of backlinks."""
    return backlink_call("backlinks", target, **kwargs)


def referring_domains(target: str, **kwargs) -> dict:
    """Return one bounded page of referring domains."""
    return backlink_call("referring_domains", target, **kwargs)


def backlinks_history(target: str, **kwargs) -> dict:
    """Return provider history without inferring profile loss from missing rows."""
    return backlink_call("backlinks_history", target, **kwargs)


def domain_pages_summary(target: str, **kwargs) -> dict:
    """Return the provider domain-pages summary."""
    return backlink_call("domain_pages_summary", target, **kwargs)
