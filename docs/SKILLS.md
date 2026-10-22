# Skill map

26 skills in `.claude/skills/`, organized as method skills, an entry controller, and one versioned
full-audit contract.

**Method skills** — 23 of them. Each covers one thing well: when to apply it, in what
order, how to read the result, and where the boundary is beyond which the tool starts to lie.

**The controller** — `control/`, which decides *which* method skill to run on a site nobody has
looked at yet, and whether to believe the answer. The versioned contract — `full-audit-v1/`
(`workflow/full-audit-v1`) — fixes the reusable audit sequence without adding checks. Both route
rather than restating methods; `control/` also carries its own sub-skills and reference archive:

```
.claude/skills/control/
  SKILL.md                     the entry point: the loop, the decision points, the cost
  subskills/scoping.md         how big is this site, what runs on it, what can be skipped
  subskills/rate-and-load.md   what rate a host tolerates, and whose fault the errors are
  subskills/reading-an-audit.md  fired vs skipped vs silent; when a score is not earned
  subskills/verifying.md       confirming a finding live before it reaches anybody
  subskills/deliverables.md    turning findings into a task, an archive, a document
  reference/defects.md         bugs found on live sites, and what gave each one away
  reference/populations.md     which set each check describes, and its invalid comparisons
  reference/limits.md          what this toolkit cannot answer at all
```

Retrieve the full-audit contract with `seohead skill-show --name workflow/full-audit-v1` or
`seo_skill_show(name="workflow/full-audit-v1")` over MCP. The v1 identifier stays addressable;
an incompatible workflow gets a new versioned ID.

Each sub-skill is loadable on its own: a reader who needs only the rate lesson should not have
to read the deliverables section. The reference archive matters as much as the sub-skills —
every defect found on a live site so far was recognisable by a pattern, and writing those down
is what lets the next run catch one in minutes instead of an afternoon.

## Discover installed guidance

`seohead skill-list` returns both `workflow/` technical/controller playbooks and the packaged
`general/` SEO/content methods. Retrieve one with `seohead skill-show --name workflow/control`;
use `scenario-show` for a concrete sequence. These calls return guidance, not execution.
An installed wheel reads entry playbooks and scenarios from the generated catalogue. Supporting
files linked by a playbook (such as `control/subskills/scoping.md`) are currently checkout-only:
open them from a matching repository revision. `skill-show` does not retrieve arbitrary reference
paths. If that checkout is unavailable, state the missing guidance and use the available full-audit
contract within its explicit scope; do not pretend the supporting reference was read.

A skill is useful when it makes a decision the command schema cannot: scope, reuse, interpretation,
review or handoff. The generated "commands not named" inventory below measures text references,
not method quality or feature availability. [The capability map](README.md#capability-map) also
routes workflows whose contracts live in docs instead of a dedicated skill.

For a full audit and developer Excel, use `control` → `full-audit-v1` → `sf-tasks` and
[the deliverable scenario](scenarios/deliverable.md). Native audits and SF audits can both feed
the task pipeline; the `sf` command group is a compatibility name, not a licence requirement.
The agent authors and reviews engineering acceptance criteria; the report formatter supplies facts.

## How to choose

```
given a domain, what to do?
   └─ control ────────── the entry point for any unscoped audit request: scope,
        │                 and route to the versioned full-audit-v1 contract
        ├─ full-audit-v1 ─ run one budgeted collection, reuse evidence, verify,
        │                    and report actual coverage and unavailable work
        ├─ seo-deep-audit ─ delegate here for the crawl step instead of native
        │                    crawl-site only when SF (licensed CLI or exports)
        │                    is available and full-registry depth is wanted
        └─ audit-roadmap ─ when the domain is new: scout the minimum
                            first and decide what to collect
```

Then by the layer of the task.

## Orchestrators

| Skill | When |
|---|---|
| **control** | The single entry point for an unscoped "audit this site" request, or when you are about to write a one-off script to check pages. The whole loop: scope, crawl, `log-scan` the run, read `audit.json`'s honesty fields before its findings, verify criticals live, build the deliverable. Routes to the method skills below rather than restating them; carries its own sub-skills and reference archive. Scope and verification must be established for the current run |
| **full-audit-v1** | Versioned, addressable execution contract used by `control`: scope and budget gates, evidence reuse, existing collector/method routes, verification, honest coverage and report completion. Adds no checks of its own |
| **seo-deep-audit** | Not a second unscoped-audit entry point — `control` delegates its crawl step here when a licensed SF CLI or supplied exports are available and full-registry depth is wanted, and it is also fine to call directly once that decision is already made (SF/exports named or already in hand) |
| **audit-roadmap** | Unfamiliar domain: 5 minutes of recon to decide what to collect next |
| **sf-boundaries** | The fork "does Screaming Frog cover this, or does it need an agent?" — a router |

## Screaming Frog crawl audit

| Skill | When | Tool |
|---|---|---|
| **sf-analyzer** | There is a crawl or exports — produce a machine-readable audit | `sf run` |
| **sf-config** | Configure SF once to maximize applicable coverage from the 280-check registry | — |
| **sf-report** | Turn the export into a human-readable report | `sf run --out` |
| **sf-tasks** | Build a prioritized backlog from `audit.json` | `sf tasks` |

## Recon and technical hygiene

| Skill | When | Tools |
|---|---|---|
| **seo-recon** | Domain age, hosting, CDN, caching — everything SF does not give | `domain-profile`, `cdn-check` |
| **tech-audit** | What the site is made of: CMS, framework, analytics, pixels | `tech-detect` |
| **security-audit** | Security headers through an SEO lens | `security-check` |
| **robots-audit** | robots.txt dissected for harmful directives | `robots-check` |
| **js-render-check** | What appears only after JavaScript + lab metrics | `render-check` |
| **regional-audit** | Regions: subdomains, folders, satellites, branches across Russia | `regions-check` |

## Content and structure

| Skill | When | Tools |
|---|---|---|
| **schema-graph** | Structured data: dissect, validate, build a `@graph` | `schema-check`, `schema-build` |
| **duplicate-audit** | Near-duplicates and thin pages | `duplicate-check` |
| **heading-outline** | The H1–H6 structure and its hierarchy | `parse` |
| **silo-audit** | Is the structure silo-like, hubs, interlinking, orphans | `links-check`, `sitemap-crawl` |
| **internal-linking** | Is the site linked well: click depth from the start URL, edges by position, repeated edges, link placement | `crawl-site`, `sf run` |
| **backlinks-check** | Verify links against your own donor list | `backlinks-check` |
| **geo-aeo-audit** | Visibility in AI answers: crawlers, llms.txt, citability | `ai-bots-check`, `llms-txt-check`, `citability-check` |

## The audit as a whole and reports

| Skill | When | Tools |
|---|---|---|
| **site-report** | The whole site dissected and a ready file — Excel, Word, CSV | `site-audit`, `report-build` |
| **traffic-report** | Monthly or annual client report from Metrika, Webmaster and GSC instead of Looker: search, sections, products, conversions as visits, buyer portrait | `metrika-traffic-pdf`, `crawl-site`, `provider-collect` |

## Analytics consoles and exports

| Skill | When | Tools |
|---|---|---|
| **analytics-console-review** | A user-authorized signed-in console or aggregate export is available, but no provider API is configured | Host browser or user export; optional `sources-doctor`, `metrika-report`, and page/SF checks |

## Packaged general skills

`seohead skill-list` also returns these seven `general/` SEO and content methods, packaged in
`seohead/skills/`. They are guidance for writing and content work, not audit routes.

| Skill | When |
|---|---|
| **article-writer** | A bilingual RU+EN expert article in JSON for a blog |
| **seo-audit-page** | A complete on-page and technical audit of one page or a site |
| **seo-content** | SEO articles, GEO-oriented copy, and E-E-A-T review before publication |
| **seo-markup** | Schema.org JSON-LD and meta tags (title, description, Open Graph, Twitter Cards) |
| **seo-optimize** | Refreshing outdated content and improving internal linking |
| **seo-research** | Keywords, competitors, SERP analysis, and content gaps |
| **service-landing-copy** | Structured copy for a `/services/<name>` landing page |

## Packaged pipeline skill

`semantic-core` is packaged alongside the general skills because it documents the repository's
accumulating semantic-core pipeline rather than general editorial work. It is discoverable through
`seohead skill-list` and `seohead skill-show --name general/semantic-core`.

## Tools without a skill of their own

183 of the 231 commands are not named in any skill body.

Commands without their own skill:
`ai-column` · `asset-weight-check` · `audit-workflow` · `backlinks-disavow` · `bi-bigquery-plan` · `bi-destination-apply` · `bi-export` · `bi-filter` · `bi-sheets-plan` · `boilerplate-report` · `cloudflare-history` · `cloudflare-instant-logs` · `cloudflare-logpush-import` · `cloudflare-traffic` · `compare-read` · `compare-report` · `compare-status` · `compare-url` · `crawl-control` · `crawl-describe-settings` · `crawl-diagnose-export` · `crawl-enrich` · `crawl-import` · `crtsh-subdomains` · `crux-report` · `custom-extract-sample` · `dataforseo-estimate` · `doctor` · `evidence-join` · `evidence-normalize` · `facts-export` · `findings-view` · `google-keywords` · `google-serp` · `gsc-archive` · `gsc-progress` · `gsc-query` · `headers-analyze` · `hreflang-check` · `images-download` · `images-optimize` · `indexnow-submit` · `inspect-url` · `keywords-cluster` · `keywords-exact` · `keywords-expand` · `keywords-seasonality` · `log-analyze` · `logs-bots` · `logs-import` · `logs-join` · `logs-summary` · `marketing-inventory` · `meta-description-drafts` · `miratext-analyze` · `mirror-check` · `monitor-collect` · `monitor-configure` · `monitor-local-deliver` · `monitor-run` · `monitor-schedule` · `monitor-status` · `project-action-log` · `project-activity` · `project-agent-heartbeat` · `project-archive` · `project-checklist-init` · `project-checklist-page` · `project-competitors-add` · `project-competitors-list` · `project-competitors-remove` · `project-competitors-scan` · `project-competitors-scan-cancel` · `project-competitors-scan-status` · `project-delete` · `project-event-append` · `project-event-page` · `project-export` · `project-export-cancel` · `project-export-status` · `project-exports-page` · `project-facts` · `project-inbox-acknowledge` · `project-inbox-list` · `project-inbox-read` · `project-inbox-submit` · `project-inbox-unread` · `project-new` · `project-observe` · `project-open` · `project-policy` · `project-priorities` · `project-restore` · `project-scans` · `project-settings` · `project-settings-proposals` · `project-settings-propose` · `project-settings-review` · `project-sources-link` · `project-sources-list` · `project-sources-unlink` · `project-start` · `project-status` · `project-task-comment` · `project-update` · `project-view-delete` · `project-view-list` · `project-view-rename` · `project-view-save` · `project-view-show` · `provider-artifact-read` · `provider-auth` · `provider-collect` · `provider-credential` · `provider-discover` · `provider-join` · `provider-readiness` · `provider-registry` · `provider-replay` · `provider-verify` · `publication-cohorts` · `redirects-check` · `redirects-generate` · `regions-tree` · `remediation-cases` · `remediation-create` · `remediation-ingest` · `remediation-recheck` · `remediation-record-verification` · `remediation-report` · `remediation-summary` · `remediation-transition` · `scan-body-diff` · `scan-body-read` · `scan-content-search` · `scan-content-search-page` · `scan-directory-tree` · `scan-evidence` · `scan-export` · `scan-extract` · `scan-findings-page` · `scan-fragment-links` · `scan-graph` · `scan-import-urls` · `scan-link-inspect` · `scan-list` · `scan-navigation` · `scan-pin` · `scan-prune` · `scan-rendered-routes` · `scan-requeue` · `scan-sitemap-generate` · `scan-snapshot` · `scan-structured-blocks` · `scan-url-detail` · `scan-url-history` · `scan-url-query` · `scenario-list` · `schedule-install` · `schedule-run-due` · `schedule-set` · `schedule-show` · `schedule-uninstall` · `segment-diff` · `semantic-inputs` · `semantic-similarity` · `serp-fetch` · `soft404-check` · `sources-export` · `sources-status` · `sources-sync` · `tool-catalog` · `topvisor-read` · `update-check` · `update-download` · `url-check-batch` · `verify-fixes` · `wayback-history` · `webmaster-url-queries` · `workflow-checkpoint` · `workflow-execute` · `workflow-resume` · `workflow-status`
## Skill rules

**Where a skill lives.** A general method applicable to any project ->
`~/.claude/skills/`. Knowledge about this repository -> `.claude/skills/`
here.

**Skills must age together with the code.** A new tool appears — the skill
that used to teach doing the same by hand gets rewritten. That is how
`js-render-check` stopped explaining `curl` and headless Chrome and started
documenting `render-check`.

**Every skill has a "Boundaries" section.** What the tool does not do and
what cannot be claimed from its output. Without it a skill turns into an
ad for the tool.

**Consistency check** — `tests/test_docs_drift.py`: every `seohead` command
mentioned in any skill must exist in the CLI. A skill referencing a
non-existent command fails the suite.
