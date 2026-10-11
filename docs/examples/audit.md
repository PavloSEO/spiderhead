# SEO audit — example.com

- **Generated:** 2026-10-11T10:12:59Z

## Health summary

> **No health score.** only 94 of 220 checks could run (43% coverage); too little evidence to score.

_94 of 220 checks could run; the score is not comparable to a run with full evidence_

- Checks: **20 fired**, 126 skipped, 74 silent, 0 disabled (of 220 total)

- URLs crawled: **6** (HTML: 4, indexable: 4)
- Total issues: **25**

| Severity | Count |
|---|---:|
| 🔴 Critical | 3 |
| 🟡 Warning | 10 |
| ⚪ Notice | 12 |

**Most frequent issues:**

| Issue | Count | Severity |
|---|---:|---|
| H2 is duplicated across multiple URLs | 3 | notice |
| Duplicate meta description | 2 | warning |
| Duplicate title element | 2 | warning |
| Title falls below the configured length threshold | 2 | notice |
| Internal link points to a 4xx URL | 1 | critical |
| Page returns a 4xx response (broken page) | 1 | critical |
| Indexable page has no canonical URL | 1 | warning |
| Meta description is missing | 1 | warning |
| Meta description falls below the configured length threshold | 1 | notice |
| Multiple H1 headings on the page | 1 | warning |
| HTML bloat: high document size relative to text content | 1 | notice |
| HTML document is large in absolute terms or relative to the site | 1 | warning |

**Internal linking**

> Not measured. no all_inlinks export (needed for the complete internal edge list).

**HTML size:** median 76 KB, p90 229 KB, p95 261 KB, max 293 KB.

## Look at these before trusting the rest

Each check below describes more than half the crawled pages. That can be true -- a site really may have no meta description anywhere -- but it is also what a broken check looks like, and it is worth one minute of checking against the live site before the rest of this report is acted on.

| Issue | Pages | Share of crawl |
|---|---:|---:|
| H2 is duplicated across multiple URLs | 3 | 75% |

## 🔴 Critical (3)

### Internal link points to a 4xx URL (1)

| Destination | Status | Source page | Anchor | Position | XPath |
|---|---:|---|---|---|---|
| https://example.com/old-page | 404 | https://example.com/ | Legacy Page | Content | `/html/body/main/article/p[3]/a` |
| https://example.com/old-page | 404 | https://example.com/page-a | view pump specifications | Footer | `/html/body/footer/nav/a[2]` |

> _How to fix:_ Update the link to the current URL or add an appropriate 301 redirect; if it appears in the footer or navigation, fix the shared template.

### Page returns a 4xx response (broken page) (1)

| URL | Details |
|---|---|
| https://example.com/old-page | Status: Not Found, Inlinks: 4 |

> _How to fix:_ Restore the page or redirect it with a 301 to a relevant URL; remove or update links that point to it.

### Title element is missing (1)

- https://example.com/no-title

> _How to fix:_ Add a unique, descriptive title element.

## 🟡 Warning (10)

### Indexable page has no canonical URL (1)

- https://example.com/no-title

> _How to fix:_ Add a valid <link rel="canonical"> element.

### Duplicate meta description (2)

- **"A sample description over seventy characters that reliably meets the configured audit threshold."** — 2 URLs:
    - https://example.com/
    - https://example.com/page-b

> _How to fix:_ Write a unique meta description for each page.

### Meta description is missing (1)

- https://example.com/no-title

> _How to fix:_ Add a useful meta description, typically up to about 160 characters.

### Multiple H1 headings on the page (1)

| URL | H1 text |
|---|---|
| https://example.com/page-a | Pump Models ⏐ Second H1 Heading |

> _How to fix:_ Keep one primary H1 and demote the remaining headings to H2 or H3 as appropriate.

### HTML document is large in absolute terms or relative to the site (1)

| URL | Size | × median | Rank |
|---|---:|---:|---:|
| https://example.com/no-title | 293 KB | ×3.87 | 1 |

> _How to fix:_ Reduce HTML size by removing unnecessary markup, extracting inline styles or scripts, and avoiding embedded base64 assets.

### Slow server response (1)

| URL | Details |
|---|---|
| https://example.com/no-title | Response time: 2.0, Max s: 1.5 |

> _How to fix:_ Improve TTFB by profiling the application and origin, then optimizing caching and infrastructure.

### Thin content (low word count) (1)

| URL | Details |
|---|---|
| https://example.com/page-b | Word count: 50, Threshold: 200 |

> _How to fix:_ Add substantial, useful content or exclude the page from indexing when it has no standalone search value.

### Duplicate title element (2)

- **"Industrial Pumps Product A"** — 2 URLs:
    - https://example.com/page-a
    - https://example.com/page-b

> _How to fix:_ Give each page a unique title element.

## ⚪ Notice (12)

### Meta description falls below the configured length threshold (1)

| URL | Details |
|---|---|
| https://example.com/page-a | Length: 7, Min chars: 70 |

> _How to fix:_ Expand the description with specific, useful page information.

### H2 is duplicated across multiple URLs (3)

| URL | Details |
|---|---|
| https://example.com/ | Value: Section, Duplicate count: 3 |
| https://example.com/no-title | Value: Section, Duplicate count: 3 |
| https://example.com/page-a | Value: Section, Duplicate count: 3 |

> _How to fix:_ Use a unique, page-specific H2 on each URL, or accept it for a shared boilerplate subheading that is genuinely meant to repeat.

### HTML bloat: high document size relative to text content (1)

| URL | Details |
|---|---|
| https://example.com/page-b | Bytes per word: 1600.0, Site median bpw: 375.0, Word count: 50, Size bytes: 80000 |

> _How to fix:_ Reduce bytes per word by extracting styles and scripts, removing embedded base64 assets, and simplifying markup.

### Low text-to-HTML ratio (1)

| URL | Details |
|---|---|
| https://example.com/page-b | Text ratio: 8.0, Threshold: 10 |

> _How to fix:_ Increase the proportion of meaningful visible content or reduce unnecessary markup.

### No indexable About page was found in the crawl's scope (1)

| URL | Details |
|---|---|
|  | State: not_discovered, Crawl pages: 6, Vocabulary: whole path segments and anchor texts from a fixed EN/RU list, Indexability:  |

> _How to fix:_ If an About page exists, make it reachable and indexable; the finding's state says whether it was found non-indexable, broken, linked but never crawled, or not discovered at all.

### No indexable Contact page was found in the crawl's scope (1)

| URL | Details |
|---|---|
|  | State: not_discovered, Crawl pages: 6, Vocabulary: whole path segments and anchor texts from a fixed EN/RU list, Indexability:  |

> _How to fix:_ If a Contact page exists, make it reachable and indexable; the finding's state says whether it was found non-indexable, broken, linked but never crawled, or not discovered at all.

### No indexable privacy policy page was found in the crawl's scope (1)

| URL | Details |
|---|---|
|  | State: not_discovered, Crawl pages: 6, Vocabulary: whole path segments and anchor texts from a fixed EN/RU list, Indexability:  |

> _How to fix:_ If a privacy policy exists, make it reachable and indexable; the finding's state says whether it was found non-indexable, broken, linked but never crawled, or not discovered at all.

### No indexable terms-of-use page was found in the crawl's scope (1)

| URL | Details |
|---|---|
|  | State: not_discovered, Crawl pages: 6, Vocabulary: whole path segments and anchor texts from a fixed EN/RU list, Indexability:  |

> _How to fix:_ If a terms page exists, make it reachable and indexable; the finding's state says whether it was found non-indexable, broken, linked but never crawled, or not discovered at all.

### Title falls below the configured length threshold (2)

| URL | Details |
|---|---|
| https://example.com/page-a | Title: Industrial Pumps Product A, Length: 26, Min chars: 30 |
| https://example.com/page-b | Title: Industrial Pumps Product A, Length: 26, Min chars: 30 |

> _How to fix:_ Expand the title to an informative length without padding it with boilerplate.

## Sitemap & robots

- Declared in robots.txt: **None**
- URLs in sitemap: **0**  ·  indexable URLs in crawl: **4**
- In sitemap but not in crawl: **0**  ·  in crawl but not in sitemap: **0**
- Non-200 URLs in sitemap: **0**  ·  non-indexable URLs in sitemap: **0**

## Appendix: skipped checks

| Issue | Reason |
|---|---|
| Image is missing alt text | missing export: images_missing_alt |
| Image exceeds the configured file-size threshold | missing export: images_over_kb |
| Image is missing width and height attributes | missing export: images_missing_size |
| Mixed content: HTTPS page loads resources over HTTP | missing export: security_mixed |
| HSTS header is missing | missing export: security_hsts |
| Structured data is missing | missing export: structured_data_missing |
| Hreflang implementation error | missing export: hreflang |
| Hreflang points to a redirecting or broken URL (3xx, 4xx, or 5xx) | missing export: all_hreflang |
| Sitemap URL returns a 4xx or 5xx response | missing export: sitemap_non_200 |
| Sitemap URL returns a 3xx response | missing export: sitemap_redirects |
| Sitemap contains a non-indexable URL | missing export: sitemap_non_indexable |
| Link fragment identifies no element on the destination page | export input retains no HTML/DOM bodies; fragment targets are measured only from a native retained scan |
| AMP document has a missing or invalid HTML5 doctype | document-bound markup evidence requires a retained native crawl |
| AMP source document has no explicit head element | document-bound markup evidence requires a retained native crawl |
| AMP source document has no explicit body element | document-bound markup evidence requires a retained native crawl |
| AMP head does not start with the required UTF-8 charset declaration | document-bound markup evidence requires a retained native crawl |
| AMP document lacks the required device-width viewport | document-bound markup evidence requires a retained native crawl |
| AMP document lacks an async AMP runtime script in the head | document-bound markup evidence requires a retained native crawl |
| AMP document lacks the structural AMP boilerplate declarations | document-bound markup evidence requires a retained native crawl |
| AMP target does not declare the html amp or lightning attribute | document-bound markup evidence requires a retained native crawl |
| Root html element has no nonempty lang declaration | document-bound markup evidence requires a retained native crawl |
| Form control has no static accessible label | document-bound markup evidence requires a retained native crawl |
| Image has no alt attribute or static accessible name | document-bound markup evidence requires a retained native crawl |
| Heading sequence skips a level in the static document | document-bound markup evidence requires a retained native crawl |
| Document content has no main landmark | document-bound markup evidence requires a retained native crawl |
| Structured data contains errors under the bundled Schema.org vocabulary | document-bound markup evidence requires a retained native crawl |
| Structured data contains warnings under the bundled Schema.org vocabulary | document-bound markup evidence requires a retained native crawl |
| Structured data lacks required properties in the bundled local rich-result rules | document-bound markup evidence requires a retained native crawl |
| Full AMP validator reports disallowed HTML | document-bound markup evidence requires a retained native crawl |
| Full AMP validator reports other document validation errors | document-bound markup evidence requires a retained native crawl |
| More than one <meta name="description"> element is present | no meta description count evidence (native crawl only) |
| Page has an H1 but no H2 headings | requirements.require_h2 is false; the check was not evaluated |
| The H1 has no text of its own; its only content is an image's alt attribute | no H1 alt-text evidence (native crawl only) |
| One or more headings appear before the page's first H1 in DOM order | no heading outline evidence (native crawl only) |
| A heading sits in the page chrome (header, nav, sidebar or footer) rather than in the content | no heading outline evidence (native crawl only) |
| Content headings skip one or more levels in DOM order | no heading outline evidence (native crawl only) |
| A heading on the page is, or contains, a link to somewhere else | no link-placement evidence (native crawl only) |
| An image link carries no anchor text and no alt text, so nothing says where it goes | no link-placement evidence (native crawl only) |
| Obsolete meta keywords element is present | no Meta Keywords 1 column in Internal:All |
| Paginated URL canonical does not match the configured project policy | no pagination canonical policy configured |
| Filtered URL canonical does not match the configured project policy | no filters canonical policy configured |
| The page's content sits inside an iframe and is not attributed to this URL | no iframe inventory in this evidence |
| Structured data validation errors | no Structured Data validation columns in Internal:All |
| A JSON-LD block is present but did not parse as valid JSON | no JSON-LD found/parsed block counts (native crawl only) |
| Text is difficult to read (low Flesch score) | no Readability/Flesch column |
| Average sentence length is too high | no Average Words Per Sentence column |
| Spelling errors detected | no Spelling Errors column (enable spell-check in SF) |
| Grammar errors detected | no Grammar Errors column (enable grammar-check in SF) |
| Redirect is implemented with an HTTP Refresh response header | no Refresh response header evidence (native crawl only) |
| Page declares multiple canonical URLs | no Canonical Link Element 2 column in Internal:All |
| A rel="next" pagination series loops back on itself | no rel="next" column in Internal:All |
| A pagination series is reachable only by following rel="next", never by a hyperlink | no rel="next" column in Internal:All |
| A rel="next" series breaks a page-number run it otherwise follows | no rel="next" column in Internal:All |
| Response uses HTTP/1.x rather than HTTP/2 or newer | no HTTP Version column in Internal:All |
| AMP version is declared | no amphtml Link Element column in Internal:All |
| The AMP page declared by rel=amphtml does not answer 200 | no amphtml Link Element column in Internal:All |
| The AMP page declares no rel=canonical | no amphtml Link Element column in Internal:All |
| The AMP page's rel=canonical does not point back to the page that declares it | no amphtml Link Element column in Internal:All |
| The AMP page's canonical target is not indexable | no amphtml Link Element column in Internal:All |
| The AMP page is indexable | no amphtml Link Element column in Internal:All |
| The Lorem Ipsum placeholder passage appears in the page's own content area | no Lorem Ipsum evidence (native crawl only) |
| Page contains a legacy plugin-dependent element (<object>/<embed>/<applet>) | no plugin-element evidence (native crawl only) |
| An <img> has no alt attribute at all (not even alt="") | no per-image evidence (native crawl only) |
| An image's alt text exceeds the configured length threshold | no per-image evidence (native crawl only) |
| Image URL answered with an HTTP error, so the browser shows nothing | no resource evidence (native crawl with resource capture only) |
| The deprecated AJAX crawling scheme (#! / _escaped_fragment_) is still used by this page's URL or by URLs it links to | no AJAX-scheme URL evidence (native crawl only) |
| Page declares <meta name="fragment"> -- the page-wide opt-in to the deprecated AJAX crawling scheme | no <meta name="fragment"> evidence (native crawl only) |
| No character encoding declared via Content-Type or an early <meta> tag | no Meta Charset column, so a page without a header charset cannot be distinguished from one declaring <meta charset> (needs a native seohead crawl or Custom Extraction in SF) |
| Document lacks a modern <!DOCTYPE html> declaration, triggering quirks mode | no Doctype column (needs a native seohead crawl or Custom Extraction in SF) |
| No <meta name=viewport> tag with width or an initial-scale of at least 1 | no Viewport column (needs a native seohead crawl or Custom Extraction in SF) |
| A <link rel=alternate media=...> declaration names no URL | no rel=alternate media evidence (native crawl only) |
| HTML response is served uncompressed above the size where gzip/br would help | no Content-Encoding column (needs a native seohead crawl or Custom Extraction in SF) |
| The <title> element is outside <head> once the parser resolves the document | no element-position evidence (needs a native seohead crawl; Screaming Frog has no notion of this on its own) |
| The meta description is outside <head> once the parser resolves the document | no element-position evidence (needs a native seohead crawl; Screaming Frog has no notion of this on its own) |
| The canonical link is outside <head> once the parser resolves the document | no element-position evidence (needs a native seohead crawl; Screaming Frog has no notion of this on its own) |
| A robots-directive meta tag is outside <head> once the parser resolves the document | no element-position evidence (needs a native seohead crawl; Screaming Frog has no notion of this on its own) |
| An hreflang alternate link is outside <head> once the parser resolves the document | no element-position evidence (needs a native seohead crawl; Screaming Frog has no notion of this on its own) |
| Document has no <head> element | no element-position evidence (needs a native seohead crawl; Screaming Frog has no notion of this on its own) |
| Document has more than one <head> element | no element-position evidence (needs a native seohead crawl; Screaming Frog has no notion of this on its own) |
| Document has no <body> element | no element-position evidence (needs a native seohead crawl; Screaming Frog has no notion of this on its own) |
| Document has more than one <body> element | no element-position evidence (needs a native seohead crawl; Screaming Frog has no notion of this on its own) |
| An element the head content model does not allow is written inside <head> | no element-position evidence (needs a native seohead crawl; Screaming Frog has no notion of this on its own) |
| <head> is not the first element under <html> once the parser resolves the document | no element-position evidence (needs a native seohead crawl; Screaming Frog has no notion of this on its own) |
| HTML id value is repeated in the parsed document | no duplicate-id evidence (native crawl only) |
| og:title is missing, so social previews may not render correctly | no Open Graph columns in Internal:All (enable OG extraction in SF) |
| Redirect chain contains two or more hops | no redirect data (Internal:All has no Redirect URL column) |
| Redirect loop detected | no redirect data (Internal:All has no Redirect URL column) |
| Multiple <title> elements | no titles_multiple export (export this SF filter to enable) |
| Internal link points to a 5xx URL | export inlinks_5xx not available |
| External link points to a 4xx or 5xx URL | export inlinks_5xx not available |
| Internal link points to a redirect (3xx) | export inlinks_3xx not available |
| External link points to a redirect (3xx) | export inlinks_3xx not available |
| Hreflang alternate target has no hyperlink pointing to it anywhere in the crawl | no all_hreflang export (export Bulk Export → Links → All Hreflang to enable) |
| Hreflang alternate points to an observed noindex page | no all_hreflang declaration export |
| Hreflang value is not a valid ISO 639-1 language / ISO 3166-1 region code | no all_hreflang export (export Bulk Export -> Links -> All Hreflang) |
| The same hreflang value is declared more than once on the page | no all_hreflang export (export Bulk Export -> Links -> All Hreflang) |
| Page declares hreflang alternates but does not reference itself | no all_hreflang export (export Bulk Export -> Links -> All Hreflang) |
| Hreflang set has no x-default fallback | no all_hreflang export (export Bulk Export -> Links -> All Hreflang) |
| Hreflang points to a URL that is not itself the canonical version | no all_hreflang export (export Bulk Export -> Links -> All Hreflang) |
| Another page's hreflang points here, but this page does not point back | no all_hreflang export (export Bulk Export -> Links -> All Hreflang) |
| This page declares a counterpart under a language and region code the counterpart does not confirm for itself | no all_hreflang export (export Bulk Export -> Links -> All Hreflang) |
| Internal link score is far below the site median | no all_inlinks export (needed for the complete internal edge list) |
| Every internal link to this page is nofollow | no all_inlinks export (needed for the complete inlink list) |
| Every internal link to this page comes from a non-indexable source | no all_inlinks export (needed for the complete inlink list) |
| The shortest hyperlink route from the start page exceeds the configured depth | no all_inlinks export (needed for the complete internal edge list) |
| An HTTPS page loads a resource (image, script, stylesheet, ...) over plain HTTP | no all_inlinks export (needed for the resource inventory) |
| The page declares more than one rel="next" (or more than one rel="prev") URL | no all_inlinks export (needed for every rel="next"/rel="prev" declaration and the anchors beside them) |
| A rel="next"/rel="prev" URL is not also linked from the same page with an anchor | no all_inlinks export (needed for every rel="next"/rel="prev" declaration and the anchors beside them) |
| The page is more clicks from the crawl's start URL than the configured floor | no all_inlinks export (needed for the complete internal edge list) |
| The page repeats the same link -- same destination, same anchor text -- more than once | no all_inlinks export (needed for the complete internal edge list) |
| Content-shaped page declares no author or byline markup | no Trust Signals column (native crawl or a same-named custom extraction) -- no author markup evidence at all |
| Content-shaped page declares no publication or update date | no Trust Signals column and no Last Modified column -- no date evidence at all |
| Content-shaped page has no observable outbound reference in its body content | no External Outlinks column and no all_inlinks export -- nothing to count outbound citations from |
| DOM nesting is too deep | no stored HTML (input.html_store_dir not set) |
| DOM contains too many nodes | no stored HTML (input.html_store_dir not set) |
| Exact duplicate content (identical hash) | SF native Hash column already covers this |
| Near-duplicate content | no stored HTML (input.html_store_dir not set) |
| Templated titles share a common prefix or suffix across most pages | too few titles to assess templating |
| robots.txt does not declare a Sitemap directive | no sitemap URL to check (no export, no --sitemap, and live_recheck disabled) |
| robots.txt blocks JavaScript or CSS resources required for rendering | no sitemap URL to check (no export, no --sitemap, and live_recheck disabled) |
| Some child sitemaps could not be fetched or parsed | no sitemap URL to check (no export, no --sitemap, and live_recheck disabled) |
| Sitemap declares more URLs than the protocol allows | no sitemap document was fetched to measure |
| Sitemap exceeds the protocol's uncompressed size limit | no sitemap document was fetched to measure |
| URL is declared in more than one sitemap | no sitemap entries were fetched to compare |
| Sitemap contains stale or boilerplate lastmod values | no sitemap entries were fetched to compare |
| Sitemap and crawl URL sets are out of sync | no sitemap URL set (no export and network disabled) |

