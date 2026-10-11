"""Issue registry: one row per check and the single source of truth.

Each entry carries the default severity, the data source tag, a human message
and a fix hint. The rule engine looks these up so check code stays declarative;
config can override severity/enabled per check id without touching code.
"""

from __future__ import annotations

from typing import Any

# check_id -> metadata. ``source`` uses these evidence tags:
#   SF:<tab>        native SF filter
#   SF-derived      computed from Internal:All columns
#   inlinks         from a *:Inlinks bulk export
#   heuristic       statistical / DOM heuristics
#   sitemap         robots.txt + sitemap module
CHECKS: dict[str, dict[str, Any]] = {
    # 7.A — indexing & response codes
    "BROKEN_PAGE_4XX": {
        "severity": "critical",
        "source": "SF:Response Codes:4xx",
        "message": "Page returns a 4xx response (broken page)",
        "fix": "Restore the page or redirect it with a 301 to a relevant URL; remove or update links that point to it.",
    },
    "SERVER_ERROR_5XX": {
        "severity": "critical",
        "source": "SF:Response Codes:5xx",
        "message": "Page returns a 5xx response (server error)",
        "fix": "Investigate the server or application; this error makes the page unavailable to users and crawlers.",
    },
    "NO_RESPONSE": {
        "severity": "critical",
        "source": "SF:Response Codes:No Response",
        "message": "No response (timeout, DNS, or connection failure)",
        "fix": "Check host availability, DNS resolution, connectivity, and timeout settings.",
    },
    "BLOCKED_BY_ROBOTS": {
        "severity": "warning",
        "source": "SF:Response Codes:Blocked by Robots.txt",
        "message": "URL is blocked by robots.txt",
        "fix": "Confirm that the block is intentional; pages intended for indexing should remain crawlable.",
    },
    "NON_INDEXABLE_LINKED": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Internally linked page is non-indexable",
        "fix": "Decide whether the page should be indexable; otherwise remove unnecessary internal links and account for crawl-budget impact.",
    },
    "IMPORTANT_URL_BLOCKED_BY_ROBOTS": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Live page is blocked by robots.txt despite receiving internal links",
        "fix": "robots.txt blocks crawling, not indexing, so link discovery is lost. Make the URL crawlable with a more specific Allow rule than the matching Disallow, and control indexing with canonical or noindex. A common case is pagination such as /blog?page=N blocked by Disallow: /*?.",
    },
    "ROBOTS_BLOCKS_RESOURCES": {
        "severity": "notice",
        "source": "sitemap",
        "message": "robots.txt blocks JavaScript or CSS resources required for rendering",
        "fix": "Do not block .js, .css, or _next/static resources in robots.txt; otherwise Google may render the page incompletely.",
    },
    "URL_INTERNAL_SEARCH": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "URL is a recognized internal search route",
        "fix": "Review whether internal search results should be linked or indexable; apply the intended crawl and indexing policy.",
    },
    "INTERNAL_LINK_NO_ANCHOR": {
        "severity": "warning",
        "source": "crawl:retained_links",
        "message": "Internal hyperlink has empty anchor text",
        "fix": "Add descriptive text to the internal link, or provide an accessible label for an intentional image or icon link.",
    },
    "EXTERNAL_NO_RESPONSE": {
        "severity": "warning",
        "source": "crawl:external_checks",
        "message": "An explicitly checked external destination did not answer",
        "fix": "Review the recorded timeout or connection failure and update or remove the link after confirming the destination is unavailable.",
    },
    "SITEMAP_URL_NON_CANONICAL": {
        "severity": "warning",
        "source": "crawl:sitemap_membership",
        "message": "Sitemap URL declares another canonical URL",
        "fix": "List the canonical destination in the sitemap and remove the non-canonical URL.",
    },
    "SITEMAP_URL_BLOCKED_ROBOTS": {
        "severity": "warning",
        "source": "crawl:robots_policy",
        "message": "Sitemap URL is blocked by the retained robots policy",
        "fix": "Remove the intentionally blocked URL from the sitemap or allow crawling if the URL should be indexed.",
    },
    "INTERNALLY_LINKED_ROBOTS_BLOCKED": {
        "severity": "warning",
        "source": "crawl:robots_policy",
        "message": "An internally linked URL is blocked by robots.txt",
        "fix": "Review the observed internal links and the blocking rule; allow crawling when this page should be discovered and indexed.",
    },
    # 7.B — links & DOM localization
    "BROKEN_INTERNAL_LINK": {
        "severity": "critical",
        "source": "inlinks:Client Error (4xx) Inlinks",
        "message": "Internal link points to a 4xx URL",
        "fix": "Update the link to the current URL or add an appropriate 301 redirect; if it appears in the footer or navigation, fix the shared template.",
    },
    "LINK_TO_5XX": {
        "severity": "critical",
        "source": "inlinks:Server Error (5xx) Inlinks",
        "message": "Internal link points to a 5xx URL",
        "fix": "Repair the destination page or remove the link.",
    },
    "INTERNAL_LINK_TO_REDIRECT": {
        "severity": "warning",
        "source": "inlinks:Redirection (3xx) Inlinks",
        "message": "Internal link points to a redirect (3xx)",
        "fix": "Point the link directly to the final URL to eliminate the unnecessary redirect hop.",
    },
    "BROKEN_EXTERNAL_LINK": {
        "severity": "warning",
        "source": "inlinks:Client Error (4xx) Inlinks",
        "message": "External link points to a 4xx or 5xx URL",
        "fix": "Update or remove the broken external link, while accounting for sites that intentionally return 403 responses to crawlers.",
    },
    "EXTERNAL_LINK_TO_REDIRECT": {
        "severity": "notice",
        "source": "inlinks:Redirection (3xx) Inlinks",
        "message": "External link points to a redirect (3xx)",
        "fix": "This is often acceptable for external sites; optionally update the link to point directly to the final URL.",
    },
    "REDIRECT_CHAIN": {
        "severity": "warning",
        "source": "SF:report Redirect Chains",
        "message": "Redirect chain contains two or more hops",
        "fix": "Replace the chain with a single 301 redirect to the final URL.",
    },
    "REDIRECT_LOOP": {
        "severity": "critical",
        "source": "SF:report Redirect Chains",
        "message": "Redirect loop detected",
        "fix": "Break the redirect cycle so every redirect path terminates at a valid destination.",
    },
    "BAD_REDIRECT_TYPE": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Temporary redirect (302, 303 or 307) used where a permanent redirect is expected",
        "fix": "Use a 301 redirect when the move is permanent.",
    },
    # 7.C — title & meta description
    "TITLE_MISSING": {
        "severity": "critical",
        "source": "SF-derived",
        "message": "Title element is missing",
        "fix": "Add a unique, descriptive title element.",
    },
    "TITLE_DUPLICATE": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Duplicate title element",
        "fix": "Give each page a unique title element.",
    },
    "TITLE_MULTIPLE": {
        "severity": "warning",
        "source": "SF:Page Titles:Multiple",
        "message": "Multiple <title> elements",
        "fix": "Keep exactly one <title> element in the document head.",
    },
    "TITLE_TOO_LONG": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Title exceeds the configured length threshold",
        "fix": "Shorten the title to fit the configured character or pixel-width limit.",
    },
    "TITLE_TOO_SHORT": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Title falls below the configured length threshold",
        "fix": "Expand the title to an informative length without padding it with boilerplate.",
    },
    "TITLE_EQUALS_H1": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Title is identical to the H1",
        "fix": "Differentiate the title and H1 by purpose, wording, or keyword emphasis.",
    },
    "DESC_MISSING": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Meta description is missing",
        "fix": "Add a useful meta description, typically up to about 160 characters.",
    },
    "DESC_MULTIPLE": {
        "severity": "warning",
        "source": "crawl:meta_description_count",
        "message": 'More than one <meta name="description"> element is present',
        "fix": "Keep exactly one meta description element; a search engine reads only the "
        "first, so the rest are dead weight that hides which value is actually live.",
    },
    "DESC_DUPLICATE": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Duplicate meta description",
        "fix": "Write a unique meta description for each page.",
    },
    "DESC_TOO_LONG": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Meta description exceeds the configured length threshold",
        "fix": "Shorten the description while preserving its primary value proposition.",
    },
    "DESC_TOO_SHORT": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Meta description falls below the configured length threshold",
        "fix": "Expand the description with specific, useful page information.",
    },
    # 7.D — headings (incl. multiple H1)
    "H1_MISSING": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "H1 heading is missing",
        "fix": "Add one H1 that clearly states the page topic.",
    },
    "H1_MULTIPLE": {
        "severity": "warning",
        "source": "SF:H1:Multiple",
        "message": "Multiple H1 headings on the page",
        "fix": "Keep one primary H1 and demote the remaining headings to H2 or H3 as appropriate.",
    },
    "H1_DUPLICATE": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "H1 is duplicated across multiple URLs",
        "fix": "Use a unique, page-specific H1 on each URL.",
    },
    "H1_TOO_LONG": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "H1 exceeds the configured length threshold",
        "fix": "Shorten the H1 while retaining the page's main topic.",
    },
    "H1_ALT_TEXT_ONLY": {
        "severity": "notice",
        "source": "crawl:h1_alt_text",
        "message": "The H1 has no text of its own; its only content is an image's alt attribute",
        "fix": "Add real, visible text to the H1 -- alt text describes an image to assistive "
        "technology, it is not a substitute for a heading a search engine reads as text.",
    },
    "H2_MISSING": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Page has an H1 but no H2 headings",
        "fix": "Add meaningful H2 subheadings where needed to structure the content.",
    },
    "H2_DUPLICATE": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "H2 is duplicated across multiple URLs",
        "fix": "Use a unique, page-specific H2 on each URL, or accept it for a shared "
        "boilerplate subheading that is genuinely meant to repeat.",
    },
    "H2_TOO_LONG": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "H2 exceeds the configured length threshold",
        "fix": "Shorten the H2 while retaining what it introduces.",
    },
    "HEADING_BEFORE_H1": {
        "severity": "notice",
        "source": "crawl:heading_outline",
        "message": "One or more headings appear before the page's first H1 in DOM order",
        "fix": "Move the H1 above the headings that precede it, or demote those headings to "
        "plain text or a non-heading element. A parser reading the outline meets them "
        "before it meets the page's subject, and a snippet built from early page text "
        "quotes them instead of the page.",
    },
    "HEADING_IN_PAGE_CHROME": {
        "severity": "notice",
        "source": "crawl:heading_outline",
        "message": "A heading sits in the page chrome (header, nav, sidebar or footer) rather "
        "than in the content",
        "fix": "Mark template furniture up as what it is -- a menu label or a widget title is "
        "not a heading of this document, and repeating it on every page pushes the "
        "page's own headings down its outline.",
    },
    "HEADING_SKIP": {
        "severity": "notice",
        "source": "crawl:heading_outline",
        "message": "Content headings skip one or more levels in DOM order",
        "fix": "Use the next heading level after the preceding content heading; for example, "
        "change an H4 that follows an H2 to H3 unless it begins a new top-level section.",
    },
    # 7.E — canonical & directives
    "CANONICAL_MISSING": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Indexable page has no canonical URL",
        "fix": 'Add a valid <link rel="canonical"> element.',
    },
    "CANONICALISED": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Canonical points to a different URL",
        "fix": "Confirm that cross-canonicalization is intentional and that the target is the preferred version.",
    },
    "PAGINATION_CANONICAL_POLICY": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Paginated URL canonical does not match the configured project policy",
        "fix": "Review the canonical against the configured pagination policy; the expected target is included in the evidence.",
    },
    "FILTER_CANONICAL_POLICY": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Filtered URL canonical does not match the configured project policy",
        "fix": "Review the canonical against the configured filter policy; the expected target is included in the evidence.",
    },
    "CANONICAL_NON_INDEXABLE": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Canonical points to a non-indexable URL",
        "fix": "Point the canonical to an indexable preferred version.",
    },
    "NOINDEX": {
        "severity": "notice",
        "source": "SF:Directives:Noindex",
        "message": "Page contains a noindex directive",
        "fix": "Confirm that exclusion from indexing is intentional.",
    },
    "NOFOLLOW_PAGE": {
        "severity": "notice",
        "source": "SF:Directives:Nofollow",
        "message": "Page-level nofollow directive is present",
        "fix": "Confirm the directive is intentional and review its effect on crawling and internal link equity.",
    },
    "META_KEYWORDS_PRESENT": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Obsolete meta keywords element is present",
        "fix": "Remove it if desired; modern search engines ignore meta keywords.",
    },
    # 7.F — content: thin & duplicates
    "THIN_CONTENT": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Thin content (low word count)",
        "fix": "Add substantial, useful content or exclude the page from indexing when it has no standalone search value.",
    },
    "CONTENT_IN_IFRAME": {
        "severity": "warning",
        "source": "crawl:content_frames",
        "message": "The page's content sits inside an iframe and is not attributed to this URL",
        "fix": "Serve the framed copy in the page's own HTML. A search engine attributes an iframe's text to the framed document, not to the page that frames it, so the copy exists and earns this URL nothing.",
    },
    "LOW_TEXT_RATIO": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Low text-to-HTML ratio",
        "fix": "Increase the proportion of meaningful visible content or reduce unnecessary markup.",
    },
    "DUPLICATE_BY_HASH": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Exact duplicate content (identical hash)",
        "fix": "Consolidate duplicates with canonicalization or rewrite them to serve distinct search intent.",
    },
    "NEAR_DUPLICATE": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Near-duplicate content",
        "fix": "Differentiate the pages with substantive content or consolidate them into one canonical page.",
    },
    "SEMANTIC_SIMILARITY_CANDIDATE": {
        "severity": "notice",
        "source": "heuristic:explicit_embedding_adapter",
        "message": "Pages have adapter-measured topical similarity; review candidate",
        "fix": "Review intent and useful internal linking. Semantic similarity alone does not prove duplicate content or cannibalization.",
    },
    "TITLE_LOW_SEMANTIC_RELEVANCE": {
        "severity": "notice",
        "source": "heuristic:explicit_embedding_adapter",
        "message": "Title has low adapter-measured similarity to the page content; review candidate",
        "fix": "Review whether the title describes the page's actual content and intent; the model score alone is not proof of irrelevance.",
    },
    "H1_LOW_SEMANTIC_RELEVANCE": {
        "severity": "notice",
        "source": "heuristic:explicit_embedding_adapter",
        "message": "H1 has low adapter-measured similarity to the page content; review candidate",
        "fix": "Review whether the heading describes the page's actual content and intent; the model score alone is not proof of irrelevance.",
    },
    "LOREM_IPSUM_PLACEHOLDER": {
        "severity": "warning",
        "source": "crawl:lorem_ipsum_count",
        "message": "The Lorem Ipsum placeholder passage appears in the page's own content area",
        "fix": "Replace the placeholder passage with real, reviewed copy before the page goes "
        "live or stays indexed.",
    },
    # 7.G — images
    "IMG_MISSING_ALT": {
        "severity": "warning",
        "source": "SF+native:retained-image-markup",
        "message": "Image is missing alt text",
        "fix": "Add concise, descriptive alt text when the image conveys content; use an empty alt attribute for decorative images.",
    },
    "IMG_MISSING_ALT_ATTRIBUTE": {
        "severity": "warning",
        "source": "crawl:images",
        "message": 'An <img> has no alt attribute at all (not even alt="")',
        "fix": 'Declare descriptive alt text for informative images, or alt="" for decoration. '
        "An ARIA name can label an image for assistive technology, but does not supply the missing alt attribute.",
    },
    "IMG_ALT_TOO_LONG": {
        "severity": "notice",
        "source": "crawl:images",
        "message": "An image's alt text exceeds the configured length threshold",
        "fix": "Shorten the alt text to a concise description; screen readers read the whole "
        "string aloud, and a search engine treats an overlong alt as a weaker signal.",
    },
    # 7.H — schema, hreflang, viewport
    "SCHEMA_VALIDATION_ERROR": {
        "severity": "warning",
        "source": "SF:Structured Data:Validation Errors",
        "message": "Structured data validation errors",
        "fix": "Correct invalid JSON-LD or Microdata markup and retest it against the applicable vocabulary and rich-result requirements.",
    },
    "STRUCTURED_DATA_PARSE_ERROR": {
        "severity": "warning",
        "source": "crawl:jsonld_blocks",
        "message": "A JSON-LD block is present but did not parse as valid JSON",
        "fix": "Fix the malformed JSON-LD block (a stray comma, an unescaped quote, or an "
        "unclosed brace commonly voids the whole block); a search engine ignores structured "
        "data it cannot parse the same as if none were present.",
    },
    "HREFLANG_ERROR": {
        "severity": "warning",
        "source": "SF:Hreflang",
        "message": "Hreflang implementation error",
        "fix": "Ensure hreflang annotations are reciprocal and reference canonical URLs.",
    },
    # 7.I — URL hygiene & performance
    "URL_TOO_LONG": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "URL exceeds the configured length threshold",
        "fix": "Shorten the URL while preserving a stable, descriptive path.",
    },
    "URL_HAS_PARAMS": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Parameterized URL has no canonical",
        "fix": "Point the canonical to the preferred parameter-free URL when the parameters do not create unique indexable content.",
    },
    "URL_HAS_PARAMETERS": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Internal URL contains query parameters",
        "fix": "Review whether the parameters create unique indexable content; keep internal links and sitemaps on the parameter-free URL where they do not.",
    },
    "URL_NON_ASCII": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "URL contains non-ASCII characters",
        "fix": "Consider a consistent ASCII transliteration for human-readable URLs where appropriate.",
    },
    "URL_UPPERCASE": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "URL path contains uppercase characters",
        "fix": "Normalize the path to lowercase and add a 301 redirect from the uppercase variant.",
    },
    "DEEP_CRAWL_DEPTH": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Page has excessive crawl depth",
        "fix": "Use relevant internal links to make the page reachable in fewer clicks from the home page or an authoritative hub.",
    },
    "ORPHAN_PAGE": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Orphan page has no internal inlinks",
        "fix": "Add relevant internal links so users and crawlers can discover the page.",
    },
    "SLOW_RESPONSE": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Slow server response",
        "fix": "Improve TTFB by profiling the application and origin, then optimizing caching and infrastructure.",
    },
    "LARGE_HTML": {
        "severity": "warning",
        "source": "SF-derived+heuristic",
        "message": "HTML document is large in absolute terms or relative to the site",
        "fix": "Reduce HTML size by removing unnecessary markup, extracting inline styles or scripts, and avoiding embedded base64 assets.",
    },
    "HTML_OVER_2MB": {
        "severity": "warning",
        "source": "SF-derived+heuristic",
        "message": "HTML document is larger than 2 MB",
        "fix": "Cut the document below 2 MB: remove inline data URIs and base64 assets, move inline CSS and scripts into cached files, and trim repeated markup.",
    },
    # 7.J — security
    "HTTP_URL": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "URL uses HTTP instead of HTTPS",
        "fix": "Serve the URL over HTTPS and redirect the HTTP version with a 301.",
    },
    # 7.K — sitemap & robots
    "SITEMAP_NOT_IN_ROBOTS": {
        "severity": "notice",
        "source": "sitemap",
        "message": "robots.txt does not declare a Sitemap directive",
        "fix": "Add a Sitemap directive to robots.txt with the absolute sitemap URL.",
    },
    "SITEMAP_URL_3XX": {
        "severity": "warning",
        "source": "sitemap",
        "message": "Sitemap URL returns a 3xx response",
        "fix": "List the final 200-status URL in the sitemap instead of a redirecting URL.",
    },
    "SITEMAP_URL_4XX_5XX": {
        "severity": "critical",
        "source": "sitemap",
        "message": "Sitemap URL returns a 4xx or 5xx response",
        "fix": "Remove broken URLs from the sitemap or restore them before listing them again.",
    },
    "SITEMAP_URL_NON_INDEXABLE": {
        "severity": "warning",
        "source": "sitemap",
        "message": "Sitemap contains a non-indexable URL",
        "fix": "Keep only canonical, indexable URLs in the sitemap.",
    },
    "SITEMAP_ORPHAN": {
        "severity": "warning",
        "source": "sitemap",
        "message": "Sitemap URL has no internal inlinks",
        "fix": "Add relevant internal links to the page or remove it from the sitemap if it should not be discoverable.",
    },
    "URL_NOT_IN_SITEMAP": {
        "severity": "notice",
        "source": "sitemap",
        "message": "Indexable page is missing from the sitemap",
        "fix": "Add the canonical page URL to the appropriate sitemap.",
    },
    "SITEMAP_STALE_LASTMOD": {
        "severity": "notice",
        "source": "sitemap",
        "message": "Sitemap contains stale or boilerplate lastmod values",
        "fix": "Generate each lastmod value from the page's actual meaningful modification date.",
    },
    "SITEMAP_TOO_MANY_URLS": {
        "severity": "warning",
        "source": "sitemap",
        "message": "Sitemap declares more URLs than the protocol allows",
        "fix": (
            "Split the sitemap into files of at most 50,000 URLs each and list them in a "
            "sitemap index. Over the limit the file is invalid and a search engine may read "
            "part of it and discard the rest without reporting anything."
        ),
    },
    "SITEMAP_TOO_LARGE": {
        "severity": "warning",
        "source": "sitemap",
        "message": "Sitemap exceeds the protocol's uncompressed size limit",
        "fix": (
            "Split the sitemap so each file stays under 50 MB uncompressed. The limit is "
            "about the document a search engine parses, so compressing it does not help."
        ),
    },
    "SITEMAP_URL_DUPLICATED": {
        "severity": "notice",
        "source": "sitemap",
        "message": "URL is declared in more than one sitemap",
        "fix": (
            "Declare each URL in exactly one sitemap. A URL in two files is usually a "
            "generator that ran twice, and it distorts every count taken from the declared set."
        ),
    },
    "SITEMAP_DESYNC": {
        "severity": "warning",
        "source": "sitemap",
        "message": "Sitemap and crawl URL sets are out of sync",
        "fix": "Synchronize the sitemap with the site's actual set of canonical, indexable pages.",
    },
    "SITEMAP_FETCH_INCOMPLETE": {
        "severity": "notice",
        "source": "sitemap",
        "message": "Some child sitemaps could not be fetched or parsed",
        "fix": (
            "Check that every child sitemap is reachable, retry in case the service was "
            "temporarily slow, and validate the XML -- a 200 response with malformed markup "
            "(e.g. an unescaped '&' in a URL) fails here the same way a network error does."
        ),
    },
    # 8.x — heuristics beyond SF
    "HTML_BLOAT": {
        "severity": "notice",
        "source": "heuristic",
        "message": "HTML bloat: high document size relative to text content",
        "fix": "Reduce bytes per word by extracting styles and scripts, removing embedded base64 assets, and simplifying markup.",
    },
    "DOM_TOO_DEEP": {
        "severity": "notice",
        "source": "heuristic",
        "message": "DOM nesting is too deep",
        "fix": "Simplify the layout hierarchy and remove unnecessary wrapper elements.",
    },
    "DOM_TOO_MANY_NODES": {
        "severity": "notice",
        "source": "heuristic",
        "message": "DOM contains too many nodes",
        "fix": "Reduce the number of page elements and avoid rendering unnecessary or duplicated components.",
    },
    "TITLE_TEMPLATED": {
        "severity": "notice",
        "source": "heuristic",
        "message": "Templated titles share a common prefix or suffix across most pages",
        "fix": "Make the page-specific portion of each title distinctive; a shared brand suffix is acceptable, but duplicated core title text is not.",
    },
    # --- extension: URL hygiene ---
    "URL_UNDERSCORES": {
        "severity": "notice",
        "source": "SF:URL:Underscores",
        "message": "URL contains underscores",
        "fix": "Use hyphens instead of underscores in URL path segments.",
    },
    "URL_MULTIPLE_SLASHES": {
        "severity": "notice",
        "source": "SF:URL:Multiple Slashes",
        "message": "URL path contains repeated slashes",
        "fix": "Remove duplicate slashes and 301-redirect the malformed variant to the canonical path.",
    },
    "URL_CONTAINS_SPACE": {
        "severity": "warning",
        "source": "SF:URL:Contains Space",
        "message": "URL contains a space",
        "fix": "Encode literal spaces in links; do not mechanically remove percent-encoded values "
        "such as q=red%20shoes. Change a canonical path only after verifying equivalent content "
        "and planning redirects or canonicals.",
    },
    "URL_REPETITIVE_PATH": {
        "severity": "notice",
        "source": "SF:URL:Repetitive Path",
        "message": "URL path contains a repeated segment",
        "fix": "Simplify the URL structure so path segments are not duplicated.",
    },
    "URL_TRACKING_PARAMS": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Indexable URL contains a tracking parameter such as utm_, gclid, or fbclid",
        "fix": "Remove tracking parameters from public links; for parameterized URLs that still receive traffic, add a self-referencing canonical or manage crawling through robots.txt and Search Console as appropriate.",
    },
    # --- extension: content and readability ---
    "READABILITY_DIFFICULT": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Text is difficult to read (low Flesch score)",
        "fix": "Use clearer wording and shorter sentences while preserving technical accuracy.",
    },
    "LONG_SENTENCES": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Average sentence length is too high",
        "fix": "Break long sentences into shorter, focused statements.",
    },
    "SPELLING_ERRORS": {
        "severity": "notice",
        "source": "SF:Content:Spelling Errors",
        "message": "Spelling errors detected",
        "fix": "Review and correct spelling errors, accounting for valid product names and specialist terminology.",
    },
    "GRAMMAR_ERRORS": {
        "severity": "notice",
        "source": "SF:Content:Grammar Errors",
        "message": "Grammar errors detected",
        "fix": "Review and correct the flagged grammar issues in their full sentence context.",
    },
    # --- extension: robots directives ---
    "NOARCHIVE": {
        "severity": "notice",
        "source": "SF:Directives:NoArchive",
        "message": "Page contains a noarchive directive",
        "fix": "Confirm that preventing cached search-result copies is intentional.",
    },
    "NOSNIPPET": {
        "severity": "notice",
        "source": "SF:Directives:NoSnippet",
        "message": "Page contains a nosnippet directive",
        "fix": "Confirm that suppressing the page's search-result snippet is intentional.",
    },
    "NOIMAGEINDEX": {
        "severity": "notice",
        "source": "SF:Directives:NoImageIndex",
        "message": "Page contains a noimageindex directive",
        "fix": "Confirm that preventing images on this page from being indexed is intentional.",
    },
    "META_REFRESH_REDIRECT": {
        "severity": "warning",
        "source": "SF:Directives:Refresh",
        "message": "Redirect is implemented with meta refresh",
        "fix": "Replace meta refresh with a server-side 301 redirect when the move is permanent.",
    },
    "HTTP_REFRESH_REDIRECT": {
        "severity": "warning",
        "source": "crawl:http_refresh",
        "message": "Redirect is implemented with an HTTP Refresh response header",
        "fix": "Replace it with a server-side 301/302 redirect (Location header); a search "
        "engine treats Refresh the same as a meta refresh -- an unreliable, delayed signal "
        "compared to a real HTTP redirect status code.",
    },
    "NOTRANSLATE": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Page contains a notranslate directive",
        "fix": "Confirm that opting out of translation-related Google Search features is intentional.",
    },
    "NOODP": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Page contains a legacy noodp directive",
        "fix": "Remove the retired noodp directive; the Open Directory Project closed in 2017 and the directive has no effect.",
    },
    "NOYDIR": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Page contains a legacy noydir directive",
        "fix": "Remove the retired noydir directive; the Yahoo Directory closed and the directive has no effect.",
    },
    "UNAVAILABLE_AFTER": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Page carries an unavailable_after directive with a deindex date",
        "fix": "Confirm the date is intentional and in the future; once it passes, the page is removed from the index automatically.",
    },
    # --- extension: canonicals ---
    "CANONICAL_RELATIVE": {
        "severity": "notice",
        "source": "SF:Canonicals:Canonical Is Relative",
        "message": "Canonical URL is relative",
        "fix": "Use an absolute URL in the canonical element.",
    },
    "CANONICAL_MULTIPLE": {
        "severity": "warning",
        "source": "SF:Canonicals:Multiple",
        "message": "Page declares multiple canonical URLs",
        "fix": "Declare exactly one canonical URL for the page.",
    },
    "CANONICAL_FRAGMENT": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Canonical URL contains a #fragment",
        "fix": "Drop the fragment; the server never receives it, so a canonical pointing at one is meaningless.",
    },
    # --- extension: canonical graph (Mode B, built from Internal:All) ---
    "CANONICAL_INVALID_ATTRIBUTE": {
        "severity": "warning",
        "source": "crawl:head_links",
        "message": "Canonical annotation contains unsupported alternate-version attributes or malformed rel/href",
        "fix": "Remove hreflang, lang, media and type from the canonical link; declare alternates separately. Use an HTML whitespace-separated canonical relation and a non-empty HTTP(S) href. Raw attributes are included in the evidence.",
    },
    "CANONICAL_CROSS_ORIGIN": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Canonical changes the declaring URL host or protocol",
        "fix": "Confirm that cross-host or protocol consolidation is intentional and that the target is the preferred URL.",
    },
    "CANONICAL_CHAIN": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Canonical chain: the target canonicalizes to another URL (two or more steps)",
        "fix": "Point the canonical directly to the final canonical URL in one step and break any canonical loops.",
    },
    "CANONICAL_TO_REDIRECT": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Canonical points to a redirecting URL (3xx)",
        "fix": "Point the canonical to the final 200-status URL; otherwise search engines must resolve conflicting canonical signals.",
    },
    "CANONICAL_TARGET_ERROR": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Canonical points to a URL returning 4xx or 5xx",
        "fix": "Point the canonical to a fetched, successful URL; restore the target or redirect it to the intended canonical page.",
    },
    "CANONICAL_HOMEPAGE_GROUP": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Distinct indexable pages in multiple sections canonicalize to the homepage",
        "fix": "Review each listed page's canonical. Use a self-canonical for distinct indexable content, or configure an explicit pagination/filter policy when a landing canonical is intentional.",
    },
    "UNLINKED_CANONICAL": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Canonical target has no hyperlink pointing to it anywhere in the crawl",
        "fix": "Add an ordinary internal link to the canonical target, or confirm relying on the canonical alone for discovery is intentional.",
    },
    "HREFLANG_UNLINKED_TARGET": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Hreflang alternate target has no hyperlink pointing to it anywhere in the crawl",
        "fix": "Add an ordinary internal link to the hreflang alternate, or confirm relying on the hreflang annotation alone for discovery is intentional.",
    },
    "HREFLANG_BROKEN_TARGET": {
        "severity": "warning",
        "source": "inlinks:All Hreflang",
        "message": "Hreflang points to a redirecting or broken URL (3xx, 4xx, or 5xx)",
        "fix": "Update hreflang to reference the final 200-status URL; redirecting or broken targets undermine localization signals and crawling.",
    },
    "HREFLANG_NOINDEX_TARGET": {
        "severity": "warning",
        "source": "crawl:hreflang graph / SF:All Hreflang + Internal:All",
        "message": "Hreflang alternate points to an observed noindex page",
        "fix": "Use an indexable alternate URL or remove the noindex directive if that page should appear in search.",
    },
    "HREFLANG_INVALID_CODE": {
        "severity": "warning",
        "source": "inlinks:All Hreflang",
        "message": "Hreflang value is not a valid ISO 639-1 language / ISO 3166-1 region code",
        "fix": "Use a valid language code, optionally followed by a valid region (e.g. en-GB, not en-UK).",
    },
    "HREFLANG_MULTIPLE_ENTRIES": {
        "severity": "warning",
        "source": "inlinks:All Hreflang",
        "message": "The same hreflang value is declared more than once on the page",
        "fix": "Declare each language/region combination exactly once; conflicting duplicates make the annotation ambiguous.",
    },
    "HREFLANG_MISSING_SELF_REFERENCE": {
        "severity": "warning",
        "source": "inlinks:All Hreflang",
        "message": "Page declares hreflang alternates but does not reference itself",
        "fix": "Every page in an hreflang set must include a self-referencing annotation for its own URL and language.",
    },
    "HREFLANG_MISSING_XDEFAULT": {
        "severity": "notice",
        "source": "inlinks:All Hreflang",
        "message": "Hreflang set has no x-default fallback",
        "fix": "Add an x-default annotation to catch users whose language/region does not match any declared alternate.",
    },
    "HREFLANG_NOT_CANONICAL": {
        "severity": "warning",
        "source": "inlinks:All Hreflang",
        "message": "Hreflang points to a URL that is not itself the canonical version",
        "fix": "Point hreflang annotations at each target's canonical URL, not at a duplicate that canonicalizes elsewhere.",
    },
    "HREFLANG_MISSING_RETURN_LINK": {
        "severity": "warning",
        "source": "inlinks:All Hreflang",
        "message": "Another page's hreflang points here, but this page does not point back",
        "fix": "Add a reciprocal hreflang annotation back to every page that names this one.",
    },
    "HREFLANG_INCONSISTENT_CONFIRMATION": {
        "severity": "warning",
        "source": "inlinks:All Hreflang",
        "message": "This page declares a counterpart under a language and region code the "
        "counterpart does not confirm for itself",
        "fix": "Make the two declarations agree: either correct this page's hreflang value for "
        "that URL, or correct the counterpart's own self-referencing hreflang. Google reads a "
        "pair as valid only when both sides name the same code, so a mismatched pair is "
        "discarded exactly as a missing return link is.",
    },
    # --- extension: pagination ---
    "PAGINATION_NONINDEXABLE": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Pagination page is non-indexable",
        "fix": "Pagination pages should generally remain crawlable and indexable unless a deliberate alternative architecture is in place.",
    },
    "PAGINATION_LOOP": {
        "severity": "warning",
        "source": "SF-derived",
        "message": 'A rel="next" pagination series loops back on itself',
        "fix": 'Fix the rel="next"/rel="prev" values so the series terminates instead of cycling.',
    },
    "UNLINKED_PAGINATION_SERIES": {
        "severity": "warning",
        "source": "SF-derived",
        "message": 'A pagination series is reachable only by following rel="next", never by a hyperlink',
        "fix": 'Add an ordinary internal link to the first page of the series so it does not depend on rel="next" alone for discovery.',
    },
    "PAGINATION_MULTIPLE": {
        "severity": "warning",
        "source": "SF-derived",
        "message": 'The page declares more than one rel="next" (or more than one rel="prev") URL',
        "fix": 'Declare exactly one rel="next" and one rel="prev" URL per page; two different '
        "successors leave the series ambiguous, and a crawler picks one of them without "
        "telling anybody which.",
    },
    "PAGINATION_URL_NOT_IN_ANCHOR": {
        "severity": "notice",
        "source": "inlinks:All Inlinks",
        "message": 'A rel="next"/rel="prev" URL is not also linked from the same page with an anchor',
        "fix": "Link the paginated URL from the page itself with an ordinary <a href> as well. "
        'Google stopped using rel="next"/rel="prev" for indexing in 2019, so an annotation '
        "with no anchor beside it is a route only some crawlers still take.",
    },
    "PAGINATION_SEQUENCE_ERROR": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Pagination breaks an established numbered run or contradicts a reciprocal next/prev declaration",
        "fix": 'Point each rel="next" at the page that actually follows this one. A run that '
        "goes 1, 2, 3, 7 leaves pages 4 to 6 out of the chain entirely, so a crawler "
        "following it never reaches them.",
    },
    # --- extension: links ---
    "NO_INTERNAL_OUTLINKS": {
        "severity": "warning",
        "source": "SF:Links:Pages Without Internal Outlinks",
        "message": "Dead-end page has no internal outlinks",
        "fix": "Add relevant internal links to help users and crawlers continue through the site.",
    },
    "HIGH_EXTERNAL_OUTLINKS": {
        "severity": "notice",
        "source": "SF:Links:Pages With High External Outlinks",
        "message": "Page has a high number of external outlinks",
        "fix": "Review the links for editorial relevance, spam, and unnecessary dilution of page focus.",
    },
    "HIGH_OUTLINKS": {
        "severity": "notice",
        "source": "SF:Links:Pages With High Outlinks",
        "message": "Page has an excessive number of outlinks",
        "fix": "Reduce unnecessary links to preserve clear navigation and crawl focus.",
    },
    "GENERIC_ANCHOR_TEXT": {
        "severity": "notice",
        "source": "inlinks:Anchor Text",
        "message": "Non-descriptive anchor text such as 'here', 'read more', or 'click here'",
        "fix": "Replace it with meaningful anchor text that describes the destination for both search engines and screen-reader users.",
    },
    "LOW_LINK_SCORE": {
        "severity": "notice",
        "source": "inlinks:All Inlinks",
        "message": "Internal link score is far below the site median",
        "fix": "Add internal, followed links to the page from well-linked pages elsewhere on the site.",
    },
    "ONLY_NOFOLLOW_INLINKS": {
        "severity": "warning",
        "source": "inlinks:All Inlinks",
        "message": "Every internal link to this page is nofollow",
        "fix": "Add at least one ordinary, followed internal link so link equity and crawl priority reach the page.",
    },
    "ONLY_NONINDEXABLE_SOURCE_INLINKS": {
        "severity": "warning",
        "source": "inlinks:All Inlinks",
        "message": "Every internal link to this page comes from a non-indexable source",
        "fix": "Link to the page from at least one indexable page so it is reachable from the part of the site search engines actually rank.",
    },
    "DEEP_DISCOVERY_PATH": {
        "severity": "notice",
        "source": "inlinks:All Inlinks",
        "message": "The shortest hyperlink route from the start page exceeds the configured depth",
        "fix": "Add a shorter internal-linking route (e.g. from a hub or category page) so the page is reachable in fewer clicks.",
    },
    "DEEP_CLICK_DEPTH": {
        "severity": "warning",
        "source": "inlinks:All Inlinks",
        "message": "The page is more clicks from the crawl's start URL than the configured floor",
        "fix": "Link the page from a hub, category or related page nearer the start URL. A page "
        "linked only by a chain of next-post links is reachable and still effectively "
        "invisible, which no orphan count reports.",
    },
    "DUPLICATE_INTERNAL_LINK": {
        "severity": "notice",
        "source": "inlinks:All Inlinks",
        "message": "The page repeats the same link -- same destination, same anchor text -- "
        "more than once",
        "fix": "Emit the block once. A layout element rendered twice (one copy hidden by CSS or "
        "removed by JavaScript) duplicates every link it contains, which inflates the internal "
        "link graph without adding a single new route.",
    },
    "INSECURE_SUBRESOURCE": {
        "severity": "warning",
        "source": "inlinks:All Inlinks",
        "message": "An HTTPS page loads a resource (image, script, stylesheet, ...) over plain HTTP",
        "fix": "Serve every page resource over HTTPS and update its URL accordingly.",
    },
    # --- extension: technical checks ---
    "HTTP1_ONLY": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Response uses HTTP/1.x rather than HTTP/2 or newer",
        "fix": "Enable HTTP/2 or HTTP/3 on the origin server or CDN where supported.",
    },
    "AMPHTML_PRESENT": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "AMP version is declared",
        "fix": "Confirm that the AMP version is still required, current, valid, and canonically linked.",
    },
    # --- AMP pairing, read from the AMP target a desktop page declares (issue #1020) ---
    "AMP_NON_200": {
        "severity": "warning",
        "source": "crawl:amp_pairing",
        "message": "The AMP page declared by rel=amphtml does not answer 200",
        "fix": "Serve the AMP URL with 200, or remove the rel=amphtml declaration that names it.",
    },
    "AMP_MISSING_CANONICAL": {
        "severity": "warning",
        "source": "crawl:amp_pairing",
        "message": "The AMP page declares no rel=canonical",
        "fix": "Add a rel=canonical on the AMP page that points to the non-AMP version.",
    },
    "AMP_MISSING_RETURN_LINK": {
        "severity": "warning",
        "source": "crawl:amp_pairing",
        "message": "The AMP page's rel=canonical does not point back to the page that declares it",
        "fix": "Point the AMP page's rel=canonical at the non-AMP URL that declares rel=amphtml.",
    },
    "AMP_NON_INDEXABLE_CANONICAL": {
        "severity": "warning",
        "source": "crawl:amp_pairing",
        "message": "The AMP page's canonical target is not indexable",
        "fix": "Make the canonical target indexable, or correct the AMP page's rel=canonical.",
    },
    "AMP_INDEXABLE": {
        "severity": "notice",
        "source": "crawl:amp_pairing",
        "message": "The AMP page is indexable",
        "fix": "Confirm the AMP page should compete in search; if the non-AMP page is canonical, the AMP page usually should not be.",
    },
    # --- extension: static Lighthouse audits (issue #59) ---
    # Correspondence to a Lighthouse audit id + doc URL lives in
    # seohead/sf/core/lighthouse.py, not here, so it can carry the longer
    # explanation and be checked by tests/test_lighthouse_map.py against a
    # snapshot of every id Lighthouse actually defines.
    "MISSING_CHARSET": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "No character encoding declared via Content-Type or an early <meta> tag",
        "fix": "Declare charset in the Content-Type response header, or add a <meta charset> tag in the first 1024 bytes of the HTML.",
    },
    "MISSING_DOCTYPE": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Document lacks a modern <!DOCTYPE html> declaration, triggering quirks mode",
        "fix": "Add `<!DOCTYPE html>` as the very first line of the document, with no PUBLIC or SYSTEM identifier.",
    },
    "VIEWPORT_MISSING": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "No <meta name=viewport> tag with width or an initial-scale of at least 1",
        "fix": 'Add `<meta name="viewport" content="width=device-width, initial-scale=1">` to the document head.',
    },
    "MOBILE_ALTERNATE_LINK": {
        "severity": "warning",
        "source": "crawl:mobile_alternate",
        "message": "A <link rel=alternate media=...> declaration names no URL",
        "fix": "Give the mobile alternate link an `href` pointing at the mobile URL, or remove "
        'the `<link rel="alternate" media=...>` tag.',
    },
    "MOBILE_CONTENT_OVERFLOW": {
        "severity": "warning",
        "source": "native rendered mobile layout",
        "message": "Rendered content extends beyond the mobile viewport width",
        "fix": "Constrain the overflowing element or provide a mobile-width layout.",
    },
    "MOBILE_TEXT_TOO_SMALL": {
        "severity": "notice",
        "source": "native rendered mobile layout",
        "message": "Rendered text uses a computed font size below 12 CSS pixels",
        "fix": "Use a readable mobile text size and verify the page at its target viewport.",
    },
    "MOBILE_TARGET_TOO_SMALL": {
        "severity": "notice",
        "source": "native rendered mobile layout",
        "message": "Rendered interactive target is smaller than 48 CSS pixels in at least one dimension",
        "fix": "Increase the interactive target's hit area and verify spacing at the target viewport.",
    },
    "NOINDEX_ONLY_IN_RAW_HTML": {
        "severity": "warning",
        "source": "native raw/rendered element comparison",
        "message": "Noindex appears in the original HTML but not in the rendered DOM",
        "fix": "Keep indexing directives consistent in the original response and rendered document.",
    },
    "NOFOLLOW_ONLY_IN_RAW_HTML": {
        "severity": "warning",
        "source": "native raw/rendered element comparison",
        "message": "Nofollow appears in the original HTML but not in the rendered DOM",
        "fix": "Keep link-following directives consistent in the original response and rendered document.",
    },
    "CANONICAL_MISMATCH": {
        "severity": "warning",
        "source": "native raw/rendered element comparison",
        "message": "Canonical URL changes after JavaScript rendering",
        "fix": "Declare one stable canonical URL in the original response.",
    },
    "CANONICAL_ONLY_IN_RENDERED_HTML": {
        "severity": "notice",
        "source": "native raw/rendered element comparison",
        "message": "Canonical URL is present only after JavaScript rendering",
        "fix": "Declare the canonical URL in the original response.",
    },
    "PAGE_TITLE_ONLY_IN_RENDERED_HTML": {
        "severity": "notice",
        "source": "native raw/rendered element comparison",
        "message": "Page title is present only after JavaScript rendering",
        "fix": "Serve the page title in the original HTML response.",
    },
    "PAGE_TITLE_UPDATED_BY_JAVASCRIPT": {
        "severity": "notice",
        "source": "native raw/rendered element comparison",
        "message": "Page title changes after JavaScript rendering",
        "fix": "Keep the initial and rendered page title aligned.",
    },
    "META_DESCRIPTION_ONLY_IN_RENDERED_HTML": {
        "severity": "notice",
        "source": "native raw/rendered element comparison",
        "message": "Meta description is present only after JavaScript rendering",
        "fix": "Serve the meta description in the original HTML response.",
    },
    "META_DESCRIPTION_UPDATED_BY_JAVASCRIPT": {
        "severity": "notice",
        "source": "native raw/rendered element comparison",
        "message": "Meta description changes after JavaScript rendering",
        "fix": "Keep the initial and rendered meta description aligned.",
    },
    "H1_ONLY_IN_RENDERED_HTML": {
        "severity": "notice",
        "source": "native raw/rendered element comparison",
        "message": "H1 is present only after JavaScript rendering",
        "fix": "Serve the primary page heading in the original HTML response.",
    },
    "H1_UPDATED_BY_JAVASCRIPT": {
        "severity": "notice",
        "source": "native raw/rendered element comparison",
        "message": "H1 changes after JavaScript rendering",
        "fix": "Keep the initial and rendered primary page heading aligned.",
    },
    "RENDER_RESOURCE_BLOCKED": {
        "severity": "warning",
        "source": "native rendered network log",
        "message": "A rendered page resource was refused or returned an HTTP error",
        "fix": "Review the recorded request reason and make required page resources available "
        "to the renderer.",
    },
    "UNSUPPORTED_PLUGIN": {
        "severity": "warning",
        "source": "crawl:plugin_elements",
        "message": "Page contains a legacy plugin-dependent element (<object>/<embed>/<applet>)",
        "fix": "Replace the plugin-dependent element with a native equivalent (HTML5 "
        "<video>/<audio>, an <img>/<svg>, or a JavaScript-driven alternative) -- mobile "
        "browsers, and modern desktop ones, do not run plugins, so this content is simply "
        "invisible there.",
    },
    "AJAX_CRAWLING_SCHEME_URL": {
        "severity": "notice",
        "source": "crawl:ajax_scheme_outlinks",
        "message": "The deprecated AJAX crawling scheme (#! / _escaped_fragment_) is still used "
        "by this page's URL or by URLs it links to",
        "fix": "Serve the same content at ordinary URLs and link to those instead. Google "
        "deprecated the scheme in 2015 and stopped supporting it in 2018, so an _escaped_fragment_ "
        "companion URL is no longer requested by anything -- informational rather than broken, "
        "because a site may still keep it for a legacy client of its own.",
    },
    "AJAX_CRAWLING_SCHEME_META_FRAGMENT": {
        "severity": "notice",
        "source": "crawl:meta_fragment",
        "message": 'Page declares <meta name="fragment"> -- the page-wide opt-in to the '
        "deprecated AJAX crawling scheme",
        "fix": "Remove the tag once the page is served as ordinary HTML (server-rendered or "
        "crawlable client-rendered). Nothing requests the _escaped_fragment_ companion URL it "
        "advertises any more, so the declaration is inert -- informational rather than broken.",
    },
    "RENDER_BLOCKING": {
        "severity": "notice",
        "source": "crawl:render_blocking",
        "message": "A <head> script or stylesheet blocks first paint (no async/defer, no non-matching media)",
        "fix": "Add `defer` or `async` to the <script src>, move it out of <head>, or give the stylesheet a `media` value that the first render does not need.",
    },
    "NO_COMPRESSION": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "HTML response is served uncompressed above the size where gzip/br would help",
        "fix": "Enable gzip, Brotli, or deflate compression for text responses on the origin server or CDN.",
    },
    # --- extension: element position & document skeleton (issue #123) ---
    "TITLE_OUTSIDE_HEAD": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "The <title> element is outside <head> once the parser resolves the document",
        "fix": "Move whatever precedes it in <head> — usually an element the head content model does not allow — so <title> is read from <head> again.",
    },
    "DESC_OUTSIDE_HEAD": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "The meta description is outside <head> once the parser resolves the document",
        "fix": "Move whatever precedes it in <head> — usually an element the head content model does not allow — so the description is read from <head> again.",
    },
    "CANONICAL_OUTSIDE_HEAD": {
        "severity": "critical",
        "source": "SF-derived",
        "message": "The canonical link is outside <head> once the parser resolves the document",
        "fix": "Move whatever precedes it in <head> — usually an element the head content model does not allow — so the canonical is read from <head> again; Google ignores a canonical outside <head>.",
    },
    "DIRECTIVES_OUTSIDE_HEAD": {
        "severity": "critical",
        "source": "SF-derived",
        "message": "A robots-directive meta tag is outside <head> once the parser resolves the document",
        "fix": "Move whatever precedes it in <head> — usually an element the head content model does not allow — so the directive is read from <head> again; a noindex/nofollow outside <head> does not apply.",
    },
    "HREFLANG_OUTSIDE_HEAD": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "An hreflang alternate link is outside <head> once the parser resolves the document",
        "fix": "Move whatever precedes it in <head> — usually an element the head content model does not allow — so the alternate is read from <head> again.",
    },
    "HEAD_MISSING": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Document has no <head> element",
        "fix": "Add a <head> element; a browser inserts one implicitly, but every metadata tag then depends on exactly where it lands.",
    },
    "HEAD_MULTIPLE": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Document has more than one <head> element",
        "fix": "Merge into a single <head>; a browser keeps both as siblings rather than combining their contents.",
    },
    "BODY_MISSING": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Document has no <body> element",
        "fix": "Add a <body> element; a browser inserts one implicitly around the visible content.",
    },
    "BODY_MULTIPLE": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Document has more than one <body> element",
        "fix": "Merge into a single <body>; a browser keeps both as siblings rather than combining their contents.",
    },
    "INVALID_HEAD_ELEMENT": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "An element the head content model does not allow is written inside <head>",
        "fix": "Move title/base/link/meta/style/script/noscript/template content into <head> and everything else into <body>; an invalid element is what forces the parser to close <head> early.",
    },
    "HEAD_NOT_FIRST": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "<head> is not the first element under <html> once the parser resolves the document",
        "fix": "Fix the markup order so <head> opens immediately after <html>, before any <body> content.",
    },
    # --- extension: export-dependent native filters (active when the export is available) ---
    "MIXED_CONTENT": {
        "severity": "warning",
        "source": "SF:Security:Mixed Content",
        "message": "Mixed content: HTTPS page loads resources over HTTP",
        "fix": "Serve every page resource over HTTPS and update its URL accordingly.",
    },
    "MISSING_HSTS": {
        "severity": "notice",
        "source": "SF:Security:Missing HSTS Header",
        "message": "HSTS header is missing",
        "fix": "Add an appropriate Strict-Transport-Security header after confirming the entire site is HTTPS-ready.",
    },
    "STRUCTURED_DATA_MISSING": {
        "severity": "notice",
        "source": "SF:Structured Data:Missing",
        "message": "Structured data is missing",
        "fix": "Add relevant, accurate Schema.org markup that reflects visible page content.",
    },
    "OG_MISSING": {
        "severity": "notice",
        "source": "SF:Social:Open Graph",
        "message": "og:title is missing, so social previews may not render correctly",
        "fix": "Add og:title, og:image, and og:url; at minimum, provide og:title and og:image for a useful preview.",
    },
    "IMG_BROKEN": {
        "severity": "warning",
        "source": "crawl:images",
        "message": "Image URL answered with an HTTP error, so the browser shows nothing",
        "fix": "Restore the file, or update the reference in the page template or CMS to a URL that answers 200.",
    },
    "IMG_OVER_KB": {
        "severity": "warning",
        "source": "SF:Images:Over X KB",
        "message": "Image exceeds the configured file-size threshold",
        "fix": "Compress the image and consider converting it to WebP or AVIF while preserving acceptable visual quality.",
    },
    "IMG_MISSING_DIMENSIONS": {
        "severity": "notice",
        "source": "SF+native:retained-image-markup",
        "message": "Image does not declare positive width and height attributes",
        "fix": "Declare intrinsic width and height values to reserve layout space and reduce CLS.",
    },
    # 9.A — link position (native crawl only; SF exports carry their own Link
    # Position column, handled separately in sf.core.inlinks). Computed the
    # same way SITEMAP_ORPHAN and URL_NOT_IN_SITEMAP are: from evidence a
    # native crawl produces that an SF export cannot, added directly by the
    # handler layer rather than through a registered export requirement.
    "INLINK_BOILERPLATE_ONLY": {
        "severity": "warning",
        "source": "crawl:link_position",
        "message": "Page is linked only from navigation, header, sidebar, or footer, never "
        "from body content",
        "fix": "Add a contextual link to the page from relevant body copy; a page reachable "
        "only through boilerplate is not linked the way a page in the content graph is.",
    },
    # 9.A2 — link placement (native crawl only, issue #634). Same construction as
    # INLINK_BOILERPLATE_ONLY just above and the heading-outline checks: read from
    # evidence a native crawl records and a Screaming Frog export has no column for,
    # so both skip by name on an export rather than run clean on nothing.
    "LINK_INSIDE_HEADING": {
        "severity": "notice",
        "source": "crawl:link_placement",
        "message": "A heading on the page is, or contains, a link to somewhere else",
        "fix": "Let the heading describe this page and put the link in the body copy beneath "
        "it. A heading that is a link stops naming the page's own subject and starts "
        "pointing away from it.",
    },
    "IMAGE_LINK_WITHOUT_TEXT": {
        "severity": "warning",
        "source": "crawl:link_placement",
        "message": "An image link carries no anchor text and no alt text, so nothing says "
        "where it goes",
        "fix": "Give the image a descriptive alt attribute, or add anchor text beside it. "
        "Neither a reader using a screen reader nor a crawler can tell what this link "
        "points to.",
    },
    # 9.B — link security & forms (issue #125). Same construction as INLINK_BOILERPLATE_ONLY
    # just above: computed directly from a native crawl's own LinkEdge/FormEdge evidence
    # (seohead.crawl.link_findings), added by the handler layer rather than through a
    # registered export requirement, because an SF export carries neither a form inventory
    # nor a link's rel/target/raw-href.
    "UNSAFE_CROSS_ORIGIN_LINK": {
        "severity": "warning",
        "source": "crawl:link_findings",
        "message": 'A target="_blank" link declares neither rel="noopener" nor rel="noreferrer"',
        "fix": 'Add rel="noopener" (or "noreferrer") so the opened page cannot reach back '
        "into this one through window.opener.",
    },
    "PROTOCOL_RELATIVE_LINK": {
        "severity": "notice",
        "source": "crawl:link_findings",
        "message": 'Link href is written in the protocol-relative "//host/path" form',
        "fix": "Write an explicit https:// href; a protocol-relative one silently follows "
        "whatever scheme served the current page, including a plain-HTTP embed.",
    },
    "MISSING_CSP": {
        "severity": "notice",
        "source": "crawl:security_headers",
        "message": "HTML page has no Content-Security-Policy header",
        "fix": "Send a Content-Security-Policy header that names the script and style sources the page uses.",
    },
    "MISSING_X_CONTENT_TYPE_OPTIONS": {
        "severity": "notice",
        "source": "crawl:security_headers",
        "message": "HTML page has no X-Content-Type-Options header",
        "fix": "Send X-Content-Type-Options: nosniff so browsers do not sniff the response into another type.",
    },
    "MISSING_X_FRAME_OPTIONS": {
        "severity": "notice",
        "source": "crawl:security_headers",
        "message": "HTML page has neither X-Frame-Options nor a CSP frame-ancestors directive",
        "fix": "Send X-Frame-Options: SAMEORIGIN or a Content-Security-Policy frame-ancestors directive.",
    },
    "MISSING_REFERRER_POLICY": {
        "severity": "notice",
        "source": "crawl:security_headers",
        "message": "HTML page has no Referrer-Policy header",
        "fix": "Send a Referrer-Policy header, for example strict-origin-when-cross-origin.",
    },
    "DUPLICATE_SECURITY_HEADER": {
        "severity": "notice",
        "source": "crawl:security_headers",
        "message": "HTML page sends a security header more than once",
        "fix": "Send each security header once. A repeated header can be ignored or applied "
        "next to the other copy, so the policy the browser enforces may not be the one intended.",
    },
    "OUTLINK_TO_LOCALHOST": {
        "severity": "warning",
        "source": "crawl:link_findings",
        "message": "A link points at a loopback address (localhost, 127.0.0.1, ::1, ...)",
        "fix": "Replace the development/staging reference with the production URL.",
    },
    "JS_CONSOLE_ERRORS": {
        "severity": "warning",
        "source": "crawl:browser_artifacts",
        "message": "The page's browser console logged errors while it was rendered",
        "fix": "Fix the script error the console reports; an uncaught exception can stop "
        "the page's content, links or metadata from being built in the browser.",
    },
    "FOLLOW_AND_NOFOLLOW_INLINKS": {
        "severity": "notice",
        "source": "crawl:link_findings",
        "message": "The page receives both a followed and a nofollow internal link",
        "fix": "Decide deliberately whether the page should be crawl-priority or not, and "
        "make every internal link to it agree.",
    },
    "INTERNAL_NOFOLLOW_OUTLINKS": {
        "severity": "notice",
        "source": "crawl:link_findings",
        "message": "The page has internal outlinks marked nofollow",
        "fix": "Confirm each internal nofollow link is intentional; links to pages that should be crawled and ranked should be followed. rel=sponsored and rel=ugc are not visible here and stay distinct.",
    },
    "INTERNAL_LINK_SPONSORED_UGC": {
        "severity": "notice",
        "source": "crawl:link_findings",
        "message": "An internal link carries rel=sponsored or rel=ugc",
        "fix": "Remove sponsored and ugc from links to pages of this site. Keep them only on "
        "links that really lead to paid placements or user-submitted content.",
    },
    "MIXED_LINK_REL": {
        "severity": "notice",
        "source": "crawl:link_findings",
        "message": "A link combines nofollow, sponsored or ugc tokens",
        "fix": "The combination is valid. Review whether each qualifying token describes the placement; retain intentional combinations and the full rel token set.",
    },
    "OUTBOUND_SPONSORED_WITHOUT_DISCLOSURE": {
        "severity": "notice",
        "source": "crawl:link_findings",
        "message": "An outbound sponsored link has no recognized local disclosure text",
        "fix": "Review the retained local text and add a clear reader-facing disclosure when needed. This bounded English/Russian text-marker heuristic does not establish legal compliance; CSS visibility, other languages and remote disclosures require manual review.",
    },
    "HTTP_LINK_ON_HTTPS": {
        "severity": "notice",
        "source": "crawl:link_findings",
        "message": "HTTPS page contains an ordinary internal http:// anchor",
        "fix": "Write the internal anchor as https:// or a relative URL. A fetched HTTP variant that already redirects to HTTPS is reported as converged, not as this finding.",
    },
    "FORM_URL_INSECURE": {
        "severity": "critical",
        "source": "crawl:link_findings",
        "message": "A form submits to an http:// action, so its data leaves the browser "
        "unencrypted regardless of the page's own scheme",
        "fix": "Point the form's action at an https:// URL.",
    },
    "FORM_ON_HTTP_URL": {
        "severity": "critical",
        "source": "crawl:link_findings",
        "message": "A form with a password field is served from a plain-HTTP page, so the "
        "credentials themselves travel unencrypted before the action URL is even reached",
        "fix": "Serve the page itself over HTTPS; an HTTPS form action does not protect "
        "input typed on an HTTP page.",
    },
    "BROKEN_BOOKMARK": {
        "severity": "warning",
        "source": "crawl:fragment_links",
        "message": "Link fragment identifies no element on the destination page",
        "fix": "Point the href at an element id or <a name> that exists in the "
        "destination document, or add the missing target; a different query "
        "string is a different document.",
    },
    "DUPLICATE_ID": {
        "severity": "warning",
        "source": "crawl:duplicate_ids",
        "message": "HTML id value is repeated in the parsed document",
        "fix": "Give each element its own id; duplicate ids make fragment targets, scripts, and accessibility relationships ambiguous.",
    },
    "DECLARED_MIME_MISMATCH": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Received Content-Type conflicts with an unambiguous URL filename extension",
        "fix": "Confirm the response Content-Type and URL extension describe the intended resource; this is a declaration consistency signal, not full MIME validation.",
    },
    "BODY_MIME_MISMATCH": {
        "severity": "warning",
        "source": "crawl:body_mime",
        "message": "Retained response body does not match its declared Content-Type",
        "fix": "Serve the resource with a Content-Type that matches its body, or return the intended body.",
    },
    "URL_SESSION_ID": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Indexable URL contains a recognized session-token parameter",
        "fix": "Prevent session identifiers from entering crawlable URLs and consolidate any indexed variants; the finding retains parameter names but redacts their values.",
    },
    "URL_TRAILING_SLASH_INCONSISTENT": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Comparable trailing-slash URL variants both serve as separate indexable pages",
        "fix": "Choose and implement a documented URL policy if both variants are not intentional; redirect or canonical convergence already observed is not reported.",
    },
    "PLACEHOLDER_MARKER": {
        "severity": "warning",
        "source": "SF-derived",
        "message": "Page declaration contains a literal TODO, TBD, or placeholder template marker",
        "fix": "Replace the observed marker with reviewed page-specific copy. This check only reports explicit bracketed template tokens, not ordinary wording such as 'coming soon'.",
    },
    # Facts a crawl can observe: which machine-readable byline/date carriers a
    # content-shaped page declared, the discovered-and-indexable state of the
    # conventional trust pages, and where outbound links actually sit.
    # Deliberately no E-E-A-T score and no trustworthiness verdict -- every
    # finding quotes the carriers and the state it stands on so a specialist
    # reviews facts.
    # 9.C — objective trust & attribution evidence (issue #823)
    "NO_AUTHOR_BYLINE": {
        "severity": "notice",
        "source": "crawl:trust_signals",
        "message": "Content-shaped page declares no author or byline markup",
        "fix": "If the page's authorship should be attributable, declare it with a "
        "machine-readable carrier: meta author, rel=author, itemprop=author, or "
        "a JSON-LD author property.",
    },
    "NO_CONTENT_DATES": {
        "severity": "notice",
        "source": "crawl:trust_signals",
        "message": "Content-shaped page declares no publication or update date",
        "fix": "If the page should be dated, declare it machine-readably: "
        "article:published_time or datePublished markup, a <time datetime> "
        "element, or a Last-Modified response header.",
    },
    "MISSING_ABOUT_PAGE": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "No indexable About page was found in the crawl's scope",
        "fix": "If an About page exists, make it reachable and indexable; the "
        "finding's state says whether it was found non-indexable, broken, "
        "linked but never crawled, or not discovered at all.",
    },
    "MISSING_CONTACT_PAGE": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "No indexable Contact page was found in the crawl's scope",
        "fix": "If a Contact page exists, make it reachable and indexable; the "
        "finding's state says whether it was found non-indexable, broken, "
        "linked but never crawled, or not discovered at all.",
    },
    "MISSING_PRIVACY_POLICY": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "No indexable privacy policy page was found in the crawl's scope",
        "fix": "If a privacy policy exists, make it reachable and indexable; the "
        "finding's state says whether it was found non-indexable, broken, "
        "linked but never crawled, or not discovered at all.",
    },
    "MISSING_TERMS_PAGE": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "No indexable terms-of-use page was found in the crawl's scope",
        "fix": "If a terms page exists, make it reachable and indexable; the "
        "finding's state says whether it was found non-indexable, broken, "
        "linked but never crawled, or not discovered at all.",
    },
    "FEW_CITATIONS": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "Content-shaped page has no observable outbound reference in its body content",
        "fix": "Review whether the page should cite its sources. An outbound link "
        "inside the body copy is the only carrier a crawl can observe as a "
        "citation; the finding never asserts that any link is authoritative.",
    },
    "YMYL_REVIEW_CANDIDATE": {
        "severity": "notice",
        "source": "SF-derived",
        "message": "URL path or title matches a YMYL-adjacent keyword — a review "
        "candidate only, not a classification",
        "fix": "Have a specialist confirm whether the page genuinely covers "
        "Your-Money-Your-Life topics; if it does, the attribution and "
        "trust-page evidence above deserves a closer look.",
    },
    # Native asset budgets and supplied Lighthouse lab evidence (issue #1303)
    "ASSET_SHORT_CACHE": {
        "severity": "notice",
        "source": "native resource delivery",
        "message": "A captured resource has no long-lived public cache policy",
        "fix": "Use an appropriate public max-age for versioned static assets; keep private and no-store policies where required.",
    },
    "FONT_DISPLAY_MISSING": {
        "severity": "notice",
        "source": "native resource delivery",
        "message": "An observed stylesheet font-face lacks an accepted font-display value",
        "fix": "Set font-display to swap, fallback or optional when appropriate for the font.",
    },
    "HEAVY_PAGE_WEIGHT": {
        "severity": "warning",
        "source": "native render responses / HTML",
        "message": "Observed navigation responses or HTML alone exceed the configured content-decoded byte budget",
        "fix": "Reduce the measured document, images, fonts and scripts; keep raw-only and incomplete runtime coverage explicit.",
    },
    "RESOURCE_OVER_2MB": {
        "severity": "warning",
        "source": "native resource graph",
        "message": "A captured resource exceeds the configured individual byte budget",
        "fix": "Reduce or split the identified resource; the default budget is 2 MiB.",
    },
    "UNUSED_CSS_JS": {
        "severity": "notice",
        "source": "native render coverage",
        "message": "CSS or JavaScript source is substantially unused in the captured navigation window",
        "fix": "Split or defer unused code after testing the interactions and routes that need it; lab coverage is not proof code is universally unused.",
    },
    "UNMINIFIED_CSS_JS": {
        "severity": "notice",
        "source": "native resource delivery",
        "message": "Captured CSS or JavaScript does not look minified (heuristic)",
        "fix": "Apply the existing production minifier and verify the result still works.",
    },
    "DUPLICATED_JS": {
        "severity": "notice",
        "source": "native resource delivery",
        "message": "Different script URLs on one page contain identical captured source bytes",
        "fix": "Load the identical script once; review the build and page template.",
    },
    "LEGACY_JS": {
        "severity": "notice",
        "source": "native resource delivery",
        "message": "Captured JavaScript has an unconditional legacy or polyfill marker (heuristic)",
        "fix": "Review target browser support before removing or splitting legacy code.",
    },
    "IMG_INTRINSIC_OVERSIZED": {
        "severity": "notice",
        "source": "native render layout",
        "message": "A heavy image has more intrinsic width than its displayed device pixels need",
        "fix": "Serve a responsive image sized for the observed viewport and device pixel ratio.",
    },
    "IMG_UPSCALED": {
        "severity": "warning",
        "source": "native render layout",
        "message": "An image has less intrinsic width than its displayed device pixels need",
        "fix": "Supply an image with enough intrinsic pixels for the observed viewport and device pixel ratio.",
    },
    "LCP_DISCOVERY": {
        "severity": "notice",
        "source": "supplied Lighthouse JSON",
        "message": "The supplied Lighthouse LCP discovery audit reports an opportunity",
        "fix": "Review the recorded LCP element and make its image discoverable and appropriately prioritized.",
    },
    "NETWORK_DEPENDENCY_TREE": {
        "severity": "notice",
        "source": "supplied Lighthouse JSON",
        "message": "The supplied Lighthouse dependency-tree audit reports a request-chain opportunity",
        "fix": "Shorten the measured critical request chain; inspect the supplied lab trace evidence.",
    },
    "LAYOUT_SHIFT_CULPRITS": {
        "severity": "notice",
        "source": "supplied Lighthouse JSON",
        "message": "The supplied Lighthouse layout-shift audit reports culprits",
        "fix": "Reserve layout space and correct the measured shift sources.",
    },
    "IMAGE_DELIVERY": {
        "severity": "notice",
        "source": "supplied Lighthouse JSON",
        "message": "The supplied Lighthouse image-delivery audit reports an opportunity",
        "fix": "Optimize the measured image size and encoding while preserving visual quality.",
    },
    "FORCED_REFLOW": {
        "severity": "notice",
        "source": "supplied Lighthouse JSON",
        "message": "The supplied Lighthouse forced-reflow audit reports an opportunity",
        "fix": "Review the measured script stack and batch layout reads and writes.",
    },
    "JS_EXECUTION_TIME": {
        "severity": "notice",
        "source": "supplied Lighthouse JSON",
        "message": "The supplied Lighthouse bootup-time audit reports expensive script execution",
        "fix": "Reduce or defer the measured JavaScript execution work.",
    },
    # Document markup: structural AMP, static accessibility, and Schema.org validation
    "AMP_MISSING_HTML_ATTRIBUTE": {
        "severity": "warning",
        "source": "crawl:markup_evidence",
        "message": "AMP target does not declare the html amp or lightning attribute",
        "fix": "Add the AMP attribute to the root html element, or correct the rel=amphtml target.",
    },
    "AMP_INVALID_DOCTYPE": {
        "severity": "warning",
        "source": "crawl:markup_evidence",
        "message": "AMP document has a missing or invalid HTML5 doctype",
        "fix": "Start the AMP source document with <!doctype html>.",
    },
    "AMP_MISSING_HEAD": {
        "severity": "warning",
        "source": "crawl:markup_evidence",
        "message": "AMP source document has no explicit head element",
        "fix": "Include an explicit head element in the AMP source.",
    },
    "AMP_MISSING_BODY": {
        "severity": "warning",
        "source": "crawl:markup_evidence",
        "message": "AMP source document has no explicit body element",
        "fix": "Include an explicit body element in the AMP source.",
    },
    "AMP_INVALID_CHARSET": {
        "severity": "warning",
        "source": "crawl:markup_evidence",
        "message": "AMP head does not start with the required UTF-8 charset declaration",
        "fix": 'Place <meta charset="utf-8"> first in the AMP head.',
    },
    "AMP_INVALID_VIEWPORT": {
        "severity": "warning",
        "source": "crawl:markup_evidence",
        "message": "AMP document lacks the required device-width viewport",
        "fix": "Declare a viewport containing width=device-width in the head.",
    },
    "AMP_INVALID_RUNTIME": {
        "severity": "warning",
        "source": "crawl:markup_evidence",
        "message": "AMP document lacks an async AMP runtime script in the head",
        "fix": 'Add <script async src="https://cdn.ampproject.org/v0.js"></script> to the AMP head.',
    },
    "AMP_INVALID_BOILERPLATE": {
        "severity": "warning",
        "source": "crawl:markup_evidence",
        "message": "AMP document lacks the structural AMP boilerplate declarations",
        "fix": "Include the required amp-boilerplate style and noscript fallback; validate their content with the official AMP validator.",
    },
    "A11Y_HTML_LANG_MISSING": {
        "severity": "warning",
        "source": "crawl:markup_evidence",
        "message": "Root html element has no nonempty lang declaration",
        "fix": "Declare the document language on html and review whether it matches the content.",
    },
    "A11Y_FORM_LABEL_MISSING": {
        "severity": "warning",
        "source": "crawl:markup_evidence",
        "message": "Form control has no static accessible label",
        "fix": "Associate a label with the control, or provide a valid aria-label or aria-labelledby reference.",
    },
    "A11Y_IMAGE_ALT_MISSING": {
        "severity": "warning",
        "source": "crawl:markup_evidence",
        "message": "Image has no alt attribute or static accessible name",
        "fix": "Provide meaningful alternative text; explicitly mark decorative images with empty alt text.",
    },
    "A11Y_HEADING_ORDER": {
        "severity": "notice",
        "source": "crawl:markup_evidence",
        "message": "Heading sequence skips a level in the static document",
        "fix": "Review the heading outline and use nested heading levels consistently.",
    },
    "A11Y_MAIN_LANDMARK_MISSING": {
        "severity": "notice",
        "source": "crawl:markup_evidence",
        "message": "Document content has no main landmark",
        "fix": "Mark the primary page content with main or role=main and verify navigation with assistive technology.",
    },
    "STRUCTURED_DATA_SCHEMA_ERROR": {
        "severity": "warning",
        "source": "crawl:structured_evidence",
        "message": "Structured data contains errors under the bundled Schema.org vocabulary",
        "fix": "Inspect the URL schema evidence and repair unknown types, properties, invalid values or unresolved references.",
    },
    "STRUCTURED_DATA_SCHEMA_WARNING": {
        "severity": "notice",
        "source": "crawl:structured_evidence",
        "message": "Structured data contains warnings under the bundled Schema.org vocabulary",
        "fix": "Review the URL schema evidence and correct ambiguous values or graph relationships.",
    },
    "STRUCTURED_DATA_RICH_RESULT_REQUIRED_MISSING": {
        "severity": "warning",
        "source": "crawl:structured_evidence",
        "message": "Structured data lacks required properties in the bundled local rich-result rules",
        "fix": "Fill the required properties for the declared type and validate the result; local eligibility does not guarantee Google display.",
    },
    "GSC_ORPHAN": {
        "severity": "warning",
        "source": "provider:gsc_search_analytics",
        "message": "Search Console reports impressions for a same-origin URL absent from the crawl",
        "fix": "Check the retained page and crawl scope, then restore, redirect, or remove the URL from search when appropriate.",
    },
    "NO_SEARCH_DATA": {
        "severity": "notice",
        "source": "provider:gsc_search_analytics",
        "message": "Indexable crawled URL has no retained Search Console impressions",
        "fix": "Review indexing, canonicalization, demand, and the measured reporting window before changing the page.",
    },
    "NON_INDEXABLE_WITH_SEARCH_DATA": {
        "severity": "warning",
        "source": "provider:gsc_search_analytics",
        "message": "Non-indexable crawled URL has Search Console impressions",
        "fix": "Review the URL's indexability and canonical signals; keep the current state only when it is intentional.",
    },
    "GSC_INDEXED_WITH_ISSUES": {
        "severity": "warning",
        "source": "provider:gsc_url_inspection",
        "message": "Search Console reports an indexed URL with AMP or rich-result issues",
        "fix": "Review the specific AMP or rich-result inspection evidence for the sampled URL.",
    },
    "GSC_AMP_INVALID": {
        "severity": "warning",
        "source": "provider:gsc_url_inspection",
        "message": "Search Console reports an invalid AMP result",
        "fix": "Review the AMP inspection result and repair or remove obsolete AMP markup.",
    },
    "GSC_RICH_RESULTS_INVALID": {
        "severity": "warning",
        "source": "provider:gsc_url_inspection",
        "message": "Search Console reports invalid structured data for a rich result",
        "fix": "Review the inspected rich-result item and correct errors reported by Search Console.",
    },
    "GSC_INDEXABLE_NOT_INDEXED": {
        "severity": "warning",
        "source": "provider:gsc_url_inspection",
        "message": "Crawl reports an indexable URL that Search Console does not index",
        "fix": "Review Search Console's coverage state, canonical, fetch state, and indexing directives.",
    },
    "GSC_USER_CANONICAL_NOT_SELECTED": {
        "severity": "warning",
        "source": "provider:gsc_url_inspection",
        "message": "Google selected a different canonical URL than the declared canonical",
        "fix": "Align internal links, redirects, sitemap membership, and the declared canonical with the preferred URL.",
    },
    "GSC_URL_NOT_ON_GOOGLE": {
        "severity": "warning",
        "source": "provider:gsc_url_inspection",
        "message": "Search Console reports that the inspected URL is not on Google",
        "fix": "Review the sampled URL's Search Console coverage state and correct blocking or canonical issues where appropriate.",
    },
    "GA_ORPHAN_URL": {
        "severity": "warning",
        "source": "provider:ga4_history",
        "message": "GA4 reports a landing URL absent from the crawl",
        "fix": "Check whether the URL is intentionally retired, blocked, redirected, or missing from the crawl population.",
    },
    "NO_GA_DATA": {
        "severity": "notice",
        "source": "provider:ga4_history",
        "message": "Indexable crawled URL has no retained GA4 sessions",
        "fix": "Review the measured period and analytics tagging before treating the absence as a page defect.",
    },
    "NON_INDEXABLE_WITH_GA_DATA": {
        "severity": "warning",
        "source": "provider:ga4_history",
        "message": "Non-indexable crawled URL has GA4 sessions",
        "fix": "Review whether the URL should be indexable; keep its current state only when that is intentional.",
    },
    "BOUNCE_RATE_ABOVE_70": {
        "severity": "notice",
        "source": "provider:metrika_history",
        "message": "Yandex Metrika reports a bounce rate above 70 percent",
        "fix": "Review the measured URL and its traffic source; bounce rate is a diagnostic signal, not a ranking verdict.",
    },
    "METRIKA_ORPHAN_URL": {
        "severity": "warning",
        "source": "provider:metrika_history",
        "message": "Yandex Metrika reports a landing URL absent from the crawl",
        "fix": "Check whether the URL is intentionally retired, blocked, redirected, or missing from the crawl population.",
    },
    "NO_METRIKA_DATA": {
        "severity": "notice",
        "source": "provider:metrika_history",
        "message": "Indexable crawled URL has no retained Yandex Metrika visits",
        "fix": "Review the measured period and Metrika tagging before treating the absence as a page defect.",
    },
    "NON_INDEXABLE_WITH_METRIKA_DATA": {
        "severity": "warning",
        "source": "provider:metrika_history",
        "message": "Non-indexable crawled URL has Yandex Metrika visits",
        "fix": "Review whether the URL should be indexable; keep its current state only when that is intentional.",
    },
    "METRIKA_BOUNCE_RATE_ABOVE_70": {
        "severity": "notice",
        "source": "provider:metrika_history",
        "message": "Yandex Metrika reports a bounce rate above 70 percent",
        "fix": "Review the measured URL and its traffic source; bounce rate is a diagnostic signal, not a ranking verdict.",
    },
}


def check_meta(check_id: str) -> dict[str, Any]:
    return CHECKS.get(
        check_id, {"severity": "notice", "source": "SF-derived", "message": check_id, "fix": None}
    )


# ── export preconditions ─────────────────────────────────────────────────────
#
# Which loader frame a check needs before it can say anything. Without this a
# check whose evidence never arrived reports nothing, and a report renders
# nothing as clean — the toolkit's one remaining place where "not measured" and
# "no problem" looked identical.
#
# Response-code checks are deliberately absent: they read status codes straight
# from ``internal_all`` and need no separate frame, so declaring one would turn
# an honest "no 5xx found" into a false "skipped".
# REDIRECT_CHAIN and REDIRECT_LOOP are also absent: the native report is only
# one of two ways they can be answered now that ``check_redirect_chains``
# falls back to resolving ``internal_all``'s own Redirect URL column, so
# declaring the report a hard requirement here would mark them skipped even
# when the fallback just fired. They still skip by name, from inside the
# check, when neither source has redirect data.
# Only frames other than ``internal_all`` are listed: the master table is
# required for a run at all, and checks derived from its columns guard
# themselves per column. ``test_every_declared_check_reports_its_frame_as_missing_when_absent``
# covers the entries declared here. Checks absent from this map rely on their
# own evidence guards; this map does not provide a universal inline-skip gate.
CHECK_REQUIRES: dict[str, tuple[str, ...]] = {
    "PAGINATION_CANONICAL_POLICY": ("internal_all",),
    "FILTER_CANONICAL_POLICY": ("internal_all",),
    "IMG_MISSING_ALT": ("images_missing_alt",),
    "IMG_OVER_KB": ("images_over_kb",),
    "IMG_MISSING_DIMENSIONS": ("images_missing_size",),
    "MIXED_CONTENT": ("security_mixed",),
    "MISSING_HSTS": ("security_hsts",),
    "STRUCTURED_DATA_MISSING": ("structured_data_missing",),
    "HREFLANG_ERROR": ("hreflang",),
    "HREFLANG_BROKEN_TARGET": ("all_hreflang",),
    # These three read an SF-native Sitemaps:* comparison export directly
    # (seohead.sf.core.sitemap_coverage._emit_from_export) and have no other
    # evidence source -- unlike SITEMAP_ORPHAN and URL_NOT_IN_SITEMAP, which a
    # native crawl answers itself from its own link graph and must stay off
    # this list so crawl_site's own evidence is never overridden by an absent
    # export (issue #165).
    "SITEMAP_URL_4XX_5XX": ("sitemap_non_200",),
    "SITEMAP_URL_3XX": ("sitemap_redirects",),
    "SITEMAP_URL_NON_INDEXABLE": ("sitemap_non_indexable",),
}


def missing_requirements(check_id: str, available: set[str]) -> tuple[str, ...]:
    """Frames a check needs that the run does not have."""
    return tuple(f for f in CHECK_REQUIRES.get(check_id, ()) if f not in available)
