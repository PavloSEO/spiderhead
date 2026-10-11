# Scenario 15 — Pagination: whether page 2 onward is reachable and allowed to exist

## The question

> The category has 40 pages of products. Google has the first one. Where did the other 39 go?

A pagination series is a chain, and a chain has two independent failure modes: a link somewhere
in it does not resolve, or every page past the first is told not to be indexed. Both look
identical from the front page.

## Covers

- **Pagination** — Non-200 Pagination URLs · Unlinked Pagination URLs · Pagination Loop · Non-Indexable · Pagination URL Not In Anchor Tag · Multiple Pagination URLs · Sequence Error

## The chain

**1. Crawl the site, which answers the indexability half immediately.**

```bash
seohead crawl-site --url https://example.com --out-dir ./run
```

`PAGINATION_NONINDEXABLE` covers both published issue names at once, because they are the same
question asked twice: a pagination URL that answers 3xx/4xx/5xx and a pagination URL that
answers 200 with a `noindex` are both pages a crawler will not keep. A series where page 1 is
indexable and pages 2..n are not is the most common shape, and it is usually a plugin default
nobody chose.

**2. Read retained head annotations and target evidence.**

Native scans preserve raw `rel`/`href` declarations in versioned document context. The SQL scan
schema stays unchanged. `PAGINATION_LOOP` walks the `rel="next"` graph; native numbered and
stride series use the same rules as SF exports. `UNLINKED_PAGINATION_SERIES` still requires a
complete hyperlink population before absence can be proven.

Same-host uncaptured canonical, hreflang and pagination targets receive at most one bounded
probe, within the crawl request budget and the shared 5,000-target cap. Probes retain HTTP
status, redirect, measured noindex state and the target's canonical. A target past a budget,
an external target or an older status-only probe remains unmeasured for HTML directives.

**3. Use existing Screaming Frog exports when that is the chosen input.**

```bash
seohead sf run --exports-dir ./exports --out report --tasks
```

Both inputs feed the same findings. Raw malformed canonical attributes require native parsed
annotation evidence; an ordinary SF Internal:All canonical cell cannot reconstruct raw markup.
Canonical annotations carrying `hreflang`, `lang`, `media` or `type` are reported by
`CANONICAL_INVALID_ATTRIBUTE`: those alternate-version attributes belong on separate link
annotations. See [Google's canonical link guidance](https://developers.google.com/search/docs/crawling-indexing/consolidate-duplicate-urls).

**4. Export All Inlinks too, because the anchor check needs a page's own link inventory.**

`PAGINATION_MULTIPLE` fires when a page declares two *different* `rel="next"` URLs (or two
`rel="prev"` URLs). "Different" is judged after URL normalization, the same identity the anchor
check uses: `/blog/page/2` and `/blog/page/2/` from two plugins are one successor spelled twice,
which is untidy markup and not an ambiguous series. Two genuinely different URLs leave a crawler
to pick one without telling anybody which. This one has a light path: Screaming Frog numbers a
repeated declaration by occurrence rather than dropping it, the same way `Canonical Link
Element 2` answers `CANONICAL_MULTIPLE`, and `Internal:All`'s `rel="next" 2` / `rel="prev" 2`
columns are read first — a second occurrence there is enough evidence on its own, with no All
Inlinks export needed. Only a profile whose `Internal:All` was written without those columns
falls back to reading All Inlinks for this one, the same export the anchor check below always
needs.

`PAGINATION_URL_NOT_IN_ANCHOR` fires when a declared pagination URL is not also linked from the
same page with an ordinary `<a href>`. Google stopped using these annotations for indexing in
2019, so a declaration with no anchor beside it is a route only some crawlers still take. This
one is provable on a partial crawl, unlike `UNLINKED_PAGINATION_SERIES`: the page was fetched,
so its own links are all in the export.

Without that export both declare themselves absent by name rather than reading clean:

```json
[
  {"id": "PAGINATION_MULTIPLE", "reason": "no all_inlinks export (needed for every rel=\"next\"/rel=\"prev\" declaration and the anchors beside them)"}
]
```

**5. Read the sequence finding as a statement about a run, not about `1..n`.**

`PAGINATION_SEQUENCE_ERROR` walks the same `rel="next"` graph and reads each URL's own page
number, from a `page`/`paged`/`pg` token and nothing else — a bare number in a path is a year
or a product id as often as a page index. It reports a break in a run that increments by one
somewhere and then does not: 1, 2, 3, 7 leaves pages 4 to 6 in nobody's chain.

A dominant stride is judged against its own step: `?page=1,3,5,8,10` reports 5 → 8,
while `0,10,20` stays silent. An irregular run with no dominant step remains unjudged.

For opaque URLs, the check never invents page numbers. It reports an explicit contradiction:
A declares `next=B`, while B declares `prev=C` instead of A (and the mirror direction).
Correct reciprocal pairs stay silent. Missing reverse declarations, uncaptured targets,
ambiguous successors and old scans without retained annotations remain unjudged with a reason.
Cycles are reported by `PAGINATION_LOOP` alone. Native graph identities preserve distinct slash
paths and uncertain fragment-route documents remain unmeasured. Reciprocal contradictions also
cover numbered neighbors in a chain whose first URL is unnumbered; duplicate forward/reverse
reports of one broken successor are collapsed. Raw declarations and document identity accompany
the contradiction finding.

Findings are retained in the audit and can be selected with `scan-url-query --issue-check
PAGINATION_SEQUENCE_ERROR`. The same saved issue stream feeds Desktop Problems; no desktop
specific check or second calculation is needed.

**6. Confirm the links themselves resolve.**

```bash
seohead links-check --url https://example.com --internal-only
```

**7. Scan the run, because an unlinked finding is a claim about the whole site.**

```bash
seohead log-scan --run ./run
```

`UNLINKED_PAGINATION_SERIES` is withheld and re-declared as a named skip when the crawl is
partial, for the same reason as `ORPHAN_PAGE` and `UNLINKED_CANONICAL`: the missing hyperlink
may simply be in the part nobody fetched.

## What comes out

```json
{
  "check": "PAGINATION_NONINDEXABLE",
  "severity": "warning",
  "target_url": "https://example.com/catalog/page/2",
  "message": "Pagination page is non-indexable",
  "fix_hint": "Pagination pages should generally remain crawlable and indexable unless a deliberate alternative architecture is in place."
}
```

Read the fix hint as written. "Generally" is doing real work there: a site with a proper
view-all page, or one that loads the rest of the catalogue from a feed, has a defensible reason
for non-indexable pagination. A site with 39 unreachable pages of products does not.

## What it costs

- One crawl for step 1, one destination fetch per internal link for step 6.
- Steps 3, 4, 5 and 7 read files already on disk. Nothing paid.
- On a large catalogue, `links-check` is the expensive step; scope it before running it.

## What it cannot answer

- **The order of a series whose URLs do not number themselves.** `/catalog/2/` states nothing a
  page-number token can be read from, and guessing produces sequence "errors" on ordered series.
- **Whether an unestablished order has a gap.** Opaque URLs with no reciprocal declaration
  evidence and numeric runs with no dominant step cannot establish a missing page.
- **Infinite scroll.** A series assembled by JavaScript has no `rel` annotations to read and no
  hyperlinks to follow; the [rendering scenario](rendering.md) comes first.
- **Whether the products on page 2 are indexed.** Reachability is not indexation, and nothing
  here reads the index.
