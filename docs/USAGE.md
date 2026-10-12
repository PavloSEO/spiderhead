# Usage — copy-paste commands

Install first ([SETUP.md](SETUP.md)); below, `seohead` means the venv's
`seohead` (`.venv/bin/seohead` with the venv not activated). Everything that
takes `https://example.com` goes to the network; audit mode B over
`docs/examples/exports` is fully offline.

## The whole site in one command

For a native link crawl that runs until its queue is empty, disable the URL,
HTTP-attempt and time limits explicitly. Resource guards and robots/scope rules
still apply; an interrupted or resource-limited scan remains partial and resumable.
This mode is available for the durable SQLite site route, not the materialized
legacy collector. A finite URL limit can be enabled again at any time for a new scan.

```bash
seohead crawl-site --url https://example.com --scan-out site.sqlite --max-urls 0 \
  --input '{"overrides":{"limits.max_depth":-1,"limits.max_requests":0,"limits.max_crawl_seconds":0}}'
```

`limits.max_depth=-1` disables the native depth limit; `0` still means the start
page only. Higher configurable admission is not evidence of measured large-site
capacity. Inspect actual retained coverage and completion before using a report.

```bash
# bounded sitemap-based live evidence pass + a ready Excel file (not a link-graph crawl)
seohead site-audit --url https://example.com --limit 50 --report xlsx --out audit.xlsx

# re-render an existing audit document into other formats
seohead report-build --audit audit.json --format docx --out client.docx
seohead report-build --audit audit.json --format pdf --lang ru --out client.pdf
```

`--limit` caps the pages parsed (default 25); URLs come from the sitemap
unless `--urls` is given. Formats are `xlsx`, `docx`, `csv`, `md`, `json`, and
`pdf`. PDF output is offline, accepts `--lang en|ru`, and needs the optional
`seohead-seotools[pdf]` extra plus a local Chrome, Edge, or Chromium executable.

Field Core Web Vitals require an explicit CrUX record. `crux-report --url` measures
one URL; `--origin` is a separate aggregate over the origin, never a substitute
for missing URL data. Use `--form-factor PHONE` or `DESKTOP` for a device class;
without it CrUX aggregates all form factors. A bounded URL sample accepts
`--urls`, `--max-samples` (1–25) and optional `--cache-dir` with
`--cache-max-age-hours`. Provider responses and CLI saved-evidence inputs are
capped at 2 MiB; private cache entries are capped at 64 KiB. An oversized or
corrupt cache is reported unavailable without an implicit Google retry. Saved
samples with missing or repeated URL records are rejected before audit collection.
These are Google API reads, subject to its quota.

CrUX p75 uses the [official field thresholds](https://web.dev/articles/defining-core-web-vitals-thresholds):
LCP 2500/4000 ms, INP 200/500 ms and CLS 0.1/0.25. Missing metrics, an
ineligible record and provider failures are unavailable, never passing.
PSI/Lighthouse lab measurements remain separate.

The sequence is `crux-report --url <page> --form-factor PHONE`, save its JSON
output locally, then pass that file to `site-audit --url <origin>
--crux-evidence <file> --report md --out <report.md>`. Requesting CrUX is an
explicit provider read; rendering the saved evidence is offline.

`site-audit --crux-evidence` consumes saved CrUX JSON or a restricted
`provider-collect` artifact locally; it does not query Google. The audit JSON
retains field scope, period, policy and partialness, and report evidence coverage
shows available as well as unavailable metrics. Supplying origin data labels an
origin aggregate; it never makes each audited URL pass. Keep provider artifacts
and client URLs outside the public repository.

## Native SQLite crawl (default for a URL crawl)

```bash
# A URL crawl writes a collision-safe native scan under ./scans/ by default.
seohead crawl-site --url https://example.com --max-urls 50

# Choose a destination explicitly when a stable filename is needed.
seohead crawl-site --url https://example.com --max-urls 50 --scan-out native.sqlite
seohead report-build --audit native.sqlite --format md --out native-report.md
seohead report-build --audit native.sqlite --format md --out native-report.md --project ./example-project

# if the crawl was interrupted, continue it from the artifact -- no other flag
seohead crawl-site --resume native.sqlite

# Explain a one-page or interrupted crawl from saved evidence; no new requests.
seohead crawl-diagnose --scan native.sqlite
seohead crawl-diagnose --run ./run --max-decisions 10

# Optional redacted copy for sharing: creates a new file, never overwrites.
seohead crawl-diagnose-export --scan native.sqlite --export diagnostic-redacted.json
```

Save a reusable finding view and apply it to a retained scan and report:

```bash
seohead project view-save --directory ./example-project --expected-revision 0 \
  --input '{"view":{"name":"critical-pages","filters":{"severity":["critical"]},"sort":{"field":"url","direction":"asc"},"columns":["severity","check","url","text"],"page_size":100}}'
seohead findings-view --directory ./example-project --name critical-pages \
  --audit ./example-project/scans/current.sqlite --offset 0
seohead report-build --audit ./example-project/scans/current.sqlite --project ./example-project \
  --view critical-pages --format md --out ./example-project/reports/critical-pages.md
```

`project view-list` reports the config revision; each `view-save` needs that revision, with `0`
used for the first save. Applying a view returns explicit source/match/page counts, missing-field
counts and the next offset. A report identifies the selected view and page; its audit totals and
evidence coverage remain source-wide.

SQLite mode keeps queue, evidence and runtime in one transactional scan and resumes
an interrupted file under the same build/configuration: `--resume` reads the start
URL and that configuration back from the artifact, and refuses by name when the
file was written by another build or for another start URL. Use `--out-dir DIR` for
the explicit legacy directory route (`pages.jsonl` and `audit.json`). Explicit
URL lists can instead use `--urls-file input.txt --scan-out list.seohead`, then
`--resume list.seohead`, preserving exact order and multiple hosts without
discovery. Large directory lists also retain a `.list.seohead` spool; the
directory JSON audit bridge remains separately bounded. Native list capture is
static-only; see [URL_LIST_SCANS.md](URL_LIST_SCANS.md) for input, budget and
resume boundaries. Cache `live`/`replay` requires the bounded eager legacy route, as does
`discovery.external.crawl`, the opt-in bounded check of recorded outlink
destinations: it runs a second phase after the internal frontier closes under
its own `external_checks.*` target/host/request/depth/redirect budgets, writes
one outcome per destination to `external_checks.jsonl`, and resumes without
reopening spent budgets — see
[scenarios/external-links.md](scenarios/external-links.md). A `--scan-out` or
list-mode run with the option set refuses by name rather than silently
dropping it. The native
scan's default body policy is
`storage.body_mode=captured_entity_bytes`, which retains bounded captured HTTP
entity bytes and separately captured DOM when available; `off` retains metadata
only. Native capture requires raw rendering, cache off, and credential-free
configuration. Audit creation has an explicit compatibility guard;
check `audit_available` before requesting a report. See [STORAGE.md](STORAGE.md)
for limits, provenance, interrupted-file handling and missing evidence.

The legacy `--out-dir` collector writes page, link and form evidence incrementally
instead of retaining a second full page/link list for collection and resume. Its
current JSON audit bridge is limited to 10,000 pages, 20,000 forms and 1,500,000
links. Beyond a bound, the command returns `audit_available: false` with exact
counts and a reason while keeping the JSONL evidence. A prior `audit.json` or
tasks file in that directory is renamed to a hidden `.stale-*` copy so it
cannot be mistaken for the new run's report. The large-audit representation is
implemented through audit.v2 for native SQLite scans, whose explicit admission
ceiling is 1,000,000 URLs. Materialized list/eager APIs retain their separate
50,000-URL safety bound. Native JS escalation reads page records from SQLite
without first building a full PageRecord list; rendering has independent
coverage and budgets. See [capacity acceptance](MILLION_CRAWL_ACCEPTANCE.md) for
source-bound workload measurements, format bounds and the remaining list-mode gap.

## Saved scan artifact

```bash
# import a finished legacy directory; SHA is the original crawler build
python -m seohead.storage import-run RUN_DIR --out scan.sqlite --producer-build SHA
python -m seohead.storage inspect scan.sqlite

# restore the compatible three-file legacy export into a new directory
python -m seohead.storage export-run scan.sqlite --out-dir NEW_DIR

# use the stored audit through existing report, comparison, and task routes
seohead report-build --audit scan.sqlite --format md --out report.md
seohead compare-crawls --before before.sqlite --after after.sqlite
# Optional declared release mapping; never inferred from titles or page text.
seohead compare-crawls --before before.sqlite --after after.sqlite \
  --correspondence ./docs/examples/url-correspondence.json
seohead sf tasks --json scan.sqlite --out tasks

# retained native evidence only: create a new derived artifact without network replay
seohead scan reanalyze --scan old.sqlite --out derived.sqlite --producer-build SOURCE_SHA
```

The scan supplies its internal saved audit to these routes. An adjacent
`audit.json` that differs or cannot be read is not substituted for it; supported
structured responses carry an `input_diagnostics` notice. The MCP report and
compare tools, plus SF summary, issues, and tasks, also accept a scan path. The
issues tool retains its list response and emits a `RuntimeWarning` for that input
notice instead of adding a synthetic issue.

The export has deterministic UTF-8 `pages.jsonl` and `links.jsonl`, plus the exact
saved `audit.json`. It does not recreate bodies, raw HTML, forms, robots,
start-page evidence, sitemap responses, or a resume checkpoint. See
[STORAGE.md](STORAGE.md) for safe read-only access, version rules, and the export
contract.

`scan reanalyze` needs a native scan with the required retained corpus; a legacy
three-file import cannot reconstruct it. It creates a new parented artifact,
never overwrites its input, and makes no HTTP, DNS, browser, provider, or resource
request. `SOURCE_SHA` is the full current analyzer build SHA, while the input
already records its original capture build.

For `--format csv`, each successful build returns `outputs` for the requested
findings CSV plus `<name>.pages.csv` and `<name>.scope.csv`. All three are
rewritten on every build; an empty findings, pages, or scope collection leaves
the corresponding file with its header only.

## Project checklist coverage

```bash
# Creates or reconciles local checklist definitions only; it runs no checks.
seohead project checklist-init --directory ./example-project

# Use the revision returned by initialization for every local write.
seohead project checklist-record \
  --directory ./example-project \
  --item-id skill:workflow/control \
  --expected-revision 1 \
  --input '{"record":{"status":"not_applicable","reason":"The work is a scoped follow-up, not an unscoped audit","reviewer":"Specialist","evidence":"Agreed scope memo 2026-10-01"}}'
```

`project-checklist-update` and `project-checklist-record` receive their structured
`item` and `record` values through `--input` JSON. They validate supplied data and
record local status; neither command executes a checklist operation or contacts a
site, browser, provider or crawler.

Preview project work priorities from saved facts, then explicitly apply the reviewed
policy with the current checklist revision:

```bash
seohead project priorities --directory ./example-project
```

The MCP equivalent is `seo_project_priorities`. Its default is read-only preview;
`apply: true` requires `expected_revision`. See [priority policy details](PROJECTS.md#preview-and-apply-work-priorities).

## Saved scan history

These commands are local and offline. `scan list` validates artifact metadata and
schema without reading body BLOBs; `scan inspect` reads only a whitelisted table
and returns a bounded page of rows. `scan status` separates native frontier work
from committed page outcomes, so a done 503 or transport failure is never called
successful and queued/inflight rows remain unfinished work. Imported artifacts
report their frontier as unavailable because they retain no native queue.

```bash
seohead scan-list --directory . --limit 100
seohead scan-inspect --scan native.sqlite --table pages --offset 0 --limit 100 --max-bytes 1048576
seohead scan-status --scan native.sqlite
seohead scan-rendered-routes --scan native.sqlite

# --out is either a new filename or an existing directory; neither form overwrites
seohead scan-snapshot --scan native.sqlite --out snapshot.sqlite
seohead scan-snapshot --scan native.sqlite --out .

seohead scan-pin --scan native.sqlite
seohead scan-pin --scan native.sqlite --unpin

# preview only: save this complete stdout JSON envelope and review it before applying
seohead scan-prune --directory . > plan.json

# compare retained, compatible evidence only; no network request and no SEO score
seohead scan-body-diff --left before.sqlite --right after.sqlite --url https://example.com/ --text

# broken bookmarks: resolve every retained link fragment against the retained
# destination document; missing or incomplete bodies stay named skips, never findings
seohead scan-fragment-links --scan native.sqlite --state missing --limit 50
```

`scan snapshot` assigns a directory output a no-clobber
`UTCtimestamp_host_shortUUID.sqlite` name. `scan prune` defaults to finished,
unpinned, non-partial scans older than 30 days and outside the newest five for the
same host/configuration. It never automatically selects `crawl_partial` or
`corpus_partial` files. Applying deletion requires the reviewed preview envelope:
`seohead scan-prune --directory . --plan plan.json --apply`. The tool rechecks
every candidate and its current retention rank before deleting anything.

Each flat form also has a nested `scan` equivalent: `scan list`, `scan inspect`,
`scan status`, `scan snapshot`, `scan pin`, `scan prune`, `scan body-diff`,
`scan evidence`, `scan extract`, `scan fragment-links`, `scan requeue`, and
`scan import-urls`.

`scan pin` takes the artifact's writer lock and uses SQLite DELETE journal mode.
It changes only the pin bit: the SQLite file hash changes, while the saved audit,
retained body rows, and evidence revision do not.

## Screaming Frog crawl audit

```bash
seohead sf run --exports-dir docs/examples/exports --out report --tasks   # mode B: ready exports
seohead sf run --crawl https://example.com --out report --tasks         # mode A: SF CLI crawls (license)
seohead sf tasks --json report/audit.json                            # backlog from an existing audit.json
seohead sf doctor                                                    # environment diagnostics
```

Output: `report/audit.json` (schema-validated by `sf/schema/audit.schema.json`)
and `report/audit.md`; `--tasks` adds `tasks.json`/`tasks.md`. Exit codes:
`0` ok, `2` critical findings found (`--fail-on critical`, for CI), `1` error.

Useful `sf run` flags: `--profile lite|full|custom`, `--config config.json`,
`--sitemap <url>`, `--auth USER:PASS` / `--auth-config` for protected
staging, `--sf-cli <path>`, `--max-urls-per-second N` (polite crawling),
`--live-recheck` (network re-check of sitemap URLs — off by default).

Mode A's `full` profile checks the installed Screaming Frog CLI's export help before
starting, then requires every supported requested file in that run's fresh export
directory before audit rules run. A missing, unreadable, or unsupported export fails
the run; it cannot be supplied by an older output folder. JavaScript, Canonicals, H1,
and structured-data validation tabs are retained as raw evidence. Sitemap redirect and
4xx/5xx evidence is derived from `Sitemaps:URLs in Sitemap` only when every row has a
valid `Status Code`. `lite` and explicitly selected `custom` exports remain smaller.
Mode B (`--exports-dir`) continues to analyze a supplied subset and reports skipped
checks.

For a polite fresh crawl, save a reusable SF config after choosing a speed in the GUI:

```bash
# In Screaming Frog: Config → Speed, set 1–2 URLs/s, then Config → File → Config → Save As.
seohead sf save-config --out audit.seospiderconfig
seohead sf run --crawl https://example.com --max-urls-per-second 1.5 --out report
```

Alternatively set `sf_cli.seospiderconfig` and `sf_cli.max_urls_per_second` in
`config.json`. A requested limit is written to a derived config and read back before
the crawl; the saved base file is unchanged. If no readable base config exists, the
run stops before Screaming Frog starts. `sf doctor` reports the saved rate or says it
is unknown; it does not infer a rate from a missing file. Loading a saved crawl has no
new crawl-rate limit to apply.

Prefer an SF-owned `--auth-config` profile where possible. A literal `--auth USER:PASS` value can
be exposed by shell history or process inspection, so use it only in an isolated transient
session and never paste it into logs or issue reports.

A worked example lives in [`docs/examples/`](examples/README.md): a synthetic
crawl with deliberate problems, its `audit.json` and the derived tasks.

## Recon: what is this domain made of

```bash
seohead domain-profile --domain example.com
seohead cdn-check      --url https://example.com
seohead tech-detect    --url https://example.com
seohead security-check --url https://example.com
seohead mirror-check   --url https://example.com
seohead ai-bots-check  --url https://example.com
```

## Single-page checks

```bash
seohead parse         --url https://example.com/product/nasos
seohead headers-check --url https://example.com
seohead hreflang-check --url https://example.com
seohead schema-check  --url https://example.com/product/nasos
seohead soft404-check --url https://example.com
seohead llms-txt-check --url https://example.com --brand "My Brand"
```

## Backlinks by your own donor list

```bash
seohead backlinks-check --target example.com --donors "https://donor1.example/page,https://donor2.example"
seohead backlinks-check --target example.com --donors-file donors.txt   # one URL per line, # comments
```

## Input conventions

Primary input for any command is `--input '<json>'` — the object is mapped
onto the handler's arguments. Frequent parameters are duplicated as flags:

```bash
seohead duplicate-check --input '{"items":[{"id":"a","text":"..."},{"id":"b","text":"..."}],"threshold":0.9}'
seohead duplicate-check --scan saved.sqlite
seohead boilerplate-report --scan saved.sqlite
seohead semantic-inputs --scan saved.sqlite
seohead semantic-similarity --cache-path semantic-cache.sqlite --input '{"items":[{"url":"https://example.com/page","html":"<main>Synthetic pump catalogue content for a deterministic offline example.</main>"}],"adapter":{"kind":"local","model_id":"synthetic-fixture","model_version":"1","settings":{"dimensions":2},"data_transfer":"none"},"embeddings":[{"url":"https://example.com/page","vector":[0.1,0.2]}]}'
seohead meta-description-drafts --input '{"items":[{"url":"https://example.com/page","html":"<html lang=en><head><title>Synthetic pumps</title></head><body><main>Synthetic pump catalogue content for a deterministic offline example.</main></body></html>"}],"context":{"language":"en"}}'  # dry-run, no model call
echo '{"url": "https://example.com"}' | seohead parse          # stdin JSON also works
```

Output is always JSON on stdout. Exit codes:

- `0` — the command completed and reports `"ok": true` (or carries no `ok` field at all).
- `1` — either the handler could not complete its check and says so in the JSON with
  `"ok": false` (a bad input, an unreachable host, a missing dependency — the tool's own
  contract in `docs/ARCHITECTURE.md` is to report this as data, never raise), or the process
  crashed before producing JSON, in which case a one-line `error: ...` goes to stderr instead
  of stdout.
- `2` — reserved for `log-scan` alone: the run it inspected contradicts itself (its own numbers
  disagree), which is a distinct signal from "the command failed" so a script can tell the two
  apart. `log-scan` still exits `1` for an ordinary failure such as a missing run directory.

An MCP client sees the same distinction via `isError` on the tool result rather than a process
exit code.

## Paid data sources (demand, SERP, traffic)

Check readiness first, then work; check what was charged afterwards:

```bash
seohead sources-doctor                                  # redacted credential sources and readiness
seohead provider-readiness --input '{"provider":"gsc","operation":"search_analytics"}'
seohead provider-verify --input '{"provider":"gsc","request":{"site_url":"sc-domain:example.com"}}'
seohead keywords-expand --phrase "underfloor heating" --limit 100
seohead keywords-exact --keywords "underfloor heating,floor screed" --region 225
seohead serp-fetch --queries "underfloor heating,floor screed" --region 213 --top 10
seohead google-keywords --keywords "seo tools"           # DataForSEO, sandbox by default
seohead google-serp --query "seo tools"
seohead metrika-counters
seohead metrika-setup --counter 12345678                 # before any traffic conclusions
seohead metrika-report --counter 12345678 --metrics "ym:s:visits" --date1 90daysAgo
seohead metrika-traffic-pdf --counter 12345678 --date1 2026-09-01 --date2 2026-09-30 --out-dir ./traffic
seohead spend-report --since 2026-08-01
```

`provider-readiness` is offline: it reports declared operation routes, credential
source references, and whether a separate permission check is still needed. Its
`input_schema` describes the shared JSON call envelope; the existing provider
adapter validates the operation-specific fields. Only an explicit
`provider-verify` call contacts a supported provider, and a configured secret alone
never means the selected property or site is accessible.

Money rules for this layer: [GOTCHAS.md](GOTCHAS.md).

## MCP server

```bash
seohead mcp        # stdio: 230 core tools + 10 seo_semantics_* tools + 6 sf_* audit/snippet tools
```

Client config (`.mcp.json` in this repo does exactly this):

```json
{ "mcpServers": { "seohead": { "command": "seohead", "args": ["mcp"] } } }
```

Large audit results come back as file paths, never dumped inline. Network
side effects (sitemap live-recheck, path probing) stay opt-in.

## Docker

```bash
docker compose run --rm seohead sf run --exports-dir /data/exports --out /data/report --tasks
docker compose run --rm seohead headers-check --url https://example.com
```

Authorized inputs and outputs go under `./workspace` (mounted as `/data`); without arguments the
container prints `--help`. The image does not include Screaming Frog or a Playwright browser.

## Configuration

Audit behaviour lives in `config.json` (template: `docs/examples/config.example.json`): `thresholds`, `severity_overrides`,
`checks.<ID>.enabled`, `tasks_pipeline`, plus the `sf_cli` block (path
search list for the SF CLI). Numbers are not hardcoded in the source — a
threshold is changed in config, not in code.

`config.example.json` is a **complete template**, not a sparse override example: it lists
every path `DEFAULT_CONFIG` (`seohead/sf/config.py`) currently defines, at that default's own
value. `DEFAULT_CONFIG` stays the one semantic owner — the example is a projection of it,
checked for parity by `tests/test_config_example_parity.py` — so copying the file and editing
only what you mean to change is safe, and a config path added to the code without a matching
line here fails that test rather than shipping a template that silently hides a control.
