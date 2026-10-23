---
name: audit-roadmap
description: >-
  Planner: given a domain, performs minimal reconnaissance in 5 minutes, then produces an informed
  roadmap specifying exactly what to collect and in what order BEFORE running a heavy audit. It
  does not perform the audit; it scopes it: site size (number of URLs), stack (whether JS rendering
  or CWV analysis is needed), type (store/blog/services/corporate), what is critical for this
  particular site, what can be skipped (YAGNI), and which access levels will be required. Triggers
  on: "create an audit plan," "roadmap for a domain," "what to collect from the site," "analysis
  plan," "what to check on the site," "where to start an audit," "scope the collection," "audit
  plan," "scoping," "audit roadmap," "what to analyze."
---

# Audit Roadmap — planner: domain -> what to collect

The `seo-deep-audit` orchestrator immediately runs everything. This skill operates one step
earlier: use quick reconnaissance to create a plan for **exactly what** to collect for a specific
site, so that expensive work is not run unnecessarily and critical issues are not missed. The
terms-of-reference principle is: "rushing is prohibited" — plan first, collect second.

## Trigger
- You were given a domain and asked to analyze it, but the scope/focus is unknown.
- You need a work plan and estimate before starting (for yourself, a client, or a team).
- Before `seo-deep-audit`: determine which phases are needed and which should be skipped.
- The request is phrased as: "create an audit plan," "roadmap for a domain," "what to collect from
  the site," "analysis plan," "what to check on the site," "where to start an audit," "scope the
  collection," "audit plan," "scoping," "audit roadmap," or "what to analyze."

## Anti-trigger
- The site has already been scoped and the ask is to actually run the checks — go straight to
  `seo-deep-audit` (execution) instead of re-planning.
- The user wants one specific technical check right now (robots, schema, duplicates, backlinks)
  rather than a full plan — jump directly to that skill (`robots-audit`, `schema-check`,
  `duplicate-audit`, `backlinks-check`, etc.); building a roadmap first is pure overhead.
- There is no domain or site to scope at all (pure keyword research, content writing, or a
  non-technical question) — this skill only scopes technical/content audits of an existing site.
- The user needs the "SF or toolkit" routing decision itself (which crawl mode to use) — that
  belongs to `sf-boundaries`, not this skill.

## Preconditions
- [ ] A domain or live URL is available to run the five Phase 0 recon commands against, or the user
  has explicitly said the network/site is unavailable and supplied domain/access/objective details
  instead.
- [ ] The audit's objective is known well enough to place the site in a type bucket (e-commerce /
  blog-media / services-local / corporate-SaaS / portal-UGC) — ask if genuinely unclear rather than
  guessing.
- [ ] Access-level expectations (L1 source, L2 server/logs, L3 database, L4 CDN) have been asked
  about or are already known, so the roadmap can mark what is and is not checkable.

## Workflow

### Phase 0 — Recon lite (5 commands, a few minutes)
Quick reconnaissance without a crawl. Each item is a single call:
```bash
seohead domain-profile --domain <domain>     # registration, hosting, ASN, TLS expiry, flags
seohead tech-detect     --url https://<domain>   # CMS, framework, server, analytics
seohead robots-check    --url https://<domain>   # what is allowed, sitemap location, junk directives
seohead sitemap-crawl   --url https://<domain>/sitemap.xml   # scale — number of URLs in the sitemap
seohead ai-bots-check   --url https://<domain>   # which AI crawlers are allowed
```
If the sitemap was not found through robots, try `/sitemap.xml` and `/sitemap_index.xml`. If the
network or site is unavailable, work with what the user provided (domain, access, objective).

### Phase 1 — Interpret signals as categories
Use the Phase 0 output to determine four dimensions — they shape the entire roadmap:

| Dimension | Signals | Decision |
|---|---|---|
| **Scale** | sitemap: <500 / 500–5,000 / 5,000–50,000 / >50,000 URLs | small -> full analysis; medium -> templates; large -> Dynamic Sampling, near-duplicate analysis required |
| **Stack** | `tech-detect`: SPA (React/Vue/Next/Nuxt) vs server rendering (WP/Bitrix/static) | SPA -> JS rendering (raw vs DOM) + CWV required; static -> JS rendering optional |
| **Site type** | based on URLs/content: e-commerce / blog-media / services-local / corporate / SaaS / portal | each type -> its own schema set and common issues (see Phase 2) |
| **Access and health** | TLS expiry <30 days, security D–F, noindex in robots, no llms.txt, retrieval bots blocked | include these in the roadmap's critical tasks |

### Phase 2 — Roadmap: what to collect (by priority)
Build the roadmap according to site type and scale. Use the baseline set (for everyone) plus the
type-specific items.

**Baseline set (any site):**
- SF crawl evidence (mode A or B), maximum applicable coverage from the 283-check registry ->
  `audit.json`/`tasks`; inspect `checks_skipped` instead of assuming every check ran.
- `security-audit`, `robots-audit`, `silo-audit`, `heading-outline` — on key templates.
- `schema-graph` (`schema-build` + `schema-check`) — on one page of each type.

**Type-specific priorities:**
- **E-commerce:** Product/Offer/AggregateRating schema on product pages; duplicates and
  cannibalization in the catalog; pagination and filters (canonical vs noindex); thin product
  pages; `duplicate-check`.
- **Blog / media:** Article schema (headline/dates/author); near-duplicate and thin content
  (`duplicate-check`); article dates; category silos; orphan pages.
- **Services / local business:** Service/LocalBusiness schema; city/service pages; NAP
  consistency (address/phone); reviews/rating.
- **Corporate / SaaS:** Organization/WebSite; product/pricing/docs pages; `llms-txt-check` and
  `citability-check` (if there are GEO objectives).
- **Portal / UGC / large site:** Dynamic Sampling (not a complete manual review), critical
  `duplicate-check`, and the template-based approach from the mega-audit terms of reference.

**Scale >10,000 pages:** explicitly include the "template-based analysis" from the terms of
reference (home / category / product page / article / utility pages — 1 reference example + 3–5
random examples + problematic pages from the crawl).

### Phase 3 — Order, resources, and access
- **Cheap and critical first:** reconnaissance flags (TLS/security), broken links, noindex
  conflicts.
- **In parallel:** reconnaissance profile, sitemap, and robots while the crawl is running.
- **Access levels:** indicate what requires L1 (source code), L2
  (server/logs), L3 (database), and L4 (CDN), and whether those access levels are available. If
  access is unavailable, record the item as "not checked"; do not invent a result.
- **Dependencies:** no SF/license -> mode B (exports) or request them; no Playwright -> postpone
  JS rendering and CWV.

### Phase 4 — YAGNI: what to skip and why
Clearly identify what this site does NOT need (with a reason), so the work does not expand
unnecessarily:
- Static site without JS and no rendering complaints -> JS rendering can be skipped (or limited
  to one sample).
- No GEO/AEO objectives -> minimize or skip `llms-txt-check`/`citability-check`.
- Small site (<100 pages) -> `duplicate-check` is excessive; a complete manual review is more
  practical.
- No rich-result ambitions -> reduce deep schema analysis to error checking.

## Decision points
- **Scale reading.** A sitemap in the 500–5,000 range could still warrant a full manual review or
  need templating — decide based on how repetitive the URL patterns actually look (few templates ->
  full review; many near-identical templates -> template-based sampling), not on the raw count
  alone.
- **Ambiguous stack signal (client-side hydration on a server-rendered site).** Check
  `tech-detect`'s raw output for which frameworks actually power the key templates, not just the
  presence of a JS bundle, before deciding JS rendering + CWV are required.
- **No SF license or no Playwright available.** Fall back to mode B (exports) or explicitly
  postpone JS rendering/CWV rather than silently dropping them from the roadmap — the missing
  dependency must show up in "Missing access," not just vanish.
- **YAGNI calls (Phase 4).** Each skip (JS rendering, GEO checks, `duplicate-check` on a small
  site) needs a reason tied to this site's actual profile, not a default; when in doubt, list the
  item as optional instead of dropping it silently.

## Definition of done
- [ ] Phase 0 recon commands were run, or explicitly marked unavailable with a stated reason.
- [ ] All four Phase 1 dimensions (scale, stack, site type, access/health) are stated together with
  the signal that produced each.
- [ ] The roadmap lists a tool for every collection item, in priority order, split into the
  baseline set and type-specific items.
- [ ] Missing access levels (L1–L4) are called out by name, not silently assumed available.
- [ ] A YAGNI section names what is skipped and why, specific to this site.
- [ ] A rough time/agent-count estimate is included.

## Cost
Phase 0 is five lightweight commands (`domain-profile`, `tech-detect`, `robots-check`,
`sitemap-crawl`, `ai-bots-check`), each a single request against the target site, no paid API,
well under a minute of network time total. The roadmap itself is then pure reasoning and write-up —
no further tool cost — until it is handed to `seo-deep-audit`, whose per-phase execution cost is
outside this skill's scope.

## What to deliver to the user (the roadmap as one document)
1. **Site profile** (from reconnaissance): domain/age, hosting/CDN, CMS/stack, TLS/security flags.
2. **Scale and strategy:** number of URLs, full analysis / templates / sampling.
3. **What to collect** — a prioritized list with a tool for every item.
4. **Order** and parallelism, including what comes first.
5. **Missing access** (L1–L4) and what cannot be checked without it.
6. **YAGNI** — what to skip and why.
7. **Estimate** (roughly: hours/days, whether agents are needed and how many).

## Graceful degradation
If the network/site is unavailable, create the roadmap from what the user provided (domain,
objective, access), marking items as "check when access first becomes available." If there is no
sitemap, estimate scale from robots, site structure, and the client's description. Put every
uncertainty on a separate "clarify with the client" line; do not make assumptions.

## Next steps
Once the roadmap is ready, pass it to `seo-deep-audit` (execution item by item) or execute the
requested parts. Integrations: signal sources — `seo-recon`, `robots-check`, `sitemap-crawl`,
`tech-detect`, `ai-bots-check`; execution — `seo-deep-audit`; "SF or toolkit" router —
`sf-boundaries`.
