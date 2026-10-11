"""Verify whether links from a supplied donor list are live and pass signals.

Finding an ``<a href>`` is not sufficient. A link may not pass ranking signals
when it has ``rel=nofollow`` (or ``ugc``/``sponsored``), when the entire donor
page is excluded by a ``noindex`` directive (meta robots or the
``X-Robots-Tag`` response header), when page-level ``nofollow`` applies to
every link, or when the page is canonicalized elsewhere. This module checks
those conditions separately so a report explains why a link is not effective
instead of merely saying it is missing.

This is deliberately not an external backlink index such as Ahrefs or Majestic.
It verifies a known donor list rather than discovering another site's profile.
"""

from __future__ import annotations

import re
from concurrent.futures import ThreadPoolExecutor
from typing import Any
from urllib.parse import urljoin, urlsplit

from seohead.checks.parser import document_base_url, robots_directives, robots_meta_scoped
from seohead.recon.net import UA, http_client, normalize_domain, normalize_url
from seohead.sf.core.normalize import norm_url

MAX_DONORS = 500
_NO_WEIGHT_RELS = ("nofollow", "ugc", "sponsored")


def _same_site(href: str, target_domain: str) -> bool:
    host = normalize_domain(urlsplit(href).netloc)
    return bool(host) and (host == target_domain or host.endswith("." + target_domain))


def _inspect_donor(
    donor: str, target_domain: str, target_url: str | None, client
) -> dict[str, Any]:
    from bs4 import BeautifulSoup

    from seohead.checks.parser import sponsorship_disclosure

    record: dict[str, Any] = {"donor": donor, "found": False, "links": []}
    try:
        resp = client.get(donor)
    except Exception as exc:  # Network failures are reported per donor.
        record["error"] = str(exc)
        record["reason"] = "donor page is unavailable"
        return record

    record["status_code"] = resp.status_code
    record["final_url"] = str(resp.url)
    if resp.status_code >= 400:
        record["reason"] = f"donor page returned HTTP {resp.status_code}"
        return record

    soup = BeautifulSoup(resp.text, "html.parser")
    x_robots_tag = str(resp.headers.get("X-Robots-Tag", ""))
    directives = robots_directives(*robots_meta_scoped(soup), x_robots_tag)
    record["donor_indexable"] = "noindex" not in directives
    page_nofollow = "nofollow" in directives

    canonical = soup.find(
        "link",
        rel=lambda v: v and "canonical" in [s.lower() for s in (v if isinstance(v, list) else [v])],
    )
    resolve_from = document_base_url(soup, str(resp.url))
    canonical_href = urljoin(resolve_from, str(canonical.get("href"))) if canonical else None
    record["canonical"] = canonical_href
    record["canonical_elsewhere"] = bool(
        canonical_href and norm_url(canonical_href) != norm_url(str(resp.url))
    )

    for tag in soup.find_all("a", href=True):
        href = urljoin(resolve_from, str(tag["href"]).strip())
        if not _same_site(href, target_domain):
            continue
        if target_url and href.rstrip("/") != target_url.rstrip("/"):
            continue
        rels = [r.lower() for r in (tag.get("rel") or [])]
        rels = sorted(set(rels))
        blocking = [r for r in rels if r in _NO_WEIGHT_RELS]
        record["links"].append(
            {
                "href": href,
                "anchor": re.sub(r"\s+", " ", tag.get_text(" ", strip=True))[:200],
                "rel": rels,
                "nofollow": "nofollow" in rels,
                "ugc": "ugc" in rels,
                "sponsored": "sponsored" in rels,
                "mixed_rel": len(set(rels) & set(_NO_WEIGHT_RELS)) > 1,
                "disclosure": sponsorship_disclosure(tag) if "sponsored" in rels else None,
                "follow": not blocking and not page_nofollow,
                "blocked_by": blocking or (["page robots nofollow"] if page_nofollow else []),
            }
        )

    record["found"] = bool(record["links"])
    if not record["found"]:
        record["reason"] = "no link to the target domain was found on the page"
    elif not record["donor_indexable"]:
        record["reason"] = "the link exists, but the donor page is not indexable"
    elif not any(link["follow"] for link in record["links"]):
        record["reason"] = "the link exists, but is marked nofollow, ugc, or sponsored"
    return record


def check_backlinks(
    target: str, donors: list[str], *, concurrency: int = 3, timeout: float = 20.0
) -> dict[str, Any]:
    """Check a donor-page list for links to *target*.

    *target* may be a domain, in which case links to it or its subdomains match,
    or a specific URL, in which case the normalized address must match exactly.
    """
    target_domain = normalize_domain(target)
    if not target_domain:
        return {"ok": False, "error": f"not a valid domain or URL: {target!r}"}
    # A path means the caller wants this exact page, not the entire domain.
    parsed = urlsplit(normalize_url(target) or "")
    target_url = normalize_url(target) if parsed.path not in ("", "/") else None

    urls = [u for u in (normalize_url(d) for d in donors or []) if u][:MAX_DONORS]
    if not urls:
        return {"ok": False, "error": "a non-empty list of donor page URLs is required"}
    dropped = len(donors or []) - len(urls)

    try:
        import httpx
    except ImportError:
        return {"ok": False, "error": "httpx is required"}

    concurrency = max(1, min(int(concurrency), 10))
    limits = httpx.Limits(max_connections=concurrency)
    client, _http2_capable = http_client(
        timeout, follow_redirects=True, limits=limits, headers={"User-Agent": UA}
    )
    with client, ThreadPoolExecutor(max_workers=concurrency) as pool:
        results = list(
            pool.map(lambda d: _inspect_donor(d, target_domain, target_url, client), urls)
        )

    alive = [r for r in results if r["found"]]
    follow = [r for r in alive if any(link["follow"] for link in r["links"])]
    return {
        "ok": True,
        "target": target_url or target_domain,
        "donors_checked": len(results),
        "donors_dropped": dropped,
        "summary": {
            "found": len(alive),
            "missing": len(results) - len(alive),
            "dofollow": len(follow),
            "nofollow": len(alive) - len(follow),
            "on_noindex_page": sum(1 for r in alive if r.get("donor_indexable") is False),
        },
        "results": results,
    }
