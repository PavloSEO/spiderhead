//! Category rows of the crawler catalogue coverage map, one function per category.
//! The rows are plain data: edit them directly and keep the counts in `tests.rs` in step.

use super::Entry;

/// Rows of the catalogue category `Response Codes`.
pub(super) fn response_codes() -> Vec<Entry> {
    vec![
        Entry::check("Internal No Response", &["NO_RESPONSE"]),
        Entry::check("Internal Client Error (4XX)", &["BROKEN_PAGE_4XX", "BROKEN_INTERNAL_LINK"]),
        Entry::check("Internal Server Error (5XX)", &["SERVER_ERROR_5XX", "LINK_TO_5XX"]),
        Entry::check("Internal Redirect Loop", &["REDIRECT_LOOP"]),
        Entry::check("Internal Blocked by Robots.txt", &["BLOCKED_BY_ROBOTS", "IMPORTANT_URL_BLOCKED_BY_ROBOTS"]),
        Entry::check("Internal Blocked Resource", &["ROBOTS_BLOCKS_RESOURCES"]),
        Entry::check("Internal Redirect Chain", &["REDIRECT_CHAIN"]),
        Entry::out_of_scope("External Blocked Resource").note("we do not fetch another host's robots.txt: a crawler that goes and asks somebody else's server about its own rules is a crawler that wanders. No part of this is measured -- resource transport is bounded to the crawled origin -- so this is a decision, not a partial finding"),
        Entry::check("Internal Redirection (3XX)", &["INTERNAL_LINK_TO_REDIRECT", "BAD_REDIRECT_TYPE"]),
        Entry::check("Internal Redirection (Meta Refresh)", &["META_REFRESH_REDIRECT"]),
        Entry::check("Internal Redirection (HTTP Refresh)", &["HTTP_REFRESH_REDIRECT"]),
        Entry::check("Internal Redirection (JavaScript)", &["JS_REDIRECT"]).note("a script-initiated top-frame navigation observed in the retained render evidence (navigation cause `script_navigation`) is reported as a redirect; raw-only scans and crawler exports keep no navigation evidence and skip by name"),
        Entry::tool("External No Response", &["links-check"]),
        Entry::check("External Client Error (4XX)", &["BROKEN_EXTERNAL_LINK"]),
        Entry::check("External Server Error (5XX)", &["BROKEN_EXTERNAL_LINK"]),
    ]
}

/// Rows of the catalogue category `Security`.
pub(super) fn security() -> Vec<Entry> {
    vec![
        Entry::check("HTTP URLs", &["HTTP_URL"]),
        Entry::check("Mixed Content", &["MIXED_CONTENT", "INSECURE_SUBRESOURCE"]),
        Entry::check("Form URL Insecure", &["FORM_URL_INSECURE"]),
        Entry::check("Form On HTTP URL", &["FORM_ON_HTTP_URL"]),
        Entry::check("Missing HSTS Header", &["MISSING_HSTS"]),
        Entry::check("Unsafe Cross Origin Links", &["UNSAFE_CROSS_ORIGIN_LINK"]),
        Entry::check(
            "Protocol-Relative Resource Links",
            &["PROTOCOL_RELATIVE_LINK"],
        ),
        Entry::check("Missing Content-Security-Policy Header", &["MISSING_CSP"]),
        Entry::check(
            "Missing X-Content-Type-Options Header",
            &["MISSING_X_CONTENT_TYPE_OPTIONS"],
        ),
        Entry::check(
            "Missing X-Frames-Options Header",
            &["MISSING_X_FRAME_OPTIONS"],
        ),
        Entry::check(
            "Missing Secure Referrer-Policy Header",
            &["MISSING_REFERRER_POLICY"],
        ),
        Entry::check(
            "Bad Content Type",
            &["BODY_MIME_MISMATCH", "DECLARED_MIME_MISMATCH"],
        ),
    ]
}

/// Rows of the catalogue category `URL`.
pub(super) fn url() -> Vec<Entry> {
    vec![
        Entry::check("Multiple Slashes", &["URL_MULTIPLE_SLASHES"]),
        Entry::check("Contains A Space", &["URL_CONTAINS_SPACE"]),
        Entry::check("Broken Bookmark", &["BROKEN_BOOKMARK"]).note("every fragment-bearing outlink resolves against the destination document's retained DOM inventory (WHATWG scroll-to-fragment, #827); inputs without HTML/DOM bodies -- crawler exports, legacy imports -- skip by name, scroll-to-text directives are named skips rather than element targets, and an unreadable destination body never counts as clean"),
        Entry::check("Non ASCII Characters", &["URL_NON_ASCII"]),
        Entry::check("Uppercase", &["URL_UPPERCASE"]),
        Entry::check("Repetitive Path", &["URL_REPETITIVE_PATH"]),
        Entry::check("Internal Search", &["URL_INTERNAL_SEARCH"]).note("recognized on path segments (search, site-search, recherche, suche, poisk) and on query parameters (s, search, q, query); a search route spelled outside that list stays unrecognized -- the list is ours, the field's is configurable"),
        Entry::check("Parameters", &["URL_HAS_PARAMS"]),
        Entry::check("GA Tracking Parameters", &["URL_TRACKING_PARAMS"]),
        Entry::check("Underscores", &["URL_UNDERSCORES"]),
        Entry::check("Over 115 Characters", &["URL_TOO_LONG"]),
    ]
}

/// Rows of the catalogue category `Page Titles`.
pub(super) fn page_titles() -> Vec<Entry> {
    vec![
        Entry::check("Missing", &["TITLE_MISSING"]),
        Entry::check("Multiple", &["TITLE_MULTIPLE"]),
        Entry::check("Outside <head>", &["TITLE_OUTSIDE_HEAD"]),
        Entry::check("Duplicate", &["TITLE_DUPLICATE"]),
        Entry::check("Over 60 Characters", &["TITLE_TOO_LONG"]),
        Entry::check("Below 30 Characters", &["TITLE_TOO_SHORT"]),
        Entry::check("Over 561 Pixels", &["TITLE_PIXELS_OVER"]).note("compares the export's measured pixel-width column against the named threshold; the width is never recomputed here, and runs without the column skip by name (#1578)"),
        Entry::check("Below 200 Pixels", &["TITLE_PIXELS_BELOW"]).note("same pixel-width evidence as Over 561 Pixels (#1578)"),
        Entry::check("Same as H1", &["TITLE_EQUALS_H1"]),
    ]
}

/// Rows of the catalogue category `Meta Description`.
pub(super) fn meta_description() -> Vec<Entry> {
    vec![
        Entry::check("Multiple", &["DESC_MULTIPLE"]).note("the first occurrence stays the authoritative value every existing length and duplication check reads; this check only counts how many live occurrences there are"),
        Entry::check("Outside <head>", &["DESC_OUTSIDE_HEAD"]),
        Entry::check("Missing", &["DESC_MISSING"]),
        Entry::check("Duplicate", &["DESC_DUPLICATE"]),
        Entry::check("Over 155 Characters", &["DESC_TOO_LONG"]),
        Entry::check("Below 70 Characters", &["DESC_TOO_SHORT"]),
        Entry::check("Over 985 Pixels", &["DESC_PIXELS_OVER"]).note("compares the export's measured pixel-width column against the named threshold (#1578)"),
        Entry::check("Below 400 Pixels", &["DESC_PIXELS_BELOW"]).note("same pixel-width evidence as Over 985 Pixels (#1578)"),
    ]
}

/// Rows of the catalogue category `H1`.
pub(super) fn h1() -> Vec<Entry> {
    vec![
        Entry::check("Missing", &["H1_MISSING"]),
        Entry::check("Multiple", &["H1_MULTIPLE"]),
        Entry::check("Alt Text in h1", &["H1_ALT_TEXT_ONLY"]).note("fires only when the H1 has no text of its own; a logo image beside real heading text is normal and is never flagged"),
        Entry::tool("Non-sequential", &["parse"]).note("the full H1-H6 order is the heading_outline field of parse, not a crawl-registry check"),
        Entry::check("Duplicate", &["H1_DUPLICATE"]),
        Entry::check("Over 70 Characters", &["H1_TOO_LONG"]),
    ]
}

/// Rows of the catalogue category `H2`.
pub(super) fn h2() -> Vec<Entry> {
    vec![
        Entry::check("Missing", &["H2_MISSING"]),
        Entry::check("Multiple", &["H2_MULTIPLE"]).note("matches the catalogue's own filter definition -- more than one H2. The count is read exactly from the native heading outline; a crawler export proves 'more than one' from a non-empty H2-2 column without an exact count"),
        Entry::tool("Non-sequential", &["parse"]),
        Entry::check("Duplicate", &["H2_DUPLICATE"]),
        Entry::check("Over 70 Characters", &["H2_TOO_LONG"]),
    ]
}

/// Rows of the catalogue category `Content`.
pub(super) fn content() -> Vec<Entry> {
    vec![
        Entry::check("Exact Duplicates", &["DUPLICATE_BY_HASH"]),
        Entry::check("Spelling Errors", &["SPELLING_ERRORS"]).note("from a crawler export column; not computed"),
        Entry::check("Grammar Errors", &["GRAMMAR_ERRORS"]).note("from a crawler export column; not computed"),
        Entry::tool("Soft 404 Pages", &["soft404-check"]),
        Entry::check("Lorem Ipsum Placeholder", &["LOREM_IPSUM_PLACEHOLDER"]).note("matched as the full multi-word passage within the resolved content area, never a substring of the whole document, so a page merely mentioning it once outside the content area is not flagged"),
        Entry::check("Near Duplicates", &["NEAR_DUPLICATE"]).note("crawler native columns or bounded SimHash/LSH over content-area text from the native scan's selected retained document or input.html_store_dir; missing bodies and exhausted analysis budgets are explicit omissions"),
        Entry::check("Semantically Similar", &["SEMANTICALLY_SIMILAR"]).note("the offline lexical adapter (TF-IDF over content-area shingles, upgraded to a locally cached sentence-transformers model when present) compares retained bodies pairwise; scores are lexical overlap, never meaning, so every pair is a review candidate"),
        Entry::check("Low Relevance Content", &["LOW_RELEVANCE_CONTENT"]).note("the same offline adapter measures how much of the title's and H1's vocabulary appears in the retained content area as a review candidate; lexical containment does not establish query relevance"),
        Entry::check("Low Content Pages", &["THIN_CONTENT", "LOW_TEXT_RATIO"]),
        Entry::check("Readability Difficult", &["READABILITY_DIFFICULT", "LONG_SENTENCES"]),
        Entry::check("Readability Very Difficult", &["READABILITY_DIFFICULT"]),
    ]
}

/// Rows of the catalogue category `Images`.
pub(super) fn images() -> Vec<Entry> {
    vec![
        Entry::check("Missing Alt Text", &["IMG_MISSING_ALT"]),
        Entry::check("Missing Alt Attribute", &["IMG_MISSING_ALT_ATTRIBUTE"]).note("a native crawl's own per-image inventory distinguishes the attribute being absent from a decorative alt=\"\"; native IMG_MISSING_ALT separately reports whitespace-only or explicitly informative/unnamed-interactive empty alt. The crawler export keeps its original filter semantics"),
        Entry::tool("Background Images", &["parse"]).note("CSS url() sources are extracted by the parser's url_sources option, which is how four images invisible to the HTML were found on a live site"),
        Entry::check("Over 100 kb", &["IMG_OVER_KB"]),
        Entry::check("Alt Text Over 100 Characters", &["IMG_ALT_TOO_LONG"]),
        Entry::check("Incorrectly Sized Images", &["IMG_INTRINSIC_OVERSIZED", "IMG_UPSCALED"]).note("retained render layout compares intrinsic and displayed width at the recorded DPR; oversized means more than IMAGE_OVERSIZED_RATIO (2) times the device pixels with an IMAGE_OVERSIZED_MIN_BYTES (10 KiB) wire-weight floor, upscaled means narrower than the box, and raw-only scans skip by name (#1578)"),
        Entry::check("Missing Size Attributes", &["IMG_MISSING_DIMENSIONS"]),
    ]
}

/// Rows of the catalogue category `Canonicals`.
pub(super) fn canonicals() -> Vec<Entry> {
    vec![
        Entry::check("Multiple Conflicting", &["CANONICAL_MULTIPLE"]),
        Entry::check("Non-Indexable Canonical", &["CANONICAL_NON_INDEXABLE"]),
        Entry::check("Invalid Attribute In Annotation", &["CANONICAL_INVALID_ATTRIBUTE"]).note("native selected-document annotations preserve raw rel/href and alternate-version attributes; canonical links carrying hreflang/lang/media/type are ignored for canonicalization; malformed separators and hrefs are also observable; unknown well-formed relation keywords remain unknown"),
        Entry::check("Contains Fragment URL", &["CANONICAL_FRAGMENT"]),
        Entry::check("Outside <head>", &["CANONICAL_OUTSIDE_HEAD"]),
        Entry::check("Canonicalised", &["CANONICALISED"]),
        Entry::check("Missing", &["CANONICAL_MISSING"]),
        Entry::check("Unlinked", &["UNLINKED_CANONICAL"]),
        Entry::check("Multiple", &["CANONICAL_MULTIPLE"]),
        Entry::check("Canonical Is Relative", &["CANONICAL_RELATIVE"]),
    ]
}

/// Rows of the catalogue category `Pagination`.
pub(super) fn pagination() -> Vec<Entry> {
    vec![
        Entry::check("Pagination URL Not In Anchor Tag", &["PAGINATION_URL_NOT_IN_ANCHOR"]),
        Entry::check("Non-200 Pagination URLs", &["PAGINATION_NONINDEXABLE"]),
        Entry::check("Unlinked Pagination URLs", &["UNLINKED_PAGINATION_SERIES"]),
        Entry::check("Multiple Pagination URLs", &["PAGINATION_MULTIPLE"]),
        Entry::check("Pagination Loop", &["PAGINATION_LOOP"]),
        Entry::check("Sequence Error", &["PAGINATION_SEQUENCE_ERROR"]).note("numbered runs and dominant strides use observed page-number steps; opaque native rel=next/prev chains are judged only for explicit reciprocal contradictions in selected-document annotations; absent, ambiguous or uncaptured relationships remain unjudged"),
        Entry::check("Non-Indexable", &["PAGINATION_NONINDEXABLE"]),
    ]
}

/// Rows of the catalogue category `Directives`.
pub(super) fn directives() -> Vec<Entry> {
    vec![
        Entry::check("Outside <head>", &["DIRECTIVES_OUTSIDE_HEAD"]),
        Entry::check("NoImageIndex", &["NOIMAGEINDEX"]),
        Entry::check("Noindex", &["NOINDEX"]),
        Entry::check("Nofollow", &["NOFOLLOW_PAGE"]),
        Entry::check("None", &["NOINDEX", "NOFOLLOW_PAGE"])
            .note("'none' is expanded to noindex+nofollow when directives are parsed"),
        Entry::check("Unavailable_After", &["UNAVAILABLE_AFTER"]),
        Entry::check("NoSnippet", &["NOSNIPPET"]),
        Entry::check("NoODP", &["NOODP"]),
        Entry::check("NoYDIR", &["NOYDIR"]),
        Entry::check("NoTranslate", &["NOTRANSLATE"]),
    ]
}

/// Rows of the catalogue category `Hreflang`.
pub(super) fn hreflang() -> Vec<Entry> {
    vec![
        Entry::check("Non-200 Hreflang URLs", &["HREFLANG_BROKEN_TARGET"]),
        Entry::check("Missing Return Links", &["HREFLANG_MISSING_RETURN_LINK"]),
        Entry::check(
            "Inconsistent Language & Region Confirmation Links",
            &["HREFLANG_INCONSISTENT_CONFIRMATION"],
        ),
        Entry::check("Non-Canonical Return Links", &["HREFLANG_NOT_CANONICAL"]),
        Entry::check("Noindex Returns Links", &["HREFLANG_NOINDEX_TARGET"]),
        Entry::check(
            "Incorrect Language & Region Codes",
            &["HREFLANG_INVALID_CODE"],
        ),
        Entry::check("Multiple Entries", &["HREFLANG_MULTIPLE_ENTRIES"]),
        Entry::check("Not Using Canonical", &["HREFLANG_NOT_CANONICAL"]),
        Entry::check("Outside <head>", &["HREFLANG_OUTSIDE_HEAD"]),
        Entry::check("Unlinked Hreflang URLs", &["HREFLANG_UNLINKED_TARGET"]),
        Entry::check(
            "Missing Self Reference",
            &["HREFLANG_MISSING_SELF_REFERENCE"],
        ),
        Entry::check("Missing X-Default", &["HREFLANG_MISSING_XDEFAULT"]),
    ]
}

/// Rows of the catalogue category `JavaScript`.
pub(super) fn javascript() -> Vec<Entry> {
    vec![
        Entry::check(
            "Noindex Only in Original HTML",
            &["NOINDEX_ONLY_IN_RAW_HTML"],
        ),
        Entry::check(
            "Nofollow Only in Original HTML",
            &["NOFOLLOW_ONLY_IN_RAW_HTML"],
        ),
        Entry::check("Canonical Mismatch", &["CANONICAL_MISMATCH"]),
        Entry::check(
            "Uses Old AJAX Crawling Scheme URLs",
            &["AJAX_CRAWLING_SCHEME_URL"],
        ),
        Entry::check(
            "Uses Old AJAX Crawling Scheme Meta Fragment Tag",
            &["AJAX_CRAWLING_SCHEME_META_FRAGMENT"],
        ),
        Entry::check(
            "Pages with Blocked Resources",
            &["ROBOTS_BLOCKS_RESOURCES", "RENDER_RESOURCE_BLOCKED"],
        ),
        Entry::tool("Contains JavaScript Links", &["render-check"]),
        Entry::tool("Contains JavaScript Content", &["render-check"]),
        Entry::check(
            "Page Title Only in Rendered HTML",
            &["PAGE_TITLE_ONLY_IN_RENDERED_HTML"],
        ),
        Entry::check(
            "Page Title Updated by JavaScript",
            &["PAGE_TITLE_UPDATED_BY_JAVASCRIPT"],
        ),
        Entry::check(
            "Meta Description Only in Rendered HTML",
            &["META_DESCRIPTION_ONLY_IN_RENDERED_HTML"],
        ),
        Entry::check(
            "Meta Description Updated by JavaScript",
            &["META_DESCRIPTION_UPDATED_BY_JAVASCRIPT"],
        ),
        Entry::check("H1 Only in Rendered HTML", &["H1_ONLY_IN_RENDERED_HTML"]),
        Entry::check("H1 Updated by JavaScript", &["H1_UPDATED_BY_JAVASCRIPT"]),
        Entry::check(
            "Canonical Only in Rendered HTML",
            &["CANONICAL_ONLY_IN_RENDERED_HTML"],
        ),
        Entry::check("Pages With JavaScript Errors", &["JS_CONSOLE_ERRORS"]),
    ]
}

/// Rows of the catalogue category `Links`.
pub(super) fn links() -> Vec<Entry> {
    vec![
        Entry::check("Outlinks To Localhost", &["OUTLINK_TO_LOCALHOST"]),
        Entry::check("Pages With Uncrawlable Internal Outlinks", &["ROBOTS_BLOCKED_OUTLINK", "BLOCKED_BY_ROBOTS", "IMPORTANT_URL_BLOCKED_BY_ROBOTS"]).note("the robots-caused half is measured from the source side (ROBOTS_BLOCKED_OUTLINK: the link edge is the only evidence connecting a page to a destination the crawl's own policy never fetched) and from the destination side (BLOCKED_BY_ROBOTS, IMPORTANT_URL_BLOCKED_BY_ROBOTS). The remaining exclusion reasons -- exclude/include patterns, segments_only, depth and query-variant budgets -- are the run's own scope configuration, not site defects, so they are deliberately not reported as findings"),
        Entry::check("Pages Without Internal Outlinks", &["NO_INTERNAL_OUTLINKS"]),
        Entry::check("Non-Indexable Page Inlinks Only", &["ONLY_NONINDEXABLE_SOURCE_INLINKS"]),
        Entry::check("Internal Nofollow Outlinks", &["INTERNAL_NOFOLLOW_OUTLINKS"]),
        Entry::check("Pages With High External Outlinks", &["HIGH_EXTERNAL_OUTLINKS"]),
        Entry::check("Pages With High Internal Outlinks", &["HIGH_OUTLINKS"]),
        Entry::check("Follow & Nofollow Internal Inlinks To Page", &["FOLLOW_AND_NOFOLLOW_INLINKS"]),
        Entry::check("Internal Nofollow Inlinks Only", &["ONLY_NOFOLLOW_INLINKS"]),
        Entry::check("Pages With High Crawl Depth", &["DEEP_CRAWL_DEPTH", "DEEP_DISCOVERY_PATH"]),
        Entry::check("Internal Outlinks With No Anchor Text", &["GENERIC_ANCHOR_TEXT", "INTERNAL_LINK_NO_ANCHOR"]).note("the wording is judged from Inlinks anchor text (GENERIC_ANCHOR_TEXT); the absence of any text is measured from retained native links (INTERNAL_LINK_NO_ANCHOR), so a crawler export without a retained scan sees wording only"),
        Entry::check("Non-Descriptive Anchor Text In Internal Outlinks", &["GENERIC_ANCHOR_TEXT"]),
    ]
}

/// Rows of the catalogue category `Structured Data`.
pub(super) fn structured_data() -> Vec<Entry> {
    vec![
        Entry::check("Validation Errors", &["SCHEMA_VALIDATION_ERROR"]),
        Entry::tool("Rich Result Validation Errors", &["schema-check"]),
        Entry::check("Parse Errors", &["STRUCTURED_DATA_PARSE_ERROR"]).note("found and parsed block counts were already recorded per page; this reads the two together and fires when found exceeds parsed"),
        Entry::check("Missing", &["STRUCTURED_DATA_MISSING"]),
        Entry::tool("Validation Warnings", &["schema-check"]),
        Entry::tool("Rich Result Validation Warnings", &["schema-check"]),
    ]
}

/// Rows of the catalogue category `Sitemaps`.
pub(super) fn sitemaps() -> Vec<Entry> {
    vec![
        Entry::check("XML Sitemap With Over 50k URLs", &["SITEMAP_TOO_MANY_URLS"]),
        Entry::check("XML Sitemap Over 50mb", &["SITEMAP_TOO_LARGE"]),
        Entry::check("URLs Not In Sitemap", &["URL_NOT_IN_SITEMAP"]),
        Entry::check("Orphan URLs", &["SITEMAP_ORPHAN", "ORPHAN_PAGE"]),
        Entry::check("Non-Indexable URLs In Sitemap", &["SITEMAP_URL_NON_INDEXABLE", "SITEMAP_URL_NON_CANONICAL"]).note("a crawler Sitemaps:* comparison export is read directly; a native crawl answers from its own reconciliation -- noindex and canonical-to-another-URL are judged on the crawl's fetch of each declared URL, and declared URLs the crawl never requested are counted as unmeasured rather than read as clean"),
        Entry::check("URLs In Multiple Sitemaps", &["SITEMAP_URL_DUPLICATED"]),
    ]
}

/// Rows of the catalogue category `PageSpeed`.
pub(super) fn pagespeed() -> Vec<Entry> {
    vec![
        Entry::check("Document Request Latency", &["SLOW_RESPONSE"]),
        Entry::check("LCP Request Discovery", &["LCP_DISCOVERY"]).note("supplied matching per-URL Lighthouse lab JSON only; no automatic trace collection"),
        Entry::check("Render Blocking Requests", &["RENDER_BLOCKING"]),
        Entry::check("Network Dependency Tree", &["NETWORK_DEPENDENCY_TREE"]).note("supplied per-URL Lighthouse lab JSON"),
        Entry::check("Use Efficient Cache Lifetimes", &["ASSET_SHORT_CACHE"]).note("completed native resource headers; static heuristic, not a Lighthouse score"),
        Entry::check("Layout Shift Culprits", &["LAYOUT_SHIFT_CULPRITS"]).note("supplied per-URL Lighthouse lab JSON"),
        Entry::check("Improve Image Delivery", &["IMAGE_DELIVERY"]).note("supplied per-URL Lighthouse lab JSON"),
        Entry::check("Forced Reflow", &["FORCED_REFLOW"]).note("supplied per-URL Lighthouse lab JSON"),
        Entry::check("Legacy JavaScript", &["LEGACY_JS"]).note("markers in completed native script source; heuristic"),
        Entry::check("Duplicated JavaScript", &["DUPLICATED_JS"]).note("exact captured bytes at different script URLs on the same page"),
        Entry::check("Avoid Enormous Network Payloads", &["HEAVY_NETWORK_PAYLOAD", "HEAVY_PAGE_WEIGHT", "LARGE_HTML", "HTML_BLOAT"]).note("the page's total captured payload -- document plus every declared subresource that was actually fetched -- is budgeted in content-decoded bytes, and the render path budgets observed navigation responses too (HEAVY_PAGE_WEIGHT). These are not compressed transfer measurements, and navigations beyond the recorded window are not modelled; incomplete declaration coverage stays a named lower bound. LARGE_HTML/HTML_BLOAT retain HTML-only scope"),
        Entry::check("Minify CSS", &["UNMINIFIED_CSS_JS"]).note("completed native source; whitespace/line-length heuristic"),
        Entry::check("Minify JavaScript", &["UNMINIFIED_CSS_JS"]).note("completed native source; whitespace/line-length heuristic"),
        Entry::check("Reduce Unused CSS", &["UNUSED_CSS_JS"]).note("bounded Chromium CDP coverage from the recorded navigation window"),
        Entry::check("Reduce Unused JavaScript", &["UNUSED_CSS_JS"]).note("bounded Chromium CDP coverage from the recorded navigation window"),
        Entry::check("Reduce JavaScript Execution Time", &["JS_EXECUTION_TIME"]).note("supplied Lighthouse bootup-time lab result"),
        Entry::out_of_scope("Minimize Main-Thread Work").note("needs a CPU profile"),
        Entry::check("Optimize DOM Size", &["DOM_TOO_MANY_NODES", "DOM_TOO_DEEP"]).note("reads the selected retained native document or crawler-stored HTML; unretained, over-budget and invalid bodies are reported with their true reason"),
        Entry::check("Font Display", &["FONT_DISPLAY_MISSING"]).note("font-face blocks in completed native stylesheets"),
    ]
}

/// Rows of the catalogue category `Mobile`.
pub(super) fn mobile() -> Vec<Entry> {
    vec![
        Entry::check("Viewport Not Set", &["VIEWPORT_MISSING"]),
        Entry::check("Content Not Sized Correctly", &["MOBILE_CONTENT_OVERFLOW"]),
        Entry::check("Illegible Font Size", &["MOBILE_TEXT_TOO_SMALL"]),
        Entry::check("Contains Unsupported Plugins", &["UNSUPPORTED_PLUGIN"]).note("<object>/<embed>/<applet> are counted, excluding an <object> whose type declares an image (an SVG or raster fallback, not plugin content)"),
        Entry::check("Target Size", &["MOBILE_TARGET_TOO_SMALL"]),
        Entry::check("Mobile Alternate Link", &["MOBILE_ALTERNATE_LINK"]).note("only a media-qualified rel=alternate with no href is flagged; the reciprocal canonical on the mobile URL is not checked"),
    ]
}

/// Rows of the catalogue category `Analytics`.
pub(super) fn analytics() -> Vec<Entry> {
    vec![
        Entry::check("Orphan URLs", &["GA_ORPHAN_URL", "METRIKA_ORPHAN_URL"]).note("joined from complete project-bound GA4 or Metrika URL history; absence claims are withheld for incomplete source coverage or partial crawls"),
        Entry::check("Bounce Rate Above 70%", &["BOUNCE_RATE_ABOVE_70", "METRIKA_BOUNCE_RATE_ABOVE_70"]).note("measured from retained per-URL Metrika bounce rates; GA4 does not provide this metric in the project join"),
        Entry::check("No GA Data", &["NO_GA_DATA", "NO_METRIKA_DATA"]).note("absence is reported only for an explicitly bound property with complete retained coverage and a complete crawl"),
        Entry::check("Non-Indexable with GA Data", &["NON_INDEXABLE_WITH_GA_DATA", "NON_INDEXABLE_WITH_METRIKA_DATA"]).note("joined from retained GA4 sessions or Metrika visits"),
    ]
}

/// Rows of the catalogue category `Search Console`.
pub(super) fn search_console() -> Vec<Entry> {
    vec![
        Entry::out_of_scope("Page is Not Mobile Friendly").note("needs Search Console; see #97"),
        Entry::check("Orphan URLs", &["GSC_ORPHAN"])
            .note("requires complete retained Search Analytics coverage and crawl"),
        Entry::check(
            "URL is on Google, But Has Issues",
            &["GSC_INDEXED_WITH_ISSUES"],
        ),
        Entry::check("AMP URL is Invalid", &["GSC_AMP_INVALID"]),
        Entry::check("Rich Result is Invalid", &["GSC_RICH_RESULTS_INVALID"]),
        Entry::check("Indexable URL Not Indexed", &["GSC_INDEXABLE_NOT_INDEXED"]),
        Entry::check(
            "User-Declared Canonical Not Selected",
            &["GSC_USER_CANONICAL_NOT_SELECTED"],
        ),
        Entry::check("No Search Analytics Data", &["NO_SEARCH_DATA"])
            .note("requires complete retained Search Analytics coverage and crawl"),
        Entry::check(
            "Non-Indexable with Search Analytics Data",
            &["NON_INDEXABLE_WITH_SEARCH_DATA"],
        ),
        Entry::check("URL Is Not on Google", &["GSC_URL_NOT_ON_GOOGLE"]),
    ]
}

/// Rows of the catalogue category `Validation`.
pub(super) fn validation() -> Vec<Entry> {
    vec![
        Entry::check("Missing <head> Tag", &["HEAD_MISSING"]),
        Entry::check("Multiple <head> Tags", &["HEAD_MULTIPLE"]),
        Entry::check("Missing <body> Tag", &["BODY_MISSING"]),
        Entry::check("Multiple <body> Tags", &["BODY_MULTIPLE"]),
        Entry::check("HTML Document Over 2MB", &["HTML_OVER_2MB"]),
        Entry::check("Resource Over 2MB", &["RESOURCE_OVER_2MB"]).note("captured declared-resource bytes; partial captures retain lower-bound status"),
        Entry::check("Invalid HTML Elements In <head>", &["INVALID_HEAD_ELEMENT"]),
        Entry::check("<body> Element Preceding <html>", &["HEAD_NOT_FIRST"]),
        Entry::check("<head> Not First In <html> Element", &["HEAD_NOT_FIRST"]),
        Entry::out_of_scope("High Carbon Rating").note("a derived score over transfer weight; the weight itself is already reported and the rating adds a model, not a measurement"),
    ]
}
