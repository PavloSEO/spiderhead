"""Redirect rule generation and live redirect-chain checking.

Two public entry points:

* :func:`generate_rules` — a **pure** function that turns a list of redirect
  descriptors into web-server rule strings (Apache mod_alias / mod_rewrite,
  Nginx, or a custom template). No network access.
* :func:`check_chain` — follows a live redirect chain **without** auto-follow
  (``httpx`` with ``follow_redirects=False``), returning one hop dict per step.

Redirect input dicts accept aliased keys so the shared handler layer can pass
whatever casing it has: ``old_url | oldUrl | from`` for the source URL,
``new_url | newUrl | to`` for the target, and ``redirect_to_default`` (bool).

Only the standard library plus ``httpx`` are imported.
"""

from __future__ import annotations

import csv
import io
import re
from typing import Any
from urllib.parse import urljoin, urlsplit, urlunsplit

from seohead.recon.net import UA, http_client

try:  # httpx is only needed for the live checker; keep import failures soft.
    import httpx
except ImportError:  # pragma: no cover - exercised only without the dep
    httpx = None  # type: ignore[assignment]


# ── Format aliases ───────────────────────────────────────────────────────────

# The handler layer speaks a small vocabulary of formats; ``apache`` is an
# alias for the mod_rewrite rule form.
_FORMAT_ALIASES = {
    "apache": "apache-rewrite-rule",
}

_KNOWN_FORMATS = {
    "apache-rewrite-rule",
    "apache-redirect",
    "nginx",
    "csv",
    "custom",
    "wordpress-redirection",
}


# ── Pure helpers ─────────────────────────────────────────────────────────────


def normalize_url(url: str) -> str:
    """Strip scheme + host, leaving only the path (+ query/fragment).

    Mirrors the TS ``normalizeUrl``: trims whitespace and removes a leading
    ``http://host`` / ``https://host`` prefix. Non-string / empty input yields
    an empty string.
    """
    if not url:
        return ""
    return re.sub(r"^https?://[^/]+", "", url.strip())


def _pick(item: dict, *keys: str) -> Any:
    """Return the first present, non-None value among ``keys`` in ``item``."""
    for key in keys:
        if key in item and item[key] is not None:
            return item[key]
    return None


def _normalize_item(item: dict) -> dict:
    """Normalize aliased redirect keys into ``old_url`` / ``new_url`` / flag."""
    return {
        "old_url": _pick(item, "old_url", "oldUrl", "from") or "",
        "new_url": _pick(item, "new_url", "newUrl", "to") or "",
        "redirect_to_default": bool(
            _pick(item, "redirect_to_default", "redirectToDefault") or False
        ),
    }


def generate_rule(
    old_url: str,
    new_url: str,
    fmt: str,
    custom_template: str = "",
) -> str:
    """Generate a single redirect rule string.

    Raises ``ValueError`` when the input is invalid for the chosen format
    (empty source URL, missing target where required, empty custom template,
    or an unknown format).
    """
    fmt = _FORMAT_ALIASES.get(fmt, fmt)
    old_url = normalize_url(old_url)

    if not old_url:
        raise ValueError("Source URL cannot be empty")

    if fmt == "custom":
        if not custom_template:
            raise ValueError("Custom template cannot be empty")
        return custom_template.replace("{{from}}", old_url).replace("{{to}}", new_url or "")

    # Apache mod_alias
    if fmt == "apache-redirect":
        if not new_url:
            raise ValueError("Target URL is required for this format")
        return f"Redirect 301 {old_url} {new_url}"

    # Apache mod_rewrite
    if fmt == "apache-rewrite-rule":
        if not new_url:
            raise ValueError("Target URL is required for this format")
        escaped = old_url.replace("$", "\\$")
        return f"RewriteRule ^{escaped}$ {new_url} [R=301,L]"

    # Nginx
    if fmt == "nginx":
        if not new_url:
            raise ValueError("Target URL is required for this format")
        escaped = old_url.replace("$", "\\$")
        return f"rewrite ^{escaped}$ {new_url} permanent;"

    # CSV row: source, target, 301 (one row per rule, quoted when needed)
    if fmt == "csv":
        if not new_url:
            raise ValueError("Target URL is required for this format")
        buffer = io.StringIO()
        csv.writer(buffer, lineterminator="").writerow([old_url, new_url, 301])
        return buffer.getvalue()

    if fmt == "wordpress-redirection":
        if not new_url:
            raise ValueError("Target URL is required for this format")
        buffer = io.StringIO()
        csv.writer(buffer, lineterminator="").writerow([old_url, new_url, 0, 301, "url"])
        return buffer.getvalue()

    raise ValueError(f"Unknown format: {fmt}")


def generate_rules(
    redirects: list[dict],
    fmt: str = "apache-rewrite-rule",
    default_url: str = "/",
    custom_template: str = "",
) -> list[str]:
    """Generate rule strings for a list of redirect descriptors (PURE).

    Each item may use aliased keys (``old_url|oldUrl|from``,
    ``new_url|newUrl|to``, ``redirect_to_default``). Items without a source URL
    are skipped. When ``redirect_to_default`` is set the target becomes
    ``default_url``. Per-item generation errors are captured as an
    ``"ERROR: <message>"`` string rather than aborting the whole batch.
    """
    rules: list[str] = []

    for raw in redirects:
        item = _normalize_item(raw)
        if not item["old_url"]:
            continue

        new_url = default_url if item["redirect_to_default"] else item["new_url"]

        try:
            rules.append(generate_rule(item["old_url"], new_url, fmt, custom_template))
        except ValueError as error:
            rules.append(f"ERROR: {error}")

    return rules


def analyze_redirect_map(redirects: list[dict], default_url: str = "/") -> dict[str, list[dict[str, Any]]]:
    """Report local redirect chains and loops without making network requests."""
    mapping: dict[str, str] = {}
    for raw in redirects:
        item = _normalize_item(raw)
        raw_source = str(item["old_url"]).strip()
        raw_target = str(default_url if item["redirect_to_default"] else item["new_url"]).strip()
        source = _chain_key(raw_source)
        target = _chain_key(urljoin(raw_source, raw_target) if urlsplit(raw_source).scheme else raw_target)
        if source and target:
            mapping[source] = target
    chains: list[dict[str, Any]] = []
    loops: list[dict[str, Any]] = []
    for source in mapping:
        path = [source]
        seen = {source: 0}
        current = source
        while current in mapping:
            current = mapping[current]
            path.append(current)
            if current in seen:
                loop_start = seen[current]
                loops.append({"source": source, "path": path[loop_start:]})
                break
            seen[current] = len(path) - 1
        else:
            if len(path) > 2:
                chains.append({"source": source, "path": path})
    return {"chains": chains, "loops": loops}


def _chain_key(value: str) -> str:
    """Normalize absolute URLs without conflating paths on different hosts."""
    value = value.strip()
    if not value:
        return ""
    parts = urlsplit(value)
    if parts.scheme and parts.netloc:
        return urlunsplit((parts.scheme.lower(), parts.netloc.lower(), parts.path or "/", parts.query, ""))
    return normalize_url(value)


# ── Live redirect chain checker ──────────────────────────────────────────────

_MAX_HOPS_CAP = 10


def _check_step(client: httpx.Client, url: str, options: dict) -> dict:
    """Perform one HTTP request (no auto-follow) and describe the hop."""
    method = str(options.get("method") or "HEAD").upper()
    user_agent = options.get("user_agent") or options.get("userAgent") or UA
    headers = {"User-Agent": user_agent, "Accept": "*/*"}

    try:
        response = client.request(method, url, headers=headers)
    except httpx.TimeoutException:
        return {"url": url, "status": 0, "location": None, "ok": False, "error": "Timeout"}
    except httpx.HTTPError as error:
        return {"url": url, "status": 0, "location": None, "ok": False, "error": str(error)}

    status = response.status_code
    return {
        "url": url,
        "status": status,
        "location": response.headers.get("location"),
        "ok": 200 <= status < 400,
    }


def check_chain(url: str, options: dict | None = None) -> list[dict]:
    """Follow a redirect chain hop by hop without auto-following.

    Returns a list of hop dicts ``{url, status, location, ok}`` (plus an
    ``error`` key on failed hops), up to a hard cap of 10 hops. Relative
    ``Location`` headers are resolved against the current URL, and repeated
    URLs are detected as a redirect loop. Never raises: any setup failure is
    returned as a single error hop.
    """
    options = options or {}

    if httpx is None:
        return [
            {
                "url": url,
                "status": 0,
                "location": None,
                "ok": False,
                "error": "httpx is not installed",
            }
        ]

    max_hops = options.get("max_hops") or options.get("maxHops") or _MAX_HOPS_CAP
    try:
        max_hops = int(max_hops)
    except (TypeError, ValueError):
        max_hops = _MAX_HOPS_CAP
    max_hops = min(max(max_hops, 1), _MAX_HOPS_CAP)

    timeout = options.get("timeout")
    try:
        timeout = float(timeout) if timeout is not None else 10.0
    except (TypeError, ValueError):
        timeout = 10.0

    chain: list[dict] = []
    current = (url or "").strip()
    visited: set[str] = set()

    try:
        client, _http2_capable = http_client(timeout, follow_redirects=False)
        with client:
            while current and len(chain) < max_hops:
                if current in visited:
                    chain.append(
                        {
                            "url": current,
                            "status": 0,
                            "location": None,
                            "ok": False,
                            "error": "Redirect loop detected",
                        }
                    )
                    break
                visited.add(current)

                step = _check_step(client, current, options)
                chain.append(step)

                status = step["status"]
                location = step.get("location")
                if 300 <= status < 400 and location:
                    try:
                        current = urljoin(current, location)
                    except ValueError:
                        break
                else:
                    break

        if len(chain) >= max_hops:
            last = chain[-1]
            if 300 <= last["status"] < 400 and last.get("location"):
                chain.append(
                    {
                        "url": current,
                        "status": 0,
                        "location": None,
                        "ok": False,
                        "error": f"Redirect limit exceeded ({max_hops})",
                    }
                )
    except Exception as error:
        chain.append(
            {"url": current, "status": 0, "location": None, "ok": False, "error": str(error)}
        )

    return chain


# ── Smoke test (no network) ──────────────────────────────────────────────────

if __name__ == "__main__":
    sample = [
        {"from": "https://example.com/old-page", "to": "/new-page"},
        {"oldUrl": "/legacy", "redirectToDefault": True},
        {"old_url": "/no-target"},  # error case for apache-rewrite-rule
        {"new_url": "/orphan"},  # skipped: no source
    ]

    apache = generate_rules(sample, fmt="apache")
    assert apache[0] == "RewriteRule ^/old-page$ /new-page [R=301,L]", apache[0]
    assert apache[1] == "RewriteRule ^/legacy$ / [R=301,L]", apache[1]
    assert apache[2].startswith("ERROR:"), apache[2]
    assert len(apache) == 3, apache  # orphan skipped

    nginx = generate_rules([{"from": "/a", "to": "/b"}], fmt="nginx")
    assert nginx == ["rewrite ^/a$ /b permanent;"], nginx

    redir = generate_rules([{"from": "/a", "to": "/b"}], fmt="apache-redirect")
    assert redir == ["Redirect 301 /a /b"], redir

    custom = generate_rules(
        [{"from": "/a", "to": "/b"}],
        fmt="custom",
        custom_template="{{from}} -> {{to}}",
    )
    assert custom == ["/a -> /b"], custom

    assert normalize_url("https://x.io/path?q=1") == "/path?q=1"
    assert normalize_url("  /already  ") == "/already"

    print("redirects.py self-check OK")
