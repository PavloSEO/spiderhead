---
name: seo-deep-audit
description: >-
  The SF-registry deep-audit pipeline: reconnaissance (domain/hosting/CMS), crawl evidence from
  a separately installed licensed Screaming Frog CLI or supplied exports analyzed against the
  full check registry, agent-level analysis (robots, JS rendering, silo structure, H1–H6), and a
  consolidated report + task backlog. `control` is the single entry point for an unscoped "audit
  this site" request and delegates its crawl step here once it has decided that SF (a licensed
  CLI or supplied exports) is available and that full-registry depth is worth the setup; go
  straight here only when that decision is already made — SF/exports are in hand, or the user
  named "Screaming Frog," "exports," or "deep audit" explicitly. Use a narrow analysis only when
  the scope is stated explicitly ("only robots," "quick/lite," "no rendering"). Triggers: SF deep
  audit, Screaming Frog audit, exports audit, full SEO audit with SF, deep SEO audit.
---

# SEO Deep Audit — the SF-registry pipeline `control` delegates to

`control` is the controller: given a bare "audit this site" with no scope, start there — it
scopes the site, then decides whether native `crawl-site` or this skill's SF-based pipeline
collects the crawl evidence. This skill is that second option: when a licensed Screaming Frog
CLI or supplied SF exports are available and full-registry depth is wanted, work at **maximum
depth**: run the entire chain of skills and tools yourself, collect everything, and
consolidate it into one report + task plan. Narrow the work only when EXPLICITLY instructed
("only robots," "quick," "lite," "no rendering," "check only headings"). The map of "which tool
retrieves what" is in the `sf-boundaries` skill.

> If you need a plan for **exactly what** to collect before expensive runs, start with
> `audit-roadmap` (Recon lite -> roadmap with priorities, scale, site type, and YAGNI), and then
> this skill executes the roadmap.

## Trigger
- `control` has scoped an unscoped "audit this site" request and decided the
  SF-based pipeline is the right collector — a licensed SF CLI or exports are
  available and full-registry depth is worth the setup.
- The user names Screaming Frog, SF exports, or "deep audit" explicitly, or
  already has a licensed SF CLI/exports in hand and wants the full pipeline
  run over them directly, without going through `control` first.
- A request that started narrow has grown into "also check X, and Y, and Z"
  to the point where running the whole SF-based pipeline is cheaper than
  hand-assembling the parts one skill at a time.

## Anti-trigger
- A bare unscoped audit request with no SF license and no exports mentioned
  yet ("audit this site," "what's wrong with it") — start with `control`
  instead. It scopes the site and begins with the native `crawl-site`
  collector, which needs neither a licensed SF CLI nor supplied exports;
  it delegates here only once it decides full SF-registry depth is worth
  the extra setup. Routing a bare request straight here makes "no SF/exports
  available" a false blocker for what `control` would have run natively.
- The user names one narrow check explicitly ("only robots.txt," "just check
  schema," "quick tech-detect") — go straight to that skill (`robots-audit`,
  `schema-graph`, `seo-recon`, …) instead of running every phase here.
- Only crawl evidence is wanted, with no reconnaissance or agent-level work —
  call `sf-analyzer` directly instead of the orchestrator.
- No domain or URL was actually supplied (e.g. a generic "how do I improve
  SEO" question) — there is nothing yet to audit.
- A full audit already exists and the question is "did anything change since
  last time" — use `compare-crawls` against the earlier `audit.json` instead
  of rerunning every phase from scratch.

## Preconditions
- [ ] A domain or URL is provided — this orchestrator has no target without one.
- [ ] `seohead sf doctor` has been run to know whether Mode A (live SF crawl)
  or Mode B (`--exports-dir`) applies for Phase 1.
- [ ] For Mode A: a licensed SF CLI is installed and discoverable; for Mode B:
  an exports folder path is available.
- [ ] Network reachability to the domain for Phase 0 reconnaissance and the
  Phase 2 checks that make live requests (`js-render-check`, `security-check`)
  — otherwise those phases degrade honestly rather than silently skip (see
  "Graceful degradation").

## Default rule
- **No scope clarification -> MAX.** Run every phase below.
- **Explicitly narrowed scope -> only that scope.** "only sitemap" -> only the sitemap portion;
  "quick/lite" -> `--profile lite` + skip expensive agent-level phases.
- Never ask "should I do a full audit?" — a full audit is the default. Clarify only when there
  is truly nothing to act on (no domain). **Neither SF being installed nor exports being
  available is a blocker for auditing the site** — it only means this specific pipeline cannot
  run; hand off to `control`'s native `crawl-site` collection instead of stopping to ask.

## Pipeline (all phases by default)
**Phase 0 — Reconnaissance (what kind of site is this?).** The `seo-recon` skill — three toolkit
tools:
```bash
seohead domain-profile --domain <domain>   # registration, DNS, ASN, geography, TLS, flags
seohead cdn-check       --url https://<domain>   # CDN + actual cache behavior, HTTP/2-3, TTFB
seohead tech-detect     --url https://<domain>   # CMS/framework/server, analytics, pixels
```
Remember the stack: if `tech-detect` found an SPA/Next.js/Nuxt, mark JS rendering as required
(Phase 2). Some findings from `domain-profile.flags` and `cdn-check.findings` go directly into
the report.

**Phase 1 — Crawl evidence (202-check registry).** SEOHEAD is the analyzer and adapter here, not
the crawler. Check the environment first with `seohead sf doctor`; live mode requires a separately
installed, actively licensed Screaming Frog CLI.
```bash
seohead sf run --crawl https://<domain> --out report --tasks
```
The **full** profile is the default (maximum available coverage), and the sitemap is automatically
obtained from robots. The registry contains 183 checks, but only checks supported by the available
exports and enabled SF modules can run. If the output contains many `skipped` results
(MIXED_CONTENT/STRUCTURED_DATA/SPELLING/DOM_*), enable them once through the `sf-config` skill
(create `audit.seospiderconfig`); the tool will pick it up automatically. If SF/a license is not
available, request exports and use mode B (`--exports-dir`); see `sf-analyzer`/`sf-config`.

**Phase 2 — Agent-level (what SF cannot see).** In parallel or sequentially:
- `robots-audit` — analyze robots.txt for junk/harmful directives (+ fix diff). Compare the
  results with `IMPORTANT_URL_BLOCKED_BY_ROBOTS` from the audit (live pages blocked by robots).
- `js-render-check` — raw HTML (view-source) vs rendered output: what appears only after JS.
  Required if Phase 0 found an SPA/Next.js, or if the crawl ran without JS Rendering.
- `silo-audit` — silo or not: clusters, hubs, depth, coverage (using `audit.json` + sitemap).
- `heading-outline` — complete H1–H6 structure (on key templates: home, category, product
  page/article — not on all 1,000 pages, but on one of each type + problematic pages from the
  audit).
- `security-audit` — security headers (HSTS/CSP/…), version leaks, cookie flags, http->https:
  `seohead security-check --url https://<domain>`. This affects trust and HTTPS ranking.
- `schema-graph` — structured data: page type, connected `@graph`, two-layer JSON-LD validation
  (vocabulary + rich results), and what is missing for the snippet. Run it on the same templates
  as heading-outline (home / category / product page / article):
  ```bash
  seohead schema-build --url https://<domain>/<template>   # type + proposed graph + diff
  seohead schema-check --url https://<domain>/<template>   # validate what already exists
  ```
  State the finding about the **template** ("product pages have no offers"), not about an
  individual URL.
- `duplicate-audit` — near-duplicate and thin pages: collect page text (from the SF crawl or
  sitemap+parse) and run `seohead duplicate-check --input '{...}'` (simhash + LSH). It finds
  duplicate clusters without pairwise comparison — critical for sites with thousands of pages.
- `geo-aeo-audit` — visibility in AI answers: `seohead ai-bots-check` (which robots.txt permits
  among GPTBot/ClaudeBot/Perplexity/Google-Extended) + `seohead llms-txt-check` (score for
  /llms.txt). Content citability is an editorial assessment by template.

**Phase 3 — Synthesis.** `sf-report` — a human-readable analysis of `audit.json`; merge the Phase
0 and Phase 2 findings into it. `sf-tasks` — a prioritized backlog; add tasks from agent-level
findings (robots fix, SSR/prerendering, completing silo clusters, heading hierarchy).

## Decision points
- **Scope is ambiguous** ("check the site," no further detail) vs explicitly
  narrowed ("only sitemap," "quick/lite") — default to MAX unless the user
  actually named a narrower scope; never ask permission to go full-depth.
- **Phase 0's `tech-detect` finds an SPA/Next.js/Nuxt stack** — this forces
  Phase 2's `js-render-check` to run (raw HTML will materially understate the
  page), rather than treating it as optional.
- **The audit comes back with many `skipped` results**
  (MIXED_CONTENT/STRUCTURED_DATA/SPELLING/DOM_*) — decide whether to pause and
  fix it via `sf-config` first (when full depth genuinely matters for this
  audit) or proceed and mark those checks "not checked" (when a fast turnaround
  matters more); never report a `skipped` check as if it were clean.
- **SF/a license is unavailable** — switch to Mode B (`--exports-dir`) rather
  than blocking the entire audit on Phase 1; request exports from the user if
  none exist yet.

## Definition of done
- [ ] Phase 0 reconnaissance (domain-profile, cdn-check, tech-detect) ran, or
  is explicitly marked "not checked" with a stated reason.
- [ ] Phase 1 produced `audit.json` + `audit.md` via `sf run`, unless the
  scope was explicitly narrowed to exclude crawl evidence.
- [ ] Every applicable Phase 2 check ran — robots-audit, js-render-check (if
  an SPA was detected), silo-audit, heading-outline, security-audit,
  schema-graph, duplicate-audit, geo-aeo-audit — or carries a stated reason
  for being skipped.
- [ ] Phase 3 synthesis exists: a readable report plus a prioritized
  `tasks.md`.
- [ ] All 8 items in "What to deliver to the user" below are populated, not
  partially filled.

## Cost
This orchestrator makes no calls of its own beyond what each phase invokes:
- **Phase 0:** 3 toolkit calls (`domain-profile`, `cdn-check`, `tech-detect`),
  each a handful of HTTP/DNS requests — seconds total, no paid API.
- **Phase 1:** delegates entirely to `sf-analyzer`. Mode B (`--exports-dir`) is
  offline/free and parses existing exports in seconds. Mode A (`--crawl`)
  requires a licensed SF CLI and takes as long as the crawl itself — minutes
  to hours depending on site size — but spends no per-request paid API, only
  the already-owned SF license's crawl time.
- **Phase 2:** one or two requests per agent-level check
  (`security-check`, `schema-build`/`schema-check` per template,
  `ai-bots-check`, `llms-txt-check`); `duplicate-check` is local computation
  over already-crawled text with no extra network cost.
- **Phase 3:** local report/task generation from `audit.json`, no network cost.

Overall: no paid API is touched anywhere in this pipeline; the only variable
cost is Mode A's crawl duration when a live SF crawl is requested.

## What to deliver to the user (one consolidated package)
1. **Site profile** (reconnaissance): domain/age, hosting/CDN, CMS/stack, risk flags.
2. **Health & critical:** health score, critical section (broken links with DOM localization,
   5xx, 4xx/5xx in the sitemap, important URLs blocked by robots).
3. **Rendering:** SSR or content/links/meta available only after JS (verdict + indexing risk).
4. **Structure:** silo / basic / extended + gaps; heading hierarchy.
5. **Sitemap & robots:** sitemap-to-crawl alignment, stale lastmod values, harmful directives.
6. **Security:** security headers (A–F grade), version leaks, http->https.
7. **Structured data:** page types, JSON-LD graph connectivity, dangling `@id` values, rich-result
   eligibility and missing required fields; an explicit note that FAQPage/HowTo no longer produce
   snippets (but remain useful for AI).
8. **Prioritized task plan** (`tasks.md`) + downloadable `audit.json`/`audit.md` files.

## Graceful degradation (without errors)
If SF is unavailable, use mode B (`--exports-dir`) if exports exist; if neither a licensed CLI
nor exports exist, that is not a reason to block the audit — hand off to `control`'s native
`crawl-site` pipeline instead of asking the user to install SF. If the network is unavailable
for reconnaissance/rendering, skip those phases and mark them "not checked" in the report; do
not crash. A `skipped` check is honest (no data/module), not zero.

## Integrations
Core: `sf-analyzer` · `sf-report` · `sf-tasks` · `sf-config`. Agent-level: `seo-recon` ·
`silo-audit` · `js-render-check` · `heading-outline` · `robots-audit` · `security-audit` ·
`schema-graph` · `duplicate-audit` · `geo-aeo-audit`.
"SF or elsewhere" router: `sf-boundaries`. Link profile check: `backlinks-check`.
