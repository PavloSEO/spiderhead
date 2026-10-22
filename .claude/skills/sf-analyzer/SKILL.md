---
name: sf-analyzer
description: >-
  Machine-readable SEO audit from a Screaming Frog crawl. Use when the user asks
  to analyze a .seospider file or Screaming Frog exports, find broken links and
  where they appear in the DOM (source, anchor, position, XPath), check a sitemap
  for stale lastmod values / 3xx-4xx entries / discrepancies with the site, detect
  oversized HTML pages (size anomalies), identify "two H1s: which ones and where,"
  or generate audit.json + audit.md for a site. Triggers: screaming frog,
  .seospider, SF export, internal_all.csv, sitemap audit, broken links, audit.json,
  crawl SEO audit.
---

# Export Analyzer — SEO Audit from Screaming Frog

Converts a single Screaming Frog crawl (19.x) into two machine-readable reports:
`audit.json` (a contract for downstream analysis/LLMs) and `audit.md` (for humans).
Extracts as much information as possible from SF panels, precisely locates links
(where each link appears, where it points, and where it is in the DOM), and adds
checks that SF itself does not flag (HTML size anomalies, sitemap discrepancies,
and stale `lastmod` values).

## Trigger
- "Run an SEO audit on this crawl / `.seospider` file / set of SF exports";
- "Find broken links and where they appear in the DOM";
- "Check the sitemap for stale lastmod values, 4xx/3xx entries, and discrepancies";
- "Which pages are abnormally large?" or "There are two H1s—which ones and where?";
- "Create an audit.json file for the site";
- keyword triggers: screaming frog, `.seospider`, SF export, `internal_all.csv`,
  sitemap audit, broken links, audit.json, crawl SEO audit.

## Anti-trigger
- Only one external check is needed (domain/hosting/CDN/stack) — use
  `seo-recon` instead; SF crawls the site itself, not who hosts it.
- The ask is *why* a `robots.txt` rule is harmful and how to fix it, not just
  which live URLs it blocks — `sf-analyzer` only reports the blocked-URL list;
  route the interpretation and fix to `robots-audit`.
- The ask is JS-rendered vs raw-HTML comparison — `sf-analyzer` only sees what
  SF's JS Rendering module captured during the crawl (see `sf-config`); it does
  not itself diff raw vs rendered output. Use `js-render-check` for that.
- The deliverable wanted is a human-readable narrative or a prioritized
  backlog, not the `audit.json`/`audit.md` pair — hand off to `sf-report` /
  `sf-tasks`, which consume this skill's output rather than duplicating it.
- There is no `.seospider` file, no export folder, and no domain/URL to
  crawl — nothing exists yet to feed in (see Preconditions).

## Preconditions
- [ ] Mode A: `seohead sf doctor` confirms a licensed SF CLI is discoverable
  (via `--sf-cli`, `$SF_CLI`/`$SCREAMINGFROG_CLI`, `config.json`, `PATH`, or a
  standard install directory) — otherwise fall back to Mode B.
- [ ] Mode A: an active SF license (headless crawling requires one).
- [ ] Mode B: an export folder exists with at least `Internal:All` exported
  from SF as CSV/XLSX.
- [ ] If module-dependent checks are expected (structured data, spelling, DOM
  depth, mixed content), `audit.seospiderconfig` exists per `sf-config` —
  otherwise those checks will legitimately come back `skipped`, not run.

## Two Input Modes
- **Mode B (default; SF not required):** a folder containing CSV/XLSX files
  exported manually from SF. Even `Internal:All` alone is sufficient. This is a
  self-contained workflow.
- **Mode A (requires an SF license + CLI):** the tool launches SF in headless mode
  for a `.seospider` file, domain, or URL list and exports the required
  tabs/bulk exports/reports.

> A `.seospider` file is Java-serialized data (it cannot be parsed directly). It
> requires either the licensed SF CLI (Mode A) or a manual CSV export (Mode B).

## Workflow
1. **Determine the input.** Get the path to a `.seospider` file or export folder,
   or get the domain. If it is unclear, clarify the mode (A/B) and profile
   (`lite`/`full`).
2. **Check the environment → locate SF.** Run `seohead sf doctor`; it searches for
   the SF CLI **everywhere**: the `--sf-cli` flag, the `$SF_CLI` and
   `$SCREAMINGFROG_CLI` variables, `config.json` (`sf_cli.path`/`search_paths`), the
   system `PATH`, and standard installation directories via glob patterns:
   `C:\Program Files*\Screaming Frog SEO Spider\*Cli.exe` (which catches both `(x86)`
   and versioned installations),
   `/Applications/Screaming Frog SEO Spider.seohead/sf/...`, `/usr/bin`, `/opt`, and
   `/snap`. If SF is found, use Mode A (`--crawl <domain>`). If it is not found,
   provide the path (`--sf-cli "...\ScreamingFrogSEOSpiderCli.exe"` or `SF_CLI=...`)
   or switch to Mode B (`reference/sf_cli_cheatsheet.md`). Mode A requires an SF
   **license**.
3. **Run the audit.** Full automated workflow from a domain (SF crawls the site →
   report → tasks):
   ```bash
   # Mode A — provide a domain and get everything (requires SF + a license)
   seohead sf run --crawl https://example.com --sitemap https://example.com/sitemap.xml --out report --tasks
   # From a saved crawl
   seohead sf run --load-crawl site.seospider --profile full --out report --tasks
   # Mode B — use existing exports (SF not required)
   seohead sf run --exports-dir ./exports --out report --tasks
   ```
   For MCP, use the `sf_audit_run` tool. The thin wrapper is
   `scripts/run_audit.sh`.
4. **Read `audit.json`.** Give the user the health summary first, followed by the
   critical section (broken links with DOM locations, and 4xx/5xx entries in the
   sitemap). On request, provide details through `sf_audit_issues` (filter by
   `check`/`severity`).
5. **Attach the files** `audit.json` + `audit.md` for download.

Related skills: **sf-report** provides a human-readable report analysis;
**sf-tasks** provides a prioritized backlog (`seohead sf tasks` / MCP
`sf_audit_tasks`). MCP tools: `sf_audit_run`, `sf_audit_summary`, `sf_audit_issues`,
`sf_list_exports`, `sf_audit_tasks`.

## Decision points
- **A `.seospider` file is provided but no SF CLI is available** — it cannot
  be parsed directly (Java serialization); switch to Mode B via a manual CSV
  export instead of attempting to read the binary file.
- **Many checks return `skipped` rather than `0`** — that means the SF module
  producing them was disabled, not that the site is clean. Route the user to
  `sf-config` instead of reporting a false-positive clean bill.
- **The mode/profile is unclear from the request** — clarify (A vs B,
  `lite` vs `full`) before running rather than guessing: re-running a full
  crawl is costly (Mode A), and guessing wrong in Mode B means asking for a
  different export afterward anyway.
- **`audit.json` exists but the user wants prose or a backlog** — hand off to
  `sf-report`/`sf-tasks` rather than writing that narrative from this skill;
  they are built to consume this skill's exact output contract.

## Definition of done
- [ ] Both `audit.json` (machine contract) and `audit.md` (human report) exist
  for the given input.
- [ ] The health summary and critical section (broken links with DOM
  location, sitemap 4xx/5xx entries) were surfaced before any deep-dive detail.
- [ ] Every check reported as `skipped` is called out explicitly as "module
  not enabled," not silently omitted from the summary.
- [ ] Both files are attached/available for the user to download.

## Cost
- **Mode B (`--exports-dir`):** offline and free — parses local export files
  already on disk, no network requests, seconds regardless of site size.
- **Mode A (`--crawl`):** requires a licensed SF CLI; the cost is the crawl
  itself — duration scales with site size (minutes to hours), and it consumes
  the already-owned SF license's crawl time, not a per-request paid API.

No paid API is touched by this skill in either mode.

## Interpretation
- Check registry and severity levels: `reference/checks.md`.
- `audit.json` contract: `reference/json_schema.md`.
- Exact SF CLI commands: `reference/sf_cli_cheatsheet.md`.

Load files from `reference/` as needed (progressive disclosure) to avoid bloating
the context.
