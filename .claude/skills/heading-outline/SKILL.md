---
name: heading-outline
description: >-
  Builds the complete H1–H6 heading structure of each page in DOM order and checks
  the hierarchy, which a crawler export holding only H1/H2 columns cannot show. It parses live HTML and checks for exactly one H1, no skipped
  levels (H2→H4), and that the H1 comes before other headings.
  Use when asked to "check headings," inspect the "heading structure," verify the
  "H1 H2 H3 hierarchy," build a "page outline," or inspect the "Hx structure." Triggers:
  heading structure, page headings, H1 H2 H3 hierarchy, page outline, heading outline,
  Hx structure, check headings, and audit headings.
---

# Heading Outline — H1–H6 Heading Structure and Hierarchy

Screaming Frog exports only H1/H2 and flags only "missing H1" / "multiple H1"
(see `../sf-analyzer/reference/checks.md`: `H1_MISSING`, `H1_MULTIPLE`,
`H2_MISSING`). It does not provide a complete H1–H6 outline **in document order**.
A native `crawl-site` run retains that outline and automatically emits `HEADING_SKIP` for a
level jump in a named content region. This skill remains the readable per-template inspection:
it prints the whole outline and applies the additional editorial heuristics below.

## Trigger
- "Check the headings / heading structure on the page";
- "Is the H1→H2→H3 hierarchy correct, with no level jumps?";
- "Build the page outline" or "inspect the Hx structure."
- Triggers from the frontmatter: heading structure, page headings, H1 H2 H3
  hierarchy, page outline, heading outline, Hx structure, check headings,
  audit headings.

## Anti-trigger
- Bulk "does every page have exactly one H1" across many pages at once —
  `sf-analyzer` already flags `H1_MISSING`/`H1_MULTIPLE` for the whole crawl;
  reach for this skill only when the full H1–H6 order and nesting on specific
  pages is needed, not as a page-by-page substitute for that crawl-wide check.
- The page is a JS-rendered SPA and no rendered HTML is available yet — a
  plain `curl` fetch returns an empty shell, so the outline would be built
  from nothing. Run `js-render-check` (or an sf-analyzer crawl with JS
  Rendering / "Store Rendered HTML") first.
- The question is about heading **wording or keyword** quality ("should the
  H1 contain this keyword") rather than structure/hierarchy — that is
  content/copy review, not this skill.

## Preconditions
- [ ] Either an sf-analyzer `audit.json` with page URLs, or a specific URL
  (or list of URLs) supplied directly.
- [ ] `lxml` installed (`pip install lxml`) — the parser in step 2 depends on it.
- [ ] For a JS-heavy page, rendered HTML is available (sf-analyzer mode A with
  Store Rendered HTML), or it is accepted up front that a raw `curl` fetch
  may return an empty shell.

## Workflow
1. **Get the URL list.** Use the sf-analyzer audit (`audit.json` → `pages[].url`
   or `Internal:All`). If no URL was provided, ask for one page or a set of pages.
   Process each URL separately.
2. **Download and parse the HTML in DOM order** (requires `pip install lxml`):
   ```bash
   curl -sL -A 'Mozilla/5.0 (audit heading-outline)' "$URL" -o /tmp/page.html
   python - "$URL" <<'PY'
   import sys, re
   from lxml import html
   url = sys.argv[1]
   doc = html.parse('/tmp/page.html').getroot()
   # All h1..h6 elements in document order
   nodes = doc.xpath('//h1|//h2|//h3|//h4|//h5|//h6')
   out = []
   for n in nodes:
       lvl = int(n.tag[1])
       txt = re.sub(r'\s+', ' ', n.text_content()).strip()
       out.append((lvl, txt))
   # Print the indented outline
   for lvl, txt in out:
       print('  ' * (lvl - 1) + f'H{lvl}: ' + (txt or '∅ (empty)'))
   PY
   ```
   For a JS-heavy SPA, `curl` returns an empty shell; in that case, use rendered HTML
   from sf-analyzer mode A (Store Rendered HTML), or warn that rendering is required.
3. **Check the hierarchy** using the collected `(level, text)` list:
   - **Exactly one H1.** 0 → `H1_MISSING`; ≥2 → `H1_MULTIPLE` (output every text).
   - **No skipped levels.** The level must not increase by more than 1 in a single step:
     H3 may follow H2, but **H2→H4 is an error** (`HEADING_SKIP`). A heading may drop
     to any level (H4→H2 is valid).
   - **H1 comes first.** If a heading occurs before the first H1 → `HEADING_BEFORE_H1`.
4. **Summarize the result.** For each page, provide the indented outline (from step 2)
   plus a list of detected issues, including the level and text of the offending heading.
   For a set of pages, provide a summary such as "N pages with hierarchy issues" and
   the most common issue types.

## Decision points
- **Zero headings found in raw HTML.** This can mean the page genuinely has
  no headings, or that the DOM is populated only by JavaScript. If body text
  is also thin or empty, treat the page as `js_dependent` and re-run on
  rendered HTML (via `js-render-check` or sf-analyzer mode A) before
  concluding "no headings."
- **H2→H4 skip vs. a legitimate level drop.** The rule only flags an
  *increase* of more than one level; a heading dropping down (H4→H2) is
  valid. Before flagging `HEADING_SKIP`, confirm the skip isn't an artifact
  of a decorative widget mistagged with the wrong heading level rather than a
  real structural jump.

## Definition of done
- [ ] Every URL in scope has been parsed and an indented H1–H6 outline
  produced, or a stated reason it could not be (e.g. JS-rendering block).
- [ ] Every page has been checked against the three heuristics: H1 count,
  skipped levels, H1-first.
- [ ] Findings carry the correct code (`H1_MISSING`, `H1_MULTIPLE`,
  `HEADING_SKIP`, `HEADING_BEFORE_H1`).
- [ ] For a multi-page run, a summary count of pages with issues and the
  most common issue types is included.

## Cost
No `seohead` CLI command is invoked — this skill runs a plain `curl` fetch
plus a local `lxml` parse per page: one HTTP request per URL, sub-second
local processing, no paid API. Cost scales linearly with the number of pages
audited; for a large set, reuse the URL list from an existing sf-analyzer
`audit.json` rather than re-crawling to build it.

## What to Deliver to the User
- **Outline** for each page: an indented tree (`H1` / `  H2` / `    H3` …),
  with empty nodes marked `∅`.
- **Issue list** by page: `URL → [HEADING_SKIP H2→H4 "…", H1_MULTIPLE …]`.
- A brief conclusion explaining where the hierarchy breaks and what to fix (lower or
  raise a level, remove an extra H1, or fill an empty heading).

## Integrations
- URL source and basic H1/H2 flags: the **sf-analyzer** skill (`audit.json`).
- For a single readable audit report, use **sf-report**; for backlog tasks, use
  **sf-tasks** (create `HEADING_SKIP`/`H1_*` as separate tasks).
