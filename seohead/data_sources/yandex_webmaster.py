"""Read-only Yandex Webmaster REST adapter with injected transport support."""

from __future__ import annotations

import json
import time
import urllib.error
import urllib.parse
import urllib.request
from collections.abc import Callable
from datetime import date, timedelta
from typing import Any

from seohead.data_sources.http import open_no_redirect

HOST = "https://api.webmaster.yandex.net/v4"
TIMEOUT = 30
Transport = Callable[[str, str, dict[str, Any] | None, str], str]


def _default_transport(method: str, url: str, payload: dict[str, Any] | None, token: str) -> str:
    request = urllib.request.Request(
        url,
        data=json.dumps(payload).encode() if payload else None,
        method=method,
        headers={"Authorization": f"OAuth {token}", "Content-Type": "application/json"},
    )
    with open_no_redirect(request, timeout=TIMEOUT) as response:
        return response.read().decode("utf-8")


# Operation -> API v4 path below ``/user/{user_id}``; ``{host}`` marks host-level reads.
OPERATIONS: dict[str, str] = {
    "hosts": "/hosts",
    "indexing": "/hosts/{host}/indexing/samples",
    "crawl": "/hosts/{host}/search-urls/events/samples",
    "sitemaps": "/hosts/{host}/sitemaps",
    "search_performance": "/hosts/{host}/search-queries/popular",
    "summary": "/hosts/{host}/summary",
    "diagnostics": "/hosts/{host}/diagnostics",
    "sqi_history": "/hosts/{host}/sqi-history",
    "search_history": "/hosts/{host}/search-queries/all/history",
    "query_history": "/hosts/{host}/search-queries/{query}/history",
    "in_search_history": "/hosts/{host}/search-urls/in-search/history",
    "events_history": "/hosts/{host}/search-urls/events/history",
    "indexing_history": "/hosts/{host}/indexing/history",
    "important_urls": "/hosts/{host}/important-urls",
    "broken_links_samples": "/hosts/{host}/links/internal/broken/samples",
    "broken_links_history": "/hosts/{host}/links/internal/broken/history",
    "external_links_history": "/hosts/{host}/links/external/history",
    "external_links_samples": "/hosts/{host}/links/external/samples",
}
# Paged sample lists: operation -> (key of the list in the answer, documented page-size cap).
# Popular queries allow up to 500 per page; the sample lists cap at 100.
PAGED: dict[str, tuple[str, int]] = {
    "search_performance": ("queries", 500),
    "crawl": ("samples", 100),
    "indexing": ("samples", 100),
    "broken_links_samples": ("links", 100),
    "external_links_samples": ("links", 100),
}
MAX_ROWS = 50_000
DEFAULT_PARAMS = {"search_performance": {"order_by": "TOTAL_SHOWS"}}
QUERY_ANALYTICS_PATH = "/hosts/{host}/query-analytics/list"
QUERY_ANALYTICS_PAGE = 500
_QUERY_FIELDS = {"IMPRESSIONS", "CLICKS", "CTR", "POSITION", "DEMAND"}


def _daily_statistics(statistics: list[Any]) -> list[dict[str, Any]]:
    buckets: dict[str, dict[str, float]] = {}
    for item in statistics:
        if not isinstance(item, dict) or not isinstance(item.get("date"), str):
            raise ValueError("malformed Yandex Webmaster query statistic")
        field, value = item.get("field"), item.get("value")
        if field not in _QUERY_FIELDS or type(value) not in (int, float):
            raise ValueError("malformed Yandex Webmaster query statistic")
        bucket = buckets.setdefault(item["date"], {})
        if field in {"IMPRESSIONS", "CLICKS", "DEMAND"}:
            bucket[field] = bucket.get(field, 0.0) + float(value)
        else:
            bucket[field] = float(value)
    rows = []
    for day, values in sorted(buckets.items()):
        impressions = values.get("IMPRESSIONS", 0.0)
        if impressions <= 0:
            continue
        clicks = values.get("CLICKS", 0.0)
        rows.append(
            {
                "date": day,
                "impressions": impressions,
                "clicks": clicks,
                "demand": values.get("DEMAND"),
                "ctr": clicks / impressions,
                "position": values.get("POSITION"),
                "unit": "provider_reported",
            }
        )
    return rows


def resolve_user_id(token: str, transport: Transport | None = None) -> str:
    """Return the account user id that every other API v4 path starts with."""
    body = json.loads((transport or _default_transport)("GET", HOST + "/user", None, token))
    return str(body["user_id"])


def _sample_page(page: Any, list_key: str) -> tuple[list[Any], int]:
    """Validate one documented paged response and return its row list and total count."""
    if not isinstance(page, dict):
        raise ValueError("malformed Yandex Webmaster response")
    chunk = page.get(list_key)
    if not isinstance(chunk, list):
        raise ValueError(f"malformed Yandex Webmaster response: '{list_key}' must be a list")
    count = page.get("count")
    if isinstance(count, str):
        try:
            count = int(count)
        except ValueError:
            count = None
    if isinstance(count, bool) or not isinstance(count, int) or count < 0:
        raise ValueError(
            "malformed Yandex Webmaster response: 'count' must be a nonnegative integer"
        )
    return chunk, count


def collect(
    operation: str,
    *,
    user_id: str | None = None,
    host_id: str | None = None,
    query_id: str | None = None,
    params: dict[str, Any] | None = None,
    paginate: bool = False,
    max_rows: int = MAX_ROWS,
    token: str | None = None,
    transport: Transport | None = None,
) -> dict[str, Any]:
    """Collect one read-only Yandex Webmaster API v4 resource.

    ``user_id`` is resolved from the token when omitted. ``params`` become the query string;
    a list value repeats the key (``query_indicator=TOTAL_SHOWS&query_indicator=TOTAL_CLICKS``).
    With ``paginate`` the paged sample lists (search queries, crawl events, indexing and broken
    links samples) are read page by page up to ``max_rows`` — each page requests at most the
    remaining row budget; without it one page is still read and validated. ``truncated`` is
    set whenever the documented ``count`` says rows remain unread — a ``max_rows`` cut, a
    single-page slice, or a page that ended early — and the state becomes ``partial``; a
    response without the documented list or count, or whose ``count`` contradicts the rows
    returned, fails.
    """
    from seohead.data_sources.credentials import MissingCredential, yandex_webmaster_token

    if operation not in OPERATIONS:
        raise ValueError(f"unsupported operation; supported: {', '.join(OPERATIONS)}")
    if "{host}" in OPERATIONS[operation] and not host_id:
        raise ValueError("host_id is required for this Yandex Webmaster operation")
    if "{query}" in OPERATIONS[operation] and not query_id:
        raise ValueError("query_id (from search_performance) is required for query_history")
    try:
        bearer = token or yandex_webmaster_token()
    except MissingCredential as exc:
        return {"ok": False, "state": "not_configured", "verified": False, "error": str(exc)}
    send = transport or _default_transport

    query = dict(DEFAULT_PARAMS.get(operation, {}), **(params or {}))
    spec = PAGED.get(operation)
    if paginate and spec is not None and max_rows < 1:
        raise ValueError("paginate requires a positive max_rows")
    try:
        user = user_id or resolve_user_id(bearer, send)
        path = (
            HOST
            + f"/user/{user}"
            + (
                OPERATIONS[operation]
                .replace("{host}", host_id or "")
                .replace("{query}", urllib.parse.quote(query_id or "", safe=""))
            )
        )

        def get(extra: dict[str, Any]) -> Any:
            encoded = urllib.parse.urlencode(dict(query, **extra), doseq=True)
            return json.loads(send("GET", path + (f"?{encoded}" if encoded else ""), None, bearer))

        truncated = False
        returned = None
        if spec is None:
            body = get({})
        else:
            list_key, page_size = spec
            first, rows = None, []
            start = int(query.get("offset", 0))
            offset = start
            while True:
                limit = min(page_size, max_rows - len(rows))
                page = get({"offset": offset, "limit": limit} if paginate else {})
                chunk, count = _sample_page(page, list_key)
                if count < offset + len(chunk):
                    raise ValueError(
                        "malformed Yandex Webmaster response: 'count' is below the returned rows"
                    )
                first = first if first is not None else page
                rows.extend(chunk)
                offset += len(chunk)
                if not paginate:
                    break
                if len(rows) >= max_rows:
                    rows = rows[:max_rows]
                    break
                if offset >= count or len(chunk) < limit:
                    break
            truncated = start + len(rows) < count
            body = dict(first or {}, **{list_key: rows})
            returned = len(rows)
    except (
        urllib.error.HTTPError,
        urllib.error.URLError,
        TimeoutError,
        ValueError,
        KeyError,
    ) as exc:
        return {"ok": False, "state": "failed", "error": str(exc)}
    if not isinstance(body, dict):
        return {"ok": False, "state": "failed", "error": "malformed Yandex Webmaster response"}
    result = {
        "ok": True,
        "state": "partial" if truncated else "complete",
        "operation": operation,
        "data": body,
        "read_only": True,
    }
    if spec is not None:
        result.update(returned=returned, truncated=truncated)
    return result


def url_queries(
    host_id: str,
    *,
    url: str | None = None,
    url_contains: str | None = None,
    user_id: str | None = None,
    max_urls: int = 100,
    max_queries_per_url: int = QUERY_ANALYTICS_PAGE,
    start_date: str | None = None,
    end_date: str | None = None,
    token: str | None = None,
    transport: Transport | None = None,
) -> dict[str, Any]:
    """Return bounded Yandex query evidence for one URL or a capped URL population.

    The API's URL and query statistics are distinct populations. This helper preserves each
    URL/query pair and its daily statistics; it never adds CTR or average position across URLs.
    """
    from seohead.data_sources.credentials import MissingCredential, yandex_webmaster_token

    if (
        not host_id
        or (url and url_contains)
        or type(max_urls) is not int
        or not 1 <= max_urls <= MAX_ROWS
    ):
        raise ValueError("host_id, one optional URL filter, and max_urls in 1..50000 are required")
    if type(max_queries_per_url) is not int or not 1 <= max_queries_per_url <= MAX_ROWS:
        raise ValueError("max_queries_per_url must be in 1..50000")
    if bool(start_date) != bool(end_date):
        raise ValueError("pass both start_date and end_date")
    requested_days: list[str] | None = None
    if start_date and end_date:
        try:
            start, end = date.fromisoformat(start_date), date.fromisoformat(end_date)
        except ValueError as exc:
            raise ValueError("start_date and end_date must be ISO dates") from exc
        if start > end:
            raise ValueError("start_date must not be after end_date")
        if end < date.today() - timedelta(days=14):
            return {
                "ok": False,
                "state": "unavailable",
                "reason": "requested_date_outside_provider_rolling_window",
            }
        requested_days = [
            (start + timedelta(days=offset)).isoformat() for offset in range((end - start).days + 1)
        ]
    try:
        bearer = token or yandex_webmaster_token()
    except MissingCredential as exc:
        return {"ok": False, "state": "not_configured", "verified": False, "error": str(exc)}
    send = transport or _default_transport
    try:
        user = user_id or resolve_user_id(bearer, send)
        host_listing = collect("hosts", user_id=user, token=bearer, transport=send)
        if not host_listing.get("ok"):
            return {
                "ok": False,
                "state": "verification_failed",
                "error": "Webmaster host verification failed",
            }
        hosts = host_listing.get("data", {}).get("hosts")
        if not isinstance(hosts, list) or not all(isinstance(item, dict) for item in hosts):
            return {
                "ok": False,
                "state": "verification_failed",
                "error": "malformed Webmaster host list",
            }
        if host_id not in {item.get("host_id") for item in hosts}:
            return {"ok": False, "state": "not_granted", "host_id": host_id}
        endpoint = HOST + f"/user/{user}" + QUERY_ANALYTICS_PATH.replace("{host}", host_id)

        def request(
            indicator: str, value: str | None, cap: int
        ) -> tuple[list[dict[str, Any]], bool]:
            operation = "TEXT_CONTAINS" if value == url_contains else "TEXT_MATCH"
            filters = (
                []
                if value is None
                else [{"text_indicator": "URL", "operation": operation, "value": value}]
            )
            entries: list[dict[str, Any]] = []
            offset = 0
            total: int | None = None
            while len(entries) < cap:
                body = {
                    "offset": offset,
                    "limit": min(QUERY_ANALYTICS_PAGE, cap - len(entries)),
                    "device_type_indicator": "ALL",
                    "text_indicator": indicator,
                    "filters": {"text_filters": filters},
                }
                for attempt in range(3):
                    try:
                        parsed = json.loads(send("POST", endpoint, body, bearer))
                        break
                    except urllib.error.HTTPError as exc:
                        if attempt == 2 or (exc.code != 429 and not 500 <= exc.code <= 599):
                            raise
                        time.sleep(2**attempt + 1)
                chunk = (
                    parsed.get("text_indicator_to_statistics") if isinstance(parsed, dict) else None
                )
                count = parsed.get("count") if isinstance(parsed, dict) else None
                if not isinstance(chunk, list) or not all(isinstance(item, dict) for item in chunk):
                    raise ValueError("malformed Yandex Webmaster query analytics response")
                if count is not None and (type(count) is not int or count < offset + len(chunk)):
                    raise ValueError("malformed Yandex Webmaster query analytics count")
                total = count if isinstance(count, int) else total
                entries.extend(chunk)
                offset += len(chunk)
                if (
                    not chunk
                    or (total is not None and offset >= total)
                    or (total is None and len(chunk) < body["limit"])
                ):
                    break
            return entries, (total is not None and len(entries) < total) or (
                total is None and len(entries) >= cap
            )

        entries, urls_truncated = request("URL", url or url_contains, max_urls)
        urls = [item.get("text_indicator", {}).get("value") for item in entries]
        if not all(isinstance(value, str) and value for value in urls):
            raise ValueError("malformed Yandex Webmaster URL result")
        rows: list[dict[str, Any]] = []
        for page in urls:
            queries, query_truncated = request("QUERY", page, max_queries_per_url)
            for item in queries:
                query = item.get("text_indicator", {}).get("value")
                statistics = item.get("statistics")
                if not isinstance(query, str) or not isinstance(statistics, list):
                    raise ValueError("malformed Yandex Webmaster query result")
                daily = _daily_statistics(statistics)
                if requested_days is not None:
                    daily = [entry for entry in daily if entry["date"] in requested_days]
                if daily:
                    rows.append(
                        {"url": page, "query": query, "daily": daily, "truncated": query_truncated}
                    )
        observed_days = sorted({entry["date"] for row in rows for entry in row["daily"]})
        coverage = (
            {
                "requested_days": requested_days,
                "observed_days": observed_days,
                "unobserved_days": sorted(set(requested_days) - set(observed_days)),
                "state": "partial_or_unknown",
            }
            if requested_days is not None
            else None
        )
        return {
            "ok": True,
            "state": "partial"
            if urls_truncated or any(row["truncated"] for row in rows)
            else "complete",
            "host_id": host_id,
            "property_access": "verified",
            "result_state": "empty" if not rows else "observed",
            "rows": rows,
            "returned_urls": len(urls),
            "returned_queries": len(rows),
            "truncated": urls_truncated or any(row["truncated"] for row in rows),
            "coverage": coverage,
            "scope": "Yandex Webmaster query analytics; provider retention and metric attribution apply",
        }
    except urllib.error.HTTPError as exc:
        error_code = f"HTTP_{exc.code}"
        try:
            payload = json.loads(exc.read().decode("utf-8", "replace"))
            if isinstance(payload, dict) and isinstance(payload.get("error_code"), str):
                error_code = payload["error_code"]
        except (OSError, ValueError):
            pass
        return {"ok": False, "state": "failed", "error_code": error_code}
    except (urllib.error.URLError, TimeoutError, ValueError, KeyError):
        return {"ok": False, "state": "failed", "error": "Yandex Webmaster query analytics failed"}
