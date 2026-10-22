# Scenario 16 — Titles that are missing, doubled or shared: one list, ordered by blast radius

## The question

> Somebody said our titles are a mess. Before I brief a copywriter, which pages actually have
> no title, and which ones are wearing somebody else's?

Three different defects hide under "the titles are a mess", and they cost different amounts to
fix. A page with no title is one page. A title shared by nine pages is one template.

## Covers

- **Page Titles** — Missing · Multiple · Duplicate · Outside <head>

## The chain

**1. Crawl once.** Everything below reads what this run collected; nothing here fetches a page
twice to answer a second question.

```bash
seohead crawl-site --url https://example.com --out-dir ./run --max-urls 200
```

Each fetched page lands in `pages.jsonl` with its `title` beside its `h1` and its canonical, so
the title checks and the duplicate grouping both read the same record.

**2. Scan the run before reading a single finding.**

```bash
seohead log-scan --run ./run
```

Exit 2 means two numbers in this run disagree. A count of duplicate titles larger than the
number of HTML pages is arithmetic, not a finding, and this is where that gets caught.

**3. Read the groups, not the rows.** `TITLE_MISSING` is per page. `TITLE_DUPLICATE` is emitted
twice: once per affected URL, and once as a **group** keyed by the shared title text, with
`group_id` linking the two. Nine pages sharing one title are nine issues and one job.

```bash
seohead report-build --audit ./run/audit.json --format xlsx --out ./titles.xlsx
```

**4. Confirm one page live before briefing anybody.**

```bash
seohead parse --url https://example.com/page
```

A missing title in an audit and a missing title in the served HTML are different claims when
anything in between caches, rewrites or injects. One request settles it.

**5. Add `TITLE_MULTIPLE`, which the crawl alone cannot give you.** Two `<title>` elements in
one document is a parser-level fact the native crawl does not keep — it records the first title
and moves on. It comes from a Screaming Frog export instead:

```bash
seohead sf run --exports-dir ./exports --out report --tasks
```

Without a `page_titles_multiple` export in that directory the check is **named as skipped**,
not reported clean:

```json
{ "id": "TITLE_MULTIPLE", "reason": "no titles_multiple export (export this SF filter to enable)" }
```

**6. Add `TITLE_OUTSIDE_HEAD`, which needs the parse tree the crawl in step 1 already built.**
A browser closes `<head>` at the first element that does not belong there — an invalid tag
sitting between the title and the rest of the metadata — and reads everything after that point
from `<body>` instead, silently. The title still exists; it is just read from the wrong place.
Step 1's crawl records this per page automatically; a crawler export analysed with
`sf run --exports-dir` cannot raise it, because the export does not record where the element sits.

## What comes out

The grouping, straight out of `audit.json`, is the part worth reading:

```json
{
  "group_id": "GRP-TITLE-0001",
  "check": "TITLE_DUPLICATE",
  "value": "Industrial Pumps Product A",
  "urls": ["https://example.com/page-a", "https://example.com/page-b"],
  "count": 2
}
```

And the backlog line the same finding produces:

```
- [ ] **Duplicate title — 2 pages** `TITLE_DUPLICATE` · warning · effort: medium
    - _How to fix:_ Write a unique title for each page.
```

`TITLE_MISSING` is a critical; `TITLE_DUPLICATE` is a warning. That ordering is severity, not
priority — see the limits below.

## What it costs

One request per crawled page, plus one for the live confirmation in step 4. No paid API. The
duplicate grouping is a dictionary build over records already in memory, so it costs nothing
beyond the crawl that was going to happen anyway.

## What it cannot answer

- **Whether a duplicate title is a defect.** Nine regional landing pages that legitimately share
  a name look exactly like a template bug. The tool reports the fact and the group; the decision
  is somebody's.
- **Whether the new title will be better.** Nothing here reads the title for meaning, keyword
  fit, or brand voice.
- **Whether two `<title>` elements exist**, unless a Screaming Frog export supplies that filter.
  The crawl keeps the first title and does not count the rest.
- **Whether a title sits outside `<head>` on a plain Screaming Frog export.** `TITLE_OUTSIDE_HEAD`
  needs the parse tree a native crawl builds; an export-only run names it skipped rather than
  reporting it clean.
- **Anything about pages the crawl did not reach.** Read `run.crawl_partial` before treating a
  count as "the site".
