"""Sitemap-declared URLs that the native crawl itself observed as unfit for a sitemap.

Sibling to ``reconcile.py`` and ``link_findings.py``: pure, stdlib plus the crawl's own
URL and robots helpers, and never imports ``seohead.sf``. The caller hands in the sitemap's
declared URLs, the crawl's ``PageRecord`` list and the robots-blocked set; each declared URL
the crawl fetched is checked against the same predicates the rest of the toolkit uses for
"indexable", so a sitemap entry is never judged by a second, looser notion of the page.

A declared URL the crawl never fetched is not reported: it is evidence of nothing, and the
reachability side of the sitemap question is already answered by ``reconcile_sitemap``.
"""

from __future__ import annotations

from collections.abc import Iterable
from typing import Any
from urllib.parse import urljoin, urlsplit

from seohead.checks.parser import robots_directives
from seohead.checks.sitemap import normalize_url

__all__ = ["sitemap_url_problems"]


def _key(url: str) -> str | None:
    try:
        return normalize_url(url)
    except ValueError:
        return None


def _non_canonical(page_url: str, canonical: str, key: str) -> bool:
    if not canonical:
        return False
    target = _key(urljoin(page_url, canonical))
    return target is not None and target != key


def sitemap_url_problems(
    declared: Iterable[str],
    pages: Iterable[Any],
    robots_blocked: Iterable[str],
    host: str,
) -> list[dict[str, Any]]:
    """Findings for declared URLs on ``host`` that the crawl fetched and found unfit.

    Each item carries ``check`` (one of ``SITEMAP_URL_3XX``, ``SITEMAP_URL_4XX_5XX``,
    ``SITEMAP_URL_NON_INDEXABLE``), the declared ``target_url``, the observed status code
    and the reasons. A URL can produce more than one item, one per check it fails.
    """
    host = host.lower()
    by_key: dict[str, Any] = {}
    for page in pages:
        key = _key(page.url)
        if key is not None:
            by_key.setdefault(key, page)
    blocked = {k for k in (_key(u) for u in robots_blocked) if k is not None}

    out: list[dict[str, Any]] = []
    seen: set[str] = set()
    for url in declared:
        key = _key(url)
        if key is None or key in seen:
            continue
        seen.add(key)
        if host and (urlsplit(url).hostname or "").lower() != host:
            continue
        page = by_key.get(key)
        if page is None:
            continue  # never fetched: nothing observed to report

        status = getattr(page, "status_code", None)
        redirect = str(getattr(page, "redirect_url", "") or "")
        if (status is not None and 300 <= int(status) <= 399) or redirect:
            out.append(
                {
                    "check": "SITEMAP_URL_3XX",
                    "target_url": url,
                    "status_code": status,
                    "reasons": ["redirect"],
                }
            )
        elif status is not None and 400 <= int(status) <= 599:
            out.append(
                {
                    "check": "SITEMAP_URL_4XX_5XX",
                    "target_url": url,
                    "status_code": status,
                    "reasons": [f"status_{int(status)}"],
                }
            )

        reasons = []
        if "noindex" in robots_directives(page.meta_robots, page.x_robots):
            reasons.append("noindex")
        if key in blocked:
            reasons.append("robots_blocked")
        if _non_canonical(page.url, getattr(page, "canonical", ""), key):
            reasons.append("non_canonical")
        if reasons:
            out.append(
                {
                    "check": "SITEMAP_URL_NON_INDEXABLE",
                    "target_url": url,
                    "status_code": status,
                    "reasons": reasons,
                }
            )
    return out
