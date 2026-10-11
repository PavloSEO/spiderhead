"""CSS/JS weight and delivery analysis.

Fetches a page, discovers its linked stylesheets and scripts, fetches each of
them, and reports the checks that are answerable from bytes on the wire and
static markup — no rendering required:

* minification (a whitespace-ratio / line-length heuristic)
* render-blocking ``<script>``/``<link rel=stylesheet>`` in ``<head>``
* oversized individual files (configurable threshold)
* duplicate libraries bundled more than once, by content hash of the
  whitespace-stripped source (not by filename)
* compression (``Content-Encoding``) and cache lifetime (``Cache-Control``)
* ``@font-face`` blocks missing ``font-display: swap`` (or an equivalent value)
* legacy transpiled/polyfilled JS shipped unconditionally (a heuristic)
* source maps whose target actually resolves (fetched, not just referenced)
* debug code (``console.log``/``debug``, ``debugger``, ``alert(``) left in a
  file that is otherwise minified
* ``document.write`` calls
* ``@import`` chains in CSS, followed one level to report their depth

Two checks from the issue this module answers are deliberately NOT attempted
here and are reported under ``skipped`` rather than silently passing:

* **unused CSS/JS** — telling "loaded" from "used" needs a rendered DOM
  (coverage-style analysis), which this static-fetch tool does not have;
* **per-site bundle-size outliers** — needs more than one page. Once a caller
  has run :func:`analyze_page_asset_weight` over several pages, feed the
  resulting ``total_bytes`` values to :func:`flag_outlier_pages`.

Two more from the same issue are out of scope for this module entirely (see
issue #78): known-vulnerable library versions need an advisory database, not
just the fingerprinting `seohead/recon/tech.py` already does; inline
``<style>``/``<script>`` bulk needs a crawl-wide, multi-page pass to tell a
repeated block from a one-off.

Public API:
    analyze_page_asset_weight(url, **options) -> dict
    flag_outlier_pages(page_totals) -> list[str]
"""

from __future__ import annotations

import hashlib
import json
import re
from concurrent.futures import ThreadPoolExecutor
from contextlib import nullcontext
from statistics import median
from typing import Any
from urllib.parse import urljoin

from bs4 import BeautifulSoup

from seohead.checks.parser import document_base_url, is_inert_template_content
from seohead.recon.net import UA, http_client

DEFAULT_TIMEOUT = 15.0
# The issue's own suggested threshold for a single oversized file.
DEFAULT_FILE_SIZE_THRESHOLD_BYTES = 500_000
# Bounds one page's own fetch fan-out: a page linking hundreds of resources
# (often third-party trackers, not the site's own delivery problem) should not
# turn one audit call into an unbounded crawl.
MAX_RESOURCES = 60
DEFAULT_CONCURRENCY = 6

# Below this, a Cache-Control max-age is not "long-lived" for a static asset:
# a hashed/versioned filename can safely be cached far longer than an HTML
# page, so a short TTL here is a missed easy win rather than a correctness bug.
LONG_CACHE_SECONDS = 7 * 24 * 3600

# A CSS file importing more than this many other stylesheets is almost always
# a build mistake, not a deliberate chain worth reporting one round trip at a
# time — bounds the follow-up fetches the same way MAX_RESOURCES bounds the
# initial discovery.
MAX_IMPORTS_PER_FILE = 10

_COMPRESSED_ENCODINGS = {"gzip", "br", "deflate", "zstd"}
_MAX_AGE_RE = re.compile(r"(?:^|,)\s*max-age\s*=\s*(\d+)(?=\s*(?:,|$))", re.IGNORECASE)
_FONT_FACE_RE = re.compile(r"@font-face\s*\{([^}]*)\}", re.IGNORECASE | re.DOTALL)
_FONT_DISPLAY_OK_RE = re.compile(r"font-display\s*:\s*(swap|fallback|optional)", re.IGNORECASE)
# core-js/babel helpers are the standard marker a bundler leaves behind when it
# shipped transpiled/polyfilled code; a hand-rolled Object.assign shim is the
# same intent without a named library.
_LEGACY_JS_RE = re.compile(
    r"core-js|regeneratorRuntime|_babelPolyfill|@babel/runtime|Object\.assign\s*=\s*function"
)
_WHITESPACE_RE = re.compile(r"\s+")
# Matches both the JS (`//# sourceMappingURL=...`) and CSS
# (`/*# sourceMappingURL=... */`) comment forms: stopping at whitespace or `*`
# excludes the CSS block comment's closing `*/` from the captured target.
_SOURCE_MAP_RE = re.compile(r"sourceMappingURL=([^\s*]+)")
# console.log/debug and alert( are legitimate in hand-authored source; the
# minification gate below is what turns their presence into a real finding.
_DEBUG_CODE_RE = re.compile(r"\bconsole\.(?:log|debug)\s*\(|\bdebugger\b|\balert\s*\(")
_DOCUMENT_WRITE_RE = re.compile(r"\bdocument\.write\s*\(")
# Covers `@import "x.css"`, `@import url(x.css)` and `@import url("x.css")`,
# with or without a trailing media query, which the capture group ignores.
_CSS_IMPORT_RE = re.compile(r"""@import\s+(?:url\(\s*)?['"]?([^'"()\s;]+)['"]?\)?""", re.IGNORECASE)


# ── pure checks (no network) ────────────────────────────────────────────────


def whitespace_ratio(text: str) -> float:
    """Fraction of ``text`` that is whitespace."""
    if not text:
        return 0.0
    return sum(1 for c in text if c.isspace()) / len(text)


def looks_minified(text: str) -> bool:
    """Heuristic: minified CSS/JS reads as long lines with little whitespace.

    Hand-authored code is reformatted onto many short, indented lines, which
    pushes the whitespace ratio well above this line and the average line
    length well below it. A round-trip through a real minifier would be exact
    but adds a build-tool dependency for a signal this heuristic already gets
    right on both fixtures the acceptance criteria describe.
    """
    stripped = text.strip()
    lines = stripped.splitlines() or [stripped]
    if len(stripped) < 200:
        # Too small for the line-length heuristic below to carry a signal,
        # but a single unbroken line with little whitespace still looks
        # minified, while multiple hand-formatted lines never do — so fall
        # back to shape rather than defaulting every short file to "minified"
        # regardless of formatting.
        return len(lines) <= 1 and whitespace_ratio(stripped) < 0.15
    avg_line_length = len(stripped) / len(lines)
    return whitespace_ratio(stripped) < 0.15 and avg_line_length > 200


def content_hash(text: str) -> str:
    """SHA-256 of ``text`` with all whitespace removed.

    Whitespace-only reformatting (a different line-wrap width, a trailing
    newline) must not hide that two files bundle the same library, and must
    not manufacture a "duplicate" out of two files that only look alike.
    """
    normalized = _WHITESPACE_RE.sub("", text)
    return hashlib.sha256(normalized.encode("utf-8", "ignore")).hexdigest()


def resource_delivery_evidence(body: bytes, kind: str, headers: Any) -> dict[str, Any]:
    """Summarize a completed native resource fetch without retaining source code."""
    from seohead.storage.bodies import decode_entity
    from seohead.storage.resources import media_matches

    kind = {"module": "script", "css_import": "stylesheet"}.get(kind, kind)
    values: dict[str, str] = {}
    for k, v in headers:
        name = str(k).lower()
        values[name] = ",".join(filter(None, (values.get(name), str(v))))
    is_source = kind in {"script", "stylesheet"} and media_matches(
        kind, values.get("content-type", "")
    )
    text = decode_entity(body, values.get("content-type", ""))[0] if is_source else ""
    fonts = find_missing_font_display(text) if is_source and kind == "stylesheet" else []
    evidence = {
        "cache": check_cache_lifetime(values.get("cache-control")),
        "source_state": "complete"
        if is_source
        else "unavailable"
        if kind in {"script", "stylesheet"}
        else "not_applicable",
        "font_display": fonts[:20] if is_source else None,
        "font_display_count": len(fonts) if is_source else None,
        "font_display_omitted": max(0, len(fonts) - 20) if is_source else None,
        "unminified": bool(text) and not looks_minified(text) if is_source else None,
        "legacy_js": looks_legacy_transpiled(text) if is_source and kind == "script" else None,
        # Exact bytes: stripping whitespace inside string literals can conflate different programs.
        "sha256": hashlib.sha256(body).hexdigest()
        if is_source and kind == "script" and body
        else None,
    }

    # The retained validator caps the whole ASCII-escaped delivery envelope,
    # not just the number of excerpts. Unicode escaping can expand short samples.
    sample = evidence["font_display"]
    if isinstance(sample, list):
        while len(json.dumps(evidence, ensure_ascii=True)) > 16 * 1024:
            sample.pop()
            evidence["font_display_omitted"] = len(fonts) - len(sample)
    return evidence


def find_duplicate_libraries(resources: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Group fetched resources of the same kind by content hash.

    A group is reported only when it spans more than one distinct URL —
    the same file linked twice is not a duplicate library, it is one file.
    """
    by_key: dict[tuple[str, str], list[str]] = {}
    for res in resources:
        if not res.get("ok") or not res.get("text"):
            continue
        key = (res.get("kind", ""), content_hash(res["text"]))
        by_key.setdefault(key, []).append(res["url"])

    out = []
    for (kind, digest), urls in sorted(by_key.items()):
        unique = sorted(set(urls))
        if len(unique) > 1:
            out.append({"kind": kind, "hash": digest, "urls": unique})
    return out


def is_render_blocking(tag_name: str, attrs: dict[str, Any]) -> bool:
    """Whether one ``<script>``/``<link>`` tag blocks first paint as written.

    A script is blocking unless it opts out: ``async``/``defer``, or a
    ``type`` the spec already defers (``module``) or that never executes as a
    classic script (``application/json`` and similar data islands).
    A stylesheet link is blocking unless ``media`` restricts it to a
    condition the initial render does not need, such as ``print``.
    """
    if tag_name == "script":
        if "async" in attrs or "defer" in attrs:
            return False
        script_type = str(attrs.get("type") or "").lower().strip()
        return script_type in ("", "text/javascript", "application/javascript")
    if tag_name == "link":
        media = str(attrs.get("media") or "").strip().lower()
        return media in ("", "all", "screen")
    return False


def find_render_blocking_resources(soup: BeautifulSoup, base_url: str) -> list[dict[str, Any]]:
    """Render-blocking ``<script src>`` / ``<link rel=stylesheet>`` in ``<head>``.

    Skips ``<template>`` descendants: a fragment a script has not cloned in yet
    blocks nothing, so it must not appear as a render-blocking resource (#236).
    """
    head = soup.head
    if head is None:
        return []
    out = []
    for tag in head.find_all("script"):
        if is_inert_template_content(tag):
            continue
        src = tag.get("src")
        if src and is_render_blocking("script", tag.attrs):
            out.append({"url": urljoin(base_url, src), "tag": "script"})
    for tag in head.find_all("link"):
        if is_inert_template_content(tag):
            continue
        rels = tag.get("rel") or []
        rels = [rels] if isinstance(rels, str) else rels
        href = tag.get("href")
        if (
            href
            and "stylesheet" in [r.lower() for r in rels]
            and is_render_blocking("link", tag.attrs)
        ):
            out.append({"url": urljoin(base_url, href), "tag": "link"})
    return out


def find_missing_font_display(css_text: str) -> list[dict[str, str]]:
    """``@font-face`` blocks without ``font-display: swap`` (or an equivalent)."""
    out = []
    for block in _FONT_FACE_RE.findall(css_text or ""):
        if not _FONT_DISPLAY_OK_RE.search(block):
            out.append({"excerpt": block.strip()[:200]})
    return out


def looks_legacy_transpiled(js_text: str) -> bool:
    """Whether ``js_text`` carries a transpiler/polyfill marker (a heuristic, not proof)."""
    return bool(_LEGACY_JS_RE.search(js_text or ""))


def find_source_map_comment(text: str) -> str | None:
    """The ``sourceMappingURL`` target referenced by ``text``, or ``None``.

    Only the comment is read here — whether the target is actually fetchable
    is a network question the caller answers separately. A ``data:`` URI is
    an inline map with nothing served over the network, so it is not
    "exposed" and is excluded. When a file carries more than one such
    comment (rebuilt without stripping the old one), the last one wins, since
    that is the one a real browser would act on.
    """
    matches = _SOURCE_MAP_RE.findall(text or "")
    if not matches:
        return None
    target = matches[-1]
    return None if target.lower().startswith("data:") else target


def find_debug_code(js_text: str) -> list[str]:
    """Debug markers (``console.log``/``debug``, ``debugger``, ``alert(``) in ``js_text``.

    Meaningful only in a file that is otherwise minified: a ``debugger``
    statement halts execution for anyone with devtools open, and a
    ``console`` call in a hot path costs real main-thread time — but the same
    calls in hand-authored source are just normal development noise, so an
    unminified file is never flagged here.
    """
    if not looks_minified(js_text):
        return []
    return sorted(set(_DEBUG_CODE_RE.findall(js_text or "")))


def has_document_write(js_text: str) -> bool:
    """Whether ``js_text`` calls ``document.write``.

    ``document.write`` blocks the HTML parser while it runs, and Chrome
    ignores it outright for scripts injected into a page loaded over a slow
    connection — so the call either stalls rendering or silently does
    nothing.
    """
    return bool(_DOCUMENT_WRITE_RE.search(js_text or ""))


def find_css_imports(css_text: str) -> list[str]:
    """Raw ``@import`` targets referenced by ``css_text``, in source order."""
    return _CSS_IMPORT_RE.findall(css_text or "")[:MAX_IMPORTS_PER_FILE]


def check_cache_lifetime(cache_control: str | None) -> dict[str, Any]:
    """Whether a static asset's ``Cache-Control`` is long-lived."""
    value = cache_control or ""
    lowered = value.lower()
    if "no-store" in lowered or "no-cache" in lowered:
        return {"ok": False, "max_age": 0, "reason": "no-store/no-cache on a static asset"}
    match = _MAX_AGE_RE.search(value)
    if match and len(match.group(1)) > 19:
        return {
            "ok": False,
            "max_age": None,
            "reason": "Cache-Control max-age exceeds the supported integer bound",
        }
    max_age = int(match.group(1)) if match else None
    if max_age is None:
        return {"ok": False, "max_age": None, "reason": "no Cache-Control max-age"}
    if "immutable" in lowered or max_age >= LONG_CACHE_SECONDS:
        return {"ok": True, "max_age": max_age, "reason": None}
    return {"ok": False, "max_age": max_age, "reason": "max-age is short for a static asset"}


def check_compression(content_encoding: str | None) -> dict[str, Any]:
    """Whether a resource was served compressed."""
    value = (content_encoding or "").strip().lower()
    return {"ok": value in _COMPRESSED_ENCODINGS, "encoding": value or None}


def flag_outlier_pages(
    page_totals: dict[str, int], *, multiple: float = 2.0, min_bytes: int = 100_000
) -> list[str]:
    """Pages whose total CSS+JS payload dwarfs the site's median.

    ``min_bytes`` guards a templated site of near-identical pages from having
    every page above the median called an outlier over a few stray bytes —
    the same failure mode ``check_html_weight`` guards against for whole-page
    size (see ``seohead/sf/core/heuristics.py``).
    """
    sizes = list(page_totals.values())
    if len(sizes) < 2:
        return []
    baseline = median(sizes) or 1
    return sorted(
        url
        for url, total in page_totals.items()
        if total > baseline * multiple and total - baseline > min_bytes
    )


# ── fetch + orchestrate ──────────────────────────────────────────────────────


def _discover_resources(soup: BeautifulSoup, base_url: str) -> list[dict[str, str]]:
    """Direct ``<link rel=stylesheet>`` and ``<script src>`` URLs, deduplicated.

    Skips ``<template>`` descendants: a stylesheet or script held only in a
    DocumentFragment is never requested by a browser, so counting it would
    fabricate bytes, minification, cache, and duplicate-library findings for a
    resource nothing ever fetches (#236).
    """
    from seohead.checks.parser import extract_resource_declarations

    declarations, _omitted = extract_resource_declarations(soup, base_url)
    seen: set[str] = set()
    out: list[dict[str, str]] = []
    for declaration_kind, output_kind in (("stylesheet", "css"), ("script", "js")):
        for declaration in declarations:
            if declaration["kind"] != declaration_kind or declaration["url"] in seen:
                continue
            seen.add(declaration["url"])
            out.append({"url": declaration["url"], "kind": output_kind})
    return out


def _response_text(resp: Any) -> str:
    """Body text of a response: ``.text`` when present, else decoded ``.content``."""
    if hasattr(resp, "text"):
        return resp.text
    return (getattr(resp, "content", None) or b"").decode("utf-8", "ignore")


def analyze_page_asset_weight(
    url: str,
    *,
    file_size_threshold: int = DEFAULT_FILE_SIZE_THRESHOLD_BYTES,
    max_resources: int = MAX_RESOURCES,
    concurrency: int = DEFAULT_CONCURRENCY,
    timeout: float = DEFAULT_TIMEOUT,
    fetcher=None,
) -> dict[str, Any]:
    """Fetch ``url`` and its linked CSS/JS, and run every static-analysis check.

    ``fetcher``, when given, replaces the network client with a callable
    ``fetcher(resource_url) -> response``; a response needs only ``.status_code``,
    ``.content`` (or ``.text``), and ``.headers``. This is how tests exercise the
    whole pipeline without a socket.
    """
    client = nullcontext()
    if fetcher is None:
        client, _http2_capable = http_client(timeout, headers={"User-Agent": UA})

    def _get(target_url: str) -> Any:
        return fetcher(target_url) if fetcher else client.get(target_url)

    def fetch_one(target: dict[str, str]) -> dict[str, Any]:
        try:
            resp = _get(target["url"])
        except Exception as exc:
            return {**target, "ok": False, "error": str(exc)}
        content = getattr(resp, "content", None)
        text = _response_text(resp)
        # Decoded size: what the browser parses and executes, which is what a
        # minification/bloat check cares about, not the compressed wire size.
        size = len(content) if content is not None else len(text.encode("utf-8"))
        headers = getattr(resp, "headers", {}) or {}
        return {
            **target,
            "ok": resp.status_code < 400,
            "status_code": resp.status_code,
            "bytes": size,
            "text": text,
            "cache_control": headers.get("cache-control"),
            "content_encoding": headers.get("content-encoding"),
        }

    def probe_url(target_url: str) -> bool:
        """Whether ``target_url`` resolves — a HEAD, falling back to GET.

        Only a status code is needed here, unlike ``fetch_one``: a source map
        is only a real exposure once its target is confirmed fetchable, and
        confirming that never requires downloading the map itself.
        """
        try:
            if fetcher:
                resp = fetcher(target_url)
            else:
                resp = client.head(target_url)
                if resp.status_code >= 400 or resp.status_code == 405:
                    resp = client.get(target_url)  # some hosts reject HEAD
        except Exception:
            return False
        return resp.status_code < 400

    def fetch_text(target_url: str) -> str | None:
        """Body text of ``target_url``, or ``None`` on any failure.

        Used for one-level ``@import`` follow-up, where (unlike a source-map
        probe) the imported file's own content must be inspected.
        """
        try:
            resp = _get(target_url)
        except Exception:
            return None
        if resp.status_code >= 400:
            return None
        return _response_text(resp)

    with client:
        try:
            page_resp = _get(url)
        except Exception as exc:
            return {"ok": False, "url": url, "error": str(exc)}
        if page_resp.status_code >= 400:
            return {"ok": False, "url": url, "status_code": page_resp.status_code}

        final_url = str(getattr(page_resp, "url", url) or url)
        soup = BeautifulSoup(page_resp.text, features="lxml")
        base_url = document_base_url(soup, final_url)

        render_blocking = find_render_blocking_resources(soup, base_url)
        targets = _discover_resources(soup, base_url)
        truncated = len(targets) > max_resources
        targets = targets[:max_resources]

        if not targets:
            resources: list[dict[str, Any]] = []
        elif fetcher is not None:
            # A caller-supplied fetcher (tests, a plain dict lookup) is not
            # promised to be thread-safe.
            resources = [fetch_one(t) for t in targets]
        else:
            workers = max(1, min(int(concurrency), 10, len(targets)))
            with ThreadPoolExecutor(max_workers=workers) as pool:
                resources = list(pool.map(fetch_one, targets))

        # A source map is only a real exposure once its target is confirmed
        # fetchable — the comment alone proves nothing about production.
        exposed_source_maps = []
        for res in resources:
            if not res.get("ok"):
                continue
            comment = find_source_map_comment(res["text"])
            if not comment:
                continue
            map_url = urljoin(res["url"], comment)
            if probe_url(map_url):
                exposed_source_maps.append({"source": res["url"], "map_url": map_url})

        # One level of @import follow-up: fetch what the stylesheet imports,
        # and check only that file (not its own imports) for further chaining.
        import_chains = []
        for res in resources:
            if not res.get("ok") or res["kind"] != "css":
                continue
            for target in find_css_imports(res["text"]):
                if target.lower().startswith("data:"):
                    continue
                import_url = urljoin(res["url"], target)
                imported_text = fetch_text(import_url)
                depth = 1
                if imported_text is not None and find_css_imports(imported_text):
                    depth = 2
                import_chains.append(
                    {"source": res["url"], "import_url": import_url, "depth": depth}
                )

    oversized = [
        {"url": r["url"], "bytes": r["bytes"], "threshold": file_size_threshold}
        for r in resources
        if r.get("ok") and r["bytes"] > file_size_threshold
    ]
    duplicates = find_duplicate_libraries(resources)

    (
        unminified,
        missing_font_display,
        legacy_js,
        cache_findings,
        compression_findings,
        debug_code,
        document_write,
    ) = ([], [], [], [], [], [], [])
    for res in resources:
        if not res.get("ok"):
            continue
        if not looks_minified(res["text"]):
            unminified.append(res["url"])
        if res["kind"] == "css":
            missing_font_display += [
                {"source": res["url"], **f} for f in find_missing_font_display(res["text"])
            ]
        else:
            if looks_legacy_transpiled(res["text"]):
                legacy_js.append(res["url"])
            markers = find_debug_code(res["text"])
            if markers:
                debug_code.append({"url": res["url"], "markers": markers})
            if has_document_write(res["text"]):
                document_write.append(res["url"])
        cache = check_cache_lifetime(res.get("cache_control"))
        if not cache["ok"]:
            cache_findings.append({"url": res["url"], **cache})
        compression = check_compression(res.get("content_encoding"))
        if not compression["ok"]:
            compression_findings.append({"url": res["url"], **compression})

    # Inline <style> blocks carry the same font-display risk as an external
    # stylesheet, at zero fetch cost.
    for style_tag in soup.find_all("style"):
        missing_font_display += [
            {"source": "inline", **f} for f in find_missing_font_display(style_tag.get_text())
        ]

    total_bytes = sum(r["bytes"] for r in resources if r.get("ok"))
    findings = []
    if render_blocking:
        findings.append(f"{len(render_blocking)} render-blocking resource(s) in <head>")
    if oversized:
        findings.append(f"{len(oversized)} file(s) over the {file_size_threshold}-byte threshold")
    if duplicates:
        findings.append(f"{len(duplicates)} library bundled more than once")
    if unminified:
        findings.append(f"{len(unminified)} file(s) do not look minified")
    if missing_font_display:
        findings.append(f"{len(missing_font_display)} @font-face block(s) without font-display")
    if legacy_js:
        findings.append(f"{len(legacy_js)} script(s) look like unconditional legacy/polyfill code")
    if cache_findings:
        findings.append(f"{len(cache_findings)} resource(s) without a long-lived Cache-Control")
    if compression_findings:
        findings.append(f"{len(compression_findings)} resource(s) served uncompressed")
    if exposed_source_maps:
        findings.append(
            f"{len(exposed_source_maps)} source map(s) fetchable in production, "
            "exposing original source, internal paths, and sometimes API endpoints"
        )
    if debug_code:
        findings.append(
            f"{len(debug_code)} minified file(s) still ship debug code "
            "(console/debugger/alert) that costs main-thread time or halts "
            "execution for anyone with devtools open"
        )
    if document_write:
        findings.append(
            f"{len(document_write)} script(s) call document.write, which blocks the "
            "parser and is ignored outright by Chrome on a slow connection"
        )
    if import_chains:
        chained = sum(1 for c in import_chains if c["depth"] > 1)
        detail = f", {chained} at least two levels deep" if chained else ""
        findings.append(
            f"{len(import_chains)} @import chain(s) each serializing a round trip "
            f"before styles apply{detail}"
        )

    return {
        "ok": True,
        "url": url,
        "final_url": final_url,
        "resources": resources,
        "resources_truncated": truncated,
        "total_bytes": total_bytes,
        "render_blocking": render_blocking,
        "oversized": oversized,
        "duplicate_libraries": duplicates,
        "unminified": unminified,
        "missing_font_display": missing_font_display,
        "legacy_js": legacy_js,
        "cache_findings": cache_findings,
        "compression_findings": compression_findings,
        "exposed_source_maps": exposed_source_maps,
        "debug_code": debug_code,
        "document_write": document_write,
        "css_import_chains": import_chains,
        "findings": findings,
        "skipped": [
            {
                "check": "unused_css_js",
                "reason": "needs a rendered DOM to tell loaded from used (tracked in #18)",
            },
            {
                "check": "site_median_outlier",
                "reason": "needs more than one page; call flag_outlier_pages() with each "
                "page's total_bytes",
            },
        ],
    }
