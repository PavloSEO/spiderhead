"""Missing security headers on HTML pages, read from a stored crawl's own response evidence (#1013).

Sibling to ``link_findings.py``: the predicate is pure over header pairs, and the only database
access is one read-only query over ``pages`` and ``responses``. A header is judged on the
final response of an HTML page that answered 2xx. Other media types are not judged, because a
missing Content-Security-Policy on a PDF or an image is not a defect of the page that links it.

Presence is what is checked, matching how Screaming Frog words these issues ("Missing ... Header").
A header sent more than once on one response is reported as DUPLICATE_SECURITY_HEADER.
Header values are not validated. X-Frame-Options is satisfied by a CSP ``frame-ancestors``
directive, the same rule ``seohead.recon.security`` applies to a live response.

The redirect-hop HSTS probe (#1414) is the module's live exception: an opt-in walk of
the start URL's host variants, because a redirecting host is never part of the stored
crawl. It records observations; ``evaluate_redirect_hops`` judges them.
"""

from __future__ import annotations

import json
import sqlite3
from typing import Any

# check id -> the header whose absence it reports. The registry carries the message and fix.
HEADER_CHECKS: dict[str, str] = {
    "MISSING_HSTS": "strict-transport-security",
    "MISSING_CSP": "content-security-policy",
    "MISSING_X_CONTENT_TYPE_OPTIONS": "x-content-type-options",
    "MISSING_X_FRAME_OPTIONS": "x-frame-options",
    "MISSING_REFERRER_POLICY": "referrer-policy",
}

# A security header sent more than once on one response. Kept apart from HEADER_CHECKS, which
# reports absence; every check id the evaluation returns is listed in CHECK_IDS.
DUPLICATE_CHECK = "DUPLICATE_SECURITY_HEADER"
CHECK_IDS: tuple[str, ...] = (*HEADER_CHECKS, DUPLICATE_CHECK)

# A redirect hop between the host variants of the start URL answered without HSTS (#1414).
# Sibling of MISSING_HSTS, which judges only the final response of a crawled page: a
# redirecting host (www to apex, http to https) is never fetched by the crawl, so the
# header on that hop is invisible to ``evaluate`` and only the opt-in probe can see it.
REDIRECT_HOP_CHECK = "MISSING_HSTS_ON_REDIRECT_HOP"

# Retained probe payload (context_items kind / version), re-read by offline reanalysis.
REDIRECT_HOP_PROBE_KIND = "redirect_hop_probe"
REDIRECT_HOP_PROBE_VERSION = "redirect_hop_probe.v1"

# A redirect loop between host variants is a mirror problem, not an HSTS one; the hops
# collected before the cap are judged, the walk just stops there.
_PROBE_MAX_HOPS = 6

# Content-Security-Policy is left out on purpose: a site may send several policies, and the browser
# enforces each one, so a repeated CSP is not a defect.
_SECURITY_HEADERS = (
    "strict-transport-security",
    "x-content-type-options",
    "x-frame-options",
    "referrer-policy",
    "permissions-policy",
)

_HTML_TYPES = frozenset({"text/html", "application/xhtml+xml"})

# The final page response, not every request: a redirected or re-fetched URL can own several
# response rows, and the newest one is the response the page row was built from. Its effective
# headers are the ones that belong to the final document, not to a redirect hop.
_PAGE_RESPONSES_SQL = (
    "SELECT u.url, p.status_code, p.content_type, r.effective_headers_redacted_json "
    "FROM pages AS p JOIN urls AS u USING(url_id) "
    "LEFT JOIN responses AS r ON r.response_id = ("
    "  SELECT MAX(response_id) FROM responses "
    "  WHERE effective_url_id = p.url_id AND purpose = 'page'"
    ") ORDER BY p.page_ordinal"
)


def _header_pairs(raw: Any) -> list[tuple[str, str]] | None:
    """The stored (name, value) pairs in wire order, or None when they do not parse."""
    try:
        pairs = json.loads(raw)
    except (TypeError, ValueError):
        return None
    if not isinstance(pairs, list):
        return None
    out: list[tuple[str, str]] = []
    for pair in pairs:
        if not (
            isinstance(pair, list)
            and len(pair) == 2
            and isinstance(pair[0], str)
            and isinstance(pair[1], str)
        ):
            return None
        out.append((pair[0].lower(), pair[1]))
    return out


def _header_map(pairs: list[tuple[str, str]]) -> dict[str, str]:
    """Lower-cased header name -> combined value; repeated names are joined, not overwritten."""
    headers: dict[str, str] = {}
    for name, value in pairs:
        headers[name] = f"{headers[name]}, {value}" if name in headers else value
    return headers


def _repeated(pairs: list[tuple[str, str]]) -> list[str]:
    """The security headers that appear more than once on the same response."""
    counts: dict[str, int] = {}
    for name, _value in pairs:
        counts[name] = counts.get(name, 0) + 1
    return [header for header in _SECURITY_HEADERS if counts.get(header, 0) > 1]


def _is_judged_html(status_code: Any, content_type: str) -> bool:
    media = (content_type or "").split(";", 1)[0].strip().lower()
    return media in _HTML_TYPES and isinstance(status_code, int) and 200 <= status_code <= 299


def _missing(check_id: str, headers: dict[str, str]) -> bool:
    header = HEADER_CHECKS[check_id]
    if header in headers and headers[header].strip():
        return False
    if header == "x-frame-options":
        return "frame-ancestors" not in headers.get("content-security-policy", "").lower()
    return True


def evaluate_page(
    url: str, status_code: Any, content_type: str, raw_headers: Any
) -> dict[str, Any]:
    """Analyze one retained page response without a request to the site."""
    if not _is_judged_html(status_code, content_type):
        return {
            "state": "not_applicable",
            "reason": "security headers are evaluated on successful HTML page responses",
            "headers": None,
            "findings": [],
        }
    pairs = _header_pairs(raw_headers)
    if pairs is None:
        return {
            "state": "unavailable",
            "reason": "response headers were not retained or could not be parsed",
            "headers": None,
            "findings": [],
        }
    headers = _header_map(pairs)
    findings = [
        {"check": check_id, "target_url": url, "header": HEADER_CHECKS[check_id]}
        for check_id in HEADER_CHECKS
        if _missing(check_id, headers)
    ]
    findings.extend(
        {"check": DUPLICATE_CHECK, "target_url": url, "header": header}
        for header in _repeated(pairs)
    )
    return {"state": "measured", "reason": "", "headers": headers, "findings": findings}


def evaluate(con: sqlite3.Connection) -> dict[str, Any]:
    """Judge every stored HTML page for the native header checks, HSTS included.

    Returns the URLs each check fires on (in crawl order), how many judged pages had no parseable
    response headers, and how many judged pages were measured. A page that was not measured is
    never counted as clean. Read-only and deterministic.
    """
    if not isinstance(con, sqlite3.Connection):
        raise TypeError("security header evaluation requires a SQLite scan connection")
    findings: dict[str, list[dict[str, Any]]] = {check_id: [] for check_id in CHECK_IDS}
    measured = 0
    unmeasured = 0
    for url, status_code, content_type, raw_headers in con.execute(_PAGE_RESPONSES_SQL):
        page = evaluate_page(url, status_code, content_type, raw_headers)
        if page["state"] == "not_applicable":
            continue
        if page["state"] == "unavailable":
            unmeasured += 1
            continue
        measured += 1
        for finding in page["findings"]:
            findings[finding["check"]].append(
                {"target_url": finding["target_url"], "header": finding["header"]}
            )
    return {"findings": findings, "pages_measured": measured, "pages_unmeasured": unmeasured}


# --------------------------------------------------------------- redirect hops


def probe_redirect_hops(url: str, timeout: float = 10.0) -> dict[str, Any]:
    """Fetch the host variants of the start URL hop by hop, recording status and HSTS (#1414).

    Live and opt-in (``analysis.redirect_hop_hsts_probe``). Walks the four origin
    variants of ``seohead.recon.mirrors.build_variants`` -- https/http times
    bare/www -- without following redirects, so every hop response is seen
    individually. Records observations only; ``evaluate_redirect_hops`` judges
    them, and an unreachable variant is data, not a defect.
    """
    import urllib.parse

    from ..recon.mirrors import build_variants
    from ..recon.net import UA, http_client

    variants = [v for v in build_variants(url) if v["group"] == "origin"]
    out: list[dict[str, Any]] = []
    client, _http2 = http_client(
        timeout, headers={"User-Agent": UA}, follow_redirects=False, verify=True
    )
    with client:
        for variant in variants:
            hops: list[dict[str, Any]] = []
            error = ""
            reachable = False
            status = None
            final_url = None
            current = variant["url"]
            for _ in range(_PROBE_MAX_HOPS):
                try:
                    resp = client.get(current)
                except Exception as exc:  # Network failures are result data.
                    error = str(exc)[:200]
                    break
                status = resp.status_code
                location = resp.headers.get("location")
                hsts = resp.headers.get("strict-transport-security", "")
                if status in (301, 302, 303, 307, 308) and location:
                    nxt = urllib.parse.urljoin(current, location)
                    hops.append({"url": current, "status": status, "location": nxt, "hsts": hsts})
                    current = nxt
                    continue
                reachable = True
                final_url = current
                break
            else:
                # Hop cap reached without a terminus: a suspected loop. The hops
                # collected are real answers and stay judgeable evidence.
                reachable = True
            out.append(
                {
                    "variant": variant["variant"],
                    "url": variant["url"],
                    "reachable": reachable,
                    "error": error,
                    "hops": hops,
                    "status": status,
                    "final_url": final_url,
                }
            )
    return {"url": url, "variants": out}


def evaluate_redirect_hops(probe: dict[str, Any]) -> dict[str, Any]:
    """Judge probe observations with a pure function that requires no network.

    A redirect response without a non-empty ``strict-transport-security`` header
    is the defect; a final 2xx without the header is MISSING_HSTS's business,
    not this check's. Returns how many redirect hops were measured and the hops
    missing the header, or ``no_redirect_hops`` when no variant answered with a
    redirect at all -- a state with nothing to judge.
    """
    missing: list[dict[str, Any]] = []
    hops_measured = 0
    for variant in probe.get("variants") or []:
        for hop in variant.get("hops") or []:
            hops_measured += 1
            if not str(hop.get("hsts", "")).strip():
                missing.append({"variant": variant["variant"], **hop})
    if not hops_measured:
        return {"state": "no_redirect_hops", "hops_measured": 0, "missing": []}
    return {"state": "measured", "hops_measured": hops_measured, "missing": missing}


def validate_redirect_hop_probe(item: dict[str, Any], payload: Any) -> None:
    """Reject a retained probe row the audit could not have written (#1414)."""
    from seohead.storage import ScanError

    def _hop_ok(hop: Any) -> bool:
        return (
            isinstance(hop, dict)
            and set(hop) == {"url", "status", "location", "hsts"}
            and isinstance(hop["url"], str)
            and bool(hop["url"])
            and type(hop["status"]) is int
            and hop["status"] in (301, 302, 303, 307, 308)
            and isinstance(hop["location"], str)
            and bool(hop["location"])
            and isinstance(hop["hsts"], str)
        )

    def _variant_ok(variant: Any) -> bool:
        return (
            isinstance(variant, dict)
            and set(variant)
            == {"variant", "url", "reachable", "error", "hops", "status", "final_url"}
            and isinstance(variant["variant"], str)
            and isinstance(variant["url"], str)
            and bool(variant["url"])
            and type(variant["reachable"]) is bool
            and isinstance(variant["error"], str)
            and isinstance(variant["hops"], list)
            and all(_hop_ok(hop) for hop in variant["hops"])
            and (variant["status"] is None or type(variant["status"]) is int)
            and (variant["final_url"] is None or isinstance(variant["final_url"], str))
        )

    if (
        not isinstance(payload, dict)
        or set(payload) != {"schema_version", "url", "variants"}
        or payload["schema_version"] != REDIRECT_HOP_PROBE_VERSION
        or not isinstance(payload["url"], str)
        or not payload["url"]
        or not isinstance(payload["variants"], list)
        or not payload["variants"]
        or not all(_variant_ok(variant) for variant in payload["variants"])
        or item["item_key"] != "run"
        or item["completeness"] != "complete"
        or item["reason"]
    ):
        raise ScanError("native redirect-hop probe context is invalid")
