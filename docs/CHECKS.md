# Check catalogue

Generated from `seohead/sf/core/registry.py` — do not edit by hand. Regenerate with:

```bash
python scripts/generate_checks_reference.py
```

**281 checks.** Severity, evidence and fix all come from the same `CHECKS` dict the rule engine reads, so this table cannot say something the engine disagrees with.

- **Fires on** — what the check id means, in the registry's own words.
- **Evidence** — the `source` tag: which export or module has to be present for the check to run at all; its absence is why a check comes back `skipped` instead of a silent pass.
- **Fix** — the remedy the audit ships next to the finding.

## 7.A — indexing & response codes

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `BROKEN_PAGE_4XX` | critical | SF:Response Codes:4xx | Page returns a 4xx response (broken page) | Restore the page or redirect it with a 301 to a relevant URL; remove or update links that point to it. |
| `SERVER_ERROR_5XX` | critical | SF:Response Codes:5xx | Page returns a 5xx response (server error) | Investigate the server or application; this error makes the page unavailable to users and crawlers. |
| `NO_RESPONSE` | critical | SF:Response Codes:No Response | No response (timeout, DNS, or connection failure) | Check host availability, DNS resolution, connectivity, and timeout settings. |
| `BLOCKED_BY_ROBOTS` | warning | SF:Response Codes:Blocked by Robots.txt | URL is blocked by robots.txt | Confirm that the block is intentional; pages intended for indexing should remain crawlable. |
| `NON_INDEXABLE_LINKED` | notice | SF-derived | Internally linked page is non-indexable | Decide whether the page should be indexable; otherwise remove unnecessary internal links and account for crawl-budget impact. |
| `IMPORTANT_URL_BLOCKED_BY_ROBOTS` | warning | SF-derived | Live page is blocked by robots.txt despite receiving internal links | robots.txt blocks crawling, not indexing, so link discovery is lost. Make the URL crawlable with a more specific Allow rule than the matching Disallow, and control indexing with canonical or noindex. A common case is pagination such as /blog?page=N blocked by Disallow: /*?. |
| `ROBOTS_BLOCKS_RESOURCES` | notice | sitemap | robots.txt blocks JavaScript or CSS resources required for rendering | Do not block .js, .css, or _next/static resources in robots.txt; otherwise Google may render the page incompletely. |
| `URL_INTERNAL_SEARCH` | notice | SF-derived | URL is a recognized internal search route | Review whether internal search results should be linked or indexable; apply the intended crawl and indexing policy. |
| `INTERNAL_LINK_NO_ANCHOR` | warning | crawl:retained_links | Internal hyperlink has empty anchor text | Add descriptive text to the internal link, or provide an accessible label for an intentional image or icon link. |
| `EXTERNAL_NO_RESPONSE` | warning | crawl:external_checks | An explicitly checked external destination did not answer | Review the recorded timeout or connection failure and update or remove the link after confirming the destination is unavailable. |
| `SITEMAP_URL_NON_CANONICAL` | warning | crawl:sitemap_membership | Sitemap URL declares another canonical URL | List the canonical destination in the sitemap and remove the non-canonical URL. |
| `SITEMAP_URL_BLOCKED_ROBOTS` | warning | crawl:robots_policy | Sitemap URL is blocked by the retained robots policy | Remove the intentionally blocked URL from the sitemap or allow crawling if the URL should be indexed. |
| `INTERNALLY_LINKED_ROBOTS_BLOCKED` | warning | crawl:robots_policy | An internally linked URL is blocked by robots.txt | Review the observed internal links and the blocking rule; allow crawling when this page should be discovered and indexed. |

## 7.B — links & DOM localization

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `BROKEN_INTERNAL_LINK` | critical | inlinks:Client Error (4xx) Inlinks | Internal link points to a 4xx URL | Update the link to the current URL or add an appropriate 301 redirect; if it appears in the footer or navigation, fix the shared template. |
| `LINK_TO_5XX` | critical | inlinks:Server Error (5xx) Inlinks | Internal link points to a 5xx URL | Repair the destination page or remove the link. |
| `INTERNAL_LINK_TO_REDIRECT` | warning | inlinks:Redirection (3xx) Inlinks | Internal link points to a redirect (3xx) | Point the link directly to the final URL to eliminate the unnecessary redirect hop. |
| `BROKEN_EXTERNAL_LINK` | warning | inlinks:Client Error (4xx) Inlinks | External link points to a 4xx or 5xx URL | Update or remove the broken external link, while accounting for sites that intentionally return 403 responses to crawlers. |
| `EXTERNAL_LINK_TO_REDIRECT` | notice | inlinks:Redirection (3xx) Inlinks | External link points to a redirect (3xx) | This is often acceptable for external sites; optionally update the link to point directly to the final URL. |
| `REDIRECT_CHAIN` | warning | SF:report Redirect Chains | Redirect chain contains two or more hops | Replace the chain with a single 301 redirect to the final URL. |
| `REDIRECT_LOOP` | critical | SF:report Redirect Chains | Redirect loop detected | Break the redirect cycle so every redirect path terminates at a valid destination. |
| `BAD_REDIRECT_TYPE` | notice | SF-derived | Temporary redirect (302, 303 or 307) used where a permanent redirect is expected | Use a 301 redirect when the move is permanent. |

## 7.C — title & meta description

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `TITLE_MISSING` | critical | SF-derived | Title element is missing | Add a unique, descriptive title element. |
| `TITLE_DUPLICATE` | warning | SF-derived | Duplicate title element | Give each page a unique title element. |
| `TITLE_MULTIPLE` | warning | SF:Page Titles:Multiple | Multiple <title> elements | Keep exactly one <title> element in the document head. |
| `TITLE_TOO_LONG` | notice | SF-derived | Title exceeds the configured length threshold | Shorten the title to fit the configured character or pixel-width limit. |
| `TITLE_TOO_SHORT` | notice | SF-derived | Title falls below the configured length threshold | Expand the title to an informative length without padding it with boilerplate. |
| `TITLE_EQUALS_H1` | notice | SF-derived | Title is identical to the H1 | Differentiate the title and H1 by purpose, wording, or keyword emphasis. |
| `DESC_MISSING` | warning | SF-derived | Meta description is missing | Add a useful meta description, typically up to about 160 characters. |
| `DESC_MULTIPLE` | warning | crawl:meta_description_count | More than one <meta name="description"> element is present | Keep exactly one meta description element; a search engine reads only the first, so the rest are dead weight that hides which value is actually live. |
| `DESC_DUPLICATE` | warning | SF-derived | Duplicate meta description | Write a unique meta description for each page. |
| `DESC_TOO_LONG` | notice | SF-derived | Meta description exceeds the configured length threshold | Shorten the description while preserving its primary value proposition. |
| `DESC_TOO_SHORT` | notice | SF-derived | Meta description falls below the configured length threshold | Expand the description with specific, useful page information. |

## 7.D — headings (incl. multiple H1)

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `H1_MISSING` | warning | SF-derived | H1 heading is missing | Add one H1 that clearly states the page topic. |
| `H1_MULTIPLE` | warning | SF:H1:Multiple | Multiple H1 headings on the page | Keep one primary H1 and demote the remaining headings to H2 or H3 as appropriate. |
| `H1_DUPLICATE` | notice | SF-derived | H1 is duplicated across multiple URLs | Use a unique, page-specific H1 on each URL. |
| `H1_TOO_LONG` | notice | SF-derived | H1 exceeds the configured length threshold | Shorten the H1 while retaining the page's main topic. |
| `H1_ALT_TEXT_ONLY` | notice | crawl:h1_alt_text | The H1 has no text of its own; its only content is an image's alt attribute | Add real, visible text to the H1 -- alt text describes an image to assistive technology, it is not a substitute for a heading a search engine reads as text. |
| `H2_MISSING` | notice | SF-derived | Page has an H1 but no H2 headings | Add meaningful H2 subheadings where needed to structure the content. |
| `H2_DUPLICATE` | notice | SF-derived | H2 is duplicated across multiple URLs | Use a unique, page-specific H2 on each URL, or accept it for a shared boilerplate subheading that is genuinely meant to repeat. |
| `H2_TOO_LONG` | notice | SF-derived | H2 exceeds the configured length threshold | Shorten the H2 while retaining what it introduces. |
| `HEADING_BEFORE_H1` | notice | crawl:heading_outline | One or more headings appear before the page's first H1 in DOM order | Move the H1 above the headings that precede it, or demote those headings to plain text or a non-heading element. A parser reading the outline meets them before it meets the page's subject, and a snippet built from early page text quotes them instead of the page. |
| `HEADING_IN_PAGE_CHROME` | notice | crawl:heading_outline | A heading sits in the page chrome (header, nav, sidebar or footer) rather than in the content | Mark template furniture up as what it is -- a menu label or a widget title is not a heading of this document, and repeating it on every page pushes the page's own headings down its outline. |
| `HEADING_SKIP` | notice | crawl:heading_outline | Content headings change by more than one level in DOM order, skipping a level down or jumping back up past an open section | Use the adjacent heading level in both directions: an H4 that follows an H2 becomes H3, and a heading returning up the outline steps back one level at a time so each open section closes in order (H3 -> H2 closes one section and starts a sibling, which is correct nesting). |

## 7.E — links the crawler could not follow at all

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `INVALID_LINK_TARGET` | notice | crawl:invalid_link_targets | The page points at hrefs that cannot become fetchable link targets (unparsable syntax or a non-http(s) scheme) | Review each quoted href as written: a truncated or malformed URL passes no crawl equity and often signals a broken template; a non-http(s) scheme is only a finding when the link was meant to be a page. Recording is opt-in (discovery.record_invalid_targets); rejected targets never enter the crawl. |

## 7.E — canonical & directives

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `CANONICAL_MISSING` | warning | SF-derived | Indexable page has no canonical URL | Add a valid <link rel="canonical"> element. |
| `CANONICALISED` | notice | SF-derived | Canonical points to a different URL | Confirm that cross-canonicalization is intentional and that the target is the preferred version. |
| `PAGINATION_CANONICAL_POLICY` | warning | SF-derived | Paginated URL canonical does not match the configured project policy | Review the canonical against the configured pagination policy; the expected target is included in the evidence. |
| `FILTER_CANONICAL_POLICY` | warning | SF-derived | Filtered URL canonical does not match the configured project policy | Review the canonical against the configured filter policy; the expected target is included in the evidence. |
| `CANONICAL_NON_INDEXABLE` | warning | SF-derived | Canonical points to a non-indexable URL | Point the canonical to an indexable preferred version. |
| `NOINDEX` | notice | SF:Directives:Noindex | Page contains a noindex directive | Confirm that exclusion from indexing is intentional. |
| `NOFOLLOW_PAGE` | notice | SF:Directives:Nofollow | Page-level nofollow directive is present | Confirm the directive is intentional and review its effect on crawling and internal link equity. |
| `META_KEYWORDS_PRESENT` | notice | SF-derived | Obsolete meta keywords element is present | Remove it if desired; modern search engines ignore meta keywords. |

## 7.F — content: thin & duplicates

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `THIN_CONTENT` | warning | SF-derived | Thin content (low word count) | Add substantial, useful content or exclude the page from indexing when it has no standalone search value. |
| `CONTENT_IN_IFRAME` | warning | crawl:content_frames | The page's content sits inside an iframe and is not attributed to this URL | Serve the framed copy in the page's own HTML. A search engine attributes an iframe's text to the framed document, not to the page that frames it, so the copy exists and earns this URL nothing. |
| `LOW_TEXT_RATIO` | notice | SF-derived | Low text-to-HTML ratio | Increase the proportion of meaningful visible content or reduce unnecessary markup. |
| `DUPLICATE_BY_HASH` | warning | SF-derived | Exact duplicate content (identical hash) | Consolidate duplicates with canonicalization or rewrite them to serve distinct search intent. |
| `NEAR_DUPLICATE` | warning | SF-derived | Near-duplicate content | Differentiate the pages with substantive content or consolidate them into one canonical page. |
| `SEMANTIC_SIMILARITY_CANDIDATE` | notice | heuristic:explicit_embedding_adapter | Pages have adapter-measured topical similarity; review candidate | Review intent and useful internal linking. Semantic similarity alone does not prove duplicate content or cannibalization. |
| `TITLE_LOW_SEMANTIC_RELEVANCE` | notice | heuristic:explicit_embedding_adapter | Title has low adapter-measured similarity to the page content; review candidate | Review whether the title describes the page's actual content and intent; the model score alone is not proof of irrelevance. |
| `H1_LOW_SEMANTIC_RELEVANCE` | notice | heuristic:explicit_embedding_adapter | H1 has low adapter-measured similarity to the page content; review candidate | Review whether the heading describes the page's actual content and intent; the model score alone is not proof of irrelevance. |
| `LOREM_IPSUM_PLACEHOLDER` | warning | crawl:lorem_ipsum_count | The Lorem Ipsum placeholder passage appears in the page's own content area | Replace the placeholder passage with real, reviewed copy before the page goes live or stays indexed. |

## 7.G — images

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `IMG_MISSING_ALT` | warning | SF+native:retained-image-markup | Image is missing alt text | Add concise, descriptive alt text when the image conveys content; use an empty alt attribute for decorative images. |
| `IMG_MISSING_ALT_ATTRIBUTE` | warning | crawl:images | An <img> has no alt attribute at all (not even alt="") | Declare descriptive alt text for informative images, or alt="" for decoration. An ARIA name can label an image for assistive technology, but does not supply the missing alt attribute. |
| `IMG_ALT_TOO_LONG` | notice | crawl:images | An image's alt text exceeds the configured length threshold | Shorten the alt text to a concise description; screen readers read the whole string aloud, and a search engine treats an overlong alt as a weaker signal. |

## 7.H — schema, hreflang, viewport

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `SCHEMA_VALIDATION_ERROR` | warning | SF:Structured Data:Validation Errors | Structured data validation errors | Correct invalid JSON-LD or Microdata markup and retest it against the applicable vocabulary and rich-result requirements. |
| `STRUCTURED_DATA_PARSE_ERROR` | warning | crawl:jsonld_blocks | A JSON-LD block is present but did not parse as valid JSON | Fix the malformed JSON-LD block (a stray comma, an unescaped quote, or an unclosed brace commonly voids the whole block); a search engine ignores structured data it cannot parse the same as if none were present. |
| `HREFLANG_ERROR` | warning | SF:Hreflang | Hreflang implementation error | Ensure hreflang annotations are reciprocal and reference canonical URLs. |

## 7.I — URL hygiene & performance

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `URL_TOO_LONG` | notice | SF-derived | URL exceeds the configured length threshold | Shorten the URL while preserving a stable, descriptive path. |
| `URL_HAS_PARAMS` | notice | SF-derived | Parameterized URL has no canonical | Point the canonical to the preferred parameter-free URL when the parameters do not create unique indexable content. |
| `URL_HAS_PARAMETERS` | notice | SF-derived | Internal URL contains query parameters | Review whether the parameters create unique indexable content; keep internal links and sitemaps on the parameter-free URL where they do not. |
| `URL_NON_ASCII` | notice | SF-derived | URL contains non-ASCII characters | Consider a consistent ASCII transliteration for human-readable URLs where appropriate. |
| `URL_UPPERCASE` | notice | SF-derived | URL path contains uppercase characters | Normalize the path to lowercase and add a 301 redirect from the uppercase variant. |
| `DEEP_CRAWL_DEPTH` | warning | SF-derived | Page has excessive crawl depth | Use relevant internal links to make the page reachable in fewer clicks from the home page or an authoritative hub. |
| `ORPHAN_PAGE` | warning | SF-derived | Orphan page has no internal inlinks | Add relevant internal links so users and crawlers can discover the page. |
| `SLOW_RESPONSE` | warning | SF-derived | Slow server response | Improve TTFB by profiling the application and origin, then optimizing caching and infrastructure. |
| `LARGE_HTML` | warning | SF-derived+heuristic | HTML document is large in absolute terms or relative to the site | Reduce HTML size by removing unnecessary markup, extracting inline styles or scripts, and avoiding embedded base64 assets. |
| `HTML_OVER_2MB` | warning | SF-derived+heuristic | HTML document is larger than 2 MB | Cut the document below 2 MB: remove inline data URIs and base64 assets, move inline CSS and scripts into cached files, and trim repeated markup. |

## 7.J — security

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `HTTP_URL` | warning | SF-derived | URL uses HTTP instead of HTTPS | Serve the URL over HTTPS and redirect the HTTP version with a 301. |

## 7.K — sitemap & robots

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `SITEMAP_NOT_IN_ROBOTS` | notice | sitemap | robots.txt does not declare a Sitemap directive | Add a Sitemap directive to robots.txt with the absolute sitemap URL. |
| `SITEMAP_URL_3XX` | warning | sitemap | Sitemap URL returns a 3xx response | List the final 200-status URL in the sitemap instead of a redirecting URL. |
| `SITEMAP_URL_4XX_5XX` | critical | sitemap | Sitemap URL returns a 4xx or 5xx response | Remove broken URLs from the sitemap or restore them before listing them again. |
| `SITEMAP_URL_NON_INDEXABLE` | warning | sitemap | Sitemap contains a non-indexable URL | Keep only canonical, indexable URLs in the sitemap. |
| `SITEMAP_ORPHAN` | warning | sitemap | Sitemap URL has no internal inlinks | Add relevant internal links to the page or remove it from the sitemap if it should not be discoverable. |
| `URL_NOT_IN_SITEMAP` | notice | sitemap | Indexable page is missing from the sitemap | Add the canonical page URL to the appropriate sitemap. |
| `SITEMAP_STALE_LASTMOD` | notice | sitemap | Sitemap contains stale or boilerplate lastmod values | Generate each lastmod value from the page's actual meaningful modification date. |
| `SITEMAP_TOO_MANY_URLS` | warning | sitemap | Sitemap declares more URLs than the protocol allows | Split the sitemap into files of at most 50,000 URLs each and list them in a sitemap index. Over the limit the file is invalid and a search engine may read part of it and discard the rest without reporting anything. |
| `SITEMAP_TOO_LARGE` | warning | sitemap | Sitemap exceeds the protocol's uncompressed size limit | Split the sitemap so each file stays under 50 MB uncompressed. The limit is about the document a search engine parses, so compressing it does not help. |
| `SITEMAP_URL_DUPLICATED` | notice | sitemap | URL is declared in more than one sitemap | Declare each URL in exactly one sitemap. A URL in two files is usually a generator that ran twice, and it distorts every count taken from the declared set. |
| `SITEMAP_DESYNC` | warning | sitemap | Sitemap and crawl URL sets are out of sync | Synchronize the sitemap with the site's actual set of canonical, indexable pages. |
| `SITEMAP_FETCH_INCOMPLETE` | notice | sitemap | Some child sitemaps could not be fetched or parsed | Check that every child sitemap is reachable, retry in case the service was temporarily slow, and validate the XML -- a 200 response with malformed markup (e.g. an unescaped '&' in a URL) fails here the same way a network error does. |

## 8.x — heuristics beyond SF

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `HTML_BLOAT` | notice | heuristic | HTML bloat: high document size relative to text content | Reduce bytes per word by extracting styles and scripts, removing embedded base64 assets, and simplifying markup. |
| `DOM_TOO_DEEP` | notice | heuristic | DOM nesting is too deep | Simplify the layout hierarchy and remove unnecessary wrapper elements. |
| `DOM_TOO_MANY_NODES` | notice | heuristic | DOM contains too many nodes | Reduce the number of page elements and avoid rendering unnecessary or duplicated components. |
| `TITLE_TEMPLATED` | notice | heuristic | Templated titles share a common prefix or suffix across most pages | Make the page-specific portion of each title distinctive; a shared brand suffix is acceptable, but duplicated core title text is not. |

## --- extension: URL hygiene ---

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `URL_UNDERSCORES` | notice | SF:URL:Underscores | URL contains underscores | Use hyphens instead of underscores in URL path segments. |
| `URL_MULTIPLE_SLASHES` | notice | SF:URL:Multiple Slashes | URL path contains repeated slashes | Remove duplicate slashes and 301-redirect the malformed variant to the canonical path. |
| `URL_CONTAINS_SPACE` | warning | SF:URL:Contains Space | URL contains a space | Encode literal spaces in links; do not mechanically remove percent-encoded values such as q=red%20shoes. Change a canonical path only after verifying equivalent content and planning redirects or canonicals. |
| `URL_REPETITIVE_PATH` | notice | SF:URL:Repetitive Path | URL path contains a repeated segment | Simplify the URL structure so path segments are not duplicated. |
| `URL_TRACKING_PARAMS` | warning | SF-derived | Indexable URL contains a tracking parameter such as utm_, gclid, or fbclid | Remove tracking parameters from public links; for parameterized URLs that still receive traffic, add a self-referencing canonical or manage crawling through robots.txt and Search Console as appropriate. |

## --- extension: content and readability ---

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `READABILITY_DIFFICULT` | notice | SF-derived | Text is difficult to read (low Flesch score) | Use clearer wording and shorter sentences while preserving technical accuracy. |
| `LONG_SENTENCES` | notice | SF-derived | Average sentence length is too high | Break long sentences into shorter, focused statements. |
| `SPELLING_ERRORS` | notice | SF:Content:Spelling Errors | Spelling errors detected | Review and correct spelling errors, accounting for valid product names and specialist terminology. |
| `GRAMMAR_ERRORS` | notice | SF:Content:Grammar Errors | Grammar errors detected | Review and correct the flagged grammar issues in their full sentence context. |

## --- extension: robots directives ---

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `NOARCHIVE` | notice | SF:Directives:NoArchive | Page contains a noarchive directive | Confirm that preventing cached search-result copies is intentional. |
| `NOSNIPPET` | notice | SF:Directives:NoSnippet | Page contains a nosnippet directive | Confirm that suppressing the page's search-result snippet is intentional. |
| `NOIMAGEINDEX` | notice | SF:Directives:NoImageIndex | Page contains a noimageindex directive | Confirm that preventing images on this page from being indexed is intentional. |
| `META_REFRESH_REDIRECT` | warning | SF:Directives:Refresh | Redirect is implemented with meta refresh | Replace meta refresh with a server-side 301 redirect when the move is permanent. |
| `HTTP_REFRESH_REDIRECT` | warning | crawl:http_refresh | Redirect is implemented with an HTTP Refresh response header | Replace it with a server-side 301/302 redirect (Location header); a search engine treats Refresh the same as a meta refresh -- an unreliable, delayed signal compared to a real HTTP redirect status code. |
| `NOTRANSLATE` | notice | SF-derived | Page contains a notranslate directive | Confirm that opting out of translation-related Google Search features is intentional. |
| `NOODP` | notice | SF-derived | Page contains a legacy noodp directive | Remove the retired noodp directive; the Open Directory Project closed in 2017 and the directive has no effect. |
| `NOYDIR` | notice | SF-derived | Page contains a legacy noydir directive | Remove the retired noydir directive; the Yahoo Directory closed and the directive has no effect. |
| `UNAVAILABLE_AFTER` | warning | SF-derived | Page carries an unavailable_after directive with a deindex date | Confirm the date is intentional and in the future; once it passes, the page is removed from the index automatically. |

## --- extension: canonicals ---

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `CANONICAL_RELATIVE` | notice | SF:Canonicals:Canonical Is Relative | Canonical URL is relative | Use an absolute URL in the canonical element. |
| `CANONICAL_MULTIPLE` | warning | SF:Canonicals:Multiple | Page declares multiple canonical URLs | Declare exactly one canonical URL for the page. |
| `CANONICAL_FRAGMENT` | notice | SF-derived | Canonical URL contains a #fragment | Drop the fragment; the server never receives it, so a canonical pointing at one is meaningless. |

## --- extension: canonical graph (Mode B, built from Internal:All) ---

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `CANONICAL_INVALID_ATTRIBUTE` | warning | crawl:head_links | Canonical annotation contains unsupported alternate-version attributes or malformed rel/href | Remove hreflang, lang, media and type from the canonical link; declare alternates separately. Use an HTML whitespace-separated canonical relation and a non-empty HTTP(S) href. Raw attributes are included in the evidence. |
| `CANONICAL_CROSS_ORIGIN` | notice | SF-derived | Canonical changes the declaring URL host or protocol | Confirm that cross-host or protocol consolidation is intentional and that the target is the preferred URL. |
| `CANONICAL_CHAIN` | warning | SF-derived | Canonical chain: the target canonicalizes to another URL (two or more steps) | Point the canonical directly to the final canonical URL in one step and break any canonical loops. |
| `CANONICAL_TO_REDIRECT` | warning | SF-derived | Canonical points to a redirecting URL (3xx) | Point the canonical to the final 200-status URL; otherwise search engines must resolve conflicting canonical signals. |
| `CANONICAL_TARGET_ERROR` | warning | SF-derived | Canonical points to a URL returning 4xx or 5xx | Point the canonical to a fetched, successful URL; restore the target or redirect it to the intended canonical page. |
| `CANONICAL_HOMEPAGE_GROUP` | warning | SF-derived | Distinct indexable pages in multiple sections canonicalize to the homepage | Review each listed page's canonical. Use a self-canonical for distinct indexable content, or configure an explicit pagination/filter policy when a landing canonical is intentional. |
| `UNLINKED_CANONICAL` | warning | SF-derived | Canonical target has no hyperlink pointing to it anywhere in the crawl | Add an ordinary internal link to the canonical target, or confirm relying on the canonical alone for discovery is intentional. |
| `HREFLANG_UNLINKED_TARGET` | warning | SF-derived | Hreflang alternate target has no hyperlink pointing to it anywhere in the crawl | Add an ordinary internal link to the hreflang alternate, or confirm relying on the hreflang annotation alone for discovery is intentional. |
| `HREFLANG_BROKEN_TARGET` | warning | inlinks:All Hreflang | Hreflang points to a redirecting or broken URL (3xx, 4xx, or 5xx) | Update hreflang to reference the final 200-status URL; redirecting or broken targets undermine localization signals and crawling. |
| `HREFLANG_NOINDEX_TARGET` | warning | crawl:hreflang graph / SF:All Hreflang + Internal:All | Hreflang alternate points to an observed noindex page | Use an indexable alternate URL or remove the noindex directive if that page should appear in search. |
| `HREFLANG_INVALID_CODE` | warning | inlinks:All Hreflang | Hreflang value is not a valid ISO 639-1 language / ISO 3166-1 region code | Use a valid language code, optionally followed by a valid region (e.g. en-GB, not en-UK). |
| `HREFLANG_MULTIPLE_ENTRIES` | warning | inlinks:All Hreflang | The same hreflang value is declared more than once on the page | Declare each language/region combination exactly once; conflicting duplicates make the annotation ambiguous. |
| `HREFLANG_MISSING_SELF_REFERENCE` | warning | inlinks:All Hreflang | Page declares hreflang alternates but does not reference itself | Every page in an hreflang set must include a self-referencing annotation for its own URL and language. |
| `HREFLANG_MISSING_XDEFAULT` | notice | inlinks:All Hreflang | Hreflang set has no x-default fallback | Add an x-default annotation to catch users whose language/region does not match any declared alternate. |
| `HREFLANG_NOT_CANONICAL` | warning | inlinks:All Hreflang | Hreflang points to a URL that is not itself the canonical version | Point hreflang annotations at each target's canonical URL, not at a duplicate that canonicalizes elsewhere. |
| `HREFLANG_MISSING_RETURN_LINK` | warning | inlinks:All Hreflang | Another page's hreflang points here, but this page does not point back | Add a reciprocal hreflang annotation back to every page that names this one. |
| `HREFLANG_INCONSISTENT_CONFIRMATION` | warning | inlinks:All Hreflang | This page declares a counterpart under a language and region code the counterpart does not confirm for itself | Make the two declarations agree: either correct this page's hreflang value for that URL, or correct the counterpart's own self-referencing hreflang. Google reads a pair as valid only when both sides name the same code, so a mismatched pair is discarded exactly as a missing return link is. |

## --- extension: pagination ---

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `PAGINATION_NONINDEXABLE` | warning | SF-derived | Pagination page is non-indexable | Pagination pages should generally remain crawlable and indexable unless a deliberate alternative architecture is in place. |
| `PAGINATION_LOOP` | warning | SF-derived | A rel="next" pagination series loops back on itself | Fix the rel="next"/rel="prev" values so the series terminates instead of cycling. |
| `UNLINKED_PAGINATION_SERIES` | warning | SF-derived | A pagination series is reachable only by following rel="next", never by a hyperlink | Add an ordinary internal link to the first page of the series so it does not depend on rel="next" alone for discovery. |
| `PAGINATION_MULTIPLE` | warning | SF-derived | The page declares more than one rel="next" (or more than one rel="prev") URL | Declare exactly one rel="next" and one rel="prev" URL per page; two different successors leave the series ambiguous, and a crawler picks one of them without telling anybody which. |
| `PAGINATION_URL_NOT_IN_ANCHOR` | notice | inlinks:All Inlinks | A rel="next"/rel="prev" URL is not also linked from the same page with an anchor | Link the paginated URL from the page itself with an ordinary <a href> as well. Google stopped using rel="next"/rel="prev" for indexing in 2019, so an annotation with no anchor beside it is a route only some crawlers still take. |
| `PAGINATION_SEQUENCE_ERROR` | warning | SF-derived | Pagination breaks an established numbered run or contradicts a reciprocal next/prev declaration | Point each rel="next" at the page that actually follows this one. A run that goes 1, 2, 3, 7 leaves pages 4 to 6 out of the chain entirely, so a crawler following it never reaches them. |

## --- extension: links ---

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `NO_INTERNAL_OUTLINKS` | warning | SF:Links:Pages Without Internal Outlinks | Dead-end page has no internal outlinks | Add relevant internal links to help users and crawlers continue through the site. |
| `HIGH_EXTERNAL_OUTLINKS` | notice | SF:Links:Pages With High External Outlinks | Page has a high number of external outlinks | Review the links for editorial relevance, spam, and unnecessary dilution of page focus. |
| `HIGH_OUTLINKS` | notice | SF:Links:Pages With High Outlinks | Page has an excessive number of outlinks | Reduce unnecessary links to preserve clear navigation and crawl focus. |
| `GENERIC_ANCHOR_TEXT` | notice | inlinks:Anchor Text | Non-descriptive anchor text such as 'here', 'read more', or 'click here' | Replace it with meaningful anchor text that describes the destination for both search engines and screen-reader users. |
| `LOW_LINK_SCORE` | notice | inlinks:All Inlinks | Internal link score is far below the site median | Add internal, followed links to the page from well-linked pages elsewhere on the site. |
| `ONLY_NOFOLLOW_INLINKS` | warning | inlinks:All Inlinks | Every internal link to this page is nofollow | Add at least one ordinary, followed internal link so link equity and crawl priority reach the page. |
| `ONLY_NONINDEXABLE_SOURCE_INLINKS` | warning | inlinks:All Inlinks | Every internal link to this page comes from a non-indexable source | Link to the page from at least one indexable page so it is reachable from the part of the site search engines actually rank. |
| `DEEP_DISCOVERY_PATH` | notice | inlinks:All Inlinks | The shortest hyperlink route from the start page exceeds the configured depth | Add a shorter internal-linking route (e.g. from a hub or category page) so the page is reachable in fewer clicks. |
| `DEEP_CLICK_DEPTH` | warning | inlinks:All Inlinks | The page is more clicks from the crawl's start URL than the configured floor | Link the page from a hub, category or related page nearer the start URL. A page linked only by a chain of next-post links is reachable and still effectively invisible, which no orphan count reports. |
| `DUPLICATE_INTERNAL_LINK` | notice | inlinks:All Inlinks | The page repeats the same link -- same destination, same anchor text -- more than once | Emit the block once. A layout element rendered twice (one copy hidden by CSS or removed by JavaScript) duplicates every link it contains, which inflates the internal link graph without adding a single new route. |
| `INSECURE_SUBRESOURCE` | warning | inlinks:All Inlinks | An HTTPS page loads a resource (image, script, stylesheet, ...) over plain HTTP | Serve every page resource over HTTPS and update its URL accordingly. |

## --- extension: technical checks ---

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `HTTP1_ONLY` | notice | SF-derived | Response uses HTTP/1.x rather than HTTP/2 or newer | Enable HTTP/2 or HTTP/3 on the origin server or CDN where supported. |
| `AMPHTML_PRESENT` | notice | SF-derived | AMP version is declared | Confirm that the AMP version is still required, current, valid, and canonically linked. |

## --- AMP pairing, read from the AMP target a desktop page declares (issue #1020) ---

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `AMP_NON_200` | warning | crawl:amp_pairing | The AMP page declared by rel=amphtml does not answer 200 | Serve the AMP URL with 200, or remove the rel=amphtml declaration that names it. |
| `AMP_MISSING_CANONICAL` | warning | crawl:amp_pairing | The AMP page declares no rel=canonical | Add a rel=canonical on the AMP page that points to the non-AMP version. |
| `AMP_MISSING_RETURN_LINK` | warning | crawl:amp_pairing | The AMP page's rel=canonical does not point back to the page that declares it | Point the AMP page's rel=canonical at the non-AMP URL that declares rel=amphtml. |
| `AMP_NON_INDEXABLE_CANONICAL` | warning | crawl:amp_pairing | The AMP page's canonical target is not indexable | Make the canonical target indexable, or correct the AMP page's rel=canonical. |
| `AMP_INDEXABLE` | notice | crawl:amp_pairing | The AMP page is indexable | Confirm the AMP page should compete in search; if the non-AMP page is canonical, the AMP page usually should not be. |

## snapshot of every id Lighthouse actually defines.

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `MISSING_CHARSET` | warning | SF-derived | No character encoding declared via Content-Type or an early <meta> tag | Declare charset in the Content-Type response header, or add a <meta charset> tag in the first 1024 bytes of the HTML. |
| `MISSING_DOCTYPE` | notice | SF-derived | Document lacks a modern <!DOCTYPE html> declaration, triggering quirks mode | Add `<!DOCTYPE html>` as the very first line of the document, with no PUBLIC or SYSTEM identifier. |
| `VIEWPORT_MISSING` | warning | SF-derived | No <meta name=viewport> tag with width or an initial-scale of at least 1 | Add `<meta name="viewport" content="width=device-width, initial-scale=1">` to the document head. |
| `MOBILE_ALTERNATE_LINK` | warning | crawl:mobile_alternate | A <link rel=alternate media=...> declaration names no URL | Give the mobile alternate link an `href` pointing at the mobile URL, or remove the `<link rel="alternate" media=...>` tag. |
| `MOBILE_CONTENT_OVERFLOW` | warning | native rendered mobile layout | Rendered content extends beyond the mobile viewport width | Constrain the overflowing element or provide a mobile-width layout. |
| `MOBILE_TEXT_TOO_SMALL` | notice | native rendered mobile layout | Rendered text uses a computed font size below 12 CSS pixels | Use a readable mobile text size and verify the page at its target viewport. |
| `MOBILE_TARGET_TOO_SMALL` | notice | native rendered mobile layout | Rendered interactive target is smaller than 48 CSS pixels in at least one dimension | Increase the interactive target's hit area and verify spacing at the target viewport. |
| `NOINDEX_ONLY_IN_RAW_HTML` | warning | native raw/rendered element comparison | Noindex appears in the original HTML but not in the rendered DOM | Keep indexing directives consistent in the original response and rendered document. |
| `NOFOLLOW_ONLY_IN_RAW_HTML` | warning | native raw/rendered element comparison | Nofollow appears in the original HTML but not in the rendered DOM | Keep link-following directives consistent in the original response and rendered document. |
| `CANONICAL_MISMATCH` | warning | native raw/rendered element comparison | Canonical URL changes after JavaScript rendering | Declare one stable canonical URL in the original response. |
| `CANONICAL_ONLY_IN_RENDERED_HTML` | notice | native raw/rendered element comparison | Canonical URL is present only after JavaScript rendering | Declare the canonical URL in the original response. |
| `PAGE_TITLE_ONLY_IN_RENDERED_HTML` | notice | native raw/rendered element comparison | Page title is present only after JavaScript rendering | Serve the page title in the original HTML response. |
| `PAGE_TITLE_UPDATED_BY_JAVASCRIPT` | notice | native raw/rendered element comparison | Page title changes after JavaScript rendering | Keep the initial and rendered page title aligned. |
| `META_DESCRIPTION_ONLY_IN_RENDERED_HTML` | notice | native raw/rendered element comparison | Meta description is present only after JavaScript rendering | Serve the meta description in the original HTML response. |
| `META_DESCRIPTION_UPDATED_BY_JAVASCRIPT` | notice | native raw/rendered element comparison | Meta description changes after JavaScript rendering | Keep the initial and rendered meta description aligned. |
| `H1_ONLY_IN_RENDERED_HTML` | notice | native raw/rendered element comparison | H1 is present only after JavaScript rendering | Serve the primary page heading in the original HTML response. |
| `H1_UPDATED_BY_JAVASCRIPT` | notice | native raw/rendered element comparison | H1 changes after JavaScript rendering | Keep the initial and rendered primary page heading aligned. |
| `RENDER_RESOURCE_BLOCKED` | warning | native rendered network log | A rendered page resource was refused or returned an HTTP error | Review the recorded request reason and make required page resources available to the renderer. |
| `UNSUPPORTED_PLUGIN` | warning | crawl:plugin_elements | Page contains a legacy plugin-dependent element (<object>/<embed>/<applet>) | Replace the plugin-dependent element with a native equivalent (HTML5 <video>/<audio>, an <img>/<svg>, or a JavaScript-driven alternative) -- mobile browsers, and modern desktop ones, do not run plugins, so this content is simply invisible there. |
| `AJAX_CRAWLING_SCHEME_URL` | notice | crawl:ajax_scheme_outlinks | The deprecated AJAX crawling scheme (#! / _escaped_fragment_) is still used by this page's URL or by URLs it links to | Serve the same content at ordinary URLs and link to those instead. Google deprecated the scheme in 2015 and stopped supporting it in 2018, so an _escaped_fragment_ companion URL is no longer requested by anything -- informational rather than broken, because a site may still keep it for a legacy client of its own. |
| `AJAX_CRAWLING_SCHEME_META_FRAGMENT` | notice | crawl:meta_fragment | Page declares <meta name="fragment"> -- the page-wide opt-in to the deprecated AJAX crawling scheme | Remove the tag once the page is served as ordinary HTML (server-rendered or crawlable client-rendered). Nothing requests the _escaped_fragment_ companion URL it advertises any more, so the declaration is inert -- informational rather than broken. |
| `RENDER_BLOCKING` | notice | crawl:render_blocking | A <head> script or stylesheet blocks first paint (no async/defer, no non-matching media) | Add `defer` or `async` to the <script src>, move it out of <head>, or give the stylesheet a `media` value that the first render does not need. |
| `NO_COMPRESSION` | notice | SF-derived | HTML response is served uncompressed above the size where gzip/br would help | Enable gzip, Brotli, or deflate compression for text responses on the origin server or CDN. |

## --- extension: element position & document skeleton (issue #123) ---

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `TITLE_OUTSIDE_HEAD` | warning | SF-derived | The <title> element is outside <head> once the parser resolves the document | Move whatever precedes it in <head> — usually an element the head content model does not allow — so <title> is read from <head> again. |
| `DESC_OUTSIDE_HEAD` | warning | SF-derived | The meta description is outside <head> once the parser resolves the document | Move whatever precedes it in <head> — usually an element the head content model does not allow — so the description is read from <head> again. |
| `CANONICAL_OUTSIDE_HEAD` | critical | SF-derived | The canonical link is outside <head> once the parser resolves the document | Move whatever precedes it in <head> — usually an element the head content model does not allow — so the canonical is read from <head> again; Google ignores a canonical outside <head>. |
| `DIRECTIVES_OUTSIDE_HEAD` | critical | SF-derived | A robots-directive meta tag is outside <head> once the parser resolves the document | Move whatever precedes it in <head> — usually an element the head content model does not allow — so the directive is read from <head> again; a noindex/nofollow outside <head> does not apply. |
| `HREFLANG_OUTSIDE_HEAD` | warning | SF-derived | An hreflang alternate link is outside <head> once the parser resolves the document | Move whatever precedes it in <head> — usually an element the head content model does not allow — so the alternate is read from <head> again. |
| `HEAD_MISSING` | warning | SF-derived | Document has no <head> element | Add a <head> element; a browser inserts one implicitly, but every metadata tag then depends on exactly where it lands. |
| `HEAD_MULTIPLE` | notice | SF-derived | Document has more than one <head> element | Merge into a single <head>; a browser keeps both as siblings rather than combining their contents. |
| `BODY_MISSING` | warning | SF-derived | Document has no <body> element | Add a <body> element; a browser inserts one implicitly around the visible content. |
| `BODY_MULTIPLE` | notice | SF-derived | Document has more than one <body> element | Merge into a single <body>; a browser keeps both as siblings rather than combining their contents. |
| `INVALID_HEAD_ELEMENT` | notice | SF-derived | An element the head content model does not allow is written inside <head> | Move title/base/link/meta/style/script/noscript/template content into <head> and everything else into <body>; an invalid element is what forces the parser to close <head> early. |
| `HEAD_NOT_FIRST` | notice | SF-derived | <head> is not the first element under <html> once the parser resolves the document | Fix the markup order so <head> opens immediately after <html>, before any <body> content. |

## --- extension: export-dependent native filters (active when the export is available) ---

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `MIXED_CONTENT` | warning | SF:Security:Mixed Content | Mixed content: HTTPS page loads resources over HTTP | Serve every page resource over HTTPS and update its URL accordingly. |
| `MISSING_HSTS` | notice | SF:Security:Missing HSTS Header | HSTS header is missing | Add an appropriate Strict-Transport-Security header after confirming the entire site is HTTPS-ready. |
| `STRUCTURED_DATA_MISSING` | notice | SF:Structured Data:Missing | Structured data is missing | Add relevant, accurate Schema.org markup that reflects visible page content. |
| `OG_MISSING` | notice | SF:Social:Open Graph | og:title is missing, so social previews may not render correctly | Add og:title, og:image, and og:url; at minimum, provide og:title and og:image for a useful preview. |
| `IMG_BROKEN` | warning | crawl:images | Image URL answered with an HTTP error, so the browser shows nothing | Restore the file, or update the reference in the page template or CMS to a URL that answers 200. |
| `IMG_OVER_KB` | warning | SF:Images:Over X KB | Image exceeds the configured file-size threshold | Compress the image and consider converting it to WebP or AVIF while preserving acceptable visual quality. |
| `IMG_MISSING_DIMENSIONS` | notice | SF+native:retained-image-markup | Image does not declare positive width and height attributes | Declare intrinsic width and height values to reserve layout space and reduce CLS. |

## handler layer rather than through a registered export requirement.

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `INLINK_BOILERPLATE_ONLY` | warning | crawl:link_position | Page is linked only from navigation, header, sidebar, or footer, never from body content | Add a contextual link to the page from relevant body copy; a page reachable only through boilerplate is not linked the way a page in the content graph is. |

## so both skip by name on an export rather than run clean on nothing.

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `LINK_INSIDE_HEADING` | notice | crawl:link_placement | A heading on the page is, or contains, a link to somewhere else | Let the heading describe this page and put the link in the body copy beneath it. A heading that is a link stops naming the page's own subject and starts pointing away from it. |
| `IMAGE_LINK_WITHOUT_TEXT` | warning | crawl:link_placement | An image link carries no anchor text and no alt text, so nothing says where it goes | Give the image a descriptive alt attribute, or add anchor text beside it. Neither a reader using a screen reader nor a crawler can tell what this link points to. |

## nor a link's rel/target/raw-href.

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `UNSAFE_CROSS_ORIGIN_LINK` | warning | crawl:link_findings | A target="_blank" link declares neither rel="noopener" nor rel="noreferrer" | Add rel="noopener" (or "noreferrer") so the opened page cannot reach back into this one through window.opener. |
| `PROTOCOL_RELATIVE_LINK` | notice | crawl:link_findings | Link href is written in the protocol-relative "//host/path" form | Write an explicit https:// href; a protocol-relative one silently follows whatever scheme served the current page, including a plain-HTTP embed. |
| `MISSING_CSP` | notice | crawl:security_headers | HTML page has no Content-Security-Policy header | Send a Content-Security-Policy header that names the script and style sources the page uses. |
| `MISSING_X_CONTENT_TYPE_OPTIONS` | notice | crawl:security_headers | HTML page has no X-Content-Type-Options header | Send X-Content-Type-Options: nosniff so browsers do not sniff the response into another type. |
| `MISSING_X_FRAME_OPTIONS` | notice | crawl:security_headers | HTML page has neither X-Frame-Options nor a CSP frame-ancestors directive | Send X-Frame-Options: SAMEORIGIN or a Content-Security-Policy frame-ancestors directive. |
| `MISSING_REFERRER_POLICY` | notice | crawl:security_headers | HTML page has no Referrer-Policy header | Send a Referrer-Policy header, for example strict-origin-when-cross-origin. |
| `DUPLICATE_SECURITY_HEADER` | notice | crawl:security_headers | HTML page sends a security header more than once | Send each security header once. A repeated header can be ignored or applied next to the other copy, so the policy the browser enforces may not be the one intended. |
| `OUTLINK_TO_LOCALHOST` | warning | crawl:link_findings | A link points at a loopback address (localhost, 127.0.0.1, ::1, ...) | Replace the development/staging reference with the production URL. |
| `JS_CONSOLE_ERRORS` | warning | crawl:browser_artifacts | The page's browser console logged errors while it was rendered | Fix the script error the console reports; an uncaught exception can stop the page's content, links or metadata from being built in the browser. |
| `FOLLOW_AND_NOFOLLOW_INLINKS` | notice | crawl:link_findings | The page receives both a followed and a nofollow internal link | Decide deliberately whether the page should be crawl-priority or not, and make every internal link to it agree. |
| `INTERNAL_NOFOLLOW_OUTLINKS` | notice | crawl:link_findings | The page has internal outlinks marked nofollow | Confirm each internal nofollow link is intentional; links to pages that should be crawled and ranked should be followed. rel=sponsored and rel=ugc are not visible here and stay distinct. |
| `INTERNAL_LINK_SPONSORED_UGC` | notice | crawl:link_findings | An internal link carries rel=sponsored or rel=ugc | Remove sponsored and ugc from links to pages of this site. Keep them only on links that really lead to paid placements or user-submitted content. |
| `MIXED_LINK_REL` | notice | crawl:link_findings | A link combines nofollow, sponsored or ugc tokens | The combination is valid. Review whether each qualifying token describes the placement; retain intentional combinations and the full rel token set. |
| `OUTBOUND_SPONSORED_WITHOUT_DISCLOSURE` | notice | crawl:link_findings | An outbound sponsored link has no recognized local disclosure text | Review the retained local text and add a clear reader-facing disclosure when needed. This bounded English/Russian text-marker heuristic does not establish legal compliance; CSS visibility, other languages and remote disclosures require manual review. |
| `HTTP_LINK_ON_HTTPS` | notice | crawl:link_findings | HTTPS page contains an ordinary internal http:// anchor | Write the internal anchor as https:// or a relative URL. A fetched HTTP variant that already redirects to HTTPS is reported as converged, not as this finding. |
| `FORM_URL_INSECURE` | critical | crawl:link_findings | A form submits to an http:// action, so its data leaves the browser unencrypted regardless of the page's own scheme | Point the form's action at an https:// URL. |
| `FORM_ON_HTTP_URL` | critical | crawl:link_findings | A form with a password field is served from a plain-HTTP page, so the credentials themselves travel unencrypted before the action URL is even reached | Serve the page itself over HTTPS; an HTTPS form action does not protect input typed on an HTTP page. |
| `BROKEN_BOOKMARK` | warning | crawl:fragment_links | Link fragment identifies no element on the destination page | Point the href at an element id or <a name> that exists in the destination document, or add the missing target; a different query string is a different document. |
| `DUPLICATE_ID` | warning | crawl:duplicate_ids | HTML id value is repeated in the parsed document | Give each element its own id; duplicate ids make fragment targets, scripts, and accessibility relationships ambiguous. |
| `DECLARED_MIME_MISMATCH` | notice | SF-derived | Received Content-Type conflicts with an unambiguous URL filename extension | Confirm the response Content-Type and URL extension describe the intended resource; this is a declaration consistency signal, not full MIME validation. |
| `BODY_MIME_MISMATCH` | warning | crawl:body_mime | Retained response body does not match its declared Content-Type | Serve the resource with a Content-Type that matches its body, or return the intended body. |
| `URL_SESSION_ID` | warning | SF-derived | Indexable URL contains a recognized session-token parameter | Prevent session identifiers from entering crawlable URLs and consolidate any indexed variants; the finding retains parameter names but redacts their values. |
| `URL_TRAILING_SLASH_INCONSISTENT` | notice | SF-derived | Comparable trailing-slash URL variants both serve as separate indexable pages | Choose and implement a documented URL policy if both variants are not intentional; redirect or canonical convergence already observed is not reported. |
| `PLACEHOLDER_MARKER` | warning | SF-derived | Page declaration contains a literal TODO, TBD, or placeholder template marker | Replace the observed marker with reviewed page-specific copy. This check only reports explicit bracketed template tokens, not ordinary wording such as 'coming soon'. |

## 9.C — objective trust & attribution evidence (issue #823)

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `NO_AUTHOR_BYLINE` | notice | crawl:trust_signals | Content-shaped page declares no author or byline markup | If the page's authorship should be attributable, declare it with a machine-readable carrier: meta author, rel=author, itemprop=author, or a JSON-LD author property. |
| `NO_CONTENT_DATES` | notice | crawl:trust_signals | Content-shaped page declares no publication or update date | If the page should be dated, declare it machine-readably: article:published_time or datePublished markup, a <time datetime> element, or a Last-Modified response header. |
| `MISSING_ABOUT_PAGE` | notice | SF-derived | No indexable About page was found in the crawl's scope | If an About page exists, make it reachable and indexable; the finding's state says whether it was found non-indexable, broken, linked but never crawled, or not discovered at all. |
| `MISSING_CONTACT_PAGE` | notice | SF-derived | No indexable Contact page was found in the crawl's scope | If a Contact page exists, make it reachable and indexable; the finding's state says whether it was found non-indexable, broken, linked but never crawled, or not discovered at all. |
| `MISSING_PRIVACY_POLICY` | notice | SF-derived | No indexable privacy policy page was found in the crawl's scope | If a privacy policy exists, make it reachable and indexable; the finding's state says whether it was found non-indexable, broken, linked but never crawled, or not discovered at all. |
| `MISSING_TERMS_PAGE` | notice | SF-derived | No indexable terms-of-use page was found in the crawl's scope | If a terms page exists, make it reachable and indexable; the finding's state says whether it was found non-indexable, broken, linked but never crawled, or not discovered at all. |
| `FEW_CITATIONS` | notice | SF-derived | Content-shaped page has no observable outbound reference in its body content | Review whether the page should cite its sources. An outbound link inside the body copy is the only carrier a crawl can observe as a citation; the finding never asserts that any link is authoritative. |
| `YMYL_REVIEW_CANDIDATE` | notice | SF-derived | URL path or title matches a YMYL-adjacent keyword — a review candidate only, not a classification | Have a specialist confirm whether the page genuinely covers Your-Money-Your-Life topics; if it does, the attribution and trust-page evidence above deserves a closer look. |

## Native asset budgets and supplied Lighthouse lab evidence (issue #1303)

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `ASSET_SHORT_CACHE` | notice | native resource delivery | A captured resource has no long-lived public cache policy | Use an appropriate public max-age for versioned static assets; keep private and no-store policies where required. |
| `FONT_DISPLAY_MISSING` | notice | native resource delivery | An observed stylesheet font-face lacks an accepted font-display value | Set font-display to swap, fallback or optional when appropriate for the font. |
| `HEAVY_PAGE_WEIGHT` | warning | native render responses / HTML | Observed navigation responses or HTML alone exceed the configured content-decoded byte budget | Reduce the measured document, images, fonts and scripts; keep raw-only and incomplete runtime coverage explicit. |
| `RESOURCE_OVER_2MB` | warning | native resource graph | A captured resource exceeds the configured individual byte budget | Reduce or split the identified resource; the default budget is 2 MiB. |
| `UNUSED_CSS_JS` | notice | native render coverage | CSS or JavaScript source is substantially unused in the captured navigation window | Split or defer unused code after testing the interactions and routes that need it; lab coverage is not proof code is universally unused. |
| `UNMINIFIED_CSS_JS` | notice | native resource delivery | Captured CSS or JavaScript does not look minified (heuristic) | Apply the existing production minifier and verify the result still works. |
| `DUPLICATED_JS` | notice | native resource delivery | Different script URLs on one page contain identical captured source bytes | Load the identical script once; review the build and page template. |
| `LEGACY_JS` | notice | native resource delivery | Captured JavaScript has an unconditional legacy or polyfill marker (heuristic) | Review target browser support before removing or splitting legacy code. |
| `IMG_INTRINSIC_OVERSIZED` | notice | native render layout | A heavy image has more intrinsic width than its displayed device pixels need | Serve a responsive image sized for the observed viewport and device pixel ratio. |
| `IMG_UPSCALED` | warning | native render layout | An image has less intrinsic width than its displayed device pixels need | Supply an image with enough intrinsic pixels for the observed viewport and device pixel ratio. |
| `LCP_DISCOVERY` | notice | supplied Lighthouse JSON | The supplied Lighthouse LCP discovery audit reports an opportunity | Review the recorded LCP element and make its image discoverable and appropriately prioritized. |
| `NETWORK_DEPENDENCY_TREE` | notice | supplied Lighthouse JSON | The supplied Lighthouse dependency-tree audit reports a request-chain opportunity | Shorten the measured critical request chain; inspect the supplied lab trace evidence. |
| `LAYOUT_SHIFT_CULPRITS` | notice | supplied Lighthouse JSON | The supplied Lighthouse layout-shift audit reports culprits | Reserve layout space and correct the measured shift sources. |
| `IMAGE_DELIVERY` | notice | supplied Lighthouse JSON | The supplied Lighthouse image-delivery audit reports an opportunity | Optimize the measured image size and encoding while preserving visual quality. |
| `FORCED_REFLOW` | notice | supplied Lighthouse JSON | The supplied Lighthouse forced-reflow audit reports an opportunity | Review the measured script stack and batch layout reads and writes. |
| `JS_EXECUTION_TIME` | notice | supplied Lighthouse JSON | The supplied Lighthouse bootup-time audit reports expensive script execution | Reduce or defer the measured JavaScript execution work. |

## Document markup: structural AMP, static accessibility, and Schema.org validation

| Check id | Severity | Evidence | Fires on | Fix |
|---|---|---|---|---|
| `AMP_MISSING_HTML_ATTRIBUTE` | warning | crawl:markup_evidence | AMP target does not declare the html amp or lightning attribute | Add the AMP attribute to the root html element, or correct the rel=amphtml target. |
| `AMP_INVALID_DOCTYPE` | warning | crawl:markup_evidence | AMP document has a missing or invalid HTML5 doctype | Start the AMP source document with <!doctype html>. |
| `AMP_MISSING_HEAD` | warning | crawl:markup_evidence | AMP source document has no explicit head element | Include an explicit head element in the AMP source. |
| `AMP_MISSING_BODY` | warning | crawl:markup_evidence | AMP source document has no explicit body element | Include an explicit body element in the AMP source. |
| `AMP_INVALID_CHARSET` | warning | crawl:markup_evidence | AMP head does not start with the required UTF-8 charset declaration | Place <meta charset="utf-8"> first in the AMP head. |
| `AMP_INVALID_VIEWPORT` | warning | crawl:markup_evidence | AMP document lacks the required device-width viewport | Declare a viewport containing width=device-width in the head. |
| `AMP_INVALID_RUNTIME` | warning | crawl:markup_evidence | AMP document lacks an async AMP runtime script in the head | Add <script async src="https://cdn.ampproject.org/v0.js"></script> to the AMP head. |
| `AMP_INVALID_BOILERPLATE` | warning | crawl:markup_evidence | AMP document lacks the structural AMP boilerplate declarations | Include the required amp-boilerplate style and noscript fallback; validate their content with the official AMP validator. |
| `A11Y_HTML_LANG_MISSING` | warning | crawl:markup_evidence | Root html element has no nonempty lang declaration | Declare the document language on html and review whether it matches the content. |
| `A11Y_FORM_LABEL_MISSING` | warning | crawl:markup_evidence | Form control has no static accessible label | Associate a label with the control, or provide a valid aria-label or aria-labelledby reference. |
| `A11Y_IMAGE_ALT_MISSING` | warning | crawl:markup_evidence | Image has no alt attribute or static accessible name | Provide meaningful alternative text; explicitly mark decorative images with empty alt text. |
| `A11Y_HEADING_ORDER` | notice | crawl:markup_evidence | Heading sequence skips a level in the static document | Review the heading outline and use nested heading levels consistently. |
| `A11Y_MAIN_LANDMARK_MISSING` | notice | crawl:markup_evidence | Document content has no main landmark | Mark the primary page content with main or role=main and verify navigation with assistive technology. |
| `STRUCTURED_DATA_SCHEMA_ERROR` | warning | crawl:structured_evidence | Structured data contains errors under the bundled Schema.org vocabulary | Inspect the URL schema evidence and repair unknown types, properties, invalid values or unresolved references. |
| `STRUCTURED_DATA_SCHEMA_WARNING` | notice | crawl:structured_evidence | Structured data contains warnings under the bundled Schema.org vocabulary | Review the URL schema evidence and correct ambiguous values or graph relationships. |
| `STRUCTURED_DATA_RICH_RESULT_REQUIRED_MISSING` | warning | crawl:structured_evidence | Structured data lacks required properties in the bundled local rich-result rules | Fill the required properties for the declared type and validate the result; local eligibility does not guarantee Google display. |
| `GSC_ORPHAN` | warning | provider:gsc_search_analytics | Search Console reports impressions for a same-origin URL absent from the crawl | Check the retained page and crawl scope, then restore, redirect, or remove the URL from search when appropriate. |
| `NO_SEARCH_DATA` | notice | provider:gsc_search_analytics | Indexable crawled URL has no retained Search Console impressions | Review indexing, canonicalization, demand, and the measured reporting window before changing the page. |
| `NON_INDEXABLE_WITH_SEARCH_DATA` | warning | provider:gsc_search_analytics | Non-indexable crawled URL has Search Console impressions | Review the URL's indexability and canonical signals; keep the current state only when it is intentional. |
| `GSC_INDEXED_WITH_ISSUES` | warning | provider:gsc_url_inspection | Search Console reports an indexed URL with AMP or rich-result issues | Review the specific AMP or rich-result inspection evidence for the sampled URL. |
| `GSC_AMP_INVALID` | warning | provider:gsc_url_inspection | Search Console reports an invalid AMP result | Review the AMP inspection result and repair or remove obsolete AMP markup. |
| `GSC_RICH_RESULTS_INVALID` | warning | provider:gsc_url_inspection | Search Console reports invalid structured data for a rich result | Review the inspected rich-result item and correct errors reported by Search Console. |
| `GSC_INDEXABLE_NOT_INDEXED` | warning | provider:gsc_url_inspection | Crawl reports an indexable URL that Search Console does not index | Review Search Console's coverage state, canonical, fetch state, and indexing directives. |
| `GSC_USER_CANONICAL_NOT_SELECTED` | warning | provider:gsc_url_inspection | Google selected a different canonical URL than the declared canonical | Align internal links, redirects, sitemap membership, and the declared canonical with the preferred URL. |
| `GSC_URL_NOT_ON_GOOGLE` | warning | provider:gsc_url_inspection | Search Console reports that the inspected URL is not on Google | Review the sampled URL's Search Console coverage state and correct blocking or canonical issues where appropriate. |
| `GA_ORPHAN_URL` | warning | provider:ga4_history | GA4 reports a landing URL absent from the crawl | Check whether the URL is intentionally retired, blocked, redirected, or missing from the crawl population. |
| `NO_GA_DATA` | notice | provider:ga4_history | Indexable crawled URL has no retained GA4 sessions | Review the measured period and analytics tagging before treating the absence as a page defect. |
| `NON_INDEXABLE_WITH_GA_DATA` | warning | provider:ga4_history | Non-indexable crawled URL has GA4 sessions | Review whether the URL should be indexable; keep its current state only when that is intentional. |
| `BOUNCE_RATE_ABOVE_70` | notice | provider:metrika_history | Yandex Metrika reports a bounce rate above 70 percent | Review the measured URL and its traffic source; bounce rate is a diagnostic signal, not a ranking verdict. |
| `METRIKA_ORPHAN_URL` | warning | provider:metrika_history | Yandex Metrika reports a landing URL absent from the crawl | Check whether the URL is intentionally retired, blocked, redirected, or missing from the crawl population. |
| `NO_METRIKA_DATA` | notice | provider:metrika_history | Indexable crawled URL has no retained Yandex Metrika visits | Review the measured period and Metrika tagging before treating the absence as a page defect. |
| `NON_INDEXABLE_WITH_METRIKA_DATA` | warning | provider:metrika_history | Non-indexable crawled URL has Yandex Metrika visits | Review whether the URL should be indexable; keep its current state only when that is intentional. |
| `METRIKA_BOUNCE_RATE_ABOVE_70` | notice | provider:metrika_history | Yandex Metrika reports a bounce rate above 70 percent | Review the measured URL and its traffic source; bounce rate is a diagnostic signal, not a ranking verdict. |
