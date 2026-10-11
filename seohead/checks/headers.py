"""HTTP response header + timing inspector for SEO.

Fetches a URL and reports SEO-relevant response headers (X-Robots-Tag, canonical
Link header, Cache-Control, Content-Type, HSTS, Vary, Content-Encoding, server),
the HTTP version, TTFB, and body size.
"""

from __future__ import annotations

import time

from seohead.recon.net import http_client

_SEO_HEADERS = [
    "content-type",
    "x-robots-tag",
    "link",
    "cache-control",
    "expires",
    "content-encoding",
    "vary",
    "strict-transport-security",
    "content-length",
    "server",
    "last-modified",
    "etag",
    "location",
    "content-language",
]


def check_headers(url: str, method: str = "GET", timeout: float = 25.0) -> dict:
    method = str(method).upper()
    if method not in {"GET", "HEAD"}:
        return {"ok": False, "url": url, "error": "headers-check method must be GET or HEAD"}
    # Without the h2 package, httpx always negotiates HTTP/1.1, so a protocol
    # finding would describe our client rather than the server. Expose this limitation.
    try:
        client, http2_capable = http_client(timeout, follow_redirects=True)
    except Exception as exc:
        return {"ok": False, "url": url, "error": str(exc)}

    try:
        start = time.perf_counter()
        with client:
            request = client.build_request(method, url)
            resp = client.send(request, stream=True)
            ttfb_ms = round((time.perf_counter() - start) * 1000, 1)
            body = resp.read()
            resp.close()
    except Exception as exc:
        return {"ok": False, "url": url, "error": str(exc)}

    headers = {k.lower(): v for k, v in resp.headers.items()}
    seo = {h: headers[h] for h in _SEO_HEADERS if h in headers}
    http_version = getattr(resp, "http_version", "?")

    findings: list[str] = []
    if "x-robots-tag" in seo and any(t in seo["x-robots-tag"].lower() for t in ("noindex", "none")):
        findings.append("X-Robots-Tag blocks indexing (noindex/none)")
    if not http2_capable:
        findings.append("HTTP version not measurable: the h2 package is missing")
    elif http_version and http_version.startswith("HTTP/1"):
        findings.append(f"served over {http_version} (consider HTTP/2)")
    if "cache-control" not in seo:
        findings.append("no Cache-Control header")

    return {
        "ok": True,
        "url": url,
        "final_url": str(resp.url),
        "status_code": resp.status_code,
        "http_version": http_version,
        "http_version_measurable": http2_capable,
        "ttfb_ms": ttfb_ms,
        "bytes": len(body),
        "redirected": str(resp.url) != url,
        "seo_headers": seo,
        "findings": findings,
    }


if __name__ == "__main__":
    # network-free sanity of the header-selection logic
    sample = {"content-type": "text/html", "x-robots-tag": "noindex, nofollow", "server": "nginx"}
    picked = {h: sample[h] for h in _SEO_HEADERS if h in sample}
    assert "x-robots-tag" in picked and "server" in picked
    print("OK: headers self-check passed")
