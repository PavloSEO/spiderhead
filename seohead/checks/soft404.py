"""Detect whether a site returns a genuine 404 for a nonexistent URL.

A broken link discovered by a crawler appears in crawl exports. A soft 404 is
different: the server returns **200 OK** for a nonexistent URL, often with a
templated "nothing found" page. Search engines can treat these responses as
low-value pages eligible for indexing.

A normal crawl does not detect this because it requests known URLs rather
than inventing nonexistent ones. The check therefore needs
active probes, like the reconnaissance modules.

The algorithm creates two deterministic nonexistent URLs from the origin's
SHA-256 hash at ordinary root paths. Requests use the shared HTTP layer with
redirects enabled because the final status matters, not the first hop. Strict
AND logic requires agreement: two 2xx/3xx responses confirm a soft 404
(warning), two 404/410 responses pass, and disagreement produces ``unknown``
without a verdict. A probe whose redirect was refused by our own network guard
(``recon.net.BlockedRedirectError``) is neither: the site tried to send the
probe somewhere we refuse to go, which is worth reporting by name rather than
folded into "the probes disagreed" (#175).
"""

from __future__ import annotations

import hashlib
from typing import Any
from urllib.parse import urlsplit, urlunsplit

PROBE_COUNT = 2


def probe_urls(start_url: str) -> list[str]:
    """Build deterministic nonexistent URLs from a site's origin.

    Hashing the origin yields the same pair on repeated runs, which is useful
    for report caching. The paths are ordinary root routes so static middleware
    that owns ``/.well-known/`` cannot bypass an application's fallback route.
    """
    parts = urlsplit(start_url if "://" in start_url else "https://" + start_url)
    origin = urlunsplit((parts.scheme or "https", parts.netloc, "", "", ""))
    seed = hashlib.sha256(origin.encode()).hexdigest()[:12]
    return [f"{origin}/seo-audit-not-found-{seed}-{i}" for i in (1, 2)]


def classify_soft404(probes: list[dict[str, Any]]) -> str:
    """Classify probes as ``pass``, ``warning`` (soft 404), ``refused``, or ``unknown``.

    Strict AND logic requires both probes to agree. Otherwise the result is
    ``unknown``: one 200 and one 404 response do not support a verdict.
    ``refused`` is checked first and independently of that agreement: a probe
    our own guard declined to follow never got a final status at all, and that
    is a different fact from "the probes disagreed" — collapsing the two would
    make a guard refusal indistinguishable from an inconclusive site (#175).
    """
    if any(p.get("blocked_by_guard") for p in probes):
        return "refused"
    conclusive = [p for p in probes if "status" in p and not p.get("access_blocked")]
    successful = [p for p in conclusive if 200 <= p["status"] < 400]
    correct_missing = [p for p in conclusive if p["status"] in (404, 410)]

    if len(conclusive) != PROBE_COUNT:
        return "unknown"
    if len(successful) == PROBE_COUNT:
        return "warning"  # both probes returned 2xx/3xx: soft 404 confirmed
    if len(correct_missing) == PROBE_COUNT:
        return "pass"  # both probes returned genuine 404/410 responses
    return "unknown"  # disagreement does not support a verdict


def check_soft404(url: str, timeout: float = 20.0) -> dict[str, Any]:
    """Request two probes and classify soft-404 behavior. Network boundary."""
    targets = probe_urls(url)
    try:
        from seohead.recon.net import BlockedRedirectError, http_client

        client, _ = http_client(timeout)
    except ImportError:
        return {"ok": False, "error": "httpx is required"}

    probes: list[dict[str, Any]] = []
    try:
        with client:
            for target in targets:
                try:
                    resp = client.get(target)
                    probes.append(
                        {
                            "url": target,
                            "status": resp.status_code,  # final status after automatic redirects
                            "final_url": str(resp.url),
                            "redirected": str(resp.url) != target,
                        }
                    )
                except BlockedRedirectError as exc:
                    # A real response came back; only the next hop was refused. Recorded
                    # with its own status/location rather than folded into "error" so
                    # classify_soft404 can tell this apart from a plain probe failure.
                    probes.append(
                        {
                            "url": target,
                            "status": exc.status_code,
                            "final_url": exc.location,
                            "redirected": True,
                            "blocked_by_guard": True,
                            "error": str(exc),
                        }
                    )
                except Exception as exc:
                    probes.append({"url": target, "error": str(exc)})
    except Exception as exc:
        return {"ok": False, "url": url, "error": str(exc)}

    verdict = classify_soft404(probes)
    findings = {
        "pass": "the site returns genuine 404/410 responses for nonexistent URLs",
        "warning": (
            "soft 404: nonexistent URLs return 200/3xx responses and may create "
            "indexable low-value pages"
        ),
        "refused": (
            "a probe's redirect target was refused by our own network guard "
            "(it pointed at a private or non-public address); no soft-404 verdict is possible"
        ),
        "unknown": "the probes disagreed or did not complete; no verdict is available",
    }[verdict]
    return {
        "ok": True,
        "url": url,
        "verdict": verdict,
        "probes": probes,
        "findings": [findings],
    }


# Stored-body classifier (issue #988, slice S1). Pure function: no network, no DB.
# ponytail: thin text = whitespace word count, no tokenizer | ceiling: CJK text without spaces counts as one word | upgrade: reuse checks.duplicate._tokenize
THIN_TEXT_WORDS = 50
TEMPLATE_HAMMING_MAX = 3  # bits out of 64 that may differ between a page and the 404 template
ERROR_PHRASES = (
    "страница не найдена",
    "ничего не найдено",
    "товар не найден",
    "page not found",
    "nothing found",
    "404 not found",
)


def classify_stored_soft404(
    *,
    status: int,
    text: str,
    template_simhash: int | None,
    page_simhash: int | None,
) -> dict[str, str]:
    """Classify a stored 200 body as a soft 404, using only the saved text and simhashes.

    Verdicts reuse the ``classify_soft404`` vocabulary: ``warning`` marks a soft 404 and
    ``pass`` means no soft-404 signal. Non-200 responses are never soft 404s here.
    """
    from seohead.checks.duplicate import hamming

    if status != 200:
        return {"verdict": "pass", "reason": "none"}
    words = len(text.split())
    if (
        template_simhash is not None
        and page_simhash is not None
        and hamming(page_simhash, template_simhash) <= TEMPLATE_HAMMING_MAX
    ):
        return {"verdict": "warning", "reason": "template_match"}
    if words == 0:
        return {"verdict": "warning", "reason": "thin_text"}
    lowered = text.lower()
    if words < THIN_TEXT_WORDS and any(phrase in lowered for phrase in ERROR_PHRASES):
        return {"verdict": "warning", "reason": "error_phrase"}
    return {"verdict": "pass", "reason": "none"}
