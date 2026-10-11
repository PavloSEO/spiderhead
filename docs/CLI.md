# CLI overview

Every public capability is one shared handler exposed twice: as a `seohead` CLI command and as
an MCP tool with the same name (`crawl-site` becomes `seo_crawl_site`). This page groups the
registered commands by job. Exact arguments, defaults, cost and failure modes live in the
generated [tool reference](TOOL_REFERENCE.md); network use and side effects per layer are
explained in [TOOLS.md](TOOLS.md); accepted inputs are in [INPUTS.md](INPUTS.md).

The **Effects** column comes from each tool's MCP annotations: `network` means the tool can send
requests, `writes` means it can create or change local files, and `paid` means it can spend
provider money. "Offline, read-only" tools neither send requests nor write files. Annotations
describe what a tool *may* do; most writes need an explicit flag such as `--apply` or `--out`,
and paid providers keep their sandbox or preview defaults where one exists.

Grouped CLI forms are aliases: `seohead scan list` is `scan-list` and `seohead project progress`
is `project-progress`.

## Entry points that are not single tools

| Command | What it does |
|---|---|
| `sf run` | Audit existing Screaming Frog CSV/XLSX exports offline (`--exports-dir`), or run a separately installed, licensed SF CLI first (`--crawl`). Writes `audit.json`/`audit.md`; `--tasks` adds the prioritized backlog. |
| `sf tasks` | Build `tasks.json`/`tasks.md` from an existing `audit.json` or scan artifact. |
| `sf doctor` | Diagnose SF CLI discovery and optional dependencies. |
| `sf save-config` | Copy the most recent SF crawl configuration into a reusable base config file. |
| `semantics <stage>` | Accumulating semantic core in its own SQLite database: `init`, `import`, `collect`, `clean`, `graph`, `exact`, `cluster`, `competitors`, `synonyms`, `report`, `excel`, `mine`, `sitematch`, `relevance`, `status`, `export`, each with `--project DIR`. `collect`, `synonyms`, `cluster` and `exact` are paid; see [SEMANTICS.md](SEMANTICS.md). |
| `mcp` | Start the local stdio MCP server; `--profile` selects `full`, `audit`, `infra`, `quick-check` or `router` ([MCP profiles](MCP_PROFILES.md)). |
| `watch` | Optional Rich terminal observer for one project beside an AI chat ([terminal observer](TERMINAL.md)). Needs the `tui` extra. |
| `tui` | Interactive terminal shell over the same handlers ([TERMINAL.md](TERMINAL.md)). Needs the `tui` extra. |
| `scan`, `project`, `skill`, `scenario` | Grouped aliases for the `scan-*`, `project-*`, `skill-*` and `scenario-*` commands below. |

The `sf_*` MCP tools (`sf_audit_run`, `sf_audit_summary`, `sf_audit_issues`, `sf_audit_tasks`,
`sf_list_exports`) expose the Screaming Frog analyzer to agents. The `seo_semantics_*` MCP tools
(`seo_semantics_status`, `seo_semantics_run`, `seo_semantics_mine`, `seo_semantics_paid`) expose
`seohead semantics`; the paid one refuses to run without `confirm_paid=true`.

## Collect a site

| Command | What it does | Effects |
|---|---|---|
| `crawl-site` | Crawl a site from a start URL by following links, or fetch an explicit `urls` list instead of following links at all, then audit the result through the same checks… | network, writes |
| `crawl-describe-settings` | List every crawl-site config setting: dotted path, type, default value, description, and whether it changes what the audit finds (results-affecting) or only… | offline, read-only |
| `crawl-diagnose` | Explain a small or unfinished native crawl from retained scan or run evidence. | offline, read-only |
| `crawl-diagnose-export` | Create one new redacted crawl-diagnostic JSON file from retained evidence. | writes |
| `crawl-import` | Read a versioned third-party crawl CSV bundle from a local manifest. | offline, read-only |
| `crawl-enrich` | Join an existing audit or scan to an offline URL-keyed CSV without a provider call. | writes |
| `sitemap-crawl` | Recursively parse a sitemap (index/urlset, gzip supported) into a URL tree. | network, writes |
| `site-audit` | Bounded sitemap-based evidence pass: selected URLs plus site-level checks, returned as one `seohead.site-audit/1` document. Not a link-graph crawl. | network |
| `inspect-url` | Inspect one URL with bounded metadata/header/robots/redirect/structured/render steps. | network |

## Saved scans (offline)

| Command | What it does | Effects |
|---|---|---|
| `scan-list` | List saved SQLite scan metadata without loading retained bodies. | offline, read-only |
| `scan-status` | Summarize frontier work and committed page outcomes from one saved scan offline. | offline, read-only |
| `scan-inspect` | Read a bounded, paginated table view from one saved scan. | offline, read-only |
| `scan-url-query` | Filter, sort and paginate the whole page table of one saved scan with total and filtered total. `--preset` applies a ready-made view; `--export PATH` writes all matching rows to a new CSV or XLSX file. | offline, read-only |
| `scan-url-detail` | Read one exact native URL's retained headers, redirects, page fields, and forms. | offline, read-only |
| `scan-url-history` | Read one exact URL's state across the newest saved scans of a project (`--limit` 1..500, default 50). | offline, read-only |
| `scan-link-inspect` | Inspect saved shortest paths, reverse inlinks, per-link DOM context, or one URL's paged links offline. | offline, read-only |
| `scan-navigation` | Read bounded observed navigation evidence from a retained local scan. | offline, read-only |
| `scan-rendered-routes` | Read stored static/rendered route evidence without fetching routes. | offline, read-only |
| `scan-evidence` | Read captured evidence, resource windows or the event timeline without fetching or migration. | offline, read-only |
| `scan-content-search` | Search one closed retained scan offline and create an indexed local NDJSON package. | writes |
| `scan-content-search-page` | Read up to 100 indexed derived content-search records without rereading the scan. | offline, read-only |
| `scan-extract` | Run bounded data-only extraction rules on retained complete bodies, without network or writes. | offline, read-only |
| `scan-fragment-links` | Evaluate every retained fragment anchor offline and page the results. | offline, read-only |
| `scan-structured-blocks` | Describe one retained URL's JSON-LD blocks offline: line, state, graph findings and source JSON. | offline, read-only |
| `scan-body-diff` | Compare compatible retained bodies offline; a change is not an SEO verdict. | offline, read-only |
| `scan-reanalyze` | Reparse retained HTML/DOM and rerun existing checks without network. | writes |
| `scan-export` | Export retained scan data under scan_export.v1 as CSV, XLSX, JSON, or XML. | writes |
| `scan-snapshot` | Create a consistent new SQLite snapshot without overwriting a destination. | writes |
| `scan-pin` | Pin or unpin a finished scan; this is an explicit metadata mutation. | writes |
| `scan-prune` | Preview candidates by default; deletion requires apply plus the reviewed plan. | writes |
| `scan-requeue` | Explicitly requeue selected saved URLs in the same SQLite with verified backup and attempt history. | writes |
| `scan-import-urls` | Explicitly import a TXT/CSV/XLSX/XML seed list through stored scope and query guards with backup. | writes |

## Page and URL checks

| Command | What it does | Effects |
|---|---|---|
| `parse` | Parse SEO data (title, meta description, canonical, OG/Twitter, H1-H6, JSON-LD, links, visible text, word count) from one URL or a list of URLs. | network |
| `robots-check` | Fetch and analyze a site's robots.txt: user-agent groups, declared sitemaps, and whether given paths are crawlable. | network |
| `headers-check` | Inspect SEO-relevant response headers (X-Robots-Tag, canonical Link, Cache-Control, HSTS, ...), HTTP version, TTFB, and body size. | network |
| `redirects-check` | Follow a live redirect chain for a URL and report each hop (status, location). | network |
| `links-check` | Check a page's outbound links for broken (4xx/5xx) targets and links that point at redirects (wasted crawl hops). | network |
| `hreflang-check` | Extract and validate a page's hreflang alternates (x-default, self-reference, duplicates, malformed codes). | network |
| `render-check` | Compare the raw server HTML with the DOM after JavaScript runs — the gap between them is what a non-rendering crawler loses. | network |
| `schema-check` | Validate a page's structured data in two layers. | network |
| `schema-build` | Suggest a connected Schema.org @graph for a page. | network |
| `social-meta-check` | Check OpenGraph and Twitter Card tags against the minimum needed for a link preview to render: which required tags (og:title/type/url/image/image:alt,… | network |
| `soft404-check` | Detect soft-404: whether the site returns an honest 404/410 for non-existent URLs, or silently serves 200/3xx (which pollutes the index with junk pages). | network |
| `mirror-check` | Audit host consolidation across HTTP/HTTPS, `www`, index.php/index.html, case, and trailing-slash variants. | network |
| `markdown-extract` | Render a page as Markdown in two scopes. | network |
| `asset-weight-check` | Fetch a page's linked CSS/JS and report delivery problems: render-blocking resources in <head>, oversized files, duplicate libraries bundled more than once (by… | network |

## Infrastructure, security and recon

| Command | What it does | Effects |
|---|---|---|
| `domain-profile` | Infrastructure profile of a domain: registrar and domain age (RDAP, whois fallback), DNS records with DNS/mail provider, hosting IP with ASN, owner and country,… | network |
| `cdn-check` | Which CDN sits in front of a URL and whether caching actually works: cache status on a repeat request (MISS->HIT), Cache-Control, ETag/Last-Modified, 304… | network |
| `tech-detect` | Detect the technologies behind a page: CMS, framework, server stack, analytics and ad pixels, chat widgets, consent tools, fonts and third-party script hosts. | network |
| `security-check` | Security headers with a score and grade (HSTS, CSP, X-Frame-Options, X-Content-Type-Options, Referrer-Policy, Permissions-Policy), software version disclosure, cookie… | network |
| `crtsh-subdomains` | Subdomains discovered from public Certificate Transparency logs (crt.sh). | network |
| `cloudflare-traffic` | Bot and human traffic for a Cloudflare zone from edge analytics (aggregated, not raw logs), shaped like `log-analyze`. | network |
| `wayback-history` | Every recorded Wayback Machine snapshot of a URL, oldest first: timestamp, HTTP status, and MIME type at capture time. | network |
| `log-analyze` | Analyse a web server access log (Apache/Nginx Common or Combined, IIS W3C; gzip-compressed files are read directly). | network |
| `regions-check` | Audit a site's regional structure: subdomains (msk.site.ru), folders (site.ru/msk/) and satellite domains (site-msk.ru). | network |
| `backlinks-check` | Verify backlinks from a list of donor pages: is the link still there, its anchor and rel, whether it passes weight (nofollow/ugc/sponsored), and whether the donor… | network |
| `backlinks-profile` | Merge explicit backlink CSV/provider evidence, retain private history, or read history with --history; source calls are explicit. | network, writes, paid opt-in |
| `backlinks-disavow` | Export only reviewed donor domains/URLs to a new local file; never submit or overwrite. | writes |
| `dataforseo-estimate` | Quote supplied provider prices offline; --refresh-prices explicitly reads the free account price endpoint. | network opt-in |

## AI search readiness (GEO/AEO)

| Command | What it does | Effects |
|---|---|---|
| `ai-bots-check` | Which AI crawlers (GPTBot, ClaudeBot, Perplexity, Google-Extended, CCBot, Bytespider, Meta-ExternalAgent, …) the site lets in, and which it blocks in robots.txt. | network |
| `llms-txt-check` | Fetch and score the site's /llms.txt (the LLM-facing manifest): 9 checks — H1 title, >=3 sections, >=3 links, brand mention, category mention, product/ proof/docs… | network |
| `citability-check` | Score how citable a piece of content is for AI answers (GEO/AEO): 0-100 across four dimensions (25 each) — Answer Blocks (self-contained 20-200 word paragraphs),… | network |

## Content, semantics and assets

| Command | What it does | Effects |
|---|---|---|
| `duplicate-check` | Find near-duplicate pages among a list of {id, text} documents using simhash + locality-sensitive hashing (no O(n^2) pairwise comparison). | offline, read-only |
| `boilerplate-report` | Answer "is the boilerplate actually the same everywhere?" across a crawled corpus. | offline, read-only |
| `semantic-inputs` | Build the reproducible normalized-input manifest for semantic analysis over retained page content. | offline, read-only |
| `semantic-similarity` | Group topical and internal-link review candidates from supplied vectors. | writes |
| `meta-description-drafts` | Prepare or validate a resumable, page-grounded meta-description batch. | writes |
| `ai-column` | Plan or validate a per-URL AI custom column over retained page evidence; no model call. | writes |
| `marketing-inventory` | Inventory supplied CTA/form DOM occurrences without fetching or submitting forms. | writes |
| `keywords-cluster` | Cluster keywords into topic groups (K-Means, DBSCAN, Agglomerative). | offline, read-only |
| `images-download` | Download images from a URL list, setting the correct extension by content-type and skipping already-downloaded files. | network, writes |
| `images-optimize` | Compress, convert, or resize raster images and conservatively minify SVG. | writes |
| `redirects-generate` | Generate redirect rules (Apache mod_rewrite/Redirect, Nginx, or a custom template) from a list of {old_url, new_url} pairs. | offline, read-only |

## Compare, verify and remediate

| Command | What it does | Effects |
|---|---|---|
| `compare-crawls` | Diff two audit documents (dict, JSON path, or scan.v1 SQLite path) into four disjoint sets per finding: entered (new problem on a page that existed before), left (the… | writes |
| `segment-diff` | Cross-segment counterpart diff: which pages in a source segment (for example `en`) have a counterpart in a target segment (for example `pl`), and which do not. | offline, read-only |
| `verify-fixes` | Recheck selected baseline findings in an explicit, bounded URL subset. | network, writes |
| `remediation-create` | Create one new empty remediation ledger bound to a validated local project; an existing path is refused. | writes |
| `remediation-ingest` | Ingest one retained saved audit into a ledger as baseline/history without rerunning a crawl; re-ingest is idempotent. | writes |
| `remediation-summary` | Read explicit remediation and recheck coverage from retained local evidence. | offline, read-only |
| `remediation-cases` | Read a bounded page of exact remediation cases and decision history. | offline, read-only |
| `remediation-transition` | Append one revision-safe, evidence-bound lifecycle decision. | writes |
| `remediation-record-verification` | Attach a retained bounded verification artifact to pending cases. | writes |
| `remediation-recheck` | Recheck exact pending ledger cases without widening to a site crawl. | writes |
| `remediation-report` | Render retained before/after remediation evidence without network access. | writes |

## Reports, views and BI

| Command | What it does | Effects |
|---|---|---|
| `report-build` | Turn an audit document into a file: xlsx, docx, csv, md, json or pdf. | writes |
| `facts-export` | Build one comparable facts table (schema facts.v1) across several sites from crawl/site audits you already produced. | offline, read-only |
| `findings-view` | Apply one saved view to an audit object, JSON file, or validated scan.v1 SQLite artifact. | offline, read-only |
| `bi-export` | Project a saved scan or audit, plus optional evidence joins, into a local typed BI package. | writes |
| `bi-filter` | Filter a verified local BI package into a new typed package and optional XLSX. | writes |
| `bi-sheets-plan` | Preflight a complete local BI package for Sheets without Google access or writes. | offline, read-only |
| `bi-bigquery-plan` | Describe an optional BigQuery load offline; it never selects a project or writes data. | offline, read-only |
| `bi-destination-apply` | Request a BI destination write; host authorization is required and credentials are never accepted. | network, writes |
| `publication-cohorts` | Project saved publication metadata and normalized provider evidence locally. | writes |
| `gsc-progress` | Project saved GSC query evidence into branded/non-branded summaries. | writes |

## Projects and agent handoff

| Command | What it does | Effects |
|---|---|---|
| `project-new` | Create a local project workspace; no crawl, checklist execution, or network work starts. | writes |
| `project-start` | Create and prepare a new bounded project; failures leave inspectable pending work. | network, writes |
| `project-prepare` | Prepare an existing project with a bounded native crawl and saved sitemap coverage. | network, writes |
| `project-competitors-add` | Record supplied competitor candidates as separate local workspaces; no crawl runs. | writes |
| `project-competitors-list` | Read the recorded competitor candidates and their workspaces. | — |
| `project-competitors-remove` | Drop one recorded competitor from the shortlist; its workspace stays on disk. | writes |
| `project-open` | Open a local project workspace without executing template references. | offline, read-only |
| `project-status` | Show project scan history and named pending checklist/preparation states. | offline, read-only |
| `project-sources-link` | Bind a provider resource ID to a project in `sources.json`; no provider call. | writes |
| `project-sources-unlink` | Remove one resource binding from `sources.json`; no provider call. | writes |
| `project-sources-list` | List resource bindings of a project. | offline, read-only |
| `project-progress` | Show a compact, paginated project checklist view and its next actions. | offline, read-only |
| `project-observe` | Read the bounded project observer snapshot: tasks, methods, competitors, retained scan state and the execution-log tail. | offline, read-only |
| `project-activity` | Read lightweight current activity for a local project and its sites. | offline, read-only |
| `project-scans` | Read a bounded page of retained project scans with evidence metadata. | offline, read-only |
| `project-task-detail` | Read one project task definition, evidence and bounded history. | offline, read-only |
| `project-checklist-init` | Initialize or reconcile a local checklist without executing a check, skill, or scenario. | writes |
| `project-checklist-update` | Add or update one local checklist definition without executing it. | writes |
| `project-checklist-record` | Record supplied evidence for one checklist item without executing its operation. | writes |
| `project-checklist-page` | Read a bounded searchable page of project checklist evidence. | offline, read-only |
| `project-facts` | Preview or record project stack facts that stack-aware priorities then read. | network, writes |
| `project-priorities` | Preview stack-aware project priorities from saved facts without network requests. | writes |
| `project-policy` | Read or explicitly update operator crawl defaults and project admission thresholds. | writes |
| `project-view-list` | List saved declarative finding views and the current project view-config revision. | offline, read-only |
| `project-view-show` | Read one saved finding view with its stable identity, schema version and revision. | offline, read-only |
| `project-view-save` | Create or revise a bounded declarative finding view using an expected config revision. | writes |
| `project-view-delete` | Delete a saved declarative finding view by name. | writes |
| `project-view-rename` | Rename a saved declarative finding view. | writes |
| `project-inbox-submit` | Persist a specialist note, proposed goal or question without starting any work. | writes |
| `project-inbox-list` | List a bounded project inbox page without consuming any entries. | offline, read-only |
| `project-inbox-read` | Record an agent's explicit inspection; acknowledgment remains separate. | writes |
| `project-inbox-acknowledge` | Explicitly acknowledge entries. | writes |
| `project-inbox-goal` | Explicitly accept or complete a stored proposed goal; no executor is launched. | writes |
| `project-inbox-triage` | Append an explicit task, goal, competitor, blocked, or rejected note outcome. | writes |
| `project-inbox-unread` | Return a bounded unread reference summary without changing delivery state. | offline, read-only |
| `project-event-append` | Append one structured event (source, actor, text) to the project journal `events.jsonl`; earlier events are never edited. | writes |
| `project-event-page` | Read a newest-first bounded page of project events, with optional source and text filters. | offline, read-only |
| `audit-workflow` | Use a closed project workflow: status, bounded start/prepare, or an evidence-backed report. | network, writes |
| `workflow-start` | Start a local registered workflow; it performs no scan or provider call. | writes |
| `workflow-checkpoint` | Persist one registered-step result before the next step or agent handoff. | writes |
| `workflow-status` | Recover the exact next registered step after interruption or handoff. | offline, read-only |
| `workflow-execute` | Checkpoint a supplied local sequence without executing network work. | writes |
| `workflow-resume` | Reopen only the interrupted registered step after a second-agent handoff. | writes |

## Monitoring

| Command | What it does | Effects |
|---|---|---|
| `monitor-configure` | Configure a disabled local incremental monitor; this starts no schedule or message delivery. | writes |
| `monitor-schedule` | Claim or recover a local run; no timer, crawl, or delivery starts here. | writes |
| `schedule-show` | Read scheduled-scan settings, last run, and the next due window. | offline, read-only |
| `schedule-set` | Merge a partial update into the project schedule with an expected revision; starts nothing. | writes |
| `schedule-run-due` | Run the project scheduled scans if due; refuses overlap and runs outside the window. | network, writes |
| `schedule-install` | Write a launchd plist, cron line, or Task Scheduler command for run-due; never loads it. | writes |
| `schedule-uninstall` | Remove a trigger file written by schedule-install; does not unload it. | writes |
| `monitor-collect` | Preview or explicitly collect one previously claimed bounded monitor plan. | network, writes |
| `monitor-run` | Record one bounded retained-scan diff; quiet runs do not notify anyone. | writes |
| `monitor-local-deliver` | Record a deduplicated local receipt for one retained monitoring run. | writes |
| `monitor-status` | Read local monitor policy and its last retained checkpoint. | offline, read-only |

## Search, analytics and demand providers

| Command | What it does | Effects |
|---|---|---|
| `provider-registry` | List provider operations, credential components, quota and privacy boundaries. | offline, read-only |
| `provider-readiness` | Inspect configured credential sources and declared operation routes offline. | offline, read-only |
| `provider-auth` | Manage GSC read-only OAuth grants: import private file, refresh, cancel a pending flow, or explicitly revoke. | network, writes |
| `provider-verify` | Explicitly verify bounded read-only account/target access; present credentials are not verification. | network |
| `provider-collect` | Collect a declared provider operation with versioned redacted evidence. | network, writes, paid |
| `provider-replay` | Join a saved private provider collection to a saved scan with no network; retain raw rows locally and return counts. | writes |
| `provider-join` | Join supplied URL evidence exactly and preserve unmatched populations and technical severity. | offline, read-only |
| `evidence-normalize` | Normalize a supplied CSV/XLSX/JSON or saved provider envelope, fully offline. | writes |
| `evidence-join` | Join normalized analytics/search evidence to crawl pages, offline only. | writes |
| `sources-doctor` | Inspect redacted credential references, readiness and declared provider operations. | offline, read-only |
| `sources-sync` | Accumulate provider data in one local SQLite database: fetch missing or explicitly forced days. | network, writes |
| `sources-status` | Offline local history: requested, complete, empty, partial, failed, pending and lagged dates, gaps, rows, metric additivity and reporting-timezone policy. | offline, read-only |
| `sources-export` | Read stored rows of one source filtered by resource, date range and a substring of any dimension (match). | writes |
| `gsc-query` | Google Search Console: search performance for a verified property (mode=search_analytics: clicks, impressions, position, CTR) or Google's own indexing verdict for one… | network |
| `gsc-archive` | Manage an explicit local GSC SQLite archive. | network, writes |
| `webmaster-url-queries` | Read bounded Yandex Webmaster URL-to-query evidence for a verified host. | network |
| `crux-report` | Field Core Web Vitals (LCP, INP, CLS) as real Chrome users experienced them, at the 75th percentile — the honest counterpart to seo_render_check's… | network |
| `indexnow-submit` | Push up to 10,000 changed URLs to Bing, Yandex, Naver, and Seznam in one call. | network, writes |
| `metrika-counters` | List the Yandex Metrika counters this token can see (id, name, site). | network |
| `metrika-setup` | How a counter is configured: goals, filters, data operations. | network |
| `metrika-report` | What visitors actually did, as flat records. | network |
| `metrika-traffic-pdf` | Static A4-landscape traffic report (HTML + PDF) in the style of an analytics dashboard, built from Yandex Metrika: KPI cards with % change, daily dynamics against the… | network, writes |
| `topvisor-read` | Read existing Topvisor projects, competitors, groups, keywords, history or summary. | network |
| `keywords-expand` | Expand a seed phrase via Yandex Wordstat: refinements (left column) plus similar queries (right column), each with its base frequency. | network, writes, paid |
| `keywords-seasonality` | Demand over time for one phrase (Yandex Wordstat dynamics). | network, writes, paid |
| `keywords-exact` | Exact frequency (!W) for a list of phrases via Arsenkin — the number Wordstat's API will not give you. | network, writes, paid |
| `serp-fetch` | Yandex search results for one query or a batch. | network, writes, paid |
| `regions-tree` | Authoritative tree of Yandex region IDs for the regions[] parameter. | network, writes |
| `google-keywords` | Google demand via DataForSEO: pass `keywords` for search volume and competition on a list, or `seed` to expand semantics from one phrase (the Google counterpart of… | network, writes, paid |
| `google-serp` | Google organic results for a query — who actually ranks. | network, writes, paid |
| `miratext-analyze` | Start or resume Miratext analysis; paid and keyword modes need confirmation. | network, writes, paid |
| `spend-report` | What the paid sources have actually charged: totals by source, by operation and by day, read from the local journal. | offline, read-only |

## Catalogue and diagnostics

| Command | What it does | Effects |
|---|---|---|
| `tool-catalog` | Search complete source-derived tool metadata and load argument details only on request. | offline, read-only |
| `skill-list` | List the packaged, source-derived method playbooks without executing them. | offline, read-only |
| `skill-show` | Return a packaged skill's exact text and definition identity. | offline, read-only |
| `scenario-list` | List the packaged workflow scenarios with their ordered steps, without running them. | offline, read-only |
| `scenario-show` | Return a packaged workflow scenario's text without running its commands. | offline, read-only |
| `log-scan` | Report claims a finished run makes that cannot all be true at once: a recorded size that disagrees with the file, a check firing more often than there are pages to… | offline, read-only |

## Shared MCP state and client registration

`seohead mcp` starts the local stdio server. It defaults to the shared profile;
`--profile full|audit|infra|quick-check|router` can further restrict a process.
`seohead mcp status --json` reports enabled state, profile, actual registry count,
last actor/time (`by_label`: a Russian caption with the actor and the local day and
time of the last switch, or the Russian for "default" before any switch), every
profile as `profiles` (`id`, Russian `label`, tool count taken from the registry), and whether each supported client
has a SEOHEAD entry. It never
reports foreign keys, tokens, connection activity, or configuration contents.

```bash
seohead mcp status --json
seohead mcp disable --json
seohead mcp enable --profile router --json
seohead mcp install --client codex --dry-run --json
seohead mcp install --client codex --yes --json
seohead mcp uninstall --client codex --yes --json
seohead mcp backups --json
```

State is in `$SEOHEAD_CONFIG_DIR/mcp-state.json` when explicitly configured, otherwise
`$XDG_CONFIG_HOME/seohead/mcp-state.json` (default `~/.config/seohead/mcp-state.json`). Missing
state preserves enabled/full compatibility. Invalid state fails closed. Each
switch records actor, UTC timestamp and history; Desktop uses `--actor "SEOHEAD
Desktop"`. Disable rejects startup and subsequent tool calls on running servers.
A call already executing is not cancelled. A narrower shared profile immediately
blocks excluded tools; restart clients to advertise newly enabled tools.

Client paths: Claude Code `~/.claude.json`; Claude Desktop's platform application
configuration (`~/Library/Application Support/Claude/claude_desktop_config.json`
on macOS); Codex `$CODEX_HOME/config.toml` (default `~/.codex/config.toml`); Cursor
`~/.cursor/mcp.json`. JSON uses `mcpServers.seohead`; Codex uses
`mcp_servers.seohead`. Registration uses the current Python interpreter with
`-m seohead mcp`, or the frozen core executable with `mcp`. An optional absolute
`--command` selects the executable explicitly.

Dry-run returns only the file, exact SEOHEAD block, adjacent timestamped
backup path and original SHA-256. It creates no files. Pass `--backup PATH` from
the reviewed plan to use exactly that destination. Writing requires `--yes`;
`--expected-sha256 HASH` additionally rejects a configuration changed after preview.
An existing SEOHEAD entry is updated only after explicit approval, without
printing its previous values; its backup can restore that entry.
Writes are locked, backed up with a SHA-256 receipt, atomically replaced and parsed
again. JSON foreign values and TOML comments/keys are preserved. Symlinks,
hardlinks, invalid documents and oversized files are refused. Backups are private
and may contain foreign secrets: retain them locally.

Uninstall verifies the latest backup/receipt and the installed SEOHEAD entry.
When the whole configuration is unchanged it restores original bytes; otherwise
it restores only SEOHEAD's entry and preserves subsequent foreign changes.
Backups remain available. Installation does not enable MCP implicitly.
`seohead mcp backups [--client NAME] [--json]` lists SEOHEAD's adjacent backups,
newest first: client, path, UTC time, reason (`install`), the backup's current
SHA-256 and whether it still matches its receipt. Files without a SEOHEAD receipt
are not listed. Call journaling is not implemented yet (#977).

## Localized terminal observer

`seohead watch --project DIR` and `seohead tui --project DIR` accept `--lang ru|en`
(default from LC_ALL/LC_MESSAGES/LANG), `--scan RUN` and `--compact`. A retained
report directory containing `project/project.json` is accepted as a project entry.
The current-scan block uses retained counters; a missing denominator, queue or
error measurement is shown as unavailable. Discovery progress is not whole-site
completion. A run is shown by a short id (`r-` plus four hex digits) and its mode;
the full hash stays in JSON. Russian output groups thousands with a space (`1 330`). Status colours follow SEOHEAD brand roles. Evidence text is preserved.
`--scan` selects a run id or saved scan UUID present in the bounded observation;
a missing id is an error, never a silent substitution. The compact mode shows one
status line for tmux; when piped it emits one snapshot and exits. It does not start
scans or update the project. Notes/goals still require their explicit editor action.

```bash
seohead watch --project workspace/site --lang ru --compact
seohead tui --project workspace/site --lang en --compact
```
