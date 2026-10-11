# Checklist coverage audit (issue #30)

> **Superseded by [COVERAGE_SF_ISSUES.md](COVERAGE_SF_ISSUES.md).** This document was written
> without the catalogue itself — it re-verified somebody else's category counts and said so.
> The catalogue is now mapped item by item, generated from
> `seohead/sf/core/sf_issue_map.py` and held to the registry by a test. Read that instead;
> this is kept for the reasoning it records about individual checks.

**Purpose.** Issue #30 asked for a coverage audit of the SF-crawl audit
registry (`seohead/sf/core/registry.py`) against an external ~320-item
technical-SEO issue catalogue, organized into 24 categories (Response Codes,
Security, URL, Page Titles, Meta Description, H1, H2, Content, Images,
Canonicals, Pagination, Directives, Hreflang, JavaScript, Links, AMP,
Structured Data, Sitemaps, PageSpeed, Mobile, Accessibility, Analytics,
Search Console, Validation). This document is the result: what is verified
covered, what is a verified gap, what remains unverified, and what the
registry cannot cover without an architecture change.

**This document does not itself carry the external catalogue.** The catalogue
lives outside this repository; issue #30 already did the category-by-category
counting against it. What follows re-verifies that counting against the real
registry (139 checks when this audit was written, up from 104 at the time of issue #30; the registry
has 204 checks now, see [CHECKS.md](CHECKS.md)) and corrects it where the counting was wrong, rather than re-deriving the count from scratch. Where a claim could
not be checked without the catalogue's exact item text, that is stated rather
than guessed.

**Methodology.** Every claim below is either:

- **grep-verified** — checked directly against `seohead/sf/core/registry.py`,
  `seohead/sf/core/rules.py`, `seohead/sf/core/inlinks.py`, and
  `seohead/sf/core/normalize.py` (the field maps that say what a Screaming
  Frog export can and cannot carry); or
- **carried forward** — the issue's own CONFIRMED section, which was already
  hand-verified, and is trusted here without re-checking every line; or
- **unverifiable here** — the issue's own CANDIDATE rows for categories where
  the item-by-item catalogue text was not available to this audit, so the
  "none apparent" / "N of M" counts are repeated as-is with that caveat.

A category not re-examined in detail below is not implicitly confirmed —
see the "Not independently re-verified" section.

## What changed in this PR

Five categories the issue marked **CONFIRMED** had a defined, small set of
missing items backed by data the registry already loads. Those are now
closed:

| Category | Before | After | Closed by this PR | Still open |
|---|---|---|---|---|
| Hreflang | 2 of ~12 | 7 of ~12 | `HREFLANG_INVALID_CODE`, `HREFLANG_MULTIPLE_ENTRIES`, `HREFLANG_MISSING_SELF_REFERENCE`, `HREFLANG_MISSING_XDEFAULT`, `HREFLANG_NOT_CANONICAL` | Outside `<head>` (needs raw HTML — see below); reciprocity/return-links tracked in #15 |
| Directives | 6 of 10 | 8 of 10 | `NOTRANSLATE`, `UNAVAILABLE_AFTER` | Outside `<head>` (needs raw HTML); `NoODP`/`NoYDIR` — **not doing**, dead standards (DMOZ closed 2017, Yahoo Directory closed 2014) |
| Canonicals | 7 of 10 | 8 of 10 | `CANONICAL_FRAGMENT` | Outside `<head>`, Invalid Attribute In Annotation — both need raw HTML |

Implementation notes:

- The five new hreflang checks (`seohead/sf/core/inlinks.py`,
  `check_hreflang_quality`) read the same Bulk Export -> Links -> All
  Hreflang report the existing `HREFLANG_BROKEN_TARGET` check already
  consumes — no new export requirement. The ISO 639-1/3166-1 language-code
  validator is **not reimplemented**: it imports `code_error` from
  `seohead/checks/hreflang.py`, the module already shipped for the live,
  single-URL `seo_hreflang_check` tool. That tool validates one page's own
  markup by live fetch; the registry checks now validate the same properties
  (code validity, self-reference, x-default, duplicate entries) across a
  whole crawl from the bulk export, plus one property the live tool does not
  check because it needs the crawl graph (`HREFLANG_NOT_CANONICAL`).
- `NOTRANSLATE` and `UNAVAILABLE_AFTER` slot into the existing
  `check_directives_extra` (`rules.py`), reading the same `meta_robots`/
  `x_robots` tokens as `NOARCHIVE`/`NOSNIPPET`/`NOIMAGEINDEX` already do —
  `unavailable_after`'s value survives `robots_directives()`'s tokenizer
  intact (it is in `_VALUED_DIRECTIVES` in `seohead/checks/parser.py`), so the
  deindex date is captured in the issue's `details`, not just the directive's
  presence.
- `CANONICAL_FRAGMENT` slots into `check_canonical_extra`, checking the
  existing `canonical` field for a URL fragment.
- All five hreflang checks share one honest skip: if `all_hreflang` is
  absent, all five report `skipped`, never a false "zero found". The three
  non-hreflang checks need no skip of their own — `Internal:All` is always
  required for a run, so the columns they read are always present (they may
  simply find nothing, same as their sibling checks `NOARCHIVE`/
  `CANONICAL_RELATIVE`).
- Registry size: 96 -> 104. Severity breakdown: 8 critical (unchanged), 39 ->
  44 warnings, 49 -> 52 notices. `.claude/skills/sf-analyzer/reference/checks.md`,
  `README.md`, `PROVENANCE.md`, and `tests/test_docs_drift.py` are updated to
  match; `tests/test_check_preconditions.py`'s `checks_silent` ratchet moved
  from 56 to 59 (the three new non-hreflang checks find nothing in the
  existing example fixture, so they run clean rather than skip — expected,
  not a regression; the five hreflang checks are honestly skipped instead,
  since the fixture has no `all_hreflang` export).

## Corrections to the CANDIDATE table

Three of the issue's own CANDIDATE rows turned out to be wrong on
re-verification against the real registry — exactly the failure mode the
issue itself warned about ("a heuristic, not a verified fact"):

- **Page Titles — "Same as H1" is not a gap.** `TITLE_EQUALS_H1` already
  exists in the registry (`seohead/sf/core/rules.py`, `check_titles`) and
  fires when a page's `<title>` and H1 are identical. The CANDIDATE table
  listed this under both Page Titles and H1/H2 as a likely gap; it is
  covered under both readings, since the check is symmetric by definition.
- **Analytics is not "3 of 4 covered" — it is 0 of 4.** `grep -rn
  "analytic\|bounce\|ga_\|google_analytics" seohead/sf/core/*.py` returns
  nothing. There is no Google Analytics client anywhere in the toolkit
  (`seohead/data_sources/` has Yandex Metrika, DataForSEO, Arsenkin, spend,
  and region data — no GA). No check in the registry has an analytics-shaped
  `source` value. Whatever the original keyword match counted as "3 covered"
  did not correspond to a real check reading real analytics data. This is a
  **bigger** gap than the CONFIRMED section already named for Search
  Console: both integrations are simply absent, and every catalogue item
  gated on either one is unimplementable until a client for one of them
  exists.
- **Structured Data — "Rich Result Validation Warnings" was investigated,
  not implemented.** `SCHEMA_VALIDATION_ERROR` reads a `validation_errors`
  field mapped from the SF column `Validation Errors`
  (`seohead/sf/core/normalize.py`). A sibling `Validation Warnings` column
  plausibly exists in a real Screaming Frog export (errors/warnings is a
  common split for this kind of report), which would make a
  `SCHEMA_VALIDATION_WARNING` check a one-line mirror of the existing one.
  It is **not added here**: the exact column header cannot be confirmed
  against a real SF export from this repository (no fixture or example
  carries a Structured Data validation export), and this codebase's stated
  bar for `INTERNAL_FIELD_MAP` entries is "confirmed against a real SF 19.4
  Internal:All export" — guessing a header name would risk the same silent
  wrongness the honest-skip design exists to prevent (a column name typo
  degrades to "always skip", which hides the mistake instead of surfacing
  it). Whoever next has a real SF export with a Structured Data validation
  report should confirm the header and add the one field mapping plus check.

## Verified-real gaps not closed here (architecture-limited)

These need data the SF `Internal:All`/bulk-export CSV pipeline does not and
structurally cannot carry — a raw-HTML/DOM pass, a live per-page fetch, or an
external API integration. Consistent with how the issue itself scoped out
CSS/JS delivery and accessibility, these are named but not built in this PR:

- **"Outside `<head>`" for canonical, directives, and hreflang.** Screaming
  Frog's CSV exports report *whether* a tag exists and *what* it says, never
  *where in the document* it sits. A canonical or hreflang `<link>` (or a
  robots meta tag) placed in `<body>` is invalid and silently ignored by
  crawlers, but detecting that requires parsing the actual HTML tree, which
  none of the three relevant exports (`Internal:All`, the canonical columns,
  the bulk hreflang export) provide. This affects all three CONFIRMED
  categories identically and is the one item left open in each.
- **Canonical "Invalid Attribute In Annotation"** — malformed
  `rel=canonical` markup (wrong `rel` value, missing `href`) needs the same
  raw-HTML pass as above.
- **Content "Lorem Ipsum placeholder text"** — `Internal:All` carries
  `word_count`/`text_ratio`, never the body text itself, so pattern-matching
  placeholder copy is not possible from the CSV path. Would need Custom
  Extraction/XPath configured in SF (mode B+, not default) or a live fetch.
- **H1/H2 "Non-sequential heading order"** — `Internal:All` exports at most
  two values each for H1 and H2 (`H1-1`, `H1-2`, `H2-1`, `H2-2`), never H3–H6
  and never document order. Detecting a skipped level (H1 -> H3) needs the
  full heading tree, which this export does not carry. Already tracked as
  §3.5 in `docs/COVERAGE_GAPS.md` under accessibility, since it is the same
  missing data.
- **Mobile "Viewport Not Set"** — no `Internal:All` column carries the
  `<meta name=viewport>` tag by default; it would need Custom Extraction
  or a live/rendered fetch.
- **Security "Form URL Insecure"** — `<form>` markup and its `action`
  attribute are not in `Internal:All`. Already tracked as §14.7 in
  `docs/COVERAGE_GAPS.md` (`INSECURE_FORM`).
- **Validation "`<body>` preceding `<html>`" / "`<head>` not first"** — HTML
  structural validity needs a DOM parse of the raw document; nothing in the
  CSV path carries document order below the tags SF explicitly extracts.

## Verified "not doing"

- **Directives: `NoODP`/`NoYDIR`.** DMOZ (ODP) closed in 2017 and the Yahoo
  Directory closed in 2014; no search engine has read these directives in
  years. Recorded so nobody re-adds them from a future keyword match against
  the catalogue.
- **JavaScript: "Old AJAX Crawling Scheme" (`_escaped_fragment_`).** Real and
  matchable (a page could still carry the meta tag or the `#!` URL
  fragment), but the scheme itself was deprecated by Google in 2015 and
  unsupported since 2018. Implementing a check for it has the same shape as
  `NoODP`/`NoYDIR`: technically buildable, meaningless in practice. Not
  doing, for the same reason.

## Gaps the original audit itself never scored

The issue's header lists 24 categories but its CONFIRMED/CANDIDATE tables
only address 22 of them — **Links** and **AMP** never got a coverage
estimate at all, in either direction:

- **AMP**: exactly one check exists, `AMPHTML_PRESENT` — presence-only
  (whether an AMP link is declared), with no validity or consistency
  checking of the AMP version itself. Against a catalogue category that
  likely covers several AMP-specific validity items, this is a thin single
  check, not "AMP: covered" or "AMP: gap" — nobody stated either.
  (AMP itself is a shrinking concern post-2021 Google Search changes, which
  may be exactly why it fell out of the original pass; naming that omission
  here rather than silently leaving it absent from both tables.)
- **Links**: well covered relative to a typical catalogue — nine checks
  touch the link graph directly: `BROKEN_INTERNAL_LINK`, `LINK_TO_5XX`,
  `INTERNAL_LINK_TO_REDIRECT`, `BROKEN_EXTERNAL_LINK`,
  `EXTERNAL_LINK_TO_REDIRECT`, `GENERIC_ANCHOR_TEXT`,
  `NO_INTERNAL_OUTLINKS`, `HIGH_EXTERNAL_OUTLINKS`, `HIGH_OUTLINKS`. Several
  more link-graph items are already tracked as open in
  `docs/COVERAGE_GAPS.md` §8 (follow/nofollow conflicts, HTTP links on HTTPS
  pages, localhost links) — that section is the more precise source for this
  category's remaining gaps than a fresh count against the external
  catalogue would be.

## URL category: one real distinction the CANDIDATE row blurred

The CANDIDATE table listed "Parameters (tracking/faceted-param detection as
its own check)" as a likely gap. On inspection this conflates two different,
already-partially-covered things:

- **Tracking parameters** (`utm_*`, `gclid`, `fbclid`, …) — covered by
  `URL_TRACKING_PARAMS` (`check_url_extra`), which already exists and was
  not part of the original 96.
- **Faceted-navigation parameters** (filter/sort/pagination combinations
  that multiply crawlable URLs) — not covered by any check, and genuinely
  distinct: it needs clustering parameter *combinations* across the crawl
  to spot combinatorial explosion, not just recognizing a known parameter
  name. `URL_HAS_PARAMS` (generic "has a parameter, no canonical") is the
  closest existing check but does not attempt this. Left open; a design for
  it belongs with `docs/COVERAGE_GAPS.md` §10 (URL hygiene), not as a
  one-line addition.

## Not independently re-verified

Given the size of the external catalogue (~320 items) and that this audit
was not handed the catalogue's item-by-item text, the following CANDIDATE
rows are repeated from the issue as-is, with the same "keyword-matched, not
hand-verified" caveat the issue itself attached — this audit did not have
grounds to confirm or refute them beyond a registry `grep` for the exact
item mentioned (done above where an item was named): Response Codes (15/15,
"none apparent"), Security (12/11, gap: Form URL Insecure — item-name
verified real above, count not re-checked), Meta Description (8/7, "none
apparent"), Content beyond Lorem Ipsum (11/9 — Semantically Similar is
already cross-referenced to #19/#15), Images (7/7, "none apparent" —
background-image detection already cross-referenced separately), Pagination
(7/7, "none apparent"), Sitemaps (6/6, "none apparent"), PageSpeed (19/~10 —
already extensively scoped in the issue itself; Core Web Vitals-adjacent
items need Lighthouse/CrUX-style lab data, a different integration; also see
`docs/COVERAGE_GAPS.md` §1).

**Search Console, CSS/JS weight & delivery, and Accessibility (92 WCAG/axe
items)** remain exactly as the issue scoped them: no data-source client, no
minification/unused-code analysis, and no axe-core integration exist in this
toolkit, and none of the three is a registry-row addition — each needs its
own design, as the issue already said. Nothing in this PR changes that
assessment.

## Closing this issue

Per the issue's own "what to do" section, every row should end up either
tracked as its own issue or explicitly marked "not doing, because ___". This
PR does the latter for the items in "Verified not doing" above, closes the
CONFIRMED categories' cheap items in code, and leaves the "architecture-
limited" and "not independently re-verified" sections as the honest
remainder — recommended as follow-up issues rather than opened here, so each
gets scoped with the same care this document tried to apply rather than
being filed as a rubber-stamped batch.
