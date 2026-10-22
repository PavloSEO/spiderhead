"""Bounded fetching of an explicit URL list.

List mode has no link discovery, site scope model or trap expansion; its
explicit input order is deterministic. Native storage may retain that input
and completed pages on disk. It is also the slice that
does real work on day one — verifying a redirect map after a migration,
re-checking the URLs a developer says are fixed, auditing a Search Console
export.

Rows are written as they are collected. Screaming Frog writes its exports only
at the end, which is why a measured 75-minute polite crawl of a struggling host
produced nothing at all; a collector that batches to the end inherits that
failure exactly where crawling is hardest.
"""

from __future__ import annotations

import contextlib
import json
import os
import time
from collections.abc import Callable, Iterable
from dataclasses import asdict, dataclass, field
from typing import Any
from urllib.parse import urljoin, urlsplit

import httpx

from seohead.checks.parser import empty_link_placement, parse_html, uses_ajax_crawling_scheme
from seohead.checks.robots import is_allowed, match_path, parse_robots
from seohead.crawl.cache import ResponseCache
from seohead.crawl.settings import (
    SENSITIVE_HEADER_NAMES,
    checked_url_budget,
    resolve_credential_headers,
)
from seohead.crawl.throttle import MAX_DELAY_S, DispatchGate, RequestBudgetExhausted, Throttle
from seohead.recon.net import (
    UA,
    BlockedRedirectError,
    client_network_policy,
    http_client,
    pinned_target,
    validate_url,
)

SCHEMA_VERSION = "crawl.v1"

MAX_RESPONSE_BYTES = 5 * 1024 * 1024
DEFAULT_TIMEOUT_S = 15.0
# Matches seohead.checks.redirects's own hop cap; a chain that has not landed by
# then is a misconfiguration (or a loop), not a slow site.
MAX_REDIRECT_CHAIN_HOPS = 10


class DurationBudgetExhausted(RequestBudgetExhausted):
    """The cumulative list deadline expired before another HTTP dispatch."""


@dataclass
class PageRecord:
    """One fetched URL, in the collector's own vocabulary."""

    url: str
    status_code: int | None = None
    content_type: str = ""
    size_bytes: int = 0
    response_time: float | None = None
    redirect_url: str = ""
    title: str = ""
    meta_description: str = ""
    h1: str = ""
    h1_2: str = ""
    h2: str = ""
    canonical: str = ""
    meta_robots: str = ""
    x_robots: str = ""
    og_title: str = ""
    og_description: str = ""
    og_image: str = ""
    # The page's own <meta property="og:url"> canonical social URL (#654), which
    # a Screaming Frog export has carried as "OG:URL" all along. "" when the page
    # declares none -- a value check_og may then report as missing, which it must
    # not do for a page that declares one.
    og_url: str = ""
    word_count: int = 0
    text_ratio: float | None = None
    # <iframe> elements sitting inside the resolved content area, and how many
    # of those the site itself serves (#360). A framed document is not part of
    # this one's DOM, so word_count above measures the shell around the content;
    # these two say so instead of leaving the page to be reported as thin.
    content_frames: int = 0
    content_frames_same_origin: int = 0
    # How many live <meta name="description"> tags the document carries (#385).
    # meta_description above is unchanged -- still exactly the first -- so
    # existing consumers keep reading the same string; this is additive.
    meta_description_count: int = 0
    # The alt text of an H1 whose own text is empty when an image supplies it
    # (#385) -- "" when no H1 qualifies, distinguishing an ordinary missing H1
    # from one that reads empty only because its text lives in an <img alt>.
    h1_alt_text: str = ""
    # Occurrences of the Lorem Ipsum placeholder passage within the resolved
    # content area (#385); 0 on a page with none.
    lorem_ipsum_count: int = 0
    # Per-page <img> alt-attribute inventory (#386): how many images the page
    # has, how many are missing the alt attribute outright (alt="" does not
    # count -- that is a correctly marked decorative image), and the longest
    # alt string among the images that do carry one.
    images_total: int = 0
    images_missing_alt_attr: int = 0
    images_max_alt_length: int = 0
    # Legacy plugin-dependent elements (<object>/<embed>/<applet>) that are not
    # a benign image fallback (#385).
    plugin_elements: int = 0
    # rel=alternate declarations with a media attribute and no href (#1016).
    mobile_alternate_broken: int = 0
    # The page's own <meta name="fragment"> content attribute, as written (#386):
    # the page-wide opt-in to Google's deprecated AJAX crawling scheme. "" when
    # the page declares none. seohead.checks.render already read this tag to pick a
    # fetch mode; recording it is what lets the audit report that the site still
    # carries it.
    meta_fragment: str = ""
    # How many of this page's own outlink hrefs are written in that same
    # deprecated scheme -- a "#!" fragment or an "?_escaped_fragment_=" query
    # (#386). Counted over every link the parser retained, internal or external,
    # because "this page still publishes hash-bang URLs" is the finding either way.
    ajax_scheme_outlinks: int = 0
    crawl_depth: int = 0
    # Response-header/markup evidence for the static Lighthouse checks in
    # seohead.sf.core.rules (charset/doctype/viewport/uses-text-compression) —
    # see seohead/sf/core/lighthouse.py for the correspondence.
    content_encoding: str = ""
    # Legacy native/JSONL projection; retained scans read the stored source head.
    render_blocking: str | None = None
    # The page's own <meta http-equiv="refresh"> declaration, as written.
    # Projected as SF's "Meta Refresh 1" so META_REFRESH_REDIRECT reaches the
    # same verdict from a native crawl as from an export.
    meta_refresh: str = ""
    # The raw HTTP "Refresh" response header, if the server sent one (#385) --
    # distinct from meta_refresh (a <meta http-equiv="refresh"> element) above;
    # see check_directives_extra for how the two are read once populated.
    http_refresh: str = ""
    charset: str = ""
    doctype: str = ""
    viewport: str = ""
    # Document-position evidence for issue #123: like the four fields above, a
    # native Screaming Frog export never carries this either, because "was
    # this element inside <head> once the parser recovered" needs the parse
    # tree, not a crawl column. ``None`` means the element itself is absent
    # (a different finding); the joined string mirrors how other multi-value
    # fields on this record (e.g. ``meta_robots``) are carried as text.
    title_outside_head: bool | None = None
    meta_description_outside_head: bool | None = None
    canonical_outside_head: bool | None = None
    directives_outside_head: bool | None = None
    hreflang_outside_head: bool | None = None
    # The alternates themselves, as the document wrote them (#357). The boolean
    # above answers where the tags sat; this answers what they said, which is the
    # only authoritative statement a site makes about which of its pages are the
    # same page in another language.
    #
    # Measured in a dedicated 8 000-record hreflang fixture: a page carrying
    # twelve alternates costs 6 392 bytes against 2 456 with none, so 3 936
    # bytes for the declaration set. Absolute totals vary with retained field
    # lengths; across a 50 000-page twelve-language crawl that fixture adds
    # 188 MiB. The scope-narrowing advice above MAX_URLS_CEILING applies here
    # too; this is not the field that decides the ceiling.
    hreflang: list[dict[str, str]] = field(default_factory=list)
    # The resolved URL of the page's <link rel="amphtml"> target (#1020); "" when none.
    amphtml: str = ""
    # Every h1-h6 with text, in DOM order, each with its level, text and page
    # region (#632). h1/h1_2/h2 above are the same headings read as a set, which
    # is all a Screaming Frog export carries; a set cannot show an H2 standing
    # before the H1, and it cannot show that the H2 is a menu label in the
    # masthead. Region is "" when the document offered no landmark and no
    # position rule matched -- unmeasured, not "content" (see
    # parser.heading_outline).
    heading_outline: list[dict[str, Any]] = field(default_factory=list)
    # Hrefs on this page that could not become fetchable link targets, each as
    # {"raw_href", "reason"} exactly as the parser rejected it (#1530). None —
    # not an empty list — when the crawl ran without
    # discovery.record_invalid_targets: "not measured" and "measured,
    # none found" are different facts.
    invalid_link_targets: list[dict[str, str]] | None = None
    # The URL entered the crawl over http:// and was fetched over https://
    # without a redirect hop, under http.upgrade_to_https (#1484). False is
    # both "not upgraded" and "upgrade off" -- the setting is on the run, not
    # the page; what the page records is what happened to it.
    transport_upgraded: bool = False
    # Reading level over the retained content area (#1516), as
    # checks.readability.reading_ease computes it. None — never zero — when the
    # area was not retained or holds no word with a letter: unmeasured.
    sentence_count: int | None = None
    avg_words_per_sentence: float | None = None
    flesch: float | None = None
    readability: str = ""
    # The two link defects a page region cannot show: anchors wrapped in an
    # h1-h6, and image links with no anchor text and no alt (#634). Both lists
    # inside are capped by the parser, so this costs a small constant per page
    # rather than growing with the link count -- which is why, like the heading
    # outline above, it is not behind link_position.classify. A record whose
    # HTML was never parsed (an error, a non-HTML response) keeps the empty
    # default, which is true of a document with no anchors in it.
    link_placement: dict[str, Any] = field(default_factory=empty_link_placement)
    # The page's authorship/date/article-scope markup evidence, as the parser's
    # per-signal list (issue #823). ``None`` — not an empty object — when this
    # record's HTML was never parsed (an error, a non-HTML response, an
    # oversized body): an empty object would read as "measured, no signals"
    # where the truth is "never measured", the same distinction link_placement
    # above makes through its own None.
    trust_signals: dict[str, Any] | None = None
    # Bounded repeated-id observations from parsed HTML (#828). ``None`` means
    # HTML was not parsed; an empty list means it was parsed and none repeated.
    duplicate_ids: list[dict[str, Any]] | None = None
    head_count: int = 0
    body_count: int = 0
    head_not_first: bool = False
    invalid_head_elements: str = ""
    # Every link found on the page, and how many of them left the host. Note
    # that Screaming Frog's Outlinks column counts internal links only, so the
    # projection in evidence.py subtracts rather than passing this through.
    outlinks: int = 0
    external_outlinks: int = 0
    jsonld_blocks_found: int = 0
    jsonld_blocks_parsed: int = 0
    error: str = ""
    # "timeout", "connection" (a transport failure that produced no response at all —
    # refused/reset connection, DNS failure, an aborted TLS handshake), "blocked_redirect"
    # (the server answered with a real redirect whose ``Location`` our own guard refused to
    # follow — see ``recon.net.BlockedRedirectError`` and #175; unlike the first two, this one
    # always comes with a real ``status_code`` and ``redirect_url``, because a response was
    # received), or "" for anything else (a non-2xx response, a parser error, no error at all).
    # Recorded as data rather than left for a caller to re-derive from ``error``'s free text,
    # because that text is whatever the underlying exception happened to say — see
    # ``_classify_fetch_error`` and #132. Both ``collect_urls``'s live Throttle and
    # ``spider._fold_failure_streaks``'s replay of the same decision from a written-out record
    # key off this field so the two never drift apart. ``_fold_failure_streaks`` only consults
    # this field when ``status_code`` is still ``None``, so "blocked_redirect" never reaches
    # that branch — it counts as the healthy response it is, like any other 3xx.
    error_kind: str = ""
    # "" when no cache was configured for this run at all. Otherwise one of "hit" (served from
    # disk, no request sent), "revalidated" (a conditional request came back 304, body reused),
    # "miss" (a real, full fetch — the first time, or because nothing on disk was usable) or
    # "bypass" (a cache was configured, but its own lookup for this URL was unusable — e.g. an
    # unsafe cache directory — so the page was fetched live without ever consulting the cache;
    # distinct from "" so a report can tell "no cache at all" from "cache configured, but this
    # URL's lookup was bypassed"). This is the per-URL half of "a report built partly from cache
    # must say so"; cache_stats on the run as a whole is the aggregate half.
    cache_status: str = ""
    # Empty means parser-derived fields were measured normally (or the page was already
    # governed by its status/content type). "oversized" means max_response_bytes stopped an
    # HTML response short of parsing; the media-type reasons mean a configured response filter
    # deliberately withheld parsing. In either case these fields are unmeasured, not absent.
    body_unavailable: str = ""
    # Which representation produced this page's evidence: "static" (raw HTML,
    # the default), "rendered" (JavaScript executed), or "legacy_fragment"
    # (the deprecated ``_escaped_fragment_`` scheme). Recorded per page, not
    # assumed for the whole crawl, because selective escalation (#18) renders
    # only the URL patterns that need it -- a report that mixed the two
    # populations in one column would compare numbers that were never
    # measured the same way.
    representation: str = "static"
    # Populated only when list mode is asked to resolve a redirect past its first hop (see
    # discovery.resolve_redirect_destination). Empty means either this page was not a redirect
    # or the option was off -- not "resolved to nowhere", which is what an empty final_url next
    # to a non-empty redirect_url would wrongly suggest.
    redirect_chain: list[dict[str, Any]] = field(default_factory=list)
    final_url: str = ""
    # Canonical targets are inspected only when list mode explicitly asks for a
    # bounded chain walk; they never become discovered list-mode pages.
    canonical_chain: list[dict[str, Any]] = field(default_factory=list)
    final_canonical: str = ""

    @property
    def is_html(self) -> bool:
        return "html" in (self.content_type or "").lower()


@dataclass
class CrawlResult:
    schema_version: str = SCHEMA_VERSION
    pages: list[PageRecord] = field(default_factory=list)
    partial: bool = False
    stopped_reason: str = ""
    # Categorical companion to stopped_reason, for callers that branch on why a
    # crawl stopped rather than parse a sentence. Always set, so "why did this
    # stop" never depends on whether stopped_reason happened to be non-empty.
    finish_reason: str = "finished"
    resumed: bool = False
    limitations: list[str] = field(default_factory=list)
    # Empty when no cache was configured. See seohead.crawl.cache.ResponseCache.stats for the
    # keys: hits, revalidations, stores, bypassed, invalidated.
    cache_stats: dict[str, int] = field(default_factory=dict)
    # True exactly when the run used mode="replay" — see seohead.crawl.cache. A replay run may
    # still contain live fetches for URLs never cached before; per-page cache_status says which.
    cache_replay: bool = False
    # URLs robots.txt disallowed for the configured token, whether or not the policy actually
    # kept them out of pages (see robots_policy on collect_urls) — spider.SpiderResult carries
    # the same field for the same reason: what would be blocked must be visible under
    # report_only too, not only when it changes what was fetched.
    robots_blocked: list[str] = field(default_factory=list)


def _upgrade_transport(url: str) -> str:
    """https for an http URL (#1484) -- same rule as crawl.spider.upgrade_transport,
    restated here because spider imports this module and not the other way round."""
    return "https://" + url[len("http://") :] if url.startswith("http://") else url


def _text_of(value: Any) -> str:
    return "" if value is None else str(value)


def _first_heading(parsed: dict, level: str, index: int = 0) -> str:
    items = (parsed.get("headings") or {}).get(level) or []
    return _text_of(items[index]) if len(items) > index else ""


def _record_from_parsed(parsed: dict) -> dict[str, Any]:
    og = parsed.get("og") or {}
    links = parsed.get("links") or []
    link_observation = parsed.get("link_observation") or {}
    position = parsed.get("position") or {}
    # Only the frames inside the content area: an iframe in a footer is a widget,
    # an iframe where the copy should be is the page's content (#360).
    framed = [f for f in (parsed.get("frames") or []) if f.get("in_content_area")]
    images = parsed.get("images") or []
    images_with_alt = [i for i in images if i.get("has_alt")]
    return {
        "title": _text_of(parsed.get("title")),
        "meta_description": _text_of(parsed.get("meta_description")),
        "meta_description_count": int(parsed.get("meta_description_count") or 0),
        "h1": _first_heading(parsed, "h1", 0),
        "h1_2": _first_heading(parsed, "h1", 1),
        "h1_alt_text": _text_of(parsed.get("h1_alt_only_text")),
        "h2": _first_heading(parsed, "h2", 0),
        "canonical": _text_of(parsed.get("canonical")),
        "lorem_ipsum_count": int(parsed.get("lorem_ipsum_count") or 0),
        "images_total": len(images),
        "images_missing_alt_attr": len(images) - len(images_with_alt),
        "images_max_alt_length": max((i.get("alt_length", 0) for i in images_with_alt), default=0),
        "plugin_elements": int(parsed.get("plugin_elements_count") or 0),
        "mobile_alternate_broken": int(parsed.get("mobile_alternate_broken") or 0),
        "meta_fragment": _text_of(parsed.get("meta_fragment")),
        # Read off the resolved hrefs the parser already produced, not re-parsed
        # from markup: an href is only a scheme URL once it is a URL (#386).
        "ajax_scheme_outlinks": sum(
            1 for link in links if uses_ajax_crawling_scheme(_text_of(link.get("href")))
        ),
        # Every crawler-addressed robots tag, agent scope preserved (see
        # parser.robots_meta_scoped): a page can be noindex for Googlebot alone,
        # and a directive named for Bingbot or Yandex must not read as global.
        "meta_robots": ", ".join(parsed.get("robots_meta_scoped") or []),
        # Keyed by the full property name, which is the shape parser.parse_html
        # produces and documents ({"og:title": ...}, not {"title": ...}). Asking
        # for the unprefixed name returned None on every page, so every native
        # crawl stored empty Open Graph and an audit read absent as blank (#646).
        "og_title": _text_of(og.get("og:title")),
        "og_description": _text_of(og.get("og:description")),
        "og_image": _text_of(og.get("og:image")),
        # Read for the same reason as the three above, and recorded because
        # rules.check_og lists "og:url" among a page's missing tags whenever the
        # field is falsy. Nothing on this path collected it, so every page of
        # every native crawl was reported as missing a tag nobody had read (#654).
        "og_url": _text_of(og.get("og:url")),
        "word_count": int(parsed.get("word_count") or 0),
        "content_frames": len(framed),
        "content_frames_same_origin": len([f for f in framed if f.get("same_origin")]),
        "outlinks": int(link_observation.get("total", len(links))),
        "external_outlinks": int(
            link_observation.get(
                "external_total", len([link for link in links if link.get("external")])
            )
        ),
        "charset": _text_of(parsed.get("charset")),
        "doctype": _text_of(parsed.get("doctype")),
        "viewport": _text_of(parsed.get("viewport")),
        "meta_refresh": _text_of(parsed.get("meta_refresh")),
        "render_blocking": json.dumps(parsed["render_blocking"])
        if "render_blocking" in parsed
        else None,
        "title_outside_head": position.get("title_outside_head"),
        "meta_description_outside_head": position.get("meta_description_outside_head"),
        "canonical_outside_head": position.get("canonical_outside_head"),
        "directives_outside_head": position.get("directives_outside_head"),
        "hreflang_outside_head": position.get("hreflang_outside_head"),
        "hreflang": list(parsed.get("hreflang") or []),
        "amphtml": str(parsed.get("amphtml") or ""),
        "heading_outline": list(parsed.get("heading_outline") or []),
        # None keeps the parser's not-measured meaning (#1530); the key is in
        # parsed exactly when the crawl asked for invalid-target recording.
        "invalid_link_targets": (
            list(parsed["invalid_link_targets"]) if "invalid_link_targets" in parsed else None
        ),
        # Reading level over the content area (#1516); the parser's None
        # (nothing measurable retained) stays None, and "" band means the same.
        "sentence_count": (r := parsed.get("readability") or {}).get("sentence_count"),
        "avg_words_per_sentence": r.get("avg_words_per_sentence"),
        "flesch": r.get("flesch"),
        "readability": r.get("readability") or "",
        "link_placement": parsed.get("link_placement") or empty_link_placement(),
        # Always a parsed dict here (a missing key would be a parser defect),
        # so the field above the default keeps its None-for-unmeasured meaning.
        "trust_signals": parsed.get("trust_signals"),
        "duplicate_ids": parsed.get("duplicate_ids"),
        "head_count": int(position.get("head_count") or 0),
        "body_count": int(position.get("body_count") or 0),
        "head_not_first": bool(position.get("head_not_first")),
        "invalid_head_elements": ", ".join(position.get("invalid_head_elements") or []),
    }


def _jsonld_counts(_html: str, parsed: dict) -> tuple[int, int]:
    """Blocks present in the markup, and blocks that actually parsed.

    The parser already excludes inert blocks and records each live malformed or
    empty block in ``jsonld_invalid``. Re-counting raw HTML would give the two
    fields different eligibility rules.
    """
    valid = parsed.get("jsonld") or []
    invalid = parsed.get("jsonld_invalid") or []
    return len(valid) + len(invalid), len(valid)


def _apply_body(
    record: PageRecord,
    url: str,
    body: str,
    parse_options: dict[str, Any] | None = None,
    max_response_bytes: int = MAX_RESPONSE_BYTES,
    size_bytes: int | None = None,
) -> dict[str, Any] | None:
    """Fill in every field derived from the body. Shared by a live fetch, a cache hit, and a
    revalidated (304) response, so the three produce identical records for identical bytes.

    ``size_bytes`` is the length of the response body as it arrived, after transfer decoding.
    It is passed in rather than derived here because ``body`` is already a decoded string, and
    for anything that is not valid UTF-8 — an image, a PDF, a font, a page in windows-1251 —
    decoding replaces each undecodable byte with U+FFFD, which re-encodes to three bytes. The
    measurement would then be a function of how much of the file happens to look like invalid
    UTF-8, not of its size (issue #99). ``None`` means the caller genuinely has no byte count
    (a test fetcher returning only ``.text``), and only then is the encoded length used.
    """
    record.size_bytes = (
        size_bytes if size_bytes is not None else len(body.encode("utf-8", "ignore"))
    )
    if record.size_bytes > max_response_bytes:
        # Too large to parse, but a 200 is still a 200: not "unreachable".
        record.error = "response too large to parse"
        if record.is_html:
            record.body_unavailable = "oversized"
        return None
    if not (record.is_html and body):
        return None
    parsed = parse_html(body, url, parse_options)
    # A successful parse replaces any earlier body-level limitation. This
    # matters when rendering supplies usable HTML after the static fetch was
    # too large to parse: the static transport error remains, but the fields
    # below are now measured rather than unavailable.
    record.body_unavailable = ""
    # Transient, never persisted to pages.jsonl or PageRecord: the rendering pre-flight gate
    # (#18) needs the start page's raw HTML to check for an empty SPA shell, and this is the
    # one place that HTML is already in memory.
    parsed["_raw_html"] = body
    for key, value in _record_from_parsed(parsed).items():
        setattr(record, key, value)
    found, parsed_count = _jsonld_counts(body, parsed)
    record.jsonld_blocks_found = found
    record.jsonld_blocks_parsed = parsed_count
    text_len = len(_text_of(parsed.get("text")).encode("utf-8", "ignore"))
    # Percent, not a fraction: the analyzer's threshold is a percentage and
    # the export format this projects onto uses percent too (20.0, 15.0).
    # Emitting 0.6 here made LOW_TEXT_RATIO fire on every crawled page,
    # since 0.6 < 10 always.
    record.text_ratio = round(text_len / record.size_bytes * 100, 2) if record.size_bytes else None
    return parsed


def _from_cache_entry(
    record: PageRecord,
    entry: Any,
    status: str,
    parse_options: dict[str, Any] | None = None,
    max_response_bytes: int = MAX_RESPONSE_BYTES,
    response_filter: Callable[[str], str] | None = None,
) -> dict[str, Any] | None:
    """Build a record from a stored (or reconfirmed) cache entry. No network involved."""
    record.status_code = entry.status_code
    record.content_type = entry.headers.get("content-type", "")
    record.x_robots = entry.headers.get("x-robots-tag", "")
    # Stored with the entry, so a replayed page still reports the encoding that was on the
    # wire when it was fetched — otherwise the compression audit would silently read "" for
    # every cached URL and call an already-compressed site uncompressed.
    record.content_encoding = entry.headers.get("content-encoding", "")
    record.http_refresh = entry.headers.get("refresh", "")
    location = entry.headers.get("location", "")
    record.redirect_url = urljoin(record.url, location) if location else ""
    record.response_time = 0.0
    record.cache_status = status
    if response_filter is not None:
        reason = response_filter(record.content_type)
        if reason:
            record.body_unavailable = reason
            return None
    return _apply_body(
        record,
        record.url,
        entry.body,
        parse_options,
        max_response_bytes,
        # Stored with the entry: see _apply_body on why this cannot be recomputed from the body.
        size_bytes=entry.size_bytes or None,
    )


def fetch_one(
    url: str,
    *,
    client: Any = None,
    fetcher: Callable[[str], Any] | None = None,
    throttle: Throttle | None = None,
    extra_headers: dict[str, str] | None = None,
    user_agent: str = "",
    max_response_bytes: int = MAX_RESPONSE_BYTES,
    retry_on_timeout: int = 0,
    parse_options: dict[str, Any] | None = None,
    cache: ResponseCache | None = None,
    wait: Callable[[], None] | None = None,
    capture_observer: Callable[[Any], None] | None = None,
    capture_max_bytes: int | None = None,
    response_filter: Callable[[str], str] | None = None,
    transport_upgraded: bool = False,
) -> tuple[PageRecord, dict[str, Any] | None]:
    """Fetch and parse one URL. Returns the record and the parsed document.

    The parsed document is handed back rather than discarded so a caller that
    needs the links — the spider — does not parse the same bytes twice.

    ``extra_headers`` is resolved by the caller for this URL's own host, so it
    never survives a redirect to a different host: the next hop is a fresh
    call with headers resolved for the new host, not these carried forward.

    ``retry_on_timeout`` retries only a timeout — a connect, TLS, or read
    timeout, per ``_classify_fetch_error`` — never a connection failure (refused,
    reset, DNS) and never a 4xx/5xx: none of those would come back different on
    a second try, only heavier on an origin that is already struggling or one
    that already answered.
    ``parse_options`` is forwarded to ``parse_html`` untouched (e.g.
    ``{"classify_links": True, "link_position_rules": [...]}``); ``None``
    keeps every parser default, including link classification being off.
    ``response_filter`` receives the response Content-Type and may return a
    stable body-omission reason. It runs before parsing and, for native streamed
    capture, before entity bytes are read; response metadata is still returned.
    ``cache``, when given, is consulted before anything else touches the network — see
    ``seohead.crawl.cache`` for the freshness policy. A credentialed request (``extra_headers``
    non-empty) always bypasses it in both directions. A conditional revalidation only ever
    happens on the real client path: an injected ``fetcher`` has no way to carry request
    headers, so a stale entry behind a ``fetcher`` is simply re-fetched in full rather than
    revalidated — still correct, just less efficient. ``wait``, when given, is called once
    before every real network round trip this call makes, including a timeout retry — never
    called at all on a cache hit, which is what keeps a hit from consuming a throttle delay slot
    or a concurrent dispatch turn it never needed. A retried timeout also updates ``throttle``
    before that next ``wait()``, so a struggling origin is backed off from immediately rather
    than only once every attempt in this call has already failed (#196).
    """
    record = PageRecord(url=url)
    # #1484: the caller upgraded an http URL to https before this call; the
    # marker belongs to the record, not the caller's bookkeeping.
    record.transport_upgraded = transport_upgraded
    observed_protocol: str | None = None
    capture_started = None
    response = None
    sent_headers = {"User-Agent": user_agent or UA, **(extra_headers or {})}
    capture_limit = max_response_bytes if capture_max_bytes is None else capture_max_bytes
    if capture_observer is not None and cache is not None:
        raise ValueError("native response capture cannot use the legacy HTTP cache")

    def emit(
        entity: bytes | None = None,
        reason: str = "not_fetched",
        response_headers: dict[str, str] | None = None,
        body_state: str | None = None,
    ) -> None:
        if capture_observer is None:
            return
        from seohead.crawl.capture import (
            CaptureEvent,
            header_pairs,
            now_utc,
            redact_headers,
            unavailable_transport_phases,
        )

        try:
            request_object = getattr(response, "request", None)
        except RuntimeError:
            request_object = None
        request_pairs = header_pairs(
            request_object.headers if request_object is not None else sent_headers
        )
        # Only a credential this run was configured to send counts as one. The wire
        # headers also carry whatever the shared client's cookie jar picked up, and a
        # session cookie the server itself set is the ordinary state of the web, not
        # the operator's authentication: reading it as one discarded four fifths of a
        # public site's bodies on a run with nothing configured at all (#647). A
        # sensitive name can only reach these two maps from http.credential_headers,
        # because configuration validation refuses one in http.headers.
        credentials = any(
            name in SENSITIVE_HEADER_NAMES and value
            for name, value in header_pairs(sent_headers)
            + header_pairs(getattr(client, "headers", None) or {})
        )
        # Redaction still reads the wire headers: a session cookie's value is not the
        # operator's credential, but it is still not something to leave in an error.
        for name, value in request_pairs:
            if name in SENSITIVE_HEADER_NAMES and value:
                record.error = record.error.replace(value, "REDACTED")
        final_headers = getattr(response, "headers", None) or response_headers or {}
        history = list(getattr(response, "history", ()) or ())
        original = history[0] if history else response
        effective = (
            str(getattr(response, "url", "") or record.final_url or url)
            if record.status_code is not None
            else ""
        )
        # The native no-follow client connects to a vetted IP with Host/SNI.
        # That socket address is not a redirect or a second logical URL.
        if fetcher is None and not history and record.status_code is not None:
            effective = url
        chain = tuple(
            {
                "request_url": str(hop.url),
                "status_code": hop.status_code,
                "location_raw": hop.headers.get("location", ""),
                "next_url": str(history[index + 1].url) if index + 1 < len(history) else effective,
                "blocked": False,
            }
            for index, hop in enumerate(history)
        )
        capture_observer(
            CaptureEvent(
                method=str(getattr(request_object, "method", "GET")),
                requested_url=url,
                effective_url=effective,
                redirect_history=chain,
                requested_at=capture_started or now_utc(),
                received_at=now_utc(),
                status_code=getattr(original, "status_code", None)
                if history
                else record.status_code,
                request_headers=redact_headers(request_pairs),
                credentials_used=bool(credentials),
                response_headers=redact_headers(
                    getattr(original, "headers", None) or final_headers
                ),
                content_type=record.content_type,
                content_encoding=record.content_encoding,
                entity_bytes=entity,
                body_fidelity="entity_bytes" if entity is not None else "unavailable",
                body_state=body_state
                or (
                    "complete"
                    if entity is not None
                    else "truncated"
                    if reason == "truncated"
                    else "unavailable"
                ),
                body_reason=reason,
                error=record.error,
                error_kind=record.error_kind,
                effective_status_code=record.status_code,
                effective_headers=redact_headers(final_headers),
                response_time=record.response_time,
                # Server-set cookies establish an ordinary anonymous session.
                # Configured Cookie headers are already credentialed and guarded
                # through the redacted credential context instead.
                session_changed=any(
                    name == "set-cookie" and value for name, value in header_pairs(final_headers)
                ),
                http_version=observed_protocol,
                timing_state="partial" if record.response_time is not None else "unavailable",
                transport_phases=(
                    transport_trace.snapshot()
                    if transport_trace is not None
                    else unavailable_transport_phases(
                        "injected fetcher does not expose HTTPX transport phases"
                    )
                ),
            )
        )

    if fetcher is None:
        # Guard only the transport we open ourselves. validate_url resolves DNS,
        # so running it against an injected transport would make offline tests
        # depend on the network and would guard a socket we never open.
        try:
            policy = client_network_policy(client)
            if policy is None:
                validate_url(url)
            else:
                validate_url(url, policy=policy)
        except Exception as exc:  # blocked target, bad scheme, private network
            record.error = str(exc)
            emit()
            return record, None

    cache_eligible = cache is not None and not extra_headers
    request_headers = {"User-Agent": user_agent or UA}
    outcome = None
    if cache_eligible:
        outcome = cache.decide(url, request_headers)
        if outcome.status == "hit":
            parsed = _from_cache_entry(
                record,
                outcome.entry,
                "hit",
                parse_options,
                max_response_bytes,
                response_filter,
            )
            return record, parsed

    if wait is not None:
        wait()

    # A conditional GET needs request headers, which an injected single-argument ``fetcher``
    # has no way to receive — so a fetcher-backed run never revalidates, it just re-fetches in
    # full, which is correct (if less efficient) rather than silently skipping the cache.
    conditional_headers = (
        outcome.conditional_headers
        if fetcher is None and outcome is not None and outcome.status == "revalidate"
        else {}
    )
    started = time.monotonic()
    attempt = 0
    captured_entity = None
    captured_text = None
    capture_failure = None
    capture_failure_reason = "truncated"
    media_filter_reason = ""
    transport_trace = None
    while True:
        media_filter_reason = ""
        if capture_observer is not None:
            from seohead.crawl.capture import TransportTrace, now_utc

            capture_started = now_utc()
            transport_trace = TransportTrace() if fetcher is None else None
        try:
            if fetcher:
                response = fetcher(url)
            else:
                # Connect to the address that was vetted, keeping the hostname for
                # SNI and certificate verification. Resolving twice would leave a
                # window between the check and the connection.
                policy = client_network_policy(client)
                if policy is None:
                    target, headers, extensions = pinned_target(url)
                else:
                    target, headers, extensions = pinned_target(url, policy=policy)
                request = {
                    **request_headers,
                    **headers,
                    **(extra_headers or {}),
                    **conditional_headers,
                }
                sent_headers = request
                requests_zstd = any(
                    name.lower() == "accept-encoding"
                    and "zstd" in {part.strip().lower() for part in value.split(",")}
                    for name, value in request.items()
                )
                if capture_observer is None and not requests_zstd:
                    response = client.get(target, headers=request, extensions=extensions)
                else:
                    from seohead.crawl.capture import (
                        EntityDecodeError,
                        EntityLimitError,
                        bounded_entity_chunks,
                        decode_entity,
                    )

                    if not any(name.lower() == "accept-encoding" for name in request):
                        request["Accept-Encoding"] = "gzip, deflate"

                    extensions = {**extensions, "trace": transport_trace}

                    with client.stream(
                        "GET", target, headers=request, extensions=extensions
                    ) as streamed:
                        response = streamed
                        streamed_headers = {k.lower(): v for k, v in dict(streamed.headers).items()}
                        if response_filter is not None:
                            media_filter_reason = response_filter(
                                streamed_headers.get("content-type", "")
                            )
                        if not media_filter_reason:
                            try:
                                captured_entity = bounded_entity_chunks(
                                    streamed.iter_raw(chunk_size=64 * 1024),
                                    streamed_headers.get("content-encoding", ""),
                                    capture_limit,
                                )
                                captured_text = decode_entity(
                                    captured_entity, streamed_headers.get("content-type", "")
                                )[0]
                            except EntityLimitError as exc:
                                capture_failure = str(exc)
                                if isinstance(exc, EntityDecodeError):
                                    capture_failure_reason = "fetch_failed"
                                captured_text = ""
                        # When zstd was explicitly negotiated without native body
                        # capture, use HTTPX's raw stream and the same bounded
                        # decoder. This works with stdlib compression.zstd and
                        # does not depend on HTTPX's optional zstd decoder.
            observed_protocol = str(getattr(response, "http_version", "") or "") or None
            break
        except BlockedRedirectError as exc:
            # The origin answered in full — this is a redirect our own guard refused to
            # follow, not a sign the origin is unreachable (#175) — so it is recorded exactly
            # like the unguarded 3xx it would have been, plus the reason it went no further.
            elapsed = time.monotonic() - started
            record.response_time = round(elapsed, 3)
            record.status_code = exc.status_code
            record.redirect_url = exc.location
            record.error = str(exc)
            record.error_kind = "blocked_redirect"
            if throttle is not None:
                throttle.record_response(elapsed, False)
                throttle.record_success()
            emit(reason="fetch_failed")
            return record, None
        except RequestBudgetExhausted:
            raise
        except Exception as exc:
            kind = _classify_fetch_error(exc)
            if kind == "timeout" and attempt < retry_on_timeout:
                if capture_observer is not None:
                    record.error, record.error_kind = str(exc), kind
                    record.response_time = round(time.monotonic() - started, 3)
                    emit(reason="fetch_failed")
                    record.error, record.error_kind = "", ""
                attempt += 1
                if throttle is not None:
                    # Each failed attempt is real evidence the origin is struggling,
                    # not only the one a caller happens to give up on — widen the
                    # delay and collapse concurrency now, before the retry is
                    # dispatched, rather than after every retry has already gone
                    # out at the pre-timeout rate (#196).
                    throttle.record_timeout()
                if wait is not None:
                    # The one call before the loop paced the first attempt only; a
                    # retry is a real request too and must wait its own turn on the
                    # (now-widened) delay, or it reuses that first paced slot for
                    # every attempt this call makes.
                    wait()
                continue
            record.error = str(exc)
            record.error_kind = kind
            if throttle is not None and kind:
                # A connection failure (refused, reset, DNS, an aborted TLS handshake) never
                # produced a response either, and is exactly the failure throttle.py's own module
                # docstring names as the reason this breaker exists — an origin that "refused TLS
                # handshakes without ever returning an error status". It gets no lighter a
                # response than a timeout: both mean "this origin cannot currently be reached",
                # so both feed the same back-off and the same consecutive-failure counter (#132)
                # rather than leaving Throttle unable to see an entire class of dead host.
                throttle.record_timeout()
            emit(reason="fetch_failed")
            return record, None

    elapsed = time.monotonic() - started
    record.response_time = round(elapsed, 3)
    record.status_code = getattr(response, "status_code", None)
    headers = {k.lower(): v for k, v in dict(getattr(response, "headers", {})).items()}
    ok = record.status_code is not None and 200 <= record.status_code < 300
    if throttle is not None:
        throttle.record_response(elapsed, ok)
        code = record.status_code or 0
        if code == 429 or 500 <= code < 600:
            throttle.record_server_error(code, _retry_after(headers.get("retry-after")))
        else:
            throttle.record_success()

    if (
        fetcher is None
        and outcome is not None
        and outcome.status == "revalidate"
        and record.status_code == 304
    ):
        emit(reason="not_fetched", response_headers=headers)
        # response_time above already reflects the real 304 round trip, not zero.
        cache.refresh(outcome.entry, headers)
        parsed = _from_cache_entry(
            record,
            outcome.entry,
            "revalidated",
            parse_options,
            max_response_bytes,
            response_filter,
        )
        record.response_time = round(elapsed, 3)
        return record, parsed

    record.content_type = headers.get("content-type", "")
    record.x_robots = headers.get("x-robots-tag", "")
    # httpx transparently decodes gzip/br/deflate, but the header itself still
    # names the encoding that was actually on the wire (see check_compression).
    record.content_encoding = headers.get("content-encoding", "")
    # The raw HTTP Refresh response header (#385) -- rare, but a real redirect
    # mechanism some servers still send (e.g. "5; url=https://example.com/new").
    record.http_refresh = headers.get("refresh", "")
    # Location may be relative ("/new"); resolve it so the destination is a
    # real URL rather than a fragment the scope check then rejects as off-host.
    location = headers.get("location", "")
    record.redirect_url = urljoin(url, location) if location else ""

    if not media_filter_reason and response_filter is not None:
        media_filter_reason = response_filter(record.content_type)
    if media_filter_reason:
        record.body_unavailable = media_filter_reason
        if cache_eligible and outcome is not None and outcome.status == "bypass":
            record.cache_status = "bypass"
        elif cache_eligible and outcome is not None and record.status_code is not None:
            record.cache_status = "miss"
        if capture_observer is not None:
            emit(
                reason="unsupported_media",
                response_headers=headers,
                body_state="omitted",
            )
        return record, None

    if capture_failure is not None:
        record.error = "response too large or incomplete to parse: " + capture_failure
        record.size_bytes = capture_limit + 1 if capture_failure_reason == "truncated" else 0
        if record.is_html and capture_failure_reason == "truncated":
            record.body_unavailable = "oversized"
        if capture_failure_reason == "fetch_failed":
            record.error_kind = "decoding"
        emit(reason=capture_failure_reason, response_headers=headers)
        return record, None

    raw = captured_entity if captured_entity is not None else getattr(response, "content", None)
    wire_size = len(raw) if isinstance(raw, (bytes, bytearray)) else None
    body = captured_text if captured_text is not None else getattr(response, "text", "") or ""
    if capture_observer is not None:
        from seohead.crawl.capture import decode_entity

        entity = None
        reason = "legacy_not_retained"
        if record.status_code == 304:
            reason = "not_in_corpus"
        elif isinstance(raw, (bytes, bytearray)):
            if len(raw) > capture_limit:
                reason = "truncated"
            else:
                entity = bytes(raw)  # injected content is already content-decoded
                body = decode_entity(entity, record.content_type)[0]
                reason = "none"
        emit(entity, reason, headers)
    # "bypass" means the cache itself is unusable (e.g. an unsafe directory) rather than merely
    # empty for this URL; the page is still fetched normally, but it never touched the cache, so
    # it must not be stamped "miss" as if a lookup had actually happened -- and it must not be
    # left at "" either, since that value is reserved for "no cache was configured for this run
    # at all" (see the PageRecord.cache_status docstring). "bypass" is its own distinct value,
    # reusing CacheOutcome's own vocabulary for the status.
    if cache_eligible and outcome is not None and outcome.status == "bypass":
        record.cache_status = "bypass"
    if (
        cache_eligible
        and outcome is not None
        and outcome.status != "bypass"
        and record.status_code is not None
    ):
        cache.store(
            url,
            request_headers,
            record.status_code,
            headers,
            body,
            size_bytes=wire_size if wire_size is not None else len(body.encode("utf-8", "ignore")),
        )
        record.cache_status = "miss"
    parsed = _apply_body(record, url, body, parse_options, max_response_bytes, wire_size)
    # #1484: set after body application -- the parse merge carries no key for
    # transport, so nothing below can overwrite the caller's fact.
    record.transport_upgraded = transport_upgraded
    return record, parsed


def _retry_after(value: str | None) -> float | None:
    """Seconds from a Retry-After header. Only the numeric form is honoured."""
    if not value:
        return None
    try:
        return max(0.0, float(value.strip()))
    except ValueError:
        return None  # HTTP-date form: respected as "back off", not as a duration


def _classify_fetch_error(exc: BaseException) -> str:
    """Sort a fetch-time exception into "timeout", "connection", or "" (anything else).

    Structural, not a substring match on ``str(exc)`` — that was the bug (#132): a
    ``ConnectionResetError``'s message never contains the word "timeout", so a
    circuit breaker keyed on that word never saw it. httpx's real client path raises
    a typed hierarchy: ``httpx.TimeoutException`` covers Connect/Read/Write/Pool
    timeouts, while ``httpx.NetworkError`` and ``httpx.ProtocolError`` (both, like
    ``TimeoutException``, subclasses of ``httpx.TransportError``) cover a refused or
    reset connection, a DNS failure, and a TLS handshake aborted mid-negotiation —
    exactly the incident throttle.py's module docstring names as the reason this
    breaker exists. An injected ``fetcher`` (every test in this file, and every
    caller that is not the real network client) never raises an httpx type at all,
    only whatever the standard library would: ``TimeoutError`` (``socket.timeout``
    is the same class since Python 3.10) for a timeout, or ``OSError`` — its
    ``ConnectionResetError``/``ConnectionRefusedError``/``socket.gaierror``/
    ``ssl.SSLError`` subclasses — for a connection failure. Both layers are checked
    so classification does not depend on which transport raised.
    """
    if isinstance(exc, (httpx.TimeoutException, TimeoutError)):
        return "timeout"
    if isinstance(exc, (httpx.TransportError, OSError)):
        return "connection"
    return ""


def _robots_blocks(
    url: str,
    *,
    robots_cache: dict[tuple[str, str], Any],
    client: Any,
    fetcher: Callable[[str], Any] | None,
    user_agent: str,
    robots_token: str,
    wait: Callable[[], None] | None = None,
) -> bool:
    """True when the URL's host disallows it for ``robots_token``.

    List mode can touch many hosts in one run, so robots.txt is cached per
    host as it is encountered rather than resolved once up front the way a
    single-site crawl does (see ``seohead.crawl.spider._fetch_robots``) —
    there is no one site to fetch it against ahead of time. A host whose
    robots.txt cannot be read is treated as unrestricted: unlike a single-site
    crawl, list mode has no whole-run "unavailable means stop", because every
    URL here is independent by construction.
    """
    parts = urlsplit(url)
    key = (parts.scheme, parts.netloc)
    if key not in robots_cache:
        robots_url = f"{parts.scheme}://{parts.netloc}/robots.txt"
        text = ""
        try:
            bounded_reader = getattr(robots_cache, "fetch", None)
            if bounded_reader is not None:
                text = bounded_reader(
                    robots_url, client=client, fetcher=fetcher, user_agent=user_agent, wait=wait
                )
            else:
                if wait is not None:
                    wait()
                response = (
                    fetcher(robots_url)
                    if fetcher
                    else client.get(robots_url, headers={"User-Agent": user_agent or UA})
                )
                status = getattr(response, "status_code", None)
                if status is not None and status < 300:
                    text = getattr(response, "text", "") or ""
        except RequestBudgetExhausted:
            raise
        except Exception:
            text = ""  # unreadable robots.txt: treated as no restrictions, same as a 4xx
        robots_cache[key] = parse_robots(text)
    return not is_allowed(robots_cache[key], match_path(url), robots_token or "*")


def _resolve_redirect_destination(
    record: PageRecord,
    *,
    client: Any,
    fetcher: Callable[[str], Any] | None,
    throttle: Throttle,
    extra_headers: dict[str, str] | None,
    user_agent: str,
    max_response_bytes: int,
    retry_on_timeout: int,
    parse_options: dict[str, Any] | None,
    cache: ResponseCache | None,
    wait: Callable[[], None],
    capture_observer: Callable[[Any], None] | None = None,
    capture_max_bytes: int | None = None,
    headers_for_url: Callable[[str], dict[str, str] | None] | None = None,
) -> None:
    """Follow ``record``'s redirect past its first hop to where it actually lands.

    ``record`` already carries hop one: list mode always fetches the URL as
    given. This only continues past it — a migration audit needs the
    destination a redirect map's row lands on, not merely confirmation that a
    hop exists — stopping at ``MAX_REDIRECT_CHAIN_HOPS`` or a repeated URL (a
    loop) rather than trusting the chain to end on its own. Depth stays
    untouched: walking a redirect is not link discovery, so ``record`` still
    reads ``crawl_depth == 0`` once this returns.
    """
    visited = {record.url}
    current = record.redirect_url
    chain: list[dict[str, Any]] = []
    record.redirect_chain = chain
    while current and len(chain) < MAX_REDIRECT_CHAIN_HOPS:
        if current in visited:
            # A repeated URL closes a loop: mark the closing step, do not fetch it again.
            chain.append({"url": current, "loop": True})
            break
        visited.add(current)
        hop, _ = fetch_one(
            current,
            client=client,
            fetcher=fetcher,
            throttle=throttle,
            extra_headers=headers_for_url(current) if headers_for_url else extra_headers,
            user_agent=user_agent,
            max_response_bytes=max_response_bytes,
            retry_on_timeout=retry_on_timeout,
            parse_options=parse_options,
            cache=cache,
            wait=wait,
            **(
                {"capture_observer": capture_observer, "capture_max_bytes": capture_max_bytes}
                if capture_observer is not None
                else {}
            ),
        )
        chain.append(
            {
                "url": hop.url,
                "status_code": hop.status_code,
                "location": hop.redirect_url,
                "response_time": hop.response_time,
                "error": hop.error,
            }
        )
        record.final_url = hop.url
        if not hop.redirect_url:
            break
        current = hop.redirect_url
    record.redirect_chain = chain
    fetched = [hop for hop in chain if not hop.get("loop")]
    if fetched:
        record.final_url = fetched[-1]["url"]


def _resolve_canonical_destination(
    record: PageRecord,
    *,
    client: Any,
    fetcher: Callable[[str], Any] | None,
    throttle: Throttle,
    extra_headers: dict[str, str] | None,
    user_agent: str,
    max_response_bytes: int,
    retry_on_timeout: int,
    parse_options: dict[str, Any] | None,
    cache: ResponseCache | None,
    sleeper: Callable[[float], None],
    wait: Callable[[], None] | None = None,
    headers_for_url: Callable[[str], dict[str, str] | None] | None = None,
    capture_observer: Callable[[Any], None] | None = None,
    capture_max_bytes: int | None = None,
) -> None:
    """Follow canonical declarations as bounded per-row evidence.

    The initial list row remains the only collected page. Each extra fetch
    exists solely to inspect the target's own canonical declaration, just as
    redirect resolution walks a response chain without treating a hop as link
    discovery. A repeated target or the shared chain cap stops the walk.
    """
    visited = {record.url}
    current = record.canonical
    chain: list[dict[str, Any]] = []
    record.canonical_chain = chain
    while current and current not in visited and len(chain) < MAX_REDIRECT_CHAIN_HOPS:
        visited.add(current)
        hop, _ = fetch_one(
            current,
            client=client,
            fetcher=fetcher,
            throttle=throttle,
            extra_headers=headers_for_url(current) if headers_for_url else extra_headers,
            user_agent=user_agent,
            max_response_bytes=max_response_bytes,
            retry_on_timeout=retry_on_timeout,
            parse_options=parse_options,
            cache=cache,
            wait=wait or ((lambda: sleeper(throttle.delay)) if throttle.delay else None),
            **(
                {"capture_observer": capture_observer, "capture_max_bytes": capture_max_bytes}
                if capture_observer
                else {}
            ),
        )
        chain.append(
            {
                "url": hop.url,
                "status_code": hop.status_code,
                "canonical": hop.canonical,
                "response_time": hop.response_time,
                "error": hop.error,
            }
        )
        record.final_canonical = hop.url
        current = hop.canonical
    record.canonical_chain = chain
    if chain:
        record.final_canonical = chain[-1]["url"]


def collect_urls(
    urls: Iterable[str],
    *,
    max_urls: int = 500,
    max_requests: int = 0,
    max_seconds: float = 0,
    timeout: float = DEFAULT_TIMEOUT_S,
    min_delay: float = 0.0,
    out_path: str | None = None,
    fetcher: Callable[[str], Any] | None = None,
    sleeper: Callable[[float], None] = time.sleep,
    credential_headers: list[dict[str, Any]] | None = None,
    http_auth: list[dict[str, Any]] | None = None,
    clock: Callable[[], float] = time.monotonic,
    max_response_bytes: int = MAX_RESPONSE_BYTES,
    max_url_length: int = 2000,
    retry_on_timeout: int = 0,
    user_agent: str = "",
    stop_after_consecutive_timeouts: int = 5,
    max_delay_seconds: float = MAX_DELAY_S,
    parse_options: dict[str, Any] | None = None,
    cache: ResponseCache | None = None,
    extra_request_headers: dict[str, str] | None = None,
    adaptive: bool = True,
    robots_policy: str = "ignore",
    robots_token: str = "*",
    resolve_redirect_destination: bool = False,
    resolve_canonical_destination: bool = False,
    proxy_route: Any = None,
    spool: Any = None,
    upgrade_to_https: bool = False,
) -> CrawlResult:
    """Fetch an explicit list of URLs in the order given.

    ``spool`` is the internal native-list storage adapter. The ordinary Python
    call still returns in-memory pages; shared CLI/MCP handlers own native scans.

    ``out_path`` receives one JSON object per line as each URL completes, so an
    interrupted run still leaves usable evidence behind. ``max_seconds`` is a
    wall-clock budget for the whole call; 0 means none. ``max_url_length`` is
    checked before a URL is fetched, not merely before it is parsed: a URL over
    the limit is skipped rather than requested.
    wall-clock budget for the whole call; 0 means none.

    ``parse_options`` is forwarded to every ``parse_html`` call unchanged; see
    wall-clock budget for the whole call; 0 means none. ``cache``, when given, is checked before
    the delay is applied, so a hit never waits for a delay slot it did not need — see
    ``fetch_one``.

    ``robots_policy`` defaults to ``"ignore"`` here, not to ``"respect"`` the
    way a fresh crawl config resolves it: list mode's very reason to exist is
    checking exactly the URLs you handed it, so silently applying the
    site-wide default the moment nobody says otherwise would be its own kind
    of silent policy change. ``seohead.mcp.handlers.crawl_site`` passes
    the configured ``robots.policy`` explicitly, which is what makes the
    policy "whatever the configuration says" rather than hard-coded either
    way — see #21. ``"respect"`` drops a disallowed URL from ``pages``
    entirely (into ``robots_blocked`` instead); ``"report_only"`` fetches it
    anyway and only records that it would have been blocked.

    ``resolve_canonical_destination`` is the canonical equivalent of the
    redirect option: it follows a page's canonical declaration through a
    bounded chain without adding any target to ``result.pages``.
    """
    limit = checked_url_budget(max_urls, materialized=spool is None)
    result = CrawlResult()
    throttle = Throttle(min_delay=min_delay, max_delay=max_delay_seconds, adaptive=adaptive)
    dispatch_gate = DispatchGate(
        throttle,
        sleeper,
        clock,
        max_requests=max_requests,
        requests_used=spool.requests_used if spool else 0,
        event_callback=spool.dispatched if spool else None,
    )
    started = clock() - (spool.elapsed_before if spool else 0)
    if spool is not None:
        spool.bind(result, throttle, dispatch_gate)
        urls = spool.pending()

    def time_exhausted():
        return bool(
            max_seconds
            and (
                (spool.interrupted_clock if spool else False)
                or (spool.elapsed() if spool else clock() - started) >= max_seconds
            )
        )

    def wait_turn():
        if time_exhausted():
            raise DurationBudgetExhausted("cumulative list time budget exhausted")
        dispatch_gate.wait_turn()
        if time_exhausted():
            raise DurationBudgetExhausted("cumulative list time budget exhausted")

    if spool is not None and (
        (max_requests and dispatch_gate.requests_used >= max_requests) or time_exhausted()
    ):
        result.partial = True
        result.finish_reason = "duration_limit" if time_exhausted() else "request_limit"
        result.stopped_reason = "the retained list request or time budget is exhausted"
        return result

    seen = set() if spool is None else None
    robots_cache = {} if spool is None else spool.robots

    def headers_for_url(target: str) -> dict[str, str] | None:
        headers = dict(extra_request_headers or {})
        if credential_headers:
            headers.update(
                resolve_credential_headers(credential_headers, urlsplit(target).hostname or "")
                or {}
            )
        return headers or None

    with contextlib.ExitStack() as stack:
        handle = None
        if out_path:
            os.makedirs(os.path.dirname(os.path.abspath(out_path)) or ".", exist_ok=True)
            handle = stack.enter_context(
                open(out_path, "a" if spool and spool.resumed else "w", encoding="utf-8")
            )

        client = None
        if fetcher is None:
            # A crawler must observe redirects, not be moved by them. With
            # follow_redirects on, a 301 is recorded as a 200 carrying the
            # target's title and body, the Location is never seen, and redirect
            # auditing is impossible — the old and new URL become duplicates.
            from seohead.crawl.auth import host_bound_auth
            from seohead.recon.net import crawl_transport_options

            client_headers = {"User-Agent": user_agent or UA}
            client_headers.update(extra_request_headers or {})
            client, _ = http_client(
                timeout,
                follow_redirects=False,
                headers=client_headers,
                auth=host_bound_auth(http_auth or [], dispatch_gate.wait_turn),
                **crawl_transport_options(proxy_route),
            )
            stack.callback(client.close)

        for raw in urls:
            if limit and len(result.pages) >= limit:
                result.partial = True
                result.stopped_reason = f"url limit reached ({limit})"
                result.finish_reason = "url_limit"
                break
            if time_exhausted():
                result.partial = True
                result.stopped_reason = f"duration limit reached ({max_seconds:.0f}s)"
                result.finish_reason = "duration_limit"
                break
            url = (raw or "").strip()
            if not url or (seen is not None and url in seen):
                continue
            if max_url_length and len(url) > max_url_length:
                # Not fetched at all, per limits.max_url_length: too long even
                # to be worth a wasted request.
                continue
            if seen is not None:
                seen.add(url)

            record = None
            try:
                if robots_policy != "ignore" and _robots_blocks(
                    url,
                    robots_cache=robots_cache,
                    client=client,
                    fetcher=fetcher,
                    user_agent=user_agent,
                    robots_token=robots_token,
                    wait=wait_turn,
                ):
                    result.robots_blocked.append(url)
                    if robots_policy == "respect":
                        continue  # report_only still fetches it below

                # http.headers goes on every request; a credential is bound to one host.
                extra_headers = headers_for_url(url)
                fetched_url, was_upgraded = (
                    (_upgrade_transport(url), True)
                    if upgrade_to_https and url.startswith("http://")
                    else (url, False)
                )
                url = fetched_url
                record, _ = fetch_one(
                    url,
                    transport_upgraded=was_upgraded,
                    client=client,
                    fetcher=fetcher,
                    throttle=throttle,
                    extra_headers=extra_headers,
                    user_agent=user_agent,
                    max_response_bytes=max_response_bytes,
                    retry_on_timeout=retry_on_timeout,
                    parse_options=parse_options,
                    cache=cache,
                    wait=wait_turn,
                    **(
                        {"capture_observer": spool.capture, "capture_max_bytes": max_response_bytes}
                        if spool
                        else {}
                    ),
                )
                if resolve_redirect_destination and record.redirect_url:
                    _resolve_redirect_destination(
                        record,
                        client=client,
                        fetcher=fetcher,
                        throttle=throttle,
                        extra_headers=extra_headers,
                        user_agent=user_agent,
                        max_response_bytes=max_response_bytes,
                        retry_on_timeout=retry_on_timeout,
                        parse_options=parse_options,
                        cache=cache,
                        wait=wait_turn,
                        headers_for_url=headers_for_url,
                        **(
                            {
                                "capture_observer": spool.capture,
                                "capture_max_bytes": max_response_bytes,
                            }
                            if spool
                            else {}
                        ),
                    )
                if resolve_canonical_destination and record.canonical:
                    _resolve_canonical_destination(
                        record,
                        client=client,
                        fetcher=fetcher,
                        throttle=throttle,
                        extra_headers=extra_headers,
                        user_agent=user_agent,
                        max_response_bytes=max_response_bytes,
                        retry_on_timeout=retry_on_timeout,
                        parse_options=parse_options,
                        cache=cache,
                        sleeper=sleeper,
                        wait=wait_turn,
                        headers_for_url=headers_for_url,
                        **(
                            {
                                "capture_observer": spool.capture,
                                "capture_max_bytes": max_response_bytes,
                            }
                            if spool
                            else {}
                        ),
                    )
            except RequestBudgetExhausted as exc:
                if record is not None:
                    result.pages.append(record)
                    _write(handle, record)
                result.partial = True
                result.finish_reason = (
                    "duration_limit"
                    if isinstance(exc, DurationBudgetExhausted)
                    else "request_limit"
                )
                result.stopped_reason = (
                    f"duration limit reached ({max_seconds:.0f}s)"
                    if isinstance(exc, DurationBudgetExhausted)
                    else f"total HTTP request budget reached ({max_requests})"
                )
                break
            result.pages.append(record)
            _write(handle, record)

            if throttle.should_stop(limit=stop_after_consecutive_timeouts):
                result.partial = True
                # throttle.timeouts now counts both real timeouts and response-less connection
                # failures (refused/reset/DNS/TLS) — see _classify_fetch_error — so the message
                # must not claim more precisely than that which one actually happened.
                result.stopped_reason = (
                    "origin stopped responding (repeated timeouts or connection failures)"
                )
                result.finish_reason = "errors"
                break
            if throttle.host_is_failing():
                result.partial = True
                result.stopped_reason = "origin refused repeatedly (429/5xx) — crawl stopped"
                result.finish_reason = "errors"
                break

    result.limitations = [
        "list mode: no link discovery, no sitemap expansion",
        "static HTML only: no JavaScript rendering",
    ]
    if cache is not None:
        result.cache_stats = dict(cache.stats)
        result.cache_replay = cache.mode == "replay"
    return result


def _write(handle, record: PageRecord) -> None:
    if handle is None:
        return
    handle.write(json.dumps(asdict(record), ensure_ascii=False) + "\n")
    handle.flush()
