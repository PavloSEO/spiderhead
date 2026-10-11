# Public design decisions

This document records stable product choices that contributors should not have to rediscover.

## Local interfaces and optional remote contract

SEOHEAD Tools runs locally through a CLI and one stdio MCP server. Both use the same handler
registry. The optional `remote` extra defines an authenticated, versioned HTTP contract over the
same Python core. It does not start a listener or provide a worker backend: self-hosted execution,
target/egress policy and deployment are separate work under #785–#787. Local installs and their
CLI/MCP behavior do not depend on the HTTP extra. The native desktop app
(`desktop/`, PyQt5) is a presentation adapter over the same handlers. There is no hosted account,
web dashboard or public MCP endpoint.

Reports are output formats, not interfaces. XLSX and DOCX are allowed because they are useful work
products; report renderers still contain no network or finding logic.

## Evidence before interpretation

Every output distinguishes observed data, derived findings, missing inputs, and failures. A tool
that cannot measure a signal must not report that signal as healthy. This is why outputs include
fields such as `http_version_measurable`, skipped-check reasons, and `tools_failed`.

## Safe defaults

- User-controlled URL requests block private and non-public networks unless explicitly enabled.
- Potentially intrusive path probes and bot DNS verification are opt-in.
- Image optimization needs a separate output directory unless `in_place=true`; in-place rewrites
  create backups by default.
- DataForSEO uses sandbox unless production is selected explicitly.
- Paid operations are journalled before response parsing.
- Yandex SERP uses the asynchronous endpoint only.

## Two Screaming Frog modes

Export mode is self-contained and tested offline. Live crawl mode is an optional adapter around a
separately installed and licensed Screaming Frog CLI. Convenience in live mode must not break
export mode.

## Lab metrics stay lab metrics

Playwright timing data lives under `metrics_lab`. One browser run is not field Core Web Vitals;
field conclusions require CrUX, Search Console, or another real-user dataset.

## Rendering waits for `load` by default

Long-lived analytics, chat, ad, and websocket connections can prevent `networkidle` indefinitely.
`load` provides a bounded default; callers may select another wait strategy when a site's behavior
requires it.

## Optional heavy dependencies

Playwright, clustering, report formats, sitemap helpers, and MCP support live in extras. A missing
optional dependency returns an actionable install instruction instead of crashing unrelated tools.

## Report generators compute nothing

Audit and aggregation layers own calculations. Renderers only lay out the finished document. This
prevents XLSX, DOCX, CSV, Markdown, and JSON outputs from disagreeing about the same run.

## No bundled GPL fingerprint database

An external technology fingerprint database can be loaded through configuration, but is not
distributed in the MIT package. Built-in signatures are curated in-repository, and compatible
sources are credited in `THIRD_PARTY_NOTICES.md`.

## Offline tests

The test suite does not depend on live sites or provider availability. Network boundaries are
tested with fakes and pure verdict functions; live verification is a separate release step.

## Content-area scoping is per-check, not a blanket flip

Restricting text extraction to a content area (nav/footer excluded) had to be decided per
consumer rather than applied everywhere, because the same whole-document text serves different
purposes:

- `word_count`, `content_text`, and the content-area strategy on `parse` output are scoped —
  a mega-menu must not make a thin page look substantial, and this changes nothing about link
  discovery, which always runs over the whole document.
- `citability-check`'s URL path is scoped: it scores `markdown_extract`'s content-area Markdown,
  not the parser's `text` field. `text` is a single collapsed line with no paragraph or heading
  breaks at all, so scoring it directly would have silently zeroed the Answer-Blocks and
  Structure-Quality dimensions for every live page regardless of content-area scoping — Markdown
  fixes both problems at once. `text` passed directly to `citability-check` is untouched, since
  the caller chose exactly what to score.
- The parser's `text` field itself stays whole-document. `page_facts.py`'s schema-evidence
  extraction (`sameAs` social links, breadcrumbs, price/rating heuristics) depends on facts that
  legitimately live in header/footer widgets a content area would exclude; scoping it would
  silently cost that evidence.
- The Screaming Frog crawl audit's `THIN_CONTENT`/`LOW_TEXT_RATIO` checks stay unscoped: their
  `word_count`/`text_ratio` come from Screaming Frog's own export columns, third-party data the
  toolkit has no raw HTML to rescope without re-fetching every page — which would defeat the
  zero-request, offline-corpus design of the SF audit path. The toolkit's own crawler (`crawl-site`)
  needs no such carve-out: its word count already reads from `parser.parse_html`, so it inherited
  the content-area scoping for free.

## Public history is intentionally clean

The public repository starts from a reviewed snapshot. Internal experiments, research journals,
client artifacts, discarded implementations, and private commit history are outside the public
source boundary described in `PROVENANCE.md`.

## The changelog is assembled from one file per change

A changelog entry is written to `changelog.d/<issue>.md` and never straight into `CHANGELOG.md`.
`python scripts/build_changelog.py` folds the fragments into `## Unreleased` at release time and
`--prune` removes them.

Every branch used to append its entry to the top of `## Unreleased`, so every branch conflicted
with every branch that landed before it, and the resolution was "keep both entries" every time.
`CHANGELOG.md merge=union` was measured and rejected: union merges line by line, so two entries
that share any line -- a blank line inside an entry is enough -- lose one copy of it silently,
with no conflict raised (#638).

`CHANGELOG.md` is deliberately not regenerated on every pull request. A gate holding it
continuously in sync with the fragments would put both branches back into the same file and
rebuild the conflict this removes.

## Page actions in rendered crawls (#1011)

The bounded data-only baseline is implemented for explicit project configuration. The open owner
decisions are whether to accept arbitrary JavaScript inside `extract` and which agent callers may
run actions under the permission model (#977); neither is enabled by this implementation.

- Actions are a closed set: `click`, `scroll`, `wait_for_selector`, `wait_ms`, and `extract`,
  where `extract` returns JSON-serialisable values only.
- Named presets (for example `expand_tabs`, `load_more`, `read_datalayer`) are the reusable form.
  `extract` is limited to selector-based text, `href`, and `value`; arbitrary JavaScript is rejected.
- Any action rule must stay within `settings.py` `evidence.extraction_rules` limits: no regular
  expressions inside actions, and no executable code beyond the accepted extract form.
- Actions do not run under `persistent_profile`, because page storage and cookies could then leak
  between crawls.
- Each step has its own timeout and the URL has a total budget. A step that fails or exceeds its
  budget is recorded in the step result, and a value is reported only when every step it depends
  on succeeded.
- Network access during actions stays inside the existing pinned route guard.
- Who may run actions depends on the agent permission model (#977). Until #977 is decided, the
  contract is recorded without permissions.
- Implementation status: the closed data-only actions (`click`, `scroll`, `wait_for_selector`,
  `wait_ms`, and selector-based `extract`) and fixed presets are wired into rendering behind
  explicit project configuration; they have bounded per-step output and a 60-second static budget.
  The `read_datalayer` preset uses a fixed serializer. Arbitrary JavaScript remains rejected. The
  actor permission policy remains open under #977, so this does not add an agent permission model.
