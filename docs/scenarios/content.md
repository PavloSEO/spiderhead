# Scenario 3 — Content extraction: how much of this page is actually the page

## The question

> The audit says these pages are fine. Read one and tell me what it really says.

## Covers

- **Accessibility** — WCAG 2.0 A - Form <input> Elements Require Labels · WCAG 2.0 A - HTML Element Requires Lang Attribute · WCAG 2.0 A - Images Require Alternate Text · WCAG 2.0 A - Select Element Requires Accessible Name

- **Content** — Low Content Pages · Near Duplicates · Lorem Ipsum Placeholder

## The chain

Use a retained native scan (`crawl-site --scan-out ./content.sqlite`) to inspect the
static accessibility findings alongside content evidence: language declarations,
control labels, image alternatives, heading jumps and the main landmark. Findings
include bounded examples and full occurrence counts from the active document.
This subset cannot answer computed contrast or full rendered axe-core/WCAG compliance.

**1. Extract the content region as Markdown.**

```bash
seohead markdown-extract --url https://example.com/page
```

Two renderings come back: `content_markdown` (the content region only) and `full_markdown`
(everything, header and footer included). The difference between them *is* the boilerplate.

The region is detected from the document's own semantics — `<main>`, then `[role="main"]`,
then `<article>` — and the strategy used is reported per page as `auto_main`, `auto_role_main`,
`auto_article` or `default_body`. On a live WordPress post, the whole-body default counted 433
words where `<main>` holds 429: 126 of them template, including a skip-to-content link and the
literal word "header".

**2. Override it when a site's markup is not semantic.**

```bash
seohead markdown-extract --input '{"url": "https://example.com/page", "content_area": {"include_selector": "#content"}}'
```

Configuration wins over detection. A selector that matches nothing reports
`fallback_default_body` rather than quietly detecting something else — a wrong selector should
be visible, not smoothed over.

**3. Find where the template drifts across pages.** `boilerplate-report` hashes header/nav/footer
*markup* — it needs the original HTML, or a hash already computed from that HTML. `full_markdown`
from step 1 has already stripped the tag structure the hash depends on, so it is not a valid
input here; two differently-templated pages fed as Markdown collapse into the hash of an empty
string and look identical.

```bash
seohead boilerplate-report --input '{"pages": [{"url": "https://example.com/a", "html": "<html>...</html>"}, {"url": "https://example.com/b", "html": "<html>...</html>"}]}'
```

Pass a precomputed digest instead with the `hash` key (not `boilerplate_hash`, which the handler
does not recognize) when the HTML itself should not be sent again:

```bash
seohead boilerplate-report --input '{"pages": [{"url": "https://example.com/a", "hash": "aaa..."}, {"url": "https://example.com/b", "hash": "bbb..."}]}'
```

Hashing header and footer per page finds the pages whose template differs from every other —
usually a legacy section nobody remembers, sometimes a broken include.

**4. Catch placeholder copy left in the content region.** `LOREM_IPSUM_PLACEHOLDER` matches the
full "lorem ipsum dolor sit amet" passage, counted only against `content_text` — the same
content-region text step 1 returns as `content_markdown`, not the whole raw document. A page
that mentions the passage once outside that region (a typography demo in a sidebar widget, a
one-off comment in a footer) never fires; a page that shipped the passage as its own body copy
does, on a single occurrence, because it is already scoped to where the real content lives.

## What comes out

```
strategy: auto_main
content:  429 words
full:     1104 words
```

Two-thirds of that page is navigation and footer. Any threshold applied to the larger number
is measuring the template.

## What it costs

One request per page. Local parsing. Nothing paid.

## What it cannot answer

- **Whether the content is good.** Extraction is not judgement.
- **What a *reader* considers the content.** A page whose sidebar carries the real answer will
  be extracted as if the sidebar were furniture. Override the selector when that is the case.
- **Content assembled by JavaScript.** See the [rendering scenario](rendering.md).
