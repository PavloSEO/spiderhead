# Scenario 45 — The bundle audit: legacy transpilation, duplicates and unminified files

## The question

> We ship a lot of JavaScript. Is any of it there twice, or built for browsers nobody uses?

Three questions with the same answer source, all of them answerable from the files themselves
without a browser, a build system or access to the repository.

## Covers

- **PageSpeed** — Legacy JavaScript · Duplicated JavaScript · Minify CSS · Minify JavaScript · Reduce Unused CSS · Reduce Unused JavaScript · Forced Reflow · Reduce JavaScript Execution Time

## The chain

**1. Fetch the page's own stylesheets and scripts.**

```bash
seohead asset-weight-check --url https://example.com/page
```

One pass over the fetched bodies answers all four. What each check actually does is worth knowing
before quoting it:

**Legacy JavaScript** is a marker search — `core-js`, `regeneratorRuntime`, `_babelPolyfill`,
`@babel/runtime`, and the `Object.assign = function` reassignment a polyfill leaves behind. A
transpiler shipping these unconditionally to every browser is the pattern; the markers are strong
evidence and are not proof, and the report says heuristic rather than pretending otherwise.

**Duplicated JavaScript** is a content hash. Each fetched body is whitespace-normalized and
hashed, and a group is reported only when the same hash appears under more than one distinct URL.
Normalizing first means a different line-wrap width cannot hide a duplicate; requiring distinct
URLs means one file linked twice is not called a duplicate, because it is one file.

**Minify CSS and Minify JavaScript** are one heuristic over line length and whitespace ratio,
applied to both kinds. Hand-authored code is reformatted onto many short indented lines; minified
code is not. A file under 200 characters is judged by shape instead: it counts as minified only when it is one
line with less than 15% whitespace, so a short multi-line file is never flagged, and a two-line
inline shim is not a false alarm.

**2. Check the same page for the delivery side of the same files.**

```bash
seohead headers-check --url https://example.com/page
```

An unminified bundle served uncompressed and without a long-lived `Cache-Control` is three
findings about one file, and they are worth fixing in that order — compression and caching are
server configuration and land the same day.

**3. Do it on one page per template, not on the home page alone.** Bundles differ by route on
every framework that code-splits, and the home page is usually the least representative page a
site has.

**4. Put the numbers in something somebody will read.**

```bash
seohead report-build --audit ./run/audit.json --format docx --out ./assets-task.docx
```

## What comes out

Native scan.v2 resource capture exposes `LEGACY_JS`, `DUPLICATED_JS` and
`UNMINIFIED_CSS_JS` in saved audits. Native duplicate detection hashes exact captured bytes,
so whitespace inside string literals cannot create false duplicates. Incomplete responses,
HTTP 206 fragments and missing source MIME evidence remain unavailable for source heuristics.
Requested Chromium renders also retain bounded JS/CSS coverage for `UNUSED_CSS_JS`, measured
in UTF-16 code units during that navigation. Reopening the scan needs no additional request.
Supplied matching per-URL Lighthouse JSON can provide `FORCED_REFLOW` and
`JS_EXECUTION_TIME`; neither is inferred from source size or coverage.

The finding lines, each backed by its own rows:

```json
{
  "findings": [
    "1 library bundled more than once",
    "2 file(s) do not look minified",
    "1 script(s) look like unconditional legacy/polyfill code"
  ],
  "duplicate_libraries": [
    {"kind": "js", "hash": "5f3a…", "urls": ["/static/vendor.js", "/static/app-legacy.js"]}
  ]
}
```

Two URLs with one hash is the whole argument for the duplicate finding. Nobody has to be
persuaded that two files are the same file when the bytes say so.

## What it costs

One request for the page and one per referenced stylesheet or script, fetched concurrently and
capped at a fixed ceiling; when there are more, `resources_truncated` is true rather than the
report quietly describing a subset. Files above 500,000 bytes are additionally listed as oversized.
Nothing is paid and nothing is executed — the bodies are read as text.

## What it cannot answer

- **Code used by later interactions, workers or unmeasured frames.** Chromium coverage concerns
  the recorded navigation window and preserves omitted sources and ranges. Other engines or
  scans without retained coverage report why this native measurement is unavailable.
- **Execution time without lab input, or general main-thread work.** Supplied Lighthouse results
  support the named execution/reflow checks; no CPU profile is collected automatically.
- **Duplicates across the site.** Hashing compares the resources of one page. Two templates
  bundling the same library separately need two runs and a comparison you make yourself.
- **Whether the legacy code is deliberate.** A site with a genuine obligation to old browsers is
  shipping those polyfills on purpose. The finding is "this is unconditional", not "this is
  wrong".
- **Whether a minifier would actually shrink it.** The check is a shape heuristic, not a
  round trip through a real minifier.
