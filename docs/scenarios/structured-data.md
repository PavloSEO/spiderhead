# Scenario 33 — Structured data: from "we have markup" to a rich result

## The question

> We added Schema.org months ago and still get no rich results. What is wrong with it?

## Covers

- **Structured Data** — Validation Errors · Missing · Parse Errors

## The chain

The shared checker validates JSON-LD, Microdata and offline RDFa subject graphs with
the bundled vocabulary and local rich-result rules. RDFa forward/reverse relations,
resource and literal values, ordered `inlist` collections and bounded iterative
property copying use the same graph validator. [HTML+RDFa defines no default
vocabulary](https://www.w3.org/TR/html-rdfa/#additional-rdfa-processing-rules); use
an explicit `vocab` or Schema.org prefix. [Property copying](https://www.w3.org/TR/html-rdfa/#property-copying)
is resolved after graph extraction. A retained native scan emits per-URL schema
errors, warnings and missing required properties; extraction limits report partial
coverage. Local eligibility
never guarantees Google display. Use `scan-structured-blocks` for the original and
parsed JSON-LD tree.

**1. Read what the page actually declares.**

```bash
seohead schema-check --url https://example.com/page
```

Two layers, reported separately because they fail for different reasons: vocabulary validity
(is this a real Schema.org type, are these real properties, is anything deprecated) and
rich-result eligibility (does Google's own requirement list for this type have everything it
needs).

**2. Check whether the markup survives the crawl at scale.** `pages.jsonl` records
`jsonld_blocks_found` against `jsonld_blocks_parsed` per page:

```bash
seohead crawl-site --url https://example.com --out-dir ./run
```

"Found 3, parsed 2" on one page is a malformed block. Reporting "no structured data" for that
page would describe a different page — which is why both numbers are kept, and why
`STRUCTURED_DATA_PARSE_ERROR` fires from exactly this comparison: found greater than parsed. A
page with none at all reads 0 and 0 — equal, so it never fires this check (that page is
`STRUCTURED_DATA_MISSING`'s concern instead). Native-crawl only: an SF export never carries a
parsed-block count, so the check skips by name there rather than reading it as clean.

**3. Generate the graph that is missing.**

```bash
seohead schema-build --url https://example.com/page
```

It reads the page and builds the `@graph` that page should carry, rather than asking you to
describe it. The point is the connected graph — `Organization` ← `WebSite` ← `WebPage` ← the page's own
type — not an isolated blob per page. Isolated blobs are the most common reason correct markup
produces nothing.

**4. Confirm the page as a whole.**

```bash
seohead parse --url https://example.com/page
```

## What comes out

```json
{
  "types": ["LocalBusiness"],
  "vocabulary": {"unknown_properties": ["opening_hours"], "deprecated": []},
  "rich_result": {"eligible": false, "missing": ["address", "telephone"]}
}
```

`opening_hours` is not a Schema.org property — `openingHours` is. That single underscore is a
common and entirely invisible failure.

## What it costs

One request per page checked. Nothing paid, no Google API involved.

## What it cannot answer

- **Whether Google will show a rich result.** Eligibility is necessary, not sufficient: Google
  decides, and quality and trust signals nobody can read are part of it.
- **Whether the data is true.** Nothing verifies that the declared address or price is real.
  Markup that contradicts the visible page is a manual-action risk this cannot see.
- **Markup injected after load.** See the [rendering scenario](rendering.md).
