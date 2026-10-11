# AGENTS.md — public repository contract

SEOHEAD Tools is a headless, local-first evidence and audit-automation layer for SEO specialists
and tool-calling agents. The `seohead` CLI and one local stdio MCP server remain the default
interfaces. An optional, authenticated remote scan API may reuse the same core only with an
explicit job backend and target/egress policy; it must never expose anonymous submission or
silently enable a listener. A planned SEOHEAD Desktop (PyQt5) is a native presentation adapter over the same core; its specification is in `desktop/docs/spec/`, and this repository does not yet ship it. A hosted account, web dashboard and remote MCP endpoint
remain outside this repository's current delivery boundary.

## Product model

Present SEOHEAD as a native Python SEO crawler and scan-analysis toolkit, with a complete
headless crawl -> retain -> analyze -> reanalyze -> compare -> report workflow. The native
crawler is a first-class collector, not merely a fallback when Screaming Frog is unavailable.

- The product direction is full crawling and analysis capability, including JavaScript-heavy
  sites, through the CLI and local MCP. Full feature or performance parity with other crawlers
  must be demonstrated before being described as shipped.
- Screaming Frog export analysis remains a supported input. Do not imply that another crawler's
  exports are drop-in compatible with the SF analyzer.
- Resource budgets, safe network behavior, provenance, and skipped/failed measurements remain
  required. Bounded execution is resource control, not a permanent restriction to small sites.
- The CLI and MCP expose the same tested core. Skills guide orchestration and interpretation;
  they are not evidence sources, and final judgement remains with the specialist.

Live SF mode launches a separately installed, actively licensed Screaming Frog CLI. Export mode
works from existing CSV/XLSX files. The `site-audit` command is a bounded sitemap-based pass, not
an exhaustive run of the tool catalog and not a general-purpose crawl.

## Start here

```bash
python -m pip install -e ".[dev,mcp,cluster,reports]"
ruff check .
ruff format --check .
pytest -q
seohead sf run --exports-dir docs/examples/exports --out /tmp/seohead-report --tasks
```

## Architecture

```text
seohead/
  cli/            CLI registration and argument mapping
  checks/         live page, content, image, log, and structured-data checks
  mcp/            shared handlers, MCP server registration, and SF MCP tools
  integrations/remote_api/  optional authenticated scan API (ASGI app, no listener)
  recon/          domain and infrastructure reconnaissance
  crawl/          native site collector (crawl-site) — no Screaming Frog required
  sf/             Screaming Frog export runner and 229-check analyzer, shared with crawl/'s output
  audit/          bounded sitemap-based evidence orchestration
  core/           shared models, job contracts, and run logs
  reports/        XLSX, DOCX, CSV, Markdown, and JSON formatting
  data_sources/   optional demand, SERP, and traffic providers
  semantics/      optional semantic-core pipeline with its own SQLite DB (extra `semantics`)
  tui/            terminal UI
  skills/         packaged SEO workflow playbooks
```

The core (`checks`, `recon`, `sf`, `audit`, `data_sources`) does not import `cli`, `mcp`,
`integrations/remote_api` or `tui`.
Public behavior starts in the core, receives a shared handler, and is then registered in both the
CLI and MCP. `tests/test_interface_binding.py` enforces the core import boundary, and
`tests/test_registration.py` enforces CLI/MCP registration parity.

## Invariants

- Missing data is not a clean result. Report skipped or unavailable checks with a reason.
- Network failures are result data at tool boundaries, not process crashes.
- User-controlled URL requests use `seohead.recon.net.http_client`; private networks are blocked
  unless `SEOHEAD_ALLOW_PRIVATE_NETWORKS=1` is set deliberately, or the target host is named in
  `SEOHEAD_ALLOW_PRIVATE_HOSTS` to authorize just that one staging host.
- Side effects are explicit. File mutation, service-path probing, DNS bot verification, provider
  production mode, and paid calls never hide behind defaults.
- Image optimization requires an output directory unless `in_place=true`; backups are enabled for
  in-place rewrites and format conversion never deletes the source.
- DataForSEO defaults to sandbox and its geographic coverage guard must not be bypassed.
- Yandex SERP uses the asynchronous endpoint only.
- Provider secrets come from environment variables or local configuration; never print values.
- Metrika log exports may contain personal identifiers and must not be committed or placed in
  client reports.
- Report renderers format an existing audit document; they do not calculate new findings.
- Screaming Frog export mode must remain usable without live crawl mode. Live crawl mode requires
  a separately installed and licensed Screaming Frog CLI.
- Public comments, docstrings, errors, documentation, examples, and skills are English. Localized
  dictionaries and explicit international-SEO fixtures may contain non-English data when required
  by the feature.

## Public boundary

Never commit credentials, client URLs or exports, access logs, `.seospider` binaries, paid-provider
responses, generated client reports, local absolute paths, internal research journals, or private
development history. Examples must use synthetic data and reserved domains.

The source is MIT-licensed. The bundled Schema.org vocabulary remains CC BY-SA 3.0; preserve its
notice in `THIRD_PARTY_NOTICES.md`.

## Adding a tool

1. Implement the smallest useful core function.
2. Add a handler in `seohead/mcp/handlers.py` and its `HANDLERS` entry.
3. Add the CLI command and argument mapping.
4. Add the MCP tool with accurate side-effect annotations.
5. Add offline tests for success, failure, limits, and missing dependencies.
6. Update `docs/TOOLS.md`, the generated tool reference, and any provider cost or safety notes.
   Keep the README focused on workflows rather than fixed inventory counts.
7. Add the changelog entry as `changelog.d/<issue>.md`, never by editing `CHANGELOG.md`.

## Changelog entries

Entries live one file per change under `changelog.d/`, named for the issue the change closes
(`changelog.d/638.md`), and are folded into `CHANGELOG.md` at release time by
`python scripts/build_changelog.py --prune`. A branch that edits `CHANGELOG.md` conflicts with every
other branch that landed before it; a branch that adds its own file never can (#638). The
fragment holds the entry verbatim -- top-level `- ` bullets with their continuation lines, no
heading -- so prose is not reformatted on the way in. See CONTRIBUTING.md for the naming rules.
