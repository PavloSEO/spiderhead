"""Crawler configuration: defaults, precedence, validation, and the run manifest.

Three properties matter more than the field list.

**A setting that nothing reads is a lie.** Every key here is wired to behaviour.
Unknown keys are rejected with their path rather than ignored, so a typo in a
scope pattern cannot silently widen a crawl and a setting cannot be added to the
file before it is added to the code.

**Store and crawl are different questions.** For each link type, "keep it in the
report" and "request it for a status code" are independent. Collapsing them into
one flag is why a crawler either misses broken images or triples its requests.

**Two thirds of these settings change what the audit finds.** Those are recorded
in the run manifest, because otherwise two reports on the same site are not
comparable and nobody can tell why they differ. The rest change only how long the
run takes. ``RESULTS_AFFECTING`` is that classification, and a test fails when a
new setting is added without being placed in one group or the other.
"""

from __future__ import annotations

import copy
import json
import os
import re
from typing import Any

# Historical estimates for the materialized legacy route. Native SQLite capture
# streams pages, links and frontier through NativeScan; these object-size figures
# describe the legacy API, not the capacity of the native route.
#
# The 383-byte LinkEdge and 2 400-byte PageRecord figures came from tracemalloc
# fixtures over 40 000 edges and 8 000 records with distinct URL/text strings;
# summing sys.getsizeof over a shared object graph roughly doubles them. The paired
# iframe-head/combined-record fixture measures a 56-byte empty-hreflang-list
# increment; the eight fields added for the #385/#386 coverage checks, and the two
# added for the AJAX-crawling-scheme checks (#386), are empty strings and zeros by
# default and measure no further allocation either -- the 56-field and 58-field
# records were measured side by side with one instrument and came back identical.
# List mode's canonical walk (#21) then added two more: final_canonical is another
# empty string and costs nothing, but canonical_chain is a list, and an empty list
# per record is not free. Measured the same way, 8 000 records with distinct URLs:
# 2 116 bytes at 58 fields, 2 172 at 60 -- one more 56-byte empty-list increment,
# the same figure the hreflang list cost. The totals below carry it.
# The heading outline (#632) is the first field whose default emptiness is not the
# normal case: every HTML page has headings, and each is a dict of three values held
# for the whole run. Measured with the same instrument over 8 000 records, the empty
# list costs the familiar 56 bytes, and a page carrying a twelve-heading outline costs
# 3 091 bytes more than one with none -- roughly doubling a page record on a
# heading-rich site. Link placement (#634) is the second such field and much the
# cheaper one: it is a fixed four-key dict rather than a list that grows with the
# page, and the parser caps both lists inside it. Measured with the same instrument
# over 8 000 records, the empty placement dict costs 303 bytes per record, and a
# page carrying one heading link and one unlabelled image link costs about 520
# bytes more than one carrying neither -- a constant per page, not a per-link cost,
# which is why it is not behind link_position.classify. Field lengths affect
# absolute memory,
# so 5 550 bytes is an approximate combined PageRecord estimate for such a site
# (about 2 500 where the outlines are short); the rounded totals are
#
#   10 000 URLs x 150 links/page -> 0.59 GiB
#   50 000 URLs x  60 links/page -> 1.33 GiB
#   50 000 URLs x 150 links/page -> 2.93 GiB
#
# Bounded trust signals (#823) add one nullable field. A paired tracemalloc run
# over 8,000 distinct-URL records on Python 3.13 measured no increment for null,
# 352 bytes for a measured empty object, and 6,648 bytes for 8 author + 8 date
# signals with distinct 160-character values. Thus the rich-outline estimate
# above may approach 12,200 bytes/page with rich attribution markup (about
# 0.31 GiB extra at 50,000 URLs). These estimates exclude other crawl/analyzer
# allocations and are not a full-run memory guarantee.
# The nullable render-blocking JSON projection (#1022) adds field 68. A paired
# Python 3.14 tracemalloc run over 8,000 distinct-URL records measured 2,503 bytes
# per default record at both 67 and 68 fields; null adds no allocation. Two
# distinct resource URLs in the populated JSON array add about 131 bytes per
# record. Longer resource lists cost more; the native route streams this evidence.
# Field 70 is the transport-upgrade marker (#1484), a bool that defaults False
# and never allocates: the same instrument measured 2,539 bytes per default
# 70-field record, within noise of the 69-field figure.
# Fields 71-74 are the readability set (#1516): three nullable numbers (None
# while the content area has nothing measurable) and the band string. The same
# instrument measured 2,540 bytes per default 74-field record and no increment
# for a populated band -- the numbers ride existing per-record slots.
# Field 69 is the nullable rejected-href list (#1530). The same instrument measured
# 2,540 bytes per default 69-field record (null adds no allocation, as before) and
# about +247 bytes for a record carrying one rejected target -- a cost only a crawl
# that opted into discovery.record_invalid_targets pays. LinkEdge gained its ninth
# field the same way (#1518: anchor_source); the extra dataclass slot costs 8 bytes
# per edge, and "alt" vs "" adds no further allocation. At the 50,000 x 150-link
# figure above that is about +0.06 GiB for the marker, against the ~46 MiB the
# raw_href family already costs.
# Explicit native crawl admission; capacity still depends on independent request,
# time, body, disk and renderer budgets. Admission is not a benchmark result.
# Higher requests are refused, never silently clamped.
MAX_URLS_CEILING = 1_000_000
# Preserve the recorded direct-storage marker and fingerprints of existing scans.
# Live collectors still reject that marker; stable requests use the shared ceiling.
MAX_EXPERIMENTAL_URLS = MAX_URLS_CEILING
# Eager legacy APIs keep PageRecord/LinkEdge objects; they are not the native route.
MAX_MATERIALIZED_URLS = 50_000
# Robots, redirects and retries have a separate explicit request budget.
MAX_REQUESTS_CEILING = 2_000_000


# ponytail: a stated page budget implies the request and time budgets a small crawl needs,
# so --max-urls alone admits a 50-300 page crawl. Ratios match the project quick-crawl preset.
# ceiling: a page budget whose derived requests/seconds exceed the project thresholds is still refused.
# upgrade: derive from observed per-page cost once the crawler reports it.
BUDGET_REQUESTS_PER_PAGE = 3
BUDGET_SECONDS_PER_PAGE = 2


def bind_budgets_to_page_budget(settings: dict, explicit: set[str]) -> None:
    """Fill unset request and time budgets from an explicit page budget (in place)."""
    limits = settings["limits"]
    pages = limits["max_urls"]
    if not pages:
        return
    defaults = DEFAULTS["limits"]
    if "limits.max_requests" not in explicit and limits["max_requests"] == defaults["max_requests"]:
        limits["max_requests"] = min(BUDGET_REQUESTS_PER_PAGE * pages, MAX_REQUESTS_CEILING)
    if (
        "limits.max_crawl_seconds" not in explicit
        and limits["max_crawl_seconds"] == defaults["max_crawl_seconds"]
    ):
        limits["max_crawl_seconds"] = BUDGET_SECONDS_PER_PAGE * pages


def checked_url_budget(max_urls: int, *, materialized: bool = False) -> int:
    """How many URLs a crawl may fetch -- or a refusal, never a quiet reduction.

    ``min(max_urls, MAX_URLS_CEILING)`` was the old answer and it was wrong in the
    one case that matters: a caller who asked for 200 000 URLs was given 50 000 and
    a result that did not say so, so a quarter of a site was reported as the whole
    of it. Both crawl entry points call this, which is also what stops the ceiling
    from being defined twice and drifting apart again (#356).
    """
    if max_urls == 0:
        if materialized:
            raise ValueError("an unlimited URL population requires a native SQLite site scan")
        return 0
    budget = max(1, int(max_urls))
    if budget > MAX_URLS_CEILING:
        raise ValueError(
            f"max_urls is {budget:,}, above this crawler's ceiling of {MAX_URLS_CEILING:,}; "
            "narrow the scope or split the work into resumable scans"
        )
    if materialized and budget > MAX_MATERIALIZED_URLS:
        raise ValueError(
            f"materialized legacy collection ceiling is {MAX_MATERIALIZED_URLS:,} URLs; "
            "use a native SQLite scan for an explicit URL list or site crawl for larger populations"
        )
    return budget


DEFAULTS: dict[str, Any] = {
    "scope": {
        # Which discovered URLs count as internal. "host" is the conservative
        # reading; "registrable_domain" also accepts subdomains.
        "internal": "host",  # host | registrable_domain
        # Stay below the start URL's path folder (#1481): a deep start URL
        # walks only its own subtree, and links pointing above the folder are
        # counted as unfollowed evidence (excluded under outside_start_folder)
        # exactly like any other scope rejection. Refused for a start URL at
        # the host root -- there is no folder to stay below.
        "start_folder_only": False,
        # Regexes searched against the whole URL of every *discovered* link.
        # The start URL is always fetched: a crawl that filters out its own seed
        # would report an empty site rather than a configuration mistake.
        "include_patterns": [],
        "exclude_patterns": [],
        # Filename suffixes are checked against the final component of the URL
        # path (not its query or fragment) after the ordinary host/regex/segment
        # scope. A nonempty include list narrows discovered routes; extensionless
        # routes are rejected with not_included_by_extension.
        "include_extensions": [],
        "exclude_extensions": [],
        # Response media types are checked only after an admitted URL is fetched.
        # Exact types and type wildcards (for example image/*) are supported.
        "include_media_types": [],
        "exclude_media_types": [],
        # Never fetched regardless of what links to them.
        "exclude_hosts": [],
        # Ordered, first-match-wins named segments -- a multilingual or multi-regional
        # site is several sites sharing one crawl, and until a URL is assigned to one
        # of them nothing downstream can group by it (#358). Each entry is
        # {"name": ..., plus at least one of "prefix" (path prefix), "host" (exact
        # hostname -- a subdomain or a wholly separate domain) or "pattern" (regex
        # over the whole URL)}. A URL matching none of them is the built-in
        # "default" segment, so every URL belongs to exactly one segment, always.
        "segments": [],
        # Fetch only these segment names (plus "default" when named) -- the thing
        # that makes crawling one region of a large site cheap without an ad-hoc
        # scope.include_patterns regex. Empty means every segment is in scope.
        "segments_only": [],
        # Common analytics click IDs do not identify distinct documents. Strip
        # them from native GET targets while retaining all other query fields.
        "normalize_tracking_parameters": True,
    },
    # Post-crawl grouping is distinct from scope.segments: scope controls
    # discovery, while analysis rules classify already collected evidence.
    "analysis": {
        "segments": [],
        "canonical_policy": {"pagination": [], "filters": []},
        "asset_thresholds": {
            "page_weight_max_bytes": 2 * 1024 * 1024,
            "resource_max_bytes": 2 * 1024 * 1024,
        },
        # Post-analysis URL-pattern suppressions. These never affect scope or
        # collection; the effective ordered policy is stored with the scan.
        "finding_exclusions": [],
    },
    "sitemaps": {
        # Seed the crawl from the sitemap declared in robots.txt (the
        # ``Sitemap:`` directive) when no explicit sitemap URL is given.
        # Every declared URL is fetched and its own links are followed,
        # rather than the sitemap being treated as the final answer.
        "auto_discover": False,
    },
    "discovery": {
        # Each link type is a pair: store it in the report, and/or request it. Only the pairs
        # that actually change an outcome are here. discovery.canonicals.* was removed in the
        # #91 pass: chasing canonicals as a discovery source is a capability the spider does
        # not have, and a setting that names behaviour nothing implements is worse than no
        # setting — it reads as a supported option in --config-help and in the run manifest.
        "hyperlinks": {"store": True, "crawl": True},
        "redirects": {"crawl": True},
        # A redirect has no LinkEdge of its own to withhold, so there is no redirects.store to
        # honour: the redirect target is recorded on the page record either way.
        # discovery.external.crawl is the real, bounded version of the option #91 removed:
        # the check exists (seohead/crawl/external.py), runs after the internal frontier
        # closes, and is confined by the external_checks.* budgets below rather than becoming
        # the unrestricted second-origin crawl that made the old name a lie.
        "external": {"store": True, "crawl": False},
        "follow_nofollow": False,
        # Off by default (#1530): hrefs that cannot become fetchable link
        # targets (unparsable syntax, a non-http(s) scheme) are dropped
        # silently unless this is on, and the page keeps no evidence that it
        # points at something broken. With it on they are recorded per page as
        # data -- raw href plus rejection reason -- and still never enter the
        # frontier. A scalar like follow_nofollow: the store/crawl pairs above
        # are per link-type outcomes, and this is neither.
        "record_invalid_targets": False,
        # List mode only (no ``url``, only ``urls``): a redirect is recorded as
        # given -- the hop is never followed as a new page -- but a migration
        # audit needs to know where the chain actually ends, not just that a
        # hop exists. Off by default because it is extra requests per redirect
        # a plain status check does not need.
        "resolve_redirect_destination": False,
        # Canonical-chain equivalent: inspect canonical targets without making
        # them list-mode pages or relaxing depth zero.
        "resolve_canonical_destination": False,
    },
    "limits": {
        "max_urls": 200,
        "max_depth": 5,
        "max_query_variants_per_path": 5,
        "max_response_bytes": 5 * 1024 * 1024,
        "max_url_length": 2000,
        "max_crawl_seconds": 0,  # 0 = no wall-clock limit
        "max_requests": 20_000,  # 0 = no total HTTP-attempt limit
    },
    # Budgets for the opt-in external-destination check (discovery.external.crawl).
    # They are a separate budget from limits.*: external requests go to origins
    # that did not ask for them, so they must not share — or silently consume —
    # the internal frontier's allowances. All values are read only when
    # discovery.external.crawl is true; with it off they are inert.
    "external_checks": {
        # Distinct external destinations the check may fetch at most, recorded
        # seeds plus any same-host continuations admitted under max_depth.
        "max_targets": 100,
        # Distinct external origins the check may contact at most — redirect
        # hops count, since a redirect is just a request to another origin.
        "max_hosts": 20,
        # HTTP attempts the check may spend; 0 = no phase-specific cap (the
        # internal limits.max_requests is a separate budget it never touches).
        "max_requests": 0,
        # 0 = check recorded destinations only. >0 also follows that many hops
        # of *same-host* links on external pages that answered — never an
        # off-host link, so never unrestricted traversal.
        "max_depth": 0,
        # Redirect hops followed per destination before the chain is recorded
        # unresolved; hard ceiling is MAX_REDIRECT_CHAIN_HOPS (10).
        "max_redirects": 5,
    },
    "http": {
        "timeout_seconds": 15.0,
        "user_agent": "",  # empty = SpiderHeadBot/<version>, resolved in load()
        "headers": {},
        "retry_on_timeout": 0,
        # Off by default (#1484): upgrading every http URL to https before the
        # request is a declaration that the hosts enforce transport security.
        # On: no redirect hop is spent or recorded for the upgrade, the http
        # and https spellings collapse into one page identity, and each page
        # fetched this way is marked transport-upgraded.
        "upgrade_to_https": False,
        # Empty is direct. Credentials may appear only in an env:VARIABLE URL.
        "proxy": "",
        "proxy_allow_private": False,
        # Safe route facts are frozen when load() resolves the proxy. They keep
        # stored scan fingerprints inspectable after the environment changes.
        "proxy_identity": "",
        "proxy_authenticated": False,
        # Each entry is {"host": "...", "headers": {"Authorization": "env:VAR"}}.
        # Bound to one host and resolved from the environment — never a bare
        # value in the file — so a credential cannot leak into a config export
        # and cannot be sent to a host it was never meant for.
        "credential_headers": [],
        # Authenticated crawling clicks every link while logged in, including
        # ones that log out, publish, or delete. Requiring this flag once
        # credential_headers is non-empty makes that risk an explicit choice.
        "credentials_acknowledged": False,
        # Per-host Basic/Digest authentication. Values are environment-variable
        # references, never inline credentials; acknowledged use widens access.
        "auth": [],
        "auth_acknowledged": False,
        "zstd": False,
    },
    "robots": {
        # respect: obey. report_only: fetch, report what would be blocked, crawl
        # anyway — the honest audit setting. ignore: do not fetch it at all.
        "policy": "respect",  # respect | report_only | ignore
        "user_agent_token": "",  # empty = the token of the resolved user agent (SpiderHeadBot)
        "unavailable_means_stop": True,
    },
    "speed": {
        "min_delay_seconds": 0.5,
        "max_delay_seconds": 60.0,
        "adaptive": True,
        "stop_after_consecutive_timeouts": 5,
        # A per-origin ceiling on requests in flight at once, not a promise:
        # the crawler starts well below it and the adaptive throttle only
        # widens toward it on sustained success. 1 is the sequential crawler.
        "concurrency": 1,
    },
    "output": {
        "dir": "",
        "write_pages_jsonl": True,
        # A structured, per-URL decision log beside pages.jsonl (issue #134):
        # measured at one small JSON line per exclusion on the chain fixture,
        # so on by default rather than a diagnostic nobody remembers to enable.
        "write_decisions_jsonl": True,
        # The prioritized backlog beside audit.json. build_tasks has always
        # taken an audit document and a native crawl has always produced one,
        # but nothing joined them: the backlog was reachable only through
        # `sf run --tasks`, so a crawl done without Screaming Frog produced
        # findings and no list of what to do about them. It is pure computation
        # over the audit already in memory -- no network, no second pass --
        # which is why it is on rather than something to remember.
        "write_tasks": True,
    },
    "link_position": {
        # Off by default: classifying every link's DOM ancestry (nav, header,
        # sidebar, footer, content) has a real per-link cost, and most crawls
        # never read the result. See tools/link_position.py.
        "classify": False,
        # Overrides the built-in nav/header/sidebar/footer selectors. Each
        # entry is {"position": ..., "selector": ...}; empty keeps the
        # built-ins. Plenty of production menus are not a <nav> element.
        "rules": [],
    },
    "link_attributes": {
        # Off by default, same reasoning as link_position.classify just above: the full
        # rel-token set, target attribute, and raw (pre-resolution) href add ~50% to
        # per-edge memory on a large crawl (measured on a synthetic 3387-page/
        # 150-link-per-page corpus, ~508k edges: roughly +95 bytes/edge, ~46 MiB total --
        # see LinkEdge's own docstring in crawl/spider.py for the breakdown, issue #125).
        # Only unsafe-cross-origin-link and protocol-relative-link detection need this;
        # the per-target follow/nofollow aggregation and the localhost-outlink check use
        # only fields already stored regardless of this setting.
        "capture": False,
    },
    "cache": {
        # off (default): no cache, reads or writes — a crawl has exactly the side effects it
        # had before this setting existed, per this project's own rule that a side effect (here,
        # writing response bodies to a fixed location outside any explicit output directory)
        # must be explicit, never hidden behind a default. live: real HTTP freshness semantics
        # (max-age/Expires, ETag/Last-Modified revalidation). replay: serve whatever is on disk
        # for a URL that already has an entry, at any age, without ever revalidating it — a URL
        # with no entry is still fetched live. See seohead.crawl.cache for the full policy.
        "mode": "off",
        # Force every lookup to miss (a live measurement happens) while still writing the
        # result back to the cache. A deliberate hard refresh, distinct from mode="off" -- and
        # rejected by validate() when mode="replay", since replay's own promise is the opposite
        # (never touch the network for an entry already on disk).
        "invalidate": False,
    },
    "resources": {
        "fetch": False,
        "max_requests": 20_000,
        "max_response_bytes": 5 * 1024 * 1024,
        "graph": {
            "max_requests": 2_000,
            "max_bytes": 100 * 1024 * 1024,
            "max_bytes_per_resource": 5 * 1024 * 1024,
            "max_seconds": 0,
            "max_origins": 1,
            "max_redirects": 0,
            "max_nesting": 2,
        },
    },
    "evidence": {
        # A scan artifact is evidence, not a reusable HTTP cache. Retaining a
        # no-store response still needs an explicit operator acknowledgement.
        "retain_no_store_acknowledged": False,
        "content_area": {
            "include_selector": "",
            "root_selector": "",
            "exclude_tags": ["nav", "header", "aside", "footer"],
            "exclude_selectors": [],
        },
        "extraction_rules": [],
    },
    "storage": {
        "format_version": "scan.v1",
        "capacity_profile": "stable",
        "body_mode": "captured_entity_bytes",
        "max_body_bytes": 5 * 1024 * 1024,
        "max_body_store_bytes": 10 * 1024 * 1024 * 1024,
        "min_free_bytes": 1024 * 1024 * 1024,
        "history_warning_bytes": 20 * 1024 * 1024 * 1024,
    },
    "rendering": {
        # raw: static HTML only, the crawler's original behaviour. legacy_
        # fragment: honour a page's own opt-in to the deprecated "#!" / "
        # ?_escaped_fragment_=" AJAX-crawling scheme when it declares one, no
        # browser required. js: execute JavaScript in a headless browser and
        # extract from the rendered DOM -- always in addition to, never
        # instead of, the links already found in the raw HTML, because a
        # link hydration removes is a real finding, not a link that never
        # existed. See seohead.crawl.render_escalation for how "js" and
        # "legacy_fragment" are applied selectively rather than to every URL.
        "mode": "raw",  # raw | legacy_fragment | js
        "escalation": {
            "policy": "sampled",  # sampled | full | auto
            # How many URLs per detected template pattern are probed
            # raw-versus-fuller before the whole pattern is escalated.
            # Sampling patterns, not every URL, is what keeps rendering an
            # order of magnitude cheaper than rendering the whole crawl.
            "sample_per_pattern": 2,
            # A budget for the re-fetch step, separate from the static
            # crawl's own limits.max_urls: escalating every page of a large
            # pattern would erase the saving sampling was meant to buy.
            "max_render_urls": 30,
            "max_render_seconds": 0,  # 0 = no wall-clock limit
        },
        "rendered_links": {
            # Store immutable raw/rendered eligible-anchor observations. This
            # never admits a route to the frontier or fetches it.
            "store": False,
            # Independently admit eligible rendered-only candidates through the
            # normal frontier after scope/depth/query validation.
            "crawl": False,
        },
        # Page interactions are an explicit project opt-in. They run only in
        # the already pinned render context and are persisted with per-URL
        # renderer provenance.
        "actions": [],
        "presets": [],
        "browser": {
            # Which headless engine Playwright launches. "chromium" keeps the
            # historical behaviour; each supported name reaches its matching
            # launcher -- an unknown name fails validation and no engine is
            # ever silently substituted.
            "engine": "chromium",  # chromium | firefox | webkit
            "transport": "local",  # local | remote; remote never launches a browser
            "remote_protocol": "playwright",
            "remote_endpoint_env": "",  # env variable name, not a stored endpoint/token
            "remote_playwright_version": "",  # operator-declared server version
            # How long JavaScript may keep running after the page and its
            # subresources have loaded. Too short loses content on a slow
            # application; too long multiplies crawl duration -- there is no
            # universally correct value, which is why it is recorded.
            "script_timeout_seconds": 10.0,
            # A responsive page renders a different DOM at different widths,
            # so word count and link count change with this setting.
            # "desktop" and "mobile" are the only presets (see
            # seohead.checks.render.VIEWPORT_PRESETS) so two runs are only
            # ever comparable by name, never by an arbitrary pixel value.
            "viewport": "desktop",  # desktop | mobile
            # Optional exact viewport size in CSS pixels. Both must be set
            # together (>0) to take effect; the pair overrides the named
            # preset's dimensions while the preset still carries the profile
            # (e.g. which user-agent shape the mobile profile implies). The
            # effective dimensions are recorded in renderer provenance, so
            # custom sizes stay comparable across runs too.
            "viewport_width": 0,
            "viewport_height": 0,
            # Grow the viewport to the rendered page's own height so lazily
            # loaded listings are captured, capped by
            # resize_to_content_max_height_px so a page that grows without
            # bound is truncated deterministically, not crawled forever.
            "resize_to_content": False,
            "resize_to_content_max_height_px": 15000,
            # Both match how a search engine assembles a page from what a
            # user's browser actually renders, and both change the extracted
            # content: a component's markup that lives only inside a shadow
            # root, or only inside a same-origin iframe, is otherwise absent
            # from page.content() entirely.
            "flatten_shadow_dom": False,
            "flatten_iframes": False,
            "device_pixel_ratio": 1.0,
            "mobile_emulation": False,
            "touch_emulation": False,
            # What counts as "loaded" before script_timeout_seconds starts
            # counting down. "networkidle" may never occur on a commercial
            # site (analytics, chat, ads keep connections open), turning a
            # useful render into a timeout -- see render.render_check.
            "wait_until": "load",  # load | domcontentloaded | networkidle
            # Maximum browser pages/contexts rendered at once during JS
            # escalation. 1 keeps the historical sequential behaviour. This is
            # a rendering bound only -- each slot holds a live browser context
            # plus its pinned network client -- and is unrelated to the HTTP
            # crawl's own fetch concurrency.
            "page_concurrency": 1,
            # Off by default, and refused without an explicit directory
            # (see validate() below): attaching a real browser profile
            # crawls the site as whoever's cookies that profile carries.
            "persistent_profile": False,
            "persistent_profile_dir": "",
        },
        "artifacts": {
            # Both off by default: heavy on disk, and most audits need
            # neither. Screenshots and console errors are per-URL artefacts,
            # not audit findings, so they never change what is found.
            "screenshots": False,
            "console_errors": False,
            "screenshot_mode": "full_page",  # full_page | first_screen
        },
    },
}

# Settings that can change what the audit finds. These go into the manifest.
# Everything else changes only duration or resource use.
RESULTS_AFFECTING: frozenset[str] = frozenset(
    {
        "scope.internal",
        # Narrowing to the start URL's folder removes whole paths from the
        # crawl (#1481) -- a scope change, not a speed knob.
        "scope.start_folder_only",
        "scope.include_patterns",
        "scope.exclude_patterns",
        "scope.include_extensions",
        "scope.exclude_extensions",
        "scope.include_media_types",
        "scope.exclude_media_types",
        "scope.exclude_hosts",
        # A host-matching segment widens which hosts count as internal, and
        # segments_only narrows the frontier to named segments -- both change what
        # is fetched, and segments alone also changes the audit's per-segment
        # breakdown even when nothing is excluded.
        "scope.segments",
        "scope.segments_only",
        "scope.normalize_tracking_parameters",
        "analysis.segments",
        "analysis.finding_exclusions",
        "analysis.canonical_policy.pagination",
        "analysis.canonical_policy.filters",
        "analysis.asset_thresholds.page_weight_max_bytes",
        "analysis.asset_thresholds.resource_max_bytes",
        # Seeding from the sitemap changes which URLs are fetched at all.
        "sitemaps.auto_discover",
        "discovery.hyperlinks.store",
        "discovery.hyperlinks.crawl",
        # Whether dropped hrefs leave evidence decides whether the invalid-link
        # check can run at all (#1530).
        "discovery.record_invalid_targets",
        "discovery.redirects.crawl",
        "discovery.external.store",
        # Whether external destinations are checked at all, and the bounds the
        # check runs under, decide which external answers a run holds -- a
        # smaller host or target budget produces more "skipped" records, which
        # is a different result, not a different cost.
        "discovery.external.crawl",
        "external_checks.max_targets",
        "external_checks.max_hosts",
        "external_checks.max_requests",
        "external_checks.max_depth",
        "external_checks.max_redirects",
        "discovery.follow_nofollow",
        "discovery.resolve_redirect_destination",
        "discovery.resolve_canonical_destination",
        # Every limit truncates the corpus, and a truncated crawl produces false
        # "not linked from anywhere" conclusions.
        "limits.max_urls",
        "limits.max_depth",
        "limits.max_query_variants_per_path",
        "limits.max_response_bytes",
        "limits.max_url_length",
        "limits.max_crawl_seconds",
        "limits.max_requests",
        # A short timeout turns slow pages into "no response"; the user agent and
        # headers change what a UA- or locale-adaptive site serves.
        "http.timeout_seconds",
        "http.user_agent",
        "http.headers",
        "http.retry_on_timeout",
        # Upgrading http to https before the request changes which URL is
        # fetched and collapses the two spellings into one page (#1484).
        "http.upgrade_to_https",
        "http.proxy",
        "http.proxy_allow_private",
        "http.proxy_identity",
        "http.proxy_authenticated",
        # Which host gets sent extra access changes what the crawl can reach.
        "http.credential_headers",
        "http.credentials_acknowledged",
        "http.auth",
        "http.auth_acknowledged",
        "http.zstd",
        "robots.policy",
        "robots.user_agent_token",
        "robots.unavailable_means_stop",
        # Politeness is normally cost-only, but a delay low enough to degrade a
        # server turns healthy pages into timeouts, and the audit then measures
        # the crawler rather than the site.
        "speed.min_delay_seconds",
        "speed.adaptive",
        "speed.stop_after_consecutive_timeouts",
        # Whether links are classified changes whether the inlink-composition
        # finding can be computed at all, and which rules classify them
        # changes where any given link lands.
        "link_position.classify",
        "link_position.rules",
        # Whether unsafe-cross-origin-link and protocol-relative-link detection can find
        # anything at all -- see LinkEdge's own docstring in crawl/spider.py.
        "link_attributes.capture",
        # A replay run can answer entirely from a stale cache; a forced-invalidate run trusted
        # nothing on disk. Both change whether the findings describe the site now or earlier.
        "cache.mode",
        "cache.invalidate",
        "storage.body_mode",
        "storage.format_version",
        "storage.capacity_profile",
        "resources.fetch",
        "resources.max_requests",
        "resources.max_response_bytes",
        "resources.graph.max_requests",
        "resources.graph.max_bytes",
        "resources.graph.max_bytes_per_resource",
        "resources.graph.max_seconds",
        "resources.graph.max_origins",
        "resources.graph.max_redirects",
        "resources.graph.max_nesting",
        "evidence.content_area.include_selector",
        "evidence.retain_no_store_acknowledged",
        "evidence.content_area.root_selector",
        "evidence.content_area.exclude_tags",
        "evidence.content_area.exclude_selectors",
        "evidence.extraction_rules",
        "storage.max_body_bytes",
        "storage.max_body_store_bytes",
        "storage.min_free_bytes",
        "storage.history_warning_bytes",
        # Every rendering setting below changes what the crawl finds on the
        # patterns it escalates -- see seohead.checks.render's module
        # docstring on why raw and rendered numbers are not comparable
        # unless the settings that produced each are recorded.
        "rendering.mode",
        "rendering.actions",
        "rendering.presets",
        "rendering.escalation.policy",
        "rendering.escalation.sample_per_pattern",
        "rendering.escalation.max_render_urls",
        "rendering.escalation.max_render_seconds",
        "rendering.rendered_links.store",
        "rendering.rendered_links.crawl",
        "rendering.browser.script_timeout_seconds",
        "rendering.browser.viewport",
        "rendering.browser.resize_to_content",
        "rendering.browser.resize_to_content_max_height_px",
        "rendering.browser.flatten_shadow_dom",
        "rendering.browser.flatten_iframes",
        "rendering.browser.device_pixel_ratio",
        "rendering.browser.mobile_emulation",
        "rendering.browser.touch_emulation",
        "rendering.browser.wait_until",
        "rendering.browser.engine",
        "rendering.browser.viewport_width",
        "rendering.browser.viewport_height",
        # More parallel pages change wall-clock only, not the DOM -- but the
        # bound is recorded so a run's render budget spend stays attributable.
        "rendering.browser.page_concurrency",
        "rendering.browser.transport",
        "rendering.browser.remote_protocol",
        "rendering.browser.remote_endpoint_env",
        "rendering.browser.remote_playwright_version",
        # A different profile crawls as a different, possibly logged-in,
        # visitor; the directory itself is not included here (nor in the
        # manifest below) for the same reason a credential's value is not:
        # a shareable manifest should not carry a local filesystem path.
        "rendering.browser.persistent_profile",
    }
)

# One-line descriptions, keyed by the same dotted paths as DEFAULTS/RESULTS_AFFECTING. This is the
# single source for every surface that lists settings for a human or an agent — the CLI's
# --config-help and, eventually, an MCP "describe settings" tool (#23) — so the three cannot drift
# into different descriptions of the same setting. A test fails if a DEFAULTS path has no entry here.
DESCRIPTIONS: dict[str, str] = {
    "analysis.asset_thresholds.page_weight_max_bytes": "Native declared-resource page budget in captured content-decoded bytes; incomplete runtime payloads remain lower bounds.",
    "analysis.asset_thresholds.resource_max_bytes": "Native per-resource budget in captured content-decoded bytes; defaults to 2 MiB.",
    "evidence.retain_no_store_acknowledged": (
        "Allow bounded SQLite body retention for anonymous responses carrying Cache-Control: "
        "no-store; off by default and never permits credentialed response retention."
    ),
    "evidence.content_area.include_selector": "Optional CSS selector for included main-content regions.",
    "evidence.content_area.root_selector": "Optional CSS root within which main content is extracted.",
    "evidence.content_area.exclude_tags": "HTML element names excluded from the saved main-content signature.",
    "evidence.content_area.exclude_selectors": "CSS selectors excluded from the saved main-content signature.",
    "evidence.extraction_rules": "Bounded declarative extraction rules; no executable code or arbitrary regular expressions.",
    "resources.fetch": "SQLite only: opt in to fetching directly declared same-origin scripts and stylesheets; never follows CSS imports or JavaScript modules.",
    "resources.max_requests": "SQLite only: maximum resource HTTP attempts, including redirects and retries; independent of the page URL limit.",
    "resources.max_response_bytes": "SQLite only: maximum content-decoded bytes per resource response; total crawl time and body-store limits still apply.",
    "resources.graph.max_requests": "scan.v2 only: bounded deduplicated declared-resource fetch count.",
    "resources.graph.max_bytes": "scan.v2 only: total bytes accepted by the declared-resource graph.",
    "resources.graph.max_bytes_per_resource": "scan.v2 only: maximum bytes accepted from one declared resource.",
    "resources.graph.max_seconds": "scan.v2 only: resource graph wall-clock budget; 0 means no graph-specific limit.",
    "resources.graph.max_origins": "scan.v2 only: maximum admitted resource origins.",
    "resources.graph.max_redirects": "scan.v2 only: maximum redirects per declared resource.",
    "resources.graph.max_nesting": "scan.v2 only: CSS import/url nesting depth.",
    "storage.body_mode": "SQLite only: captured_entity_bytes retains fetched HTML/DOM; off retains metadata only.",
    "storage.format_version": "Explicit scan storage format: scan.v1 (default) or scan.v2 for optional graph/event extensions.",
    "storage.capacity_profile": (
        "'stable' (default) admits explicit URL limits up to 1,000,000; native SQLite "
        "max_urls=0 disables the URL limit while retaining independent resource guards. "
        "'experimental_synthetic' is the compatible direct-storage fixture marker; "
        "live collectors refuse that marker. Neither setting certifies measured capacity."
    ),
    "storage.max_body_bytes": "SQLite only: maximum decoded bytes retained for one complete body.",
    "storage.max_body_store_bytes": "SQLite only: total unique encoded body bytes retained per scan.",
    "storage.min_free_bytes": "SQLite only: filesystem reserve; low space interrupts collection with a checkpoint.",
    "storage.history_warning_bytes": "SQLite only: history-size warning threshold; never enables automatic deletion.",
    "scope.internal": (
        "Which discovered URLs count as internal: 'host' (conservative) or "
        "'registrable_domain' (also accepts subdomains -- on a shared hosting suffix such as "
        "github.io or vercel.app, only the audited tenant's own subdomain, never another "
        "tenant's, per seohead.recon.net.registrable_domain)."
    ),
    "scope.start_folder_only": (
        "Fetch only URLs at or below the start URL's path folder: links above the folder "
        "count as unfollowed evidence (excluded under outside_start_folder). Refused for a "
        "start URL at the host root."
    ),
    "scope.include_patterns": "Regexes; a discovered link must match at least one to be followed.",
    "scope.exclude_patterns": "Regexes; a discovered link matching any of these is not followed.",
    "scope.include_extensions": (
        "Optional filename suffix allowlist for discovered routes (for example ['html', 'pdf']); "
        "matches the final URL-path suffix, without query or fragment. If set, extensionless "
        "routes are not included. Exclusions take precedence."
    ),
    "scope.exclude_extensions": (
        "Filename suffix denylist for discovered routes; matches the final URL-path suffix, "
        "without query or fragment. Exclusions take precedence over the extension allowlist."
    ),
    "scope.include_media_types": (
        "Optional response Content-Type allowlist checked after fetch and before parsing or body "
        "retention; supports exact types and type wildcards such as image/*. A missing or "
        "malformed type is recorded as unavailable when this list is set."
    ),
    "scope.exclude_media_types": (
        "Response Content-Type denylist checked after fetch and before parsing or body retention; "
        "supports exact types and type wildcards such as image/*. Exclusions take precedence."
    ),
    "scope.exclude_hosts": "Hosts never fetched regardless of what links to them.",
    "scope.segments": (
        "Ordered, first-match-wins named segments for a multilingual or multi-regional "
        "site: [{'name': ..., 'prefix': '/en/', 'host': 'en.example.com', "
        "'pattern': '...'}, ...] -- at least one of prefix/host/pattern per entry. A "
        "URL matching none of them is the built-in 'default' segment, so every URL "
        "belongs to exactly one segment, always."
    ),
    "scope.segments_only": (
        "Fetch only these segment names (plus 'default' when named); empty means "
        "every segment is in scope. Subsumes a subfolder-only or single-subdomain "
        "crawl without an ad-hoc scope.include_patterns regex."
    ),
    "scope.normalize_tracking_parameters": (
        "Remove utm_* and fbclid GET parameters from native crawl targets; other query parameters stay intact."
    ),
    "analysis.segments": (
        "Post-crawl segment rules: [{'name': ..., 'rules': [{'op': 'eq|prefix|contains|regex|in|segment', "
        "'field': ..., 'value': ...}]}]. Rules may use page fields or another segment."
    ),
    "analysis.finding_exclusions": (
        "Post-crawl finding suppressions only; never narrows URLs fetched. Ordered rules use "
        "Python regex search on a finding's target_url, optional exact check IDs, and a required "
        "reason. First matching rule wins."
    ),
    "analysis.canonical_policy.pagination": (
        "Ordered URL-regex policies for paginated pages: [{'pattern': ..., 'policy': "
        "'self|first_page|landing', 'target': absolute URL}]. Non-self policies require an "
        "explicit target; the first matching rule wins. Empty means unmeasured, not a finding."
    ),
    "analysis.canonical_policy.filters": (
        "Ordered URL-regex policies for configured filter pages: [{'pattern': ..., 'policy': "
        "'self|landing', 'target': absolute URL}]. Landing policies require an explicit target; "
        "the first matching rule wins. Empty means unmeasured, not a finding."
    ),
    "sitemaps.auto_discover": (
        "Seed the crawl from the sitemap declared in robots.txt when no explicit "
        "sitemap URL is given."
    ),
    "discovery.hyperlinks.store": "Keep discovered hyperlinks in the report.",
    "discovery.hyperlinks.crawl": "Request discovered hyperlinks (fetch them).",
    "discovery.record_invalid_targets": (
        "Record hrefs that cannot become fetchable link targets (unparsable syntax, a "
        "non-http(s) scheme) per page, with the raw attribute value and the rejection "
        "reason. Off by default; recorded targets are data and never enter the frontier."
    ),
    "discovery.redirects.crawl": "Request discovered redirect targets (fetch them).",
    "discovery.external.store": "Keep discovered external links in the report.",
    "discovery.external.crawl": (
        "After the internal frontier closes, check each recorded external destination once "
        "— opt-in, bounded by external_checks.*, never recursive. Requires "
        "discovery.external.store and a site crawl (--url); refused on the native SQLite "
        "route and in list mode."
    ),
    "discovery.follow_nofollow": "Follow links marked rel=nofollow instead of skipping them.",
    "discovery.resolve_redirect_destination": (
        "List mode only: follow a fetched redirect past its first hop to where it actually "
        "lands, recording every hop. Depth stays 0; this is a per-URL chain walk, not link "
        "discovery."
    ),
    "discovery.resolve_canonical_destination": (
        "List mode only: follow a fetched page's canonical declaration through a bounded "
        "chain, recording every inspected target without adding it to the page population."
    ),
    "limits.max_urls": (
        "Maximum number of URLs the crawl will fetch; 0 disables the URL limit for "
        "native SQLite site scans, which collect until their queue is exhausted. Values above "
        f"{MAX_URLS_CEILING:,} are refused rather than clamped; narrow the scope instead."
    ),
    "limits.max_depth": "Maximum link depth from the start URL; -1 disables the depth limit for native SQLite site scans.",
    "limits.max_query_variants_per_path": "Maximum distinct query strings kept per URL path.",
    "limits.max_response_bytes": "Response bodies larger than this are truncated before parsing.",
    "limits.max_url_length": "URLs longer than this are not fetched.",
    "limits.max_crawl_seconds": "Wall-clock budget for the whole crawl; 0 means no limit.",
    "limits.max_requests": (
        "Total HTTP attempts for the crawl, including bootstrap, redirects and retries; 0 means "
        f"no total-attempt limit. Explicit values may be at most {MAX_REQUESTS_CEILING:,}, "
        "allowing robots, sitemaps, redirects and retries above the URL budget."
    ),
    "external_checks.max_targets": (
        "Distinct external destinations the opt-in check may fetch at most — recorded "
        "outlinks plus same-host continuations admitted under max_depth. Destinations "
        "past the budget are recorded as skipped, not silently dropped."
    ),
    "external_checks.max_hosts": (
        "Distinct external origins the opt-in check may contact at most; redirect hops "
        "count toward it, since a redirect is a request to another origin."
    ),
    "external_checks.max_requests": (
        "HTTP attempts the external check may spend, including redirect hops and retries; "
        "0 means no phase-specific cap. Separate from limits.max_requests."
    ),
    "external_checks.max_depth": (
        "Hops of same-host links followed on external pages that answered; 0 checks "
        "recorded destinations only. Off-host links are never followed."
    ),
    "external_checks.max_redirects": (
        "Redirect hops followed per external destination before the chain is recorded "
        "unresolved; hard ceiling is 10."
    ),
    "http.timeout_seconds": "Per-request timeout in seconds.",
    "http.user_agent": "Request User-Agent string; empty uses the toolkit's identifiable default.",
    "http.headers": (
        "Extra request headers to send with every fetch. With --set, pass a JSON object."
    ),
    "http.retry_on_timeout": "Number of retries after a request times out.",
    "http.upgrade_to_https": (
        "Fetch every http URL over https instead, before any request: no redirect hop "
        "is recorded for the upgrade, the two spellings collapse into one page, and "
        "each upgraded page is marked. Turn on only for hosts that enforce transport "
        "security -- a host without https answers with connection errors."
    ),
    "http.proxy": (
        "Explicit http:// forward proxy with host and port, or env:VARIABLE containing its URL "
        "and optional username/password. No ambient proxy variables are used by a native crawl."
    ),
    "http.proxy_allow_private": (
        "Explicitly allow only the configured proxy endpoint to resolve privately; target URL "
        "private-network guards remain unchanged."
    ),
    "http.proxy_identity": "Resolved nonsecret proxy endpoint; managed by the crawler, not user input.",
    "http.proxy_authenticated": "Whether the resolved proxy used authentication; managed by the crawler.",
    "http.credential_headers": (
        "Host-bound extra headers for authenticated crawling: "
        "[{'host': ..., 'headers': {name: 'env:VAR_NAME'}}]."
    ),
    "http.credentials_acknowledged": "Must be true for http.credential_headers to take effect.",
    "http.auth": (
        "Host-bound HTTP Basic/Digest entries: [{'host': 'example.test', 'scheme': 'basic|digest', "
        "'username_env': 'env:USER_VAR', 'password_env': 'env:PASS_VAR'}]."
    ),
    "http.auth_acknowledged": "Must be true when http.auth is non-empty; authenticated crawls follow site links.",
    "http.zstd": "Request zstd-compressed HTTP response bodies when Python's stdlib or the optional zstandard decoder is available.",
    "robots.policy": (
        "'respect' (obey), 'report_only' (fetch, report what would be blocked, crawl "
        "anyway), or 'ignore' (do not fetch robots.txt)."
    ),
    "robots.user_agent_token": "The User-Agent token matched against robots.txt rules.",
    "robots.unavailable_means_stop": "Stop the crawl if robots.txt cannot be fetched at all.",
    "speed.min_delay_seconds": "Minimum delay between requests; the floor beneath adaptive back-off.",
    "speed.max_delay_seconds": "Maximum delay adaptive back-off may reach.",
    "speed.adaptive": "Increase the delay automatically when the target slows down or times out.",
    "speed.stop_after_consecutive_timeouts": "Stop the crawl after this many timeouts in a row.",
    "speed.concurrency": (
        "Per-origin ceiling on requests in flight at once. The adaptive throttle starts "
        "well below it and only grows toward it on sustained success; 1 is sequential."
    ),
    "output.dir": "Directory to write pages.jsonl and audit.json into; empty writes nothing to disk.",
    "output.write_pages_jsonl": "Write one JSON line per fetched page to pages.jsonl.",
    "output.write_decisions_jsonl": (
        "Write one JSON line per exclusion decision to decisions.jsonl, naming the URL and the "
        "rule that rejected it — see seohead.checks.logscan for what reads it."
    ),
    "output.write_tasks": (
        "Write the prioritized backlog beside audit.json as tasks.json and tasks.md — the same "
        "pipeline `sf run --tasks` uses, run over this crawl's own audit."
    ),
    "link_position.classify": (
        "Classify each link's DOM ancestry (nav/header/sidebar/footer/content) as it is "
        "parsed, at no extra requests; off by default because storing a position per link "
        "costs memory on a large crawl."
    ),
    "link_position.rules": (
        "Ordered [{'position', 'selector'}] overrides for the built-in nav/header/sidebar/"
        "footer selectors; empty keeps the built-ins. Only read when classify is true."
    ),
    "link_attributes.capture": (
        "Store each link's full rel tokens, target attribute, and raw (pre-resolution) href "
        "-- needed for unsafe-cross-origin-link and protocol-relative-link detection, at a "
        "real memory cost on a large crawl."
    ),
    "cache.mode": (
        "'off' (default: no cache), 'live' (real HTTP freshness: max-age/Expires, "
        "ETag/Last-Modified revalidation), or 'replay' (serve any cached entry regardless of "
        "age, without revalidating; a URL with no entry yet is still fetched live)."
    ),
    "cache.invalidate": (
        "Force every cache lookup to miss (a live measurement happens) while still writing the "
        "result back to the cache. Does not disable the cache; 'cache.mode=off' does that. "
        "Rejected together with cache.mode='replay', which promises the opposite guarantee."
    ),
    "rendering.mode": (
        "'raw' (static HTML only), 'legacy_fragment' (honour a page's own "
        "'_escaped_fragment_' opt-in), or 'js' (execute JavaScript in a headless "
        "browser, selectively -- see rendering.escalation)."
    ),
    "rendering.escalation.policy": (
        "sampled: probe template patterns and escalate those that diverge; full: render every "
        "eligible URL within the independent URL/time budgets; auto: sampled, and also escalate "
        "patterns whose raw page carries little text (the desktop Auto mode)."
    ),
    "rendering.escalation.sample_per_pattern": (
        "URLs probed raw-versus-fuller per detected template pattern before deciding "
        "whether the whole pattern needs escalation."
    ),
    "rendering.escalation.max_render_urls": (
        "Maximum number of pages re-fetched under the fuller representation, across all "
        "escalated patterns combined."
    ),
    "rendering.escalation.max_render_seconds": (
        "Wall-clock budget for the escalation step; 0 means no limit."
    ),
    "rendering.rendered_links.store": (
        "Store eligible a[href] route observations from static and rendered documents; "
        "this records evidence only and never crawls discovered routes."
    ),
    "rendering.rendered_links.crawl": (
        "Admit eligible rendered-link candidates through the normal bounded crawl frontier; "
        "independent of rendered route evidence storage."
    ),
    "rendering.actions": (
        "Bounded declarative page actions (click, scroll, wait_for_selector, wait_ms, extract); "
        "extract reads selector-based text or href/value fields; no user-supplied JavaScript."
    ),
    "rendering.presets": (
        "Opt-in named page-action presets: expand_tabs, load_more, read_datalayer."
    ),
    "rendering.browser.script_timeout_seconds": (
        "How long JavaScript may keep running after the page and its subresources have "
        "loaded, before the DOM is captured."
    ),
    "rendering.browser.engine": (
        "Headless engine Playwright launches: 'chromium', 'firefox' or 'webkit'. An "
        "unsupported name fails validation; an uninstalled browser binary reports a "
        "distinct install hint. Firefox cannot emulate a mobile viewport (is_mobile)."
    ),
    "rendering.browser.viewport": "Viewport preset used for rendering: 'desktop' or 'mobile'.",
    "rendering.browser.viewport_width": (
        "Custom viewport width in CSS pixels (0 = unset). Must be set together with "
        "viewport_height; the pair overrides the named preset's dimensions."
    ),
    "rendering.browser.viewport_height": (
        "Custom viewport height in CSS pixels (0 = unset). Must be set together with "
        "viewport_width; the pair overrides the named preset's dimensions."
    ),
    "rendering.browser.transport": "Launch a local browser (default) or connect to an explicitly supplied remote Playwright server.",
    "rendering.browser.remote_protocol": "Remote browser protocol; only Playwright is supported, not CDP.",
    "rendering.browser.remote_endpoint_env": "Name of an environment variable containing the remote ws/wss endpoint; never store the endpoint in a scan.",
    "rendering.browser.remote_playwright_version": "Operator-declared remote Playwright version; its major/minor must match the installed client.",
    "rendering.browser.resize_to_content": (
        "Grow the viewport to the rendered page's own height before capture, capped by "
        "resize_to_content_max_height_px."
    ),
    "rendering.browser.resize_to_content_max_height_px": (
        "Deterministic cap on resize_to_content, so an unbounded page is truncated rather "
        "than crawled forever."
    ),
    "rendering.browser.flatten_shadow_dom": (
        "Merge open shadow roots into their host elements before capturing the DOM."
    ),
    "rendering.browser.flatten_iframes": (
        "Replace same-origin iframes with their own document's body content before capture."
    ),
    "rendering.browser.device_pixel_ratio": "Device scale factor used for rendering.",
    "rendering.browser.mobile_emulation": "Emulate a mobile device (touch UA, is_mobile).",
    "rendering.browser.touch_emulation": "Emulate touch input.",
    "rendering.browser.wait_until": (
        "Page-load strategy before script_timeout_seconds starts counting down: 'load', "
        "'domcontentloaded', or 'networkidle'."
    ),
    "rendering.browser.page_concurrency": (
        "Maximum browser pages/contexts rendered at once during JS escalation; 1 keeps "
        "sequential behaviour. A rendering bound, not HTTP crawl concurrency."
    ),
    "rendering.browser.persistent_profile": (
        "Unavailable with the pinned renderer: requested persistent profiles return an "
        "explicit unavailable result and are not opened. Off by default."
    ),
    "rendering.browser.persistent_profile_dir": (
        "Directory for the persistent profile; required when persistent_profile is true, "
        "and never a computed default."
    ),
    "rendering.artifacts.screenshots": "Save one screenshot per rendered URL.",
    "rendering.artifacts.screenshot_mode": (
        "Capture the full page or only the initial viewport when screenshots are enabled."
    ),
    "rendering.artifacts.console_errors": "Capture browser console error messages per rendered URL.",
}

# Environment overrides, applied between the file and explicit arguments.
ENV_OVERRIDES: dict[str, str] = {
    "SEOHEAD_CRAWL_MAX_URLS": "limits.max_urls",
    "SEOHEAD_CRAWL_MAX_DEPTH": "limits.max_depth",
    "SEOHEAD_CRAWL_MIN_DELAY": "speed.min_delay_seconds",
    "SEOHEAD_CRAWL_ROBOTS": "robots.policy",
    "SEOHEAD_CRAWL_USER_AGENT": "http.user_agent",
}

ROBOTS_POLICIES = ("respect", "report_only", "ignore")
INTERNAL_SCOPES = ("host", "registrable_domain")
# The segment every URL falls into when it matches none of scope.segments. Reserved:
# an operator cannot declare a segment under this name, since it would collide with
# the one every unmatched URL already gets (#358).
DEFAULT_SEGMENT = "default"
_SEGMENT_KEYS = frozenset({"name", "prefix", "host", "pattern"})
CACHE_MODES = ("live", "off", "replay")
RENDER_MODES = ("raw", "legacy_fragment", "js")
RENDER_VIEWPORTS = ("desktop", "mobile")
RENDER_WAIT_UNTIL = ("load", "domcontentloaded", "networkidle")
# Headless engines Playwright can launch through the pinned renderer in
# seohead/checks/render.py; keep in step with that module's BROWSER_ENGINES.
RENDER_ENGINES = ("chromium", "firefox", "webkit")
# Engines Playwright can emulate a mobile viewport (is_mobile) on -- it
# documents the option as unsupported on Firefox. Touch input (has_touch) is
# a general context option every engine accepts, so touch_emulation alone is
# not gated by this set.
RENDER_MOBILE_EMULATION_ENGINES = frozenset({"chromium", "webkit"})
# Upper bound for a custom viewport dimension; matches the largest canvas
# real devices report and keeps absurd values from reaching the browser.
MAX_VIEWPORT_DIMENSION_PX = 16384
# Upper bound for simultaneous render pages/contexts; each holds a live
# browser context plus its network client, so this stays deliberately low.
MAX_RENDER_PAGE_CONCURRENCY = 16

# A reference to an environment variable, never an inline secret. This is the
# only value shape a credential header may carry in a config file.
_ENV_REF_RE = re.compile(r"^env:([A-Za-z_][A-Za-z0-9_]*)$")

# These headers authenticate a request. They may not use the global
# ``http.headers`` lane: that lane is deliberately sent with every fetch.
SENSITIVE_HEADER_NAMES = frozenset(
    {"authorization", "proxy-authorization", "cookie", "x-api-key", "x-auth-token"}
)

# Applied automatically to the crawl scope once credential_headers is set. A
# crawler clicks every link, and while logged in that includes links that log
# out, publish, or delete — this is a default, not a guarantee, and a caller
# who has audited the site can still widen scope.exclude_patterns.
DESTRUCTIVE_PATH_PATTERNS: tuple[str, ...] = (
    r"(?i)/(log[-_]?out|sign[-_]?out)(?:/|$|\?)",
    r"(?i)/(delete|remove|destroy|unsubscribe)(?:/|$|\?)",
)


class ConfigError(ValueError):
    """A configuration that cannot be trusted to mean what it says."""


def _flatten(mapping: dict[str, Any], prefix: str = "") -> dict[str, Any]:
    """Dotted paths to leaf values. Free-form maps are leaves, not branches."""
    out: dict[str, Any] = {}
    for key, value in mapping.items():
        path = f"{prefix}.{key}" if prefix else key
        if isinstance(value, dict) and path not in ("http.headers",):
            out.update(_flatten(value, path))
        else:
            out[path] = value
    return out


def _set_path(mapping: dict[str, Any], path: str, value: Any) -> None:
    parts = path.split(".")
    node = mapping
    for part in parts[:-1]:
        node = node.setdefault(part, {})
    node[parts[-1]] = value


def _coerce(path: str, raw: str) -> Any:
    """Environment and command-line values arrive as strings; give them the default's type."""
    current = _flatten(DEFAULTS).get(path)
    if isinstance(current, bool):
        value = raw.strip().lower()
        if value in ("1", "true", "yes", "on"):
            return True
        if value in ("0", "false", "no", "off"):
            return False
        raise ValueError("expected true or false")
    if isinstance(current, int) and not isinstance(current, bool):
        return int(raw)
    if isinstance(current, float):
        return float(raw)
    if isinstance(current, list):
        # A list setting is a set of patterns or hosts; an empty string means the
        # empty list rather than a list containing one empty pattern, which would
        # match everything and silently widen a crawl.
        return [item.strip() for item in raw.split(",") if item.strip()]
    if isinstance(current, dict):
        value = json.loads(raw)
        if not isinstance(value, dict) or any(
            not isinstance(name, str) or not isinstance(header, str)
            for name, header in value.items()
        ):
            raise ValueError("expected a JSON object with string keys and values")
        return value
    return raw


def parse_setting_assignment(text: str) -> tuple[str, Any]:
    """Turn one ``path=value`` argument into a validated override.

    The path is checked against DEFAULTS here rather than at validate() time, so a
    typo is reported as a typo -- naming the setting the operator meant to reach --
    instead of surfacing later as an unknown-key error about a path nobody typed.
    """
    path, sep, raw = text.partition("=")
    path = path.strip()
    if not sep or not path:
        if "proxy" in text.lower():
            raise ConfigError("--set expects PATH=VALUE for http.proxy")
        raise ConfigError(f"--set expects PATH=VALUE, got {text!r}")
    known = _flatten(DEFAULTS)
    if path not in known:
        near = [k for k in sorted(known) if k.split(".")[-1] == path.split(".")[-1]]
        hint = f"; did you mean {near[0]}?" if near else "; see --config-help"
        raise ConfigError(f"unknown setting {path!r}{hint}")
    try:
        return path, _coerce(path, raw)
    except ValueError as exc:
        if path == "http.proxy":
            raise ConfigError("http.proxy value is invalid") from None
        raise ConfigError(f"{path}={raw!r} is not valid: {exc}") from exc


def delay_for_request_rate(rate: float) -> float:
    """The ``speed.min_delay_seconds`` that caps a crawl at ``rate`` requests/second.

    The inverse of ``effective_request_rate``. Politeness is the number an operator
    actually has in mind -- a site owner says "no more than seven a second", not
    "at least 0.1428 seconds apart" -- and making them invert it themselves is how
    a decimal point in the wrong place becomes somebody's site under load.
    """
    if rate <= 0:
        raise ConfigError("--max-urls-per-second must be greater than 0")
    return 1.0 / rate


def _merge(base: dict[str, Any], overlay: dict[str, Any]) -> dict[str, Any]:
    out = copy.deepcopy(base)
    for key, value in overlay.items():
        if isinstance(value, dict) and isinstance(out.get(key), dict):
            out[key] = _merge(out[key], value)
        else:
            out[key] = value
    return out


def validate(config: dict[str, Any]) -> None:
    """Reject anything that cannot be honoured, naming the offending path."""
    known = set(_flatten(DEFAULTS))
    for path in _flatten(config):
        if path not in known:
            raise ConfigError(
                f"unknown setting {path!r}. A setting the crawler does not read would "
                "promise behaviour that does not exist"
            )

    robots = config["robots"]["policy"]
    if robots not in ROBOTS_POLICIES:
        raise ConfigError(f"robots.policy must be one of {ROBOTS_POLICIES}, got {robots!r}")
    scope = config["scope"]["internal"]
    if scope not in INTERNAL_SCOPES:
        raise ConfigError(f"scope.internal must be one of {INTERNAL_SCOPES}, got {scope!r}")
    if type(config["scope"]["normalize_tracking_parameters"]) is not bool:
        raise ConfigError("scope.normalize_tracking_parameters must be boolean")
    cache_mode = config["cache"]["mode"]
    if cache_mode not in CACHE_MODES:
        raise ConfigError(f"cache.mode must be one of {CACHE_MODES}, got {cache_mode!r}")
    if config["http"]["auth"] and cache_mode != "off":
        raise ConfigError(
            "http.auth requires cache.mode=off to avoid reusing authenticated responses"
        )
    if type(config["http"]["zstd"]) is not bool:
        raise ConfigError("http.zstd must be boolean")
    if config["http"]["zstd"]:
        if not zstd_decoder_available():
            raise ConfigError(
                "http.zstd requires Python 3.14+ compression.zstd or the optional zstandard package"
            )
        if cache_mode != "off":
            raise ConfigError(
                "http.zstd requires cache.mode=off so encoded responses are not mixed"
            )
    # "replay" promises it never touches the network for a URL already on disk;
    # "invalidate" promises the opposite -- a live measurement on every lookup.
    # Both are explicit, deliberate settings, so silently letting one win (issue
    # #137) is worse than refusing the combination outright.
    if cache_mode == "replay" and config["cache"]["invalidate"]:
        raise ConfigError(
            "cache.invalidate cannot be combined with cache.mode='replay': replay promises "
            "to serve on-disk entries without ever touching the network, which invalidate "
            "exists to override. Pick one: cache.mode='live' with cache.invalidate=true for "
            "a forced hard refresh, or drop invalidate to keep replaying from disk."
        )
    if "resources" in config:
        resources = config["resources"]
        if type(resources["fetch"]) is not bool:
            raise ConfigError("resources.fetch must be boolean")
        for name in ("max_requests", "max_response_bytes"):
            value = resources[name]
            if type(value) is not int or not 0 <= value <= 2**63 - 1:
                raise ConfigError(f"resources.{name} must be a nonnegative SQLite-sized integer")
        graph = resources["graph"]
        if not isinstance(graph, dict):
            raise ConfigError("resources.graph must be an object")
        for name, value in graph.items():
            if type(value) is not int or value < 0:
                raise ConfigError(f"resources.graph.{name} must be a nonnegative integer")
        if graph["max_requests"] < 1 or graph["max_bytes_per_resource"] < 1:
            raise ConfigError(
                "resources.graph request and per-resource byte limits must be positive"
            )
        if graph["max_origins"] < 1:
            raise ConfigError("resources.graph.max_origins must be positive")
    if "storage" in config:
        storage = config["storage"]
        if storage["format_version"] not in {"scan.v1", "scan.v2"}:
            raise ConfigError("storage.format_version must be scan.v1 or scan.v2")
        if type(storage["capacity_profile"]) is not str or storage["capacity_profile"] not in {
            "stable",
            "experimental_synthetic",
        }:
            raise ConfigError("storage.capacity_profile must be stable or experimental_synthetic")
        if storage["body_mode"] not in {"off", "captured_entity_bytes"}:
            raise ConfigError("storage.body_mode must be off or captured_entity_bytes")
        for name in (
            "max_body_bytes",
            "max_body_store_bytes",
            "min_free_bytes",
            "history_warning_bytes",
        ):
            value = storage[name]
            if type(value) is not int or not 0 <= value <= 2**63 - 1:
                raise ConfigError(f"storage.{name} must be a nonnegative SQLite-sized byte count")
        if storage["body_mode"] != "off" and storage["max_body_bytes"] == 0:
            raise ConfigError(
                "storage.max_body_bytes must be positive when body capture is enabled"
            )
    # A pattern that does not compile would otherwise fail mid-crawl, after the
    # site has already been asked for a few hundred pages.
    for key in ("include_patterns", "exclude_patterns"):
        for pattern in config["scope"][key] or ():
            try:
                re.compile(pattern)
            except re.error as exc:
                raise ConfigError(f"scope.{key}: {pattern!r} is not a valid regex: {exc}") from exc

    _validate_file_type_filters(config["scope"])

    limits = config["limits"]
    if type(limits["max_urls"]) is not int or limits["max_urls"] < 0:
        raise ConfigError("limits.max_urls must be a nonnegative integer")
    if limits["max_urls"] > MAX_URLS_CEILING:
        raise ConfigError(
            f"limits.max_urls is {limits['max_urls']:,}, above this crawler's ceiling of "
            f"{MAX_URLS_CEILING:,}; narrow the scope or split the work into resumable scans."
        )
    if type(limits["max_depth"]) is not int or limits["max_depth"] < -1:
        raise ConfigError("limits.max_depth must be an integer of at least -1")
    if type(limits["max_requests"]) is not int or limits["max_requests"] < 0:
        raise ConfigError("limits.max_requests must be a nonnegative integer")
    if limits["max_requests"] > MAX_REQUESTS_CEILING:
        raise ConfigError(
            f"limits.max_requests is above the crawler request ceiling of "
            f"{MAX_REQUESTS_CEILING:,}; split the work into resumable scans instead"
        )
    if limits["max_query_variants_per_path"] < 0:
        # 0 is the crawler's own "unlimited" (see spider.py's truthy check on this
        # value); a negative number is not a smaller budget, it makes every
        # comparison against it (`len(variants) >= cap`) true immediately, which
        # rejects the first query URL on every path and every one after it (#195).
        raise ConfigError("limits.max_query_variants_per_path cannot be negative")
    if config["speed"]["min_delay_seconds"] < 0:
        raise ConfigError("speed.min_delay_seconds cannot be negative")
    if config["speed"]["concurrency"] < 1:
        raise ConfigError("speed.concurrency must be at least 1")

    external = config["discovery"]["external"]
    if type(external["store"]) is not bool or type(external["crawl"]) is not bool:
        raise ConfigError("discovery.external.store and discovery.external.crawl must be boolean")
    if external["crawl"] and not external["store"]:
        raise ConfigError(
            "discovery.external.crawl requires discovery.external.store: destinations "
            "are checked from the recorded edges, and store=False keeps no edges"
        )
    for name in ("max_targets", "max_hosts", "max_requests", "max_depth", "max_redirects"):
        value = config["external_checks"][name]
        if type(value) is not int or value < 0:
            raise ConfigError(f"external_checks.{name} must be a nonnegative integer")
    # The chain-walk ceiling lives in seohead/crawl/collect.py
    # (MAX_REDIRECT_CHAIN_HOPS); settings cannot import collect without a
    # circular dependency, so the literal is mirrored here.
    if config["external_checks"]["max_redirects"] > 10:
        raise ConfigError(
            "external_checks.max_redirects cannot exceed 10 (the redirect-chain ceiling)"
        )

    # A SQLite site scan may disable population/time limits while retaining its
    # filesystem reserve and durable checkpoint. Materialized routes refuse an
    # unlimited URL population at their admission boundary.
    if (
        not limits["max_urls"]
        and limits["max_depth"] in (-1, 0)
        and not limits["max_crawl_seconds"]
        and not limits["max_requests"]
        and not config["storage"]["min_free_bytes"]
    ):
        raise ConfigError(
            "a crawl needs at least one budget or a positive storage.min_free_bytes reserve"
        )

    for rule in config["link_position"]["rules"]:
        if not isinstance(rule, dict) or not rule.get("position") or not rule.get("selector"):
            raise ConfigError(
                f"link_position.rules entries need both 'position' and 'selector'; got {rule!r}"
            )

    _validate_segments(config["scope"])
    for name, value in config["analysis"].get("asset_thresholds", {}).items():
        if type(value) is not int or not 0 < value < 2**63:
            raise ConfigError(
                f"analysis.asset_thresholds.{name} must be a positive SQLite-sized integer"
            )
    if not isinstance(config["analysis"]["segments"], list):
        raise ConfigError("analysis.segments must be a list")
    from seohead.checks.finding_exclusions import validate_rules

    try:
        validate_rules(config["analysis"]["finding_exclusions"])
    except ValueError as exc:
        raise ConfigError(f"analysis.finding_exclusions: {exc}") from exc
    from seohead.core.canonical_policy import validate_canonical_policy

    try:
        validate_canonical_policy(
            config["analysis"]["canonical_policy"], path="analysis.canonical_policy"
        )
    except ValueError as exc:
        raise ConfigError(str(exc)) from exc
    _validate_http_headers(config["http"])
    _validate_credential_headers(config["http"])
    _validate_http_auth(config["http"])
    route = resolve_proxy(config)
    identity = config["http"]["proxy_identity"]
    authenticated = config["http"]["proxy_authenticated"]
    if not isinstance(identity, str) or type(authenticated) is not bool:
        raise ConfigError("http.proxy route facts are invalid")
    if identity and (route is None or identity != route.identity):
        raise ConfigError("http.proxy_identity differs from the configured proxy")
    if identity and authenticated is not route.authenticated:
        raise ConfigError("http.proxy_authenticated differs from the configured proxy")
    if not identity and authenticated:
        raise ConfigError("http.proxy_authenticated is managed by the crawler")
    if config["http"]["proxy"] and config["cache"]["mode"] != "off":
        raise ConfigError(
            "http.proxy requires cache.mode=off to avoid mixing direct and proxy evidence"
        )
    _validate_rendering(config["rendering"])


def resolve_proxy(config: dict[str, Any]):
    """Resolve an explicit proxy reference without storing its credentials in settings."""
    from seohead.recon.net import resolve_proxy_route

    allow_private = config["http"]["proxy_allow_private"]
    if type(allow_private) is not bool:
        raise ConfigError("http.proxy_allow_private must be true or false")
    try:
        return resolve_proxy_route(config["http"]["proxy"], allow_private=allow_private)
    except ValueError as exc:
        raise ConfigError(str(exc)) from None


def _validate_segments(scope: dict[str, Any]) -> None:
    """Every segment is named, ordered, and matches by at least one declared rule.

    ``segments_only`` is checked against the names declared here so a typo'd
    segment name is refused before the crawl runs rather than silently scoping
    to nothing (#358).
    """
    segments = scope["segments"]
    if not isinstance(segments, list):
        raise ConfigError("scope.segments must be a list")
    names: set[str] = set()
    for entry in segments:
        if not isinstance(entry, dict):
            raise ConfigError(f"scope.segments entries must be objects, got {entry!r}")
        extra = set(entry) - _SEGMENT_KEYS
        if extra:
            raise ConfigError(f"scope.segments entry has unknown keys {sorted(extra)}: {entry!r}")
        name = entry.get("name")
        if not isinstance(name, str) or not name:
            raise ConfigError(f"scope.segments entry needs a non-empty 'name': {entry!r}")
        if name == DEFAULT_SEGMENT:
            raise ConfigError(
                f"scope.segments entry cannot be named {DEFAULT_SEGMENT!r}: that name is "
                "reserved for a URL matching none of the declared segments"
            )
        if name in names:
            raise ConfigError(f"scope.segments has duplicate name {name!r}")
        names.add(name)
        if not entry.get("prefix") and not entry.get("host") and not entry.get("pattern"):
            raise ConfigError(
                f"scope.segments entry {name!r} needs at least one of 'prefix', 'host' "
                "or 'pattern' to match by"
            )
        pattern = entry.get("pattern")
        if pattern:
            try:
                re.compile(pattern)
            except re.error as exc:
                raise ConfigError(
                    f"scope.segments entry {name!r}: pattern {pattern!r} is not a valid "
                    f"regex: {exc}"
                ) from exc

    segments_only = scope["segments_only"]
    if not isinstance(segments_only, list):
        raise ConfigError("scope.segments_only must be a list")
    allowed = names | {DEFAULT_SEGMENT}
    unknown = [name for name in segments_only if name not in allowed]
    if unknown:
        raise ConfigError(
            f"scope.segments_only names {unknown} that scope.segments never declares "
            f"(known: {sorted(allowed)})"
        )


def _validate_file_type_filters(scope: dict[str, Any]) -> None:
    """Validate the URL-suffix and response-media filters as separate contracts."""
    extension_pattern = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]*\Z")
    media_token = r"[A-Za-z0-9!#$%&'+\-.^_`|~]+"
    media_pattern = re.compile(rf"{media_token}/(?:{media_token}|\*)\Z")

    for name in ("include_extensions", "exclude_extensions"):
        setting = f"scope.{name}"
        values = scope[name]
        if type(values) is not list:
            raise ConfigError(f"{setting} must be a list of filename extensions")
        seen: set[str] = set()
        for value in values:
            if type(value) is not str:
                raise ConfigError(f"{setting} entries must be strings, got {value!r}")
            extension = value[1:] if value.startswith(".") else value
            if not extension_pattern.fullmatch(extension):
                raise ConfigError(
                    f"{setting} entry {value!r} must be one filename suffix "
                    "(letters, digits, underscore or hyphen; optional leading dot)"
                )
            normalized = extension.lower()
            if normalized in seen:
                raise ConfigError(
                    f"{setting} contains duplicate suffix {normalized!r} after normalization"
                )
            seen.add(normalized)

    for name in ("include_media_types", "exclude_media_types"):
        setting = f"scope.{name}"
        values = scope[name]
        if type(values) is not list:
            raise ConfigError(f"{setting} must be a list of media types")
        seen: set[str] = set()
        for value in values:
            if type(value) is not str or not media_pattern.fullmatch(value):
                raise ConfigError(
                    f"{setting} entry {value!r} must be an exact type or type wildcard "
                    "such as text/html or image/*"
                )
            normalized = value.lower()
            if normalized in seen:
                raise ConfigError(
                    f"{setting} contains duplicate media type {normalized!r} after normalization"
                )
            seen.add(normalized)


def _validate_rendering(rendering: dict[str, Any]) -> None:
    if rendering["mode"] not in RENDER_MODES:
        raise ConfigError(
            f"rendering.mode must be one of {RENDER_MODES}, got {rendering['mode']!r}"
        )

    from seohead.checks.page_actions import preset_actions, validate_actions

    try:
        actions, action_budget = validate_actions(rendering.get("actions", []))
        presets, preset_budget = preset_actions(rendering.get("presets", []))
        if len(actions) + len(presets) > 20 or action_budget + preset_budget > 60_000:
            raise ValueError("page actions exceed the combined step or time budget")
    except ValueError as exc:
        raise ConfigError(f"rendering page actions: {exc}") from exc

    escalation = rendering["escalation"]
    if rendering["artifacts"].get("screenshot_mode", "full_page") not in {
        "full_page",
        "first_screen",
    }:
        raise ConfigError("rendering.artifacts.screenshot_mode must be full_page or first_screen")
    if type(rendering["rendered_links"]["store"]) is not bool:
        raise ConfigError("rendering.rendered_links.store must be a boolean")
    if type(rendering["rendered_links"]["crawl"]) is not bool:
        raise ConfigError("rendering.rendered_links.crawl must be a boolean")
    if escalation.get("policy", "sampled") not in {"sampled", "full", "auto"}:
        raise ConfigError("rendering.escalation.policy must be sampled, full or auto")
    if escalation["sample_per_pattern"] < 1:
        raise ConfigError("rendering.escalation.sample_per_pattern must be at least 1")
    if escalation["max_render_urls"] < 0:
        raise ConfigError("rendering.escalation.max_render_urls cannot be negative")
    if escalation["max_render_seconds"] < 0:
        raise ConfigError("rendering.escalation.max_render_seconds cannot be negative")

    browser = rendering["browser"]
    from seohead.checks.browser_transport import validate_config as validate_browser_transport

    try:
        validate_browser_transport(browser, embedded=True)
    except ValueError as exc:
        raise ConfigError(f"rendering.browser: {exc}") from exc
    if browser["viewport"] not in RENDER_VIEWPORTS:
        raise ConfigError(
            f"rendering.browser.viewport must be one of {RENDER_VIEWPORTS}, "
            f"got {browser['viewport']!r}"
        )
    if browser["engine"] not in RENDER_ENGINES:
        raise ConfigError(
            f"rendering.browser.engine must be one of {RENDER_ENGINES}, got {browser['engine']!r}"
        )
    # Only mobile viewport emulation is gated: Playwright documents is_mobile
    # as unsupported on Firefox, while has_touch is a general context option
    # every engine accepts.
    if browser["engine"] not in RENDER_MOBILE_EMULATION_ENGINES and browser["mobile_emulation"]:
        raise ConfigError(
            f"rendering.browser.engine={browser['engine']!r} cannot emulate a "
            "mobile viewport (Playwright does not implement is_mobile for it); "
            "unset mobile_emulation or pick another engine"
        )
    for name in ("viewport_width", "viewport_height"):
        value = browser[name]
        if type(value) is not int or value < 0 or value > MAX_VIEWPORT_DIMENSION_PX:
            raise ConfigError(
                f"rendering.browser.{name} must be an integer between 0 and "
                f"{MAX_VIEWPORT_DIMENSION_PX}, got {value!r}"
            )
    if (browser["viewport_width"] > 0) != (browser["viewport_height"] > 0):
        raise ConfigError(
            "rendering.browser.viewport_width and "
            "rendering.browser.viewport_height must be set together "
            "(both >0 or both 0)"
        )
    if type(browser["page_concurrency"]) is not int or not (
        1 <= browser["page_concurrency"] <= MAX_RENDER_PAGE_CONCURRENCY
    ):
        raise ConfigError(
            f"rendering.browser.page_concurrency must be an integer between 1 "
            f"and {MAX_RENDER_PAGE_CONCURRENCY}, got {browser['page_concurrency']!r}"
        )
    if browser["wait_until"] not in RENDER_WAIT_UNTIL:
        raise ConfigError(
            f"rendering.browser.wait_until must be one of {RENDER_WAIT_UNTIL}, "
            f"got {browser['wait_until']!r}"
        )
    if browser["script_timeout_seconds"] < 0:
        raise ConfigError("rendering.browser.script_timeout_seconds cannot be negative")
    if browser["resize_to_content_max_height_px"] < 1:
        raise ConfigError("rendering.browser.resize_to_content_max_height_px must be at least 1")
    if browser["device_pixel_ratio"] <= 0:
        raise ConfigError("rendering.browser.device_pixel_ratio must be positive")
    if browser["persistent_profile"] and not browser["persistent_profile_dir"]:
        # No default directory on purpose: a default would eventually collide
        # with someone's real, cookie-carrying browser profile. Naming one is
        # the explicit choice this setting requires.
        raise ConfigError(
            "rendering.browser.persistent_profile is true but "
            "persistent_profile_dir is empty; a persistent profile must name "
            "a directory explicitly"
        )


def _validate_credential_headers(http: dict[str, Any]) -> None:
    entries = http["credential_headers"]
    if not entries:
        return
    if not http["credentials_acknowledged"]:
        raise ConfigError(
            "http.credential_headers is set but http.credentials_acknowledged is not true; "
            "a crawler clicks every link, including ones that log out, publish, or delete, "
            "so authenticated crawling needs an explicit acknowledgement"
        )
    for entry in entries:
        if not isinstance(entry, dict) or not entry.get("host"):
            raise ConfigError(
                "each http.credential_headers entry needs a host binding; an unbound "
                "credential is sent on every request and the leak is silent"
            )
        host = entry["host"]
        headers = entry.get("headers")
        if not isinstance(headers, dict) or not headers:
            raise ConfigError(f"http.credential_headers entry for {host!r} needs headers")
        for name, value in headers.items():
            match = _ENV_REF_RE.match(str(value))
            if not match:
                raise ConfigError(
                    f"http.credential_headers header {name!r} for {host!r} must reference an "
                    "environment variable as 'env:VAR_NAME', never an inline value"
                )
            if match.group(1) not in os.environ:
                raise ConfigError(
                    f"http.credential_headers for {host!r} references environment variable "
                    f"{match.group(1)!r}, which is not set"
                )


def _validate_http_auth(http: dict[str, Any]) -> None:
    entries = http["auth"]
    if type(http["auth_acknowledged"]) is not bool:
        raise ConfigError("http.auth_acknowledged must be boolean")
    if not isinstance(entries, list):
        raise ConfigError("http.auth must be a list")
    if entries and not http["auth_acknowledged"]:
        raise ConfigError(
            "http.auth is set but http.auth_acknowledged is not true; authenticated crawls follow links"
        )
    seen_hosts: set[str] = set()
    header_hosts = {str(entry.get("host", "")).lower() for entry in http["credential_headers"]}
    for entry in entries:
        if not isinstance(entry, dict) or set(entry) != {
            "host",
            "scheme",
            "username_env",
            "password_env",
        }:
            raise ConfigError(
                "each http.auth entry requires exactly host, scheme, username_env and password_env"
            )
        host = entry["host"]
        if not isinstance(host, str) or not host or host != host.lower() or "/" in host:
            raise ConfigError("each http.auth host must be a lowercase hostname")
        if host in seen_hosts:
            raise ConfigError(f"http.auth has duplicate host {host!r}")
        seen_hosts.add(host)
        if host in header_hosts:
            raise ConfigError(
                f"http.auth and http.credential_headers cannot both configure {host!r}"
            )
        if not isinstance(entry["scheme"], str) or entry["scheme"] not in {"basic", "digest"}:
            raise ConfigError(f"http.auth scheme for {host!r} must be basic or digest")
        for field in ("username_env", "password_env"):
            ref = entry[field]
            match = _ENV_REF_RE.fullmatch(ref) if isinstance(ref, str) else None
            if match is None:
                raise ConfigError(f"http.auth {field} for {host!r} must reference env:VAR")
            if match.group(1) not in os.environ:
                raise ConfigError(
                    f"http.auth for {host!r} references environment variable "
                    f"{match.group(1)!r}, which is not set"
                )


def _validate_http_headers(http: dict[str, Any]) -> None:
    headers = http["headers"]
    if not isinstance(headers, dict) or any(
        not isinstance(name, str) or not isinstance(value, str) for name, value in headers.items()
    ):
        raise ConfigError("http.headers must be an object mapping header names to string values")
    if any(name.lower() in SENSITIVE_HEADER_NAMES for name in headers):
        raise ConfigError(
            "http.headers cannot carry credentials; use host-bound, environment-sourced "
            "http.credential_headers instead"
        )


def redact_sensitive_headers(headers: Any) -> Any:
    """Copy a header map while redacting credential values from legacy mappings."""
    if not isinstance(headers, dict):
        return headers
    return {
        name: "REDACTED"
        if isinstance(name, str) and name.lower() in SENSITIVE_HEADER_NAMES
        else value
        for name, value in headers.items()
    }


def load(
    path: str | None = None,
    overrides: dict[str, Any] | None = None,
    *,
    base_overrides: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Resolve defaults, project defaults, file, environment, then explicit overrides.

    The order is fixed and tested. Explicit arguments win because they are the
    most local statement of intent.
    """
    config = copy.deepcopy(DEFAULTS)

    for setting, value in (base_overrides or {}).items():
        if value is None:
            continue
        if isinstance(value, dict) and not isinstance(_flatten(DEFAULTS).get(setting), dict):
            raise ConfigError(
                f"base override {setting!r} is a mapping; use a dotted setting path instead"
            )
        _set_path(config, setting, value)

    if path:
        try:
            with open(path, encoding="utf-8") as handle:
                from_file = json.load(handle)
        except OSError as exc:
            raise ConfigError(f"cannot read config {path!r}: {exc}") from exc
        except ValueError as exc:
            raise ConfigError(f"config {path!r} is not valid JSON: {exc}") from exc
        if not isinstance(from_file, dict):
            raise ConfigError(f"config {path!r} must contain an object")
        config = _merge(config, from_file)

    for variable, setting in ENV_OVERRIDES.items():
        raw = os.environ.get(variable)
        if raw is not None and raw != "":
            try:
                _set_path(config, setting, _coerce(setting, raw))
            except ValueError as exc:
                raise ConfigError(f"{variable}={raw!r} is not valid for {setting}: {exc}") from exc

    for setting, value in (overrides or {}).items():
        if value is None:
            continue
        # Dotted paths only. A nested mapping here replaces the whole subtree and
        # takes its siblings' defaults with it, and the failure then surfaces from
        # validate() as a bare KeyError about a key the caller never mentioned.
        if isinstance(value, dict) and not isinstance(_flatten(DEFAULTS).get(setting), dict):
            raise ConfigError(
                f"override {setting!r} is a mapping; use dotted paths such as "
                f"{setting}.{next(iter(value), 'key')} so sibling defaults survive"
            )
        _set_path(config, setting, value)

    if config["http"]["credential_headers"] or config["http"]["auth"]:
        existing = config["scope"]["exclude_patterns"]
        config["scope"]["exclude_patterns"] = existing + [
            p for p in DESTRUCTIVE_PATH_PATTERNS if p not in existing
        ]

    from seohead.checks.finding_exclusions import validate_rules

    try:
        config["analysis"]["finding_exclusions"] = validate_rules(
            config["analysis"]["finding_exclusions"]
        )
    except ValueError as exc:
        raise ConfigError(f"analysis.finding_exclusions: {exc}") from exc
    from seohead.crawl.user_agents import apply_defaults as _apply_user_agent

    _apply_user_agent(config["http"], config["robots"])
    validate(config)
    route = resolve_proxy(config)
    config["http"]["proxy_identity"] = route.identity if route else ""
    config["http"]["proxy_authenticated"] = route.authenticated if route else False
    return config


def _redact_credential_headers(entries: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Host and header names are what makes a run reproducible; values are not."""
    return [
        {
            "host": entry.get("host", ""),
            "headers": dict.fromkeys(entry.get("headers") or {}, "REDACTED"),
        }
        for entry in entries
    ]


def _redact_http_auth(entries: list[dict[str, Any]]) -> list[dict[str, str]]:
    """Keep authentication method/host provenance, never env-var references."""
    return [{"host": entry.get("host", ""), "scheme": entry.get("scheme", "")} for entry in entries]


def manifest(config: dict[str, Any]) -> dict[str, Any]:
    """The resolved values of every setting that can change what was found.

    Resolved values, not their sources: a report that says "the default was used"
    is not reproducible once the default moves. Credential values are the one
    exception — the manifest is meant to be shared for review, not to carry
    secrets, so only the host and header names survive here.
    """
    flat = _flatten(config)
    out = {}
    for path in sorted(RESULTS_AFFECTING):
        if path not in flat:
            continue
        value = flat[path]
        if path == "http.headers":
            value = redact_sensitive_headers(value)
        elif path == "http.credential_headers":
            value = _redact_credential_headers(value)
        elif path == "http.auth":
            value = _redact_http_auth(value)
        elif path == "http.proxy":
            identity = config["http"].get("proxy_identity", "")
            authenticated = config["http"].get("proxy_authenticated", False)
            if not identity and value:
                route = resolve_proxy(config)
                identity = route.identity if route else ""
                authenticated = route.authenticated if route else False
            value = {
                "mode": "proxy" if identity else "direct",
                "endpoint": identity or None,
                "authenticated": authenticated,
            }
        out[path] = value
    return out


def resolve_credential_headers(entries: list[dict[str, Any]], host: str) -> dict[str, str]:
    """Headers bound to ``host``, with their environment references resolved.

    Resolved fresh for each request rather than carried from a previous one:
    a redirect to a different host asks this again and gets nothing back,
    which is what keeps a credential from crossing a cross-host redirect.
    """
    host = (host or "").lower()
    resolved: dict[str, str] = {}
    for entry in entries or ():
        if (entry.get("host") or "").lower() != host:
            continue
        for name, ref in (entry.get("headers") or {}).items():
            match = _ENV_REF_RE.match(str(ref))
            if match:
                resolved[name] = os.environ.get(match.group(1), "")
    return resolved


def request_headers(config: dict[str, Any]) -> dict[str, str] | None:
    """Explicit non-secret request headers, including opt-in zstd negotiation."""
    http = config["http"]
    headers = dict(http["headers"])
    if http["zstd"]:
        accept_key = next(
            (name for name in headers if name.lower() == "accept-encoding"),
            "Accept-Encoding",
        )
        accepted = [
            part.strip().lower() for part in headers.get(accept_key, "gzip, deflate").split(",")
        ]
        if "zstd" not in accepted:
            accepted.append("zstd")
        headers[accept_key] = ", ".join(accepted)
    return headers or None


def describe_settings() -> list[dict[str, Any]]:
    """Every configurable setting: its dotted path, type, default, and description.

    This is the one source that a CLI ``--config-help`` and an eventual MCP
    "describe settings" tool (#23) both read, so the two cannot drift into
    different descriptions of the same setting. Generated from ``DEFAULTS`` and
    ``DESCRIPTIONS`` rather than hand-maintained per surface.
    """
    flat = _flatten(DEFAULTS)
    out = []
    for path in sorted(flat):
        default = flat[path]
        out.append(
            {
                "path": path,
                "type": type(default).__name__,
                "default": default,
                "results_affecting": path in RESULTS_AFFECTING,
                "description": DESCRIPTIONS[path],
            }
        )
    return out


def fingerprint(config: dict[str, Any]) -> str:
    """Stable identity for "this configuration", to detect a change across runs.

    Used to invalidate a resumable crawl's checkpoint: a frontier built under
    one scope or limit set must not be resumed under another, or the result
    silently mixes rules from two different runs.
    """
    import hashlib
    import json

    payload = json.dumps(manifest(config), sort_keys=True, ensure_ascii=False)
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()[:16]


def effective_request_rate(config: dict[str, Any]) -> float:
    """Worst-case requests per second this configuration permits.

    Politeness is a combination, not a single knob, so the number worth printing
    and gating on is this one rather than any individual setting.
    """
    delay = float(config["speed"]["min_delay_seconds"])
    return 1.0 / delay if delay > 0 else float("inf")


# Above this worst-case rate the crawl still runs; the operator is told, not stopped.
# This is a warning threshold, not a limit: the pacer's max(2.0, rate) is a ceiling.
RATE_WARN_RPS = 2.0


def zstd_decoder_available() -> bool:
    """Whether a bounded zstd decoder is available for explicitly negotiated responses."""
    try:
        from compression import zstd
    except ImportError:
        try:
            import zstandard
        except ImportError:
            return False
        return hasattr(zstandard, "ZstdDecompressor")
    return hasattr(zstd, "open")


def rate_warnings(config: dict[str, Any]) -> list[str]:
    """Warnings about the worst-case rate; empty while the rate is at or under the threshold."""
    rate = effective_request_rate(config)
    if rate <= RATE_WARN_RPS:
        return []
    shown = "unbounded" if rate == float("inf") else f"{rate:.2f}"
    return [
        f"effective rate {shown} req/s is above {RATE_WARN_RPS:g} req/s; "
        "confirm the host tolerates it"
    ]


def load_estimate(config: dict[str, Any]) -> dict[str, Any]:
    """Return configured rate, a rate-cap duration floor, and an honest disk null."""
    rate = effective_request_rate(config)
    url_budget = config["limits"]["max_urls"]
    duration = url_budget / rate if url_budget and 0 < rate < float("inf") else None
    duration_reason = (
        None
        if duration is not None
        else "a finite URL population and finite request rate are required"
    )
    return {
        "format": "crawl-load-estimate.v1",
        "request_rate_rps": None if rate == float("inf") else rate,
        "request_rate_state": "unbounded" if rate == float("inf") else "configured_limit",
        "duration_floor_seconds": duration,
        "duration_reason": duration_reason,
        "disk_estimate_bytes": None,
        "disk_reason": (
            "retained page, link and body sizes vary; configured storage caps are limits, not a measured estimate"
        ),
        "body_storage_budget_bytes": config.get("storage", {}).get("max_body_store_bytes"),
    }


def rate_fields(config: dict[str, Any]) -> dict[str, Any]:
    """Configured rate, warnings and nullable pre-run capacity estimates."""
    rate = effective_request_rate(config)
    return {
        "effective_max_requests_per_second": "unbounded" if rate == float("inf") else rate,
        "warnings": rate_warnings(config),
        "load_estimate": load_estimate(config),
    }
