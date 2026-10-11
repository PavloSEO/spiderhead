"""robots.txt fetcher + analyzer.

Fetches a site's robots.txt, parses user-agent groups (Allow/Disallow), lists the
declared sitemaps, and can test whether specific paths are crawlable for a UA.
"""

from __future__ import annotations

import contextlib
import math
import re
from collections.abc import Callable
from typing import Any, cast
from urllib.parse import urlparse, urlsplit

from seohead.core.models import ParsedRobots, RobotsCheckResult, RobotsGroup
from seohead.recon.net import ProxyRoute, crawl_transport_options, http_client

_REQUEST_RATE = re.compile(r"^([1-9][0-9]*)/([1-9][0-9]*)$")


def _robots_url(url: str) -> str:
    p = urlparse(url if "://" in url else "https://" + url)
    return f"{p.scheme}://{p.netloc}/robots.txt"


def parse_robots(text: str) -> ParsedRobots:
    """Pure parse of robots.txt content into groups + sitemaps (no network)."""
    groups: list[dict[str, Any]] = []
    sitemaps: list[str] = []
    current: dict[str, Any] | None = None
    # Crawl-delay was parsed by nobody, so a site asking to be crawled slowly was
    # crawled at whatever rate the operator chose.
    for raw in text.splitlines():
        line = raw.split("#", 1)[0].strip()
        if not line or ":" not in line:
            continue
        field, _, value = line.partition(":")
        field = field.strip().lower()
        value = value.strip()
        if field == "user-agent":
            if current is None or current.get("_has_rules"):
                current = {
                    "user_agents": [],
                    "allow": [],
                    "disallow": [],
                    "crawl_delay": None,
                    "request_rate_delay": None,
                    "_has_rules": False,
                }
                groups.append(current)
            current["user_agents"].append(value)
        elif field in ("allow", "disallow") and current is not None:
            current[field].append(value)
            current["_has_rules"] = True
        elif field == "crawl-delay" and current is not None:
            # A malformed delay is no delay, not a crash.
            current["_has_rules"] = True
            with contextlib.suppress(ValueError):
                delay = float(value.replace(",", "."))
                if math.isfinite(delay) and delay >= 0:
                    current["crawl_delay"] = delay
        elif field == "request-rate" and current is not None:
            current["_has_rules"] = True
            # The non-standard directive has one broadly used, unambiguous
            # shape: positive whole requests over positive whole seconds.  Do
            # not turn a malformed value into a rate by guessing at decimal,
            # locale, zero, or whitespace semantics.
            match = _REQUEST_RATE.fullmatch(value.strip())
            if match is not None:
                requests, seconds = (int(part) for part in match.groups())
                current["request_rate_delay"] = seconds / requests
        elif field == "sitemap":
            sitemaps.append(value)
    for g in groups:
        g.pop("_has_rules", None)
    # Built with a temporary "_has_rules" bookkeeping key above (popped by now),
    # so a plain dict is the natural builder; cast once at the boundary.
    return cast(ParsedRobots, {"groups": groups, "sitemaps": sitemaps})


EMPTY_GROUP: RobotsGroup = {
    "user_agents": [],
    "allow": [],
    "disallow": [],
    "crawl_delay": None,
    "request_rate_delay": None,
}


def _rules_for(parsed: ParsedRobots, user_agent: str) -> RobotsGroup:
    """The combined rules of every group at the longest matching product token.

    RFC 9309 selects the most specific match, but a robots.txt file can address the
    same agent from more than one group (a site adding a second ``Disallow`` block
    for a crawler it already named earlier), and every group tied at that
    specificity applies together -- see RFC 9309 section 2.2.1's "combine". Keeping
    only the first such group silently dropped every later rule for that crawler.
    Taking the *last* match instead meant file order decided the outcome, and
    matching by substring meant a group naming a browser engine captured any
    agent whose string happened to contain it.
    """
    from seohead.crawl.user_agents import token_aliases  # seohead.crawl imports this module

    ua = user_agent.lower()
    # The default crawler still answers to its pre-rename name, as an exact match.
    aliases = token_aliases(ua) - {ua}
    best_length = -1
    matches: list[RobotsGroup] = []
    star_matches: list[RobotsGroup] = []
    for group in parsed["groups"]:
        group_best = -1
        has_star = False
        # A blank value -- a bare "User-agent:", or a name that an inline comment
        # swallowed -- names no crawler at all. Left in, it was a zero-length
        # prefix of every agent, so it matched all of them and, counting as a
        # named match, outranked the file's real "*" group for every crawler the
        # file never mentions (#566). The group itself still exists and keeps its
        # own directives; it simply names nobody.
        for token in (t for t in (u.lower().strip() for u in group["user_agents"]) if t):
            if token == "*":
                has_star = True
                continue
            # A token matches when the agent name starts with it; "Googlebot"
            # applies to "Googlebot-Image", but not the other way round.
            if (ua == token or ua.startswith(token)) and len(token) > group_best:
                group_best = len(token)
            elif token in aliases and len(ua) > group_best:
                group_best = len(ua)
        if group_best > best_length:
            best_length, matches = group_best, [group]
        elif group_best >= 0 and group_best == best_length:
            matches.append(group)
        if has_star:
            star_matches.append(group)
    # A wildcard group only applies when nothing named this agent specifically.
    selected = matches if best_length >= 0 else star_matches
    if not selected:
        return cast(RobotsGroup, dict(EMPTY_GROUP))
    return {
        "user_agents": [ua for g in selected for ua in g["user_agents"]],
        "allow": [pattern for g in selected for pattern in g["allow"]],
        "disallow": [pattern for g in selected for pattern in g["disallow"]],
        # Matching groups combine.  Unlike Allow/Disallow, these non-standard
        # directives describe a floor, so preserve the strictest value rather
        # than letting file order choose a faster request rate.
        "crawl_delay": max(
            (float(g["crawl_delay"]) for g in selected if g["crawl_delay"] is not None),
            default=None,
        ),
        "request_rate_delay": max(
            (float(g.get("request_rate_delay") or 0) for g in selected), default=0
        )
        or None,
    }


def crawl_delay(parsed: ParsedRobots, user_agent: str = "*") -> float | None:
    """The delay the site asks this agent to keep, if it states one."""
    return _rules_for(parsed, user_agent).get("crawl_delay")


def request_rate_delay(parsed: ParsedRobots, user_agent: str = "*") -> float | None:
    """Return the minimum interval implied by a valid ``Request-rate`` directive."""
    return _rules_for(parsed, user_agent).get("request_rate_delay")


def politeness_delay(parsed: ParsedRobots, user_agent: str = "*") -> float | None:
    """Return the strictest applicable robots-derived interval.

    ``crawl_delay_applied`` in crawl results and scan runtime stores this
    effective interval, while the parsed robots context retains each source
    directive separately.
    """
    values = [
        value
        for value in (crawl_delay(parsed, user_agent), request_rate_delay(parsed, user_agent))
        if value is not None
    ]
    return max(values) if values else None


def _pattern_to_regex(pattern: str) -> re.Pattern[str]:
    """Google robots pattern -> regex: ``*`` is any sequence, trailing ``$`` anchors end."""
    anchored = pattern.endswith("$")
    body = pattern[:-1] if anchored else pattern
    regex = re.escape(body).replace(r"\*", ".*")
    return re.compile("^" + regex + ("$" if anchored else ""))


def match_path(url: str) -> str:
    """The part of a URL that robots.txt patterns are matched against.

    Path *and* query: a rule like ``Disallow: /*?`` exists to block query
    strings, so comparing it against the path alone can never match.
    """
    parts = urlsplit(url)
    return (parts.path or "/") + (f"?{parts.query}" if parts.query else "")


def is_allowed(parsed: ParsedRobots, path: str, user_agent: str = "*") -> bool:
    """Allow/Disallow decision (Google precedence: longest matching pattern wins;
    Allow wins ties). Handles ``*`` wildcards and the ``$`` end-anchor.

    ``path`` is the value ``match_path`` returns, query string included."""
    rules = _rules_for(parsed, user_agent)
    best_len, decision = -1, True
    for patterns, allow in ((rules["disallow"], False), (rules["allow"], True)):
        for pattern in patterns:
            if pattern == "":
                continue
            if _pattern_to_regex(pattern).match(path):
                plen = len(pattern.rstrip("$"))
                if plen > best_len or (plen == best_len and allow):
                    best_len = plen
                    decision = allow
    return decision


def check_robots(
    url: str,
    user_agent: str = "*",
    paths: list[str] | None = None,
    timeout: float = 20.0,
    *,
    request_gate: Callable[[], None] | None = None,
    proxy_route: ProxyRoute | None = None,
) -> RobotsCheckResult:
    robots_url = _robots_url(url)
    try:
        options: dict[str, Any] = {"follow_redirects": True}
        if request_gate is not None:
            options["event_hooks"] = {"request": [lambda _request: request_gate()]}
        client, _http2_capable = http_client(
            timeout, **crawl_transport_options(proxy_route), **options
        )
        with client:
            resp = client.get(robots_url)
    except Exception as exc:
        return {"ok": False, "robots_url": robots_url, "error": str(exc)}
    if resp.status_code == 429 or resp.status_code >= 500:
        return {
            "ok": False,
            "robots_url": robots_url,
            "status_code": resp.status_code,
            "error": f"robots.txt returned {resp.status_code}; rules could not be read",
        }
    if resp.status_code >= 400:
        return {
            "ok": True,
            "robots_url": robots_url,
            "status_code": resp.status_code,
            "exists": False,
            "groups": [],
            "sitemaps": [],
            "note": "no robots.txt (crawl allowed)",
        }
    parsed = parse_robots(resp.text)
    result: dict[str, Any] = {
        "ok": True,
        "robots_url": robots_url,
        "status_code": resp.status_code,
        "exists": True,
        "groups": parsed["groups"],
        "sitemaps": parsed["sitemaps"],
    }
    if paths:
        result["path_checks"] = [
            {"path": p, "allowed": is_allowed(parsed, p, user_agent)} for p in paths
        ]
    # Built imperatively above (path_checks is added only when requested), so a
    # plain dict is the natural builder; cast once at the boundary.
    return cast(RobotsCheckResult, result)


if __name__ == "__main__":
    sample = "User-agent: *\nDisallow: /api/\nDisallow: /*?\nAllow: /api/public\n\nSitemap: https://x/sitemap.xml"
    parsed = parse_robots(sample)
    assert parsed["sitemaps"] == ["https://x/sitemap.xml"]
    assert is_allowed(parsed, "/api/public/x") is True  # Allow /api/public beats Disallow /api/
    assert is_allowed(parsed, "/api/private") is False
    assert is_allowed(parsed, "/blog") is True
    assert is_allowed(parsed, "/blog?page=2") is False  # matches wildcard Disallow: /*?
    print("OK: robots self-check passed")
