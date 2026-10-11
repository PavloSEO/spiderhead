# Audit Tasks — example.com

> 94 of 219 checks could run; the score is not comparable to a run with full evidence

- Source: audit generated at 2026-10-11T12:08:50Z (health n/a)
- Tasks: **20** (P1: 3, P2: 8, P3: 9)

## P1 (3)

- [ ] **Internal link points to a 4xx URL — 1 page** · critical · effort: high
    - _How to fix:_ Update the link to the current URL or add an appropriate 301 redirect; if it appears in the footer or navigation, fix the shared template.
    - Reproduction: https://example.com/old-page returned HTTP 404. Recorded observation: Destination scope: internal
    - Broken links (destination ← source · position · XPath):
        - https://example.com/old-page (404) ← https://example.com/ · Content · `/html/body/main/article/p[3]/a`
        - https://example.com/old-page (404) ← https://example.com/page-a · Footer · `/html/body/footer/nav/a[2]`

- [ ] **Page returns a 4xx response (broken page) — 1 page** · critical · effort: high
    - _How to fix:_ Restore the page or redirect it with a 301 to a relevant URL; remove or update links that point to it.
    - Reproduction: https://example.com/old-page returned HTTP 404. Recorded observation: Inlinks: 4
        - https://example.com/old-page

- [ ] **Title element is missing — 1 page** · critical · effort: high
    - _How to fix:_ Add a unique, descriptive title element.
    - Reproduction: At https://example.com/no-title: Title element is missing
        - https://example.com/no-title

## P2 (8)

- [ ] **Duplicate meta description — 2 pages** · warning · effort: medium
    - _How to fix:_ Write a unique meta description for each page.
    - Reproduction: At https://example.com/: Duplicate count: 2.
    - Reproduction: At https://example.com/page-b: Duplicate count: 2.
        - https://example.com/
        - https://example.com/page-b

- [ ] **Duplicate title element — 2 pages** · warning · effort: medium
    - _How to fix:_ Give each page a unique title element.
    - Reproduction: At https://example.com/page-a: Duplicate count: 2.
    - Reproduction: At https://example.com/page-b: Duplicate count: 2.
        - https://example.com/page-a
        - https://example.com/page-b

- [ ] **Indexable page has no canonical URL — 1 page** · warning · effort: medium
    - _How to fix:_ Add a valid <link rel="canonical"> element.
    - Reproduction: At https://example.com/no-title: Indexable page has no canonical URL
        - https://example.com/no-title

- [ ] **Meta description is missing — 1 page** · warning · effort: medium
    - _How to fix:_ Add a useful meta description, typically up to about 160 characters.
    - Reproduction: At https://example.com/no-title: Meta description is missing
        - https://example.com/no-title

- [ ] **Multiple H1 headings on the page — 1 page** · warning · effort: medium
    - _How to fix:_ Keep one primary H1 and demote the remaining headings to H2 or H3 as appropriate.
    - Reproduction: At https://example.com/page-a: H1 count: 2.
        - https://example.com/page-a

- [ ] **HTML document is large in absolute terms or relative to the site — 1 page** · warning · effort: medium
    - _How to fix:_ Reduce HTML size by removing unnecessary markup, extracting inline styles or scripts, and avoiding embedded base64 assets.
    - Reproduction: At https://example.com/no-title: Abs threshold kb: 200.
        - https://example.com/no-title

- [ ] **Slow server response — 1 page** · warning · effort: medium
    - _How to fix:_ Improve TTFB by profiling the application and origin, then optimizing caching and infrastructure.
    - Reproduction: At https://example.com/no-title: Max s: 1.5.
        - https://example.com/no-title

- [ ] **Thin content (low word count) — 1 page** · warning · effort: medium
    - _How to fix:_ Add substantial, useful content or exclude the page from indexing when it has no standalone search value.
    - Reproduction: At https://example.com/page-b: Threshold: 200.
        - https://example.com/page-b

## P3 (9)

- [ ] **H2 is duplicated across multiple URLs — 3 pages** · notice · effort: low
    - _How to fix:_ Use a unique, page-specific H2 on each URL, or accept it for a shared boilerplate subheading that is genuinely meant to repeat.
    - Reproduction: At https://example.com/: Duplicate count: 3.
    - Reproduction: At https://example.com/no-title: Duplicate count: 3.
    - Reproduction: At https://example.com/page-a: Duplicate count: 3.
        - https://example.com/
        - https://example.com/no-title
        - https://example.com/page-a

- [ ] **Title falls below the configured length threshold — 2 pages** · notice · effort: low
    - _How to fix:_ Expand the title to an informative length without padding it with boilerplate.
    - Reproduction: At https://example.com/page-a: Length: 26.
    - Reproduction: At https://example.com/page-b: Length: 26.
        - https://example.com/page-a
        - https://example.com/page-b

- [ ] **Meta description falls below the configured length threshold — 1 page** · notice · effort: low
    - _How to fix:_ Expand the description with specific, useful page information.
    - Reproduction: At https://example.com/page-a: Length: 7.
        - https://example.com/page-a

- [ ] **HTML bloat: high document size relative to text content — 1 page** · notice · effort: low
    - _How to fix:_ Reduce bytes per word by extracting styles and scripts, removing embedded base64 assets, and simplifying markup.
    - Reproduction: At https://example.com/page-b: Bytes per word: 1600.0.
        - https://example.com/page-b

- [ ] **Low text-to-HTML ratio — 1 page** · notice · effort: low
    - _How to fix:_ Increase the proportion of meaningful visible content or reduce unnecessary markup.
    - Reproduction: At https://example.com/page-b: Text ratio: 8.0.
        - https://example.com/page-b

- [ ] **No indexable About page was found in the crawl's scope** · notice · effort: low
    - _How to fix:_ If an About page exists, make it reachable and indexable; the finding's state says whether it was found non-indexable, broken, linked but never crawled, or not discovered at all.
    - Reproduction: Reproduction unavailable from the saved audit.

- [ ] **No indexable Contact page was found in the crawl's scope** · notice · effort: low
    - _How to fix:_ If a Contact page exists, make it reachable and indexable; the finding's state says whether it was found non-indexable, broken, linked but never crawled, or not discovered at all.
    - Reproduction: Reproduction unavailable from the saved audit.

- [ ] **No indexable privacy policy page was found in the crawl's scope** · notice · effort: low
    - _How to fix:_ If a privacy policy exists, make it reachable and indexable; the finding's state says whether it was found non-indexable, broken, linked but never crawled, or not discovered at all.
    - Reproduction: Reproduction unavailable from the saved audit.

- [ ] **No indexable terms-of-use page was found in the crawl's scope** · notice · effort: low
    - _How to fix:_ If a terms page exists, make it reachable and indexable; the finding's state says whether it was found non-indexable, broken, linked but never crawled, or not discovered at all.
    - Reproduction: Reproduction unavailable from the saved audit.

