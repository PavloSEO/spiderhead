# Scenario 44 — The blocking head: stylesheets, scripts and fonts that hold up first paint

## The question

> The server answers fast and the page still shows nothing for two seconds. What is it waiting
> for?

Whatever is in `<head>` and has not opted out of blocking. This chain reads the document's own
head, names each blocking resource, and finds the web fonts that hide text after the paint
finally happens.

## Covers

- **PageSpeed** — Render Blocking Requests · Font Display · LCP Request Discovery · Network Dependency Tree · Layout Shift Culprits

## The chain

**1. Read the head as the parser reads it.**

```bash
seohead asset-weight-check --url https://example.com/page
```

A `<script src>` in `<head>` blocks unless it opts out with `async` or `defer`, or carries a type
the specification already defers (`module`) or never executes as a classic script. A
`<link rel=stylesheet>` blocks unless `media` restricts it to something the first render does not
need, such as `print`. Both rules are applied to the tag as written, which is the point: this is
a markup finding with a markup fix.

**2. Read the font declarations in the same pass.** Every `@font-face` block without
`font-display: swap`, `fallback` or `optional` is reported, from external stylesheets and from
inline `<style>` blocks alike — the inline ones at no extra fetch. A font without a display
strategy means invisible text for as long as the file takes to arrive, which on a slow connection
is the difference between a slow page and a blank one.

**3. Follow the stylesheet's own imports one level.** A `@import` inside a stylesheet serializes
another round trip before any style applies, and the report says how many of them chain at least
two levels deep. This is the cheapest of the three findings to fix and the easiest to miss,
because nothing in the HTML shows it.

**4. Confirm the wait is not upstream of all this.**

```bash
seohead headers-check --url https://example.com/page
```

A 900 ms first byte and three blocking resources are two separate tasks, and doing the second
first wastes the sprint. The [speed server response scenario](speed-server-response.md) is the other half.

**5. Take the whole delivery picture, if this is a takeover rather than a tune-up.**

```bash
seohead site-audit --url https://example.com
```

## What comes out

Saved native audits expose `RENDER_BLOCKING` from the selected retained static head and
`FONT_DISPLAY_MISSING` from completed captured stylesheets. They keep source URLs, reasons
and measured/partial/unavailable counts; head markup does not establish blocking time.
The `input.lighthouse_reports` audit setting accepts a URL-to-JSON-file map for bounded matching
Lighthouse reports. It enables `LCP_DISCOVERY`, `NETWORK_DEPENDENCY_TREE` and
`LAYOUT_SHIFT_CULPRITS` with report hashes and lab provenance. Missing sibling reports stay
unavailable, and no browser trace is silently collected by this input path.

From a real run of `asset-weight-check` against a live site's home page:

```json
{
  "findings": [
    "3 render-blocking resource(s) in <head>",
    "6 @font-face block(s) without font-display"
  ]
}
```

Each finding has its rows beside it — `render_blocking` names every URL with the tag it came
from, `missing_font_display` names the stylesheet and quotes the start of the offending block —
so the task can be written against specific lines rather than against an audit score.

## What it costs

One request for the page, one per referenced stylesheet or script, and one more per stylesheet
that imports another. The resource list is capped, and `resources_truncated` says so rather than
quietly reporting on a subset. Nothing is paid, and no browser is launched: this is markup and
headers, read directly.

## What it cannot answer

- **How much time any of it costs.** Three blocking resources is a count, not a delay. The delay
  depends on connection, cache state, HTTP version and priority, and measuring it needs a real
  navigation trace.
- **LCP discovery or layout-shift causes without matching lab input.** These are unavailable
  until a corresponding Lighthouse report is supplied for the page; field CWV is not inferred.
- **Usage after the captured navigation window.** Requested Chromium renders collect bounded
  JS/CSS coverage, while later interaction, workers and unmeasured frames remain outside it.
- **Anything a script inserts into the head after load.** The document is read as served.
