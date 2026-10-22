# Scenario 9 — Canonical basics: the defects that are a typo, not a strategy

## The question

> Somebody told us our canonicals are a mess. Before I open a ticket with the developers,
> which pages have one at all, and is what they have even a URL?

Three canonical defects need no theory to judge: the element is absent, its value is not an
absolute URL, or its value carries a `#fragment` the server will never receive. This chain
finds those, and stops before the ones that need a person.

## Covers

- **Canonicals** — Missing · Canonical Is Relative · Contains Fragment URL · Outside <head> · Invalid Attribute In Annotation

## The chain

**1. Read one page first, so you know what the markup looks like.**

```bash
seohead parse --url https://example.com/page
```

`parse` resolves the canonical against the document's own base, so the `canonical` field it
prints is the absolute form a browser would follow. That is the right answer for "where does
this point", and the wrong one for "what does the attribute literally say" — step 4 is where
the difference starts to matter.

**2. Crawl the site once, and let the audit run over what it collected.**

```bash
seohead crawl-site --url https://example.com --out-dir ./run
```

`audit.json` now carries `CANONICAL_MISSING` for every indexable HTML page with no canonical
element, and `CANONICAL_FRAGMENT` for every canonical whose value contains a fragment. The
fragment case is worth stating plainly in the ticket: the part after `#` is never sent to the
server, so a canonical pointing at one identifies nothing.

**3. Read `CANONICAL_OUTSIDE_HEAD`, the classic case a source diff never catches.** A browser
closes `<head>` at the first element that does not belong there — commonly a stray `<script>`
or tag a plugin injected — and everything after that point, canonical included, is read from
`<body>` instead. Google ignores it there. The tag looks perfectly correct in the source; only
the parsed tree, which step 2's crawl already resolved, tells the difference:

```json
{
  "check": "CANONICAL_OUTSIDE_HEAD",
  "severity": "critical",
  "target_url": "https://example.com/page",
  "message": "The canonical link is outside <head> once the parser resolves the document"
}
```

The same canonical in a clean `<head>` never fires this. An analysed crawler export cannot
raise it — the fact only exists where the parse tree was actually built.

Native retained scans also preserve raw canonical `rel` and `href`, together with
`hreflang`, `lang`, `media` and `type` attributes. `CANONICAL_INVALID_ATTRIBUTE` reports
those four attributes because [Google ignores such canonical annotations](https://developers.google.com/search/docs/crawling-indexing/consolidate-duplicate-urls), and reports
malformed relation separators or unusable canonical targets with the original declaration.
Missing or capped retained annotations remain unmeasured; an SF export without raw attributes
cannot establish that this check is clean.

**4. Read the counts, not the issue list, first.**

`summary.by_check` is one line per check. If `CANONICAL_MISSING` is larger than
`totals.html_pages`, the finding is arithmetic rather than a defect. Step 6 is the mechanical
version of that suspicion.

**5. For the literal attribute value, read a Screaming Frog export instead.**

```bash
seohead sf run --exports-dir ./exports --out report
```

`CANONICAL_RELATIVE` fires on the raw value in Internal:All's `Canonical Link Element 1`
column. A native crawl cannot raise it, because the parser has already resolved
`href="/page"` to an absolute URL by the time the check sees it — the same resolution a
browser performs. Relative canonicals are legal and usually work; they stop working the moment
the page is served under a second base, which is exactly when nobody is looking.

**6. Scan the run before quoting a number from it.**

```bash
seohead log-scan --run ./run
```

**7. Hand it over.**

```bash
seohead report-build --audit ./run/audit.json --format md --out ./canonicals.md
```

## What comes out

The counts, then the addresses. An illustrative shape of the first half:

```json
"by_check": {
  "CANONICAL_MISSING": 12,
  "CANONICAL_FRAGMENT": 3
}
```

and one finding from the second:

```json
{
  "check": "CANONICAL_FRAGMENT",
  "severity": "notice",
  "target_url": "https://example.com/services/pumps",
  "message": "Canonical URL contains a #fragment",
  "details": {"canonical": "https://example.com/services/pumps#specs"}
}
```

## Check an explicit pagination or filter policy

The audit can also compare matching URLs with a site's declared canonical policy. It never decides
that pagination or a filter should use a particular canonical by default. Configure URL regexes
and an expected target in `crawl.json` under `analysis.canonical_policy`, or in the SF
`config.json` at the top level. For example, a project may require `/catalog/page/2/` to
self-canonicalize while a color-filter URL points to the category landing page:

```json
{
  "canonical_policy": {
    "pagination": [
      {"pattern": "/catalog/page/[2-9][0-9]*/?$", "policy": "self"}
    ],
    "filters": [
      {
        "pattern": "[?&]color=",
        "policy": "landing",
        "target": "https://shop.example.test/catalog/"
      }
    ]
  }
}
```

The native-crawl form wraps that object in `analysis`. Patterns are searched against absolute source
URLs; within each category, the first matching rule wins. A first-page or landing policy requires
an explicit target. `PAGINATION_CANONICAL_POLICY` and `FILTER_CANONICAL_POLICY` are separate
findings and include source, declared canonical, expected canonical, selected policy, and matched
pattern. Empty policy lists are skipped as unmeasured, and a target missing from the crawl keeps
that category unavailable. Check #824 owns canonical-target response status and broad homepage
canonical groups.

## What it costs

- One request per crawled page. Nothing is fetched twice, and nothing is fetched off-host.
- No paid API at any step. `sf run --exports-dir` is a local read of files you already have.
- Minutes on a small site; the crawl's own pace is set by `speed.min_delay_seconds`.

## What it cannot answer

- **Whether the canonical points at the right page when no project policy is declared.**
  "Present and absolute" is structural. Whether `/product/blue-widget` should defer to
  `/product/widget` is a person's call about what the two pages are for; these policy checks
  compare against the URLs the operator configured and do not choose the strategy.
- **Whether Google honours it.** A canonical is a hint. Nothing in this loop reads the index,
  so it cannot tell you which URL was actually selected.
- **A canonical injected by JavaScript.** The crawl reads the served HTML. If the tag is added
  after hydration, run the [rendering scenario](rendering.md) first and read
  `pages_by_representation` before believing a `CANONICAL_MISSING` count.
- **Two canonical elements on one page.** A native crawl records the first one only; see the
  [canonical conflicts scenario](canonical-conflicts.md).
- **A canonical sent as an HTTP `Link` header.** Only the HTML element is read.
