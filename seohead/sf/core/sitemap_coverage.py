"""Sitemap and robots.txt audit.

Two independent sources, merged: (1) Screaming Frog's native ``Sitemaps:*``
exports when present, and (2) a direct parse of robots.txt / sitemap.xml (with
sitemap-index recursion and gzip) over the network. The direct parse uses only
the stdlib; ``advertools`` is used opportunistically if installed but is never
required. Network access is opt-in (``--sitemap`` or ``live_recheck.enabled``).
"""

from __future__ import annotations

import gzip
import io
import re
import statistics
import time
import urllib.parse
from collections import Counter
from collections.abc import Callable
from datetime import datetime, timezone
from typing import Any

from defusedxml import ElementTree as ET

from seohead.checks.sitemap import _normalized_index, normalize_url
from seohead.recon.net import UA, http_client, validate_url

from .context import AuditContext
from .normalize import find_column, normalize_value

SITEMAP_DIRECTIVE = re.compile(r"^\s*sitemap:\s*(\S+)", re.IGNORECASE | re.MULTILINE)

# Hardening bounds for the opt-in network parser.
MAX_DOWNLOAD_BYTES = 50 * 1024 * 1024  # cap a single download
MAX_DECOMPRESSED_BYTES = 100 * 1024 * 1024  # cap gunzip output (gzip-bomb guard)
MAX_SITEMAP_DEPTH = 5  # sitemapindex recursion depth
MAX_SITEMAP_URLS = 200_000  # total <loc> across the chain


def _host(url: str) -> str:
    try:
        return urllib.parse.urlparse(url).netloc.lower()
    except (ValueError, AttributeError):
        return ""


def _local_children(element: Any, name: str) -> list[Any]:
    """Direct children whose local name is *name*, whatever wraps it.

    Generators emit sitemaps under non-standard prefixes and occasionally under
    a mistyped namespace URI, so elements are matched on the local name alone —
    the same convention ``seohead/checks/sitemap.py`` already uses for the same
    document shape (#1560).
    """
    return [
        child
        for child in element
        if isinstance(child.tag, str) and child.tag.rsplit("}", 1)[-1].rsplit(":", 1)[-1] == name
    ]


def _local_text(element: Any, name: str) -> str | None:
    """Trimmed text of the first local-name child *name*, or ``None``."""
    for child in _local_children(element, name):
        text = child.text
        return text.strip() if text else None
    return None


# --------------------------------------------------------------------------
# network helpers (opt-in)
# --------------------------------------------------------------------------
def _safe_gunzip(data: bytes) -> bytes:
    out = bytearray()
    with gzip.GzipFile(fileobj=io.BytesIO(data)) as gz:
        while True:
            chunk = gz.read(65536)
            if not chunk:
                break
            out += chunk
            if len(out) > MAX_DECOMPRESSED_BYTES:
                raise ValueError("decompressed sitemap exceeds size limit")
    return bytes(out)


def _fetch(
    url: str,
    user_agent: str,
    timeout: int,
    retries: int = 2,
    *,
    request_gate: Callable[[], None] | None = None,
    proxy_route=None,
) -> bytes | None:
    if not url.lower().startswith(("http://", "https://")):  # no file://, ftp://, etc.
        return None
    try:
        validate_url(url)
    except ValueError:
        return None
    # Retry transient failures so a flaky host doesn't silently drop sitemap subtrees.
    for attempt in range(retries + 1):
        try:
            options = {"follow_redirects": True, "headers": {"User-Agent": user_agent}}
            if request_gate is not None:
                # The hook runs for every automatic redirect; each retry owns
                # a fresh client and therefore reserves a fresh request turn.
                options["event_hooks"] = {"request": [lambda _request: request_gate()]}
            from seohead.recon.net import crawl_transport_options

            client, _http2_capable = http_client(
                timeout, **crawl_transport_options(proxy_route), **options
            )
            data = bytearray()
            with client, client.stream("GET", url) as response:
                response.raise_for_status()
                for chunk in response.iter_raw():
                    data += chunk
                    if len(data) > MAX_DOWNLOAD_BYTES:
                        return None
            data = bytes(data)
            if len(data) > MAX_DOWNLOAD_BYTES:
                return None
            if url.endswith(".gz") or data[:2] == b"\x1f\x8b":
                data = _safe_gunzip(data)
            return data
        except Exception:
            if attempt < retries:
                time.sleep(0.6 * (attempt + 1))
    return None


def _parse_sitemap_bytes(
    data: bytes,
    user_agent: str,
    timeout: int,
    seen: set[str],
    allowed_hosts: set[str],
    depth: int = 0,
    failures: list[str] | None = None,
    documents: list[dict[str, Any]] | None = None,
    source: str = "",
    truncated: list[str] | None = None,
    request_gate: Callable[[], None] | None = None,
    proxy_route=None,
) -> list[dict[str, Any]]:
    """Return [{loc, lastmod}], recursing through <sitemapindex> with guards.

    Child sitemaps that can't be fetched *or parsed* are appended to ``failures`` (so the
    caller can report a partial parse instead of silently undercounting) — a document that
    came back 200 with an unescaped ``&`` or a stray DOCTYPE is not the same fact as "no
    sitemap exists here", and reporting it as one is worse than naming the failure (#146).

    ``truncated`` is the distinct case of the depth guard below: the index that hit
    ``MAX_SITEMAP_DEPTH`` fetched and parsed cleanly, it simply was not followed any
    deeper. That is neither "no sitemap exists" nor "this document is broken" — it is
    incomplete evidence of a different shape, and conflating it with either of the other
    two used to report a deeply-nested index as a fetched, genuinely empty sitemap (#312).

    This uses ``defusedxml``'s strict, non-recovering parser rather than the lenient,
    ``recover=True`` lxml parser ``seohead/checks/sitemap.py`` uses for the same document
    shape: that module reads a sitemap already vetted as this site's own declared list,
    where recovering a mangled entry is a straightforward win; this one runs the DTD/entity
    guard below on bytes that may come from a sitemap-index child on an operator-supplied
    host, and #148 is a reminder that lxml's own tolerant mode (``huge_tree=True``) already
    trades away a safeguard once — stacking a second, recovering parser on top of that on the
    less-trusted path is not a trade worth making silently. Two parsers giving two answers on
    the same bytes is still the underlying defect; the fix here is that a rejection is now
    named instead of swallowed, not that the two converge.

    ``documents``, when given, collects one record per sitemap document actually parsed:
    its URL, its uncompressed byte size and how many entries it declares. Those are the two
    numbers the sitemap protocol puts a hard limit on, and neither survives the flattening
    into a single list of locations — a search engine may take the first 50,000 entries of an
    over-long sitemap and discard the rest, silently, with no error the site owner can see.
    """
    # Reject DTDs/entities outright — sitemaps never use them (XXE / billion-laughs guard).
    if b"<!DOCTYPE" in data or b"<!ENTITY" in data:
        if failures is not None and source:
            failures.append(source)
        return []
    try:
        root = ET.fromstring(data)
    except ET.ParseError:
        if failures is not None and source:
            failures.append(source)
        return []
    tag = root.tag.rsplit("}", 1)[-1].rsplit(":", 1)[-1]
    out: list[dict[str, Any]] = []
    if tag == "sitemapindex":
        if depth >= MAX_SITEMAP_DEPTH:
            if truncated is not None and source:
                truncated.append(source)
            return []
        sitemap_elements = _local_children(root, "sitemap")
        for idx, sm in enumerate(sitemap_elements):
            loc = _local_text(sm, "loc")
            if not loc:
                continue
            loc = _resolve_sitemap_loc(loc.strip(), source)
            # SSRF guard: only follow children on an allowed host.
            if loc in seen or (allowed_hosts and _host(loc) not in allowed_hosts):
                continue
            seen.add(loc)
            fetch_kwargs = {"request_gate": request_gate} if request_gate is not None else {}
            if proxy_route is not None:
                fetch_kwargs["proxy_route"] = proxy_route
            child = _fetch(loc, user_agent, timeout, **fetch_kwargs)
            if child:
                out.extend(
                    _parse_sitemap_bytes(
                        child,
                        user_agent,
                        timeout,
                        seen,
                        allowed_hosts,
                        depth + 1,
                        failures,
                        documents,
                        loc,
                        truncated,
                        request_gate,
                        proxy_route,
                    )
                )
            elif failures is not None:
                failures.append(loc)
            if len(out) >= MAX_SITEMAP_URLS:
                # The URL cap was hit mid-index: every sibling after this one is never
                # fetched. Left unrecorded, that reads identically to an index that simply
                # had no more children (#454) -- the same false-complete-parse shape #312
                # fixed for the depth cap right below. Name the dropped children in
                # ``truncated`` so callers get the same honest "incomplete evidence" signal.
                if truncated is not None:
                    for remaining in sitemap_elements[idx + 1 :]:
                        rloc = _local_text(remaining, "loc")
                        if rloc:
                            truncated.append(_resolve_sitemap_loc(rloc.strip(), source))
                break
    else:  # urlset
        declared = 0
        for url_el in _local_children(root, "url"):
            declared += 1
            loc = _local_text(url_el, "loc")
            lastmod = _local_text(url_el, "lastmod")
            # Extension attachments are counted per entry, never merged into the
            # page population: a urlset of 1,283 <loc> and 1,130 <image:loc> is
            # two counts, not 2,413 URLs (#1560).
            attached = {
                "image_entries": len(_local_children(url_el, "image")),
                "news_entries": len(_local_children(url_el, "news")),
                "video_entries": len(_local_children(url_el, "video")),
            }
            if loc:
                out.append(
                    {
                        "loc": _resolve_sitemap_loc(loc.strip(), source),
                        "lastmod": (lastmod or "").strip() or None,
                        "source": source,
                        **{k: v for k, v in attached.items() if v},
                    }
                )
                if len(out) >= MAX_SITEMAP_URLS:
                    # The parse stops here, so the declared count stops being trustworthy;
                    # the document record below says how far it got rather than implying
                    # it read the whole file.
                    break
        if documents is not None:
            documents.append({"url": source, "bytes": len(data), "declared": declared})
    return out


def _origin(url: str) -> str | None:
    """The scheme+host of an absolute URL, or ``None`` if it isn't one.

    An explicit ``--sitemap`` target carries its own host and needs no crawled page
    to derive an origin from -- unlike ``_base_url``, which only ever looks at
    ``ctx.pages`` (#452).
    """
    try:
        parsed = urllib.parse.urlparse(url)
    except (ValueError, AttributeError):
        return None
    if parsed.scheme and parsed.netloc:
        return f"{parsed.scheme}://{parsed.netloc}"
    return None


def _resolve_sitemap_loc(loc: str, source: str) -> str:
    """Resolve a sitemap location only when its document URL is explicit and absolute."""
    return urllib.parse.urljoin(source, loc) if _origin(source) else loc


def _base_url(ctx: AuditContext) -> str | None:
    counts: Counter = Counter()
    for page in ctx.pages:
        parts = page.url.split("/", 3)
        if len(parts) >= 3:
            counts[f"{parts[0]}//{parts[2]}"] += 1
    return counts.most_common(1)[0][0] if counts else None


def _site_target(ctx: AuditContext, base: str | None) -> str | None:
    """A site-wide finding's URL, in the form this run actually recorded it.

    ``_base_url`` returns a bare origin because that is what a robots.txt or
    sitemap fetch needs. A finding is read by a person, though, and an origin
    without a path matches no row in ``pages.jsonl`` -- so a site-wide finding
    aimed at it names a URL the reader cannot look up in the very run that
    reported it. Prefer the crawled home page; fall back to the origin only
    when the crawl never fetched one.
    """
    if base is None:
        return None
    for candidate in (base, base + "/"):
        for page in ctx.pages:
            if page.url == candidate:
                return page.url
    return base


# --------------------------------------------------------------------------
# lastmod parsing
# --------------------------------------------------------------------------
def _parse_date(value: str | None) -> datetime | None:
    if not value:
        return None
    text = value.strip()
    try:
        dt = datetime.fromisoformat(text.replace("Z", "+00:00"))
        return dt if dt.tzinfo else dt.replace(tzinfo=timezone.utc)
    except ValueError:
        pass
    for fmt in ("%Y-%m-%d", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M:%S%z"):
        try:
            dt = datetime.strptime(text, fmt)
            return dt if dt.tzinfo else dt.replace(tzinfo=timezone.utc)
        except ValueError:
            continue
    return None


# --------------------------------------------------------------------------
# export-driven checks
# --------------------------------------------------------------------------
def _urls_from_export(ctx: AuditContext, key: str) -> list[str]:
    df = ctx.exports.get(key)
    if df is None or df.empty:
        return []
    col = find_column(df, ["Address", "URL"])
    if not col:
        return []
    return [normalize_value(v) for v in df[col].tolist() if normalize_value(v)]


def _emit_from_export(ctx: AuditContext, key: str, check_id: str) -> int:
    urls = _urls_from_export(ctx, key)
    for url in urls:
        ctx.add(
            check_id,
            target_url=url,
            details={"in_sitemap": True},
            evidence={"export": ctx.exports.files.get(key)},
        )
    return len(urls)


_NATIVE_SITEMAP_STATUS = ("SITEMAP_URL_4XX_5XX", "SITEMAP_URL_3XX", "SITEMAP_URL_NON_INDEXABLE")


def _native_sitemap_status(ctx: AuditContext, entries: list[dict[str, Any]]) -> None:
    """SITEMAP_URL_4XX_5XX, SITEMAP_URL_3XX and SITEMAP_URL_NON_INDEXABLE from a native crawl.

    A sitemap-listed URL is judged by the crawl's own fetch of that URL. A listed URL the
    crawl never requested has no status: it is counted, and when that leaves a check with
    no finding the check says so, rather than reading the unrequested URLs as clean.
    """
    listed = _normalized_index(e["loc"] for e in entries)
    if not listed:
        for check_id in _NATIVE_SITEMAP_STATUS:
            ctx.skip(check_id, "no sitemap entries were fetched to compare")
        return
    crawled = {normalize_url(p.url): p for p in ctx.pages}
    unmeasured = 0
    for key, loc in listed.items():
        page = crawled.get(key)
        code = page.status_code if page is not None else None
        if not isinstance(code, int):
            unmeasured += 1
            continue
        details = {"in_sitemap": True, "status_code": code}
        if code >= 400:
            ctx.add("SITEMAP_URL_4XX_5XX", target_url=loc, details=details)
        elif 300 <= code < 400:
            ctx.add("SITEMAP_URL_3XX", target_url=loc, details=details)
        elif page.indexability and not page.is_indexable:
            ctx.add(
                "SITEMAP_URL_NON_INDEXABLE",
                target_url=loc,
                details={**details, "indexability": page.indexability},
            )
    if unmeasured:
        reason = (
            f"{unmeasured} sitemap URL(s) were not requested by this crawl, "
            "so their status is unmeasured"
        )
        for check_id in _NATIVE_SITEMAP_STATUS:
            ctx.skip(check_id, reason)


def settle_native_sitemap_status(ctx: AuditContext, reason: str) -> None:
    """Leave no native sitemap-status check silent when its sitemap was never compared.

    _native_sitemap_status settles these checks whenever it runs. A native run that never
    reached that step (no sitemap fetch at all) still has to say why they did not answer.
    """
    for check_id in _NATIVE_SITEMAP_STATUS:
        if not ctx.settled(check_id):
            ctx.skip(check_id, reason)


# The sitemap protocol's own limits, not thresholds anybody should configure: a file over
# either one is invalid rather than merely large.
MAX_SITEMAP_URLS_PER_FILE = 50_000
MAX_SITEMAP_BYTES_PER_FILE = 52_428_800  # 50 MiB, uncompressed


def _check_protocol_limits(
    ctx: AuditContext,
    documents: list[dict[str, Any]],
    entries: list[dict[str, Any]],
    summary: dict[str, Any],
) -> None:
    """Assert the two hard protocol limits, and name URLs declared in two files.

    Reported against the individual document rather than the index: "your sitemap is too big"
    is not actionable when a sitemap index has forty children.
    """
    for doc in documents:
        url = doc.get("url") or _base_url(ctx)
        if doc.get("declared", 0) > MAX_SITEMAP_URLS_PER_FILE:
            ctx.add(
                "SITEMAP_TOO_MANY_URLS",
                target_url=url,
                occurrences_count=doc["declared"],
                details={"declared": doc["declared"], "limit": MAX_SITEMAP_URLS_PER_FILE},
            )
        if doc.get("bytes", 0) > MAX_SITEMAP_BYTES_PER_FILE:
            ctx.add(
                "SITEMAP_TOO_LARGE",
                target_url=url,
                details={"bytes": doc["bytes"], "limit": MAX_SITEMAP_BYTES_PER_FILE},
            )

    # Grouped on the normalised key (#145's fix applied here too): a URL declared as
    # "/a" in one sitemap and "/a/" in another is one duplicated page, not two distinct
    # ones that happen to look alike.
    sources: dict[str, list[str]] = {}
    display: dict[str, str] = {}
    for entry in entries:
        loc = entry.get("loc")
        source = entry.get("source") or ""
        if not loc:
            continue
        try:
            key = normalize_url(loc)
        except ValueError:
            key = loc
        display.setdefault(key, loc)
        seen = sources.setdefault(key, [])
        if source and source not in seen:
            seen.append(source)
    duplicated = {key: srcs for key, srcs in sources.items() if len(srcs) > 1}
    for key, srcs in sorted(duplicated.items()):
        ctx.add("SITEMAP_URL_DUPLICATED", target_url=display[key], details={"sitemaps": srcs})
    if duplicated:
        summary["urls_in_multiple_sitemaps"] = len(duplicated)


# --------------------------------------------------------------------------
# main entry
# --------------------------------------------------------------------------
def run_sitemap(
    ctx: AuditContext,
    sitemap_url: str | None = None,
    compare_with_crawl: bool = True,
    sitemap_urls: list[str] | None = None,
    crawl_partial: bool = False,
    *,
    request_gate: Callable[[], None] | None = None,
    proxy_route=None,
) -> dict[str, Any]:
    """``sitemap_urls``, when given, is every discovered sitemap root the caller wants
    audited (#311) -- auto-discovery can find more than one independent ``Sitemap:``
    directive in robots.txt, and each is its own document with its own protocol-limit
    and duplicate-URL evidence. ``sitemap_url`` alone (its first entry, by convention)
    still selects the single-target behaviour an explicit ``--sitemap`` or an SF export
    needs, so passing both is safe and the list wins when both are given.

    ``crawl_partial`` tells this function the crawl that produced ``ctx.pages`` did not
    reach the whole frontier -- a URL limit, a duration limit, an interruption. A
    thresholded sitemap-versus-link-graph verdict (SITEMAP_DESYNC) is a claim about the
    whole graph, and the unfetched remainder could still hold the very edges the verdict
    depends on, so it is withheld by name rather than computed on an incomplete graph
    (#362).
    """
    summary: dict[str, Any] = {}
    cfg_live = ctx.config.get("live_recheck", {})
    ua = cfg_live.get("user_agent", UA)
    timeout = cfg_live.get("timeout_s", 10)

    # --- 1. SF native Sitemaps:* exports ---------------------------------
    sf_in = _urls_from_export(ctx, "sitemap_in")
    _emit_from_export(ctx, "sitemap_non_200", "SITEMAP_URL_4XX_5XX")
    _emit_from_export(ctx, "sitemap_redirects", "SITEMAP_URL_3XX")
    _emit_from_export(ctx, "sitemap_non_indexable", "SITEMAP_URL_NON_INDEXABLE")
    # Kept as its own list, not just a count: the summary below (#368) needs the
    # declared orphan URLs themselves to tell a crawled-but-orphaned page apart
    # from one Internal:All merely happened to include.
    sf_orphans = _urls_from_export(ctx, "sitemap_orphan")
    for orphan_url in sf_orphans:
        ctx.add(
            "SITEMAP_ORPHAN",
            target_url=orphan_url,
            details={"in_sitemap": True},
            evidence={"export": ctx.exports.files.get("sitemap_orphan")},
        )
    not_in = _urls_from_export(ctx, "sitemap_not_in")
    for url in not_in:
        ctx.add("URL_NOT_IN_SITEMAP", target_url=url)

    # --- 2. direct robots.txt + sitemap parse (opt-in) -------------------
    sitemap_entries: list[dict[str, Any]] = []
    sitemap_documents: list[dict[str, Any]] = []
    declared_in_robots: bool | None = None
    sitemaps_declared: list[str] = []
    want_network = bool(sitemap_url) or bool(sitemap_urls) or cfg_live.get("enabled", False)
    base = _base_url(ctx)
    if base is None:
        # No crawled pages to derive an origin from -- but an explicit --sitemap target
        # names its own host and needs no crawl at all to be worth fetching (#452). Fall
        # back to the first explicit target's origin so the sitemap fetch (and, as a
        # bonus, the robots.txt check) can still run against it.
        explicit_first = (
            (sitemap_urls or [sitemap_url])[0] if (sitemap_urls or sitemap_url) else None
        )
        if explicit_first:
            base = _origin(explicit_first)
    fetch_failures: list[str] = []
    depth_truncated: list[str] = []
    # Whether a sitemap was actually pursued over the network, as opposed to merely
    # requested-but-unable (no base host) — the SITEMAP_DESYNC skip reason below needs this
    # to tell "nobody asked for a sitemap" apart from "one was fetched and came back unusable"
    # (#146: those used to report the same false "no sitemap URL set" reason).
    network_attempted = bool(want_network and base)

    if network_attempted:
        fetch_kwargs = {"request_gate": request_gate} if request_gate is not None else {}
        if proxy_route is not None:
            fetch_kwargs["proxy_route"] = proxy_route
        robots = _fetch(f"{base}/robots.txt", ua, timeout, **fetch_kwargs)
        if robots is not None:
            robots_text = robots.decode("utf-8", "replace")
            sitemaps_declared = SITEMAP_DIRECTIVE.findall(robots_text)
            declared_in_robots = bool(sitemaps_declared)
            if not declared_in_robots:
                ctx.add("SITEMAP_NOT_IN_ROBOTS", target_url=_site_target(ctx, base))
            # robots blocking render-critical resources breaks Google's rendering
            disallows = re.findall(r"(?im)^\s*disallow:\s*(\S+)", robots_text)
            res_re = re.compile(
                r"\.(?:js|css)\b|/_next/|/static/|/assets/|/js/|/css/", re.IGNORECASE
            )
            blocked_res = [d for d in disallows if res_re.search(d)]
            if blocked_res:
                ctx.add(
                    "ROBOTS_BLOCKS_RESOURCES",
                    target_url=_site_target(ctx, base),
                    details={"rules": blocked_res[:10]},
                )
        if sitemap_urls:
            # Every discovered root, de-duplicated but order-preserved (#311) --
            # auto-discovery may have found several independent ``Sitemap:``
            # directives, and each is audited in full rather than only the first.
            seen_targets: set[str] = set()
            targets = [
                t for t in sitemap_urls if t and not (t in seen_targets or seen_targets.add(t))
            ]
        elif sitemap_url:
            targets = [sitemap_url]
        else:
            targets = sitemaps_declared or [f"{base}/sitemap.xml"]
        # SSRF allow-list: the base host plus the hosts of explicitly-given sitemaps.
        allowed_hosts = {_host(base)} | {_host(t) for t in targets if t}
        allowed_hosts.discard("")
        seen = set(targets)
        for sm_url in targets:
            data = _fetch(sm_url, ua, timeout, **fetch_kwargs)
            if data:
                sitemap_entries.extend(
                    _parse_sitemap_bytes(
                        data,
                        ua,
                        timeout,
                        seen,
                        allowed_hosts,
                        failures=fetch_failures,
                        documents=sitemap_documents,
                        source=sm_url,
                        truncated=depth_truncated,
                        request_gate=request_gate,
                        proxy_route=proxy_route,
                    )
                )
            else:
                fetch_failures.append(sm_url)
        if fetch_failures or depth_truncated:
            if fetch_failures:
                summary["sitemap_fetch_failures"] = fetch_failures[:20]
            if depth_truncated:
                summary["sitemap_depth_truncated"] = depth_truncated[:20]
            ctx.add(
                "SITEMAP_FETCH_INCOMPLETE",
                target_url=base,
                details={
                    "failed_count": len(fetch_failures),
                    "examples": fetch_failures[:10],
                    "depth_truncated_count": len(depth_truncated),
                    "depth_truncated_examples": depth_truncated[:10],
                    "max_depth": MAX_SITEMAP_DEPTH,
                },
            )
    else:
        # No sitemap URL is known (no export, no explicit sitemap_url, live_recheck
        # off) or the crawl never resolved a base host to check robots.txt against
        # -- either way robots.txt was never fetched, so nothing here can honestly
        # answer whether it declares a sitemap or blocks a resource path.
        reason = (
            "no sitemap URL to check (no export, no --sitemap, and live_recheck disabled)"
            if not want_network
            else "could not determine the crawled site's base URL"
        )
        ctx.skip("SITEMAP_NOT_IN_ROBOTS", reason)
        ctx.skip("ROBOTS_BLOCKS_RESOURCES", reason)
        ctx.skip("SITEMAP_FETCH_INCOMPLETE", reason)

    _check_protocol_limits(ctx, sitemap_documents, sitemap_entries, summary)
    # Both lists come only from the live parse above; an export never fills them
    # (see _urls_from_export/sf_in for the export-driven half). No document at all
    # means the protocol-limit checks have nothing to measure; no entry at all
    # means there is nothing to compare across sitemaps or date-check.
    if not sitemap_documents:
        reason = "no sitemap document was fetched to measure"
        ctx.skip("SITEMAP_TOO_MANY_URLS", reason)
        ctx.skip("SITEMAP_TOO_LARGE", reason)
    if not sitemap_entries:
        reason = "no sitemap entries were fetched to compare"
        ctx.skip("SITEMAP_URL_DUPLICATED", reason)
        ctx.skip("SITEMAP_STALE_LASTMOD", reason)

    if ctx.scan_con is not None:
        _native_sitemap_status(ctx, sitemap_entries)

    # Prefer the richer source for the URL set / lastmod analysis.
    sitemap_locs = [e["loc"] for e in sitemap_entries] or sf_in
    # True only when the URL set came from the SF export (``sitemap_in``) rather than a
    # live fetch above -- the distinction #368's summary fix below needs: a native/live
    # fetch's ``ctx.pages`` are the crawl's own link-followed pages, a reasonable proxy for
    # "linked", but an SF export's Internal:All also contains pages Screaming Frog reached
    # by requesting the sitemap directly, so presence there is not proof of an internal link.
    using_export_sitemap = bool(sf_in) and not sitemap_entries

    # --- 3. lastmod staleness -------------------------------------------
    lastmod_summary = _analyze_lastmod(ctx, sitemap_entries, base)
    if lastmod_summary:
        summary["lastmod"] = lastmod_summary

    # --- 4. desync (both directions) ------------------------------------
    # Compared on normalize_url()'s key, not the raw written string: a sitemap URL and its
    # matching crawled page differing only by a trailing slash used to read as two disjoint
    # URLs, so a trailing-slash-only mismatch reported 100% desync where the native crawl
    # path's own reconcile_sitemap() reports 0% on the same input (#145).
    sitemap_index = _normalized_index(sitemap_locs)
    pages_index = _normalized_index([p.url for p in ctx.pages])
    indexable_index = _normalized_index([p.url for p in ctx.indexable_html_pages()])
    sitemap_keys = set(sitemap_index)
    pages_keys = set(pages_index)
    # Declared, named from the sitemap's own text — nothing crawled matches this key at all.
    in_sitemap_not_crawl = sorted(sitemap_index[k] for k in sitemap_keys - pages_keys)
    # Named from the crawl's own text — declared side only decides membership, not display.
    in_crawl_not_sitemap = (
        sorted(indexable_index[k] for k in set(indexable_index) - sitemap_keys)
        if sitemap_keys
        else []
    )

    # mark pages with sitemap membership, same normalised key as the diff above so a page is
    # not falsely marked absent from the sitemap over a trailing slash.
    for page in ctx.pages:
        if sitemap_keys:
            try:
                page.metrics["is_in_sitemap"] = normalize_url(page.url) in sitemap_keys
            except ValueError:
                page.metrics["is_in_sitemap"] = False

    if sitemap_keys and not compare_with_crawl:
        # The caller reconciles the sitemap against its own crawl and owns these
        # findings (see seohead.crawl.reconcile.reconcile_sitemap, wired in
        # handlers.crawl_site). Emitting here as well would answer one question
        # twice with two different degrees of rigour -- and this side compares
        # against the whole page list rather than the comparable subset, so it
        # is the cruder of the two. Skipping by name keeps the check accounted
        # for; it is not silently absent.
        ctx.skip("SITEMAP_DESYNC", "the caller reconciles the sitemap against its own crawl")
    elif sitemap_keys and crawl_partial:
        # A thresholded site-wide verdict is unprovable from a graph the crawl did not
        # finish walking -- the unfetched frontier could hold exactly the pages that
        # would flip either percentage below the warn threshold (#362).
        ctx.skip(
            "SITEMAP_DESYNC",
            "crawl is partial: a sitemap-versus-link-graph verdict cannot be proven "
            "when the crawl did not reach every URL",
        )
    elif sitemap_keys:
        threshold = ctx.thresholds["sitemap_desync_pct_warn"]
        # direction 1: indexable pages crawled but missing from the sitemap
        crawl_only_pct = round(100 * len(in_crawl_not_sitemap) / max(len(indexable_index), 1), 1)
        # direction 2: sitemap URLs the crawl never reached (orphan / unlinked / EN half / depth)
        sitemap_only_pct = round(100 * len(in_sitemap_not_crawl) / max(len(sitemap_keys), 1), 1)
        if crawl_only_pct >= threshold or sitemap_only_pct >= threshold:
            ctx.add(
                "SITEMAP_DESYNC",
                target_url=_site_target(ctx, base),
                details={
                    "in_crawl_not_in_sitemap": len(in_crawl_not_sitemap),
                    "in_sitemap_not_in_crawl": len(in_sitemap_not_crawl),
                    "crawl_not_in_sitemap_pct": crawl_only_pct,
                    "sitemap_not_in_crawl_pct": sitemap_only_pct,
                    "examples_missing_from_sitemap": in_crawl_not_sitemap[:20],
                    "examples_in_sitemap_not_crawled": in_sitemap_not_crawl[:20],
                },
            )
    elif not network_attempted:
        ctx.skip("SITEMAP_DESYNC", "no sitemap URL set (no export and network disabled)")
    elif depth_truncated:
        # Distinct from a fetch/parse failure (#312): every attempted document came back
        # and parsed, but a nested index hit the depth cap before yielding a single URL,
        # so the empty URL set here is incomplete evidence, not a genuinely empty sitemap.
        ctx.skip(
            "SITEMAP_DESYNC",
            f"sitemap traversal hit the depth cap ({MAX_SITEMAP_DEPTH} levels) before "
            f"{len(depth_truncated)} descendant document(s) were followed: "
            f"{depth_truncated[:5]}",
        )
    elif fetch_failures:
        # Network was enabled and every declared sitemap document was reached, but not one
        # of them yielded a usable URL set (fetch error, or a parse failure such as an
        # unescaped '&' -- #146). That is not the same fact as "no sitemap exists", and
        # reporting it that way hid a real defect on the target site behind a skip reason
        # that was itself false.
        ctx.skip(
            "SITEMAP_DESYNC",
            f"sitemap fetch/parse failed for all {len(fetch_failures)} attempted "
            f"document(s): {fetch_failures[:5]}",
        )
    else:
        ctx.skip("SITEMAP_DESYNC", "sitemap fetched but declared zero URLs")

    summary.update(
        {
            "declared_in_robots": declared_in_robots,
            "sitemaps": sitemaps_declared
            or (list(sitemap_urls) if sitemap_urls else ([sitemap_url] if sitemap_url else [])),
            "urls_in_sitemap": len(sitemap_keys),
            "urls_in_crawl_indexable": len(indexable_index),
            "in_sitemap_not_in_crawl": len(in_sitemap_not_crawl),
            "in_crawl_not_in_sitemap": len(in_crawl_not_sitemap),
            "non_200_in_sitemap": len(_urls_from_export(ctx, "sitemap_non_200")),
            "non_indexable_in_sitemap": len(_urls_from_export(ctx, "sitemap_non_indexable")),
        }
    )
    # Extension coverage (#1560): image/news/video attachments, counted separately
    # from the page entries and surfaced only when the sitemap declared any — an
    # absent key means "none were declared", never a measured zero.
    summary.update(
        {
            name: total
            for name, total in (
                (
                    "image_entries_in_sitemap",
                    sum(e.get("image_entries", 0) for e in sitemap_entries),
                ),
                (
                    "news_entries_in_sitemap",
                    sum(e.get("news_entries", 0) for e in sitemap_entries),
                ),
                (
                    "video_entries_in_sitemap",
                    sum(e.get("video_entries", 0) for e in sitemap_entries),
                ),
            )
            if total
        }
    )
    if sitemap_keys:
        # Same three key names the native crawler's own reconciliation uses
        # (seohead.crawl.reconcile.reconcile_sitemap), so a consumer of
        # audit.json's summary.sitemap does not need two schemas depending on
        # which crawl mode produced the report. Full lists, not the capped
        # 20-item examples above — this is the first-class output, not a
        # threshold-gated issue detail.
        if using_export_sitemap and ctx.exports.has("sitemap_orphan"):
            # SF's own dedicated evidence for "no internal link reaches this" is the
            # Orphan URLs export, not membership in Internal:All -- Internal:All only
            # proves Screaming Frog crawled the URL, which it also does for a sitemap
            # URL it reached by requesting the sitemap directly (#368). A declared URL
            # absent from Internal:All entirely is still not-linked by construction (no
            # link exists to a page nothing ever crawled) and remains separately named
            # under in_sitemap_not_in_crawl below -- it is folded in here too, but that
            # narrower fact is not lost, just not the only source for this broader list.
            orphan_index = _normalized_index(sf_orphans)
            not_linked_keys = (set(orphan_index) & sitemap_keys) | (sitemap_keys - pages_keys)
            summary["in_sitemap_not_linked"] = sorted(
                orphan_index.get(k, sitemap_index.get(k, k)) for k in not_linked_keys
            )
            summary["in_sitemap_and_linked"] = sorted(
                pages_index[k] for k in (sitemap_keys & pages_keys) - not_linked_keys
            )
        elif using_export_sitemap:
            # No Orphan URLs export was supplied: Internal:All membership alone is not
            # proof an internal link reaches the page, so the navigation-reachability
            # lists are named unavailable rather than filled with that guess. The
            # crawl-presence facts this would otherwise borrow from are still reported,
            # under their own honest names, a few lines above
            # (in_sitemap_not_in_crawl / in_crawl_not_in_sitemap).
            summary["in_sitemap_linked_unavailable"] = (
                "no Sitemaps: Orphan URLs export; Internal: All presence is not proof "
                "of an internal link"
            )
        else:
            summary["in_sitemap_and_linked"] = sorted(
                pages_index[k] for k in sitemap_keys & pages_keys
            )
            summary["in_sitemap_not_linked"] = in_sitemap_not_crawl
        summary["linked_not_in_sitemap"] = in_crawl_not_sitemap
    return summary


def _analyze_lastmod(
    ctx: AuditContext, entries: list[dict[str, Any]], base: str | None
) -> dict[str, Any]:
    dates: list[datetime] = []
    invalid = 0
    future = 0
    now = datetime.now(timezone.utc)
    for e in entries:
        if not e.get("lastmod"):
            continue
        dt = _parse_date(e["lastmod"])
        if dt is None:
            invalid += 1
            continue
        if dt > now:
            future += 1
            continue  # future dates are generation errors — keep them out of stats
        dates.append(dt)
    if not dates:
        return {}
    dates.sort()
    stale_days = ctx.thresholds["sitemap_lastmod_stale_days"]
    cutoff_secs = stale_days * 86400
    cutoff_share = sum(1 for d in dates if (now - d).total_seconds() > cutoff_secs) / len(dates)
    all_same = len({d.date() for d in dates}) == 1
    median_dt = datetime.fromtimestamp(
        statistics.median([d.timestamp() for d in dates]), tz=timezone.utc
    )
    summary = {
        "oldest": dates[0].date().isoformat(),
        "median": median_dt.date().isoformat(),
        "newest": dates[-1].date().isoformat(),
        "share_older_than_threshold": round(cutoff_share, 2),
        "threshold_days": stale_days,
        "all_identical": all_same,
        "invalid_count": invalid,
        "future_count": future,
    }
    if cutoff_share >= 0.5 or all_same or future or invalid:
        # A bare origin (what _base_url returns) matches no row in pages.jsonl -- log-scan's
        # findings_are_about_crawled_urls rule (#285) would then flag this site-wide finding
        # as pointing outside the run's own page list. _site_target names the crawled home
        # page instead, the same rewrite SITEMAP_NOT_IN_ROBOTS and ROBOTS_BLOCKS_RESOURCES
        # already use above.
        ctx.add("SITEMAP_STALE_LASTMOD", target_url=_site_target(ctx, base), details=summary)
    return summary
