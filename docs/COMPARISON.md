# How SEOHEAD fits into a technical SEO stack

SEOHEAD Tools supports SEO specialists and engineers working with AI agents. Its native Python
crawler, retained evidence, analysis, task tracking and report formats form a configurable loop:
**goal → collection/extraction policy → analysis → comparison → tasks → recheck → report → refinement**.

## Canonical product description

> SEOHEAD is a local SEO engineering toolkit for specialists working with AI agents and agent
> systems. Configure evidence collection and extraction, investigate technical project health,
> turn findings into developer work, verify changes, and repeat from retained project evidence
> through Python, CLI and local MCP interfaces.

The native crawler is a first-class collector; supplied Screaming Frog CSV/XLSX exports and a
separately licensed SF CLI are distinct supported inputs. Other crawler exports require the
[declared import contract](THIRD_PARTY_CRAWL_IMPORT.md), not assumed compatibility.

## Compare a complete workflow

Use the same target or fixture, agreed URL/template population, configuration, representation and
resource budget in both products. Keep unavailable evidence visible. These dimensions reveal more
than a command total or an unsupported percentage of "SEO tasks automated":

| Dimension | Inspect in SEOHEAD | What a comparison must demonstrate |
|---|---|---|
| Configurable collection | [source-derived settings](HEADLESS_CAPABILITIES.md) | Which URLs/resources were admitted, excluded, rendered and retained under the selected policy |
| Extraction and analysis | [checks](CHECKS.md), [input contracts](INPUTS.md) | Correct facts for representative templates, rule thresholds and missing-data behavior |
| Repeatability | [storage](STORAGE.md), [comparison](COMPARE.md) | Evidence reuse, provenance, compatible deltas and explicit partial/incompatible states |
| Engineering handoff | [developer deliverable](scenarios/deliverable.md) | Inspectable tasks, full affected population, evidence, proposed changes and reviewed acceptance criteria |
| Work and repair tracking | [projects](PROJECTS.md), [ledger](LEDGER.md) | Scoped task progress separately from finding resolution, review gates and evidence-bound rechecks |
| Reporting and integrations | [reports](examples/reports/README.md), [providers](PROVIDERS.md), [BI](BI.md) | Requested output actually produced, provider grain/availability preserved, destination validated if used |
| Operations and capacity | [platforms](PLATFORMS.md), [capacity profile](SCAN_CAPACITY_PROFILE.md) | Complete retained runtime evidence for the actual build, workload and environment |

The native SQLite crawler admits explicit configurations up to 1,000,000 URLs. This is an
admission bound, not a benchmark showing every such workload completes. Materialized legacy
APIs retain a separate 50,000-URL safety bound. Million-URL end-to-end delivery and
full feature/performance parity remain unaccepted; the [capacity record](SCAN_CAPACITY_PROFILE.md)
names the measured limits. Configurability means supported settings, views and policies, not an
arbitrary plugin runtime or autonomous code rewriting.

## Where it is strong

### One local interface for an agent

The CLI and MCP server share the same 234 handlers, and six additional Screaming Frog-specific
MCP tools cover crawl audits and bounded snippet review. A registration test prevents a command
from existing in only one interface.

### Release review across a declared URL migration

`compare-crawls` compares exact URLs by default. A changed path or host is not silently paired by
title, canonical, content, or a redirect guess. For a release that intentionally moved URLs, pass
an explicit closed `url-correspondence.v1` JSON document:

```json
{
  "schema_version": "url-correspondence.v1",
  "origin_map": {
    "https://before.example.test": "https://after.example.test"
  },
  "pairs": [
    {
      "before": "https://before.example.test/legacy%2Fpage?view=full",
      "after": "https://after.example.test/replacement?view=full"
    }
  ]
}
```

`origin_map` preserves each remaining raw path, encoded character, query, and fragment. Explicit
`pairs` override that path when a page moved differently; repeated source or destination pairs,
unknown keys, and incomplete objects are refused. The result has an inspectable
`release_review` section with schema `release_review.v1`: declared/matched/missing pairs, both
URLs and origins, host-change flags, source and coverage state for status, title, description,
H1, canonical, and robots directives, plus the same finding deltas. A mapped host is reported as
a host change; it is never rewritten away. Missing pages, partial crawls, and incompatible policy
or configuration evidence retain their normal warnings and classifications.

### Deep analysis of existing crawl data

Export mode evaluates Screaming Frog CSV/XLSX data against a 282-check registry without crawling
again. It is useful when the crawl was taken by another specialist, came from CI, or must remain
offline. Missing exports become explicit skipped checks rather than silent zeroes.

### Evidence beyond a crawler

Live tools add DNS/RDAP/TLS, cache behavior, technology markers, security headers, mirror
canonicalization, AI crawler access, regional structure, rendering differences, log analysis,
Schema.org graphs, and optional demand/traffic data.

### Structured deliverables

`site-audit` runs a bounded sitemap-based pass: ten site-level tools and three page-level tools,
with 25 selected pages by default. It assembles one document and records individual tool failures;
it is not an exhaustive run of the catalog or a link-graph crawl. `report-build` formats existing
evidence as XLSX, DOCX, CSV, Markdown, JSON or optional offline PDF without recalculating findings.
A grouped task backlog is a separate `sf tasks` output from native or SF audit evidence; a reviewed
developer workbook additionally needs the agent's engineering interpretation and artifact authoring.

## Where another tool is the right choice

| Need | Use instead or alongside SEOHEAD | Reason |
|---|---|---|
| Crawl a large or JavaScript-heavy site | Evaluate native `crawl-site` against the required discovery, rendering and resource policy; use another collector where a verified gap remains | Full feature and scale parity requires acceptance evidence; no universal size or speed claim is made |
| Discover a domain's full backlink profile | Ahrefs, Majestic, Semrush, GSC, or another index | `backlinks-check` verifies a donor list; it owns no web index |
| Field Core Web Vitals | CrUX, Search Console, or PageSpeed Insights | `render-check` records one lab run and labels it as lab data |
| Search volume, rankings, and SERP history | Wordstat, Arsenkin, DataForSEO, or another provider | These are external datasets, not facts code can derive |
| Machine translation | A reviewed translation model or professional localization workflow | SEOHEAD audits international structure and hreflang; it does not claim a translation engine |
| A hosted multi-user dashboard | A SaaS SEO platform | SEOHEAD is deliberately headless and local |
| Automatic production changes | A reviewed deployment/CMS workflow | SEOHEAD produces evidence and files; it does not deploy fixes |

## Screaming Frog boundary

Export analysis works with files you already have. Live crawl mode launches a separately installed
Screaming Frog CLI and requires an active paid SEO Spider licence. The toolkit does not bundle,
activate, or bypass Screaming Frog. Native crawling works independently of its licence; full
Screaming Frog feature parity is not claimed.

## Interpretation boundary

Heuristics are labelled as heuristics. Lab data is not field data. A missing provider row is not
zero demand, and a failed tool is not a clean result. Final recommendations still require a
specialist to understand business intent, templates, release risk, and the cost of implementation.
