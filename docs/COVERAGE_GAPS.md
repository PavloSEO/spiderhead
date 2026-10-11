# Audit coverage — the gap map

**Purpose.** The list of SEO checks our SF audit
(`seohead/sf/core/registry.py`, 204 checks) still **lacks**. For every gap:
value, implementation mode, likely home in the code. This is a filling plan,
not a bug report. Items implemented since this map was written are marked
**DONE**.

**Reading the "Mode" column.**
- **B** — doable on ready SF exports (Internal:All, native filters,
  `*:Inlinks`, Redirect Chains). A rule in `rules.py`/`inlinks.py` + a row
  in `registry.py`, no network.
- **B+** — doable on exports **but** needs a non-standard SF column (crawl
  config or Custom Extraction/XPath). Absent in default crawls — noted
  separately.
- **A/live** — SF cannot give it; needs a live HTTP probe, headless
  rendering (Playwright) or an external source (Lighthouse/CrUX/PageSpeed).
  Outside `sf-analyzer` — a candidate for a separate live tool.

**Value** — a subjective SEO-effect estimate (high/medium/low), not
implementation complexity.

**Related.** [`CHECKLIST_AUDIT.md`](CHECKLIST_AUDIT.md) checks this same
registry against an external ~320-item technical-SEO catalogue, organized by
that catalogue's own categories rather than by implementation cost. The two
documents overlap (hreflang and canonical items in particular) but were built
from different starting lists; read both before filing a new gap.

> Already covered — keep these out of new-feature proposals
> again: `NEAR_DUPLICATE` (native SF column), `READABILITY_DIFFICULT` +
> `LONG_SENTENCES` (Flesch), `META_KEYWORDS_PRESENT`,
> `IMG_MISSING_DIMENSIONS`, `IMG_OVER_KB`, `IMG_MISSING_ALT`,
> `MIXED_CONTENT`, `MISSING_HSTS`, `ORPHAN_PAGE`, `NO_INTERNAL_OUTLINKS`,
> `URL_UNDERSCORES`/`URL_MULTIPLE_SLASHES`/`URL_CONTAINS_SPACE`/
> `URL_REPETITIVE_PATH`, `META_REFRESH_REDIRECT`, `CANONICAL_RELATIVE`,
> `CANONICAL_MULTIPLE`, `PAGINATION_NONINDEXABLE`, `HTTP1_ONLY` —
> and, added after this map was first written: `CANONICAL_CHAIN`,
> `CANONICAL_TO_REDIRECT`, `HREFLANG_BROKEN_TARGET`,
> `GENERIC_ANCHOR_TEXT`, `URL_TRACKING_PARAMS`, `OG_MISSING`,
> `CANONICAL_FRAGMENT`, `HREFLANG_INVALID_CODE`,
> `HREFLANG_MISSING_SELF_REFERENCE`, `HREFLANG_MISSING_XDEFAULT`,
> `HREFLANG_MULTIPLE_ENTRIES`, `HREFLANG_NOT_CANONICAL`, `NOTRANSLATE`,
> `UNAVAILABLE_AFTER` (issue #30), and `IMG_MISSING_ALT_ATTRIBUTE` +
> `IMG_ALT_TOO_LONG` (rows 9.1/9.2 below, coverage-evidence #385/#386).

## Reconciliation snapshot — 2026-10-04

This map was reconciled against the generated `CHECKS.md`, current registry,
CLI/MCP handlers, synthetic tests, and open issues. Existing **DONE** rows
remain covered; the following stale candidates are now settled without
inventing a broader claim.

| Map rows | Status and current evidence |
|---|---|
| 2.1–2.5 | **Covered in registry:** `NO_AUTHOR_BYLINE`, `NO_CONTENT_DATES`, the four trust-page checks, and `FEW_CITATIONS`. They report observed markup/discovery states, never trustworthiness or citation authority. |
| 2.6–2.7 | **Partial / missing:** `YMYL_REVIEW_CANDIDATE` is a deterministic review candidate, not a classification; disclaimer/editorial-policy detection remains missing. |
| 3.1–3.9 | **Explicitly out of scope:** no 92-item accessibility/AMP or WCAG engine. `provider-collect` can return a capped opt-in Lighthouse sample with provider, engine, version, strategy, score, and failed/incomplete IDs; it is not WCAG or legal-compliance evidence. |
| 6.3–6.4 | **Covered in registry:** `CANONICAL_TARGET_ERROR` and `CANONICAL_HOMEPAGE_GROUP`. |
| 8.2 / 8.4 | **Covered in registry:** `ONLY_NOFOLLOW_INLINKS` for verified indexable targets (with partial-crawl withholding) and native `HTTP_LINK_ON_HTTPS` from retained raw hrefs; observed HTTP→HTTPS redirect convergence stays silent. |
| 10.2 / 10.4 | **Covered in registry:** `URL_SESSION_ID` redacts values and `URL_TRAILING_SLASH_INCONSISTENT` requires observed, non-convergent indexable pairs while preserving path case. |
| 11.1–11.7 | **Covered in registry:** `DUPLICATE_ID`, `MISSING_DOCTYPE`, `MISSING_CHARSET`, head/body structure, invalid head elements, `LOREM_IPSUM_PLACEHOLDER`/`PLACEHOLDER_MARKER`, and `DECLARED_MIME_MISMATCH`. These are bounded observations, not HTML conformance validation. |

Every other non-DONE row remains **missing** unless it says partial or A/live.
The priority list below is therefore a backlog, not a product-capability claim.

---

## 1. Performance and Core Web Vitals

| # | Name | Checks | Value | Mode | Home |
|---|---|---|---|---|---|
| 1.1 | Real Core Web Vitals (LCP/INP/CLS) | Field p75 vs official thresholds (LCP 2.5/4 s, INP 200/500 ms, CLS 0.1/0.25) | **high** | **partially DONE**: opt-in CrUX current record via `crux-report` and supplied `site-audit` evidence; no automatic crawl-registry check | `data_sources/crux.py` + `cwv.py` (#822) |
| 1.2 | TTFB separate from `response_time` | Time to first byte as its own metric (800/1800 ms), not overall response time | medium | **PARTIAL** — `render-check` exposes lab `ttfb_ms`; no dedicated threshold finding or field-data coverage | extend `check_url_and_perf`, id `SLOW_TTFB` |
| 1.3 | FCP / render speed | First Contentful Paint (1.8/3 s) | medium | **PARTIAL** — `render-check` exposes lab `first_contentful_paint_ms`; no dedicated threshold finding or field-data coverage | `cwv.py` / PSI |
| 1.4 | Response compression (Brotli/gzip) | content-encoding on text responses | medium | **partially DONE** in the live `asset-weight-check` (CSS/JS only; the HTML response itself is still open) | id `NO_COMPRESSION` |
| 1.5 | Cache-Control / cacheability | Presence and sanity of cache headers on static resources | medium | **partially DONE** in `asset-weight-check` (CSS/JS only; images/fonts still open) | id `WEAK_CACHE_POLICY` |
| 1.6 | Render-blocking resources | CSS/JS in `<head>` blocking first paint | medium | **DONE** in the live `asset-weight-check` (no registry id) | id `RENDER_BLOCKING` |
| 1.7 | Page weight (total) | Total page weight with resources, not HTML only (`LARGE_HTML` covers markup alone) | medium | A/live | id `HEAVY_PAGE_WEIGHT` |

**Context.** `SLOW_RESPONSE` already catches a slow server, but it is no
substitute for real CWV. None of the 183 crawl-registry checks measures field
CWV directly. The opt-in CrUX path added in #822 assesses current-window p75
with URL/origin, form factor and period provenance; missing field data stays
unavailable. A one-run `render-check` result remains lab evidence.

---

## 2. E-E-A-T (objective observations and remaining review gaps)

| # | Name | Checks | Value | Mode | Home |
|---|---|---|---|---|---|
| 2.1 | Authorship (byline) | Observed author/byline markup on scoped content pages | **high** | **DONE** — `NO_AUTHOR_BYLINE`; no author-quality verdict | `eeat.py` |
| 2.2 | Publish/update dates | Observed publication/update markup | **high** | **DONE** — `NO_CONTENT_DATES`; freshness remains unjudged | `eeat.py` |
| 2.3 | About / Contact pages | Discovered/indexable state of about/contact routes | **high** | **DONE** — `MISSING_ABOUT_PAGE` / `MISSING_CONTACT_PAGE` | `eeat.py` |
| 2.4 | Privacy Policy / Terms | Discovered/indexable legal-page state | medium | **DONE** — `MISSING_PRIVACY_POLICY` / `MISSING_TERMS_PAGE` | `eeat.py` |
| 2.5 | Outbound citations | Observable body-positioned outbound references | medium | **DONE** — `FEW_CITATIONS`; authority is not inferred | `eeat.py` |
| 2.6 | YMYL detection | Deterministic review candidate by path/title vocabulary | **high** | **Partial** — `YMYL_REVIEW_CANDIDATE`, never a classification | `eeat.py` |
| 2.7 | Trust signals / disclaimers | Disclaimer / editorial-policy markers | low | Missing | — |

**Context.** Rows 2.1–2.5 measure observed markup and discovered routes.
They do not establish author expertise, content quality, trustworthiness or
citation authority. Row 2.6 identifies review candidates only; row 2.7 remains
missing. These observations support specialist review rather than an E-E-A-T score.

---

## 3. Accessibility (category missing entirely)

| # | Name | Checks | Value | Mode | Home |
|---|---|---|---|---|---|
| 3.1 | ARIA roles and labels | Correctness of `role`/`aria-label`/`aria-*` | **high** | A (HTML/render) | new `a11y.py` (or Lighthouse) |
| 3.2 | Color contrast | Text/background contrast per WCAG | medium | A (Lighthouse) | `a11y.py` |
| 3.3 | Focus-visible / keyboard | Focus visibility, keyboard navigation | medium | A (Lighthouse) | `a11y.py` |
| 3.4 | Form labels | A `<label>` for every form field | medium | A (HTML) | `a11y.py` |
| 3.5 | Heading order (a11y) | Logical h1->h2->h3 nesting for screen readers | medium | A (HTML/render — SF gives counts, not order) | `a11y.py`, id `HEADING_ORDER_BAD` |
| 3.6 | Skip link | A "skip to content" link | low | A (HTML) | `a11y.py` |
| 3.7 | Link text (descriptive) | Anchors like "here", "click" are uninformative | medium | **DONE** as `GENERIC_ANCHOR_TEXT` (RU+EN dictionary) | `inlinks.py` |
| 3.8 | Touch-target size | Tap target size for fingers | low | A (render) — consciously excluded earlier, revisit | `a11y.py` |
| 3.9 | Table headers | `<th>` in data tables | low | A (HTML) | `a11y.py` |

**Context.** Most a11y checks need HTML/rendering and are best closed by a
Lighthouse integration, not home-grown. The pragmatic path is one external
`lighthouse-a11y` skill relaying Google's standard audits.

---

## 4. JS rendering: raw vs DOM

| # | Name | Checks | Value | Mode | Home |
|---|---|---|---|---|---|
| 4.1 | raw/render diff (title, desc, h1, canonical, noindex) | SSR/CSR rewrites key tags after render | **high** | **PARTIAL** — `render-check` compares title, h1 and canonical; description and meta-robots/noindex differences remain unmeasured | formalize into ids `RENDER_DIFF_TITLE` etc. if the audit needs them |
| 4.2 | content/links diff | Text and links appearing only after JS | **high** | **DONE** in `render-check` (share of JS-only text/links) | id `RENDER_DIFF_CONTENT` / `RENDER_DIFF_LINKS` |
| 4.3 | SSR vs CSR detect | The site is fundamentally client-side rendered | medium | **DONE** in `render-check` (empty SPA shell) | id `CSR_ONLY` |
| 4.4 | Render-blocking resources for bots | JS/CSS the bot cannot load to render | medium | B (`ROBOTS_BLOCKS_RESOURCES` exists; extend) | link with `ROBOTS_BLOCKS_RESOURCES` |

**Context.** `render-check` compares title, H1, canonical, content and links,
and reports an empty rendered shell. Description and meta-robots/noindex
differences remain unmeasured. Formalizing the measured subset as registry
ids with thresholds would make that subset part of the crawl audit document.

---

## 5. Redirects (additional forms)

| # | Name | Checks | Value | Mode | Home |
|---|---|---|---|---|---|
| 5.1 | JS redirect | Observed script navigation, separate from HTTP 3xx, history and fragment changes | **high** | **DONE** — `render-check` and retained `scan-navigation` evidence; unknown causes remain partial | `docs/BROWSER_NAVIGATION.md`; no blanket defect verdict |
| 5.2 | URL case normalization | The server silently redirects `/Foo` -> `/foo` (or vice versa) — a canonicalization signal | medium | A (live probe) | id `URL_CASE_REDIRECT` |
| 5.3 | Soft 404 | The page answers 200 but the content is "not found" | **high** | **DONE** as the live `soft404-check` (two deterministic probes, strict verdict) | id `SOFT_404` if the audit needs it |

**Context.** HTTP chains/loops/meta-refresh are covered; these three forms
were a blind spot, and JS redirects and soft-404 are everywhere.

---

## 6. Canonical — graph checks

The implementation at the time of writing caught only the one-step
canonical->non-indexable case. Graph checks over the whole crawl are a
separate class (they need the full page set, which mode B already has).

| # | Name | Checks | Value | Mode | Home |
|---|---|---|---|---|---|
| 6.1 | Canonical chain | A->B, and B canonicalizes elsewhere | **high** | **DONE** — `CANONICAL_CHAIN` | `check_canonical_directives` |
| 6.2 | Canonical -> redirect | Canonical points at a 3xx URL | **high** | **DONE** — `CANONICAL_TO_REDIRECT` | same |
| 6.3 | Canonical -> 4xx/5xx | Canonical points at a broken URL | **high** | **DONE** — `CANONICAL_TARGET_ERROR` | `check_canonical_target_error` |
| 6.4 | Canonical -> homepage (stamp) | Distinct sections canonicalize to homepage | medium | **DONE** — `CANONICAL_HOMEPAGE_GROUP` | `check_canonical_homepage_group` |
| 6.5 | Canonical header vs tag | `<link rel=canonical>` disagrees with the HTTP `Link: rel=canonical` | medium | B+ (SF catches HTTP canonical when configured) / A | id `CANONICAL_HEADER_MISMATCH` |
| 6.6 | Canonical contains a fragment | `<link rel=canonical>` points at a `#fragment`, which the server never sees | low | **DONE** (issue #30) — `CANONICAL_FRAGMENT` | `check_canonical_extra` |
| 6.7 | Canonical outside `<head>` | The tag is placed in `<body>` and is silently ignored | medium | **DONE** (issue #123) — `CANONICAL_OUTSIDE_HEAD`, from a native crawl's own parse tree (`seohead.checks.parser.parse_html`'s `document_position`); the eleven catalogued "outside head"/document-skeleton entries all closed together, see `check_element_position`/`check_document_skeleton` | `rules.py` |
| 6.8 | Invalid attribute in canonical annotation | Malformed `rel=canonical` markup (e.g. `rel="canonical "`, missing `href`) | low | **A only** — same reason as 6.7 (issue #30) | new live check |

---

## 7. hreflang — relation and target detail

The SF Issues tab groups hreflang defects. Separate registry IDs make each
measured relation and target defect actionable in the report.

| # | Name | Checks | Value | Mode | Home |
|---|---|---|---|---|---|
| 7.1 | No x-default | hreflang present, no `x-default` entry | medium | **DONE** (issue #30) — `HREFLANG_MISSING_XDEFAULT` | `inlinks.py` |
| 7.2 | No self-reference | No hreflang link to the page itself | medium | **DONE** (issue #30) — `HREFLANG_MISSING_SELF_REFERENCE` | `inlinks.py` |
| 7.3 | lang ≠ page language | Self-reference hreflang disagrees with `<html lang>` | medium | B+ — still open; `<html lang>` is not a column `Internal:All` exports | `HREFLANG_LANG_MISMATCH` |
| 7.4 | Relative URL in hreflang | `href` not absolute | low | B+ — still open; the bulk hreflang export resolves `href` before SF writes it, so a relative-vs-absolute check needs the raw markup | `HREFLANG_RELATIVE` |
| 7.5 | No return link | A->B exists, B->A does not | **high** | **DONE** — `HREFLANG_MISSING_RETURN_LINK`; native retained declarations distinguish a measured missing return from an unmeasured target (#825) | `inlinks.py` |
| 7.6 | hreflang -> non-canonical | The target is itself canonicalized elsewhere | medium | **DONE** (issue #30) — `HREFLANG_NOT_CANONICAL` | `inlinks.py` |
| 7.7 | hreflang -> noindex | The target is closed from indexing | medium | **DONE** — `HREFLANG_NOINDEX_TARGET` over observed target evidence (#825) | `inlinks.py` |
| 7.8 | hreflang -> redirect/4xx | The target is broken or redirecting | **high** | **DONE** — `HREFLANG_BROKEN_TARGET` | `inlinks.py` |
| 7.9 | Duplicate lang per target | One URL listed with different `lang`s from different sources | medium | B (graph) — still open (distinct from 7.10: this is one *target* with conflicting incoming langs, not one *source* repeating a lang) | `HREFLANG_MULTI_LANG` |
| 7.10 | Duplicate lang per source | One page declares the same hreflang value more than once | medium | **DONE** (issue #30) — `HREFLANG_MULTIPLE_ENTRIES` | `inlinks.py` |
| 7.11 | Malformed language/region code | hreflang value fails ISO 639-1/3166-1 (e.g. `en-UK`) | medium | **DONE** (issue #30) — `HREFLANG_INVALID_CODE`, reusing `seohead/checks/hreflang.py`'s `code_error` | `inlinks.py` |
| 7.12 | Outside `<head>` | The `<link rel=alternate hreflang>` tag is placed in `<body>` | medium | **DONE** — `HREFLANG_OUTSIDE_HEAD` uses retained native element-position evidence and offline reanalysis; ordinary SF exports without that evidence stay skipped | `check_element_position` |

**Context.** The live `seo_hreflang_check` (x-default, self-reference,
duplicates, malformed codes) validates one URL's own markup by live fetch;
7.1/7.2/7.5/7.6/7.7/7.10/7.11 above now cover the equivalent ground for a whole
crawl from the bulk hreflang export, reusing that tool's ISO validator
instead of re-implementing it (issues #30 and #825). 7.3/7.4/7.9 remain
open.

---

## 8. Link graph and anchors

| # | Name | Checks | Value | Mode | Home |
|---|---|---|---|---|---|
| 8.1 | follow/nofollow conflict per target | One URL receives both follow and nofollow links from different pages | medium | **DONE** — `FOLLOW_AND_NOFOLLOW_INLINKS` | `crawl/link_findings.py` (native crawl, not the `*:Inlinks` export) |
| 8.2 | Nofollow onto an indexable page | Verified indexable page is linked only via nofollow | medium | **DONE** — `ONLY_NOFOLLOW_INLINKS`, withheld on partial crawl | `inlinks.py` |
| 8.3 | External without nofollow | The page leaks equity without `rel=nofollow/sponsored/ugc` | low | B (Inlinks: external + rel) | id `EXTERNAL_DOFOLLOW` |
| 8.4 | HTTP links on an HTTPS page | Explicit internal `http://` anchor on a fetched HTTPS page | low | **DONE** — `HTTP_LINK_ON_HTTPS`, with observed redirect convergence excluded | `crawl/link_findings.py` |
| 8.5 | Localhost/127.0.0.1 in links | A forgotten dev artefact | medium | **DONE** — `OUTLINK_TO_LOCALHOST` | `crawl/link_findings.py` |
| 8.6 | Generic anchor text | "click here"/"learn more" and localized equivalents | medium | **DONE** — `GENERIC_ANCHOR_TEXT` | `inlinks.py` |
| 8.7 | Anchor without title (duplicating) | A link without `title=""` with an implicit anchor | low | B+ | id `ANCHOR_NO_TITLE` |

**Context.** `*:Inlinks` already carries anchor/follow/rel/host — most
graph checks land in `inlinks.py` with almost no new math.

---

## 9. Images (detail over `IMG_*`)

| # | Name | Checks | Value | Mode | Home |
|---|---|---|---|---|---|
| 9.1 | "no alt" vs "empty alt" | Attribute missing vs `alt=""` (the latter often deliberate for decorative images) | medium | **DONE** — `IMG_MISSING_ALT_ATTRIBUTE` | `tools/parser.py`, `crawl/collect.py`, `sf/core/rules.py` |
| 9.2 | Long alt (>100 chars) | Overly long alt text | low | **DONE** — `IMG_ALT_TOO_LONG` | `tools/parser.py`, `crawl/collect.py`, `sf/core/rules.py` |
| 9.3 | `<picture>` without `<img>` | Lost fallback inside picture | low | A (HTML) | id `PICTURE_NO_IMG` |
| 9.4 | Modern format (WebP/AVIF) | Legacy formats where WebP/AVIF fits | medium | B+ / A | id `IMG_LEGACY_FORMAT` |
| 9.5 | Responsiveness (srcset) | No `srcset`/`sizes` on large images | low | A (HTML) | id `IMG_NOT_RESPONSIVE` |
| 9.6 | Lazy-loading | Above-fold images without `loading=lazy`, or the opposite | low | A (HTML/render) | id `IMG_NO_LAZYLOAD` |
| 9.7 | Filename quality | Names like `IMG_1234.jpg` with no keywords | low | B (URL parse of src) | id `IMG_GENERIC_FILENAME` |

---

## 10. URL hygiene (addition to `URL_*`)

| # | Name | Checks | Value | Mode | Home |
|---|---|---|---|---|---|
| 10.1 | Tracking parameters | `utm_*`, `gclid`, `fbclid`, `msclkid` in an indexable URL | medium | **DONE** — `URL_TRACKING_PARAMS` | `check_url_extra` |
| 10.2 | Session ID in URL | Known session parameter on an indexable URL | medium | **DONE** — `URL_SESSION_ID`, values redacted | `check_url_hygiene` |
| 10.3 | Stop words in slug | "the/a/and" and Russian equivalents in the path | low | B | id `URL_STOP_WORDS` |
| 10.4 | Trailing-slash desync | Comparable slash/non-slash variants both serve indexable content | medium | **DONE** — `URL_TRAILING_SLASH_INCONSISTENT`; path case/convergence preserved | `check_url_hygiene` |
| 10.5 | WWW canonicalization | No redirect www <-> non-www | medium | **DONE** as the live `mirror-check` (no registry id) | id `NO_WWW_REDIRECT` |

---

## 11. HTML validity

| # | Name | Checks | Value | Mode | Home |
|---|---|---|---|---|---|
| 11.1 | Duplicate `id`s | Bounded repeated literal IDs in parsed HTML | medium | **DONE** — `DUPLICATE_ID`; not full HTML conformance | `check_html_structure` |
| 11.2 | No `<!DOCTYPE>` | Quirks mode | low | **DONE** — `MISSING_DOCTYPE` | `check_doctype` |
| 11.3 | No charset | Encoding not declared | low | **DONE** — `MISSING_CHARSET` | `check_charset` |
| 11.4 | Multiple `<head>`/structural dupes | Head/body counts and order | low | **DONE** — `HEAD_MULTIPLE`, `BODY_MULTIPLE`, `HEAD_NOT_FIRST` | `check_document_skeleton` |
| 11.5 | Block elements in `<head>` | Observed invalid head element | low | **DONE** — `INVALID_HEAD_ELEMENT` | `check_document_skeleton` |
| 11.6 | Lorem ipsum / placeholder | Content Lorem plus literal template markers in declarations | medium | **DONE** — `LOREM_IPSUM_PLACEHOLDER` / `PLACEHOLDER_MARKER`; ordinary prose stays unclassified | `rules.py` |
| 11.7 | MIME vs extension | Declared Content-Type conflicts with known filename extension | low | **DONE** — `DECLARED_MIME_MISMATCH`; not full MIME validation | `check_html_structure` |

---

## 12. Pagination (currently only `PAGINATION_NONINDEXABLE`)

| # | Name | Checks | Value | Mode | Home |
|---|---|---|---|---|---|
| 12.1 | Invalid rel=next/prev | The `href` does not parse as a URL | medium | B (Internal:All: rel_next/rel_prev) | extend `check_pagination`, id `PAGINATION_BROKEN` |
| 12.2 | Canonical chain on pagination | The whole series canonicalizes to page one — the tail is lost | medium | B (canonical x rel_next graph) | id `PAGINATION_CANONICAL_CHAIN` |
| 12.3 | Pagination loop | rel=next forms a cycle | medium | B (graph) | id `PAGINATION_LOOP` |
| 12.4 | Sequence gap | `/page/2` without `/page/3` while `/page/4` exists | low | **DONE** (issue #385) — `PAGINATION_SEQUENCE_ERROR`, over the `rel="next"` graph rather than over URL numbers alone; a series with an irregular offset scheme, or one whose URLs do not state a page number, is named as unjudged instead of reported (stride series with a dominant step are judged, #1057) | `check_pagination_sequence` |
| 12.5 | Pagination orphan | A pagination page without internal inlinks | medium | B (Inlinks x rel_next) | id `PAGINATION_ORPHAN` |

---

## 13. Schema / structured data (detail)

| # | Name | Checks | Value | Mode | Home |
|---|---|---|---|---|---|
| 13.1 | Required fields per type | Product->price/availability, Article->datePublished/author, Review->rating, LocalBusiness->address, FAQ->mainEntity, Video->duration | **high** | **mostly DONE** in the live `schema-check` (vocabulary + Google rich-result eligibility per type); audit ids still absent | new `schema_fields.py` |
| 13.2 | Type-specific scenarios | Breadcrumb/FAQ/Video/LocalBusiness/... where they are relevant | medium | B (SF "Schema Type") | id `SCHEMA_TYPE_OPPORTUNITY` |
| 13.3 | Schema drift | Markup diverges from visible content (JSON-LD price ≠ page price) | medium | A (JSON-LD + render) | id `SCHEMA_CONTENT_DRIFT` |

**Context.** The audit itself still has only `SCHEMA_VALIDATION_ERROR`
(error count) and `STRUCTURED_DATA_MISSING` (none at all). The live
`schema-check`/`schema-build` pair already validates two layers and builds
graphs; wiring its findings into the audit document is the missing half.

---

## 14. Security headers (addition to `MISSING_HSTS`/`MIXED_CONTENT`)

| # | Name | Checks | Value | Mode | Home |
|---|---|---|---|---|---|
| 14.1 | No CSP | `Content-Security-Policy` missing (header or meta) | medium | **DONE** in the live `security-check` (grade includes CSP) | id `MISSING_CSP` |
| 14.2 | No X-Content-Type-Options | Header ≠ `nosniff` | low | **DONE** in `security-check` | id `MISSING_NO_SNIFF` |
| 14.3 | No Referrer-Policy | Missing referrer policy | low | **DONE** in `security-check` | id `MISSING_REFERRER_POLICY` |
| 14.4 | No Permissions-Policy | Missing permissions-policy | low | **DONE** in `security-check` | id `MISSING_PERMISSIONS_POLICY` |
| 14.5 | SSL certificate lifetime | Expiring/expired (<30 days) | **high** | **DONE** in the live `domain-profile` (TLS section) | id `SSL_EXPIRING` |
| 14.6 | Secrets leaked in HTML | API keys/tokens/AKIA patterns in source | **high** | A (HTML + regex) | id `LEAKED_SECRETS` |
| 14.7 | Forms on HTTP | `<form>` on an http page or `action="http://…"` | medium | **DONE** — `FORM_URL_INSECURE` (insecure action) / `FORM_ON_HTTP_URL` (password field on an http page) | `crawl/link_findings.py` |

**Context.** SF does not export security headers in bulk — a separate HTTP
probe is needed (cheap, but a live network call). The live `security-check`
already scores the header set; registry ids would surface it in the crawl
audit. Leaked secrets and SSL lifetime are high-value audit findings that
are formally not SEO but expected in a technical audit.

---

## 15. Social / Open Graph

| # | Name | Checks | Value | Mode | Home |
|---|---|---|---|---|---|
| 15.1 | OG:title/description/url/image present | Basic OG tags exist | medium | **DONE** — `OG_MISSING` (og:title focus) | `rules.py` |
| 15.2 | OG:image pixel size | 1200x630 recommended, >=200x200 minimum | medium | A (fetch the image) | id `OG_IMAGE_TOO_SMALL` |
| 15.3 | OG:url vs canonical | `og:url` must match canonical | medium | B | id `OG_URL_CANONICAL_MISMATCH` |
| 15.4 | Twitter Card | `twitter:card`/`twitter:image` present | low | **DONE** as the live `social-meta-check` (required/recommended checklist) | id `TWITTER_CARD_MISSING` |

---

## 16. Legal / compliance

| # | Name | Checks | Value | Mode | Home |
|---|---|---|---|---|---|
| 16.1 | Cookie-consent / CMP | A consent mechanism when trackers are present (18 platforms + regex) | low | A (HTML: classes ids/scripts) | id `NO_COOKIE_CONSENT` |

**Context.** Not an SEO factor, but part of a technical/legal audit.

---

## 17. GEO / AI generation

| # | Name | Checks | Value | Mode | Home |
|---|---|---|---|---|---|
| 17.1 | AI bots: training vs retrieval | Separate accounting of blocked training bots (GPTBot, ClaudeBot…) and retrieval bots (OAI-SearchBot, PerplexityBot…) | medium | **DONE** as the live `ai-bots-check` (per-bot role: training/retrieval/user) | registry ids if needed |
| 17.2 | Semantic structure / citability | `<article>`, section headings, LLM-parseable blocks | medium | **DONE** as the live `citability-check` (4x25 scoring) | `geo.py`, id `WEAK_SEMANTIC_STRUCTURE` |

---

## 18. Duplicates and content (additions)

| # | Name | Checks | Value | Mode | Home |
|---|---|---|---|---|---|
| 18.1 | Keyword stuffing | Word density >10% or >=3 overheated words at >=200 content words | medium | B (word count) / A (tokenization) | id `KEYWORD_STUFFING` |
| 18.2 | Title = brand only | The title is nothing but a brand/stamp (not the same as `TITLE_TEMPLATED`) | low | B | id `TITLE_BRAND_ONLY` |

> **Near-duplicates are closed** (`NEAR_DUPLICATE` reads the native SF
> column). Exact duplicates by hash — `DUPLICATE_BY_HASH`. The live
> `duplicate-check` scales near-duplicate search to tens of thousands of
> texts via simhash + LSH.

---

## Priorities

The generated [reconciliation](COVERAGE_GAPS_RECONCILIATION.md) owns the
current priority order. It excludes covered and explicitly out-of-scope rows,
then ranks remaining gaps by the map's stated value, declared B/B+/A
feasibility, and row number. This avoids leaving shipped checks in an
implementation queue.
