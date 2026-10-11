"""Where this toolkit stands against the field's published issue catalogue.

Screaming Frog publishes a list of the issues its crawler reports — 320 across 24 categories.
It is the closest thing technical SEO has to a shared checklist, so it is the honest yardstick
for "what does this find". Without it, "118 checks" is a number with nothing to compare to.

**Only the issue names are used here.** They are the field's shared vocabulary for these
problems — "Redirect Chain", "Missing Alt Text", "Orphan URLs" — and naming a problem is not
copying a description of it. Every explanation in this module is written about *our own*
behaviour: what we find, what we find only partly, what we do not find, and what we have
decided not to find. Nothing is reproduced from that page.

Each entry carries exactly one status:

``check``         a registry check finds it; ``refs`` names the check ids.
``tool``          a command outside the crawl registry finds it; ``refs`` names the commands.
``partial``       we find part of it. ``note`` says which part we miss.
``gap``           we should find it and do not. ``note`` says what is missing.
``out_of_scope``  we will not, and ``note`` says why it is a decision rather than an oversight.

``tests/test_sf_issue_map.py`` asserts every referenced check id and command still exists, so a
rename breaks the build instead of quietly making this document wrong.
"""

from __future__ import annotations

from dataclasses import dataclass

__all__ = ["CATEGORIES", "Entry", "coverage_counts", "entries"]

STATUSES = ("check", "tool", "partial", "gap", "out_of_scope")


@dataclass(frozen=True)
class Entry:
    name: str
    status: str
    refs: tuple[str, ...] = ()
    note: str = ""


def _c(name: str, *refs: str, note: str = "") -> Entry:
    return Entry(name, "check", tuple(refs), note)


def _t(name: str, *refs: str, note: str = "") -> Entry:
    return Entry(name, "tool", tuple(refs), note)


def _p(name: str, note: str, *refs: str) -> Entry:
    return Entry(name, "partial", tuple(refs), note)


def _g(name: str, note: str) -> Entry:
    return Entry(name, "gap", (), note)


def _o(name: str, note: str) -> Entry:
    return Entry(name, "out_of_scope", (), note)


CATEGORIES: dict[str, list[Entry]] = {
    "Response Codes": [
        _c("Internal No Response", "NO_RESPONSE"),
        _c("Internal Client Error (4XX)", "BROKEN_PAGE_4XX", "BROKEN_INTERNAL_LINK"),
        _c("Internal Server Error (5XX)", "SERVER_ERROR_5XX", "LINK_TO_5XX"),
        _c("Internal Redirect Loop", "REDIRECT_LOOP"),
        _c(
            "Internal Blocked by Robots.txt", "BLOCKED_BY_ROBOTS", "IMPORTANT_URL_BLOCKED_BY_ROBOTS"
        ),
        _c("Internal Blocked Resource", "ROBOTS_BLOCKS_RESOURCES"),
        _c("Internal Redirect Chain", "REDIRECT_CHAIN"),
        _p(
            "External Blocked Resource",
            "we do not fetch another host's robots.txt: a crawler that goes and asks somebody "
            "else's server about its own rules is a crawler that wanders",
        ),
        _c("Internal Redirection (3XX)", "INTERNAL_LINK_TO_REDIRECT", "BAD_REDIRECT_TYPE"),
        _c("Internal Redirection (Meta Refresh)", "META_REFRESH_REDIRECT"),
        _c("Internal Redirection (HTTP Refresh)", "HTTP_REFRESH_REDIRECT"),
        _p(
            "Internal Redirection (JavaScript)",
            "navigation is observed on rendered scans (`scan-navigation`, cause "
            "`script_navigation`), but no finding reports a script-initiated location change "
            "as a redirect yet, and the SF-export and legacy paths never see one",
        ),
        _t("External No Response", "links-check"),
        _c("External Client Error (4XX)", "BROKEN_EXTERNAL_LINK"),
        _c("External Server Error (5XX)", "BROKEN_EXTERNAL_LINK"),
    ],
    "Security": [
        _c("HTTP URLs", "HTTP_URL"),
        _c("Mixed Content", "MIXED_CONTENT", "INSECURE_SUBRESOURCE"),
        _c("Form URL Insecure", "FORM_URL_INSECURE"),
        _c("Form On HTTP URL", "FORM_ON_HTTP_URL"),
        _c("Missing HSTS Header", "MISSING_HSTS"),
        _c("Unsafe Cross Origin Links", "UNSAFE_CROSS_ORIGIN_LINK"),
        _c("Protocol-Relative Resource Links", "PROTOCOL_RELATIVE_LINK"),
        _c("Missing Content-Security-Policy Header", "MISSING_CSP"),
        _c("Missing X-Content-Type-Options Header", "MISSING_X_CONTENT_TYPE_OPTIONS"),
        _c("Missing X-Frames-Options Header", "MISSING_X_FRAME_OPTIONS"),
        _c("Missing Secure Referrer-Policy Header", "MISSING_REFERRER_POLICY"),
        _p(
            "Bad Content Type",
            "content type is recorded and drives HTML detection, but a mismatch between the "
            "declared type and the body is not asserted",
        ),
    ],
    "URL": [
        _c("Multiple Slashes", "URL_MULTIPLE_SLASHES"),
        _c("Contains A Space", "URL_CONTAINS_SPACE"),
        _p(
            "Broken Bookmark",
            "measured on retained native scans only: SF exports and legacy imports keep no "
            "destination DOM inventory, and scroll-to-text directives are named skips, not "
            "element targets",
            "BROKEN_BOOKMARK",
        ),
        _c("Non ASCII Characters", "URL_NON_ASCII"),
        _c("Uppercase", "URL_UPPERCASE"),
        _c("Repetitive Path", "URL_REPETITIVE_PATH"),
        _p(
            "Internal Search",
            "search URLs are caught only when they carry parameters; a path-based search route "
            "is not recognised",
            "URL_HAS_PARAMS",
        ),
        _c("Parameters", "URL_HAS_PARAMS"),
        _c("GA Tracking Parameters", "URL_TRACKING_PARAMS"),
        _c("Underscores", "URL_UNDERSCORES"),
        _c("Over 115 Characters", "URL_TOO_LONG"),
    ],
    "Page Titles": [
        _c("Missing", "TITLE_MISSING"),
        _c("Multiple", "TITLE_MULTIPLE"),
        _c("Outside <head>", "TITLE_OUTSIDE_HEAD"),
        _c("Duplicate", "TITLE_DUPLICATE"),
        _c("Over 60 Characters", "TITLE_TOO_LONG"),
        _c("Below 30 Characters", "TITLE_TOO_SHORT"),
        _p(
            "Over 561 Pixels",
            "pixel width needs font metrics; the column is carried from an SF export when one "
            "supplies it, and is never computed here",
        ),
        _p("Below 200 Pixels", "same: pixel width is read, never computed"),
        _c("Same as H1", "TITLE_EQUALS_H1"),
    ],
    "Meta Description": [
        _c(
            "Multiple",
            "DESC_MULTIPLE",
            note="the first occurrence stays the authoritative value every existing length "
            "and duplication check reads; this check only counts how many live occurrences "
            "there are",
        ),
        _c("Outside <head>", "DESC_OUTSIDE_HEAD"),
        _c("Missing", "DESC_MISSING"),
        _c("Duplicate", "DESC_DUPLICATE"),
        _c("Over 155 Characters", "DESC_TOO_LONG"),
        _c("Below 70 Characters", "DESC_TOO_SHORT"),
        _p("Over 985 Pixels", "pixel width is read from an export, never computed"),
        _p("Below 400 Pixels", "pixel width is read from an export, never computed"),
    ],
    "H1": [
        _c("Missing", "H1_MISSING"),
        _c("Multiple", "H1_MULTIPLE"),
        _c(
            "Alt Text in h1",
            "H1_ALT_TEXT_ONLY",
            note="fires only when the H1 has no text of its own; a logo image beside real "
            "heading text is normal and is never flagged",
        ),
        _t(
            "Non-sequential",
            "parse",
            note="the full H1-H6 order is the heading_outline field of parse, not a crawl-registry check",
        ),
        _c("Duplicate", "H1_DUPLICATE"),
        _c("Over 70 Characters", "H1_TOO_LONG"),
    ],
    "H2": [
        _c("Missing", "H2_MISSING"),
        _p(
            "Multiple",
            "multiple H2s are normal, and the issue that asked for this row supplied no "
            "defensible count past which they stop being normal; we record them without "
            "judging the count",
        ),
        _t("Non-sequential", "parse"),
        _c("Duplicate", "H2_DUPLICATE"),
        _c("Over 70 Characters", "H2_TOO_LONG"),
    ],
    "Content": [
        _c("Exact Duplicates", "DUPLICATE_BY_HASH"),
        _c("Spelling Errors", "SPELLING_ERRORS", note="from an SF export column; not computed"),
        _c("Grammar Errors", "GRAMMAR_ERRORS", note="from an SF export column; not computed"),
        _t("Soft 404 Pages", "soft404-check"),
        _c(
            "Lorem Ipsum Placeholder",
            "LOREM_IPSUM_PLACEHOLDER",
            note="matched as the full multi-word passage within the resolved content area, "
            "never a substring of the whole document, so a page merely mentioning it once "
            "outside the content area is not flagged",
        ),
        _p(
            "Near Duplicates",
            'an SF export\'s native "No. Near Duplicates" column is read directly and answers '
            "this fully; the SimHash-based fallback that answers it without one needs HTML "
            "stored to disk (input.html_store_dir), which a native `crawl-site` run never "
            "writes, so that run always skips this one by name instead",
            "NEAR_DUPLICATE",
        ),
        _g(
            "Semantically Similar",
            "needs an embedding adapter; the semantic-similarity command and MCP tool take caller-supplied vectors or a local adapter, but no adapter ships and the SF run does not call it yet, so the row stays open. simhash finds near-duplicates by shingles, not by meaning",
        ),
        _g("Low Relevance Content", "needs a query or a topic model to be relevant to"),
        _c("Low Content Pages", "THIN_CONTENT", "LOW_TEXT_RATIO"),
        _c("Readability Difficult", "READABILITY_DIFFICULT", "LONG_SENTENCES"),
        _c("Readability Very Difficult", "READABILITY_DIFFICULT"),
    ],
    "Images": [
        _c("Missing Alt Text", "IMG_MISSING_ALT"),
        _c(
            "Missing Alt Attribute",
            "IMG_MISSING_ALT_ATTRIBUTE",
            note="a native crawl's own per-image inventory distinguishes the attribute being "
            'absent from a decorative alt="" -- only the former fires; IMG_MISSING_ALT (an '
            "SF export column) still reports the two together, since that export's own shape "
            "has not changed",
        ),
        _t(
            "Background Images",
            "parse",
            note="CSS url() sources are extracted by the parser's url_sources option, which is "
            "how four images invisible to the HTML were found on a live site",
        ),
        _c("Over 100 kb", "IMG_OVER_KB"),
        _c("Alt Text Over 100 Characters", "IMG_ALT_TOO_LONG"),
        _p(
            "Incorrectly Sized Images",
            "native retained render layout compares intrinsic and displayed width at the recorded "
            "DPR; raw-only scans skip, and the existing 10 KiB oversized-image floor awaits owner review",
            "IMG_INTRINSIC_OVERSIZED",
            "IMG_UPSCALED",
        ),
        _c("Missing Size Attributes", "IMG_MISSING_DIMENSIONS"),
    ],
    "Canonicals": [
        _c("Multiple Conflicting", "CANONICAL_MULTIPLE"),
        _c("Non-Indexable Canonical", "CANONICAL_NON_INDEXABLE"),
        _g(
            "Invalid Attribute In Annotation",
            "a matched canonical <link> already has a well-formed 'canonical' token by "
            "construction (BeautifulSoup only yields whitespace-free tokens, and the match "
            "itself requires one); every other token a real document could add is either a "
            "known link-relation keyword or an unrecognised-but-well-formed one, which the "
            "issue's own acceptance criteria say must stay unknown, not invalid -- leaving no "
            "reachable case that is actually malformed without a raw-attribute-text signal "
            "this parser does not keep",
        ),
        _c("Contains Fragment URL", "CANONICAL_FRAGMENT"),
        _c("Outside <head>", "CANONICAL_OUTSIDE_HEAD"),
        _c("Canonicalised", "CANONICALISED"),
        _c("Missing", "CANONICAL_MISSING"),
        _c("Unlinked", "UNLINKED_CANONICAL"),
        _c("Multiple", "CANONICAL_MULTIPLE"),
        _c("Canonical Is Relative", "CANONICAL_RELATIVE"),
    ],
    "Pagination": [
        _c("Pagination URL Not In Anchor Tag", "PAGINATION_URL_NOT_IN_ANCHOR"),
        _c("Non-200 Pagination URLs", "PAGINATION_NONINDEXABLE"),
        _c("Unlinked Pagination URLs", "UNLINKED_PAGINATION_SERIES"),
        _c("Multiple Pagination URLs", "PAGINATION_MULTIPLE"),
        _c("Pagination Loop", "PAGINATION_LOOP"),
        _p(
            "Sequence Error",
            "a run that increments by one, or a stride that one step covers most of, is "
            "judged; an irregular offset scheme, or one whose URLs do not state a page number, "
            "is left unevaluated rather than reported against a numbering we would have had to "
            "invent -- and is then named, with the reason true of that series, among the run's "
            "skipped checks",
            "PAGINATION_SEQUENCE_ERROR",
        ),
        _c("Non-Indexable", "PAGINATION_NONINDEXABLE"),
    ],
    "Directives": [
        _c("Outside <head>", "DIRECTIVES_OUTSIDE_HEAD"),
        _c("NoImageIndex", "NOIMAGEINDEX"),
        _c("Noindex", "NOINDEX"),
        _c("Nofollow", "NOFOLLOW_PAGE"),
        _c(
            "None",
            "NOINDEX",
            "NOFOLLOW_PAGE",
            note="'none' is expanded to noindex+nofollow when directives are parsed",
        ),
        _c("Unavailable_After", "UNAVAILABLE_AFTER"),
        _c("NoSnippet", "NOSNIPPET"),
        _c("NoODP", "NOODP"),
        _c("NoYDIR", "NOYDIR"),
        _c("NoTranslate", "NOTRANSLATE"),
    ],
    "Hreflang": [
        _c("Non-200 Hreflang URLs", "HREFLANG_BROKEN_TARGET"),
        _c("Missing Return Links", "HREFLANG_MISSING_RETURN_LINK"),
        _c(
            "Inconsistent Language & Region Confirmation Links",
            "HREFLANG_INCONSISTENT_CONFIRMATION",
        ),
        _c("Non-Canonical Return Links", "HREFLANG_NOT_CANONICAL"),
        _c("Noindex Returns Links", "HREFLANG_NOINDEX_TARGET"),
        _c("Incorrect Language & Region Codes", "HREFLANG_INVALID_CODE"),
        _c("Multiple Entries", "HREFLANG_MULTIPLE_ENTRIES"),
        _c("Not Using Canonical", "HREFLANG_NOT_CANONICAL"),
        _c("Outside <head>", "HREFLANG_OUTSIDE_HEAD"),
        _c("Unlinked Hreflang URLs", "HREFLANG_UNLINKED_TARGET"),
        _c("Missing Self Reference", "HREFLANG_MISSING_SELF_REFERENCE"),
        _c("Missing X-Default", "HREFLANG_MISSING_XDEFAULT"),
    ],
    "JavaScript": [
        _t("Noindex Only in Original HTML", "render-check"),
        _t("Nofollow Only in Original HTML", "render-check"),
        _t("Canonical Mismatch", "render-check"),
        _c("Uses Old AJAX Crawling Scheme URLs", "AJAX_CRAWLING_SCHEME_URL"),
        _c(
            "Uses Old AJAX Crawling Scheme Meta Fragment Tag",
            "AJAX_CRAWLING_SCHEME_META_FRAGMENT",
        ),
        _c("Pages with Blocked Resources", "ROBOTS_BLOCKS_RESOURCES"),
        _t("Contains JavaScript Links", "render-check"),
        _t("Contains JavaScript Content", "render-check"),
        _t("Page Title Only in Rendered HTML", "render-check"),
        _t("Page Title Updated by JavaScript", "render-check"),
        _t("Meta Description Only in Rendered HTML", "render-check"),
        _t("Meta Description Updated by JavaScript", "render-check"),
        _t("H1 Only in Rendered HTML", "render-check"),
        _t("H1 Updated by JavaScript", "render-check"),
        _t("Canonical Only in Rendered HTML", "render-check"),
        _c("Pages With JavaScript Errors", "JS_CONSOLE_ERRORS"),
    ],
    "Links": [
        _c("Outlinks To Localhost", "OUTLINK_TO_LOCALHOST"),
        _p(
            "Pages With Uncrawlable Internal Outlinks",
            "attribution is possible -- the exclusion map holds the reason and the link graph "
            "holds the source -- but every reason a crawl records for an internal destination "
            "is a property of the run's own scope configuration (exclude/include patterns, "
            "segments_only, depth and query-variant budgets), not of the site, so a finding "
            "built on them would report the operator's settings back as defects. The two "
            "site-caused reasons already have their own checks from the destination side "
            "(BLOCKED_BY_ROBOTS, IMPORTANT_URL_BLOCKED_BY_ROBOTS) and a link to a "
            "robots-disallowed cart or search URL is ordinary, not a defect. This needs a "
            "reason the crawler does not record today -- a destination that is unfetchable in "
            "itself, e.g. a malformed href, which the parser discards before the crawl ever "
            "sees it",
        ),
        _c("Pages Without Internal Outlinks", "NO_INTERNAL_OUTLINKS"),
        _c("Non-Indexable Page Inlinks Only", "ONLY_NONINDEXABLE_SOURCE_INLINKS"),
        _c("Internal Nofollow Outlinks", "INTERNAL_NOFOLLOW_OUTLINKS"),
        _c("Pages With High External Outlinks", "HIGH_EXTERNAL_OUTLINKS"),
        _c("Pages With High Internal Outlinks", "HIGH_OUTLINKS"),
        _c("Follow & Nofollow Internal Inlinks To Page", "FOLLOW_AND_NOFOLLOW_INLINKS"),
        _c("Internal Nofollow Inlinks Only", "ONLY_NOFOLLOW_INLINKS"),
        _c("Pages With High Crawl Depth", "DEEP_CRAWL_DEPTH", "DEEP_DISCOVERY_PATH"),
        _p(
            "Internal Outlinks With No Anchor Text",
            "empty anchors are recorded; the generic-anchor check covers the wording, not the "
            "absence",
            "GENERIC_ANCHOR_TEXT",
        ),
        _c("Non-Descriptive Anchor Text In Internal Outlinks", "GENERIC_ANCHOR_TEXT"),
    ],
    "Structured Data": [
        _c("Validation Errors", "SCHEMA_VALIDATION_ERROR"),
        _t("Rich Result Validation Errors", "schema-check"),
        _c(
            "Parse Errors",
            "STRUCTURED_DATA_PARSE_ERROR",
            note="found and parsed block counts were already recorded per page; this reads "
            "the two together and fires when found exceeds parsed",
        ),
        _c("Missing", "STRUCTURED_DATA_MISSING"),
        _t("Validation Warnings", "schema-check"),
        _t("Rich Result Validation Warnings", "schema-check"),
    ],
    "Sitemaps": [
        _c("XML Sitemap With Over 50k URLs", "SITEMAP_TOO_MANY_URLS"),
        _c("XML Sitemap Over 50mb", "SITEMAP_TOO_LARGE"),
        _c("URLs Not In Sitemap", "URL_NOT_IN_SITEMAP"),
        _c("Orphan URLs", "SITEMAP_ORPHAN", "ORPHAN_PAGE"),
        _p(
            "Non-Indexable URLs In Sitemap",
            "reads an SF Sitemaps:* comparison export directly; a native `crawl-site` run "
            "produces no equivalent, so that run always skips this one by name instead",
            "SITEMAP_URL_NON_INDEXABLE",
        ),
        _c("URLs In Multiple Sitemaps", "SITEMAP_URL_DUPLICATED"),
    ],
    "PageSpeed": [
        _c("Document Request Latency", "SLOW_RESPONSE"),
        _c(
            "LCP Request Discovery",
            "LCP_DISCOVERY",
            note="supplied matching per-URL Lighthouse lab JSON only; no automatic trace collection",
        ),
        _c("Render Blocking Requests", "RENDER_BLOCKING"),
        _c(
            "Network Dependency Tree",
            "NETWORK_DEPENDENCY_TREE",
            note="supplied per-URL Lighthouse lab JSON",
        ),
        _c(
            "Use Efficient Cache Lifetimes",
            "ASSET_SHORT_CACHE",
            note="completed native resource headers; static heuristic, not a Lighthouse score",
        ),
        _c(
            "Layout Shift Culprits",
            "LAYOUT_SHIFT_CULPRITS",
            note="supplied per-URL Lighthouse lab JSON",
        ),
        _c("Improve Image Delivery", "IMAGE_DELIVERY", note="supplied per-URL Lighthouse lab JSON"),
        _c("Forced Reflow", "FORCED_REFLOW", note="supplied per-URL Lighthouse lab JSON"),
        _c(
            "Legacy JavaScript",
            "LEGACY_JS",
            note="markers in completed native script source; heuristic",
        ),
        _c(
            "Duplicated JavaScript",
            "DUPLICATED_JS",
            note="exact captured bytes at different script URLs on the same page",
        ),
        _p(
            "Avoid Enormous Network Payloads",
            "HEAVY_PAGE_WEIGHT uses observed navigation response bodies or an HTML-only lower bound, "
            "with explicit content-decoded units; not a total page payload across future interactions "
            "or a compressed transfer measurement. LARGE_HTML/HTML_BLOAT retain HTML-only scope",
            "HEAVY_PAGE_WEIGHT",
            "LARGE_HTML",
            "HTML_BLOAT",
        ),
        _c(
            "Minify CSS",
            "UNMINIFIED_CSS_JS",
            note="completed native source; whitespace/line-length heuristic",
        ),
        _c(
            "Minify JavaScript",
            "UNMINIFIED_CSS_JS",
            note="completed native source; whitespace/line-length heuristic",
        ),
        _c(
            "Reduce Unused CSS",
            "UNUSED_CSS_JS",
            note="bounded Chromium CDP coverage from the recorded navigation window",
        ),
        _c(
            "Reduce Unused JavaScript",
            "UNUSED_CSS_JS",
            note="bounded Chromium CDP coverage from the recorded navigation window",
        ),
        _c(
            "Reduce JavaScript Execution Time",
            "JS_EXECUTION_TIME",
            note="supplied Lighthouse bootup-time lab result",
        ),
        _o("Minimize Main-Thread Work", "needs a CPU profile"),
        _p(
            "Optimize DOM Size",
            "needs HTML stored to disk (input.html_store_dir) to measure; a Screaming Frog "
            "export configured to store HTML supplies it, but a native `crawl-site` run never "
            "writes one, so that run always skips both by name instead",
            "DOM_TOO_MANY_NODES",
            "DOM_TOO_DEEP",
        ),
        _c(
            "Font Display",
            "FONT_DISPLAY_MISSING",
            note="font-face blocks in completed native stylesheets",
        ),
    ],
    "Mobile": [
        _c("Viewport Not Set", "VIEWPORT_MISSING"),
        _o("Content Not Sized Correctly", "needs a rendered mobile layout to measure overflow"),
        _o("Illegible Font Size", "needs computed styles from a rendered page"),
        _c(
            "Contains Unsupported Plugins",
            "UNSUPPORTED_PLUGIN",
            note="<object>/<embed>/<applet> are counted, excluding an <object> whose type "
            "declares an image (an SVG or raster fallback, not plugin content)",
        ),
        _o("Target Size", "needs rendered hit-box geometry"),
        _c(
            "Mobile Alternate Link",
            "MOBILE_ALTERNATE_LINK",
            note="only a media-qualified rel=alternate with no href is flagged; the "
            "reciprocal canonical on the mobile URL is not checked",
        ),
    ],
    "Accessibility": [],  # filled below: one decision, 92 entries
    "Analytics": [
        _t(
            "Orphan URLs",
            "crawl-enrich",
            note="found from an offline analytics CSV joined to the crawl, not by sf run; "
            "a withheld result on a partial crawl; live per-URL sourcing needs #990",
        ),
        _t(
            "Bounce Rate Above 70%",
            "crawl-enrich",
            note="found from an offline per-URL bounce column (--bounce-column), not by sf run; "
            "live per-URL sourcing needs #990 and #984",
        ),
        _t(
            "No GA Data",
            "crawl-enrich",
            note="found from an offline analytics CSV joined to the crawl, not by sf run; "
            "skipped for an empty analytics file",
        ),
        _t(
            "Non-Indexable with GA Data",
            "crawl-enrich",
            note="found from an offline visits column (--visits-column), not by sf run",
        ),
    ],
    "Search Console": [
        _o("Page is Not Mobile Friendly", "needs Search Console; see #97"),
        _o("Orphan URLs", "needs Search Console"),
        _o("URL is on Google, But Has Issues", "needs Search Console"),
        _o("AMP URL is Invalid", "needs Search Console, and AMP is retired"),
        _o("Rich Result is Invalid", "needs Search Console"),
        _o("Indexable URL Not Indexed", "needs Search Console"),
        _o("User-Declared Canonical Not Selected", "needs Search Console"),
        _o("No Search Analytics Data", "needs Search Console"),
        _o("Non-Indexable with Search Analytics Data", "needs Search Console"),
        _o("URL Is Not on Google", "needs Search Console"),
    ],
    "Validation": [
        _c("Missing <head> Tag", "HEAD_MISSING"),
        _c("Multiple <head> Tags", "HEAD_MULTIPLE"),
        _c("Missing <body> Tag", "BODY_MISSING"),
        _c("Multiple <body> Tags", "BODY_MULTIPLE"),
        _c("HTML Document Over 2MB", "HTML_OVER_2MB"),
        _c(
            "Resource Over 2MB",
            "RESOURCE_OVER_2MB",
            note="captured declared-resource bytes; partial captures retain lower-bound status",
        ),
        _c("Invalid HTML Elements In <head>", "INVALID_HEAD_ELEMENT"),
        # Both collapse, after an HTML5 parser recovers from either shape of malformed
        # markup, into the same resolved fact: something other than <head> is the first
        # element under <html>. See seohead/checks/parser.py's _head_not_first for what was
        # verified directly against lxml before writing this as one check.
        _c("<body> Element Preceding <html>", "HEAD_NOT_FIRST"),
        _c("<head> Not First In <html> Element", "HEAD_NOT_FIRST"),
        _o(
            "High Carbon Rating",
            "a derived score over transfer weight; the weight itself is already reported and "
            "the rating adds a model, not a measurement",
        ),
    ],
    "AMP": [],  # filled below: six pairing checks, ten declined
}

# Two categories are a single decision rather than 108 separate ones. Listing each row would
# imply each was considered on its own merits; it was not, and pretending otherwise would make
# the map look more thorough than it is.
_ACCESSIBILITY_NOTE = (
    "A full WCAG/axe-core engine. Real, valuable, and a different product: it needs a rendered "
    "DOM, computed styles and a rule set larger than this entire registry. Out of scope until "
    "that is a deliberate decision with its own design, not a checkbox."
)
_AMP_NOTE = (
    "AMP is effectively retired: Google dropped the Top Stories carousel requirement in 2021 "
    "and the format is in maintenance. The AMP validator's document model is a separate "
    "project, so these ten rules stay declined; only the pairing rules are checked."
)


def _bulk(names: list[str], note: str) -> list[Entry]:
    return [_o(name, note) for name in names]


# The rule names are listed in full rather than counted, so the decision is auditable: a
# reader can see exactly what is being declined.
ACCESSIBILITY_RULES = [
    "Best Practice - Accesskey Attribute Value Must Be Unique",
    "Best Practice - Elements Must Not Have Tabindex Greater Than Zero",
    "Best Practice - ARIA Dialog & Alertdialog Require Accessible Name",
    "Best Practice - ARIA Treeitem Nodes Require Accessible Name",
    "Best Practice - Role=text Should Have No Focusable Descendants",
    "Best Practice - Form Elements Should Have Visible Label",
    "Best Practice - Frames Should Be Tested With axe-core",
    "Best Practice - Scope Attribute Should Be Used Correctly On Tables",
    "WCAG 2.0 A - Scrollable Region Requires Keyboard Access",
    "WCAG 2.0 A - Required ARIA Attributes Must Be Provided",
    "WCAG 2.0 A - ARIA Attribute Must Be Used As Specified For Role",
    "WCAG 2.0 A - ARIA Attributes Require Valid Values",
    "WCAG 2.0 A - ARIA Attributes Require Valid Names",
    "WCAG 2.0 A - ARIA Commands Require Accessible Name",
    "WCAG 2.0 A - ARIA Input Fields Require Accessible Name",
    "WCAG 2.0 A - ARIA Meter Nodes Require Accessible Name",
    "WCAG 2.0 A - ARIA Progressbar Nodes Require Accessible Name",
    "WCAG 2.0 A - ARIA Roles Must Be Contained By Required Parent",
    "WCAG 2.0 A - ARIA Roles Require Valid Values",
    "WCAG 2.0 A - ARIA Toggle Fields Require Accessible Name",
    "WCAG 2.0 A - ARIA Tooltip Nodes Require Accessible Name",
    "WCAG 2.0 A - Certain ARIA Roles Must Contain Specific Children",
    "WCAG 2.0 A - Aria-braille Require Non-braille Equivalent",
    "WCAG 2.0 A - Aria-hidden Elements Contains Focusable Elements",
    "WCAG 2.0 A - Aria-hidden=true Must Not Be Used In <body>",
    "WCAG 2.0 A - Elements Must Only Use Permitted ARIA Attributes",
    "WCAG 2.0 A - Elements Must Use Allowed ARIA Attributes",
    "WCAG 2.0 A - IDs Used In ARIA & Labels Must Be Unique",
    "WCAG 2.0 A - Page Requires Means To Bypass Repeated Blocks",
    "WCAG 2.0 A - Form <input> Elements Require Labels",
    "WCAG 2.0 A - Frames Require Title Attribute",
    "WCAG 2.0 A - Frames Require Unique Title Attribute",
    "WCAG 2.0 A - Frames With Focusable Content Must Not Use tabindex=-1",
    "WCAG 2.0 A - Page Must Contain <title>",
    "WCAG 2.0 A - HTML Element Lang Attribute Value Must Be Valid",
    "WCAG 2.0 A - HTML Element Requires Lang Attribute",
    "WCAG 2.0 A - Image Button Requires Alternate Text",
    "WCAG 2.0 A - Images Require Alternate Text",
    "WCAG 2.0 A - <object> Elements Require Alternate Text",
    "WCAG 2.0 A - Active <area> Elements Require Alternate Text",
    "WCAG 2.0 A - Elements Marked role=img Require Alternate Text",
    "WCAG 2.0 A - SVG Images & Graphics Require Accessible Text",
    "WCAG 2.0 A - <video> Elements Require <track> For Captions",
    "WCAG 2.0 A - <video> or <audio> Elements Must Not Auto-play",
    "WCAG 2.0 A - Buttons Require Discernible Text",
    "WCAG 2.0 A - Input Buttons Require Discernible Text",
    "WCAG 2.0 A - Links Require Discernible Text",
    "WCAG 2.0 A - Links Must Be Distinguishable",
    "WCAG 2.0 A - Select Element Requires Accessible Name",
    "WCAG 2.0 A - Summary Elements Require Discernible Text",
    "WCAG 2.0 A - Deprecated <marquee> Element Must Not Be Used",
    "WCAG 2.0 A - Interactive Controls Must Not Be Nested",
    "WCAG 2.0 A - List Items Must Be Contained In List Elements",
    "WCAG 2.0 A - Lists Must Only Contain <li> Content Elements",
    "WCAG 2.0 A - <dt> & <dd> Elements Must Be Contained by <dl>",
    "WCAG 2.0 A - <dl> Must Only Have Ordered <dt> & <dd> Groups",
    "WCAG 2.0 A - <blink> Elements Deprecated & Must Not Be Used",
    "WCAG 2.0 A - <th> Element Requires Associated Data Cells",
    "WCAG 2.0 A - Table Header Attr Must Refer To Cell In Same Table",
    "WCAG 2.0 AA - Meta Viewport Zoom & Scaling Disabled",
    "WCAG 2.0 AA - Lang Attribute Requires Valid Value",
    "WCAG 2.0 AA - Text Requires Higher Color Contrast to Background",
    "WCAG 2.0 AAA - Text Requires Higher Color Contrast Ratio",
    "WCAG 2.1 AA - Autocomplete Attribute Must Be Used Correctly",
    "WCAG 2.1 AA - Inline Text Spacing Must Be Adjustable",
    "WCAG 2.2 AA - Touch Targets Require Sufficient Size & Spacing",
    "Best Practice - Skip-link Target Should Exist & Be Focusable",
    "Best Practice - All Page Content Must Be Contained By Landmarks",
    "Best Practice - Page Requires One Main Landmark",
    "Best Practice - Page Must Not Have More Than One Banner Landmark",
    "Best Practice - Banner Landmark Must Not Be In Another Landmark",
    "Best Practice - Page Must Not Have Multiple Contentinfo Landmarks",
    "Best Practice - Page Requires At Most One Main Landmark",
    "Best Practice - Complementary Landmarks & Asides Must Be Top Level",
    "Best Practice - Contentinfo Landmark Must Be Top Level Landmark",
    "Best Practice - Main Landmark Must Not Be In Another Landmark",
    "Best Practice - Landmarks Require Unique Role Or Accessible Name",
    "Best Practice - Page Must Contain <h1>",
    "Best Practice - Heading Levels Should Only Increase By One",
    "WCAG 2.0 A - Form Field Must Not Have Multiple Label Elements",
    "WCAG 2.0 A - HTML Lang & XML Lang Value Should Match",
    "Best Practice - Ensure Elements Marked Presentational Are Ignored",
    "Best Practice - ARIA Role Should Be Appropriate For Element",
    "Best Practice - Headings Should Not Be Empty",
    "Best Practice - Meta Viewport Should Allow Zoom & Scale Up to 500%",
    "Best Practice - Alt Text Should Not Be Repeated As Text",
    "Best Practice - Table Headers Require Discernible Text",
    "Best Practice - Table With Identical Summary & Caption Text",
    "WCAG 2.0 A - Deprecated ARIA Roles Must Not Be Used",
    "WCAG 2.0 A - Server-Side Image Maps Must Not Be Used",
    "WCAG 2.0 AAA - Delayed Meta Refresh Must Not Be Used",
    "WCAG 2.0 AAA - Links With Same Accessible Name",
]

# The six pairing rules are read from the crawl (#1020); the rest need the AMP
# validator's document model and stay declined.
AMP_PAIRING_RULES = {
    "Non-200 Response": "AMP_NON_200",
    "Missing Non-AMP Return Link": "AMP_MISSING_RETURN_LINK",
    "Missing Canonical to Non-AMP": "AMP_MISSING_RETURN_LINK",
    "Non-Indexable Canonical": "AMP_NON_INDEXABLE_CANONICAL",
    "Missing Canonical": "AMP_MISSING_CANONICAL",
    "Indexable": "AMP_INDEXABLE",
}
AMP_RULES = [
    "Missing <html amp> Tag",
    "Missing/Invalid Doctype HTML Tag",
    "Missing Head Tag",
    "Missing Body Tag",
    "Missing/Invalid Meta Charset Tag",
    "Missing/Invalid Meta Viewport Tag",
    "Missing/Invalid AMP Script",
    "Missing/Invalid AMP Boilerplate",
    "Contains Disallowed HTML",
    "Other Validation Errors",
]

CATEGORIES["Accessibility"] = _bulk(ACCESSIBILITY_RULES, _ACCESSIBILITY_NOTE)
CATEGORIES["AMP"] = [
    _c(name, code, note="pairing read from the AMP target the crawl captured (#1020)")
    for name, code in AMP_PAIRING_RULES.items()
] + _bulk(AMP_RULES, _AMP_NOTE)


def entries() -> list[tuple[str, Entry]]:
    """Every catalogued issue, as (category, entry)."""
    return [(category, entry) for category, items in CATEGORIES.items() for entry in items]


def coverage_counts() -> dict[str, int]:
    counts: dict[str, int] = dict.fromkeys(STATUSES, 0)
    for _category, entry in entries():
        counts[entry.status] += 1
    return counts
