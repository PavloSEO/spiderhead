"""Google Search Console: what Google itself observed, not what a crawl inferred.

This is the biggest gap the free-sources issue names (#97). A crawl says what a page *is*; only
Search Console says whether Google indexed it, what it ranks for, and its own indexing verdict
for a single URL. Two operations cover that:

* ``search_analytics`` — clicks, impressions, average position, and CTR per query/page, for an
  own, verified property.
* ``inspect_url`` — the URL Inspection endpoint's indexing verdict for one URL.

**This is a credential-gated skeleton, not an exercised client.** Search Console requires OAuth
against a verified property; nothing in this environment can obtain or verify that. Both
functions parse a real response shape against recorded fixtures, but neither has been run
against the live API. A missing token returns an explicit, truthful failure — see
``credentials.gsc_access_token`` — never a fabricated or synthesized result.

**Date policy.** Search Analytics ``startDate``/``endDate`` must be ``YYYY-MM-DD`` calendar dates
in Pacific Time, with an inclusive range (``startDate <= endDate``); see
https://developers.google.com/webmaster-tools/v1/searchanalytics/query. Search Console has not
finished processing the current Pacific day, so ``default_date_range`` resolves to the last
*completed* inclusive 28-day window: it ends on yesterday in Pacific Time and starts 27 days
before that, so every day in the reported period represents a full day of data. The legacy
relative labels ``28daysAgo``/``today`` (still the public CLI/MCP defaults for
backward-compatible call sites) are recognized as a request for this same resolved window;
anything else must already be a valid ISO date.
"""

from __future__ import annotations

import json
import math
import re
import urllib.error
import urllib.parse
import urllib.request
from collections.abc import Callable
from datetime import date, datetime, timedelta
from typing import Any
from zoneinfo import ZoneInfo

from seohead.data_sources.http import open_no_redirect

SEARCH_ANALYTICS_HOST = "https://www.googleapis.com/webmasters/v3"
INSPECTION_HOST = "https://searchconsole.googleapis.com/v1"
TIMEOUT = 30
PACIFIC = ZoneInfo("America/Los_Angeles")
DEFAULT_WINDOW_DAYS = 28
READONLY_SCOPE = "https://www.googleapis.com/auth/webmasters.readonly"
_LEGACY_START_LABEL = "28daysAgo"
_LEGACY_END_LABEL = "today"
_ISO_DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")

# payload, bearer token -> response body text
Fetcher = Callable[[dict[str, Any], str], str]
RequestTransport = Callable[[str, str, dict[str, Any] | None, str], str]
MAX_INSPECTION_URLS = 50
MAX_INSPECTION_URL_BYTES = 8192
MAX_ANALYTICS_ROWS = 25_000
MAX_RESPONSE_BYTES = 8 * 1024 * 1024


class ResponseTooLarge(ValueError):
    """A Search Console response exceeded the local read budget."""
_ARCHIVE_DIMENSIONS = ("date", "query", "page", "country", "device", "searchAppearance")
_SEARCH_TYPES = ("web", "image", "video", "news", "discover", "googleNews")
_AGGREGATION_TYPES = ("auto", "byPage", "byProperty")


def default_date_range() -> tuple[str, str]:
    """Return the default completed, inclusive 28-day window as ``(start_date, end_date)``.

    ``end_date`` is yesterday in Pacific Time — the most recent day Search Console has fully
    processed — and ``start_date`` is 27 days before it, so the window covers exactly
    ``DEFAULT_WINDOW_DAYS`` complete calendar days.
    """
    end = datetime.now(PACIFIC).date() - timedelta(days=1)
    start = end - timedelta(days=DEFAULT_WINDOW_DAYS - 1)
    return start.isoformat(), end.isoformat()


def _is_iso_date(value: str) -> bool:
    if not isinstance(value, str) or not _ISO_DATE_RE.match(value):
        return False
    try:
        date.fromisoformat(value)
    except ValueError:
        return False
    return True


def _resolve_date_range(start_date: str | None, end_date: str | None) -> tuple[str, str]:
    """Resolve the outbound date pair, honoring the legacy relative-label sentinel.

    ``None``/``None`` (an omitted call) and the legacy ``28daysAgo``/``today`` labels both mean
    "the default window"; any other value is passed through unchanged for validation.
    """
    if (start_date is None and end_date is None) or (
        start_date == _LEGACY_START_LABEL and end_date == _LEGACY_END_LABEL
    ):
        return default_date_range()
    return start_date, end_date  # type: ignore[return-value]


def _validate_date_range(start_date: str, end_date: str) -> str | None:
    """Return an error message for an invalid/reversed range, or ``None`` when it is usable."""
    if not _is_iso_date(start_date) or not _is_iso_date(end_date):
        return (
            "start_date and end_date must be YYYY-MM-DD calendar dates in Pacific Time, got "
            f"{start_date!r} and {end_date!r}"
        )
    if start_date > end_date:
        return f"start_date must not be after end_date: {start_date!r} > {end_date!r}"
    return None


def _default_fetcher(url: str) -> Fetcher:
    def fetch(payload: dict[str, Any], token: str) -> str:
        data = json.dumps(payload).encode("utf-8")
        request = urllib.request.Request(
            url,
            data=data,
            method="POST",
            headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
        )
        # The request URL is the fixed HTTPS Search Console endpoint; the token travels in a
        # header, never in the URL, so it cannot end up echoed into a log line or a stack trace.
        with open_no_redirect(request, timeout=TIMEOUT) as response:
            raw = response.read(MAX_RESPONSE_BYTES + 1)
        if len(raw) > MAX_RESPONSE_BYTES:
            raise ResponseTooLarge("Search Console response exceeded the 8 MiB limit")
        return raw.decode("utf-8")

    return fetch


def _api_error(exc: urllib.error.HTTPError, secret: str | None = None) -> str:
    try:
        body = json.loads(exc.read().decode("utf-8", "replace"))
        message = str(body.get("error", {}).get("message") or exc.reason)
    except ValueError:
        message = str(exc.reason)
    return message.replace(secret, "[redacted]") if secret else message


def _response_object(raw: str) -> dict[str, Any] | None:
    try:
        body = json.loads(raw)
    except (AttributeError, ValueError):
        return None
    return body if isinstance(body, dict) else None


def search_analytics(
    site_url: str,
    *,
    start_date: str | None = None,
    end_date: str | None = None,
    dimensions: list[str] | None = None,
    row_limit: int = 1000,
    token: str | None = None,
    fetcher: Fetcher | None = None,
) -> dict[str, Any]:
    """Query rows a verified property earned in search, by query, page, country, or device.

    ``start_date``/``end_date`` default to :func:`default_date_range` (also recognizing the
    legacy ``28daysAgo``/``today`` labels as a request for that same window). Any other value
    must already be a valid ``YYYY-MM-DD`` date with ``start_date <= end_date``; an invalid or
    reversed range is rejected here, before Search Console is ever contacted.
    """
    if not site_url:
        raise ValueError("site_url required")
    start_date, end_date = _resolve_date_range(start_date, end_date)
    date_error = _validate_date_range(start_date, end_date)
    if date_error:
        return {"ok": False, "error": date_error}
    bearer, token_error = _acquire_token(token)
    if bearer is None:
        return {"ok": False, "error": token_error}

    url = f"{SEARCH_ANALYTICS_HOST}/sites/{urllib.parse.quote(site_url, safe='')}/searchAnalytics/query"
    payload = {
        "startDate": start_date,
        "endDate": end_date,
        "dimensions": dimensions or ["query"],
        "rowLimit": row_limit,
    }
    fetch = fetcher or _default_fetcher(url)
    try:
        raw = fetch(payload, bearer)
    except urllib.error.HTTPError as exc:
        return {"ok": False, "error": _api_error(exc, bearer), "status": exc.code}
    except ResponseTooLarge as exc:
        return {"ok": False, "error": str(exc), "state": "response_too_large"}
    except (urllib.error.URLError, TimeoutError) as exc:
        return {"ok": False, "error": f"Search Console request failed: {exc}"}

    body = _response_object(raw)
    if body is None:
        return {"ok": False, "error": "Search Console malformed response"}
    if "rows" not in body:
        rows = []
    else:
        rows = body["rows"]
        if not isinstance(rows, list) or not all(isinstance(row, dict) for row in rows):
            return {"ok": False, "error": "Search Console malformed response"}
    return {
        "ok": True,
        "site_url": site_url,
        "period": f"{start_date}..{end_date}",
        "count": len(rows),
        "rows": [
            {
                "keys": r.get("keys"),
                "clicks": r.get("clicks"),
                "impressions": r.get("impressions"),
                "ctr": r.get("ctr"),
                "position": r.get("position"),
            }
            for r in rows
        ],
    }


def inspect_url(
    site_url: str,
    inspection_url: str,
    *,
    token: str | None = None,
    fetcher: Fetcher | None = None,
) -> dict[str, Any]:
    """Google's own indexing verdict for one URL: indexed or not, and why."""
    if not site_url or not inspection_url:
        raise ValueError("site_url and inspection_url required")
    bearer, token_error = _acquire_token(token)
    if bearer is None:
        return {"ok": False, "error": token_error}

    url = f"{INSPECTION_HOST}/urlInspection/index:inspect"
    payload = {"inspectionUrl": inspection_url, "siteUrl": site_url}
    fetch = fetcher or _default_fetcher(url)
    try:
        raw = fetch(payload, bearer)
    except urllib.error.HTTPError as exc:
        return {"ok": False, "error": _api_error(exc, bearer), "status": exc.code}
    except ResponseTooLarge as exc:
        return {"ok": False, "error": str(exc), "state": "response_too_large"}
    except (urllib.error.URLError, TimeoutError) as exc:
        return {"ok": False, "error": f"Search Console request failed: {exc}"}

    body = _response_object(raw)
    if body is None:
        return {"ok": False, "error": "Search Console malformed response"}
    inspection = body.get("inspectionResult")
    if not isinstance(inspection, dict):
        return {"ok": False, "error": "Search Console malformed response"}
    index_status = inspection.get("indexStatusResult")
    if index_status is None:
        index_status = {}
    if not isinstance(index_status, dict):
        return {"ok": False, "error": "Search Console malformed response"}
    return {
        "ok": True,
        "site_url": site_url,
        "inspection_url": inspection_url,
        "verdict": index_status.get("verdict"),
        "coverage_state": index_status.get("coverageState"),
        "indexing_state": index_status.get("indexingState"),
        "last_crawl_time": index_status.get("lastCrawlTime"),
        "google_canonical": index_status.get("googleCanonical"),
        "user_canonical": index_status.get("userCanonical"),
    }


def _request(method: str, url: str, payload: dict[str, Any] | None, token: str) -> str:
    data = json.dumps(payload).encode("utf-8") if payload is not None else None
    request = urllib.request.Request(
        url,
        data=data,
        method=method,
        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
    )
    with open_no_redirect(request, timeout=TIMEOUT) as response:
        raw = response.read(MAX_RESPONSE_BYTES + 1)
    if len(raw) > MAX_RESPONSE_BYTES:
        raise ResponseTooLarge("Search Console response exceeded the 8 MiB limit")
    return raw.decode("utf-8")


_SERVICE_ACCOUNT_ERRORS = {
    "missing": "GSC service-account JSON file is not configured or readable",
    "unsafe_file": "GSC service-account JSON must be a private regular file (mode 0600)",
    "too_large": "GSC service-account JSON exceeds the maximum supported size",
    "unreadable": "GSC service-account JSON exists but cannot be read",
    "malformed_json": "GSC service-account JSON is not valid JSON",
    "unsupported_shape": "GSC service-account JSON has an unsupported type or token URI",
}


def _acquire_token(value: str | None) -> tuple[str | None, str | None]:
    """Choose an explicit bearer first, then a durable grant, then a service account."""
    from seohead.data_sources.credentials import (
        MissingCredential,
        gsc_access_token,
        gsc_service_account_status,
    )

    try:
        return value or gsc_access_token(), None
    except MissingCredential as bearer_error:
        from seohead.data_sources.oauth import grant_available

        if grant_available("gsc"):
            try:
                return durable_oauth_token()["access_token"], None
            except MissingCredential as refresh_error:
                if "invalid_grant" in str(refresh_error).lower():
                    return None, "invalid_grant"
                return None, "stored OAuth grant refresh failed; reconnect or check the grant"
            except (OSError, ValueError):
                return None, "stored OAuth grant refresh failed; reconnect or check the grant"
        status = gsc_service_account_status()
        if status == "missing":
            return None, str(bearer_error)
        if status != "configured_unverified":
            return None, f"OAuth bearer unavailable; {_SERVICE_ACCOUNT_ERRORS[status]}"
        try:
            return service_account_access_token(), None
        except MissingCredential as service_error:
            return None, f"OAuth bearer unavailable; service account unavailable: {service_error}"


def service_account_access_token(scope: str = READONLY_SCOPE) -> str:
    """Refresh one read-only Google token; the caller supplies a product scope."""
    from seohead.data_sources.credentials import (
        MissingCredential,
        gsc_service_account_document,
    )

    try:
        import requests
        from google.auth.transport.requests import Request
        from google.oauth2 import service_account
    except ImportError as exc:
        raise MissingCredential(
            "GSC service-account authentication requires the optional gsc extra (google-auth)"
        ) from exc
    status, info = gsc_service_account_document()
    if info is None:
        raise MissingCredential(_SERVICE_ACCOUNT_ERRORS[status])
    try:
        credentials = service_account.Credentials.from_service_account_info(info, scopes=[scope])
        session = requests.Session()
        session.max_redirects = 0
        credentials.refresh(Request(session=session))
    except Exception as exc:
        raise MissingCredential(
            "GSC service-account token refresh failed; check file permissions and Google grants"
        ) from exc
    if not isinstance(credentials.token, str) or not credentials.token:
        raise MissingCredential("GSC service-account token refresh returned no access token")
    return credentials.token


def durable_oauth_token(
    *, refresh_transport: Callable[[dict[str, str]], dict[str, Any]] | None = None
) -> dict[str, Any]:
    """Refresh an explicitly stored ``webmasters.readonly`` grant for a bounded operation."""
    from seohead.data_sources.oauth import refresh_access_token

    return refresh_access_token("gsc", transport=refresh_transport)


def discover_properties(
    *, token: str | None = None, transport: RequestTransport | None = None
) -> dict[str, Any]:
    """List properties the authenticated principal can access; never treats a token as verified."""
    bearer, token_error = _acquire_token(token)
    if bearer is None:
        return {"ok": False, "state": "not_configured", "verified": False, "error": token_error}
    try:
        body = _response_object(
            (transport or _request)("GET", f"{SEARCH_ANALYTICS_HOST}/sites", None, bearer)
        )
    except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError, ResponseTooLarge) as exc:
        result = {
            "ok": False,
            "state": "verification_failed",
            "verified": False,
            "error": str(exc),
        }
        if isinstance(exc, urllib.error.HTTPError):
            result["status"] = exc.code
            result["error"] = _api_error(exc, bearer)
        return result
    # Google omits siteEntry entirely when the principal has no properties: that is an empty
    # list, not a malformed response (a fresh service account before it is added anywhere).
    entries = body.get("siteEntry", []) if isinstance(body, dict) else None
    if not isinstance(entries, list) or not all(isinstance(entry, dict) for entry in entries):
        return {
            "ok": False,
            "state": "verification_failed",
            "verified": False,
            "error": "malformed GSC property response",
        }
    return {
        "ok": True,
        "verified": True,
        "properties": [
            {"site_url": entry.get("siteUrl"), "permission_level": entry.get("permissionLevel")}
            for entry in entries
        ],
        "scopes": ["https://www.googleapis.com/auth/webmasters.readonly"],
    }


def search_analytics_pages(
    site_url: str,
    *,
    start_date: str | None = None,
    end_date: str | None = None,
    dimensions: list[str] | None = None,
    row_limit: int = 1_000,
    max_rows: int = MAX_ANALYTICS_ROWS,
    token: str | None = None,
    transport: RequestTransport | None = None,
) -> dict[str, Any]:
    """Paginate bounded Search Analytics rows with explicit truncation and quota limits."""
    if not site_url or not 1 <= row_limit <= 25_000 or not 1 <= max_rows <= MAX_ANALYTICS_ROWS:
        raise ValueError("site_url and bounded row limits are required")
    start_date, end_date = _resolve_date_range(start_date, end_date)
    if error := _validate_date_range(start_date, end_date):
        return {"ok": False, "error": error}
    bearer, token_error = _acquire_token(token)
    if bearer is None:
        return {"ok": False, "state": "not_configured", "verified": False, "error": token_error}
    endpoint = f"{SEARCH_ANALYTICS_HOST}/sites/{urllib.parse.quote(site_url, safe='')}/searchAnalytics/query"
    request = transport or _request
    rows: list[dict[str, Any]] = []
    offset = 0
    while offset < max_rows:
        payload = {
            "startDate": start_date,
            "endDate": end_date,
            "dimensions": dimensions or ["page"],
            "rowLimit": min(row_limit, max_rows - offset),
            "startRow": offset,
        }
        try:
            body = _response_object(request("POST", endpoint, payload, bearer))
        except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError) as exc:
            return {"ok": False, "state": "failed", "error": str(exc), "returned": len(rows)}
        batch = body.get("rows", []) if body else None
        if not isinstance(batch, list) or not all(isinstance(row, dict) for row in batch):
            return {"ok": False, "state": "failed", "error": "malformed GSC analytics response"}
        rows.extend(batch)
        if len(batch) < payload["rowLimit"]:
            break
        offset += len(batch)
    return {
        "ok": True,
        "state": "complete" if len(rows) < max_rows else "partial",
        "period": {"start_date": start_date, "end_date": end_date},
        "dimensions": dimensions or ["page"],
        "rows": rows,
        "returned": len(rows),
        "truncated": len(rows) == max_rows,
        "quota_mode": "provider row limit; bounded locally",
    }


def search_analytics_page(
    site_url: str,
    *,
    start_date: str,
    end_date: str,
    dimensions: list[str] | None = None,
    search_type: str = "web",
    aggregation_type: str = "auto",
    data_state: str = "final",
    row_limit: int = 25_000,
    start_row: int = 0,
    dimension_filter_groups: list[dict[str, Any]] | None = None,
    token: str | None = None,
    transport: RequestTransport | None = None,
) -> dict[str, Any]:
    """Fetch one archive page without implicit pagination, retries, or dimension defaults.

    Empty dimensions request property totals. Missing CTR/position stay null, including for
    Discover and Google News. Failures never echo provider messages or exception text because
    either may include authentication material. ``status`` is an HTTP code, or None when no
    HTTP response exists; ``reason`` is a stable local code or an allowlisted Google reason.
    """

    def failure(reason: str, error: str, status: int | None = None) -> dict[str, Any]:
        return {
            "ok": False,
            "rows": [],
            "response_aggregation_type": None,
            "metadata": {},
            "error": error,
            "status": status,
            "reason": reason,
        }

    invalid = "Invalid Search Console page request"
    if not isinstance(site_url, str) or not site_url.strip():
        return failure("invalid_argument", invalid + ": site_url is required")
    if _validate_date_range(start_date, end_date):
        return failure("invalid_argument", invalid + ": use an ordered YYYY-MM-DD date range")
    if (
        type(row_limit) is not int
        or not 1 <= row_limit <= MAX_ANALYTICS_ROWS
        or type(start_row) is not int
        or not 0 <= start_row <= 50_000
    ):
        return failure(
            "invalid_argument", invalid + ": row_limit is 1..25000; start_row is 0..50000"
        )
    if (
        search_type not in _SEARCH_TYPES
        or aggregation_type not in _AGGREGATION_TYPES
        or data_state not in ("final", "all")
        or (search_type in ("discover", "googleNews") and aggregation_type == "byProperty")
    ):
        return failure(
            "invalid_argument", invalid + ": unsupported type, aggregation, or data state"
        )
    dimensions = [] if dimensions is None else dimensions
    if (
        not isinstance(dimensions, list)
        or not all(isinstance(d, str) and d in _ARCHIVE_DIMENSIONS for d in dimensions)
        or len(set(dimensions)) != len(dimensions)
    ):
        return failure("invalid_argument", invalid + ": invalid or repeated dimensions")
    if dimension_filter_groups is not None:
        if not isinstance(dimension_filter_groups, list):
            return failure("invalid_argument", invalid + ": filter groups must be a JSON list")
        for group in dimension_filter_groups:
            if (
                not isinstance(group, dict)
                or set(group) - {"groupType", "filters"}
                or group.get("groupType", "and") != "and"
                or not isinstance(group.get("filters"), list)
                or not group["filters"]
            ):
                return failure("invalid_argument", invalid + ": malformed filter group")
            for item in group["filters"]:
                if (
                    not isinstance(item, dict)
                    or set(item) != {"dimension", "operator", "expression"}
                    or item["dimension"] not in _ARCHIVE_DIMENSIONS
                    or item["operator"]
                    not in (
                        "contains",
                        "equals",
                        "notContains",
                        "notEquals",
                        "includingRegex",
                        "excludingRegex",
                    )
                    or not isinstance(item["expression"], str)
                ):
                    return failure("invalid_argument", invalid + ": malformed dimension filter")
    if token is not None and not isinstance(token, str):
        return failure("invalid_argument", invalid + ": token must be a string")

    def api_failure(body: dict[str, Any] | None, status: int | None) -> dict[str, Any]:
        error = body.get("error") if body else None
        reason = "api_error"
        known_reasons = {
            "quotaExceeded",
            "rateLimitExceeded",
            "userRateLimitExceeded",
            "dailyLimitExceeded",
            "servingLimitExceeded",
            "forbidden",
            "insufficientPermissions",
            "authError",
            "keyInvalid",
            "accessNotConfigured",
            "notFound",
            "invalid",
            "badRequest",
        }
        if isinstance(error, dict):
            if status is None and type(error.get("code")) is int:
                status = error["code"]
            details = error.get("errors", [])
            if isinstance(details, list):
                for detail in details:
                    candidate = detail.get("reason") if isinstance(detail, dict) else None
                    if isinstance(candidate, str) and candidate in known_reasons:
                        reason = candidate
                        break
        return failure(reason, "Search Console API request failed", status)

    try:
        bearer, _ = _acquire_token(token)
    except Exception:
        return failure("not_configured", "Search Console authentication is unavailable")
    if not isinstance(bearer, str) or not bearer:
        return failure("not_configured", "Search Console authentication is unavailable")
    endpoint = f"{SEARCH_ANALYTICS_HOST}/sites/{urllib.parse.quote(site_url, safe='')}/searchAnalytics/query"
    payload: dict[str, Any] = {
        "startDate": start_date,
        "endDate": end_date,
        "dimensions": dimensions,
        "type": search_type,
        "aggregationType": aggregation_type,
        "dataState": data_state,
        "rowLimit": row_limit,
        "startRow": start_row,
    }
    if dimension_filter_groups is not None:
        payload["dimensionFilterGroups"] = dimension_filter_groups
    try:
        raw = (transport or _request)("POST", endpoint, payload, bearer)
    except urllib.error.HTTPError as exc:
        try:
            error_body = _response_object(exc.read().decode("utf-8", "replace"))
        except (OSError, TypeError, ValueError):
            error_body = None
        return api_failure(error_body, exc.code)
    except (OSError, ValueError, TypeError):
        return failure("transport_error", "Search Console request failed before a valid response")
    try:
        body = _response_object(raw)
    except TypeError:
        body = None
    malformed = failure("malformed_response", "Search Console malformed analytics response", 200)
    if body is None:
        return malformed
    if "error" in body or "errors" in body:
        return api_failure(body, None)
    batch = body.get("rows", [])
    metadata = body.get("metadata", {})
    aggregation = body.get("responseAggregationType")
    if (
        not isinstance(batch, list)
        or len(batch) > row_limit
        or not isinstance(metadata, dict)
        or (aggregation is not None and aggregation not in _AGGREGATION_TYPES)
    ):
        return malformed
    rows = []
    for row in batch:
        if not isinstance(row, dict):
            return malformed
        keys = row.get("keys", [])
        if (
            not isinstance(keys, list)
            or len(keys) != len(dimensions)
            or not all(isinstance(key, str) for key in keys)
        ):
            return malformed
        for metric in ("clicks", "impressions", "ctr", "position"):
            value = row.get(metric)
            if value is None and metric in ("ctr", "position"):
                continue
            if (
                type(value) not in (int, float)
                or value < 0
                or (isinstance(value, float) and not math.isfinite(value))
            ):
                return malformed
        rows.append(
            {"keys": keys, **{k: row.get(k) for k in ("clicks", "impressions", "ctr", "position")}}
        )
    return {
        "ok": True,
        "rows": rows,
        "response_aggregation_type": aggregation,
        "metadata": metadata,
        "error": None,
        "status": 200,
        "reason": None,
    }


def inspect_urls(
    site_url: str,
    urls: list[str],
    *,
    token: str | None = None,
    transport: RequestTransport | None = None,
) -> dict[str, Any]:
    """Inspect a declared bounded sample; this is never an index census."""
    if (
        not site_url
        or not urls
        or len(urls) > MAX_INSPECTION_URLS
        or not all(
            isinstance(url, str)
            and url
            and len(url.encode("utf-8")) <= MAX_INSPECTION_URL_BYTES
            for url in urls
        )
    ):
        raise ValueError(f"site_url and 1..{MAX_INSPECTION_URLS} inspection URLs are required")
    bearer, token_error = _acquire_token(token)
    if bearer is None:
        return {
            "ok": False,
            "state": "not_configured",
            "verified": False,
            "requests_made": 0,
            "error": token_error,
        }
    request = transport or _request
    outcomes = []
    requests_made = 0
    for url in urls:
        try:
            requests_made += 1
            raw = request(
                "POST",
                f"{INSPECTION_HOST}/urlInspection/index:inspect",
                {"inspectionUrl": url, "siteUrl": site_url},
                bearer,
            )
            raw_bytes = raw.encode("utf-8") if isinstance(raw, str) else raw
            if not isinstance(raw_bytes, bytes) or len(raw_bytes) > MAX_RESPONSE_BYTES:
                raise ResponseTooLarge("Search Console inspection response exceeded the 8 MiB limit")
            body = _response_object(raw)
        except (
            urllib.error.HTTPError,
            urllib.error.URLError,
            TimeoutError,
            ResponseTooLarge,
            UnicodeError,
        ):
            outcomes.append({"url": url, "state": "failed", "error": "Search Console inspection request failed"})
            continue
        inspection = (body or {}).get("inspectionResult")
        if not isinstance(inspection, dict):
            outcomes.append(
                {"url": url, "state": "failed", "error": "Search Console malformed inspection response"}
            )
            continue
        index_status = inspection.get("indexStatusResult")
        verdict = index_status.get("verdict") if isinstance(index_status, dict) else None
        if not isinstance(index_status, dict) or verdict not in {"PASS", "FAIL", "NEUTRAL"}:
            outcomes.append(
                {"url": url, "state": "failed", "error": "Search Console malformed inspection response"}
            )
            continue
        index_fields = (
            "verdict",
            "coverageState",
            "indexingState",
            "robotsTxtState",
            "pageFetchState",
            "googleCanonical",
            "userCanonical",
            "lastCrawlTime",
            "crawledAs",
        )
        index_summary = {}
        for name in index_fields:
            value = index_status.get(name)
            if isinstance(value, str):
                limit = MAX_INSPECTION_URL_BYTES if name.endswith("Canonical") else 256
                index_summary[name] = value[:limit]
            elif name == "verdict" and value in {"PASS", "FAIL", "NEUTRAL"}:
                index_summary[name] = value
        amp = inspection.get("ampResult")
        amp_summary = None
        if isinstance(amp, dict):
            amp_summary = {
                name: amp[name][:256]
                for name in ("verdict", "ampIndexStatusVerdict", "pageFetchState", "indexingState")
                if name in amp and isinstance(amp[name], str)
            }
        rich = inspection.get("richResultsResult")
        rich_summary = None
        if isinstance(rich, dict):
            detected = rich.get("detectedItems")
            rich_summary = {
                "verdict": rich.get("verdict")[:64]
                if isinstance(rich.get("verdict"), str)
                else None,
                "detected_items": [
                    {
                        "type": item.get("richResultType")[:128]
                        if isinstance(item.get("richResultType"), str)
                        else None,
                        "items": len(item.get("items") or [])
                        if isinstance(item.get("items"), list)
                        else 0,
                    }
                    for item in detected[:50]
                    if isinstance(item, dict)
                ]
                if isinstance(detected, list)
                else [],
            }
        outcome = {"url": url, "state": "complete", "index_status": index_summary}
        if amp_summary is not None:
            outcome["amp_result"] = amp_summary
        if rich_summary is not None:
            outcome["rich_results_result"] = rich_summary
        outcomes.append(outcome)
    return {
        "ok": all(item["state"] == "complete" for item in outcomes),
        "state": "complete" if all(item["state"] == "complete" for item in outcomes) else "partial",
        "scope": "bounded URL diagnostic; not an index census",
        "requests_made": requests_made,
        "urls": outcomes,
    }


def sitemap_status(
    site_url: str, *, token: str | None = None, transport: RequestTransport | None = None
) -> dict[str, Any]:
    """Read the selected property's submitted sitemap status."""
    if not site_url:
        raise ValueError("site_url required")
    bearer, token_error = _acquire_token(token)
    if bearer is None:
        return {"ok": False, "state": "not_configured", "verified": False, "error": token_error}
    endpoint = f"{SEARCH_ANALYTICS_HOST}/sites/{urllib.parse.quote(site_url, safe='')}/sitemaps"
    try:
        body = _response_object((transport or _request)("GET", endpoint, None, bearer))
    except (urllib.error.HTTPError, urllib.error.URLError, TimeoutError, ResponseTooLarge) as exc:
        return {"ok": False, "state": "failed", "error": str(exc)}
    sitemaps = (body or {}).get("sitemap")
    if not isinstance(sitemaps, list) or not all(isinstance(item, dict) for item in sitemaps):
        return {"ok": False, "state": "failed", "error": "malformed GSC sitemap response"}
    return {"ok": True, "state": "complete", "sitemaps": sitemaps, "returned": len(sitemaps)}
