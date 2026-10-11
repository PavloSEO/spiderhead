# Tool reference

Tools are reachable identically from the CLI and MCP. One implementation, two
faces: `seohead <command>` in the terminal and `seo_<command>` on the local MCP
server (`seohead mcp`). The `sf_*` tools cover the Screaming Frog crawl-audit
workflow specifically — see that section below and the generated
[CHECKS.md](CHECKS.md) for its current check catalogue.

This page is hand-written orientation: what each group is for, and the calling
conventions shared across it. The generated [TOOL_REFERENCE.md](TOOL_REFERENCE.md)
carries the part that must never drift by hand — every argument with its type
and default, and each tool's cost (network/writes/idempotent/spend) and its own
docstring's failure-mode notes — read straight from the MCP tool definitions.

The shared contract: JSON out; when a source is unreachable the tool returns
`{"ok": false, "error": "..."}` instead of raising. An unreachable site is
data, not an accident.

The current registry has 195 commands and 204 callable tools,
with 248 audit checks. These are inventories, not coverage on every input.

<!-- generated-command-inventory:start -->
## Generated command inventory

The command names below are generated from the shared CLI/MCP registry; detailed behavior
and inputs remain in the nearby route sections and generated tool reference.

`ai-bots-check` · `ai-column` · `asset-weight-check` · `audit-workflow` · `backlinks-check` ·
`backlinks-disavow` · `backlinks-profile` · `bi-bigquery-plan` · `bi-destination-apply` · `bi-export` ·
`bi-filter` · `bi-sheets-plan` · `boilerplate-report` · `cdn-check` · `citability-check` ·
`cloudflare-traffic` · `compare-crawls` · `crawl-describe-settings` · `crawl-diagnose` ·
`crawl-diagnose-export` · `crawl-enrich` · `crawl-import` · `crawl-site` · `crtsh-subdomains` · `crux-report`
· `dataforseo-estimate` · `domain-profile` · `duplicate-check` · `evidence-join` · `evidence-normalize` ·
`facts-export` · `findings-view` · `google-keywords` · `google-serp` · `gsc-archive` · `gsc-progress` ·
`gsc-query` · `headers-check` · `hreflang-check` · `images-download` · `images-optimize` · `indexnow-submit` ·
`inspect-url` · `keywords-cluster` · `keywords-exact` · `keywords-expand` · `keywords-seasonality` ·
`links-check` · `llms-txt-check` · `log-analyze` · `log-scan` · `logs-bots` · `logs-import` · `logs-join` ·
`logs-summary` · `markdown-extract` · `marketing-inventory` · `meta-description-drafts` · `metrika-counters` ·
`metrika-report` · `metrika-setup` · `metrika-traffic-pdf` · `miratext-analyze` · `mirror-check` ·
`monitor-collect` · `monitor-configure` · `monitor-local-deliver` · `monitor-run` · `monitor-schedule` ·
`monitor-status` · `parse` · `project-activity` · `project-checklist-init` · `project-checklist-page` ·
`project-checklist-record` · `project-checklist-update` · `project-competitors-add` ·
`project-competitors-list` · `project-competitors-remove` · `project-event-append` · `project-event-page` ·
`project-facts` · `project-inbox-acknowledge` · `project-inbox-goal` · `project-inbox-list` ·
`project-inbox-read` · `project-inbox-submit` · `project-inbox-triage` · `project-inbox-unread` ·
`project-new` · `project-observe` · `project-open` · `project-policy` · `project-prepare` ·
`project-priorities` · `project-progress` · `project-scans` · `project-sources-link` · `project-sources-list`
· `project-sources-unlink` · `project-start` · `project-status` · `project-task-detail` ·
`project-view-delete` · `project-view-list` · `project-view-rename` · `project-view-save` ·
`project-view-show` · `provider-auth` · `provider-collect` · `provider-join` · `provider-readiness` ·
`provider-registry` · `provider-replay` · `provider-verify` · `publication-cohorts` · `redirects-check` ·
`redirects-generate` · `regions-check` · `regions-tree` · `remediation-cases` · `remediation-create` ·
`remediation-ingest` · `remediation-recheck` · `remediation-record-verification` · `remediation-report` ·
`remediation-summary` · `remediation-transition` · `render-check` · `report-build` · `robots-check` ·
`scan-body-diff` · `scan-content-search` · `scan-content-search-page` · `scan-evidence` · `scan-export` ·
`scan-extract` · `scan-fragment-links` · `scan-graph` · `scan-import-urls` · `scan-inspect` ·
`scan-link-inspect` · `scan-list` · `scan-navigation` · `scan-pin` · `scan-prune` · `scan-reanalyze` ·
`scan-rendered-routes` · `scan-requeue` · `scan-sitemap-generate` · `scan-snapshot` · `scan-status` ·
`scan-structured-blocks` · `scan-url-detail` · `scan-url-history` · `scan-url-query` · `scenario-list` ·
`scenario-show` · `schedule-install` · `schedule-run-due` · `schedule-set` · `schedule-show` ·
`schedule-uninstall` · `schema-build` · `schema-check` · `security-check` · `segment-diff` · `semantic-inputs`
· `semantic-similarity` · `serp-fetch` · `site-audit` · `sitemap-crawl` · `skill-list` · `skill-show` ·
`social-meta-check` · `soft404-check` · `sources-doctor` · `sources-export` · `sources-status` ·
`sources-sync` · `spend-report` · `tech-detect` · `tool-catalog` · `topvisor-read` · `verify-fixes` ·
`wayback-history` · `webmaster-url-queries` · `workflow-checkpoint` · `workflow-execute` · `workflow-resume` ·
`workflow-start` · `workflow-status`
<!-- generated-command-inventory:end -->
See [Server logs](SERVER_LOGS.md) for import, join and DNS evidence contracts.

## Core identity, diagnosis and updates

`seohead version --json` reports the versioned core handshake, supported project/scan/ledger
formats, the CLI and MCP tool names, verified package revision/build time and the exact-revision
Desktop compatibility policy. Pass `--required-core-revision` with the revision pinned by a Desktop
bundle to receive a `compatible` boolean; it remains null when either revision is unavailable.
Older package manifests without a build timestamp retain a verified revision and report the
timestamp as unavailable.

`doctor` / `seo_doctor` inspect local Python, Playwright, Chromium-on-PATH, Screaming Frog CLI,
MCP, SQLite locking and optional project-directory permission flags. They do not write a probe,
launch a browser, crawl a target or infer Screaming Frog license validity from executable presence.

`update-check` and `seo_update_check` require explicit consent before reading the bounded stable
GitHub release feed and saving the result. A consented automatic caller can reuse that result for
24 hours; SpiderHead does not start a scheduler. `update-download` and `seo_update_download`
download only an asset listed by the last checked release, verify its SHA-256 against release
metadata or `SHA256SUMS.txt`, enforce a byte limit, report progress and remove partial output on
cancellation. Downloads are saved to a new file and are never installed or executed.

## Offline bounded comparison

`compare-crawls --before before.sqlite --after after.sqlite --out-dir ./comparison`
creates a new `compare.v2` manifest and NDJSON collections for exact finding changes,
unchanged findings and per-check totals. Source identities, checksums and before/after
conservation remain explicit. Existing destinations are refused. Small calls without
`--out-dir` retain `compare.v1`; audit.v2 sources above 10,000 pages or issues require
an explicit output directory. This workflow reads retained evidence without network calls.
Add `--compression gzip` only when compressed NDJSON is wanted. Compression preserves
all rows; manifests declare compressed checksums/bytes and uncompressed bytes. The
default remains `none`; gzip requires an explicit output directory.
For audit.v2 inputs, the temporary comparison index stores exact keys and source
ordinals, then reads complete rows from the validated SQLite read snapshot. It does
not duplicate the audit payload; legacy document inputs retain their compatibility
fallback. Snapshot readers remain open until comparison finishes. Output members
are published with the manifest last, and failed publication removes only entries
owned by that attempt. This is a manifest commit boundary, not atomic visibility
of the whole directory.

## Offline BI projection

`bi-filter` selects declared columns and exact field values from a verified local BI
package. It writes a new bounded package, preserves source/selected/omitted counts
and coverage, and can write split XLSX output. No network or cloud call occurs.

`scan-navigation` reads paginated retained navigation observations, optionally for
one document. It never fetches a route or infers a link from unobserved script text.

`project-activity`, `project-checklist-page`, `project-task-detail`, and `project-scans`
expose bounded local observer reads for agents and the terminal. Search, pagination,
item definitions, evidence and history remain available without changing the project.

`bi-export` writes typed, bounded CSV partitions and a provenance/coverage manifest
from saved local scan/audit and supplied provider evidence. See [BI.md](BI.md) for
source/output limits, null states and the explicit audit.v2 compatibility bound.
It makes no provider requests or remote writes.

`bi-sheets-plan` and `bi-bigquery-plan` verify a complete local package and emit only an
offline preflight. They never authenticate, select a cloud target, activate billing, or write.

`publication-cohorts` and `gsc-progress` write separate offline cohort packages
from saved normalized evidence. They do not collect providers, infer missing
rows as zero, blend analytics with GSC, or make causal/rank-placement claims.
Their explicit input contracts and CSV/JSON manifests are in [BI.md](BI.md).

## Topvisor

`topvisor-read` / `seo_topvisor_read` reads one bounded page of existing projects,
competitors, keyword groups, keywords, position history or summaries. Pass an
`operation` and a `params` object through the ordinary JSON input. Credentials are
`~/.config/topvisor/access_token` and `~/.config/topvisor/user_id`, with environment
overrides `TOPVISOR_TOKEN` and `TOPVISOR_USER_ID`. No project-local credential copies
are needed. This tool never launches checks or modifies provider records.

Paginate explicitly using `limit` and `offset`; a single page is not a complete
inventory. The continuation signal is the provider's `nextOffset` key — present on
every non-final page, absent on the last — not `len(result) == limit`. `total` and
`limitedBy` pass through when Topvisor sends them, separately from the echoed
request `limit`/`offset`. `projects`, `competitors`, `groups` and `keywords`
return arrays; `history` and `summary` return objects — history rows live at
`result.keywords`, and `summary` covers the two requested dates rather than a
page of rows.

Field semantics worth keeping straight: a `position` inside
`result.keywords[N].positionsData[date:projectId:regionIndex]` is an ordinal
rank; Topvisor's `"--"` marker means the query had no position inside the
checked depth — an unavailable value, never rank 0 or 100 — and a requested
date with no `positionsData` entry is a missing observation, not zero
movement. `result.headers.dates` lists the dates actually included in a
history report; `existsDates` can name checks outside the requested interval.
`topsByDepth` is a percent of queries in Top N; `visitors`, `dynamics` and
`tops` are counts; `avgs` is an average rank. History needs `regions_indexes` —
the project region *index* from `projects` with `show_searchers_and_regions:2`,
not the geographic region `key`; `summary` takes the singular `region_index`.

## Project workspace

| Command | What it does | Network |
|---|---|---|
| `project-new` | Create a portable local project with site facts and custom template/profile references; does not execute a checklist | no |
| `project-open` | Validate and open a saved project without rewriting it | no |
| `project-status` | Show scan history and explicit pending checklist/preparation states | no |
| `project-sources-link` | Bind a provider resource ID (`--service`, `--resource`, optional `--label`) to a project in `sources.json`; no provider call or discovery | no |
| `project-sources-unlink` | Remove one `(service, resource)` binding from `sources.json` | no |
| `project-sources-list` | List the project's resource bindings with `status: selected` | no |
| `project-progress` | Show a compact, bounded page of checklist states and next actions; any percentage is explicitly labelled audit-task completion and requires an agreed scope | no |
| `project-facts` | Preview or record the project's stack facts; `--detect` fetches the target once after robots.txt, and an unavailable or ambiguous detection leaves the fact absent with its reason | only with `--detect` |
| `project-checklist-init` | Initialize or reconcile a local checklist from the built-in catalogue, an optional data-only template, and an optional agreed scope `plan` that fixes the URL-population and task denominators; does not execute items | no |
| `project-checklist-update` | Add or edit one checklist definition with an expected revision; does not execute it | no |
| `project-checklist-record` | Validate and record supplied evidence for one item with an expected revision; does not execute it | no |
| `project-view-list` / `project-view-show` | List saved finding views or retrieve one with stable identity and schema/config revisions | no |
| `project-view-save` | Create or revise a closed declarative finding view using an expected config revision | writes project view configuration |
| `findings-view` | Apply a saved view to audit JSON or a validated scan.v1 artifact; return a bounded stable page with explicit counts | no |
| `project-priorities` | Preview saved-fact work order; an explicit expected-revision apply preserves operator decisions and never changes technical severity | no |
| `project-policy` | Preview or explicitly save the bounded crawl/admission policy; applying it requires the current policy revision | no |
| `project-prepare` | Runs the declared bounded preparation path: checklist initialization, a policy-bounded crawl, supplied competitor workspace setup, and an inspectable initial plan | yes |
| `project-start` | Creates a new local project then enters the same bounded preparation path | yes |
| `project-competitors-add` | Records supplied competitor candidates as separate local workspaces, without crawling | yes |
| `project-competitors-list` | Reads the recorded competitor candidates and their workspaces | no |
| `project-competitors-remove` | Drops one recorded competitor from the shortlist; the workspace stays on disk | yes |

The nested aliases are `seohead project new`, `seohead project open`,
`seohead project status`, `seohead project facts`, `seohead project checklist-init`,
`seohead project checklist-update`, `seohead project checklist-record`,
`seohead project view-list`, `seohead project view-show`, `seohead project view-save`,
`seohead project priorities`, `seohead project policy`, `seohead project prepare`,
and `seohead project start`.
See [PROJECTS.md](PROJECTS.md) for the format, custom references and shared
CLI/MCP scan-routing rules.

`project-prepare` does not discover competitors or declare a completed audit on
its own. Competitors must be supplied and the preparation state keeps every
not-run or partial step. Use `project-policy` first when its default 50-page,
150-request, 60-second preparation crawl is not the intended scope; a larger
requested budget needs `approve_large_crawl=true`.

Saved finding views use closed severity/check/URL/segment filters, registered sort fields,
selected columns and a bounded page size. The same read-only view is available through
`findings-view`, `seo_findings_view`, and `report-build --project DIR --view NAME [--offset N]`.
Results expose source identity, view/config revisions, source/matched/returned counts,
missing-field counts, and `has_more`/`next_offset`. Missing filter fields stay unmatched and are
counted; missing projected values remain `null`. These views do not suppress findings, modify
evidence, or alter scoring, tasks or coverage. Segment selections reuse declarations in the audit;
missing definitions are explicitly unavailable. The view schema itself does not accept regexes.

## Guided workflow and catalogue tools

| Command | What it does | Network |
|---|---|---|
| `skill-list`, `skill-show` | List or retrieve one addressable packaged skill playbook; showing a playbook does not execute it | no |
| `scenario-show` | Retrieve one addressable packaged scenario without executing its steps | no |
| `inspect-url` | Runs a bounded selection of metadata, headers, robots, redirects, structured-data, and render checks for one URL; representations and indexing outcomes remain separate | yes when selected checks read the target |
| `audit-workflow` | Closed project action router: status, start, prepare, or report; it does not accept arbitrary handler names | depends on action |
| `tool-catalog` | Searches source-derived tool metadata with category, project-awareness, and offset paging; argument shapes are opt-in | no |
| `url-check-batch` | Runs up to 50 URL/check pairs from the supported single-URL checks; failures stay attached to their URL | yes |

MCP startup can use the `full`, `audit`, `infra`, `quick-check`, or `router`
profile. A restricted profile removes unavailable tool schemas at startup;
the catalogue may still describe the route needed for a lower-level operation.
See [MCP profiles and progress](MCP_PROFILES.md).

`tool-catalog` filters before paging, accepts `offset` and a `limit` from 1 to 50, and returns
the category and project-awareness flag with each result. Playbook list/show responses expose
front-matter description, version, and explicitly declared applicability/requirements; missing
fields appear in `metadata_unavailable` rather than being inferred from prose.

## How to read the tables

- **Network** — whether the tool goes out to the internet.
- **Side effects** — whether it writes to disk or makes requests the site
  does not expect.
- "—" in side effects means "read-only".

---

## Domain and infrastructure recon (`seohead/recon/`)

| Command | What it does | Network |
|---|---|---|
| `domain-profile` | Who owns the domain and where it lives: RDAP/whois, registration and expiry dates, DNS (A/AAAA/MX/NS/TXT), ASN and hoster, country, TLS certificate and its lifetime | yes |
| `cdn-check` | Is there a CDN and **does the cache actually work**: `cache-control`, `age`, `x-cache` headers, 20+ network signatures, a repeat request to confirm the MISS -> HIT transition | yes |
| `tech-detect` | The site's stack from HTML and headers: CMS, framework, server, analytics, chats, payment widgets. An external fingerprint database plugs in via an environment variable | yes |
| `security-check` | Security headers (HSTS, CSP, X-Frame-Options, Referrer-Policy, …) with a grade. With `--probe-paths` it also probes whether `.git/`, `.env` and friends are exposed | yes |
| `backlinks-check` | Verify a list of donor pages: is the link there, its anchor, `rel`, does it pass weight, is the donor indexable. Donors come as a list or a file | yes |
| `ai-bots-check` | Which AI crawlers robots.txt lets in: GPTBot, ClaudeBot, PerplexityBot, Google-Extended and a dozen and a half more, each with its role | yes |
| `mirror-check` | Mirror glue: scheme x www x index files x letter case x trailing slash; step-by-step redirect chains per variant, live 200-duplicates, http downgrades; www resolved via DoH | yes, read-only |
| `regions-check` | Regional structure: subdomains, folders, satellites. See the [regional-audit](../.claude/skills/regional-audit/SKILL.md) skill | yes |

**Recon boundaries.** Geo is resolved to country only — no invented cities.
No RDAP and no system `whois` -> `source: none`, not an empty answer passed
off as fact. Without the `h2` package the HTTP version is not measured and
the report says `http_version_measurable: false`.

---

## Live URL tools (`seohead/checks/`)

### Crawling basics and server responses

| Command | What it does | Side effects |
|---|---|---|
| `parse` | Page breakdown: title, description, headings, canonical, meta robots, OG, counters | — |
| `headers-check` | The full response header set with the SEO-relevant ones decoded; GET or HEAD only | — |
| `asset-weight-check` | Fetches a page's linked CSS/JS and reports render-blocking resources, oversized files, duplicate libraries (by content hash), missing minification, missing `font-display`, legacy polyfilled JS, and missing compression/long-lived caching. Unused-code and cross-page outlier checks need rendering/multiple pages and are reported under `skipped` | — |
| `redirects-check` | The live redirect chain to the final URL: hop count, codes, where the loop is; GET or HEAD only | — |
| `redirects-generate` | Ready redirect rules from URL pairs: Apache, Nginx, WordPress Redirection CSV, or a custom format; reports local chains and loops | writes to stdout |
| `links-check` | Broken links on a page; `--internal-only` — own links only | — |
| `robots-check` | Parse robots.txt and test specific paths for a specific user-agent | — |
| `sitemap-crawl` | Walk a sitemap (index/urlset, `.txt`, gzip), or discover it from a site root through `robots.txt` then `/sitemap.xml`; verify response codes | — |
| `soft404-check` | Soft 404: requests a deliberately non-existent URL; a 200 answer means the page lies to the robot about its status | requests a non-existent URL |
| `hreflang-check` | hreflang annotations: x-default, self-reference, duplicates, malformed language codes | — |

`robots-check` parses a successful robots.txt response. A 404 and every other 4xx response except
429 report no robots.txt and a permissive crawl policy; a 429 or 5xx response returns `ok: false`
because the rules could not be read, so the command never claims crawling is allowed.

### Content and markup

| Command | What it does |
|---|---|
| `schema-check` | JSON-LD, Microdata and common RDFa validation **in two layers**: the bundled vocabulary (`domainIncludes`/`rangeIncludes`, inheritance, deprecated terms and literal values) and local Google rich-result requirements; unsupported RDFa constructs are partial evidence |
| `schema-build` | Builds a suggested connected `@graph` with `@id`s for the page type; `--type` sets the type manually when the classifier is unsure |
| `social-meta-check` | Open Graph and Twitter Card: what is there, what is missing, what contradicts the content |
| `citability-check` | How quotable a text is for an AI answer: direct answers, facts, structure. Fetching a URL scores the resolved content area's Markdown (nav/footer excluded), not the raw whole-document text |
| `llms-txt-check` | Is there a `/llms.txt`, how useful it is to a model, is the brand mentioned |
| `duplicate-check` | Near-duplicates via simhash + LSH: finds almost-identical texts in a large set without comparing all pairs; exact duplicates (by content hash) are reported separately and excluded from near-duplicate clusters. `--all-pages` also compares non-indexable items (default: indexable only). `--scan` streams retained page bodies offline with explicit coverage: the corpus is capped at 10,000 analyzed documents and 16 MiB of retained input (extracted Markdown, not raw HTML), the analysis at 1,000,000 shingles and 250,000 candidate comparisons; reaching a corpus bound reports the exact partial coverage, an exhausted analysis budget is unavailable — never a clean result. |
| `markdown-extract` | Renders a page as Markdown in two scopes: `content_markdown` (boilerplate stripped, structure kept — worth diffing, scoring, or feeding to a model) and `full_markdown` (header/footer included, for reading — Markdown has already lost the tag structure `boilerplate-report` hashes, so it is not a valid input there) |
| `boilerplate-report` | Hashes header/nav/footer *markup* per page across a crawled corpus and reports minority template groups (fraction + sample URL), answering whether boilerplate is actually the same everywhere; each page needs the original `html` or a precomputed `hash`, never Markdown. `--scan` streams retained page HTML offline and keeps only each page's digest, so coverage does not depend on total HTML size; reaching the 10,000-document or 16 MiB retained-input bound reports the exact partial coverage — never a clean result. |
| `semantic-inputs` | Builds the reproducible normalized-input manifest semantic analysis consumes: per document the retained body hash, the exact decoded input hash, the normalized output hash, the content-area strategy, and language evidence (`<html lang>` plus letter-script shares over the normalized text) — the normalized text itself is never returned. `--scan` streams retained complete bodies offline under the crawl's recorded content-area config with no refetch; a missing or partial body stays an explicit omission, never a clean empty result. `--input '{"items":[{"url":...,"html":...}]}'` normalizes supplied markup under an optional `content_area`. Corpus bounds are the shared 10,000-document and 16 MiB retained-input caps. |
| `semantic-similarity` | Groups supplied embedding vectors into topical-similarity and internal-link review candidates over supplied HTML or a retained `scan.v1` corpus. It reuses `semantic-inputs` normalization and records model/version/settings, input coverage and bounded comparisons. SEOHEAD does not load, download or call a model: the calling agent supplies vectors and an explicit local/provider transfer declaration. Cache entries are local SQLite and keyed by source hash plus model configuration. Similarity is never presented as a duplicate, cannibalization conclusion, or automatic linking decision. |
| `meta-description-drafts` | Prepares a bounded dry-run or validates structured drafts supplied by a calling/delegated agent. It reads only supplied HTML or retained scan content, stores local checkpoints by source/context/executor identity, reports unavailable inputs and review flags, and optionally exports local JSON plus formula-safe CSV. It never calls a model, writes a CMS or promises a search snippet. |
| `keywords-cluster` | Keyword clustering; the algorithm and parameters come via `--input` |
| `render-check` | Raw HTML vs the rendered DOM + lab metrics. See the [js-render-check](../.claude/skills/js-render-check/SKILL.md) skill |

### Native asset budgets and lab findings

`RENDER_BLOCKING` reads complete retained static HTML, with a 2 MiB per-document analysis
bound. It uses the existing head-markup helper and preserves the frozen `scan.v1` schema.
Missing, incomplete and oversized retained bodies stay unavailable. This is markup evidence,
not a measured blocking duration.

For declared-resource budgets and delivery findings, enable `storage.format_version=scan.v2`
and `resources.fetch=true`. The existing resource request, origin, byte, nesting and time
budgets still apply. `HEAVY_PAGE_WEIGHT` reads actual responses delivered by a requested native
browser navigation, with document, image, font and third-party buckets. Third-party here means
a hostname different from the initial navigation target, including subdomains. It observes the existing
pinned transport and adds no requests. Declared responsive alternatives and unused CSS resources
are not summed as runtime bytes. A raw-only page can exceed the budget on its HTML alone; remaining
runtime assets stay unavailable. Partial navigation bytes remain a lower bound within the recorded
window. Browser weight uses content-decoded body bytes, not compressed transfer size; the
HTML-only fallback labels its own captured-byte unit. `RESOURCE_OVER_2MB` identifies
individual resources over the configured limit. The default is 2 MiB for both checks.
Native overrides are `analysis.asset_thresholds.page_weight_max_bytes` and
`analysis.asset_thresholds.resource_max_bytes`; SF config uses the same names under `thresholds`.

Completed resource responses also feed `ASSET_SHORT_CACHE`, `FONT_DISPLAY_MISSING`,
`UNMINIFIED_CSS_JS`, `LEGACY_JS` and `DUPLICATED_JS`. Minification and legacy markers are
heuristics. Duplicated scripts require identical captured bytes at different URLs on the same
page; whitespace inside string literals is preserved. A script URL answering with HTML is
unavailable source evidence. HTTP 206 fragments remain partial even in older retained graphs.
Font-face samples fit the whole 16 KiB ASCII-escaped delivery envelope and retain omitted counts.
Shared-cache `s-max-age` does not substitute for browser `max-age`.

Requested native renders collect bounded Chromium JS/CSS precise coverage before navigation.
`UNUSED_CSS_JS` reports source unused during that recorded window in UTF-16 code units, not
transfer bytes. Workers, out-of-process frames and later interactions are outside its scope.
Other engines or failed CDP collection remain unavailable. Sources are limited to 60 per kind,
2 million code units each and 8 million combined. Precise coverage can affect lab timings.
Rendered layout feeds `IMG_INTRINSIC_OVERSIZED` and `IMG_UPSCALED`, with physical file/displayed
width, DPR, source-page and response evidence. File dimensions account for EXIF orientation;
browser density-corrected `naturalWidth` is not used as file pixels. The supported geometry is a
plain image content rectangle: unsupported object-fit, padding, border or transforms remain
unavailable. The existing oversized-image floor is 10 KiB. Missing image sizes, unknown encoded
sizes and observation/result limits remain partial or unavailable.
Layout/coverage summaries survive retained-scan reopening and offline reanalysis without refetch.
`summary.asset_measurements` records eligible, measured, partial and unavailable counts for every
asset check. A valid finding can coexist with explicitly partial coverage; missing sibling pages,
resources or reports do not become clean results. Raw CDP responses materialize before local
source/range acceptance limits; these limits do not claim an absolute browser memory ceiling.

LCP discovery, dependency trees, layout-shift culprits, image delivery, forced reflow and JS
execution time can consume already collected per-URL Lighthouse JSON in SF audit config:

```json
{"input":{"lighthouse_reports":{"https://example.test/page":"./lighthouse/page.json"}}}
```

Run `seohead sf run --exports-dir ./exports --config ./audit-config.json --out ./audit`.
Each report must match a crawled URL by `finalUrl`; runtime errors, missing audits and null scores
stay unavailable. Imports are bounded to 2,000 reports and 8 MiB per report. Findings retain
input hashes, Lighthouse version, capture time and bounded details, without local paths. These
are supplied lab results; the toolkit does not launch Lighthouse or infer field Core Web Vitals.

### Logs and images

| Command | What it does | Side effects |
|---|---|---|
| `log-analyze` | Offline file analysis with exact disk-backed IP totals and bounded per-IP records; explicit `--verify-bots` checks only the selected page | reads a file; opt-in DNS |
| `logs-import` | Stream files/gzip or a bounded directory into a fresh local project log artifact with date partitions and provenance | creates a local artifact |
| `logs-summary` | Exact paged URL/IP/bot/date/status/section aggregates with explicit filters | offline, read-only |
| `logs-join` | Compare retained bot visits with a validated native scan; incomplete evidence withholds absence claims | offline, read-only |
| `logs-bots` | Read per-IP PTR, forward match, verdict, visits and last visit; explicit verification updates a one-hour local cache | opt-in DNS and local cache writes |
| `log-scan` | Reads a finished run's own artifacts (`audit.json`, `pages.jsonl`, an `images-download` directory) and reports claims that cannot all be true at once: a recorded size that disagrees with the file, a check firing more often than there are pages to fire on, a finding about a URL the run never fetched, a summary that disagrees with its own rows. Not a second audit — only contradictions, each naming both values | reads files |
| `images-download` | Download a list of images into a directory | writes files |
| `images-optimize` | Recompress, resize, or convert raster images and conservatively minify SVG; `--quality auto` uses bounded search and reports measured SSIM plus whether the target was met over an aligned 1024 px RGB thumbnail (0.98 by default); EXIF can be removed explicitly; `--scan` downloads and optimizes up to 50 distinct declared images with source-occurrence and before/after evidence under a 100 MiB total download cap | requires `--output-dir`; scan mode writes only to that directory; local-file source mutation needs explicit `--in-place` and backs up by default |

---

## Bounded site audit and reports (`seohead/audit/`, `seohead/reports/`)

| Command | What it does |
|---|---|
| `site-audit` | Runs a bounded live pass: 10 site-level tools once and 3 page-level tools per selected URL (from the sitemap by default; 25 pages by default). Returns one `seohead.site-audit/1` document. It is not a full crawl or an exhaustive run of the catalog; site-level failures remain in `summary.tools_failed`, while page-level failures remain in that page's issues |
| `report-build` | Document -> file: `xlsx`, `docx`, `csv`, `md`, `json`; XLSX splits large Findings/Pages sheets with a hashed index; optional `--project --view` applies a saved finding view to a bounded report page while preserving source-wide totals |
| `scan-reanalyze` | Reparse retained HTML/DOM and run existing checks offline into a new SQLite artifact, preserving source evidence and provenance |
| `facts-export` | Zero-network comparison: reads crawl/site audits you already produced for several domains and returns one `facts.v1` document — measured/absent/partial/unavailable/not_requested facts per site, never a score, rank, or ratio |

```bash
seohead site-audit --url https://example.com --limit 50 --report xlsx --out audit.xlsx
seohead report-build --audit audit.json --format docx --out client.docx
seohead report-build --audit audit.json --format docx --out client.docx --project ./example-project
seohead report-build --audit audit.json --format pdf --lang ru --out client.pdf
seohead facts-export --input '{"sites": [{"label": "site-a.test", "crawl_audit": {"schema_version": "2.0", "run": {"source": "https://site-a.test/"}, "summary": {"totals": {"urls_crawled": 10}}, "issues": [], "pages": [], "groups": []}}]}'
```

The document contract and skeletons to fill in — [`docs/examples/reports/`](examples/reports/README.md).
PDF output uses the self-contained bilingual layout and local Chromium renderer; install
`seohead-seotools[pdf]` and Chrome, Edge, or Chromium. Rendering makes no network requests.

The finding level (`critical`/`warning`/`notice`) is assigned by **aggregator
rules**, not measured by a tool; the document says so itself in
`summary.severity_note`. The rules table is `SEVERITY_RULES` in
`seohead/audit/site.py`.

---

## Crawl your own site (`seohead/crawl/`)

No Screaming Frog licence required: the toolkit drives its own spider and audits the
result through the same check registry as a Screaming Frog export. See
[SETUP.md](SETUP.md#crawling-without-a-screaming-frog-licence) for the module
details (adaptive back-off, which checks come back `skipped` and why) and
[SETUP.md](SETUP.md#comparing-two-crawls) for the four-way diff `compare-crawls` makes.

| Command | What it does | Side effects |
|---|---|---|
| `crawl-site` | Follows links from a start URL on the same host, respects `robots.txt`, and audits the result. `scope.include_extensions` / `scope.exclude_extensions` filter discovered URL-path suffixes before requests; `scope.include_media_types` / `scope.exclude_media_types` filter response bodies after `Content-Type` arrives while retaining response and link evidence. A URL crawl writes one collision-safe native SQLite artifact under `./scans/` by default; `--out-dir` is the explicit legacy directory route. Not full Screaming Frog parity — checks needing evidence a native crawl cannot produce (redirect chains, near-duplicates, readability, ...) come back `skipped`, never a false clean | writes a native scan, or legacy files under explicit `--out-dir` |
| `crawl-diagnose` | Explains low progress from a retained native SQLite scan (`--scan`) or legacy run (`--run`): frontier, decisions, robots/scope/depth/budgets, content types, rendering eligibility and recorded failures. Shows exact bounded decision samples and explicit next steps; the total number of site URLs stays unknown. No crawl or network request. | reads files |
| `crawl-diagnose-export` | Explicitly writes a redacted JSON copy of the same offline diagnosis with `--export PATH`; unknown freeform labels and scan identity are removed, and existing files are never overwritten. | creates one file |
| `compare-crawls` | Diffs two audit documents into `entered` / `left` / `appeared` / `disappeared` findings, so a fix is distinguished from a page that simply dropped out of the crawl. An optional closed `url-correspondence.v1` declaration pairs a release migration only by declared origin/path pairs and returns a `release_review.v1` artifact with facts and finding deltas; no title/content inference occurs. Refuses known-different effective crawl settings unless the operator explicitly passes `--force`. | — |
| `verify-fixes` | Rechecks selected baseline finding IDs, URLs, or a saved `verification_view.v1` selection. Reuses recorded HTTP/robots/render policy and classifies resolved, persisting, changed, and not-verifiable findings from the affected pages only; whole-site and unmeasured checks cannot become fixed. `--after` uses an existing audit offline. | fetches selected URLs unless `--after` is given; creates a new immutable JSON/Markdown verification under `--out-dir` |
| `crawl-enrich` | Joins an existing audit or scan to a local URL-keyed traffic/search CSV. It keeps matched, crawl-only, external-only, and unkeyable rows distinct; a completed crawl can export reliable same-origin external-only URLs for list mode. | optionally writes a URL-list file under `--out-urls` |
| `crawl-import` | Reads a local manifest-mapped CSV crawl bundle and returns `third_party_crawl.v1` with foreign source identity, pages/links/statuses/redirects, exact field coverage, duplicate counts and input hashes. This is not a native scan or SF audit. | reads the manifest and listed CSV files |
| `segment-diff` | Answers "which pages exist in one segment and not in another" from one crawl, using the site's own hreflang declarations as the authority. Mirrored paths are a fallback only where the site's declared pairs prove it mirrors them; a partially crawled target segment yields no absences at all, because a page nobody fetched is not a page that is missing. Reads a native crawl whose config declared `scope.segments`, not an SF export | — |
| `crawl-describe-settings` | Lists every `crawl-site` config setting — dotted path, type, default, description, and whether it is results-affecting — generated from `seohead/crawl/settings.py`. Same source as `crawl-site --config-help`, reachable over MCP for an agent with no filesystem access | — |

Native `crawl-site` can use an explicit `http.proxy` policy from JSON config, CLI `--set`, or
MCP overrides. It supports one HTTP forward proxy for HTTP and HTTPS CONNECT, with credentials
only through an `env:VARIABLE` URL reference. Proxy and target addresses are separately vetted;
ambient proxy variables are ignored. Proxied runs require cache off and a fresh output artifact.
See [SETUP.md](SETUP.md#crawler-configuration) for the supported transport and limits.

`rendering.mode=raw` remains static-only. When a fuller representation is
enabled, `rendering.escalation.policy=sampled` is the default: it uses the
configured per-pattern sample before deciding which evidence is worth fuller
collection. `full` requests the fuller policy deliberately. `auto` (the desktop Auto mode)
is sampled plus escalation of any pattern whose raw page carries little text
(under 50 words), even when raw and rendered agree. All remain bounded
by render URL/time settings, and a route or corpus relation stays unknown when
one representation was not completely captured.

The browser transport defaults to a local sandboxed Chromium launch. An operator
can instead connect to an already running Playwright browser server by setting
`rendering.browser.transport=remote`, `remote_endpoint_env` to the **name** of an
environment variable containing its WebSocket endpoint, and
`remote_playwright_version` to the server's declared version. The Python client
and server must share a Playwright major/minor version; the Playwright connection
also performs its protocol handshake. Only the Playwright protocol is supported;
CDP is refused. A remote connection error never starts a local browser. The tool
neither launches nor provisions a remote service. The endpoint value, including
any query token, is never recorded in a scan or returned in an error.

Remote browser requests still pass through the existing pinned HTTP route;
page WebSockets are blocked and service workers disabled. An unsupported route
capability fails before a page opens. Use `wss://` for a non-loopback endpoint;
plain `ws://` is accepted only on loopback. The operator is responsible for
trusting and securing the server because rendered page data travels over this
connection. Closing the connected Browser releases its contexts and disconnects
the client; it does not stop the operator's browser server. See the
[Playwright BrowserType.connect contract](https://playwright.dev/python/docs/api/class-browsertype#connect).

```bash
# The value stays in the environment, not in CLI arguments or saved crawl config.
export SEOHEAD_REMOTE_BROWSER_WS='wss://browser.example.test/playwright'
seohead render-check --url https://example.test/ --browser-transport remote \
  --remote-endpoint-env SEOHEAD_REMOTE_BROWSER_WS --remote-playwright-version 1.55.0
# For crawl-site, set the same fields under rendering.browser in its JSON config.
```

Render elapsed time is saved in the scan context across resume cycles. If a
previous process died while an active finite render phase was running, the next
cycle conservatively treats that budget as exhausted instead of resetting it and
claiming the original time bound still applies.

```bash
seohead crawl-site --url https://example.com/ --max-urls 200
seohead crawl-diagnose --scan ./scans/audit.sqlite
seohead crawl-diagnose-export --run ./run --max-decisions 10 --export ./diagnostic-redacted.json
seohead compare-crawls --before old-audit.json --after new-audit.json
seohead verify-fixes --baseline old-audit.json --finding-ids ISSUE-000001 --out-dir ./verification-1
seohead crawl-import --manifest third_party_crawl/full/manifest.json
seohead segment-diff --audit ./multilingual/audit.json --source en --target pl
seohead crawl-describe-settings
```

The supported manifest, field, coverage, normalization, and size-limit contract
is documented in [THIRD_PARTY_CRAWL_IMPORT.md](THIRD_PARTY_CRAWL_IMPORT.md).

---

## Saved scan history (`seohead/storage/`)

These local operations work only on validated `scan.v1` SQLite artifacts. They
do not create a background catalog, migrate an older schema, or delete a body
without deleting its scan. The exact arguments and defaults are in the generated
[TOOL_REFERENCE.md](TOOL_REFERENCE.md).

| Command | What it does | Side effects |
|---|---|---|
| `scan-list` | Validates and lists metadata for `*.sqlite` files in one existing directory without reading retained body BLOBs. It stops at 10,000 files and 64 MiB of metadata, and reports unreadable candidates under `errors` rather than treating them as scans. | — |
| `scan-inspect` | Reads one allowed table (`pages`, `links`, `forms`, `decisions`, `frontier`, `query_variants`, `context_items`, `responses`, `documents`, `resource_refs`, or `audit`) as a paginated view. At most 1,000 rows and 8 MiB of row payload are returned; `has_more`/`truncated` says when the caller must narrow or continue. | — |
| `scan-url-detail` | Reads one exact native URL's bounded retained page, redacted request/response headers, redirect chain and forms. Query values and sensitive headers are redacted; HTML body bytes are not returned. Legacy and Screaming Frog sources name this evidence as unavailable. | — |
| `scan-url-query` | Server-side filter, sort and pagination over the whole `pages` table of one saved scan (up to 200 rows per call, projected columns, AND-combined filters from a fixed column and operator allow-list). Returns `total`, `filtered_total` (`null` with `filtered_total_state: capped` when the count exceeds its time budget), scan coverage labels and the `seohead.scan-url-query.v1` format. Sorting by `url`, `status_code`, `page_ordinal` or `url_id` uses indexes; any other column needs a filter leaving at most 100,000 rows, else `reason_code: sort_not_indexed`. Failures carry a machine-readable `reason_code`. | — |
| `scan-link-inspect` | Reads an observed shortest path, cursor-paginated reverse inlinks, one retained document's per-link placement/heading context, or (`--view links`) the paged outgoing/incoming links of one URL. It returns scan identity and explicit partial/unavailable evidence; traversal, body and result sizes are bounded. | — |
| `scan-status` | Separates queued, inflight, done, and excluded native frontier rows from committed page HTTP outcome classes and no-response records. It reports interrupted captures as unfinished; imported scans name their absent native frontier as unavailable rather than an empty queue. The scan is accepted by a light header/schema check and the response carries `validation: "light"`; `full_validation: true` (or earlier full validation of the same bytes) reports `"full"`. | — |
| `scan-rendered-routes` | Reads stored eligible static/rendered `a[href]` route evidence offline. It never queues or fetches a route; relation is `unknown` until both representation coverages are complete. | — |
| `scan-snapshot` | Makes a validated, portable single-file SQLite copy. `--out` may name a new file or an existing directory; a directory receives a UTC timestamp, host, and short scan UUID filename. Existing destinations are never overwritten. | writes a new file |
| `scan-export` | Exports retained scan data under the versioned `scan_export.v1` contract as CSV, XLSX, JSON, or XML. Accepts a `scan.v1` artifact or an SF Analyzer `audit.json`; validates `--records`/`--fields` before writing; XLSX splits oversized record types into numbered sheets and records every partition; XML uses the documented `scan-export` root element and namespace. | writes new files |
| `scan-pin` | Explicitly pins a scan, or unpins it with `--unpin`, so retention will not select it. | changes scan metadata |
| `scan-prune` | Produces a retention plan by default. Deletion needs `--apply` and the exact reviewed plan. | deletes only with `--apply` |
| `scan-body-diff` | Compares matching retained body hashes from two validated scans; optional text output is bounded and only applies to compatible textual evidence. A changed body is not an SEO score or verdict. | — |
| `scan-evidence` | Reads one bounded saved-evidence section: capabilities, corpus, structured data, rendered routes, resources, or timeline. It never fetches or replays a scan. | — |
| `scan-extract` | Applies closed declarative extraction rules to retained complete bodies only. It is offline, body-retention limited, and does not persist the ad-hoc result. | — |
| `scan-content-search` | Searches all retained complete textual documents offline for a literal string (or a Python regular expression with `kind="regex"`; a document over the regex time budget is unavailable, never absent), including HEAD, raw HTML, body text, or CSS-selected markup. Static/rendered coverage is explicit: unavailable bodies stay unknown, never absent. GTM/GA4/analytics tags are useful audit inputs; a tag's presence does not prove it fired. | writes a new indexed local result package |
| `scan-content-search-page` | Reads at most 100 rows of that derived package using validated offsets and integrity checks, with unchanged global coverage counters. This operation never reopens or fetches the site. | — |
| `marketing-inventory` | Correlates CTA and form/iframe fields per supplied DOM occurrence. It never fetches, submits forms, or inspects iframe contents; an explicit new output directory writes local JSON and formula-safe CSV. | optional local artifact write |
| `scan-fragment-links` | Evaluates every fragment-bearing `a[href]` in retained complete HTML/DOM and reports whether each `#fragment` identifies a target in the retained destination document (WHATWG scroll-to-the-fragment matching: serialized fragment against ids and `<a name>` first, then the percent/UTF-8-decoded value against both, then `top`). Static and rendered representations are measured independently; missing, truncated, unsupported or budget-exhausted bodies stay named skips, never broken findings. It never fetches a destination. | — |
| `scan-requeue` | Requeues a restricted saved URL/page selection only after creating a mandatory verified backup. | writes artifact and backup |
| `scan-import-urls` | Imports an explicit local URL list into a saved scan only after creating a mandatory verified backup. | writes artifact and backup |

For a saved route, use `scan-link-inspect --scan FILE --view path --seed URL --target URL`.
For the links of one page by URL use `--view links --url URL --direction out|in`
(`--offset`, `--limit` up to 200; filters `--link-type internal|external`,
`--follow follow|nofollow`, `--status-class 2xx|3xx|4xx|5xx|broken|error|unscanned`,
`--contains`, and for `out` `--sort order|url|status`). Each row has the anchor, `rel`
with `nofollow`/`sponsored`/`ugc`, internal/external, the stored position (null when
the scan did not classify it), the `target` attribute and the HTTP status of the other end
(`null` with `target_scanned: false` when that page was not scanned, never 0). `total` and
`filtered_total` are exact or flagged capped (`total_capped`, `filtered_total_state`); the scan
is opened read-only without full validation. Failures carry a `reason_code`: `url_not_found`,
`scan_not_available`, `url_not_scanned`, `links_not_retained` (links were not stored, which is
not zero links). `hreflang` and `download` are not retained in `scan.v1`.
For reverse links by cursor use `--view inlinks --target URL`, then pass `next_cursor`
back as `--cursor`; the cursor is tied to the scan revision, target and
representation. For a particular occurrence use `--view context --link-id ID`,
or paginate one source document with `--document-id ID --offset N --limit N`.
Missing body/DOM context and incomplete graph coverage remain explicit in the
JSON result.

```bash
# metadata-only directory view; no retained body BLOBs are read
seohead scan-list --directory . --limit 100

# inspect a whitelisted table with a smaller total row-payload budget
seohead scan-inspect --scan native.sqlite --table documents --limit 100 --max-bytes 1048576

# inspect operational state without retrying, requeueing, or reading retained bodies
seohead scan-status --scan native.sqlite

# no-clobber snapshot: either a new filename or an existing directory
seohead scan-snapshot --scan native.sqlite --out snapshot.sqlite
seohead scan-snapshot --scan native.sqlite --out .

# versioned scan_export.v1 data export; field selection is validated upfront
seohead scan-export --scan native.sqlite --out export.json --format json
seohead scan-export --scan native.sqlite --out export.xml --format xml
seohead scan-export --scan audit.json --out export.csv --format csv --records pages,findings --fields pages=url,status_code

# pin before retaining a comparison baseline; use --unpin to reverse only the pin
seohead scan-pin --scan native.sqlite
seohead scan-pin --scan native.sqlite --unpin

# inspect the JSON preview, then retain its stdout envelope for explicit review
seohead scan-prune --directory . > plan.json

# hash-first comparison; text materialization is explicit and bounded
seohead scan-body-diff --left before.sqlite --right after.sqlite --url https://example.com/ --text --max-bytes 5242880 --max-lines 10000

# read retained evidence or make an ad-hoc offline extraction; neither fetches a URL
seohead scan evidence --scan native.sqlite --section structured
seohead scan extract --scan native.sqlite --input '{"rules":[{"id":"product-name","kind":"text","selector":"h1","operator":"matches","value":"Product *","max_matches":1}]}'
```

The default retention plan selects only scans that are finished, unpinned,
complete in both crawl and corpus state, older than 30 days, outside the five
newest scans for the same host and configuration, and free of a live writer
lock. Before any unlink it revalidates every reviewed candidate and recomputes
the current retention rank. `crawl_partial` and `corpus_partial` scans are
always protected by automatic selection. The preview's stdout envelope is an
accepted `--plan` JSON file; a changed directory, identity, metadata, or rank
invalidates it. After reviewing `plan.json`, run `seohead scan-prune --directory .
--plan plan.json --apply` to perform that exact deletion plan. The flat commands
also have the nested `scan list`, `scan inspect`, `scan status`, `scan snapshot`,
`scan pin`, `scan prune`, `scan body-diff`, and `scan link-inspect` forms.

Pinning acquires the writer lock and writes the artifact in SQLite DELETE journal
mode. It changes only the `pinned` field, so the SQLite container hash changes,
while the audit, body records, and evidence revision stay intact.

---

## External data sources (`seohead/data_sources/`)

Technical checks describe what a site exposes; **demand** and traffic evidence lives behind
external APIs. This layer keeps one client per provider with common credential, retry, quota,
and spend-journal rules.

### Provider evidence boundary

| Command | What it does | Network / writes |
|---|---|---|
| `provider-registry` | Lists declared providers and their bounded operations; it does not verify credentials | no |
| `provider-readiness` | Reports redacted credential-source states, supported operation routes and their shared JSON call-envelope schema, quota/privacy, and whether verification remains necessary. It makes no provider requests | no |
| `provider-auth` | Manages a private GSC read-only OAuth grant: status, connect from a private grant file, explicit refresh, cancel of a pending browser flow record (local only), confirmed local disconnect, or confirmed remote revoke. It never returns OAuth material. | refresh/revoke only when requested |
| `provider-verify` | Performs one explicit read-only credential and optional target-access check. An authenticated account does not by itself prove access to a requested target. | provider read |
| `provider-collect` | Performs one declared read-only operation and returns a versioned evidence envelope with complete, partial, failed, or skipped state. An optional restricted artifact directory keeps raw rows locally. | provider read; optional local artifact |
| `provider-join` | Joins supplied crawl pages and collected evidence rows without changing a frontier. It preserves matched, crawl-only, external-only, and unkeyable populations. | no |
| `provider-replay` | Replays a private saved provider collection against a saved scan offline, writes a restricted joined artifact, and keeps the crawl frontier unchanged. | local artifact write |
| `evidence-normalize` | Normalizes a supplied CSV/XLSX/JSON or a saved provider envelope into `seohead.normalized-evidence.v1` against an optional `seohead.evidence-mapping.v1` manifest, offline. Every row keeps its grain, provenance and availability state: a measured `0` stays zero while missing, null, blank, suppressed, uncollected and failed inputs stay unavailable. Restricted sources return counts and redacted provenance. | optional restricted artifact under `--out-dir` |
| `evidence-join` | Joins normalized evidence to `--pages`, `--scan`, or `--audit` under the strict URL policy (explicit `--ignore-query`/`--ignore-scheme`/`--casefold-path` relaxations), preserving matched, crawl-only, external-only and unkeyable populations plus collision counts. `--compare` with a declared `--policy` yields a pure compatible/incompatible/unknown decision across period, timezone, identity, attribution, engine and grain; source metrics such as GSC clicks and GA4 sessions stay distinct and are never summed. | optional private artifact under `--out-dir` |

`sources-doctor` and `provider-readiness` are local setup checks. They distinguish
missing, invalid and configured-but-unverified credential sources; references use
`env:VARIABLE`, `config:relative/path`, or a named local grant, never the value or
an absolute path. Operation discovery labels a route as `provider-collect`, a
dedicated command, a separate write, or unsupported. Supported operations expose
the shared `provider`/`operation`/`request` JSON envelope; provider-specific request
fields remain validated by that operation adapter. Readiness always reports target
access as unverified; `provider-verify` is the separate, explicit read that can
return `verified`, `not_granted`, `insufficient_scope`, or `unsupported`. Supplying
credentials alone never proves access to a property or site.

Provider evidence can change work order only when its coverage is usable; sampled,
truncated, unmatched, or privacy-thresholded values remain unavailable for a
priority adjustment. It never changes a technical finding's severity. See the
[provider-evidence scenario](scenarios/provider-evidence.md).

| Command | What it does | Money |
|---|---|---|
| `keywords-expand` | Expand a phrase via Yandex Wordstat: refinements (left column) + similar queries (right column) with base frequency | RUB 20/1000 GetTop requests at the first snapshot; quota **100/hour**; verify current tariff |
| `keywords-seasonality` | Demand over months/weeks/days — tell a dead query from a seasonal one | same |
| `keywords-exact` | Exact `!W` frequency (Arsenkin's `overal`/`!WS` selector) — what the Wordstat API, base-only, will not give you | Arsenkin account limits; the charge and `task_id` are journaled at task creation |
| `serp-fetch` | Yandex SERP for a query or a batch. Async only | metered; synchronous search is intentionally absent; verify current tariff |
| `spend-report` | What was actually charged: by source, operation and day, from the local journal | free |
| `sources-doctor` | Which sources have usable credentials, with redacted source references; configured credentials are not verified access | free |
| `sources-sync` | Fetch bounded missing or explicitly forced provider days into a local versioned SQLite history. Records complete, empty, partial, failed, and lagged days without storing tokens | Provider read subject to GSC, GA4, Metrika, or Webmaster quotas; writes local SQLite |
| `sources-status` | Read requested-day coverage, resource identity, lag policy, and non-additive metric metadata from local history | Offline read only |
| `sources-export` | Read ordered provider rows as bounded JSON or a new private CSV; does not recompute totals across grains | Offline; optional local CSV write |
| `regions-tree` | The authoritative Yandex region tree via `getRegionsTree` | **free** — the only free Wordstat method |
| `metrika-counters` | Metrika counters visible to the token — this is where `counter_id` comes from | free |
| `metrika-setup` | How a counter is configured: goals, filters, data operations | free |
| `metrika-report` | What visitors actually did: any metrics and dimensions, auto-pagination; a `Query is too complicated` refusal is retried in month slices and, if needed, at sampled accuracy — the answer states what was used. Only additive count metrics (`ym:s:visits`, `ym:s:pageviews`) merge; unique-visitor, ratio, or average metrics fail rather than sum wrong | free |
| `metrika-traffic-pdf` | Static A4 traffic report (HTML + PDF) in a dashboard layout: KPI cards with % change, daily dynamics, 3/6/12-month windows, engines, cities, countries, devices, age, gender, landing pages, phrases, channels, optional Search Console queries. Collection and rendering run separately | free; about 40 read-only Metrika requests; writes only `out_dir`; PDF needs a local Chrome/Edge/Chromium |
| `google-keywords` | Google: search volume for a keyword list, semantic expansion from a seed phrase, keyword difficulty | DataForSEO price list; RUB 0 in the sandbox |
| `google-serp` | Google organic results for a query | same |
| `wayback-history` | Every Internet Archive snapshot of a URL: when it changed, what status it returned, what MIME type it was | free, no key |
| `crtsh-subdomains` | Hosts named in public TLS certificates for a domain — subdomains nothing links to | free, no key |
| `cloudflare-traffic` | Bots x URLs x status codes x cache status per day for a Cloudflare zone, from edge GraphQL analytics, shaped like `log-analyze`. Aggregated (`source: cloudflare-aggregated`), not raw logs: no IPs, no rDNS bot verification; Free plan keeps 32 days | free; needs a read-only Cloudflare API token (`CLOUDFLARE_API_TOKEN` or `SEOHEAD_CLOUDFLARE_TOKEN_FILE`) |
| `gsc-query` | Search Console: clicks, impressions, position and CTR per query or page, plus Google's own indexing verdict for one URL | free; needs OAuth against a property you own |
| `webmaster-url-queries` | Yandex Webmaster query evidence for one URL or a bounded URL population; URL/query rows stay separate and caps are explicit | free within Webmaster quota; needs an own verified host |
| `miratext-analyze` | Start or resume bounded competitor text analysis; paid and keyword modes require explicit confirmation | paid provider; API key required |
| `crux-report` | CrUX current-window field LCP/INP/CLS p75 with official threshold findings, URL/origin and form-factor scope, collection dates; optional bounded URL sample/cache | free within Google API quota; needs a Google Cloud API key |
| `indexnow-submit` | Push changed URLs to Bing, Yandex, Naver and Seznam. **Google has not joined IndexNow** | free; needs a self-generated key hosted on the site |
| `gsc-archive` | Explicit local SQLite archive: offline `status`, `prepare` a property/date queue, bounded resumable `run`, or verified `backup`. Only prepare creates a database. Different grains are independent; never sum them. | only run calls Google; free API with quotas and configured GSC credentials |

The Yandex query-analytics API provides its own rolling retention window. This route preserves
the provider-returned daily buckets and does not advertise a caller-selected date range.

`miratext-analyze` returns a resumable hash while the provider queues work. An accepted result
contains bounded `author_tables.words` and `author_tables.density_deviation` JSON arrays for
direct CLI/MCP export or joining into a local report. Values, filters, and stopword handling keep
the provider's stated units; an unknown or malformed final table is marked unavailable.

`gsc-archive --database ./analytics/search-console.sqlite --action prepare --site-url sc-domain:example.test --start-date 2025-06-01 --end-date 2026-09-01`
creates the archive and queues availability checks without network calls. Dates are inclusive in
Pacific Time; select a range inside Google's available history (up to approximately 16 months).
For a new file, it also initializes the shared versioned provider-history store used by
`sources-sync`, `sources-status`, and `sources-export`; richer archive datasets remain separate
grains in that same SQLite file.
Then run `gsc-archive --database ./analytics/search-console.sqlite --action run --max-requests 10 --pause 1`.
`--action status` reads the existing archive without credential access or creating an absent file.
`--action backup --backup-path ./backups/search-console-snapshot.sqlite` creates a verified snapshot
and refuses an existing destination. JSON-only input and `seo_gsc_archive` use the same arguments.

Archive datasets preserve provider, engine, request provenance and nullable metrics. Availability
expands into separate daily totals, pages, queries, detail and appearance datasets where supported.
Google may omit anonymized queries or cap returned rows: a `capped` job is not exhaustive evidence.
Quota waits cover all GSC properties in that archive (15 minutes; daily quota: 24 hours); transient
transport/5xx failures get at most three attempts per page. Across multiple archives, the caller must
coordinate provider backoff. Neither prepare nor status launches collection implicitly.

Provider history uses one `sources.sqlite` per project (`--project`) or an explicit `--db`.
`sources-sync` fetches missing days, records every requested day, and replaces each successfully
fetched day in one SQLite transaction. Complete zero-row days differ from sampled, thresholded,
truncated or capped partial results, failures and days held back by provider lag. A failed or
partial forced retry never erases a previously complete day. GSC history is the bounded web
date/query/page grain; the separate archive adds independent datasets and resumable checkpoints.

`sources-status` reads the local file without provider calls or schema writes. `sources-export`
orders by resource, date and dimensions and limits both JSON and CSV to at most 100,000 rows.
Exports retain nulls for absent metrics and identify non-additive measures such as users, rates,
CTR and average position. They do not calculate totals or blend incompatible grains. GSC dates
use Pacific time; other property/counter timezones are not known locally, so their lag cutoff
conservatively holds back an extra UTC day. SQLite files and CSV exports are created with private
owner-only permissions. These rows can contain search queries and URLs; keep them out of public
reports and Git.

```bash
seohead sources-doctor                                     # what is ready to run
seohead provider-readiness --input '{"provider":"gsc","operation":"search_analytics"}'
seohead provider-verify --input '{"provider":"gsc","request":{"site_url":"sc-domain:example.com"}}'
seohead keywords-expand --phrase "underfloor heating" --limit 100
seohead keywords-exact --keywords "underfloor heating,floor screed" --region 225
seohead serp-fetch --queries "underfloor heating,floor screed" --region 213 --top 10
seohead spend-report --since 2026-08-01
```

The last five answer the question a crawl cannot: what actually happens in search. Two of
them need nothing at all — `wayback-history` and `crtsh-subdomains` read public archives and
public certificate logs. The other three need a credential this repository cannot obtain for
you, and say so rather than guessing: with no key configured they return
`{"ok": false, "error": "..."}` naming what is missing, which since #155 is a non-zero exit.
That is the command working, not failing.

**Layer rules.** Wordstat frequency is **base**, not exact: the API has no
`!`/`+`/`[]` operators and the base runs roughly 9x higher than exact. A
multi-region request **sums** frequency — query regions one at a time. Every
charge is journaled to `~/.config/seohead/spend.jsonl` the moment the task
is created, together with the `task_id`: a paid result can be re-fetched for
free even if parsing crashed. Secrets come only from `~/.config` files and
the environment; a missing-secret message carries the path, never the value.

**Google and the split of territories.** Wordstat and Arsenkin cover Yandex,
i.e. the Runet. Everything outside it — English-language projects, India,
the Gulf — lives in Google, and that is `google-keywords` and `google-serp`
via DataForSEO.

Warning: **DataForSEO does not support locations in Russia or Belarus across its services.**
Hence a geo guard: a request
with `--country` set to an uncovered country **does not go out to the
network** — it returns a refusal naming what to use instead. Without the
guard such a request charges money and returns an empty result — paying for
a zero hurts most.

Warning: **the default environment is `sandbox`**: real response shape, fake
data, nothing charged. Production mode is enabled deliberately:
`DATAFORSEO_ENV=prod`. It never switches automatically on purpose: a pipeline can be validated
against the sandbox, while production remains an explicit decision with financial impact.

**Why Metrika belongs here.** A crawl shows what a site has; Wordstat shows
the demand; Metrika shows what people did on the site. Without the third, a
report rests on guesses: a page can be technically perfect and get zero
visits. Order of work: `metrika-setup` **before** any traffic conclusions —
with no goals configured, "zero conversions" in a report is a consequence
of setup, not a fact about the site; data operations can silently reshape
reports.

**Traffic report as a PDF.** `metrika-traffic-pdf` turns one period into a client-ready,
static report shaped like an analytics traffic dashboard, in two separable stages:

1. **Collect** (`--counter`, `--date1`, `--date2`, `--out-dir`) reads the Reporting API and writes
   `metrika-traffic.json` (schema `seohead.metrika-traffic/1`). Every request passes `offset=1`;
   the API counts rows from 1. Attribution defaults to the **last significant source**
   (`ym:s:lastSignTrafficSource`, `ym:s:lastSignSearchEngineRoot`, `ym:s:lastSignSearchPhrase`),
   which is what Metrika's own interface shows; `--attribution last_click` uses the last-click
   dimensions instead, and its search totals usually differ by a few percent. `--traffic organic`
   (the default) keeps search-engine visits; the channels block always covers all traffic. Robot
   visits stay excluded, as the API does by default, and a high robot share becomes a warning.
2. **Render** formats a document — just collected or passed with `--document` — into
   `metrika-traffic.html` and `metrika-traffic.pdf`. Rendering computes nothing and makes no
   network request: the page is static, self-contained HTML with inline SVG charts and a system
   font stack.

Every block carries `status` (`ok`, `unavailable`, `skipped`) and a reason; a comparison with no
baseline is `null` with a status such as `new` or `no_baseline`, never a zero or an infinite
change, and the report draws such a block as an explicit "data unavailable" panel. Comparisons:
the previous period of equal length, the same dates a year earlier, and 3/6/12-month windows
against the windows before them. Existing report files are refused unless `--overwrite`.

The PDF is printed by a locally installed Chrome, Edge or Chromium in headless mode with its
sandbox on and a throwaway profile; `SEOHEAD_CHROME` overrides discovery. Without a browser the
HTML is still written and the result reports `pdf: skipped` with the reason.

Branding is a small JSON object or file (`--brand`), for example:

```json
{"name": "Example Co", "accent": "#B71C1C", "ink": "#0E2247", "logo_text": "EXAMPLE CO"}
```

Other keys: `card`, `table_header`, `positive`, `negative` (`#RRGGBB`) and `font_stack`. Labels
are English or Russian (`--lang en|ru`). Search Console queries are optional: pass rows as
`gsc_rows` in JSON input, or `--gsc-site-url` to read them from a property you own.

```bash
seohead metrika-traffic-pdf --counter 12345678 --date1 2026-09-01 --date2 2026-09-30 --out-dir ./traffic
seohead metrika-traffic-pdf --document ./traffic/metrika-traffic.json --out-dir ./traffic-ru --lang ru --brand ./brand.json
```

Warning: **Logs API exports contain raw `ClientID`** — visitors' personal
data. Never commit them, never show them to a client. The loader returns
text; the caller picks the output path and it must stay under `.gitignore`.

**No project-state accumulation here.** Which keyword was queried and why it survived filtering
belongs to the caller's project dataset, not to a provider transport client.

---

## Screaming Frog crawl audit (`seohead/sf/`)

A subcommand with its own argument parser:

```bash
seohead sf run --exports-dir docs/examples/exports --out report --tasks   # mode B
seohead sf run --crawl https://example.com --out report --tasks         # mode A
seohead sf doctor                                                    # diagnostics
seohead sf tasks --json report/audit.json                            # backlog from a report
```

Note: `sf tasks` takes the audit path via the required `--json` flag, not as
a positional argument (`seohead/sf/cli.py`).

`tasks_pipeline.group_by` accepts `check`, `issue`, or `check_assignment`.
The last mode groups a check only by an operator-declared template/component URL
assignment or by the audit's already validated segment rules. It records
assignment provenance and keeps candidate, declared, confirmed, and unassigned
states separate; it does not infer a shared implementation from repeated findings.

**248 checks**: 12 critical, 115 warnings, 121 notices. Sources: SF exports,
derived metrics, inlink exports, the sitemap module, and heuristics.

Native scans with retained bodies evaluate DOM depth/node count and content-area exact/near
duplicates offline, reading each page's selected document by exact URL. The duplicate fallback
uses finite document (10,000), text-input (16 MiB), shingle (1,000,000) and candidate-comparison
(250,000) budgets. Missing or invalid bodies and exhausted budgets stay explicit; partial
findings carry coverage. A non-stored crawl has no body source for these checks.

SF configuration can explicitly enable semantic review with `semantic_similarity.adapter`
and caller-supplied `embeddings` rows (`url`, `body`, `title`, `h1` vectors). An adapter declaration
contains `kind`, `model_id`, `model_version`, `settings` and `data_transfer`; an external declaration
also requires `external_authorized=true`. Core callers may instead inject a local embedding adapter.
`threshold` controls topical similarity (default 0.82), `relevance_threshold` controls title/H1
review (0.35), `max_documents` defaults to 1,000, and comparisons default to 250,000. A durable
`cache_path` is optional; otherwise the cache exists only for the run. No model is downloaded or
selected automatically. These notice-level findings are review candidates, and the semantic SF
coverage rows remain gaps until a real adapter is selected and validated.

**Two modes.** A crawls by itself through the SF CLI (license required). B
works from ready exports and is self-sufficient. Mode B is covered by tests
and must not be broken for the sake of mode A.

No data for a check -> it is marked skipped with a reason, not a silent
zero. This is a principle: "0 problems" and "the check never ran" are
different things.

---

## Calling conventions

**Input.** The primary form is `--input '<json>'`; the object is mapped onto
the handler's arguments. Frequent parameters are duplicated as flags:

```bash
seohead schema-check --url https://example.com/product/nasos
seohead backlinks-check --target example.com --donors-file donors.txt
seohead duplicate-check --input '{"items":[{"id":"a","text":"..."}],"threshold":0.9}'
seohead duplicate-check --scan saved.sqlite
seohead markdown-extract --url https://example.com/product/nasos
seohead boilerplate-report --input '{"pages":[{"url":"https://example.com/a","html":"..."}]}'
seohead boilerplate-report --scan saved.sqlite
echo '{"url":"https://example.com"}' | seohead parse
```

**Side effects only behind an explicit flag.** `--probe-paths` in
`security-check` is off by default. The SF audit sitemap/robots
live recheck (`--live-recheck` / `--no-live-recheck`) defaults to off in config, but it
turns on automatically for crawl modes (`--crawl`, `--crawl-list`, `--load-crawl`) when no
`--sitemap` is given; pass `--no-live-recheck` to keep it off.

**MCP.** The same set under the `seo_*` names plus the `sf_*` audit tools and the
`seo_semantics_*` tools (192 + 5 + 4):

```bash
seohead mcp        # stdio
```

## Where to go next
- [TOOL_REFERENCE.md](TOOL_REFERENCE.md) — every tool's arguments, types, defaults, cost, and failure modes, generated from the MCP definitions
- [CHECKS.md](CHECKS.md) — the 248 checks the SF crawl audit runs, generated from the registry
- [ARCHITECTURE.md](ARCHITECTURE.md) — layers, invariants, where new code goes
- [SKILLS.md](SKILLS.md) — which skill drives which tool
- [DECISIONS.md](DECISIONS.md) — why it was decided this way and not another

### Project priority policy

`project-priorities` / `seo_project_priorities` previews data-only work priorities from saved project facts. Explicit `--apply` requires the current checklist revision and saves policy provenance. It preserves operator choices and completion evidence; it does not run detection or change finding severity. See [project workspaces](PROJECTS.md).

`project-facts` / `seo_project_facts` is what puts those facts on the record after creation. Supplied facts are operator decisions and always win; `--detect` runs one `tech-detect` pass over the project's own target, reading robots.txt first, and records what it found as evidence under its own provenance. A detection that fails, is disallowed, or cannot tell two candidates apart leaves the fact absent with its reason. Nothing here runs implicitly: without `--detect` the command is offline, and without `--apply` it only previews.


## Additional registered workflows

`remediation-summary`, `remediation-cases`, `remediation-transition`, `remediation-record-verification`, `remediation-report`, `project-observe`, `project-inbox-submit`, `project-inbox-list`, `project-inbox-read`, `project-inbox-acknowledge`, `project-inbox-goal`, `project-inbox-unread`. See the generated tool reference for exact inputs, limits and side effects.

`project-observe` accepts `run_offset`/`run_limit` (`--run-offset`/`--run-limit`
in CLI) for each site's terminal-run history. Stored running records remain
visible on every page. Defaults are offset 0 and 20 terminal rows; totals cover
the retained 100-record store, not lifetime history. See [Projects](PROJECTS.md)
for pagination, revision and retention semantics.

## Explicit retained-audit PDF overview

`report-build --audit scan.sqlite --format pdf --pdf-policy overview-v1 --out report.pdf`
creates a bounded PDF overview plus complete JSON, findings/pages/scope CSV files and a
hashed manifest. The PDF states full, displayed and omitted counts; it is not an
exhaustive listing or representative sample. Streamed audit.v2 PDF inputs require this
explicit policy. Materialized inputs and other formats refuse it. Both the PDF and
its companion directory must be new. See [PDF_OVERVIEW.md](PDF_OVERVIEW.md) for display,
disk and publication limits.

## One-shot monitoring

After configuring and explicitly claiming a bounded monitor plan, `monitor-collect`
previews it. Add `--apply` to collect exactly that claim. The collector records source
provenance and partial/failure states; it does not start a scheduler or delivery.
`monitor-local-deliver` records a deduplicated `local:receipt` for a retained run,
without contacting an external recipient. Both require the current monitoring revision.
See [PROJECTS.md](PROJECTS.md) for the claim and local receipt sequence.

## Backlink profiles and paid-call estimates

`backlinks-profile` / `seo_backlinks_profile` merges explicit CSV imports, retained provider
responses, Common Crawl extracts and selected bounded provider operations. It is offline when
`sources` is absent. `project` or `db` opts into private local history snapshots; it never writes
the retained scan schema. Full rel token sets preserve normal, nofollow, sponsored, ugc and
unknown observations separately. See [BACKLINK_PROFILES.md](BACKLINK_PROFILES.md).

`backlinks-disavow` generates only explicitly reviewed donor domains/URLs, optionally at a new
local path. It does not automatically classify a donor as harmful or submit a search-engine request.

`dataforseo-estimate` is an offline quote using a supplied current `appendix/user_data` price
snapshot. Query count, maximum billed rows and caller-declared cache hits stay visible; unknown
pricing stays unavailable. SERP quotes require the query and include depth/operator adjustments. Production Google calls require `confirm_paid=true`; backlink calls
also require explicit enablement/account/cost approval. A supplied `spend_ceiling_usd` is enforced
before transport and requires known pricing. Project/month currency limits await the owner decision.

## Retained link graph and sitemap generation

`scan-graph` / `seo_scan_graph` reads retained internal edges without network. Filter positions
(including `not_classified`), sections or an `--around` URL with `--depth`; page nodes and edges
with independent offsets/limits. Hard ceilings are 5,000 nodes and 20,000 edges. Large graphs
use bounded section aggregation and report clipping, coverage and the scope of orphan/component
counts. `--compare-to` reads a second scan and reports added/removed pairs and new observed
orphans. These counts do not prove whole-site reachability on an incomplete crawl.

`scan-reanalyze --step links.position --max-documents N` writes a new derived scan containing
a bounded offline position-backfill batch. Continue by passing that output as the next input.
The checkpoint and document identities carry into each derived file; source scans remain
immutable. The default full reanalysis also backfills retained links. Missing bodies and
occurrence-identity mismatches remain `not_classified` with a reason in `scan-status` and
`scan-link-inspect`. No page is fetched to infer a position.

`scan-sitemap-generate` / `seo_scan_sitemap_generate` writes fresh local sitemap files from
retained same-origin, indexable, canonical HTTP 200 HTML pages. Supply an explicit deployment
base URL ending in `/`, and a fresh output directory. It splits at 50,000 URL entries or 50 MiB
uncompressed and writes a sitemap index. Optional image sitemaps use complete retained HTML;
missing image bodies and unmeasured responses are reported. Known non-200 resource
responses are excluded. lastmod uses retained modification evidence and is omitted
when unknown. Exclusion globs are explicit. The command does not deploy the generated files.

Native retained audits also group 4xx/5xx internal link targets with source pages and anchors,
record redirect chain/loop hop lists and checked external no-response outcomes, distinguish
empty internal anchors from generic text, and reconcile sitemap members with retained HTTP,
canonical, noindex and robots evidence. Finding location samples carry exact omitted counts;
all original contributing links remain pageable through `scan-url-links`.

Graph comparison applies section, position and representation filters across both scans.
The `around` option limits the displayed current graph; it does not narrow comparison scope.
