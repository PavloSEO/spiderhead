"""Typed return shapes for the most-reused dict contracts in the toolkit.

Every dict below already exists at runtime: `seohead.checks.parser.parse_url`
and `seohead.checks.robots.check_robots` have returned exactly these keys since
before this module existed. Adding a `TypedDict` for them is purely additive —
it gives a type checker (and a reader) something to verify a caller against,
without changing what either function returns or asking any caller to change.

`TypedDict` was chosen over a dataclass or a Pydantic model for this first
slice because both call sites are already deep in the codebase (crawl,
recon, the CLI/MCP handler layer) passing plain dicts around; a `TypedDict`
describes that same dict, at zero runtime cost, with no migration required.
A Pydantic model remains the right choice where runtime validation at a
boundary is wanted (see issue #31) — that trade-off should stay a deliberate
per-shape decision, not a blanket rule.

Two shapes are covered here, in order of reuse:

- ``ParseResult`` (via ``ParseFetched`` / ``ParseFailed``): the return of
  ``parser.parse_url`` / ``parser.parse_html``, reused by
  ``seohead.checks.page_facts``, ``seohead.checks.links``,
  ``seohead.crawl.collect``, ``seohead.recon.backlinks``,
  ``seohead.recon.regions``, and the ``parse`` / ``citability_check`` /
  ``social_meta_check`` handlers — the single most-shared shape in the package.
- ``RobotsCheckResult`` (via ``RobotsCheckFound`` / ``RobotsCheckError``): the
  return of ``robots.check_robots``, the ``robots_check`` handler's shape and
  a representative "ok/error envelope with a couple of optional keys"
  pattern that recurs across most of the other 42 handlers.
"""

from __future__ import annotations

import sys
from typing import Any, Literal

if sys.version_info >= (3, 12):
    from typing import TypedDict
else:  # pragma: no cover - exercised by the 3.10/3.11 CI jobs
    # Pydantic refuses typing.TypedDict below 3.12: on those versions the
    # runtime cannot see which keys are required, so a shape used in an MCP
    # tool signature would be validated against nothing. typing_extensions
    # backports the __required_keys__ machinery pydantic needs.
    from typing_extensions import TypedDict


class _LinkInfoOptional(TypedDict, total=False):
    # Only present when the caller opts in via options["classify_links"]=True
    # (off by default) — see parser.parse_html / tools/link_position.py.
    position: str


class LinkInfo(_LinkInfoOptional):
    """One `<a href>` extracted from a page."""

    href: str
    # The href attribute exactly as written, before resolution against the document base —
    # the only place a protocol-relative ("//host/path") form is still visible (issue #125).
    raw_href: str
    text: str
    rel: str
    # The anchor's own target attribute (e.g. "_blank"), "" when absent. Kept alongside `rel`
    # so a crawl can tell a cross-origin new-tab link from an ordinary one (issue #125).
    target: str
    nofollow: bool
    external: bool


class HreflangAlternate(TypedDict):
    """One `<link rel="alternate" hreflang="...">` declaration (issue #357).

    The code and the href are kept exactly as the document wrote them: a code
    with the wrong case or a malformed region is itself a finding, and
    normalising on capture would hide it. ``url`` is that same href resolved
    against the document base -- what a browser does -- so a reciprocity check
    can compare targets without the original being lost.
    """

    lang: str
    raw_href: str
    url: str


class HeadingRef(TypedDict):
    """One `<h1>`-`<h6>` in document order, with where on the page it sits (issue #632).

    ``level`` and ``text`` are what the eight existing heading checks read as an
    unordered set; keeping them in a sequence is what lets a check ask whether a
    heading precedes the page's first H1. ``region`` reuses the link-position
    taxonomy (``seohead.checks.link_position.POSITIONS``) so "in the header"
    means the same thing about a heading as it already does about a link, and is
    ``""`` when the document offered nothing to place the heading against --
    unmeasured, which is neither chrome nor content.
    """

    level: int
    text: str
    region: str


class HeadingLinkRef(TypedDict):
    """One anchor wrapped in an `<h1>`-`<h6>` (issue #634).

    ``level`` is the *nearest* heading ancestor's level, which is what a reader
    sees; ``anchor`` is the link's own collapsed text, which on this defect is
    usually the whole heading.
    """

    level: int
    destination: str
    anchor: str


class ImageLinkRef(TypedDict):
    """One image link that names nothing about where it goes (issue #634)."""

    destination: str


class DuplicateId(TypedDict):
    """One repeated HTML id value and its observed occurrence count."""

    id: str
    count: int


class LinkPlacement(TypedDict):
    """The two link defects a page region cannot show (issue #634).

    ``seohead.checks.link_position`` answers "which part of the template is this
    link in". A link wrapped in the page's own H1 can sit squarely in ``content``
    and still point the heading away from the page, and an image link with nothing
    to read is a property of what the anchor contains rather than of where it sits.
    Both lists are capped (``parser._MAX_PLACEMENT_ITEMS``); the ``*_total`` counts
    beside them are the whole counts, so a truncated list reads as truncated.
    """

    in_heading: list[HeadingLinkRef]
    in_heading_total: int
    image_no_text: list[ImageLinkRef]
    image_no_text_total: int


class TrustSignal(TypedDict):
    """One objective authorship/date markup observation (issue #823).

    ``signal`` names the carrier exactly -- e.g. ``meta_author``,
    ``jsonld_date:datePublished``, ``time_element``, ``url_path_date`` -- so an
    auditor can review which markup produced the evidence rather than trusting a
    blended verdict. ``confidence`` is ``high`` for an explicit machine-readable
    declaration and ``medium`` for a conventional carrier (a ``byline`` class, a
    ``<time>`` element, a date-shaped URL path). ``value`` is the declared
    string the markup carried (an author name, a datetime, the matched path
    fragment), truncated by the parser; ``""`` when the carrier holds no value.
    """

    signal: str
    confidence: str
    value: str


class TrustSignals(TypedDict):
    """A page's authorship/date/article-scope markup evidence (issue #823).

    An empty list is a measured absence: the page was parsed and declared no
    such signal. ``article`` holds the markers that place the page in the
    authorship checks' scope (a JSON-LD article ``@type``, ``og:type=article``,
    an ``<article>`` element) -- the page declaring itself content-shaped, which
    is a fact about its markup, never a judgement about its quality.
    """

    author: list[TrustSignal]
    dates: list[TrustSignal]
    article: list[str]


class FrameInfo(TypedDict):
    """One `<iframe>` extracted from a page (issue #360).

    A framed document is not part of the parent DOM, so every text-derived
    measurement -- word count, headings, the content area -- describes the shell
    around the content rather than the content. Recording what a page frames is
    what lets a check say "the copy is in an iframe" instead of "this page is
    thin", which is the same fact read the wrong way round.
    """

    # Resolved against the document base; "" when the src attribute is absent
    # (a JavaScript-populated frame, which is still a frame).
    src: str
    # The src exactly as written, before resolution -- the only place a
    # protocol-relative or empty form stays visible.
    raw_src: str
    # Same registrable scope as the page, by the same rule links use. A
    # same-origin frame holds content the site owns; a third-party frame is
    # usually an embed and is judged differently.
    same_origin: bool
    # Whether the frame sits inside the resolved content area. An iframe in a
    # footer is a widget; an iframe in the content area is the page's content.
    in_content_area: bool
    title: str
    loading: str
    sandbox: str


class ImageInfo(TypedDict):
    """One `<img>` as the coverage checks need it (issues #385, #386).

    ``has_alt`` is whether the attribute is present at all, which is a different
    fact from its length: ``alt=""`` is a deliberate mark on a decorative image
    and must stay silent, while a missing attribute is the finding. Collapsing
    the two into one truthy value is how a correctly marked-up site gets told to
    add alt text it already declined on purpose.
    """

    has_alt: bool
    alt_length: int


class FormInfo(TypedDict):
    """One `<form>` extracted from a page (issue #125)."""

    method: str
    # Resolved against the document base; the page's own URL when the attribute is absent or
    # empty, per the HTML standard's own default for form submission.
    action: str
    has_password: bool


class _ParsedPageOptional(TypedDict, total=False):
    # Only present when the caller opts in via options["url_sources"]=True
    # (off by default) — see parser.parse_html.
    url_sources: list[dict[str, str]]
    # Direct script/stylesheet declarations, including repeated occurrences,
    # are a separate bounded opt-in from the generic URL inventory.
    resource_declarations: list[ResourceDeclaration]
    resource_declarations_omitted: int


class ResourceDeclaration(TypedDict):
    """One direct script or stylesheet declaration in source order."""

    kind: Literal["script", "stylesheet"]
    url: str
    raw_url: str


class DocumentPosition(TypedDict):
    """Where key elements sit relative to `<head>`, as `parser.parse_html`'s
    parse tree resolved them -- not as the source text suggests (issue #123).

    A browser closes `<head>` at the first element that does not belong there,
    so a canonical or robots directive placed after one silently stops
    applying; the page still reads fine in the source. Each `*_outside_head`
    flag is `None` when the element is simply absent -- a different, already
    covered finding -- and a bool only once it exists.
    """

    head_count: int
    body_count: int
    head_not_first: bool
    invalid_head_elements: list[str]
    title_outside_head: bool | None
    meta_description_outside_head: bool | None
    canonical_outside_head: bool | None
    directives_outside_head: bool | None
    hreflang_outside_head: bool | None


class ParsedPage(_ParsedPageOptional):
    """The on-page fields `parser.parse_html` extracts (pure, no network)."""

    title: str | None
    meta_description: str | None
    robots: str | None
    robots_meta: list[str]
    # Same values as robots_meta, prefixed "<name>: " for every tag but the
    # generic one -- see parser.robots_meta_scoped. Native-crawl evidence joins
    # this, not robots_meta, so a Bingbot/Yandex-only directive keeps its scope.
    robots_meta_scoped: list[str]
    canonical: str | None
    # Raw canonical and pagination link attributes; resolved targets are separate.
    head_links: list[dict[str, str]]
    position: DocumentPosition
    # Static Lighthouse audits (issue #59) — see seohead.sf.core.rules and
    # seohead.sf.core.lighthouse for the correspondence and doc links.
    charset: str | None
    doctype: str | None
    viewport: str | None
    # <html amp> / <html ⚡> presence, data only (#1021 slice 1).
    html_amp: bool
    # The document's <html lang> claim: {"declared", "declared_primary"}, both "" when absent.
    html_lang: dict[str, str]
    # The <meta http-equiv="refresh"> content attribute exactly as written,
    # "" when the page declares none -- the same fact SF's Meta Refresh 1
    # column carries, so one check reads it whichever source produced it.
    meta_refresh: str
    og: dict[str, str]
    twitter: dict[str, str]
    headings: dict[str, list[str]]
    # The same headings as a sequence rather than a set: every h1-h6 with text,
    # in DOM order, each carrying its level, text and page region (#632). The
    # grouping above answers "which levels exist and how many"; only this
    # answers "in what order" and "where on the page".
    heading_outline: list[HeadingRef]
    jsonld: list[Any]
    jsonld_invalid: list[dict[str, Any]]
    links: list[LinkInfo]
    # Where those same anchors sit: the ones inside a heading, and the image
    # links that name nothing (#634). ``None`` when link parsing was switched
    # off, which is "not measured" rather than "none found".
    link_placement: LinkPlacement | None
    forms: list[FormInfo]
    text: str
    # The whole body, and the content area alone. word_count follows the
    # content area, because a nav-and-footer word count describes the template
    # rather than the page.
    content_text: str
    content_area_strategy: str
    word_count: int
    # Always extracted: see parse_html. Empty when the page declares none.
    hreflang: list[HreflangAlternate]
    # Every <iframe> the document declares -- see FrameInfo. Empty when the
    # text option is off, because in_content_area needs the resolved root.
    frames: list[FrameInfo]
    # How many <applet>/<embed>/<object> elements the page carries, for the
    # unsupported-plugin check. Always computed: it is one tree lookup, and a
    # key the parser returns but this model does not declare is a contract
    # the typed-handler gate refuses.
    plugin_elements_count: int
    # rel=alternate declarations with a media attribute and no href (#1016).
    mobile_alternate_broken: int
    # Per-image alt facts -- see ImageInfo. Empty when the page has no images.
    images: list[ImageInfo]
    # Runs of boilerplate placeholder prose found in the content area.
    lorem_ipsum_count: int
    # The first H1's text when it comes only from an image's alt attribute,
    # None when the heading carries text of its own. A logo inside a text H1
    # is not an image-only heading, which is why this is a value and not a flag.
    h1_alt_only_text: str | None
    # The page's authorship/date/article-scope markup evidence (#823) — see
    # TrustSignals. Always extracted like heading_outline: each list capped and
    # empty on a measured absence, so an audit can review the signals behind a
    # finding instead of trusting a blended verdict.
    trust_signals: TrustSignals
    # Repeated ids are a bounded native-parser observation. ``[]`` means the
    # parsed document had none; parser-disabled evidence is represented by the
    # absence of the projected crawl field, never as a clean empty list.
    duplicate_ids: list[DuplicateId]
    # How many live <meta name="description"> tags the document declares.
    meta_description_count: int
    # The <meta name="fragment"> content attribute exactly as written, "" when
    # the page declares none -- the page-wide opt-in to Google's deprecated AJAX
    # crawling scheme. A value, not a flag: "!" is the scheme's own opt-in and
    # anything else is a different declaration a report should quote back.
    meta_fragment: str
    # The resolved target of the first <link rel="amphtml">, "" when none (#1020).
    amphtml: str
    # Each <link rel="alternate" media=...> declaration, resolved against the page URL (#1051).
    mobile_alternates: list[dict[str, str]]


class ParseFetched(ParsedPage):
    """`parser.parse_url` once the request itself completed: page fields plus
    fetch metadata. ``ok`` mirrors the HTTP response (``response.is_success``)
    and can be ``False`` here too — e.g. a clean 404 still parses the body."""

    url: str
    final_url: str
    status_code: int
    ok: bool


class ParseFailed(TypedDict):
    """`parser.parse_url` when the request itself raised (network, timeout,
    invalid URL, ...) before any response existed to parse."""

    url: str
    ok: Literal[False]
    error: str


# parser.parse_url's actual return type: one or the other, never a mix.
ParseResult = ParseFetched | ParseFailed


class ParseManyResult(TypedDict):
    """The `parse` handler's return: one `ParseResult` per requested URL."""

    count: int
    results: list[ParseResult]


class RobotsGroup(TypedDict):
    """One `User-agent:` group from a parsed robots.txt."""

    user_agents: list[str]
    allow: list[str]
    disallow: list[str]
    crawl_delay: float | None
    # Request-rate: requests/seconds, retained as its equivalent minimum
    # interval so consumers do not need to repeat the division or guess which
    # spelling was valid. ``None`` means absent or malformed.
    request_rate_delay: float | None


class ParsedRobots(TypedDict):
    """`robots.parse_robots`'s pure parse of robots.txt content."""

    groups: list[RobotsGroup]
    sitemaps: list[str]


class RobotsPathCheck(TypedDict):
    path: str
    allowed: bool


class _RobotsCheckFoundOptional(TypedDict, total=False):
    # Present only on the "no robots.txt" branch (status >= 400).
    note: str
    # Present only when the caller passed `paths`.
    path_checks: list[RobotsPathCheck]


class RobotsCheckFound(_RobotsCheckFoundOptional):
    """`robots.check_robots` once the robots.txt request itself succeeded
    (whether or not a robots.txt actually exists — see `exists`)."""

    ok: Literal[True]
    robots_url: str
    status_code: int
    exists: bool
    groups: list[RobotsGroup]
    sitemaps: list[str]


class _RobotsCheckErrorOptional(TypedDict, total=False):
    # An HTTP response was received but its rules could not be read.
    status_code: int


class RobotsCheckError(_RobotsCheckErrorOptional):
    """`robots.check_robots` when fetching robots.txt itself failed."""

    ok: Literal[False]
    robots_url: str
    error: str


# robots.check_robots's actual return type: one or the other, never a mix.
RobotsCheckResult = RobotsCheckFound | RobotsCheckError
