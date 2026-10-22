# Scenario 52 — Oversized documents, and the responses the crawler had to truncate

## The question

> One page in the catalogue takes forever to open on a phone. Is the page heavy, or is it the
> images?

Those are two different measurements that get reported as one complaint. The HTML document is
what the crawler downloads and parses; everything the page then loads is a separate budget. A
chain that mixes them produces a task nobody can act on.

## Covers

- **Validation** — HTML Document Over 2MB · Resource Over 2MB · Missing <head> Tag · Multiple <head> Tags · Missing <body> Tag · Multiple <body> Tags · Invalid HTML Elements In <head> · <head> Not First In <html> Element · <body> Element Preceding <html>

## The chain

**1. Crawl with the response ceiling set explicitly in the config.** `limits.max_response_bytes`
decides how much of a response is read before parsing stops; the default is 5 MB. Setting it
deliberately is what makes truncation a decision you recorded instead of a silent one.

```bash
seohead crawl-site --url https://example.com --config ./crawl.json --out-dir ./run
```

The recorded size is bytes on the wire, measured before the body is decoded. That is not a
detail: it was wrong by 1.72x until recently, and every weight-based conclusion drawn from it
was unusable.

**2. Scan the run.**

```bash
seohead log-scan --run ./run
```

**3. Read the distribution before reading any single page.** `audit.json` carries
`summary.size_stats_bytes` over the HTML documents in the run — this one is from the example
audit shipped in `docs/examples/`:

```json
"size_stats_bytes": {
  "count": 4, "median": 77500, "p75": 135000, "p90": 234000, "p95": 266999, "max": 300000
}
```

**4. Read `LARGE_HTML` knowing what fired it.** It fires two ways: an absolute threshold
(200 KB by default) and a relative one — a document more than three times the site median, or
above the Tukey upper fence when the distribution has real spread. Each finding carries
`size_bytes`, `site_median`, `ratio`, the page's `rank` by size and whether it was an `outlier`,
so a page can be reported as heavy *for this site* rather than against a number from a blog
post.

**5. Read the document-skeleton facts the same crawl already resolved.** A heavy page and a
malformed one are different complaints that happen to share a symptom, and the second is
invisible to a source diff: a browser closes `<head>` at the first element that does not belong
there, so a canonical or robots directive placed after it is silently read from `<body>`
instead — the page looks correct in the source. `HEAD_MISSING`, `HEAD_MULTIPLE`, `BODY_MISSING`,
and `BODY_MULTIPLE` name how many of each tag the parser actually found (once per page, not once
per stray tag — two `<body>` elements is one `BODY_MULTIPLE`, not two), `INVALID_HEAD_ELEMENT`
names the tag that forced `<head>` to close early, and `HEAD_NOT_FIRST` covers both
"`<head>` not first in `<html>`" and "`<body>` preceding `<html>`" from the published catalogue —
after a browser recovers from either shape of broken markup, both collapse into the same
resolved fact. None of this needs a separate step: it comes from the parse tree step 1's crawl
already built, and — unlike `LARGE_HTML` — an analysed crawler export cannot raise any of
these; the fact only exists where the parse tree was actually built.

**6. Separate the document from what it loads.**

```bash
seohead asset-weight-check --url https://example.com/page
```

Per-resource sizes, totals and the render-blocking list. This is where "it is the images"
becomes true or false.

**7. Report both, with the numbers in the task.**

```bash
seohead report-build --audit ./run/audit.json --format docx --out ./weight.docx
```

## What comes out

A ranked list of documents with each one's size against the site median, and beside it the
per-resource weight of the pages you inspected. Together they say which of the two budgets is
the actual problem, which is the only thing the original question was asking.

`HTML_BLOAT` sits next to `LARGE_HTML` and answers a different question: bytes per word against
the site's own median. A page can be large because it contains a lot, or large because it
contains a lot of markup around very little, and those go to different people.

## What it costs

Nothing beyond the crawl, plus one request per page passed to `asset-weight-check`. Nothing
paid, nothing sent anywhere.

## What it cannot answer

- **The true size of a response it refused to read.** A body above the ceiling is truncated
  before parsing and recorded as truncated. That is a limit, not a finding: the toolkit reports
  that it stopped reading, not how large the thing it stopped reading was.
- **Whether a heavy page is slow.** Weight is not time. A 400 KB document on a fast connection
  behind a working CDN may be fine, and a small one on a cold origin may not be.
- **What is inside the bytes.** Nothing here says whether the weight is inline CSS, embedded
  base64 assets or an over-generous template. That is a look at the document.
- **The rendered weight.** The size recorded is the response as served. A page that assembles
  most of itself client-side is measured before that happens — see the [rendering scenario](rendering.md).
- **Whether compression is in play.** The size is what crossed the wire; whether the origin
  could have compressed it further is `headers-check`, not this chain.
