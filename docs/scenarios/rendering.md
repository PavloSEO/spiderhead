# Scenario 35 — Rendering: does a crawler see what a visitor sees

## The question

> The page looks fine in my browser. Why does the audit say it has no content?

## Covers

- **JavaScript** — Contains JavaScript Content · Contains JavaScript Links · Pages with Blocked Resources

## The chain

**1. Compare raw HTML against the rendered DOM for one page.**

```bash
seohead render-check --url https://example.com/page --viewport desktop --wait load
```

This is the diagnosis, and it is one request plus one browser render. It reports what exists
only after JavaScript: text, internal links, title, canonical, `h1`, and Schema.org — plus lab
timings (TTFB, FCP, LCP, CLS).

**2. If the crawl itself needs rendering, it says so before scoring anything.** A crawl whose
start page has zero internal links, or an empty SPA shell, sets `requires_rendering` and
withholds the health score. A green audit of a page a static crawler could not read is worse
than no audit.

**3. Turn rendering on selectively, not everywhere.** In the crawler config:

```json
{"rendering": {"mode": "js", "escalation": {"sample_per_pattern": 2, "max_render_urls": 30},
               "browser": {"engine": "chromium", "page_concurrency": 2}}}
```

Two URLs per detected template pattern are probed raw-versus-rendered; only patterns that
actually differ are escalated. Rendering every page of a 3000-URL site to find the one template
that hydrates its links costs an order of magnitude more for the same answer.

Desktop mode labels map to crawl settings as **Off** = `rendering.mode: raw`, **Auto** =
`rendering.mode: js` with `rendering.escalation.policy: auto`, and **Always** = `js` with
`policy: full`. URL and time budgets still bound Auto and Always. For a mobile pass, select the
`mobile` viewport or a custom width of 600 CSS pixels or less; the retained renderer provenance
then carries overflow, computed text-size and interactive-target measurements. Checks stay
unavailable when no mobile render was captured.

Page actions run only when the project opts into them. They use bounded selectors and values, and
the named presets expand-tabs, load-more, and read-dataLayer never accept caller JavaScript:

```json
{"rendering":{"mode":"js","presets":["expand_tabs","load_more"],
 "actions":[{"action":"wait_for_selector","selector":".product-list","timeout_ms":5000},
            {"action":"extract","selector":"a.product","attribute":"href"}],
 "artifacts":{"screenshots":true,"screenshot_mode":"first_screen"}}}
```

Action results and the bounded pinned-network rejection log are stored with each rendered
document's renderer evidence. `screenshot_mode` accepts `first_screen` or `full_page`.

`browser.engine` chooses the headless engine — `chromium` (the default), `firefox` or
`webkit`; an uninstalled browser fails with an install hint, never a silent fallback.
`browser.viewport_width`/`viewport_height` replace the `desktop`/`mobile` preset's
dimensions with an exact pixel pair (set both, 1–16384), and `browser.page_concurrency`
bounds how many browser fetches run at once — independently of the crawl's HTTP politeness
concurrency, with 1 keeping the older sequential behaviour. The probe and the full render of
a run always share the same engine, viewport and emulation flags, and the effective values
are recorded per rendered page.

```bash
seohead crawl-site --url https://example.com --config ./crawl.json --out-dir ./run
```

**4. Every page records how it was measured.** `representation` is `static`, `rendered` or
`legacy_fragment` per page, so a report never silently mixes two populations of numbers.

## What comes out

```json
{
  "js_dependent": true,
  "raw": {"links": 0, "words": 12},
  "rendered": {"links": 34, "words": 812},
  "empty_shell": true
}
```

`dual_crawl` (schema `dualcrawl.v1`) rides alongside this in the same response: it diffs the raw
and rendered evidence per URL (images, links) instead of one page's before/after, so a report can
tell "this page changed" (the block above) apart from "these two collection methods disagree
about what this page contains" (`dual_crawl`) — see `js-render-check`.

Zero links raw and 34 rendered is the whole finding: every crawler that does not execute
JavaScript sees a dead end.

## When the render itself did not finish

A browser that fails mid-navigation still returns a document, and that document is nearly
empty. Compared naively against a full raw response it reads as the site deleting its own
metadata — "the title changes after JavaScript" from a title the render never read (#623).
So the pair is judged before it is compared: a rendered document with no title, no `h1`, no
canonical and no internal links, at a fraction of the raw response's byte size, did not
capture the page.

```json
{
  "ok": false,
  "reason": "incomplete_render",
  "error": "The rendered DOM was not captured, so the raw-versus-rendered comparison is unavailable: ...",
  "js_dependent": null
}
```

No `findings` are emitted from that state, and `js_dependent` is `null` rather than `false`:
the run does not know. Both snapshots are returned so the reason can be checked. In
`site-audit` this lands in `summary.tools_failed`, where an unmeasured check belongs, not in
the findings list.

A wait milestone that never arrives is the ordinary cause. A site with long-polling
analytics, chat or ad scripts never goes network-idle, so `--wait networkidle` times out
where the page is in fact complete; when that happens the DOM is read at `domcontentloaded`
instead of the check being lost, and `wait_reached` records which milestone the snapshot
actually came from. A short settle after the milestone gives deferred scripts time to write
the DOM.

## What it costs

A headless browser per rendered URL — seconds each, not milliseconds, and real memory. That is
why escalation samples patterns instead of rendering everything. The static crawl costs one
request per page as usual.

## What it cannot answer

- **What Google actually rendered.** This is a headless browser — Chromium by default, or
  Firefox/WebKit when configured — not Googlebot's renderer, on
  a different schedule with different budgets. It answers "is this content JS-dependent",
  which is the actionable half.
- **Whether rendering delay costs rankings.** Nobody outside Google can measure that.
- **Field performance.** LCP and CLS here are lab numbers from one machine on one connection.
- **Anything behind a login.** Attaching a real browser profile crawls as whoever's cookies it
  carries, which is off by default and refused without an explicit directory.
