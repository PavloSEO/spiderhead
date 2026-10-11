"""Breadth-first link discovery — the part that makes this a crawler.

Fetching a list someone else produced tests the fetcher. Following links is the
thing that closes the gap: without it the toolkit cannot obtain a URL list of
its own, which is the whole reason this module exists.

Traversal is deterministic given identical responses: the frontier is a queue,
children are enqueued in document order rather than sorted, and every exclusion
is recorded as data rather than dropped silently. That guarantee survives
concurrency too — a slice of the frontier is fetched as a batch of concurrent
requests, but results are always folded back in the order they were queued —
into ``result.pages``, the circuit breaker, redirect and link enqueueing, and
the checkpoint — before anything downstream sees them, so the recorded output
does not depend on which request happened to answer first.
"""

from __future__ import annotations

import contextlib
import dataclasses
import json
import os
import re
import time
from collections import deque
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from itertools import zip_longest
from pathlib import PurePosixPath
from typing import Any
from urllib.parse import urldefrag, urljoin, urlsplit, urlunsplit

from seohead.checks.robots import is_allowed, match_path, parse_robots, politeness_delay
from seohead.core.models import ParsedRobots
from seohead.crawl import state as crawl_state
from seohead.crawl.cache import ResponseCache
from seohead.crawl.collect import (
    MAX_RESPONSE_BYTES,
    CrawlResult,
    PageRecord,
    _write,
    fetch_one,
)
from seohead.crawl.external import ExternalCheck, ExternalPolicy, run_external_checks
from seohead.crawl.settings import (
    DEFAULT_SEGMENT,
    checked_url_budget,
    resolve_credential_headers,
)
from seohead.crawl.throttle import MAX_DELAY_S, DispatchGate, Throttle
from seohead.recon.net import UA, http_client, normalize_url, registrable_domain

_MEDIA_TYPE_TOKEN = re.compile(r"[a-z0-9!#$%&'+\-.^_`|~]+\Z")
_MEDIA_TYPE_FILTER_REASONS = frozenset(
    {"excluded_by_media_type", "not_included_by_media_type", "media_type_unavailable"}
)

MAX_DEPTH_CEILING = 20
ROBOTS_TOKEN = "SpiderHeadBot"
# The same shape ``parse_robots`` returns, because this stands in for its result on
# every path where robots.txt could not be read: a different shape here is not an
# empty ruleset, it is a ruleset the scan artifact refuses to store (#629).
EMPTY_ROBOTS: ParsedRobots = {"groups": [], "sitemaps": []}
# RFC 9309 §2.3.1.2 asks crawlers to follow "at least five consecutive redirects"
# before giving up on robots.txt — this is that number, not an arbitrary one.
MAX_ROBOTS_REDIRECTS = 5
# Matches Throttle.should_stop / host_is_failing's own default limit — kept as
# a separate constant here because the circuit breaker's *decision* is no
# longer read off Throttle's live counters (see _fold_failure_streaks below).
STOP_AFTER_CONSECUTIVE_FAILURES = 5


_DispatchGate = DispatchGate


@dataclass
class LinkEdge:
    source: str
    destination: str
    anchor: str
    # Derived from rel at parse time ("nofollow" in its tokens) and kept as its own field —
    # the convenience every existing caller already reads — regardless of whether the fuller
    # rel/target/raw_href below were captured for this edge.
    nofollow: bool
    # Where the link sits in the DOM (nav/header/sidebar/footer/content/other;
    # see tools/link_position.py). Empty when the crawl did not classify links
    # (the default): storing this per link on a large crawl is a real memory
    # cost, so it is switchable, and leaving it off means the position of every
    # edge is simply unmeasured rather than a false "content" or "".
    position: str = ""
    # The full rel token set, the target attribute, and the href exactly as written before
    # resolution -- gated by link_attributes.capture (default off), the same
    # shape as position above. Measured on a synthetic 3387-page/150-link-per-page crawl
    # (~508k edges, tests/chains/chain_site.py's own fixture is far smaller): with realistic
    # attribute rates (~10-12% of links carry a target or rel, raw_href present on nearly all
    # of them) these three fields add roughly 95 bytes/edge -- about +53% over the ~179
    # bytes/edge this dataclass already costs -- ~46 MiB total for that crawl. That is
    # `raw_href` doing almost all of the damage (a second URL-shaped string per edge); `rel`
    # and `target` alone cost only a few bytes/edge each. Kept as one flag rather than three
    # because a caller wanting cross-origin/protocol-relative detection needs all three
    # together, and splitting them would not change which ones are actually expensive to have
    # both on.
    rel: tuple[str, ...] = ()
    target: str = ""
    raw_href: str = ""


@dataclass
class FormEdge:
    """One ``<form>`` recorded during a crawl (issue #125).

    Unlike ``LinkEdge``'s optional attributes above, this carries no memory-cost caveat:
    forms are rare compared to links -- a handful per page at most -- so there is no
    per-crawl total worth trading off, and it is always recorded.
    """

    page: str
    method: str
    action: str
    has_password: bool


@dataclass
class SpiderResult(CrawlResult):
    links: list[LinkEdge] = field(default_factory=list)
    # Every form found while parsing, regardless of settings -- see FormEdge's own docstring
    # on why this one is unconditional where LinkEdge's rel/target/raw_href are not.
    forms: list[FormEdge] = field(default_factory=list)
    excluded: dict[str, int] = field(default_factory=dict)
    max_depth_reached: int = 0
    robots_note: str = ""
    # URLs robots.txt disallows. Under "report_only" they are crawled anyway and
    # listed here, which is what an audit needs: full coverage plus an inventory
    # of what a compliant crawler would not have seen.
    robots_blocked: list[str] = field(default_factory=list)
    # Effective robots-derived interval: the stricter applicable Crawl-delay
    # or Request-rate floor, after the configured floor is considered.
    crawl_delay_applied: float | None = None
    effective_delay: float = 0.0
    # The adaptive concurrency level reached by the end of the crawl. 1 means
    # the crawl never ran more than one request in flight at a time, whether
    # because it was configured that way or because the origin never earned
    # more.
    effective_concurrency: int = 1
    # Why the checkpoint was or wasn't used, for the run output — see state.py.
    resume_note: str = ""
    # Seed URLs accepted into the frontier beyond the start URL itself (e.g. a
    # sitemap's declared URL set). Recorded so a sitemap-seeded run is
    # auditable: which URLs were fetched only because they were seeded, versus
    # discovered by following a link.
    seed_urls: list[str] = field(default_factory=list)
    # The start page's raw HTML and outlink counts, captured once as a
    # by-product of the ordinary fetch (never an extra request). This is what
    # lets seohead.crawl.render_escalation's pre-flight gate (#18) check for
    # an empty SPA shell or a link-less start page even in "raw" mode, which
    # has no render to fall back on. Empty when the start page was not
    # (re-)fetched in this call, e.g. a resumed run that starts past depth 0.
    start_page_evidence: dict[str, Any] = field(default_factory=dict)
    # The legacy output route may keep evidence in JSONL instead of duplicating
    # it in these lists. Direct callers keep the original in-memory contract.
    spooled_evidence: bool = False
    page_count: int = 0
    link_count: int = 0
    form_count: int = 0

    # Opt-in bounded check of recorded external destinations (see
    # seohead.crawl.external — enabled by discovery.external.crawl). Empty
    # unless that option ran: an absent check is itself a fact the summary
    # below records, so "not checked" never masquerades as "checked clean".
    external_checks: list[ExternalCheck] = field(default_factory=list)
    # The external phase's own coverage statement — policy used, per-outcome
    # counts, why it stopped — kept apart from the internal frontier's
    # finish_reason/partial because internal completeness and external-check
    # coverage are different measurements (#746).
    external_summary: dict[str, Any] = field(default_factory=dict)


def _canonical_key(url: str) -> str:
    """Identity for de-duplication: fragment dropped, nothing else rewritten."""
    parts = urlsplit(url)
    return urlunsplit((parts.scheme, parts.netloc, parts.path or "/", parts.query, ""))


def _strip_fragment(url: str) -> str:
    """The request target for a frontier entry.

    A fragment selects nothing on the server, so keeping it on the queued and
    fetched URL made a fragment-only variant of a page its own PageRecord
    (#194) even though ``_canonical_key`` above already treats the two as one
    identity for de-duplication. Unlike that helper the path is left exactly as
    given -- this value is what actually gets requested, not merely compared --
    which is precisely what ``urldefrag`` does, so it does the work.
    """
    return urldefrag(url).url


def _same_host(url: str, host: str) -> bool:
    return (urlsplit(url).hostname or "").lower() == host


def _fold_failure_streaks(
    record: PageRecord, consecutive_timeouts: int, consecutive_server_errors: int
) -> tuple[int, int]:
    """Advance the two failure streaks by exactly one record, in queue order.

    Mirrors Throttle.record_timeout / record_server_error / record_success —
    the same rules, applied to the *sequence of folded-back records* instead
    of Throttle's live counters. Those counters are mutated inside worker
    threads as each fetch actually completes, which is completion order, not
    queue order; reading them straight from ``after_fetch`` would make the
    circuit breaker's trip point depend on real thread scheduling instead of
    on the deterministic order the rest of the fold-back already uses. An
    exception that carries no origin-health signal at all (``status_code``
    never set, ``error_kind`` empty — a bug in caller code, say) leaves both
    streaks untouched, matching ``fetch_one``: it calls none of Throttle's
    mutators in that case either. ``error_kind`` — not a fresh string check on
    ``record.error`` — is what fetch_one itself used to decide whether to call
    ``record_timeout()``, so reading it back here is reading the same decision,
    not re-deriving a second, possibly divergent one (#132).
    """
    if record.status_code is None:
        return (
            (consecutive_timeouts + 1, consecutive_server_errors)
            if record.error_kind  # "timeout" or "connection" — see PageRecord.error_kind
            else (consecutive_timeouts, consecutive_server_errors)
        )
    if record.status_code == 429 or 500 <= record.status_code < 600:
        return consecutive_timeouts, consecutive_server_errors + 1
    return 0, 0


def _continues_failure_streak(record: PageRecord) -> bool:
    """True when ``record`` is the same kind of failure the breaker counts.

    Used only to decide whether an already-completed request from a batch
    the breaker has already tripped on should be merged as evidence — see
    the concurrent branch of ``crawl_site`` (#475). It mirrors the two
    failure arms of ``_fold_failure_streaks`` without needing the running
    counters: any record that would advance either streak is one more of
    the failure that already spent the caller's configured tolerance.
    """
    if record.status_code is None:
        return bool(record.error_kind)
    return record.status_code == 429 or 500 <= record.status_code < 600


_PAGE_RECORD_FIELDS = {f.name for f in dataclasses.fields(PageRecord)}


def _jsonl_rows(path: str, *, checkpoint: bool = False):
    """Stream objects; checkpoint append requires intact, newline-terminated rows."""
    try:
        with open(path, encoding="utf-8") as handle:
            for line in handle:
                if not line.strip():
                    continue
                if checkpoint and not line.endswith("\n"):
                    raise ValueError(f"checkpoint evidence sidecar has an unterminated row: {path}")
                try:
                    raw = json.loads(line)
                except ValueError as exc:
                    if checkpoint:
                        raise ValueError(
                            f"checkpoint evidence sidecar has invalid JSON: {path}"
                        ) from exc
                    continue
                if isinstance(raw, dict):
                    yield raw
                elif checkpoint:
                    raise ValueError(f"checkpoint evidence sidecar row is not an object: {path}")
    except FileNotFoundError:
        if checkpoint:
            raise ValueError(f"checkpoint evidence sidecar is missing: {path}") from None
        return
    except UnicodeError as exc:
        if checkpoint:
            raise ValueError(f"checkpoint evidence sidecar is not UTF-8: {path}") from exc
        raise


def _read_pages_jsonl(path: str) -> list[PageRecord]:
    """Reconstruct previously fetched pages from a prior run's output.

    Unknown keys are dropped rather than rejected, so a state file written by
    an older build with fewer fields still resumes.
    """
    return [
        PageRecord(**{k: v for k, v in raw.items() if k in _PAGE_RECORD_FIELDS})
        for raw in _jsonl_rows(path)
    ]


_LINK_EDGE_FIELDS = {f.name for f in dataclasses.fields(LinkEdge)}


def _write_link(handle, edge: LinkEdge) -> None:
    if handle is None:
        return
    handle.write(json.dumps(dataclasses.asdict(edge), ensure_ascii=False) + "\n")
    handle.flush()


def _write_decision(handle, entry: dict[str, Any]) -> None:
    """Append one structured decision record (issue #134).

    Redaction is reused from ``runlog`` rather than duplicated: a decision log
    is the same kind of artifact — an append-only record of what a run did —
    and must be held to the same "nothing secret in it" rule.
    """
    if handle is None:
        return
    from seohead.core import runlog

    handle.write(json.dumps(runlog.safe_arguments(entry), ensure_ascii=False) + "\n")
    handle.flush()


def _iter_links_jsonl(path: str):
    """Reconstruct the link graph recorded before a checkpoint.

    Edges are appended to this sidecar as they are found (see ``_write_link``)
    rather than embedded in ``crawl_state.json``, the same reasoning that keeps
    pages out of it: ``links`` is the largest structure a long crawl builds up,
    so a resume must not pay to reserialise the whole thing on every checkpoint
    save — only ever appending what is new. Unknown keys are dropped rather
    than rejected, so a file written by an older build still resumes.
    """
    for raw in _jsonl_rows(path):
        fields = {k: v for k, v in raw.items() if k in _LINK_EDGE_FIELDS}
        # rel is a tuple in memory but a list once it has been through JSON.
        if "rel" in fields:
            fields["rel"] = tuple(fields["rel"] or ())
        yield LinkEdge(**fields)


def _read_links_jsonl(path: str) -> list[LinkEdge]:
    return list(_iter_links_jsonl(path))


def _read_forms_jsonl(path: str) -> list[FormEdge]:
    return [FormEdge(**raw) for raw in _jsonl_rows(path)]


def _read_external_jsonl(path: str) -> list[ExternalCheck]:
    """Reconstruct the external-destination outcomes a prior run recorded.

    Same append-only sidecar contract as ``links.jsonl``: a truncated final
    line must not discard the rest, and a missing file just means the earlier
    run stopped before its first check.
    """
    try:
        with open(path, encoding="utf-8") as handle:
            lines = handle.readlines()
    except FileNotFoundError:
        return []
    records = []
    for line in lines:
        if not line.strip():
            continue
        try:
            raw = json.loads(line)
        except ValueError:
            continue  # a truncated final line must not discard the rest
        if isinstance(raw, dict):
            records.append(ExternalCheck.from_dict(raw))
    return records


def form_edges(parsed: dict[str, Any] | None, source_url: str) -> tuple[list[FormEdge], int]:
    """Translate the parser's retained form observations without side effects.

    The optional omitted count is emitted only by the SQLite collector's opt-in
    parser cap. Legacy parsing has no such key and therefore keeps its current
    complete list and zero omitted forms.
    """
    forms = [
        FormEdge(
            page=source_url,
            method=form.get("method") or "get",
            action=form.get("action") or "",
            has_password=bool(form.get("has_password")),
        )
        for form in (parsed or {}).get("forms") or []
    ]
    omitted = int((parsed or {}).get("forms_omitted") or 0)
    return forms, omitted


def apply_document_links(
    parsed: dict[str, Any] | None,
    source_url: str,
    depth: int,
    *,
    depth_limit: int,
    host: str,
    rejection: Callable[[str, str], str],
    discover: Callable[[str, int, str], str | None],
    store_hyperlinks: bool,
    store_external_links: bool,
    crawl_hyperlinks: bool,
    follow_nofollow: bool,
    capture_link_attributes: bool,
    record_edge: Callable[[LinkEdge], None],
    reject: Callable[[str, str | None], None],
    mark_link_partial: Callable[[int], None],
) -> None:
    """Apply one parsed document's link observations in crawler order.

    ``discover`` owns stateful query reservation and identity deduplication. It
    is deliberately called after storage, discovery and nofollow gates: moving
    it earlier spends a query-variant slot on a link that was never eligible to
    reach the frontier (#193). The legacy crawler supplies its deque/set closure;
    the SQLite adapter supplies one transaction-local reservation callback.
    """
    if parsed is None:
        return
    if depth_limit >= 0 and depth >= depth_limit:
        reject("depth_limit", None)
        return
    for link in parsed.get("links") or []:
        href = (link.get("href") or "").strip()
        if not href:
            continue
        target = _strip_fragment(href)
        nofollow = bool(link.get("nofollow"))
        scope_reason = rejection(target, host)
        is_external = scope_reason == "outside_host"
        if store_external_links if is_external else store_hyperlinks:
            edge = LinkEdge(
                source=source_url,
                destination=href,
                anchor=(link.get("text") or "")[:200],
                nofollow=nofollow,
                position=link.get("position") or "",
            )
            if capture_link_attributes:
                edge.rel = tuple((link.get("rel") or "").split())
                edge.target = link.get("target") or ""
                edge.raw_href = link.get("raw_href") or ""
            record_edge(edge)
        if not crawl_hyperlinks:
            reject("hyperlink_discovery_off", href)
            continue
        if nofollow and not follow_nofollow:
            reject("nofollow", href)
            continue
        reason = scope_reason or discover(target, depth + 1, href)
        if reason:
            reject(reason, href)
    omitted = int((parsed.get("link_observation") or {}).get("omitted") or 0)
    if omitted:
        mark_link_partial(omitted)


@dataclass(frozen=True)
class SegmentRule:
    """One named segment: matches a URL by path prefix, exact host, or a regex over
    the whole URL -- the three shapes a multilingual or multi-regional site uses
    (path prefix, subdomain, or a wholly separate domain -- #358).

    Evaluated in declaration order by ``Scope.segment_for`` -- first match wins,
    so overlapping rules are predictable rather than accidental, exactly as #21
    specifies for segment rules in general.
    """

    name: str
    prefix: str = ""
    host: str = ""
    pattern: re.Pattern[str] | None = None

    def matches(self, url: str, path: str, host: str) -> bool:
        if self.prefix and path.startswith(self.prefix):
            return True
        if self.host and host == self.host:
            return True
        return bool(self.pattern and self.pattern.search(url))


@dataclass(frozen=True)
class Scope:
    """Which discovered URLs a crawl may fetch.

    The seed is always fetched: a crawl whose own start URL is filtered out
    would report an empty site rather than a configuration mistake. Everything
    reached from it is tested here, and every rejection is counted in
    ``SpiderResult.excluded`` under the rule that rejected it.
    """

    internal: str = "host"
    include_patterns: tuple[re.Pattern[str], ...] = ()
    exclude_patterns: tuple[re.Pattern[str], ...] = ()
    include_extensions: frozenset[str] = frozenset()
    exclude_extensions: frozenset[str] = frozenset()
    include_media_types: tuple[str, ...] = ()
    exclude_media_types: tuple[str, ...] = ()
    exclude_hosts: frozenset[str] = frozenset()
    segments: tuple[SegmentRule, ...] = ()
    segments_only: frozenset[str] = frozenset()

    @classmethod
    def from_config(cls, scope: dict[str, Any] | None) -> Scope:
        scope = scope or {}
        return cls(
            internal=scope.get("internal", "host"),
            include_patterns=tuple(re.compile(p) for p in scope.get("include_patterns") or ()),
            exclude_patterns=tuple(re.compile(p) for p in scope.get("exclude_patterns") or ()),
            include_extensions=frozenset(
                value.removeprefix(".").lower() for value in scope.get("include_extensions") or ()
            ),
            exclude_extensions=frozenset(
                value.removeprefix(".").lower() for value in scope.get("exclude_extensions") or ()
            ),
            include_media_types=tuple(
                value.lower() for value in scope.get("include_media_types") or ()
            ),
            exclude_media_types=tuple(
                value.lower() for value in scope.get("exclude_media_types") or ()
            ),
            exclude_hosts=frozenset(
                host.lower().lstrip(".") for host in scope.get("exclude_hosts") or ()
            ),
            segments=tuple(
                SegmentRule(
                    name=item["name"],
                    prefix=item.get("prefix") or "",
                    host=(item.get("host") or "").lower(),
                    pattern=re.compile(item["pattern"]) if item.get("pattern") else None,
                )
                for item in scope.get("segments") or ()
            ),
            segments_only=frozenset(scope.get("segments_only") or ()),
        )

    def segment_for(self, url: str) -> str:
        """The one named segment this URL belongs to -- every URL gets exactly
        one, a declared segment or the built-in ``DEFAULT_SEGMENT`` (#358)."""
        parts = urlsplit(url)
        host = (parts.hostname or "").lower()
        for rule in self.segments:
            if rule.matches(url, parts.path, host):
                return rule.name
        return DEFAULT_SEGMENT

    def is_internal(self, url: str, start_host: str) -> bool:
        host = (urlsplit(url).hostname or "").lower()
        if not host:
            return False
        # A segment naming an exact host is the operator declaring that host part
        # of this crawl -- a subdomain or a wholly separate domain (#358) -- so it
        # is internal regardless of scope.internal, which is what lets "separate
        # domain" segments work without also being asked to crawl the entire web.
        if any(host == rule.host for rule in self.segments if rule.host):
            return True
        if self.internal == "registrable_domain":
            return registrable_domain(host) == registrable_domain(start_host)
        return host == start_host

    def rejection(self, url: str, start_host: str) -> str:
        """The rule that rejects this URL, or "" when it may be fetched."""
        if not self.is_internal(url, start_host):
            return "outside_host"
        host = (urlsplit(url).hostname or "").lower()
        if any(host == bad or host.endswith("." + bad) for bad in self.exclude_hosts):
            return "excluded_host"
        if any(pattern.search(url) for pattern in self.exclude_patterns):
            return "excluded_by_pattern"
        if self.include_patterns and not any(p.search(url) for p in self.include_patterns):
            return "not_included_by_pattern"
        if self.segments_only and self.segment_for(url) not in self.segments_only:
            return "outside_segment"
        suffix = PurePosixPath(urlsplit(url).path).suffix
        extension = suffix[1:].lower() if suffix else ""
        if extension and extension in self.exclude_extensions:
            return "excluded_by_extension"
        if self.include_extensions and extension not in self.include_extensions:
            return "not_included_by_extension"
        return ""

    def response_media_rejection(self, content_type: str) -> str:
        """Return a body-filter reason for a response Content-Type, if any.

        This is separate from ``rejection``: suffixes are checked before a
        request, while a response media type exists only after the server
        answers. Missing or malformed types remain eligible unless an
        allowlist makes a positive type match necessary.
        """
        media_type = (content_type or "").split(";", 1)[0].strip().lower()
        if not media_type or media_type.count("/") != 1:
            return "media_type_unavailable" if self.include_media_types else ""
        major, minor = media_type.split("/", 1)
        if not _MEDIA_TYPE_TOKEN.fullmatch(major) or not _MEDIA_TYPE_TOKEN.fullmatch(minor):
            return "media_type_unavailable" if self.include_media_types else ""

        def matches(pattern: str) -> bool:
            return (
                media_type.startswith(pattern[:-1])
                if pattern.endswith("/*")
                else media_type == pattern
            )

        if any(matches(pattern) for pattern in self.exclude_media_types):
            return "excluded_by_media_type"
        if self.include_media_types and not any(
            matches(pattern) for pattern in self.include_media_types
        ):
            return "not_included_by_media_type"
        return ""


def _fetch_robots(
    start: str,
    fetcher: Callable[[str], Any] | None,
    client: Any,
    *,
    wait: Callable[[], None] | None = None,
) -> tuple[dict, str, bool]:
    """Read robots.txt. Returns ``(parsed_or_empty, note, unavailable)``.

    ``unavailable`` is true only when the fetch itself failed (network error),
    the server (or the final hop of a redirect, see below) answered 429/5xx, or a
    redirect could not be trusted — a host that could not say what is
    disallowed, as distinct from one that has nothing to say. A 404 or another
    non-429 4xx robots.txt means "no restrictions" per RFC 9309 and is not
    "unavailable". What
    happens when it is unavailable — stop the crawl, or continue as if
    unrestricted — is ``robots.unavailable_means_stop``, decided by the
    caller: RFC 9309 treats an unavailable robots.txt as a full disallow, and
    the practical reason for the default (stop) is sharper than the standard —
    a host answering 5xx is already failing, and crawling it harder is the
    wrong response.

    ``client`` is the crawl's own content-fetching client, built with
    ``follow_redirects=False`` so a 3xx on a *page* stays visible as a 3xx
    (see ``crawl_site``). robots.txt has no such requirement — nothing reads
    its Location header — so a 3xx here is followed by hand, hop by hop,
    rather than reusing that client's redirect behaviour as-is (which is how
    this function used to read a redirect's own near-always-empty body as the
    whole ruleset: fully permissive, and silently so). Three things bound how
    far "followed" goes, each because trusting it unconditionally reopens the
    same silent-permissive hole from a different angle:

    - hop count: ``MAX_ROBOTS_REDIRECTS`` hops, and a repeated URL is caught
      as a loop before that budget even runs out;
    - host: a redirect off the crawled site's registrable domain is not
      followed — a robots.txt fetched from somebody else's server (accidental
      misconfiguration or deliberate) must not govern this crawl, only its
      own robots.txt may; the crawl's *content* fetches make an equivalent
      call for the exact same reason (``excluded["redirect_off_host"]``);
    - content type: a redirect that resolves to a non-``text/plain`` body
      (an HTML error page, a login wall) is not parsed as robots.txt just
      because ``parse_robots`` will not choke on it — it will simply find no
      ``User-agent:`` lines and return the same empty, permissive ruleset
      this whole function exists to stop producing.

    Every one of those three outcomes is reported as ``unavailable=True``
    with a specific note, so ``unavailable_means_stop`` (and the crawl's
    ``robots_note``) reflect what actually happened rather than reading as an
    ordinary, unrestricted robots.txt.
    """
    parts = urlsplit(start)
    site_domain = registrable_domain(parts.hostname or "")
    url = urlunsplit((parts.scheme, parts.netloc, "/robots.txt", "", ""))
    visited = {url}
    hops = 0
    redirected = False
    while True:
        try:
            if wait is not None:
                wait()
            response = fetcher(url) if fetcher else client.get(url)
        except Exception as exc:
            return dict(EMPTY_ROBOTS), f"robots.txt unreachable: {exc}", True
        code = getattr(response, "status_code", None)
        if code is None or not (300 <= code < 400):
            break
        location = (getattr(response, "headers", None) or {}).get("location", "")
        if not location:
            break  # a redirect status with no destination is just the final response
        hops += 1
        if hops > MAX_ROBOTS_REDIRECTS:
            return (
                dict(EMPTY_ROBOTS),
                f"robots.txt redirected more than {MAX_ROBOTS_REDIRECTS} times",
                True,
            )
        next_url = urljoin(url, location)
        if next_url in visited:
            return dict(EMPTY_ROBOTS), f"robots.txt redirect loop at {next_url}", True
        visited.add(next_url)
        next_host = urlsplit(next_url).hostname or ""
        if registrable_domain(next_host) != site_domain:
            return (
                dict(EMPTY_ROBOTS),
                f"robots.txt redirected off-site to {next_url}; not followed",
                True,
            )
        url = next_url
        redirected = True

    if code is not None and (code == 429 or 500 <= code < 600):
        return dict(EMPTY_ROBOTS), f"robots.txt returned {code}", True
    if code is not None and code >= 400:
        return dict(EMPTY_ROBOTS), "no robots.txt", False
    if redirected:
        content_type = (getattr(response, "headers", None) or {}).get("content-type", "")
        media_type = content_type.split(";", 1)[0].strip().lower()
        if content_type and media_type != "text/plain":
            return (
                dict(EMPTY_ROBOTS),
                f"robots.txt redirected to non-text/plain content ({media_type})",
                True,
            )
    note = f"robots.txt redirected to {url}" if redirected else ""
    return parse_robots(getattr(response, "text", "") or ""), note, False


def crawl_site(
    start_url: str,
    *,
    max_urls: int = 200,
    max_requests: int = 0,
    max_depth: int = 5,
    max_seconds: float = 0,
    min_delay: float = 0.5,
    timeout: float = 15.0,
    robots_policy: str = "respect",
    scope: dict[str, Any] | Scope | None = None,
    seed_urls: list[str] | None = None,
    out_path: str | None = None,
    links_path: str | None = None,
    forms_path: str | None = None,
    decisions_path: str | None = None,
    state_path: str | None = None,
    config_fingerprint: str = "",
    fetcher: Callable[[str], Any] | None = None,
    sleeper: Callable[[float], None] = time.sleep,
    credential_headers: list[dict[str, Any]] | None = None,
    clock: Callable[[], float] = time.monotonic,
    concurrency: int = 1,
    max_response_bytes: int = MAX_RESPONSE_BYTES,
    max_url_length: int = 2000,
    max_query_variants_per_path: int = 5,
    retry_on_timeout: int = 0,
    user_agent: str = "",
    robots_token: str = ROBOTS_TOKEN,
    unavailable_means_stop: bool = True,
    stop_after_consecutive_timeouts: int = STOP_AFTER_CONSECUTIVE_FAILURES,
    max_delay_seconds: float = MAX_DELAY_S,
    follow_nofollow: bool = False,
    classify_links: bool = False,
    link_position_rules: list[dict[str, Any]] | None = None,
    content_area_config: dict[str, Any] | None = None,
    cache: ResponseCache | None = None,
    extra_request_headers: dict[str, str] | None = None,
    adaptive: bool = True,
    store_hyperlinks: bool = True,
    crawl_hyperlinks: bool = True,
    store_external_links: bool = True,
    crawl_redirects: bool = True,
    capture_link_attributes: bool = False,
    crawl_external_links: bool = False,
    external_policy: dict[str, Any] | None = None,
    external_path: str | None = None,
    dispatch_gate: DispatchGate | None = None,
    progress: Callable[[int, int], None] | None = None,
    spool_evidence: bool = False,
    proxy_route: Any = None,
) -> SpiderResult:
    """Crawl one host breadth-first from ``start_url``, within ``scope``.

    ``cache``, when given, is consulted for every fetch before any delay or dispatch-gate wait
    is applied, so a cache hit costs neither a request nor a throttle slot — see ``fetch_one``.
    ``max_seconds`` is a wall-clock budget for the whole call; 0 means none.
    ``state_path``, when given, checkpoints the frontier, the exclusion tally and
    the query-variant budget there so a later call with the same path and start
    URL resumes instead of restarting — ``config_fingerprint`` is compared too,
    so a scope or limit change since the checkpoint starts fresh rather than
    mixing frontiers built under different rules. ``links_path``, when given
    alongside ``state_path``, is a sidecar that every discovered edge is
    appended to as it is found, and is read back into ``result.links`` on
    resume the same way ``out_path`` rebuilds ``result.pages`` — kept as a
    side file rather than folded into the checkpoint because the link graph is
    the largest thing a long crawl accumulates.
    ``decisions_path``, when given, is a structured, per-URL decision log
    (issue #134): one JSON line per exclusion, naming the URL and the rule
    that rejected it, so a wrong decision is debuggable after the fact rather
    than surviving only as a count in ``result.excluded``. Not resumed or
    replayed on a resumed run — it is a diagnostic trace of one call, not
    state the crawl depends on.
    ``seed_urls``, when given, are additional entry points added to the
    frontier at depth 0 alongside ``start_url`` — a sitemap-seeded crawl mode:
    every declared URL is fetched and its own links are followed, rather than
    treating the sitemap as the final answer. Each seed still goes through
    ``scope`` like any discovered link, and a rejected seed is counted in
    ``excluded`` under the rule that rejected it. Being seeded is not being
    "found by following links": a seed with no inbound edge in ``links`` is
    still reachable only because it was declared, which is what makes orphan
    detection against ``result.links`` honest even in this mode.

    ``concurrency`` is a per-origin ceiling, not a promise: the crawl starts at
    a conservative fan-out and the adaptive throttle widens it toward this
    ceiling only on sustained success, collapsing back to one request in flight
    on the first timeout or server refusal. A dispatch gate paces requests from
    a single shared clock regardless of how many workers are running, so
    ``min_delay`` still means "at least this long between requests to the
    origin" — concurrency only overlaps the *wait* for a response, never the
    rate at which requests go out. Each slice of the frontier is fetched as one
    batch, sized to what the origin has earned, and results are folded back in
    queue order — into ``result.pages``, the circuit breaker, redirect and link
    enqueueing, and the checkpoint — before anything downstream sees them, so
    the output (and the saved frontier, on an early stop) is identical to
    ``concurrency=1`` aside from ``response_time``.

    ``max_url_length`` and ``max_query_variants_per_path`` are enforced at
    enqueue time — a rejected URL is never fetched, and is counted in
    ``excluded`` like any other scope rejection. ``follow_nofollow`` decides
    whether a ``rel=nofollow`` link is still recorded in ``links`` (it always
    is) but also enqueued (only when true).
    ``classify_links`` resolves each recorded ``LinkEdge.position`` (nav,
    header, sidebar, footer, content, other — see ``tools/link_position.py``)
    while the page is already being parsed, at zero extra requests; it is off
    by default because storing a position per link is a real memory cost on a
    large crawl, and with it off every edge's ``position`` is simply ``""``
    (unmeasured), never a guessed value. ``link_position_rules`` overrides the
    default nav/header/sidebar/footer selectors (site-specific menus are
    common); ``content_area_config`` is the same config
    ``content_area.resolve_content_area`` takes, reused here so "content"
    means the same thing it means for word counts.
    ``capture_link_attributes`` copies each recorded ``LinkEdge``'s full rel token set,
    target attribute and raw (pre-resolution) href onto the edge — off by default for the
    same reason ``classify_links`` is: measured on a synthetic multi-thousand-page crawl,
    the three together add roughly 50% to per-edge memory (see ``LinkEdge``'s own
    docstring for the number). With it off, every edge's ``rel``/``target``/``raw_href``
    are simply unmeasured (``()``/``""``/``""``), and unsafe-cross-origin-link and
    protocol-relative-link detection — the two findings that need them — report nothing
    rather than a false clean result. ``nofollow`` is unaffected either way: it is derived
    from rel at parse time regardless of this setting.
    ``crawl_external_links`` opts into the bounded post-crawl check of recorded
    external destinations (``seohead.crawl.external``): once the internal
    frontier closes, each distinct off-host edge target is fetched once inside
    ``external_policy``'s own target/host/request/depth/redirect budgets —
    never recursed into an unrestricted crawl. ``external_path`` is the
    append-only sidecar (``external_checks.jsonl``) each decided outcome is
    written to, read back on resume the same way ``links_path`` rebuilds
    ``result.links`` — resumed runs never re-request a destination already
    decided, and the requests and hosts already spent count against the
    budgets rather than reopening them. The phase needs recorded edges, so it
    only has destinations to check when ``store_external_links`` kept them
    (``settings.validate`` refuses ``crawl`` without ``store``).
    ``progress``, when given, is called with ``(fetched, queued)`` -- pages
    already recorded, and URLs discovered but not yet fetched -- once before the
    first request and again after every page (after every batch, when
    concurrent). Both numbers are read straight from the structures the crawl
    itself walks, so a caller displaying them is displaying the crawl's own
    state rather than a parallel tally that can drift from it. It is called from
    the loop, not a timer: a crawl blocked on one slow response reports nothing
    new until that response lands, which is what is actually true of it.
    """
    start = normalize_url(start_url)
    host = (urlsplit(start).hostname or "").lower() if start else ""
    # normalize_url is lenient — it turns "not a url" into "https://not a url" —
    # so the host is checked here rather than trusted.
    if not start or not host or " " in host or "." not in host:
        raise ValueError(f"not a crawlable URL: {start_url!r}")
    rules = scope if isinstance(scope, Scope) else Scope.from_config(scope)
    if max_urls == 0:
        raise ValueError("an unlimited URL population requires a native SQLite site scan")
    limit = checked_url_budget(max_urls, materialized=not spool_evidence)
    if spool_evidence and not (out_path and links_path and forms_path and state_path):
        raise ValueError("spooled evidence requires page, link, form and state paths")
    depth_limit = max(0, min(int(max_depth), MAX_DEPTH_CEILING))
    max_concurrency = max(1, int(concurrency))
    if state_path:
        crawl_state.ensure_safe_dir(os.path.dirname(os.path.abspath(state_path)) or ".")
    parse_options = (
        {
            "classify_links": True,
            "link_position_rules": link_position_rules,
            "content_area": content_area_config,
        }
        if classify_links
        else None
    )

    result = SpiderResult(spooled_evidence=spool_evidence)
    if dispatch_gate is None:
        throttle = Throttle(
            min_delay=min_delay,
            max_delay=max_delay_seconds,
            max_concurrency=max_concurrency,
            adaptive=adaptive,
        )
        dispatch_gate = _DispatchGate(throttle, sleeper, clock, max_requests=max_requests)
    else:
        throttle = dispatch_gate.throttle
    excluded: dict[str, int] = {}
    # Distinct query strings already enqueued for a given path, so the Nth+1
    # facet/filter variant on the same path is excluded rather than fetched.
    query_budget: dict[str, set[str]] = {}
    page_count = link_count = form_count = 0
    crawl_started = clock()

    def exclude(reason: str, url: str | None = None) -> None:
        excluded[reason] = excluded.get(reason, 0) + 1
        # ``url`` is omitted for a decision that rejects a whole page's worth of
        # links at once (``depth_limit``) rather than one URL: attributing a
        # batch decision to a single URL would be a fabricated record, and a
        # wrong log line is worse than a gap named as one (see ``excluded``,
        # which still counts it).
        if url is not None and decisions_handle is not None:
            _write_decision(
                decisions_handle,
                {
                    "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                    "type": "exclude",
                    "url": url,
                    "reason": reason,
                    "host": host,
                },
            )

    def extra_rejection(candidate: str) -> str:
        """Checks beyond scope: a URL too long, or a path's query budget spent.

        Mutates ``query_budget`` the moment it runs, reserving a slot for
        ``candidate``'s query string whether or not it is later fetched — so a
        caller must run every check that can still reject the URL for a reason
        unrelated to the budget (nofollow, discovery toggles, scope) first, or
        that slot is spent on a URL that was never going to be dispatched (#193).
        """
        if max_url_length and len(candidate) > max_url_length:
            return "url_too_long"
        if max_query_variants_per_path:
            parts = urlsplit(candidate)
            path_key = parts.path or "/"
            variants = query_budget.setdefault(path_key, set())
            if parts.query not in variants:
                if len(variants) >= max_query_variants_per_path:
                    return "query_variants_limit"
                variants.add(parts.query)
        return ""

    with contextlib.ExitStack() as stack:
        client = None
        if fetcher is None:
            # A crawler must observe redirects, not be moved by them. With
            # follow_redirects on, a 301 is recorded as a 200 carrying the
            # target's title and body, the Location is never seen, and redirect
            # auditing is impossible — the old and new URL become duplicates.
            from seohead.recon.net import crawl_transport_options

            client, _ = http_client(
                timeout,
                follow_redirects=False,
                headers={"User-Agent": user_agent or UA},
                **crawl_transport_options(proxy_route),
            )
            stack.callback(client.close)

        enforce = robots_policy == "respect"
        if robots_policy == "ignore":
            # Not fetched at all, so there is nothing to report either.
            robots = dict(EMPTY_ROBOTS)
            note, unavailable = "robots.txt not fetched (policy: ignore)", False
        else:
            robots, note, unavailable = _fetch_robots(
                start, fetcher, client, wait=dispatch_gate.wait_turn
            )
        result.robots_note = note
        if enforce and unavailable and unavailable_means_stop:
            result.partial = True
            result.stopped_reason = note or "robots.txt unavailable"
            result.finish_reason = "robots_unavailable"
            return result

        # A site asking to be crawled slowly is asking the crawler, not the
        # operator. The configured delay is a floor, never a ceiling on politeness.
        # ``min_delay``'s setter (issue #150) raises ``max_delay`` along with it when
        # the site asks for more than the crawl's own ceiling, so every later clamp
        # into [min_delay, max_delay] keeps honouring this value instead of silently
        # sinking back to a smaller max_delay.
        asked = politeness_delay(robots, robots_token) if robots else None
        if asked is not None:
            effective = max(throttle.min_delay, asked)
            if asked > throttle.min_delay:
                throttle.min_delay = asked
                throttle.delay = max(throttle.delay, asked)
            result.crawl_delay_applied = effective

        loaded_state, resume_note = (
            crawl_state.load(state_path, start, config_fingerprint) if state_path else (None, "")
        )
        if loaded_state and loaded_state.spooled_evidence:
            if not spool_evidence:
                raise ValueError("checkpoint requires spooled page, link and form evidence")
            if not os.path.isfile(forms_path):
                raise ValueError("checkpoint form evidence sidecar is missing")
        result.resume_note = resume_note
        result.resumed = loaded_state is not None

        handle = None
        if out_path:
            if loaded_state:
                if spool_evidence:
                    page_count = sum(1 for _ in _jsonl_rows(out_path, checkpoint=True))
                else:
                    result.pages.extend(_read_pages_jsonl(out_path))
            mode = "a" if loaded_state else "w"
            handle = stack.enter_context(open(out_path, mode, encoding="utf-8"))

        links_handle = None
        if links_path:
            if loaded_state:
                if spool_evidence:
                    link_count = sum(1 for _ in _jsonl_rows(links_path, checkpoint=True))
                else:
                    result.links.extend(_read_links_jsonl(links_path))
            mode = "a" if loaded_state else "w"
            links_handle = stack.enter_context(open(links_path, mode, encoding="utf-8"))

        forms_handle = None
        if spool_evidence and forms_path:
            if loaded_state and not loaded_state.spooled_evidence:
                if os.path.exists(forms_path):
                    if any(
                        actual != expected
                        for actual, expected in zip_longest(
                            _jsonl_rows(forms_path, checkpoint=True), loaded_state.forms
                        )
                    ):
                        raise ValueError("checkpoint inline forms and form sidecar disagree")
                else:
                    with open(forms_path, "w", encoding="utf-8") as previous:
                        for entry in loaded_state.forms:
                            previous.write(json.dumps(entry, ensure_ascii=False) + "\n")
            if loaded_state:
                form_count = sum(1 for _ in _jsonl_rows(forms_path, checkpoint=True))
            forms_handle = stack.enter_context(
                open(forms_path, "a" if loaded_state else "w", encoding="utf-8")
            )
            if loaded_state and loaded_state.evidence_counts is not None:
                actual = {"pages": page_count, "links": link_count, "forms": form_count}
                if actual != loaded_state.evidence_counts:
                    raise ValueError("checkpoint evidence sidecar counts disagree")

        external_handle = None
        if external_path:
            if loaded_state:
                # Same move as links above: prior check outcomes live in the
                # sidecar, not the checkpoint — and they are also the "done"
                # set the resumed external phase must not re-request.
                result.external_checks.extend(_read_external_jsonl(external_path))
            mode = "a" if loaded_state else "w"
            external_handle = stack.enter_context(open(external_path, mode, encoding="utf-8"))

        # Appended across a resume like links_handle above — a decision recorded
        # before a checkpoint is still a decision this run made — but never read
        # back: nothing here needs to reconstruct prior exclusions in memory.
        decisions_handle = None
        if decisions_path:
            mode = "a" if loaded_state else "w"
            decisions_handle = stack.enter_context(open(decisions_path, mode, encoding="utf-8"))

        if loaded_state:
            queue: deque[tuple[str, int]] = deque(loaded_state.queue)
            seen: set[str] = set(loaded_state.seen)
            result.max_depth_reached = loaded_state.max_depth_reached
            excluded.update(loaded_state.excluded)
            for path_key, variants in loaded_state.query_budget.items():
                query_budget[path_key] = set(variants)
            # Both are produced only for pages fetched in this invocation, so a
            # resumed run would otherwise finish reporting fewer forms than the
            # interrupted one had already found, and would hand the rendering
            # gate an empty start page (issue #188).
            if not spool_evidence:
                result.forms.extend(FormEdge(**entry) for entry in loaded_state.forms)
            result.start_page_evidence = dict(loaded_state.start_page_evidence)
            # Crawl-wide evidence, not per-invocation data (issue #349): a
            # completed report-only audit needs every blocked URL ever seen,
            # not only ones fetched after this checkpoint.
            result.robots_blocked.extend(loaded_state.robots_blocked)
            # Seeds already accepted before this checkpoint (issue #348
            # extension). ``seen`` alone cannot answer "was this a seed": an
            # accepted seed is added to ``seen`` too, so the loop below would
            # otherwise find it already there and silently drop it from
            # ``result.seed_urls`` on every resume.
            result.seed_urls.extend(loaded_state.accepted_seed_urls)
        else:
            queue = deque([(start, 0)])
            seen = {_canonical_key(start)}

        for seed in seed_urls or []:
            seed = (seed or "").strip()
            if not seed:
                continue
            # Checked against ``seen`` before the rejection rules run, and
            # added to ``seen`` either way (issue #348): a rejected seed is a
            # decision this run made about that URL, exactly like an accepted
            # one, so a resumed retry that re-supplies the same declaration
            # must not re-evaluate and re-count it.
            key = _canonical_key(seed)
            if key in seen:
                continue
            seen.add(key)
            reason = rules.rejection(seed, host) or extra_rejection(seed)
            if reason:
                exclude(reason, seed)
                continue
            queue.append((seed, 0))
            result.seed_urls.append(seed)

        # The circuit breaker's own streaks, advanced only here as records are
        # folded back in queue order — see _fold_failure_streaks.
        consecutive_timeouts = 0
        consecutive_server_errors = 0

        def _extra_headers_for(url: str) -> dict[str, str] | None:
            # Resolved for this hop's own host, never carried over from the
            # last one — that is what keeps a credential off a cross-host
            # redirect target. Called with each URL's own host regardless of
            # concurrency, so nothing is ever carried between hops or workers.
            # http.headers applies to every request; a credential applies only to the host it
            # was bound to. Merging here rather than at client construction keeps both in one
            # place and keeps the per-host resolution the credential rule depends on.
            headers = dict(extra_request_headers or {})
            if credential_headers:
                headers.update(
                    resolve_credential_headers(credential_headers, urlsplit(url).hostname or "")
                    or {}
                )
            return headers or None

        def robots_blocks(url: str) -> bool:
            """True when this URL must not be fetched under the current policy."""
            if robots and not is_allowed(robots, match_path(url), robots_token):
                result.robots_blocked.append(url)
                if enforce:
                    exclude("blocked_by_robots", url)
                    return True
            return False

        def handle_redirect(record: PageRecord, depth: int) -> None:
            # A redirect is a discovery too, and it stays inside the budget — unless
            # discovery.redirects.crawl says a redirect target is not a discovery source,
            # in which case the redirect is still recorded on the page, just never followed.
            if not crawl_redirects:
                return
            if not record.redirect_url:
                return
            target = _strip_fragment(record.redirect_url)
            if depth >= depth_limit:
                exclude("depth_limit", target)
                return
            reason = rules.rejection(target, host) or extra_rejection(target)
            if reason:
                exclude("redirect_off_host" if reason == "outside_host" else reason, target)
            else:
                key = _canonical_key(target)
                if key not in seen:
                    seen.add(key)
                    queue.append((target, depth + 1))

        def handle_links(parsed: dict[str, Any] | None, url: str, depth: int) -> None:
            def discover(target: str, next_depth: int, _requested_url: str) -> str | None:
                reason = extra_rejection(target)
                if reason:
                    return reason
                key = _canonical_key(target)
                if key not in seen:
                    seen.add(key)
                    queue.append((target, next_depth))
                return None

            def record_edge(edge: LinkEdge) -> None:
                nonlocal link_count
                if spool_evidence:
                    link_count += 1
                else:
                    result.links.append(edge)
                _write_link(links_handle, edge)

            apply_document_links(
                parsed,
                url,
                depth,
                depth_limit=depth_limit,
                host=host,
                rejection=rules.rejection,
                discover=discover,
                store_hyperlinks=store_hyperlinks,
                store_external_links=store_external_links,
                crawl_hyperlinks=crawl_hyperlinks,
                follow_nofollow=follow_nofollow,
                capture_link_attributes=capture_link_attributes,
                record_edge=record_edge,
                reject=exclude,
                mark_link_partial=lambda omitted: exclude("link_observations_limit"),
            )

        def handle_forms(parsed: dict[str, Any] | None, url: str) -> None:
            nonlocal form_count
            forms, omitted = form_edges(parsed, url)
            if spool_evidence:
                form_count += len(forms)
                for form in forms:
                    forms_handle.write(
                        json.dumps(dataclasses.asdict(form), ensure_ascii=False) + "\n"
                    )
                forms_handle.flush()
            else:
                result.forms.extend(forms)
            if omitted:
                exclude("form_observations_limit")

        def record_evidence(
            url: str, depth: int, record: PageRecord, parsed: dict[str, Any] | None
        ) -> None:
            """Persist a fetched page as evidence, independent of the breaker.

            Split out of ``after_fetch`` so the concurrent batch merge (#475) can
            keep every already-completed request's evidence (per #304 — a spent
            request must not be discarded) without also feeding it through the
            streak counters or the frontier: once the breaker has tripped on an
            earlier, ordered result, later completions in the same batch must be
            recorded but must not extend how many consecutive failures were
            tolerated, nor discover further URLs to chase.
            """
            nonlocal page_count
            record.crawl_depth = depth
            if spool_evidence:
                page_count += 1
            else:
                result.pages.append(record)
            _write(handle, record)
            handle_forms(parsed, url)

            if (
                depth == 0
                and url == start
                and not result.start_page_evidence
                and parsed is not None
            ):
                # Captured once, straight from the ordinary fetch -- no extra
                # request. Used only by the pre-flight rendering gate (#18):
                # an empty SPA shell or a link-less start page must withhold
                # the health score, and both checks are static, so they must
                # not wait for a render that a raw-mode run will never perform.
                result.start_page_evidence = {
                    "html": parsed.get("_raw_html", ""),
                    "outlinks": record.outlinks,
                    "external_outlinks": record.external_outlinks,
                }

        def after_fetch(
            url: str, depth: int, record: PageRecord, parsed: dict[str, Any] | None
        ) -> bool:
            """Bookkeeping shared by every fetched page. Returns True to stop the crawl."""
            nonlocal consecutive_timeouts, consecutive_server_errors
            if record.body_unavailable in _MEDIA_TYPE_FILTER_REASONS:
                exclude(record.body_unavailable, url)
            record_evidence(url, depth, record, parsed)

            consecutive_timeouts, consecutive_server_errors = _fold_failure_streaks(
                record, consecutive_timeouts, consecutive_server_errors
            )
            if consecutive_timeouts >= stop_after_consecutive_timeouts:
                result.partial = True
                result.stopped_reason = "origin stopped responding (repeated timeouts)"
                result.finish_reason = "errors"
                return True
            if consecutive_server_errors >= STOP_AFTER_CONSECUTIVE_FAILURES:
                # The host has refused repeatedly. Continuing would measure the
                # crawler rather than the site.
                result.partial = True
                result.stopped_reason = "origin refused repeatedly (429/5xx) — crawl stopped"
                result.finish_reason = "errors"
                return True

            handle_redirect(record, depth)
            handle_links(parsed, url, depth)
            return False

        def report_progress() -> None:
            """Hand the caller the crawl's own counters, or do nothing when nobody asked."""
            if progress is not None:
                progress(page_count if spool_evidence else len(result.pages), len(queue))

        stopped = False
        # Reported before the first request too: on a slow origin the operator
        # otherwise waits out the whole first fetch with nothing on screen, and
        # cannot tell a crawl that started from one that hung on robots.txt.
        report_progress()
        if max_concurrency <= 1:
            # The plain sequential path: one request, wait for the response,
            # then the next. Kept byte-for-byte separate from the batched path
            # below so the common case (the default) carries zero concurrency
            # overhead and zero risk of it changing behaviour.
            while queue and not stopped:
                if (page_count if spool_evidence else len(result.pages)) >= limit:
                    result.partial = True
                    result.stopped_reason = f"url limit reached ({limit})"
                    result.finish_reason = "url_limit"
                    break
                if max_seconds and (clock() - crawl_started) >= max_seconds:
                    result.partial = True
                    result.stopped_reason = f"duration limit reached ({max_seconds:.0f}s)"
                    result.finish_reason = "duration_limit"
                    break
                url, depth = queue.popleft()
                result.max_depth_reached = max(result.max_depth_reached, depth)

                if robots_blocks(url):
                    # The URL was counted as queued when it was discovered, so leaving
                    # without a report keeps it in the operator's denominator forever:
                    # a finished crawl then signs off short of 100%, reading as stalled.
                    report_progress()
                    continue

                try:
                    record, parsed = fetch_one(
                        url,
                        client=client,
                        fetcher=fetcher,
                        throttle=throttle,
                        extra_headers=_extra_headers_for(url),
                        user_agent=user_agent,
                        max_response_bytes=max_response_bytes,
                        retry_on_timeout=retry_on_timeout,
                        parse_options=parse_options,
                        cache=cache,
                        wait=dispatch_gate.wait_turn,
                        response_filter=rules.response_media_rejection,
                    )
                except KeyboardInterrupt:
                    # Not processed: put it back so a resume retries it rather
                    # than silently dropping it from the frontier.
                    queue.appendleft((url, depth))
                    result.partial = True
                    result.stopped_reason = "interrupted"
                    result.finish_reason = "interrupted"
                    break
                stopped = after_fetch(url, depth, record, parsed)
                report_progress()
        else:
            gate = dispatch_gate

            def dispatch(item: tuple[str, int]) -> tuple[str, int, PageRecord, dict | None]:
                url, depth = item
                # gate.wait_turn is passed as ``wait`` rather than called here directly, so a
                # cache hit — decided inside fetch_one — never claims a dispatch turn it did not
                # need. A hit costs no request, so it must not cost a pacing slot either.
                record, parsed = fetch_one(
                    url,
                    client=client,
                    fetcher=fetcher,
                    throttle=throttle,
                    extra_headers=_extra_headers_for(url),
                    user_agent=user_agent,
                    max_response_bytes=max_response_bytes,
                    retry_on_timeout=retry_on_timeout,
                    parse_options=parse_options,
                    cache=cache,
                    wait=gate.wait_turn,
                    response_filter=rules.response_media_rejection,
                )
                return url, depth, record, parsed

            with ThreadPoolExecutor(max_workers=max_concurrency) as pool:
                while queue and not stopped:
                    if (page_count if spool_evidence else len(result.pages)) >= limit:
                        result.partial = True
                        result.stopped_reason = f"url limit reached ({limit})"
                        result.finish_reason = "url_limit"
                        break
                    if max_seconds and (clock() - crawl_started) >= max_seconds:
                        result.partial = True
                        result.stopped_reason = f"duration limit reached ({max_seconds:.0f}s)"
                        result.finish_reason = "duration_limit"
                        break

                    # One batch is one slice of the frontier, sized to what the
                    # origin has earned so far — never more than the pool has
                    # workers for, since that is the largest unit that stays
                    # sound to re-sort by discovery order in one pass.
                    batch = [queue.popleft() for _ in range(min(throttle.concurrency, len(queue)))]

                    blocked_depths = []
                    to_fetch = []
                    for u, d in batch:
                        if robots_blocks(u):
                            blocked_depths.append(d)
                        else:
                            to_fetch.append((u, d))

                    # A URL budget ends the crawl at an exact page count,
                    # concurrency or not: anything past the remaining budget
                    # goes back to the front of the queue rather than being
                    # dispatched.
                    budget = limit - (page_count if spool_evidence else len(result.pages))
                    if len(to_fetch) > budget:
                        overflow, to_fetch = to_fetch[budget:], to_fetch[:budget]
                        for item in reversed(overflow):
                            queue.appendleft(item)

                    # Depth bookkeeping matches the sequential walk: it covers
                    # every item popped for good (fetched or robots-blocked),
                    # never an item pushed back to the queue as overflow.
                    for d in blocked_depths:
                        result.max_depth_reached = max(result.max_depth_reached, d)
                    for _, d in to_fetch:
                        result.max_depth_reached = max(result.max_depth_reached, d)

                    if not to_fetch:
                        report_progress()  # same reason as the sequential path above
                        continue

                    # Futures are submitted up front and then consumed in the
                    # order of ``to_fetch`` regardless of which request
                    # actually finished first, so every downstream step —
                    # recording, the circuit breaker, link and redirect
                    # enqueueing — sees the same order the sequential crawler
                    # would have used. Keeping the futures themselves (rather
                    # than ``pool.map``'s generator) lets a breaker firing
                    # partway through the batch still recover whichever later
                    # requests already finished, instead of discarding them.
                    futures = [pool.submit(dispatch, item) for item in to_fetch]
                    processed = 0
                    interrupted = False
                    try:
                        for future in futures:
                            url, depth, record, parsed = future.result()
                            processed += 1
                            if after_fetch(url, depth, record, parsed):
                                stopped = True
                                break
                    except KeyboardInterrupt:
                        # Some workers in this batch may still be running; the
                        # ones whose result was never consumed here are not
                        # known to be processed, so they (and anything not yet
                        # dispatched) go back to the front of the queue rather
                        # than being dropped from the frontier. A future's
                        # completion is not established here, so it is always
                        # requeued rather than risked on a `.done()` check
                        # that could race the interrupt itself.
                        interrupted = True

                    if stopped and not interrupted:
                        # The breaker fired on an earlier, ordered result, but
                        # later requests in this same batch ran concurrently
                        # and may have already completed. Merge each one that
                        # has — its PageRecord, sidecar write and forms — before
                        # requeueing anything, so the checkpoint holds only work
                        # that never resolved rather than repeating requests
                        # that already reached the origin (#304). But a
                        # completed record that would itself continue the very
                        # streak that just tripped the breaker (another
                        # timeout, another 429/5xx) must not be merged either:
                        # the tolerance the caller configured has already been
                        # spent by the ordered result that stopped the crawl,
                        # and merging more of the same failure would let batch
                        # size alone raise the effective tolerance (#475).
                        # Those are requeued for a future crawl to retry, same
                        # as a completion that has not landed yet.
                        requeue: list[tuple[str, int]] = []
                        for item, future in zip(
                            to_fetch[processed:], futures[processed:], strict=True
                        ):
                            if future.done():
                                url, depth, record, parsed = future.result()
                                if _continues_failure_streak(record):
                                    requeue.append(item)
                                else:
                                    record_evidence(url, depth, record, parsed)
                            else:
                                requeue.append(item)
                        for item in reversed(requeue):
                            queue.appendleft(item)
                    elif interrupted:
                        for item in reversed(to_fetch[processed:]):
                            queue.appendleft(item)
                    if interrupted:
                        result.partial = True
                        result.stopped_reason = "interrupted"
                        result.finish_reason = "interrupted"
                        stopped = True
                    # Once per batch, after every requeue has been folded back
                    # in: mid-batch the queue is briefly short of URLs that are
                    # about to return to it, and reporting there would show a
                    # frontier that never existed.
                    report_progress()

        # Bounded check of recorded external destinations (#746). It runs only
        # after the internal frontier has said its piece — a partially walked
        # site still yields its recorded destinations for checking — and its
        # own coverage is kept in external_summary rather than folded into
        # finish_reason, because internal completeness and external-check
        # coverage are different measurements.
        external_finished = True
        if crawl_external_links:
            if result.finish_reason == "interrupted":
                # A KeyboardInterrupt means stop now, not start a second phase;
                # the destinations recorded so far are checked by the resumed
                # run once the frontier actually closes.
                result.external_summary = {
                    "enabled": True,
                    "ran": False,
                    "reason": (
                        "internal crawl interrupted; recorded destinations are "
                        "checked once the internal frontier completes"
                    ),
                    "robots_policy": "not_evaluated",
                }
                external_finished = False
            else:
                policy = ExternalPolicy.from_config(external_policy)
                # A gate of its own: the external phase is sequential, paced by
                # the same delay policy, and spends a request budget the
                # internal frontier never touches (external_checks.max_requests).
                external_gate = DispatchGate(
                    Throttle(
                        min_delay=min_delay,
                        max_delay=max_delay_seconds,
                        max_concurrency=1,
                        adaptive=adaptive,
                    ),
                    sleeper,
                    clock=clock,
                    max_requests=policy.max_requests,
                )

                def external_excluded(url: str) -> bool:
                    # The same never-fetch rule the internal scope applies: an
                    # operator-excluded host stays unfetched no matter what
                    # links to it.
                    h = (urlsplit(url).hostname or "").lower()
                    return any(h == bad or h.endswith("." + bad) for bad in rules.exclude_hosts)

                def emit_check(check: ExternalCheck) -> None:
                    result.external_checks.append(check)
                    _write(external_handle, check)

                try:
                    result.external_summary = run_external_checks(
                        _iter_links_jsonl(links_path) if spool_evidence else result.links,
                        policy=policy,
                        is_internal=lambda url: rules.is_internal(url, host),
                        emit=emit_check,
                        excluded_host=external_excluded,
                        done=result.external_checks,
                        client=client,
                        fetcher=fetcher,
                        headers_for_url=_extra_headers_for,
                        user_agent=user_agent,
                        max_response_bytes=max_response_bytes,
                        retry_on_timeout=retry_on_timeout,
                        parse_options=parse_options,
                        dispatch_gate=external_gate,
                        time_exhausted=lambda: bool(
                            max_seconds and (clock() - crawl_started) >= max_seconds
                        ),
                    )
                except KeyboardInterrupt:
                    result.external_summary = {
                        "enabled": True,
                        "ran": True,
                        "policy": policy.as_dict(),
                        "robots_policy": "not_evaluated",
                        "finish_reason": "interrupted",
                        "internal_finish_reason": result.finish_reason,
                    }
                    external_finished = False
                    result.partial = True
                    result.stopped_reason = (
                        f"{result.stopped_reason}; external checks interrupted"
                        if result.stopped_reason
                        else "external checks interrupted"
                    )
                else:
                    # The internal frontier's own verdict travels inside the
                    # external summary, so one object tells both halves of the
                    # coverage story without conflating them.
                    result.external_summary["internal_finish_reason"] = result.finish_reason
                    external_finished = result.external_summary["finish_reason"] == "finished"
                    if not external_finished:
                        result.partial = True

        if state_path:
            if result.finish_reason == "finished" and external_finished:
                # Nothing left to resume: a later call with the same path
                # should crawl fresh, not "resume" into an empty frontier.
                crawl_state.clear(state_path)
            else:
                # Saved only once the loop above has finished folding the last
                # batch back in, so the frontier on disk is always a complete
                # BFS state, never a snapshot taken mid-batch.
                crawl_state.save(
                    state_path,
                    crawl_state.CrawlState(
                        start_url=start,
                        queue=list(queue),
                        seen=sorted(seen),
                        max_depth_reached=result.max_depth_reached,
                        config_fingerprint=config_fingerprint,
                        excluded=dict(excluded),
                        query_budget={
                            path_key: sorted(variants)
                            for path_key, variants in query_budget.items()
                        },
                        forms=[]
                        if spool_evidence
                        else [dataclasses.asdict(form) for form in result.forms],
                        start_page_evidence=dict(result.start_page_evidence),
                        robots_blocked=list(result.robots_blocked),
                        accepted_seed_urls=list(result.seed_urls),
                        spooled_evidence=spool_evidence,
                        evidence_counts={
                            "pages": page_count if spool_evidence else len(result.pages),
                            "links": link_count if spool_evidence else len(result.links),
                            "forms": form_count if spool_evidence else len(result.forms),
                        },
                    ),
                )

    result.excluded = excluded
    result.page_count = page_count if spool_evidence else len(result.pages)
    result.link_count = link_count if spool_evidence else len(result.links)
    result.form_count = form_count if spool_evidence else len(result.forms)
    result.effective_delay = throttle.delay
    result.effective_concurrency = throttle.concurrency
    if cache is not None:
        result.cache_stats = dict(cache.stats)
        result.cache_replay = cache.mode == "replay"
    result.limitations = [
        f"scope {rules.internal}: links outside it are recorded, never fetched",
        "static HTML only: no JavaScript rendering",
    ]
    if not seed_urls:
        # The spider itself never fetches a sitemap; a caller expands one and
        # passes the URL set in via seed_urls. When it did, this crawl was
        # sitemap-seeded, so the blanket limitation would be false.
        result.limitations.append("no sitemap expansion")
    return result
