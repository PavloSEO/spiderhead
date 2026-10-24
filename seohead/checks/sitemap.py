"""Bounded sitemap crawler and parser.

Recursively fetches a sitemap URL, following ``<sitemapindex>`` entries down to child
``<urlset>`` documents, and collects every ``<url>`` entry.

The crawler understands:

* gzip-compressed sitemaps (``*.gz`` files and ``Content-Encoding: gzip``),
* nested sitemap indexes (index -> child sitemaps -> urlsets),
* XML namespaces (they are stripped before matching element names),
* sitemap extensions inside ``<url>`` entries — ``image:``, ``news:``, ``video:``
  and ``xhtml:link`` hreflang alternates — retained on the entry and counted
  separately from the page (``loc``) entries (#1560),
* duplicate ``loc`` values across sitemaps (reported, not re-emitted),
* a global cap on the number of URLs collected (truncation is flagged).

Public API
----------
``crawl(url, concurrency=3) -> dict``
    Fetch and recursively expand a sitemap tree. Never raises: on failure it
    records an entry in the returned ``errors`` list.

``parse_sitemap(xml_bytes, base_url) -> dict``
    Pure helper that classifies a single sitemap document as an ``index`` or
    ``urlset`` and extracts its entries. No network access.

Depends only on the Python stdlib plus ``httpx`` and ``lxml``.
"""

from __future__ import annotations

import gzip
import io
import re
import sqlite3
import tempfile
from collections.abc import Callable, Iterable
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing, nullcontext
from urllib.parse import urljoin, urlsplit, urlunsplit

import httpx
from lxml import etree

from seohead.recon.net import http_client

__all__ = [
    "classify_url_type",
    "crawl",
    "detect_host_policy",
    "normalize_url",
    "parse_sitemap",
    "strip_www",
]

# ── Limits ──────────────────────────────────────────────────────────────────
MAX_SITEMAPS = 5000
# Native crawl admission is one million URLs.  A sitemap index may distribute
# that population across protocol-valid 50,000-URL child documents; this is a
# crawler-wide retained-member ceiling, not a per-file sitemap protocol limit.
MAX_URLS = 1_000_000
MAX_XML_BYTES = 12 * 1024 * 1024
TIMEOUT_S = 25.0
MAX_REDIRECTS = 8

_GZIP_MAGIC = b"\x1f\x8b"
_GUNZIP_CHUNK = 64 * 1024  # read granularity for the bounded gunzip loop below
# One message for "too large", raised whichever stage (compressed or decompressed) catches
# it, so a caller sees one consistent failure rather than branching on which check fired.
_TOO_LARGE_MSG = f"Response too large (> {MAX_XML_BYTES} bytes)"


class _StreamingDeduper:
    """Per-selected-root normalized dedup without an in-memory URL map."""

    def __init__(self, sink: Callable[[dict], None]) -> None:
        self._temporary = tempfile.TemporaryDirectory(prefix="seohead-sitemap-")
        self._con = sqlite3.connect(f"{self._temporary.name}/keys.sqlite")
        self._con.execute("PRAGMA temp_store=FILE")
        self._con.execute("PRAGMA cache_size=-8192")
        self._con.execute("CREATE TABLE keys (normalized TEXT PRIMARY KEY)")
        self._sink = sink
        self.count = 0
        self.duplicates = 0

    def add(self, entry: dict, normalized: str) -> bool:
        try:
            self._con.execute("INSERT INTO keys(normalized) VALUES (?)", (normalized,))
        except sqlite3.IntegrityError:
            self.duplicates += 1
            return False
        try:
            payload = {
                "loc": entry["loc"],
                "loc_normalized": normalized,
                "lastmod": entry.get("lastmod"),
                "changefreq": entry.get("changefreq"),
                "priority": entry.get("priority"),
            }
            # Extensions ride along when present (#1560); the five base keys above
            # keep their exact shape for every entry, extension or not.
            for name in ("images", "news", "videos", "alternates"):
                if entry.get(name):
                    payload[name] = entry[name]
            self._sink(payload)
        except Exception:
            self.close()
            raise
        self.count += 1
        return True

    def close(self) -> None:
        self._con.close()
        self._temporary.cleanup()


# ── Pure helpers ────────────────────────────────────────────────────────────
def strip_www(host: str) -> str:
    """Return *host* without a leading ``www.`` label."""
    return host.removeprefix("www.")


def normalize_url(url_str: str) -> str:
    """Canonicalise a URL for de-duplication.

    Lower-cases the host, drops default ports (80/443), removes the fragment,
    and strips a trailing slash from non-root paths. Raises ``ValueError`` if
    the string has no scheme or host (mirrors the TS ``new URL`` behaviour of
    rejecting non-absolute inputs).
    """
    parts = urlsplit(url_str.strip())
    if not parts.scheme or not parts.netloc:
        raise ValueError(f"Invalid URL: {url_str!r}")

    scheme = parts.scheme.lower()
    host = (parts.hostname or "").lower()
    port = parts.port
    if (scheme == "https" and port == 443) or (scheme == "http" and port == 80):
        port = None

    netloc = host
    if port is not None:
        netloc = f"{host}:{port}"
    if parts.username:
        userinfo = parts.username
        if parts.password:
            userinfo += f":{parts.password}"
        netloc = f"{userinfo}@{netloc}"

    path = parts.path
    if len(path) > 1 and path.endswith("/"):
        path = re.sub(r"/+$", "", path)

    # Fragment intentionally dropped; query preserved.
    return urlunsplit((scheme, netloc, path, parts.query, ""))


def _normalized_index(urls: Iterable[str]) -> dict[str, str]:
    """Normalised key -> the URL as it was actually written, first occurrence wins.

    Comparison has to happen on the normalised key, or a canonical written without a trailing
    slash would never match the page that has one. Reporting has to happen on the original,
    or a finding names a URL that appears nowhere in the crawl — which is both unactionable
    and indistinguishable, to a reader or to the anomaly scanner, from a finding about a page
    that was never fetched.
    """
    out: dict[str, str] = {}
    for url in urls:
        if not url:
            continue
        try:
            out.setdefault(normalize_url(url), url)
        except ValueError:
            continue  # not an absolute URL; cannot be compared, so it is dropped
    return out


def classify_url_type(url_str: str) -> str:
    """Classify a URL as a ``"folder"`` or a ``"finalUrl"``.

    The root path is a folder; otherwise the last path segment decides: a dot
    in the leaf (e.g. ``page.html``) marks a final URL, everything else a
    folder. On a malformed URL the original code defaults to ``"finalUrl"``.
    """
    try:
        parts = urlsplit(url_str)
        if not parts.scheme or not parts.netloc:
            return "finalUrl"
        path = parts.path or "/"
        if path == "/":
            return "folder"
        segments = [s for s in path.split("/") if s]
        leaf = segments[-1] if segments else ""
        return "finalUrl" if "." in leaf else "folder"
    except Exception:
        return "finalUrl"


def detect_host_policy(urls: list[dict]) -> dict:
    """Summarise the www vs non-www split across collected URLs.

    Returns ``{dominant, mixed, counts: {www, non_www}, by_host}`` where
    ``dominant`` is ``"www"`` when www hosts are at least as common as non-www
    (ties favour www, matching the TS ``>=``), and ``mixed`` is true when both
    styles appear.
    """
    counts = {"www": 0, "non_www": 0}
    by_host: dict[str, int] = {}
    for u in urls:
        try:
            host = (urlsplit(u["loc"]).hostname or "").lower()
        except Exception:
            continue
        if not host:
            continue
        if host.startswith("www."):
            counts["www"] += 1
        else:
            counts["non_www"] += 1
        by_host[host] = by_host.get(host, 0) + 1
    dominant = "www" if counts["www"] >= counts["non_www"] else "non_www"
    mixed = counts["www"] > 0 and counts["non_www"] > 0
    return {
        "dominant": dominant,
        "mixed": mixed,
        "counts": counts,
        "by_host": list(by_host.items()),
    }


def _local_name(tag: object) -> str:
    """Return an element's tag name with any XML namespace stripped.

    A declared namespace arrives as ``{uri}name``; an *undeclared* prefix (which
    the recovering parser tolerates rather than rejects — generators do emit
    these) arrives raw as ``prefix:name`` and is stripped the same way (#1560).
    """
    if not isinstance(tag, str):
        return ""
    return tag.rsplit("}", 1)[-1].rsplit(":", 1)[-1]


def _child_text(element: etree._Element, name: str) -> str | None:
    """Return the trimmed text of the first namespace-agnostic child *name*."""
    for child in element:
        if _local_name(child.tag) == name:
            text = child.text
            return text.strip() if text else None
    return None


def _parse_priority(value: str | None) -> float | None:
    """Parse a ``<priority>`` value into a float, or ``None`` if unparseable."""
    if value is None:
        return None
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


# ── Sitemap extensions inside <url> (#1560) ─────────────────────────────────
# Simple text children retained per extension element. Namespaces are matched
# on local names because generators emit these extensions under whatever prefix
# they chose (Yoast, Yandex and hand-rolled exporters all differ), and neither
# the sitemap protocol nor the extensions define a mandatory prefix.
_IMAGE_FIELDS = ("loc", "title", "caption", "geo_location", "license")
_NEWS_FIELDS = ("name", "publication_date", "title")
_VIDEO_FIELDS = (
    "thumbnail_loc",
    "title",
    "description",
    "player_loc",
    "duration",
    "expiration_date",
    "rating",
    "view_count",
    "publication_date",
    "family_friendly",
    "requires_subscription",
    "live",
    "tag",
    "category",
    "gallery_loc",
    "uploader",
)


def _text_fields(element: etree._Element, names: tuple[str, ...]) -> dict[str, str]:
    """Collect the non-empty text children of *element* for the given names."""
    record: dict[str, str] = {}
    for name in names:
        value = _child_text(element, name)
        if value:
            record[name] = value
    return record


def _extension_records(url_el: etree._Element, base_url: str) -> dict[str, object]:
    """Return the extension content of one ``<url>`` entry, keyed for the entry dict.

    Only keys with content are returned, so an entry without extensions keeps the
    exact ``{loc, lastmod, changefreq, priority}`` shape it always had. Relative
    ``image:loc`` values resolve against *base_url* like page locations do.
    """
    images: list[dict[str, str]] = []
    news: dict[str, str] | None = None
    videos: list[dict[str, str]] = []
    alternates: list[dict[str, str]] = []
    for child in url_el:
        name = _local_name(child.tag)
        if name == "image":
            record = _text_fields(child, _IMAGE_FIELDS)
            if record.get("loc"):
                record["loc"] = urljoin(base_url, record["loc"])
                images.append(record)
        elif name == "news":
            record = _text_fields(child, _NEWS_FIELDS)
            if not record.get("name"):
                # Google News nests <name> inside <news:publication>.
                for sub in child:
                    if _local_name(sub.tag) == "publication":
                        name_text = _child_text(sub, "name")
                        if name_text:
                            record["name"] = name_text
                        break
            if record:
                news = record
        elif name == "video":
            record = _text_fields(child, _VIDEO_FIELDS)
            if record:
                videos.append(record)
        elif name == "link":
            rel = (child.get("rel") or "").strip().lower()
            hreflang = (child.get("hreflang") or "").strip()
            href = (child.get("href") or "").strip()
            if rel == "alternate" and hreflang and href:
                alternates.append({"hreflang": hreflang, "href": urljoin(base_url, href)})
    out: dict[str, object] = {}
    if images:
        out["images"] = images
    if news is not None:
        out["news"] = news
    if videos:
        out["videos"] = videos
    if alternates:
        out["alternates"] = alternates
    return out


def parse_text_sitemap(text_bytes: bytes, base_url: str) -> dict:
    """Parse a plain-text sitemap with one URL per line and ``#`` comments.

    The sitemaps.org specification permits ``.txt`` sitemaps in which every
    non-empty line is an absolute URL. Relative references are resolved against
    ``base_url``; blank lines and comments are ignored. Pure; no network access.
    """
    # A byte-order mark must not become part of the first URL (#1560): some
    # generators write .txt sitemaps with a BOM, and urljoin would happily
    # produce a location that exists nowhere on the site.
    if isinstance(text_bytes, str):  # tolerate a caller that already decoded
        text = text_bytes
    elif text_bytes.startswith((b"\xff\xfe", b"\xfe\xff")):
        text = text_bytes.decode("utf-16", errors="replace")
    else:
        text = text_bytes.decode("utf-8-sig", errors="replace")
    urls: list[dict] = []
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        try:
            absolute = urljoin(base_url, line)
        except ValueError:
            continue
        # Keep only HTTP(S); plain-text sitemap files sometimes contain noise.
        if not absolute.startswith(("http://", "https://")):
            continue
        urls.append({"loc": absolute, "lastmod": None, "changefreq": None, "priority": None})
    return {"ok": True, "type": "text", "sitemaps": [], "urls": urls}


def parse_sitemap(xml_bytes: bytes, base_url: str) -> dict:
    """Classify and extract a single sitemap document. Pure; no network.

    Parameters
    ----------
    xml_bytes:
        Raw (already decompressed) XML bytes.
    base_url:
        URL the document was fetched from; used to resolve any relative
        ``<loc>`` values against an absolute base.

    Returns a dict of the form::

        {
            "ok": True,
            "type": "index" | "urlset" | "text" | "unknown",
            "sitemaps": [{"loc", "lastmod"}, ...],
            "urls": [{"loc", "lastmod", "changefreq", "priority"}, ...],
        }

    ``urlset`` entries that carry sitemap extensions additionally hold the keys
    ``images`` (``image:loc``/``title``/``caption``/``geo_location``/``license``),
    ``news`` (``name``/``publication_date``/``title``), ``videos`` (the video
    extension's simple child tags) and ``alternates`` (``xhtml:link
    rel="alternate" hreflang`` pairs); the keys appear only when the entry has
    that content, so plain sitemaps keep the base shape above (#1560).
    ``lastmod`` is always retained verbatim — W3C dates, full datetimes with a
    timezone and RFC3339 values all survive unchanged.

    Plain-text sitemaps (one URL per line, optional ``#`` comments) are detected
    when the body does not look like XML and recognised as ``type: "text"``.

    On a parse error it returns ``{"ok": False, "error": <message>, ...}`` with
    empty ``sitemaps``/``urls`` rather than raising.
    """
    # Plain-text sitemap: one URL per line. A body without '<' is not XML.
    head = xml_bytes.lstrip()[:512]
    if head and b"<" not in head:
        return parse_text_sitemap(xml_bytes, base_url)

    try:
        # recover=True tolerates stray bytes / malformed markup, like the
        # lenient JS parser. huge_tree lifts libxml2's default node limits.
        parser = etree.XMLParser(recover=True, huge_tree=True, resolve_entities=False)
        root = etree.fromstring(xml_bytes, parser=parser)
    except (etree.XMLSyntaxError, ValueError) as exc:
        # A boundary case may still be non-XML text; fall back when '<' is absent.
        if b"<" not in (xml_bytes[:512]):
            return parse_text_sitemap(xml_bytes, base_url)
        return {
            "ok": False,
            "error": f"XML parse error: {exc}",
            "type": "unknown",
            "sitemaps": [],
            "urls": [],
        }

    if root is None:
        return {
            "ok": False,
            "error": "XML parse error: empty document",
            "type": "unknown",
            "sitemaps": [],
            "urls": [],
        }

    root_name = _local_name(root.tag)

    if root_name == "sitemapindex":
        sitemaps: list[dict] = []
        for child in root:
            if _local_name(child.tag) != "sitemap":
                continue
            loc = _child_text(child, "loc")
            if not loc:
                continue
            sitemaps.append(
                {
                    "loc": urljoin(base_url, loc),
                    "lastmod": _child_text(child, "lastmod"),
                }
            )
        return {"ok": True, "type": "index", "sitemaps": sitemaps, "urls": []}

    if root_name == "urlset":
        urls: list[dict] = []
        for child in root:
            if _local_name(child.tag) != "url":
                continue
            loc = _child_text(child, "loc")
            if not loc:
                continue
            urls.append(
                {
                    "loc": urljoin(base_url, loc),
                    "lastmod": _child_text(child, "lastmod"),
                    "changefreq": _child_text(child, "changefreq"),
                    "priority": _parse_priority(_child_text(child, "priority")),
                    **_extension_records(child, base_url),
                }
            )
        return {"ok": True, "type": "urlset", "sitemaps": [], "urls": urls}

    return {"ok": True, "type": "unknown", "sitemaps": [], "urls": []}


# ── Fetching ────────────────────────────────────────────────────────────────
def _maybe_gunzip(url: str, body: bytes) -> bytes:
    """Decompress *body* if it is a ``.gz`` sitemap or has a gzip magic header.

    Reads through the decompressor in ``_GUNZIP_CHUNK``-sized steps and aborts as soon as
    the output exceeds :data:`MAX_XML_BYTES`, rather than materialising the whole stream
    first: ``gzip.decompress()`` has no size bound of its own, so a 2 MB payload can expand
    to 2 GiB in under two seconds regardless of the pre-decompression size check in
    :func:`_fetch` (issue #148). zlib's deflate format tops out around a 1032:1 compression
    ratio (https://www.zlib.net/zlib_tech.html), so bounding the *output* at
    ``MAX_XML_BYTES`` bounds the peak extra memory this can cost to about one document's
    worth of XML, independent of how the compressed bytes were crafted -- a compressed input
    already capped well under that ratio's worth of ``MAX_XML_BYTES`` can never reach the
    ceiling before ``_GzipFile.read()`` itself runs out of bytes to expand.

    Mirrors the bounded-read loop ``seohead/sf/core/sitemap_coverage.py``'s ``_safe_gunzip``
    already uses for its own, independent sitemap fetch -- the two modules don't share code
    across the tools/sf layering boundary, but the bound is the same one either way.

    ``httpx`` already handles the ``Content-Encoding`` header transparently, so this only
    needs to catch gzipped *payloads* (``.xml.gz`` files, or servers that gzip the body
    without advertising it).
    """
    path = urlsplit(url).path.lower()
    looks_gzip = body[:2] == _GZIP_MAGIC
    if not (path.endswith(".gz") or looks_gzip):
        return body
    out = bytearray()
    try:
        with gzip.GzipFile(fileobj=io.BytesIO(body)) as gz:
            while True:
                chunk = gz.read(_GUNZIP_CHUNK)
                if not chunk:
                    break
                out += chunk
                if len(out) > MAX_XML_BYTES:
                    raise ValueError(_TOO_LARGE_MSG)
    except (OSError, EOFError, gzip.BadGzipFile):
        # Not actually gzip (or truncated): fall back to the raw bytes, same as before.
        return body
    return bytes(out)


def _fetch(client: httpx.Client, url: str) -> bytes:
    """Fetch *url* and return its (decompressed) body bytes.

    Raises ``httpx.HTTPError`` on transport failure or a non-2xx status, and
    ``ValueError`` if the body exceeds :data:`MAX_XML_BYTES` either compressed or, once
    decompressed, in :func:`_maybe_gunzip`.
    """
    resp = client.get(
        url,
        headers={"Accept": "application/xml,text/xml,application/gzip,*/*"},
    )
    resp.raise_for_status()
    body = resp.content
    if len(body) > MAX_XML_BYTES:
        raise ValueError(_TOO_LARGE_MSG)
    return _maybe_gunzip(str(resp.url), body)


# ── Crawl ───────────────────────────────────────────────────────────────────
def crawl(
    url: str,
    concurrency: int = 3,
    *,
    sink: Callable[[dict], None] | None = None,
    request_gate: Callable[[], None] | None = None,
    proxy_route=None,
) -> dict:
    """Recursively crawl a sitemap tree starting at *url*.

    Follows sitemap-index documents down to their child sitemaps and collects
    every ``<url>`` entry, de-duplicating by normalized ``loc``. A site-root
    URL first reads ``robots.txt`` for ``Sitemap:`` directives, then tries the
    conventional ``/sitemap.xml`` fallback when none are usable. Child sitemaps
    within a level are fetched in parallel using up to *concurrency* threads.

    Transport, HTTP and parse failures are recorded in the returned ``errors``
    list. A caller-supplied sink streams accepted URLs instead of returning
    their list; sink/storage failures propagate so incomplete writes cannot
    look like a successful expansion.

    Returns
    -------
    dict
        ``{"ok", "root", "count", "urls", "sitemaps", "duplicates",
        "errors", "truncated"}`` where:

        * ``root`` — the normalized starting URL,
        * ``count`` — number of unique URLs collected,
        * ``urls`` — ``[{"loc", "lastmod", "changefreq", "priority"}, ...]``,
          with ``images``/``news``/``videos``/``alternates`` attached to entries
          that carry sitemap extensions (#1560),
        * ``sitemaps`` — per-document ``{"url", "type", "count"}`` records,
        * ``duplicates`` — ``loc`` values seen more than once,
        * ``errors`` — ``{"url", "error"}`` records,
        * ``truncated`` — ``True`` if a hard limit stopped collection,

        plus page/attachment counters kept strictly apart (#1560):
        ``loc_entries`` (page ``<loc>`` entries — the same number as ``count``),
        ``image_entries``, ``news_entries`` and ``video_entries`` (extension
        attachments on the accepted entries).
    """
    try:
        root = normalize_url(url)
    except ValueError as exc:
        return {
            "ok": False,
            "root": url,
            "count": 0,
            "urls": [],
            "sitemaps": [],
            "duplicates": [],
            "errors": [{"url": url, "error": str(exc)}],
            "truncated": False,
            "loc_entries": 0,
            "image_entries": 0,
            "news_entries": 0,
            "video_entries": 0,
        }

    concurrency = max(1, int(concurrency))
    visited: set[str] = set()
    root_parts = urlsplit(root)
    discover_from_root = root_parts.path in ("", "/") and not root_parts.query
    queued: set[str] = set()
    frontier: list[str] = []
    source_of: dict[str, str] = {}

    sitemaps: list[dict] = []
    errors: list[dict] = []
    seen_locs: dict[str, str] = {}
    duplicates: list[str] = []
    # A URL declared in two child sitemaps is usually a generator that ran twice, and it
    # distorts every count derived from the declared set. Counting them says it happened;
    # naming both documents says where to look.
    duplicate_sources: dict[str, list[str]] = {}
    all_urls: list[dict] = []
    truncated = False
    parsed_documents = 0
    discovery_errors: list[dict] = []
    # Extension attachments on accepted entries, counted separately from the page
    # (``loc``) population so a crawl-vs-sitemap comparison can never mix the two:
    # a sitemap with 1,283 pages and 1,130 image:loc values is two numbers, not
    # "2,413 URLs" (#1560).
    extension_counts = {"image": 0, "news": 0, "video": 0}

    def _count_extensions(entry: dict) -> None:
        extension_counts["image"] += len(entry.get("images") or [])
        extension_counts["news"] += 1 if entry.get("news") else 0
        extension_counts["video"] += len(entry.get("videos") or [])

    def enqueue(target: str, source: str) -> None:
        if target not in visited and target not in queued:
            frontier.append(target)
            queued.add(target)
            source_of[target] = source

    def process(target: str) -> dict:
        """Fetch and parse one sitemap; return a result record (no shared state)."""
        try:
            body = _fetch(client, target)
        except Exception as exc:
            return {"kind": "error", "url": target, "error": _err_message(exc)}
        parsed = parse_sitemap(body, target)
        if not parsed.get("ok"):
            return {"kind": "error", "url": target, "error": parsed.get("error", "parse error")}
        # Measured after decompression, because the protocol's 50 MB limit is about the document
        # a search engine has to parse, not about what travelled over the wire.
        return {"kind": "parsed", "url": target, "parsed": parsed, "bytes": len(body)}

    limits = httpx.Limits(max_connections=concurrency)
    options = {
        "follow_redirects": True,
        "max_redirects": MAX_REDIRECTS,
        "limits": limits,
    }
    if request_gate is not None:
        # httpx invokes request hooks for every hop it follows itself.  Calling
        # the gate here rather than in _fetch means redirects reserve the same
        # turn as the selected root and nested sitemap documents.
        options["event_hooks"] = {"request": [lambda _request: request_gate()]}
    from seohead.recon.net import crawl_transport_options

    client, _http2_capable = http_client(
        TIMEOUT_S, **crawl_transport_options(proxy_route), **options
    )
    with (
        client,
        closing(_StreamingDeduper(sink)) if sink is not None else nullcontext() as streaming,
        ThreadPoolExecutor(max_workers=concurrency) as pool,
    ):
        if discover_from_root:
            robots_url = urlunsplit((root_parts.scheme, root_parts.netloc, "/robots.txt", "", ""))
            discovered: list[str] = []
            try:
                from seohead.checks.robots import parse_robots

                parsed_robots = parse_robots(_fetch(client, robots_url).decode("utf-8", "replace"))
                for declared in parsed_robots["sitemaps"]:
                    try:
                        discovered.append(normalize_url(urljoin(root, declared)))
                    except ValueError as exc:
                        discovery_errors.append({"url": declared, "error": str(exc)})
            except Exception as exc:
                discovery_errors.append({"url": robots_url, "error": _err_message(exc)})
            for target in dict.fromkeys(discovered):
                enqueue(target, "robots.txt")
            if not frontier:
                fallback = urlunsplit(
                    (root_parts.scheme, root_parts.netloc, "/sitemap.xml", "", "")
                )
                enqueue(fallback, "sitemap.xml fallback")
        else:
            enqueue(root, "explicit")
        while frontier and not truncated:
            if len(visited) >= MAX_SITEMAPS:
                truncated = True
                break

            batch = [u for u in frontier if u not in visited]
            frontier = []
            for u in batch:
                visited.add(u)
                queued.discard(u)

            for result in pool.map(process, batch):
                if truncated:
                    # The cap already tripped on an earlier document in this same
                    # frontier batch. pool.map() has no way to cancel the results
                    # still queued behind it, so we must keep draining the
                    # iterator but stop appending anything it yields — otherwise
                    # every remaining document in the batch adds at least one
                    # more URL before its own inner-loop check ever fires, and
                    # `count` overshoots MAX_URLS (#473).
                    break

                target = result["url"]
                if result["kind"] == "error":
                    errors.append({"url": target, "error": result["error"]})
                    continue

                parsed = result["parsed"]
                if parsed["type"] == "index":
                    parsed_documents += 1
                    children = parsed["sitemaps"]
                    sitemaps.append(
                        {
                            "url": target,
                            "source": source_of.get(target, "sitemap index"),
                            "type": "index",
                            "count": len(children),
                            "bytes": result.get("bytes", 0),
                            "declared": len(children),
                        }
                    )
                    for child in children:
                        try:
                            norm = normalize_url(child["loc"])
                        except ValueError:
                            continue
                        enqueue(norm, target)
                elif parsed["type"] in ("urlset", "text"):
                    parsed_documents += 1
                    added = 0
                    for entry in parsed["urls"]:
                        try:
                            norm_loc = normalize_url(entry["loc"])
                        except ValueError:
                            continue
                        if streaming is not None:
                            if (
                                streaming.count >= MAX_URLS
                                and not streaming._con.execute(
                                    "SELECT 1 FROM keys WHERE normalized=?", (norm_loc,)
                                ).fetchone()
                            ):
                                truncated = True
                                break
                            if not streaming.add(entry, norm_loc):
                                continue
                            added += 1
                            _count_extensions(entry)
                            continue
                        if norm_loc in seen_locs:
                            duplicates.append(norm_loc)
                            sources = duplicate_sources.setdefault(norm_loc, [seen_locs[norm_loc]])
                            if target not in sources:
                                sources.append(target)
                            continue
                        if len(all_urls) >= MAX_URLS:
                            truncated = True
                            break
                        seen_locs[norm_loc] = target
                        stored = {
                            # The address the sitemap published, not the normalised key.
                            # Normalisation is right for deciding whether two entries are
                            # the same URL and wrong for deciding what to request: a
                            # sitemap declaring /a/ must not cause a fetch of /a, which on
                            # most CMSes is a 301 the crawler invented for itself (#115).
                            "loc": entry["loc"],
                            # Kept beside it so every consumer that was comparing on the
                            # normalised form still can, without re-deriving it.
                            "loc_normalized": norm_loc,
                            "lastmod": entry.get("lastmod"),
                            "changefreq": entry.get("changefreq"),
                            "priority": entry.get("priority"),
                        }
                        for name in ("images", "news", "videos", "alternates"):
                            if entry.get(name):
                                stored[name] = entry[name]
                        all_urls.append(stored)
                        added += 1
                        _count_extensions(entry)
                    sitemaps.append(
                        {
                            "url": target,
                            "source": source_of.get(target, "sitemap index"),
                            "type": parsed["type"],
                            # Added to the run after de-duplication and the global budget.
                            "count": added,
                            # What this one document declares, before either — the number the
                            # protocol's 50,000-URL limit is actually about.
                            "declared": len(parsed["urls"]),
                            "bytes": result.get("bytes", 0),
                        }
                    )
                else:
                    errors.append({"url": target, "error": "Unknown sitemap format"})

    count = streaming.count if streaming is not None else len(all_urls)
    duplicate_count = streaming.duplicates if streaming is not None else len(duplicates)
    if not parsed_documents and discovery_errors:
        errors = [*discovery_errors, *errors]
    result = {
        "ok": bool(parsed_documents),
        "root": root,
        "count": count,
        "urls": [] if sink is not None else all_urls,
        "sitemaps": sitemaps,
        "duplicates": [] if sink is not None else duplicates,
        "duplicate_sources": {} if sink is not None else duplicate_sources,
        "errors": errors,
        "truncated": truncated,
        # Pages and extension attachments are counted separately (#1560): the
        # comparison consumers of this result must never mix the two populations.
        "loc_entries": count,
        "image_entries": extension_counts["image"],
        "news_entries": extension_counts["news"],
        "video_entries": extension_counts["video"],
    }
    if discover_from_root:
        result["discovery"] = {
            "robots_url": urlunsplit((root_parts.scheme, root_parts.netloc, "/robots.txt", "", "")),
            "fallback": urlunsplit((root_parts.scheme, root_parts.netloc, "/sitemap.xml", "", "")),
        }
    if sink is not None:
        result["duplicate_count"] = duplicate_count
    return result


def _err_message(exc: Exception) -> str:
    """Render an exception as a concise, HTTP-aware message string."""
    if isinstance(exc, httpx.HTTPStatusError):
        return f"HTTP {exc.response.status_code}"
    if isinstance(exc, httpx.TimeoutException):
        return "Timeout"
    msg = str(exc).strip()
    return msg or exc.__class__.__name__


# ── Smoke test (no network) ─────────────────────────────────────────────────
if __name__ == "__main__":
    index_xml = b"""<?xml version="1.0" encoding="UTF-8"?>
    <sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
      <sitemap><loc>https://example.com/sitemap-1.xml</loc><lastmod>2024-01-01</lastmod></sitemap>
      <sitemap><loc>/sitemap-2.xml</loc></sitemap>
    </sitemapindex>"""
    idx = parse_sitemap(index_xml, "https://example.com/sitemap.xml")
    assert idx["type"] == "index", idx
    assert idx["sitemaps"][0]["loc"] == "https://example.com/sitemap-1.xml"
    assert idx["sitemaps"][1]["loc"] == "https://example.com/sitemap-2.xml", idx

    urlset_xml = b"""<?xml version="1.0" encoding="UTF-8"?>
    <urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
      <url><loc>https://example.com/a.html</loc><priority>0.8</priority><changefreq>daily</changefreq></url>
      <url><loc>https://example.com/blog/</loc><lastmod>2024-02-02</lastmod></url>
      <url><loc>bad</loc></url>
    </urlset>"""
    us = parse_sitemap(urlset_xml, "https://example.com/sitemap.xml")
    assert us["type"] == "urlset", us
    assert us["urls"][0]["priority"] == 0.8, us
    assert us["urls"][0]["changefreq"] == "daily"
    assert us["urls"][2]["loc"] == "https://example.com/bad"  # resolved against base

    assert normalize_url("https://Example.com:443/path/") == "https://example.com/path"
    assert classify_url_type("https://example.com/page.html") == "finalUrl"
    assert classify_url_type("https://example.com/blog") == "folder"
    assert classify_url_type("https://example.com/") == "folder"
    assert strip_www("www.example.com") == "example.com"

    policy = detect_host_policy(
        [{"loc": "https://www.example.com/a"}, {"loc": "https://example.com/b"}]
    )
    assert policy["mixed"] is True, policy
    assert policy["counts"] == {"www": 1, "non_www": 1}

    bad = parse_sitemap(b"<<not xml", "https://example.com/")
    assert bad["type"] in ("unknown", "urlset", "index"), bad  # recover parser is lenient

    print("sitemap.py self-check OK")
