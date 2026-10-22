# Changelog

All notable public changes are documented here.

## Unreleased

Entries for merged, unreleased work live one file per change in `changelog.d/`, not here:
`python scripts/build_changelog.py` folds them in between the markers below at release
time, so two branches never edit this file and can never conflict over it (#638).

<!-- changelog.d: assembled entries start -->
<!-- changelog.d: assembled entries end -->

- Keep fingerprinting a page whose site sends a valueless `Set-Cookie` header (#651). A header
  with no `name=value` pair at all -- `Set-Cookie: Secure; HttpOnly`, which `emall.by` sends
  beside two well-formed cookies -- becomes `Cookie(name='Secure', value=None)` in the jar, and
  `httpx.Cookies.__getitem__` raises `KeyError` for it. `dict(resp.cookies)` therefore failed on
  a name the mapping's own iterator had produced: `tech-detect` exited 1 with a bare
  `error: 'Secure'` and no JSON on stdout for a page that had been fetched successfully, MCP
  `seo_tech_detect` raised the `KeyError` to its caller, and `site-audit` quietly came back with
  its entire technology section missing. `detect_tech` now walks the cookie jar instead. A
  valueless name is kept with an empty value rather than dropped, because several fingerprints
  (`BITRIX_SM_GUEST_ID`, `craft_session`, `wfvt_`) match on cookie name alone and dropping the
  entry would lose that signal -- but it is never silently normalized into an ordinary cookie:
  the names are reported in `malformed_cookies` and named in a finding, so a header the site
  sent broken stays a fact about the site. A response with only well-formed cookies is
  unaffected and reports an empty list.
- `render-check` measures compressed sites again instead of calling them broken (#650). The
  pinned render route read the origin with httpx's undecoded stream and handed those bytes to
  Playwright together with the origin's `content-encoding`. `route.fulfill` never applies a
  content coding to the body it is given, so Chromium parsed gzip as `text/html` and built a DOM
  out of the compressed stream: no title, no h1, no canonical, no links, and a rendered size
  equal to the transfer. Every site that compresses its HTML -- nearly all of them -- came back
  `ok: false, reason: incomplete_render`, and the message blamed the render, suggesting
  `--wait domcontentloaded` or a longer `--timeout` when the render had finished perfectly.
  `seohead.tech` reported 22 435 rendered bytes against 121 793 raw; it now reports 123 048 with
  the real title and 31 links. The route now reads the decoded entity, drops `content-encoding`
  from what it forwards (`content-length` was already dropped as hop-by-hop, so Playwright states
  the length of the body it actually received), and asks the origin only for the codings this
  client can decode rather than forwarding Chromium's own invitation to `br` and `zstd`. A coding
  that arrives undecoded anyway aborts the request and names itself, because passing it through
  would reproduce the bug in silence. The byte cap now counts decoded bytes, which is the body
  the renderer sees -- a compression bomb previously passed the cap compressed.
- Read Open Graph off a native crawl by the key the parser actually writes (#646).
  `parse_html` stores each tag under its full property name -- `{"og:title": ...}`, as its own
  docstring says -- and `crawl/collect.py` asked the same dict for `"title"`, `"description"` and
  `"image"`. `dict.get` answered `None` every time, so `og_title`, `og_description` and `og_image`
  were empty on every page of every native crawl ever run: 0 of 40 920 pages carried one on the
  run that surfaced this, on a site whose pages do publish Open Graph. That is the repository's
  central rule inverted -- an unmeasured field read as a measured one -- and it travelled, because
  `crawl/evidence.py` projects those columns into `OG:Title`, `OG:Description` and `OG:Image` of
  the Internal:All frame, so a check saw blank on a native crawl and the real value on a Screaming
  Frog export of the same site. `check_og`'s honesty contract hid the damage rather than showing
  it: with no OG column populated anywhere it skipped, so a site with Open Graph audited
  identically to a site without it. The parser is unchanged; only the read side moved. A new
  `tests/test_open_graph_seam.py` crawls two loopback fixture sites and follows the value from
  parser to `pages.jsonl` to the `pages` table of the scan artifact to the `OG:*` frame columns,
  crawls a second site with no Open Graph to hold "empty because absent" apart from "empty because
  unread", and pins the parser's key shape in the same file -- the failure was two correct modules
  disagreeing about a key name with no test spanning both.
- Answer "is this site linked well, and where is it linked badly", taking the registry from 157
  to 161 checks and adding an `internal-linking` skill (#634). The pieces existed -- every edge
  with its position, anchor and nofollow flag in the `links` table, `ORPHAN_PAGE`,
  `INLINK_BOILERPLATE_ONLY`, `LOW_LINK_SCORE` -- and nothing turned them into an answer about the
  graph. A 40 920-page audit carried the wrong headline for hours because of it: 66% of the
  sitemap was reported unreachable by internal links, a figure produced by subtracting a crawl's
  URL count from the sitemap's, which measures "the crawler did not get there". Walking the
  artifact's own 5 252 235 edges from the home page put reachability at 91.9%. The real defect
  was depth: 23 742 pages -- 58% of the site -- were more than ten clicks from the home page, and
  the deepest was 3 005, because the archive is a linked list of "next post" links rather than a
  tree. Click depth is now the headline the audit prints. `summary.internal_linking` carries the
  histogram, the maximum, the reachable/unreachable split, internal edges by page position, and
  how many edges repeat another edge, and `audit.md` renders it beside the health summary. The
  walk starts at the URL the crawl actually began from, never at `pages.crawl_depth`: that column
  records where the crawler happened to reach a URL, and a sitemap-seeded crawl records 0 for
  most of a site, so a walk seeded from it describes somewhere else. When no start URL was
  recorded and more than one page claims depth 0, the measurement refuses to run and says which.
- Four checks read those numbers. `DEEP_CLICK_DEPTH` fires on an indexable page whose shortest
  route from the start URL exceeds `thresholds.click_depth_max` (default 10, and every finding
  names the number it used). `DUPLICATE_INTERNAL_LINK` fires on a page that writes the same
  destination and anchor more than once: on the site above, 509 744 of 1 134 307 edges -- 44.9%
  -- were repeats, and they were one masthead emitted twice with the second copy removed by
  JavaScript, a template defect that had taken a hand-written script to find. `LINK_INSIDE_HEADING`
  and `IMAGE_LINK_WITHOUT_TEXT` need what a link's page region cannot carry -- the anchor's own
  ancestor chain and contents -- so a native crawl now records a small, capped per-page
  `link_placement_json` beside the heading outline, and both checks skip by name on a Screaming
  Frog export rather than run clean on nothing.
- The honesty rules the rest of the analyzer holds apply throughout. An edge with no recorded
  position is counted as `unclassified` and never folded into `content`: `link_position.classify`
  defaults to off, which leaves that on every edge, and a run in that state must read as "nobody
  looked" rather than as a site whose links are all body copy. A position absent from the
  distribution is reported as "no edges classified here", not as a region the site lacks -- rules
  are tried in order and a menu inside `<header>` matches `nav` first. A partial crawl withdraws
  the click-depth verdict whether or not it fired, because a frontier never fetched may hold the
  shorter route, and a check silently finding nothing would read as "every page is within the
  floor".

- Stop reporting a requirement-gated check as clean when it was never evaluated (#635).
  `H2_MISSING` only fires when a configuration sets `requirements.require_h2`, and the default
  is false, so on an ordinary run its branch is unreachable. It left no trace of that: coverage
  counted it among the silent checks -- the bucket that means "ran over every page and found
  nothing" -- and a reader was told the site passed a check nobody ran. On the audit that
  surfaced this, 61% of the site's pages had no H2 at all and 5 066 of those were longer than
  800 words, so the check would have fired thousands of times. It is now declared skipped by
  name, with a reason naming the setting that turned it off, so the answer is "not evaluated,
  and here is how to evaluate it" rather than silence. `require_canonical` is the same shape
  and gets the same treatment; its default is true, so it was not producing a wrong reading
  yet, and now cannot start.

- Fix the task backlog heading reading "Audit Tasks — None" for any audit whose run
  recorded no project (#640). `render_tasks_md` built the heading with
  `src.get("project", "site")`, but `"project"` is always a present key in the backlog's
  `source` block -- it is set at build time from `run.get("project")`, so a run with no
  project stores the value `None` there rather than leaving the key absent, and
  `dict.get`'s default only fires for a missing key, never for one holding `None`. A
  native crawl and a reanalysis both leave `project` unset, so every backlog built from
  either got the literal word "None" as its heading. The heading now falls back with
  `or`, which does catch a stored `None`, and reaches for the site's `start_url` or
  `source` before giving up -- the same order `seohead.reports.facts.crawl_domain`
  already used to resolve a site's name from its `run` block, now exported and reused
  here instead of reinvented, so a backlog built from a project-less audit names the
  actual site instead of the word "site". `seohead.reports._normalize_sf_audit` had the
  same latent shape one line over (`run.get("project") or ""`, at `reports/__init__.py`)
  -- already guarded against a stored `None`, but its fallback was an empty domain rather
  than the URL the run also knows, so the docx/md "SEO Audit: " heading it feeds went
  blank in the same situations. It now falls back through the same `crawl_domain`
  resolution rather than staying empty.

- Read the heading outline as a sequence and as a map of the page, taking the registry from 155
  to 157 checks (#632). The eight existing heading checks judge headings as a set: they can say
  a page has one H1 and four H2s, and nothing more. So a page whose DOM order is `H2, H2, ..., H1,
  H2` satisfied every one of them, and so did a page whose H2s were all menu labels in its
  masthead -- found on a live site as an article template with eighteen headings standing before
  its own H1, reported as clean on headings. What was missing was the evidence, not the rule:
  `pages` stored `h1`, `h1_2` and `h2` -- three strings with neither order nor place. A native
  crawl now records the whole `h1`-`h6` outline in DOM order, each heading with its level, text
  and page region, in a new nullable `heading_outline_json` column, and `HEADING_BEFORE_H1` and
  `HEADING_IN_PAGE_CHROME` read it. The region reuses the taxonomy link classification already
  uses (`nav`, `header`, `sidebar`, `footer`, `content`, `other`), so "in the header" means the
  same thing about a heading as it already does about a link. A heading no position rule matched
  is placed against the content area only when the document named one -- a `<main>`,
  a `[role=main]`, an `<article>` or a configured selector; where the content root falls back to
  the whole `<body>`, calling a heading "content" would mean nothing more than "somewhere on the
  page", so the region is recorded as unmeasured and the page is named among the run's skipped
  checks with that reason rather than passed as clean or flagged as a defect. Order needs no
  region, so `HEADING_BEFORE_H1` still answers for those pages. A Screaming Frog export carries
  no outline at all, and both checks skip there by name.

- Keep a finished collection when the audit that follows it crashes (#627). A native crawl of
  a live site spent twenty minutes gathering 600 pages into a `--scan-out` artifact and then
  printed `error: 'rel_next_2'` -- a `KeyError`'s `str()` is the key and nothing else. No
  phase, no traceback, and no sign that every page it had collected was sitting complete and
  re-analysable in the file. Collection commits its rows and closes before the audit begins,
  so anything raised after that point is the analyzer misbehaving and never the crawl; an
  unexpected exception there is now named by phase, carries the exception's type as well as
  its message, and points at the retained artifact and the command that re-analyses it,
  exactly as the oversized-audit path already did. The run reports `audit_available: false`
  with that reason rather than exiting on the exception, and `crawl-site` exits 2 for it --
  the same third answer the log-scan contradiction already uses, because 0 would read as "the
  audit ran and found this" and 1 as "the command produced nothing", and neither is true of a
  crawl whose evidence survived. The `KeyError` itself did not reproduce: the same command
  against the same site on current main completed, so this changes what happens when an audit
  fails, not what made that one fail.

- Stop losing a completed crawl's audit to a fixed 30-second inspection budget on reopen
  (#631). `NativeScan.open()` reads a native scan's header through `inspect()`, which also
  runs `PRAGMA quick_check` and `PRAGMA foreign_key_check` over the whole database -- a real
  safety check, but one whose cost scales with the artifact while the budget did not. A
  3.5-hour, 40 920-page crawl finished collecting everything it set out to collect and then
  produced no audit at all, because reopening the resulting 1 GB artifact needed roughly 48
  seconds of validation against a budget fixed at 30. Worse, the failure gave no reason to
  suspect a budget: the deadline abort surfaces from SQLite as a bare
  `sqlite3.OperationalError("interrupted")`, and that was wrapped verbatim into `cannot
  inspect native scan: interrupted`, indistinguishable from a genuinely corrupt file. Both
  are fixed. The default budget is now derived from the artifact's size (a 30-second floor
  for small files, plus a generous per-byte allowance well under the ~22 MB/s measured
  validation rate, since this is a ceiling against a hung read and not a performance target)
  -- an explicit `timeout_seconds` from a caller still overrides it. And a deadline abort now
  raises a `ScanError` that names the budget, the elapsed time and the artifact's size, so an
  operator can tell a slow large file from a corrupt one instead of reading "interrupted" as
  something they did wrong.

- Add `crawl-site --resume <scan.sqlite>` (#619). `NativeScan.resume_snapshot()`, the
  `resume_state` table and the writer that fills it had existed since the scan artifact was
  designed, and `crawl_to_scan` already continued an existing file -- but only for a caller
  that reproduced the original effective settings byte for byte, and no interface offered
  that. Pointing `--scan-out` at an interrupted artifact with any changed flag was refused as
  a configuration mismatch, so in practice an interrupted crawl (6 516 of 34 635 pages, over
  somebody else's site) had to start from zero. `--resume` takes the artifact and nothing
  else: the start URL and the complete effective configuration are read back from the file,
  because they are what the stored frontier was built under, and any other crawl-shaping flag
  passed with it is refused rather than applied. A scan written by a different producing
  build, recorded for a different start URL, already finished, derived from a reanalysis, or
  crawled with credential headers it can only store redacted is refused by name from the file
  alone, before a single request is dispatched. `crawl-site` now also prints one line to
  stderr on exit saying whether the crawl finished or stopped early, why, and how to continue
  it -- except after a URL or duration budget, which a resume reads back too and therefore
  cannot pass, where the line says that instead of offering a no-op. `run.crawl_resumed`
  remains the durable record: a resumed crawl of the offline fixture site fetches each URL
  exactly once across both processes and produces an audit identical to an uninterrupted
  crawl's in every field but that one.

- Stop `render-check` calling an unrendered page clean, and stop it discarding raw-HTML
  evidence with a failed render (#642). Two consequences of #623's guard, both in
  `render.py`. First, a render that timed out at its load milestone falls back to reading the
  DOM at `domcontentloaded` -- sound in itself -- but when no script had run by then the
  rendered DOM equalled the raw HTML, the comparator found nothing, and the run reported
  `ok: True`, `js_dependent: False` and the single finding "Raw HTML and rendered DOM are
  materially equivalent; JavaScript rendering does not determine SEO-visible content", graded
  a notice, about a page nobody had rendered. `wait_reached` recorded the truth, but the site
  audit carries findings text and nothing else into its report, so the truth never arrived.
  A run that fell back to an earlier milestone now says so in the findings list itself,
  withholds the all-clear, and reports `js_dependent: None` -- this run does not know --
  instead of `False`. It fired under the default `--wait load`, not only under an explicitly
  requested `networkidle`. Second, the guard returned its single "the comparison is
  unavailable" statement before reaching the empty-shell branch, and `render_check`'s early
  return omitted the `empty_shell` key altogether -- so an empty single-page-application
  mount point, which `detect_empty_shell()` reads out of the raw server response and which
  needs no browser at all, was thrown away together with the render that failed. Since an
  SPA shell is exactly the page whose render times out, the finding was lost precisely where
  it mattered. A finding derived from the raw half now survives an unfinished render; only
  findings derived from the rendered half are suppressed. In a crawl, a render probe that
  reached no verdict now lands in `patterns_unprobed` with its reason (#626's channel)
  rather than counting as a pattern measured and found not to need rendering.

- Report an unreadable robots.txt as a fact about the site, not as an internal error (#629).
  `EMPTY_ROBOTS` -- the stand-in `_fetch_robots` returns on every path where robots.txt could
  not be read -- was still the parsed-robots shape from before the parser grew groups and
  sitemaps. The success path returns `parse_robots`'s shape, and that is the only shape the
  scan artifact's `robots_summary` context accepts, so in `--scan-out` mode a crawl aborted
  with `native parsed robots summary is invalid` whenever robots.txt was unreachable, answered
  429 or 5xx, redirected off-host, into a loop, past the hop budget or to a non-`text/plain`
  body -- or was a plain 404, which is not an error at all but the RFC 9309 "no restrictions"
  case and the most common of the seven. Both documented outcomes were lost behind that one
  line: a missing robots.txt now crawls unrestricted again, and an unavailable one stops the
  run with the note saying which of the six ways it failed. The legacy directory-mode crawl
  never noticed because nothing reads the old keys. Found on a live site whose robots.txt
  302s to a cookie-sync endpoint for any user agent it does not recognise.

- Fix `escalate()` counting a failed render probe as "this pattern needs no rendering"
  (#626). It samples one URL per template pattern to decide whether that pattern needs a
  fuller, JavaScript-rendered fetch, but when every sample for a pattern failed to probe at
  all — a browser launch failure, a timeout, or an incomplete render, the ordinary failure
  modes of a real crawl — the `for`/`else` still recorded the pattern as probed, so it ended
  up neither escalated nor listed among the patterns the run could not evaluate. That
  inverted this project's central rule that missing or unmeasured evidence must never read
  as clean: a pattern nothing was learned about was silently crawled static and reported the
  same as one that was genuinely measured and found not to need rendering. `EscalationResult`
  now reports such a pattern in `patterns_unprobed`, alongside the deadline-cut patterns
  already tracked there, and records the probe's own failure reason in the new
  `patterns_unprobed_reasons`, so a report can say "this pattern was not evaluated" instead
  of implying it was. A pattern with at least one successful probe is unaffected: this only
  changes what happens when every probe for a pattern comes back `ok: False`.

- Stop `render-check` reporting an unfinished render as a site defect (#623). When the render
  did not complete, the rendered snapshot came back with no title, no `h1`, no canonical, zero
  links and a fifth of the raw response's bytes -- and the comparator read that emptiness as
  evidence about the site, emitting "the title changes after JavaScript" about a title the
  render never read and "the canonical is injected by JavaScript" about a canonical it never
  saw. On one live audit that was four confident wrong findings in a row, each of the kind that
  sends a developer hunting a JavaScript bug that does not exist. The pair is now judged before
  it is compared: when the raw response carries landmarks and the rendered document carries none
  of them at all, at a fraction of its size, the check returns `ok: false` with
  `reason: "incomplete_render"`, a named reason carrying both byte counts, and both snapshots
  for inspection -- no findings, and `js_dependent: null` rather than `false`, because the run
  does not know. `site-audit` files that in `summary.tools_failed`, where an unmeasured check
  belongs. The detection is deliberately narrow: one surviving landmark, or a rendered document
  at least half the raw response's size, is compared as before, and a page with no title and no
  links on either side is measured and merely empty rather than unmeasured. A title a script
  genuinely rewrites still fires its finding. The wait strategy copes with the ordinary cause
  too: a site whose long-polling analytics, chat or ad scripts never let it go network-idle
  timed the whole check out under `--wait networkidle`, and now falls back to reading the DOM at
  `domcontentloaded` -- without a second navigation -- with `wait_reached` recording which
  milestone the snapshot actually came from, plus a short settle for deferred scripts. A page
  whose DOMContentLoaded never fired at all remains a rendering failure, not an incomplete one.

- Give `crawl-site` a live progress line (#619, progress half). A native crawl printed its
  effective request rate and then nothing at all until it finished; on a 34 000-page site the
  only way to tell it was still working was to watch the scan file grow in `ls -la`. The line
  now refreshes in place on a TTY with pages fetched, the URLs known so far, the percentage of
  that set, the current request rate over a trailing window, elapsed time, and the scan
  artifact's size when `--scan-out` is writing one. What it deliberately does not do is imply a
  total it cannot know: a crawler discovers its own workload, so the denominator is
  `fetched + queued` capped at the URL budget, it is labelled `known` rather than `total`, and a
  header line printed once says the frontier is still growing and that the percentage is not an
  estimate of when the run will finish. An unmeasurable rate reads as `rate n/a`, never as
  `0.0 req/s`. Both numbers come from the crawl's own structures -- for a SQLite scan, from the
  same `resume_snapshot` query the collection loop steers by -- so what is on screen is what the
  artifact holds at that moment rather than a parallel tally that can drift from it. A non-TTY
  (a pipe, a log file, CI) gets a plain line every 30 seconds instead of carriage-return
  redraws, and a new `-q`/`--quiet` silences both the progress line and the startup rate line
  while leaving the JSON result on stdout untouched.

- Close the three pagination rows #385 left open, taking the registry from 152 to 155 checks.
  `PAGINATION_MULTIPLE` and `PAGINATION_URL_NOT_IN_ANCHOR` report a page that declares two
  different successors, or one whose declared URL is not also an anchor on the same page.
  Successors are compared after URL normalization, the same identity the anchor half uses, so
  `/blog/page/2` and `/blog/page/2/` from two plugins are one successor spelled twice rather
  than an ambiguous series. `PAGINATION_URL_NOT_IN_ANCHOR` needs a page's whole link inventory
  and reads the All Inlinks export for it; `PAGINATION_MULTIPLE` does not, and now reads the
  lighter `Internal:All` `rel="next" 2` / `rel="prev" 2` occurrence columns first, the same way
  `CANONICAL_MULTIPLE` already answers from `Canonical Link Element 2` — All Inlinks is only a
  fallback for a profile whose `Internal:All` was written without those columns.
  `PAGINATION_SEQUENCE_ERROR` reports a break in a page-number run the series otherwise
  follows, per the issue's own caveat: a series may start at a number other than one, a stride
  is not a break, and a series whose URLs do not state a page number is declared unevaluated
  rather than judged against a numbering that would have had to be invented. Every series left
  unjudged is named among the run's skipped checks with the reason true of *that* series --
  a stride, a cycle, a series too short to hold a run and an unreadable page number are four
  different statements, and three of them describe series whose URLs all state their number.
  The count is per series, not per run, so the ordinary WordPress shape (page one at an
  unnumbered `/blog/`, the rest at `/blog/page/N/`) cannot disappear behind one judgeable
  series elsewhere on the same crawl.
- Add a check-verdict coverage gate (#98): `test_check_producer_gate.py` proved every check
  ID is registered and every `check_*` function is dispatched, but said nothing about whether
  a check's conclusion was ever proven true against markup that actually has the defect, and
  stays silent on markup that does not -- exactly the gap that let #94, #95 and #96 pass every
  existing test while still misfiring on live sites. `test_check_verdict_coverage.py` scans
  `tests/` structurally (AST, not a fixed call-site list) for both halves per check, and fails
  the build for any check newly added to the registry without a two-sided test or a named,
  reasoned exemption. Of the 152 checks in the registry, 88 already had two-sided proof; a new
  `test_check_verdict_gaps.py` closes 22 more of the cheaply-testable gaps the scan surfaced
  (title/description length and duplication, H1 duplication, H2 presence, URL hygiene,
  directive and markup checks, response codes, and the two native-filter-export checks). The
  remaining 64 are named, not hidden, in the gate's `KNOWN_UNCOVERED` ratchet -- shrink that
  list as coverage lands; it must never grow to admit a check added after this gate existed.
- Fix a robots.txt group-selection defect that let a blank `User-agent` value void a site's
  default policy (#566). A bare `User-agent:` line, or one whose bot name an inline comment
  swallowed (`User-agent: # old bot rule`), parsed to an empty token. `_rules_for` treated that
  token like any other literal name: it is a zero-length prefix of every agent, so it matched
  all of them, and because it counted as a *named* match it outranked the file's real
  `User-agent: *` group for every crawler the file never mentions. A robots.txt whose only
  substantive rule was `User-agent: * / Disallow: /` therefore read as fully crawlable -- both
  in the AI-bots access report and in the live crawler's own fetch gating, which calls the same
  `is_allowed`. Blank tokens are now skipped, so such a group names nobody and the wildcard
  group applies; the most specific named group still wins, and a robots.txt naming nobody still
  means everything is allowed.
- Fix four export-selection and run-validation defects that let an audit present a result it
  could not support (#209, #210, #215, #216). `internal_all`'s matcher required only the
  filename token `internal`, so a partial per-type Internal tab (e.g. `internal_html.csv`,
  missing every non-HTML row) satisfied the required Internal:All master export with no
  missing-master warning; the matcher now also requires `all`. `discover_exports` picked the
  first candidate for a logical key in sorted filename order when two files matched it (e.g.
  `internal_all.csv` and `internal-all.csv` both present with different rows), silently
  discarding the other with nothing in run metadata to say a choice was made; it now raises,
  naming every candidate. `run_sf` picked the newest timestamped subfolder under `--out`
  regardless of when it was created, so a re-run that exited 0 without writing anything (a
  startup failure Screaming Frog does not report as a nonzero exit) could return a prior
  invocation's exports as if they were fresh; it now compares against a snapshot taken before
  the process starts and fails loudly when nothing new appears. `build_command` added
  `--auth-config` only when the given profile file already existed, silently starting an
  unauthenticated crawl on a typo'd or deleted path — appropriate for the optional
  `seospiderconfig` default, wrong for a profile the caller explicitly requested; a missing
  explicit `sf_cli.auth_config` now raises before Screaming Frog starts.
- Fix two content-extraction defects in `seohead/checks/parser.py` (#138, #140). `collapse_whitespace`
  decoded HTML entities a second time on top of the single decode BeautifulSoup's lxml parser
  already performs on every `tag.get_text()`/`tag.get(attr)` value it hands to that helper — a
  silent no-op on ordinary markup, but on a page whose CMS or import pipeline already
  double-escaped its entities (a real, common artifact) it turned visibly-broken entity soup into
  clean-looking text and shortened the reported length below what a browser tab or a SERP snippet
  actually renders, flipping length-based title/description checks in both directions. The helper
  no longer decodes at all. Separately, link and URL-source extraction walked the whole document
  unconditionally, so an `<a>`/`<img>` that existed only inside an inert `<template>` — never part
  of the rendered document per the HTML spec, never requested by a browser or a crawler unless a
  script clones it in — was reported as a real, on-page link and actually fetched by `spider.py`,
  the same phantom-URL failure mode closed for `<base href>` in #4; both extractors now skip
  `<template>` descendants. `<noscript>` is deliberately left reachable there, unlike in text
  extraction: it is real, spec-defined fallback markup a JS-disabled client (and search engines'
  non-rendering crawl pass) does load, and excluding it would hide a genuinely fetchable URL from
  the auditor. Word count also no longer counts `<svg>`/`<math>` descendant text (an icon sprite's
  accessibility `<title>`, glyph `<text>`, MathML notation) as body copy — a 20-icon header could
  double a page's reported word count against its real content — and that exclusion list now lives
  in one place (`content_area.TEXT_EXCLUDED_TAGS`) shared by both text extractors instead of two
  copies that had already drifted out of sync.
- Keep a link's full `rel` tokens, its `target` attribute, and its raw (pre-resolution) href,
  and extract `<form>` elements — method, action, whether a password field is present (#125).
  Six catalogued issues depended on those three facts being discarded: unsafe cross-origin
  links (`target="_blank"` without `rel="noopener"`/`"noreferrer"`), protocol-relative links
  (`//host/path`, before resolution), outlinks to localhost, a page receiving both a followed
  and a nofollow internal link, an insecure form action, and a password form served from a
  plain-HTTP page. All six are now `UNSAFE_CROSS_ORIGIN_LINK`, `PROTOCOL_RELATIVE_LINK`,
  `OUTLINK_TO_LOCALHOST`, `FOLLOW_AND_NOFOLLOW_INLINKS`, `FORM_URL_INSECURE` and
  `FORM_ON_HTTP_URL` in the registry. The form/localhost/nofollow-mix checks need only fields
  every crawl already recorded, so those four run unconditionally; the cross-origin and
  protocol-relative pair need `link_attributes.capture`, off by default,
  because the extra per-edge data measured roughly +50% on a synthetic 3387-page,
  150-link-per-page crawl — about +95 bytes/edge, +46 MiB total, `raw_href` alone accounting
  for most of it. Registry grows from 121 to 127 checks.
- Fix `report-build` silently rendering a zero-findings report for an Export Analyzer
  `audit.json` (#151). The documented recipe — `sf run --tasks` piped into
  `report-build --format docx`/`xlsx`/`csv`/`md` — read `findings`/flat page keys, which
  the SF schema does not use (findings live under `issues`, page facts under
  `pages[].metrics`); the mismatch produced a confident `0/0/0/0` summary for an audit
  that found real critical issues, with no findings sections at all. `build_report` now
  recognizes both the native `seohead.site-audit/1` shape and the Export Analyzer shape,
  normalizing the latter into the flat contract the four human-facing writers already
  understand before rendering. `--format json` is untouched — it already relayed the
  original document correctly, which is what proved the data was never missing. A
  document matching neither contract is refused with `ok: False` naming the schema
  mismatch instead of being rendered as an empty deliverable, and the dead
  `if ... : pass` conditional that looked like this validation but always no-opped is
  gone. `tests/test_docs_commands_execute.py` now asserts the documented recipe's
  rendered summary against `audit.json`'s own totals, not just its exit code.
- Close two money-safety gaps in `seohead/data_sources/` (#157, #159). `geo_guard` checked only
  the advisory `country` string, so `search_volume`/`keyword_ideas`/`keyword_difficulty`/`serp`
  could still reach DataForSEO's live endpoint for `location_code=2643` (Russia) or `2112`
  (Belarus) whenever a caller supplied the numeric geo-target without also filling in `country`;
  the guard now checks `location_code` first, since that is the field actually billed on. Separately,
  a network-level exception (`URLError`/`TimeoutError`/`SSLError`) during a billed call — DataForSEO's
  `post`, Arsenkin's `/set`, and Yandex Cloud's `wordstat.topRequests`/`wordstat.dynamics`/
  `web.searchAsync` — used to retry the identical payload with no idempotency key and log only the
  attempt that finally returned a response. None of the three providers offers an idempotency
  mechanism for these endpoints, so a lost response is no longer retried: the attempt is recorded
  in the spend ledger (cost unknown, flagged `attempt_failed: network_error`) and the call fails
  outright instead of risking a second charge. Idempotent reads (Arsenkin `/check`/`/get`, Yandex
  Cloud operation polling) are unaffected and keep retrying. `yandex_cloud.WebSearch.search_batch`
  isolates a lost submission to its own query instead of aborting the batch. `metrika.py` retries
  network errors the same way but is not billed money and was left unchanged; flagged for a
  follow-up if its Logs API export creation should get the same treatment for quota reasons.
- Close a DNS-rebinding gap in `http_client()` (#142): `pinned_target()`, the fix for the
  TOCTOU window described in #14, protected only `collect.py`'s list-mode fetch — the
  other fourteen call sites, including `spider.py`'s `crawl-site` engine, let httpx resolve
  DNS a second, independent time to open the socket, so a hostile resolver answering that
  second lookup differently than the guard's own reached `169.254.169.254` or any RFC 1918
  address regardless of the guard's verdict. `http_client()` now builds every client on a
  transport that pins the connection to the address it resolves itself, on the first
  request and on every redirect hop, keeping the hostname only for the `Host` header and
  TLS SNI — so the fix is structural rather than a discipline every caller had to remember.
  `SEOHEAD_ALLOW_PRIVATE_NETWORKS`/`SEOHEAD_ALLOW_PRIVATE_HOSTS` are unchanged.
- Fix the CLI/MCP exit-code contract for a handler's own-reported failure, and complete
  `SOURCE_FLAGS` (#155, #156). A handler returning `{"ok": false, ...}` — the tool layer's
  documented way of reporting a fetch, parse, or provider failure without raising — used to
  print that JSON and exit 0 on the CLI and return a normal (non-`isError`) result over MCP,
  so a pipeline gating on `$?` or a client checking `isError` alone could not detect it.
  `cli.py` now exits 1 for that case (`log-scan`'s own exit 2 for a self-contradicting run
  stays a separate, documented signal — see `docs/USAGE.md`), and `mcp_server.py` raises
  `ToolError` from a shared `_checked()` wrapper so a client sees `isError` instead. Both call
  a single `handlers.handler_failed()` so the two interfaces cannot drift on what counts as a
  failure. Separately, `SOURCE_FLAGS` gained `--phrase`, `--keywords`, `--query`/`--queries`,
  `--seed`, `--counter`, and `--before`/`--after` — each already identifies a command's whole
  input the way `--url` does, but was missing, so a per-line loop over any of them silently
  processed only its first line. The set is now built by `_source_flag()` at the point each
  flag is declared instead of hand-listed separately, so it cannot drift out of sync again.
- Expand `docs/scenarios/` from ten chains to fifty-six, grouped by the question a reader
  arrives with (#120). Which scenarios exist is decided by the coverage map rather than by
  taste: each declares the catalogued issues it resolves, and
  `tests/test_scenario_coverage.py` asserts that every issue this toolkit claims to find
  appears in at least one, that no scenario names an issue the map does not list, and that
  none omits its limits. All 143 findable issues are covered. Every command in every file
  is still executed against the fixture site in CI — the suite runs 2008 tests, of which
  the doc-command gate is now the largest single group.
- Assert the sitemap protocol's own limits (#124): `SITEMAP_TOO_MANY_URLS` above 50,000
  entries in one file, `SITEMAP_TOO_LARGE` above 50 MB uncompressed, and
  `SITEMAP_URL_DUPLICATED` for a URL declared in more than one sitemap, naming both
  documents. A file over either limit is invalid rather than merely large — a search
  engine may read part of it and discard the rest with nothing the site owner can see —
  and the thresholds come from the protocol, not from config. Both parsers now record the
  per-document byte size and declared count, and the findings name the child sitemap
  rather than the index. Registry grows from 118 to 121 checks.
- Add `docs/GUIDELINE.md`, the document a person reads first (#121): what this is and is
  not, a first run end to end, how to choose a crawl rate and how to tell whether the
  errors are yours, how to read `audit.json` without being misled by it, what the config
  file changes and what it only costs, the six mistakes everybody makes first, what to do
  when the tool is wrong, and what it cannot answer at all. Linked first from the README,
  ahead of the reference.
- Add `docs/COVERAGE_SF_ISSUES.md`, generated from `seohead/sf/core/sf_issue_map.py`:
  every one of the 320 issues in the field's published catalogue, each with exactly one
  status — found by a named check, found by a named command, found in part with the
  missing part stated, a named gap, or a decision with its reason (#119). Of the 212
  in-scope issues, 126 are found today and 17 in part; 41 are gaps and 28 need something
  deliberately not built. Accessibility (92) and AMP (16) are declined as single
  decisions, with their full lists shown so the decision is auditable. A test asserts
  every referenced check id and command still exists, so a rename breaks the build rather
  than the document. Supersedes `docs/CHECKLIST_AUDIT.md`, which was written without the
  catalogue in hand.
- The sitemap seeder requests the address the sitemap published, not its normalised form
  (#115). `sitemap.crawl()` returned only the normalised `loc`, so a sitemap declaring
  `/a/` caused a fetch of `/a` — on most CMSes a 301 the crawler invented and then
  reported as a fact about the site, and a 404 wherever the slashless form is not routed
  at all. Each entry now carries `loc` as published alongside `loc_normalized` for the
  consumers that compare on it. Redirect statistics from earlier sitemap-seeded crawls
  are suspect for this reason.
- Restructure the operator skill into a controller directory (#111). `.claude/skills/`
  now has two tiers: 21 method skills, each covering one thing well, and `control/`,
  which decides which of them to run on a site nobody has looked at yet. The controller
  routes rather than restating, and carries five loadable sub-skills (scoping, rate and
  load, reading an audit, verifying, deliverables) and a three-file reference archive
  (defects found on live sites and what gave each away, which population each check
  describes, and what the toolkit cannot answer at all). The English-only gate and the
  doc-command gate now cover every Markdown file under a skill directory, not only
  `SKILL.md`.
- Add `tests/chains/`: a fixture site built out of the shapes that actually break chains —
  both slash forms of one URL, a body that is not valid UTF-8, a windows-1251 page, a
  masthead outside `<main>`, an off-host link, a robots-disallowed path — crawled over
  loopback, with seventeen assertions about the run as a whole rather than about any one
  module (#112). Four properties: conservation (a number does not change meaning as it
  travels), population (a finding is about a member of the set it describes),
  determinism (two crawls, two concurrency levels, one answer) and representation (a page
  says how it was measured). The population rules are `logscan`'s own, so a contradiction
  the scanner can name is a chain test that asserts it.
- `reconcile_sitemap` reports each URL as it was written rather than as it was normalised.
  Comparison still happens on the normalised key, but a finding that named a normalised
  form named a URL appearing nowhere in the crawl — unactionable for a reader, and
  indistinguishable to the anomaly scanner from a finding about a page never fetched.
- Add `docs/scenarios/`: ten end-to-end chains, each with the real commands in order, the
  artifact that comes out, what it costs, and what that chain cannot answer (#110). The
  rest of the documentation lists what the toolkit has; this describes what it does. Linked
  from the README above the tool list, so an agent evaluating the repository finds the
  chains before the inventory. Every command shown is executed against the fixture site by
  `tests/test_docs_commands_execute.py`, whose extractor and whose English-only and count
  gates now walk `docs/` at every level rather than only its top.
- Add `log-scan` (CLI) and `seo_log_scan` (MCP): read a finished run's own artifacts and
  report claims that cannot all be true at once (#109). Eight rules, each written from a
  defect that shipped past the whole test suite — a recorded size that disagrees with the
  file on disk, a text ratio over 100%, a check firing more often than there are pages to
  fire on, a finding about a URL the run never fetched, a canonical called a redirect while
  that URL answered 2xx in the same run, a summary that disagrees with its own rows, words
  counted on a zero-byte page, and pages measured two ways where only some say which. Each
  anomaly names both values and where each was read from. The CLI exits 2 when a run
  contradicts itself, so a pipeline stops instead of publishing the numbers.
- The cross-worker pacing test no longer measures the wall clock (#107). `_DispatchGate`
  now reads the crawl's injected clock instead of `time.monotonic()` directly, so the test
  drives it with a virtual clock that advances only when something sleeps: the dispatch
  instants are the crawler's own arithmetic and the assertion is exact. The old form
  compared real elapsed gaps against a 0.024s floor and failed on unchanged code whenever
  the machine was busy.
- Close the second half of the unwired-settings audit (#91). `http.headers` is merged into
  every request beside the credential headers; `speed.adaptive` gates the throttle's delay
  and concurrency adjustment (the timeout and server-error counters keep running — giving
  up is a separate mechanism from backing off); `discovery.hyperlinks.store` / `.crawl`,
  `discovery.external.store` and `discovery.redirects.crawl` now decide what the crawl
  records and what it follows. `discovery.canonicals.*`, `discovery.external.crawl` and
  `discovery.redirects.store` are removed rather than wired: they named capability the
  spider does not have (canonical-chasing, cross-host crawling) or state it cannot
  withhold, and a setting that appears in `--config-help` and the run manifest while
  changing nothing is worse than no setting. The coverage canary's exemption set is now
  empty: every `DEFAULTS` path changes an observable outcome and is named by a test.
- Detect the content area from the document's own semantics when nothing is configured
  (#96): `main`, then `[role="main"]`, then `article`, recording which one matched as
  `auto_main` / `auto_role_main` / `auto_article`. The previous default — the whole body
  minus the `nav` and `footer` tags — counted 126 template words out of 433 on a live
  WordPress post (29%), including a skip-to-content link, and that inflation feeds
  `THIN_CONTENT` and `LOW_TEXT_RATIO` in the same direction on every page of a template.
  `header` and `aside` join `nav` and `footer` in `DEFAULT_EXCLUDE_TAGS` for the fallback
  path. A configured selector still wins, and one that matches nothing still falls back to
  `fallback_default_body` rather than silently auto-detecting a different region.
- `URL_NOT_IN_SITEMAP` now compares pages with pages (#94). It compared a sitemap's URLs
  against every destination in the crawl's link graph, so on a live 124-page site it fired
  392 times — 362 image files a gallery links to directly, five off-host links, and 30 URLs
  the crawl never fetched — which was 74% of that report and buried the findings that were
  real. The observed side is now the pages a sitemap is supposed to declare: fetched, 2xx,
  HTML, same-host and indexable. `reconcile_sitemap` takes that population as a separate
  `comparable` argument, so `SITEMAP_ORPHAN` keeps asking about reachability against every
  link destination and cannot invent orphans; what was set aside is returned under
  `linked_not_comparable` rather than dropped.
- Fix the canonical checks on a site that serves both slash forms of a URL (#95).
  `norm_url` folds a trailing slash away on purpose, so a canonical written without one
  matches the page that has it — but the normalised index kept only one page per key, and
  a crawl of a typical WordPress site holds two: `/x` (301) and `/x/` (200). Reading
  whichever was inserted first made `CANONICAL_TO_REDIRECT` report 78 live pages whose
  canonical answers 200. `AuditContext` now exposes `pages_by_norm` with every page under
  a key, `page_by_norm` returns the variant that answered 2xx, and `CANONICAL_TO_REDIRECT`
  and `CANONICAL_NON_INDEXABLE` only fire when no variant contradicts them.
- Fix `size_bytes`: it is now the response body as it arrived on the wire, measured before
  the body is decoded (#99). It was measured from the decoded string, so every byte that is
  not valid UTF-8 became U+FFFD and re-encoded to three — a 739 KB WebP from a real crawl
  was recorded as 1.27 MB, and the inflation factor differs per file. Images, PDFs, fonts
  and HTML served in a legacy charset (windows-1251) were all over-counted, and so was the
  text ratio computed against that denominator. The HTTP cache stores the wire size with
  the entry, so a replayed page reports what the live fetch reported; its schema moves to
  `http_cache.v2` and v1 entries are re-fetched once rather than replayed without a size.
- Add `docs/TOOL_REFERENCE.md`, generated from the MCP tool definitions
  (`seohead/mcp/tool_reference.py`, `scripts/generate_tool_reference.py`): every
  `seo_*`/`sf_*` tool's arguments with type and default, its cost (network/writes/
  idempotent/spend, read from its `ToolAnnotations` profile), and its own docstring's
  behavior and failure-mode notes. `tests/test_docs_drift.py` fails the build if it
  drifts from the tool definitions or is missing a tool.
- Add `tests/test_docs_commands_execute.py`: extracts every `seohead ...` command
  shown in README/docs/skills/examples (`scripts/doc_commands.py`) and runs each one
  offline, against a loopback fixture site (`tests/doc_fixtures/`) and materialized
  copies of `examples/`, asserting a clean exit. Commands that need real
  infrastructure (RDAP/DNS, a licensed SF binary, a paid provider credential, the
  never-returning `mcp` server) are at least parsed against the live argument parser.
  A documented command that no longer works now fails CI instead of shipping stale.
- Reshape all 21 technical workflow skills (`.claude/skills/*/SKILL.md`) into a
  shared shape: Trigger, Anti-trigger, Preconditions (as a checklist), the existing
  Workflow, Decision points, Definition of done (as a checklist), and Cost. Fix the
  stale command-coverage count at the bottom of `docs/SKILLS.md` (the real count,
  recomputed by scanning every skill file, is asserted by a new drift test).
- Fix several tool/test counts that had silently drifted from the real registries
  (a stale CLI command count in `docs/USAGE.md`/`docs/COMPARISON.md`/`README.md`/
  `docs/TESTING.md`, a stale MCP tool count in `docs/TOOLS.md`, a stale tool-reference
  count in `docs/README.md`, and the offline test count) and pin the fixed ones with
  a regression test (`test_stale_tool_counts_do_not_reappear`).

- Add the four static Lighthouse audits that need no browser and no third-party API
  (#59): `MISSING_CHARSET`, `MISSING_DOCTYPE`, `VIEWPORT_MISSING` and `NO_COMPRESSION`,
  each computed from evidence a crawl already holds. `content_encoding`,
  `meta_charset`, `doctype` and `viewport` join the normalized column vocabulary, so an
  SF export that happens to carry them as Custom Extraction columns feeds the same
  checks. Registry grows from 114 to 118 checks.
- Wire the ten remaining crawler settings that were validated, written into the run manifest, and
  described by `--config-help` but read by nothing (#63): `limits.max_response_bytes`,
  `speed.max_delay_seconds`, `robots.user_agent_token`, and `speed.stop_after_consecutive_timeouts`
  now configure behaviour that was previously hardcoded; `robots.unavailable_means_stop` now
  governs whether an unreachable or 5xx robots.txt stops the crawl or is treated as unrestricted
  (previously an unreachable robots.txt never stopped the crawl regardless of policy, while a 5xx
  one always did — now both are the same "unavailable" case, gated by the setting);
  `limits.max_url_length`, `limits.max_query_variants_per_path`, `http.retry_on_timeout`, and
  `discovery.follow_nofollow` are newly-implemented behaviour. `http.user_agent` is now applied to
  real requests instead of always sending the toolkit's default. Add `crawl-describe-settings`
  (CLI) and `seo_crawl_describe_settings` (MCP) so an agent can discover the configuration surface
  without a filesystem (#23).
- Add custom search (`tools/custom_search.py`) and custom extraction
  (`tools/custom_extract.py`) over an already-crawled corpus: presence/absence filters
  (raw source, visible text, a named CSS element, or an XPath node) and CSS/XPath/regex
  extractors, both reporting which representation (static markup vs. rendered DOM) they ran
  against. Absence is counted honestly: a page whose fetch failed is excluded from both the
  numerator and the denominator rather than counted as missing. Extraction runs each
  (document, extractor) pair under a wall-clock budget (`SIGALRM` on POSIX): a pathological
  expression aborts only that document, and the run still finishes.
- Add link position classification (`tools/link_position.py`): nav/header/sidebar/footer/content,
  by ordered rule over a link's ancestor path, reusing `content_area.py`'s notion of content
  rather than inventing a second one. Wired into `crawl/spider.py`'s link recording behind
  `link_position.classify` (default off — a position per link costs memory on a large crawl) and
  aggregated site-wide by `crawl/linkgraph.py`'s `inlink_composition`, which now feeds a new
  `INLINK_BOILERPLATE_ONLY` audit finding for pages linked only from boilerplate. Registry grows
  from 104 to 105 checks.
- Add eight post-crawl second-pass computations that only become answerable once a crawl is
  complete (issue #15): an internal link score computed from the `all_inlinks` edge graph
  (`LOW_LINK_SCORE`); a canonical target no hyperlink ever points to (`UNLINKED_CANONICAL`);
  `rel="next"` loop and unlinked-series detection (`PAGINATION_LOOP`,
  `UNLINKED_PAGINATION_SERIES`); hreflang reciprocity (`HREFLANG_MISSING_RETURN_LINK`);
  inlink-composition aggregates (`ONLY_NOFOLLOW_INLINKS`, `ONLY_NONINDEXABLE_SOURCE_INLINKS`);
  the concrete shortest discovery path from the crawl seed (`DEEP_DISCOVERY_PATH`); a
  self-computed mixed-content fallback (`INSECURE_SUBRESOURCE`); and near-duplicate clustering
  from stored page text (`NEAR_DUPLICATE`/`DUPLICATE_BY_HASH`), wiring `tools/duplicate.py` and
  `tools/content_area.py` into the audit for the first time. `ORPHAN_PAGE`, `SITEMAP_ORPHAN` and
  the two new "unlinked" checks are now withheld — reported as a named skip, not a finding — on a
  crawl the aggregator has marked partial, since "nothing links here" is unprovable on a
  truncated crawl. Registry grows from 104 to 114 checks.
- Audit the crawl registry against an external technical-SEO checklist and close eight cheap,
  verified gaps: five hreflang checks (invalid language/region codes, missing self-reference,
  missing x-default, duplicate entries, non-canonical targets), two robots directives
  (`notranslate`, `unavailable_after`), and canonical URLs containing a fragment. Registry grows
  from 96 to 104 checks. See `docs/CHECKLIST_AUDIT.md`.
- Add `asset-weight-check`: fetches a page's linked CSS/JS and reports
  render-blocking resources, oversized files, duplicate libraries (by content
  hash), missing minification, missing `font-display`, legacy polyfilled JS,
  and missing compression/long-lived caching.
- Add `crawl-site --sitemap <url>` (and `sitemaps.auto_discover` in `--config`): seed the native
  crawler from a sitemap's declared URLs, follow links from each, and reconcile the declared and
  observed sets into `audit.json`'s `summary.sitemap`, under the same `SITEMAP_ORPHAN` /
  `URL_NOT_IN_SITEMAP` check ids the Screaming Frog pipeline already reports.
- Add `crawl-site --config-help`, generated from `seohead/crawl/config.py`, and hide `--max-depth`
  and `--min-delay` from `--help` (still accepted) so the flag surface stops growing with every
  crawler setting.
- Split technology fingerprinting into a fetch step and a pure `analyze_tech` step,
  capture analytics/tag-manager ids instead of only names, and add a `tag_coverage`
  report that groups presence by URL template and stamps how each page was measured.
- Resolve redirect chains and loops as a second pass over a finished crawl's own
  redirect targets, so `REDIRECT_CHAIN`/`REDIRECT_LOOP` no longer require the native
  Screaming Frog Redirect Chains report — a light-profile export or a `crawl-site` run gets
  the same findings for free.
- Add a configurable content area (`content_area.py`) that scopes word count to
  the main region, excluding navigation and footer by default, without
  affecting link discovery; the resolved strategy is reported per page.
- Separate exact from near duplicates in `duplicate.py`: exact matches are
  hashed from extracted text (not raw bytes) and excluded from near-duplicate
  clusters, and comparisons default to indexable pages only.
- Add a boilerplate-consistency report (`boilerplate_report.py`) that hashes
  header/nav/footer per page and flags minority template groups.
- Add dependency-free Markdown extraction (`markdown_extract.py`): a
  content-area-only rendering and a full-document one.
- Wire `markdown_extract` and `boilerplate_report` into the CLI and MCP surface as
  `markdown-extract`/`seo_markdown_extract` and `boilerplate-report`/`seo_boilerplate_report`
  (47 core tools, up from 45), and add the `only_indexable` flag `duplicate_check` already had
  at the handler layer to `seo_duplicate_check`'s MCP signature, where it had been missed.
  Rescope `citability-check`'s URL path from the parser's whole-document `text` field (a single
  collapsed line with no paragraph or heading breaks at all) to `markdown_extract`'s content-area
  Markdown, fixing both the boilerplate dilution the issue raised and a latent bug where the flat
  text silently zeroed the Answer-Blocks and Structure-Quality dimensions for every live URL.
  Left unscoped, deliberately: the parser's `text` field itself, still whole-document, because
  `page_facts.py`'s schema-evidence extraction (`sameAs` social links, breadcrumbs, price/rating
  regexes) depends on facts that legitimately live in header/footer widgets the content area
  excludes; and the Screaming-Frog-driven `THIN_CONTENT`/`LOW_TEXT_RATIO` checks in
  `sf/core/rules.py`, whose `word_count`/`text_ratio` come from Screaming Frog's own export
  columns — third-party data the toolkit has no raw HTML to rescope without re-fetching every
  page, defeating the zero-request offline-corpus design of the SF audit path. The toolkit's own
  crawler (`crawl-site`) already inherits the content-area scoping for free, since its word count
  reads straight from `parser.parse_html`.
- Write down a naming convention (`docs/NAMING.md`) and resolve the module-basename collisions
  and process-named test files it found; no CLI command, handler, or MCP tool name changed.
- Add community, citation, and no-key agent onboarding files.
- Add the permissioned `analytics-console-review` workflow skill and three practical recipes.
- Document support for the current `3.x` security line.
- Require TLS 1.2 or newer and pin direct certificate probes to prevalidated public addresses.
- Add an HTTP response cache for `crawl-site` (`seohead/crawl/cache.py`, opt-in via
  `cache.mode` — default `off`, so no side effect appears behind a default): real HTTP freshness
  semantics (`max-age`/`Expires`, `ETag`/`Last-Modified` revalidation, `Vary`-aware variants,
  `no-store`/`no-cache` honoured), a `replay` mode for debugging that is stamped in the manifest
  and never the default, and an `invalidate` flag for an explicit hard refresh. Every fetched
  page carries `cache_status` (`hit`/`revalidated`/`miss`); the run carries `cache_stats` and
  `cache_replay` in both the handler output and `audit.json`'s `run` block, so a report built
  partly from cache says so. A cache hit costs no request and consumes no throttle delay or
  concurrent dispatch-gate slot either — the wait is issued from inside `fetch_one` itself, only
  once a real network round trip is actually about to happen.
- Add journal-driven reuse to `seohead/core/runlog.py` (`SEOHEAD_REUSE_POLICY`, a per-tool maximum
  age in seconds; default empty, meaning nothing is ever reused). A configured, still-fresh,
  successful prior answer is returned instead of calling the tool again, marked `reused: true`
  with `reused_from_ts` in both the result and the new journal entry it still writes — freshness
  is always measured against when the value was actually fetched, never extended by reuse itself.

## 3.0.0 — first public snapshot

- One Python package with 42 shared CLI/MCP tools.
- Five MCP tools for crawler exports and a 96-check crawl analyzer.
- Domain, CDN, technology, security, mirror, regional, bot, and backlink reconnaissance.
- Schema.org validation and connected graph generation.
- Bounded sitemap-based site evidence and XLSX, DOCX, CSV, Markdown, and JSON reports.
- Optional Wordstat, Yandex SERP, Arsenkin, Metrika, and DataForSEO integrations.
- Twenty-one technical workflow skills and seven packaged SEO playbooks.
- Local stdio MCP and Docker support; no GUI, hosted API, or telemetry.
- Evidence-led public README with reproducible synthetic audit, task, interface, and report visuals.
- History-free public release boundary and explicit third-party notices.
