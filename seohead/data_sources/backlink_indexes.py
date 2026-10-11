"""Explicit, bounded paid-index reads with preflight units and shared spend receipts.

Ahrefs v3 and Majestic return backlink pairs. Moz's documented legacy v2 URL metrics
route returns per-URL authority/spam measurements; it does not enumerate backlinks.
Transport injection keeps all contracts testable without consuming an account's quota.
"""

from __future__ import annotations

import base64
import json
import math
import urllib.error
import urllib.parse
import urllib.request
from typing import Any

from seohead.data_sources import credentials, spend
from seohead.data_sources.http import open_no_redirect
from seohead.recon.net import normalize_domain

_AHREFS_FIELDS = (
    "url_from",
    "url_to",
    "anchor",
    "is_nofollow",
    "is_sponsored",
    "is_ugc",
    "is_lost",
    "first_seen_link",
    "last_visited",
    "last_seen",
    "domain_rating_source",
)
# Every selected field has the documented default weight of 1. None of the
# 5-unit refdomain/subnet or 10-unit traffic fields is requested.
_AHREFS_FIELD_UNITS = dict.fromkeys(_AHREFS_FIELDS, 1)
_ENDPOINTS = {
    "ahrefs": "https://api.ahrefs.com/v3/site-explorer/all-backlinks",
    "majestic": "https://api.majestic.com/api/json",
    "moz": "https://lsapi.seomoz.com/v2/url_metrics",
}


def estimate(provider: str, *, limit: int = 100) -> dict:
    """No network or credential access; estimates show units, not invented dollar prices."""
    if provider not in _ENDPOINTS or type(limit) is not int or not 1 <= limit <= 1000:
        raise ValueError("provider must be ahrefs/majestic/moz and limit must be 1..1000")
    if provider == "ahrefs":
        units = {"api_units": max(50, sum(_AHREFS_FIELD_UNITS.values()) * limit)}
        operation = "backlinks"
        reference = "https://docs.ahrefs.com/en/api/docs/limits-consumption"
    elif provider == "majestic":
        units = {"analysis_units": 5000, "retrieval_units": limit}
        operation = "backlinks"
        reference = "https://developer-support.majestic.com/api/commands/get-back-link-data.shtml"
    else:
        units = None
        operation = "url_metrics_v2"
        reference = "https://moz.com/help/links-api/making-calls/url-metrics"
    return {
        "ok": True,
        "provider": provider,
        "operation": operation,
        "max_rows": 1 if provider == "moz" else limit,
        "estimated_units": units,
        "currency_cost": None,
        "price_state": "account_plan_dependent",
        "price_reference": reference,
        "requires_confirmation": True,
        "legacy": provider == "moz",
        "field_units": _AHREFS_FIELD_UNITS.copy() if provider == "ahrefs" else None,
    }


def _transport(request: urllib.request.Request) -> tuple[str, dict]:
    with open_no_redirect(request, timeout=30) as response:
        return response.read().decode(), dict(response.headers)


def collect(
    provider: str,
    target: str,
    *,
    limit: int = 100,
    offset: int = 0,
    confirm_paid: bool = False,
    max_units: float | None = None,
    api_key: str | None = None,
    access_id: str | None = None,
    secret_key: str | None = None,
    transport=None,
) -> dict[str, Any]:
    """Issue one paid request only after confirmation; never auto-paginate.

    ``max_units`` caps every estimated currency independently (Ahrefs API units,
    Majestic analysis and retrieval units). Moz v2 units are account-dependent,
    so a ceiling refuses its unpriced call.
    """
    preflight = estimate(provider, limit=limit)
    if not normalize_domain(target) or type(offset) is not int or offset < 0:
        raise ValueError("a valid target and nonnegative offset are required")
    if provider != "majestic" and offset:
        raise ValueError("offset is supported only by Majestic")
    units = preflight["estimated_units"]
    if max_units is not None:
        if isinstance(max_units, bool) or not math.isfinite(max_units) or max_units < 0:
            raise ValueError("max_units must be finite and nonnegative")
        if units is None or any(value > max_units for value in units.values()):
            return {"ok": False, "state": "cost_limit", "estimate": preflight}
    if confirm_paid is not True:
        return {"ok": False, "state": "confirmation_required", "estimate": preflight}
    headers = {"Accept": "application/json"}
    payload = None
    url = _ENDPOINTS[provider]
    try:
        if provider == "ahrefs":
            key = api_key or credentials.read("ahrefs/api_key", "AHREFS_API_KEY")
            headers["Authorization"] = "Bearer " + key
            query = {
                "target": target,
                "mode": "subdomains",
                "limit": limit,
                "aggregation": "all",
                "history": "all_time",
                "output": "json",
                "select": ",".join(_AHREFS_FIELDS),
            }
            url += "?" + urllib.parse.urlencode(query)
        elif provider == "majestic":
            key = api_key or credentials.read("majestic/api_key", "MAJESTIC_API_KEY")
            payload = urllib.parse.urlencode(
                {
                    "app_api_key": key,
                    "cmd": "GetBackLinkData",
                    "item": target,
                    "Count": limit,
                    "From": offset,
                    "datasource": "fresh",
                    "Mode": 0,
                }
            ).encode()
            headers["Content-Type"] = "application/x-www-form-urlencoded"
        else:
            identifier = access_id or credentials.read("moz/access_id", "MOZ_ACCESS_ID")
            secret = secret_key or credentials.read("moz/secret_key", "MOZ_SECRET_KEY")
            encoded = base64.b64encode(f"{identifier}:{secret}".encode()).decode()
            headers["Authorization"] = "Basic " + encoded
            payload = json.dumps({"targets": [target]}).encode()
            headers["Content-Type"] = "application/json"
    except credentials.MissingCredential as exc:
        return {"ok": False, "state": "not_configured", "estimate": preflight, "error": str(exc)}
    request = urllib.request.Request(
        url, data=payload, headers=headers, method="POST" if payload else "GET"
    )
    try:
        response = (transport or _transport)(request)
    except (urllib.error.URLError, TimeoutError, OSError):
        spend.record(
            provider,
            preflight["operation"],
            unit="unknown",
            extra={"charge_uncertain": True, "response_received": False},
        )
        return {
            "ok": False,
            "state": "failed",
            "error": "backlink index request failed",
            "estimate": preflight,
        }
    raw, response_headers = response if isinstance(response, tuple) else (response, {})
    receipt_headers = {str(key).lower(): value for key, value in response_headers.items()}
    charge = receipt_headers.get("x-api-units-cost-total-actual") if provider == "ahrefs" else None
    try:
        charge = float(charge)
        if not math.isfinite(charge) or charge < 0:
            charge = None
    except (TypeError, ValueError):
        charge = None
    # Write before parsing: even unusable/malformed paid responses have a receipt.
    receipt = spend.record(
        provider,
        preflight["operation"],
        cost=charge or 0.0,
        unit="api_units" if provider == "ahrefs" else "unknown",
        extra={"cost_unknown": charge is None, "response_received": True, "estimated_units": units},
    )
    try:
        body = json.loads(raw)
        if not isinstance(body, dict):
            raise ValueError("malformed response")
        if provider == "ahrefs":
            rows = body.get("backlinks")
        elif provider == "majestic":
            if body.get("Code") != "OK":
                raise ValueError("provider returned an error")
            rows = body.get("DataTables", {}).get("BackLinks", {}).get("Data")
        else:
            rows = body.get("results")
        if (
            not isinstance(rows, list)
            or not all(isinstance(row, dict) for row in rows)
            or len(rows) > preflight["max_rows"]
        ):
            raise ValueError("malformed response rows")
    except (ValueError, TypeError, AttributeError):
        return {
            "ok": False,
            "state": "failed",
            "error": "malformed backlink index response",
            "estimate": preflight,
            "receipt": receipt,
        }
    return {
        "ok": True,
        "state": "partial",
        "provider": provider,
        "operation": preflight["operation"],
        "rows": rows if provider != "moz" else None,
        "data": body,
        "url_metrics": rows if provider == "moz" else None,
        "returned": len(rows),
        "truncated": len(rows) >= preflight["max_rows"],
        "scope": "legacy v2 per-URL metrics" if provider == "moz" else "bounded index sample",
        "estimate": preflight,
        "receipt": receipt,
        "read_only": True,
    }
