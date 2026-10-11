# Setup from zero

The general venv workflow below is verified on macOS (darwin, arm64). For a
versioned headless install, upgrade, and rollback over SSH on Ubuntu Server,
follow [Linux VPS over SSH](LINUX_VPS.md). Windows paths for the SF CLI are
supported by `config.json` search paths.

## Requirements

- **Python 3.10+** (`requires-python` in `pyproject.toml`).
- pip, git. That is all — every other dependency is a Python package.
- Optional, improves results if present on the system:
  - **Screaming Frog SEO Spider CLI** — required only for live audit mode A
    and requires a separately installed, active licence. Mode B analyzes
    supplied CSV/XLSX exports offline without an SF installation or licence.
  - **system `whois`** — fallback for ccTLDs without RDAP. Without RDAP and
    without `whois`, domain registration data is honestly reported as
    `source: none`.

## Install

```bash
git clone https://github.com/PavloSEO/seohead-tools.git
cd seohead-tools

python3 -m venv .venv
source .venv/bin/activate

pip install -e ".[all,dev]"            # everything incl. reports, render, tests
python -m playwright install chromium  # browser for render-check (~150 MB)
```

The repository is named `seohead-tools`; the Python distribution remains
`seohead-seotools` and the installed command remains `seohead` for compatibility.

The optional remote Playwright transport needs the Python `render` extra, but
does not need a locally installed browser binary. It connects only to a browser
server that the operator has already provisioned; see [TOOLS.md](TOOLS.md) for
the explicit endpoint/version configuration and security boundary.

Why `-e`: the `seohead` entry point must see the working tree while you edit
it. The install is not global — everything lives inside `.venv/` (which is
gitignored).

The toolkit is deliberately split into dependency groups (`pyproject.toml`,
`[project.optional-dependencies]`); with any group missing, the rest still
works, and the affected tool answers `{"ok": false, "error": ...,
"install": "..."}` with the exact install command instead of crashing:

| Group | Installs | What stops working without it |
|---|---|---|
| base (always) | `httpx`, `beautifulsoup4`, `lxml`, `defusedxml`, `pandas`, `h2`, `pydantic`, `jsonschema`, `openpyxl`, `Pillow`, `tomlkit`, `charset-normalizer`, `typing_extensions` (Python < 3.12) | nothing, this is the minimum |
| `reports` | `python-docx` (+ openpyxl already in base) | `docx` output of `report-build` (xlsx/csv/md/json stay) |
| `pdf` | `pypdf` | Validate generated PDF signature, pages, extracted text, source counts and declared size limits |
| `render` | `playwright` | `render-check`, `regions-check --render` |
| `sitemap` | `advertools`, `python-dateutil` | deep parsing of very large sitemaps |
| `mcp` | `mcp` | the MCP server (the CLI stays) |
| `cluster` | `scikit-learn`, `numpy`, `snowballstemmer` | `keywords-cluster` |
| `dev` | `pytest`, `pytest-cov`, `ruff`, `respx`, `build`, `cryptography`, `mypy`, `PyYAML`, `twine` | the test suite |

## First run

```bash
seohead --version                     # seohead 3.0.0
seohead --help                        # the command list
pytest -q                             # over 7000 offline tests; runtime depends on extras
seohead sf run --exports-dir docs/examples/exports --out /tmp/report --tasks
```

The last command runs a real audit (mode B) over the synthetic crawl in
`docs/examples/exports/` and writes `/tmp/report/audit.json` + `audit.md` +
`tasks.json` + `tasks.md`. If that works, the toolkit works.

## Crawling without a Screaming Frog licence

```bash
seohead crawl-site --url https://example.com/ --max-urls 200
```

Follows links from the start URL on the same host, respects `robots.txt`, and audits the result
through the same checks used for Screaming Frog exports. `--min-delay` is the floor beneath an
adaptive back-off: latency widens the delay, a timeout widens it hard, and repeated timeouts stop
the run rather than pushing a failing origin. A URL crawl commits evidence to a native SQLite
artifact under `./scans/` by default, returning its path as `scan`. Use `--scan-out` for a chosen
filename, or explicit `--out-dir` for the legacy `pages.jsonl`/`audit.json` workflow. Interrupted
native scans keep committed evidence and resume with `crawl-site --resume FILE`.

While it runs, a progress line on stderr is refreshed in place:

```
crawl-site: 6516 fetched, 34635 known (18%), 9.8 req/s, 11m05s
```

`known` is what the crawl has found so far — pages fetched plus URLs still
queued, capped at the URL budget — not the size of the site. A crawler
discovers its own workload as it walks, so that denominator grows, and the
percentage is not an estimate of when the run will finish. The scan size
appears for the default native artifact (or an explicit `--scan-out`), and counts the artifact's write-ahead log
alongside the file itself — which is why it can fall at the end of a run, when
SQLite folds that log back into the file.

Piped or redirected output gets a plain line every 30 seconds instead of
carriage-return redraws, so a log file stays readable. `-q` silences the
progress line and the startup rate line; the JSON result on stdout is
unaffected either way.

For a migration map, analytics export, or hand-maintained set, list mode fetches
only the supplied URLs at depth zero. `--urls-file` scans TXT, CSV, XLSX, or XML
for absolute HTTP(S) URLs and preserves their source order; it does not infer a
column name or follow links discovered on those pages.

```bash
seohead crawl-site --urls-file ./redirect-map.xlsx --robots report_only --out-dir ./report
```

Set `discovery.resolve_redirect_destination` or
`discovery.resolve_canonical_destination` in the crawl config when a migration
audit needs the final target of a redirect or canonical chain. Each is an
explicit bounded per-row walk; neither follows ordinary links or changes list
mode's depth-zero page population.

`scope.segments` limits discovery by URL shape. For report grouping after
collection, use `analysis.segments`: its rules can match any recorded page
field and reference another segment. The engine validates cycles before the
crawl starts and uses deterministic dependency order.

To join observed pages to traffic or search data without calling a provider,
give `crawl-enrich` a URL-keyed CSV. It retains unmatched and malformed rows as
evidence instead of treating them as zero. A completed crawl may write its
same-origin external-only URLs for a follow-up list-mode audit; partial crawls
do not label those URLs as orphans.

```bash
seohead crawl-enrich --audit ./report/audit.json --external-csv ./gsc.csv --out-urls ./not-observed.txt
seohead crawl-site --urls-file ./not-observed.txt --out-dir ./orphan-audit
```

This is not Screaming Frog parity. Checks whose evidence a native crawl cannot produce —
near-duplicates, readability, pixel widths, link score — are reported as **skipped**, never
as clean, and `summary.check_coverage` states how much of the registry ran. Redirect chain
and loop detection are the exception: they are resolved from the crawl's own per-URL
redirect targets, as a second pass over the finished crawl, with no Screaming Frog report
required.

`seohead sf doctor` prints environment diagnostics: where the SF CLI is (or
is not), which optional dependencies are present, which base Screaming Frog
config a crawl would use, and which module switches that config turns on. A
module the reader cannot decode is printed as `unknown`, never as `off`.

Module switches decide whether the module-dependent checks can run at all.
Without a base config SF crawls with its own defaults and those checks come
back `skipped` — which you would otherwise only discover after the crawl. Two
things make that visible up front:

```bash
seohead sf save-config                # copy the latest SF crawl config to audit.seospiderconfig
seohead sf save-config --out base.seospiderconfig --force
```

To limit a new crawl, set a speed in Screaming Frog before saving the config:
open **Config → Speed**, choose a modest value such as 1–2 URLs/s, then use
**Config → File → Config → Save As**. `seohead sf save-config` copies the latest
SF crawl config; it does not choose or add a rate. Point `sf_cli.seospiderconfig`
at the saved file and set `sf_cli.max_urls_per_second` (or pass
`--max-urls-per-second 1.5`). The runner writes and reads back a derived config,
leaving the saved base untouched. If the requested limit cannot be applied, it
fails before SF starts. `sf doctor` reports the saved rate when it can read it,
otherwise it marks the rate unknown; a missing config is not treated as an
unlimited rate that was measured.

`sf run` prints a `[preflight]` line before a fresh crawl for every check that
the configuration in force cannot satisfy, so the config can be fixed first.
Mode B (`--exports-dir`) already has the exports and is not affected.

`sf-analyzer` is also installed as a focused audit-CLI alias (`[project.scripts]` in
`pyproject.toml`). Use `seohead sf ...` when one entry point is preferable.

## Docker

```bash
docker build -t seohead-tools .                      # slim: crawl, audit, MCP
docker build --build-arg EXTRAS=all -t seohead-tools:full .   # + clustering, rendering
```

The default image carries the crawl-and-audit path and the MCP server. Keyword clustering is
deliberately not in it: `scikit-learn` pulls `scipy` transitively, which is 119 MB for a feature
unrelated to crawling a site. Rendering (Playwright) is likewise opt-in — it needs a real Chromium
and its shared libraries.

Measured: **440 MB slim, 1.08 GB full.** CI prints the image size on every run, so a transitive
dependency that quietly adds a couple of hundred megabytes shows up in the build log rather than
being discovered later. Both variants are built and smoke-tested in CI.

The builder stage also drops build-only weight that a straight `pip install` leaves behind: it
installs with `--no-compile` (pip's install-time `compileall` pass otherwise bakes in ~67 MB of
`__pycache__` that `PYTHONDONTWRITEBYTECODE` at runtime does nothing to prevent, since that only
stops imports from writing bytecode later) and uninstalls `pip` itself (~13 MB) before the venv is
copied into the runtime stage — the image only ever runs the `seohead` entry point, never pip.

`pandas` does **not** require `pyarrow`: as of pandas 3.0, `pyarrow` is still an optional extra
(`pandas[pyarrow]`), not a runtime dependency — confirmed against the pandas 3.0.5 wheel metadata
and by building this image and checking `pip`'s own dependency-resolution log. `pyarrow` is real
weight in the *full* image, but it comes from `advertools` (the `sitemap` extra, pulled in by
`EXTRAS=all`), not from pandas, and not from anything in the default slim build — the slim image
never installs `pyarrow` regardless of the pandas pin. Pinning `pandas<3` would therefore not
remove it and was not made; if `sitemap`'s ~46 MB `pyarrow` weight needs trimming later, the fix is
in `advertools`'s own dependency tree, tracked separately from this issue.

## Targeted fix verification

For a focused recheck, select baseline finding IDs or affected URLs and write a new
verification directory:

```bash
seohead verify-fixes --baseline before.sqlite --finding-ids ISSUE-000001,ISSUE-000002 --out-dir ./verify-2026-10-03
# Compare already collected evidence without making requests:
seohead verify-fixes --baseline before.sqlite --after after.sqlite --finding-ids ISSUE-000003 --out-dir ./verify-offline
```

`--view` accepts a JSON file such as
`{"schema_version":"verification_view.v1","finding_ids":["ISSUE-000001"]}`;
`--urls-file` accepts the same TXT/CSV/XLSX/XML inputs as crawl list mode. The
command creates `verification.json` and `verification.md` without overwriting an
existing directory. It retains the baseline audit hash, observation time, exact
before/after page and finding evidence, and the recrawl audit files. Raw pages
use the existing list collector; a baseline with JavaScript rendering uses one
bounded URL crawl per selected page. Missing, skipped, partial or changed-policy
evidence is `not_verifiable`, never a fix; whole-site graph checks cannot be
cleared by this URL subset. A baseline that redacts authentication settings
needs the original local `--config` to reproduce its policy. No credential
value is copied into the verification report.

Each selected baseline finding gets one verdict: **resolved** when its check
ran clean on the same measured page representation; **persisting** when the
same finding and evidence remain; **changed** when its evidence or HTTP
response changed without proving the old condition clear; **not_verifiable**
when the page was not fetched, the check was skipped, the source/configuration
changed, or the finding needs a whole-site population. A partial targeted run
may still verify a page it did measure, while every unvisited page stays
`not_verifiable`.
With offline `--after`, both audits must record a scan UUID and a timezone-aware
`run.generated_at`. The after scan must have a different UUID and a later time;
otherwise the verification artifact explicitly says `not_verifiable`. This
prevents a replayed scan or an older saved audit from appearing to prove a fix.

## Comparing two crawls


```bash
seohead compare-crawls --before old-audit.json --after new-audit.json
```

The most repeated question in audit work is "did the fix actually ship", and a naive diff of two
finding sets cannot answer it: a page that stopped matching because it was fixed and a page that
stopped matching because it dropped out of the crawl look identical unless something also checks
which pages were actually crawled each time.

Every finding lands in exactly one of four sets:

- **entered** — a new problem on a page that was already being crawled;
- **left** — the URL was fetched by both crawls and no longer matches: a real fix;
- **appeared** — a genuinely new page, and it has a finding;
- **disappeared** — not rechecked: the URL was not fetched by both crawls (the bucket key
  stays `disappeared` for compatibility), so a missing finding proves nothing about whether
  it was fixed.

`left` and `disappeared` are the pair that matters. Confusing them is how "we fixed it" gets said
about a page nobody re-checked.

The result carries `warnings` when the comparison should be trusted less: either crawl marked
invalid or partial, or the two runs using different results-affecting settings (`run.crawl_config`
from #13) — in which case some of the difference may be the configuration, not the site.

## Crawler configuration

```bash
seohead crawl-site --url https://example.com/ --config crawl.json
```

```json
{
  "limits": {"max_urls": 500, "max_depth": 4},
  "speed": {"min_delay_seconds": 1.0},
  "robots": {"policy": "report_only"},
  "discovery": {"external": {"store": true, "crawl": false}}
}
```

### Site-specific canonical policy

Pagination and filter canonicals are checked only when a project supplies patterns and an
expected policy. There is no default rule that paginated or filtered URLs must self-canonicalize
or point to another page. For a native crawl, add `analysis.canonical_policy` to `crawl.json`:

```json
{
  "analysis": {
    "canonical_policy": {
      "pagination": [
        {"pattern": "/catalog/page/[2-9][0-9]*/?$", "policy": "self"},
        {
          "pattern": "/archive/page/[2-9][0-9]*/?$",
          "policy": "first_page",
          "target": "https://shop.example.test/archive/"
        }
      ],
      "filters": [
        {
          "pattern": "[?&]color=",
          "policy": "landing",
          "target": "https://shop.example.test/catalog/"
        }
      ]
    }
  }
}
```

Patterns are regular expressions searched against each absolute source URL. Rules are ordered and
the first match wins. Pagination policies accept `self`, `first_page`, or `landing`; filter
policies accept `self` or `landing`. Non-self policies require an explicit absolute target URL.
The expected target must appear in the crawl evidence; when a canonical is declared, its target
must also appear. Otherwise the corresponding check is reported as skipped. The same
`canonical_policy` object can be placed at the top level of the SF `config.json` used with
`sf run --exports-dir`. The checks compare URL
policy only; destination status and site-wide homepage groups are covered separately by #824.

Resolution order is defaults, then the file, then environment variables, then explicit command-line
arguments — the most local statement of intent wins.

Use `http.headers` only for non-credential request headers such as `Accept-Language`. Authentication
headers, cookies, API keys, and tokens are refused there: put them in host-bound
`http.credential_headers` as `env:VARIABLE` references and set `http.credentials_acknowledged=true`.
For username/password login, use `http.auth` with an exact host, `scheme` (`basic` or `digest`),
and `username_env` / `password_env` references such as `env:CRAWL_USER`; set
`http.auth_acknowledged=true`. These references are resolved only for that host, Digest challenge
retries consume another rate/request-budget turn, and authenticated scans cannot be resumed from
their retained redacted config. Profiles refuse authentication settings. Authenticated runs require
`cache.mode=off` and HTTP uses HTTPS only. Example config shape:

```json
{
  "http": {
    "auth_acknowledged": true,
    "auth": [
      {
        "host": "example.test",
        "scheme": "digest",
        "username_env": "env:CRAWL_USER",
        "password_env": "env:CRAWL_PASSWORD"
      }
    ]
  }
}
```

To ask the origin for zstd-compressed response bodies, set `http.zstd=true`. Python 3.14+ uses its
standard-library decoder; older Python versions need the optional `zstd` extra. This is opt-in and
requires `cache.mode=off`; compressed content is decoded under the same response-body limit before
parsing or retention. The scan records the setting, while its load estimate reports disk size as
unavailable until body-size evidence exists.

```bash
python -m pip install -e '.[zstd]'
```

For a native crawl through an HTTP forward proxy, set `http.proxy` explicitly. The default is
direct and ignores ambient `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, and `NO_PROXY`. An anonymous
endpoint may be written as `http://proxy.example.test:3128`; credentials must come through an
environment reference to the complete URL:

```json
{"http": {"proxy": "env:SEOHEAD_CRAWL_PROXY"}}
```

For example, `SEOHEAD_CRAWL_PROXY` may contain an `http://` URL with a percent-encoded username
and password (`@` as `%40`, `:` in a password as `%3A`; colons in usernames are refused).
It is read at crawl start, never stored in the scan or manifest. Only `http://`
forward proxies with an explicit port are supported for HTTP targets and HTTPS CONNECT targets;
HTTPS proxy endpoints, SOCKS, PAC, bypass lists and browser-native proxy fallback are rejected.
`http.proxy_allow_private=true` authorizes a private **proxy socket** only; it never authorizes
private target URLs. The crawler validates and pins both the proxy socket and each target/redirect
before connecting. A proxy's onward behavior cannot be attested after the request reaches it.
Proxy failures never switch to direct egress. The manifest records the safe proxy endpoint and
whether authentication was configured, never its value. Proxied crawls require `cache.mode=off`
and cannot resume from saved legacy or SQLite frontiers; start a new output instead. The pinned
HTTP route uses the platform trust store, including Windows system certificates; `SSL_CERT_FILE`
or `SSL_CERT_DIR` appends an explicit corporate CA while TLS verification remains enabled. For an
environment managed by uv, run `uv run --system-certs seohead render-check ...` when its Python
trust configuration has not already been prepared. Browser POST subrequests use that same pinned
route; unsupported methods and WebSockets are recorded as unavailable without direct browser egress.
Proxied HTTPS uses a fresh verified CONNECT tunnel for each request, with the origin hostname
kept for SNI and certificate validation even though CONNECT names its vetted IP; HTTP/2 and
keepalive reuse are disabled on this route to avoid mixing hostnames on one IP.
The same policy is reachable with `--set http.proxy=env:SEOHEAD_CRAWL_PROXY` or the MCP
`seo_crawl_site` overrides.

`crawl-site --help` shows, among others, the settings used directly on the command line
(`--url`, `--max-urls`, `--out-dir`, `--scan-out`, `--config`, `--robots`, `--sitemap`); everything else — the
settings above and every one the crawler build-out has added since — lives in the config file. Run
`seohead crawl-site --config-help` for the full list: every key's path, type, default, and
description, generated from this module rather than hand-maintained. (`--max-depth` and
`--min-delay` still work as direct flags for scripts written before `--config` existed; they are
just no longer shown in `--help`.) The same list is reachable without a filesystem via
`seohead crawl-describe-settings` (CLI) or the `seo_crawl_describe_settings` MCP tool — a
tool-calling agent can discover the configuration surface, not only a human reading the source.

`--sitemap <url>` seeds the crawl from that sitemap's declared URLs — each one is fetched and its
own links are followed, rather than the sitemap being treated as the final answer — and reconciles
the declared set against the URLs the crawl actually reaches by following a link. The result lands
in `audit.json` as `summary.sitemap`, with three disjoint sets: URLs declared and linked (healthy),
URLs declared but never linked from any crawled page (orphaned — reported via `SITEMAP_ORPHAN`),
and URLs linked but never declared (reported via `URL_NOT_IN_SITEMAP`). With no explicit
`--sitemap`, setting `sitemaps.auto_discover` in `--config` seeds from every sitemap robots.txt
declares — a site can list more than one `Sitemap:` directive, and each one is fetched and its
URLs unioned, not just the first.

Three properties are deliberate:

**An unknown key is an error, not a no-op.** A setting the crawler does not read would promise
behaviour that does not exist, and a typo in a scope pattern would silently widen a crawl.

**`store` and `crawl` are separate flags** for every link type: keep it in the report, versus
request it for a status code. These are different questions, and one flag for both is why a crawler
either misses broken images or triples its request count.

**Settings that change what the audit finds are written into `audit.json`** as
`run.crawl_config`, with their resolved values. Two reports on the same site are otherwise not
comparable, and nobody can tell why they differ. `run.effective_max_requests_per_second` records the
politeness the run actually permitted, because politeness is a combination of settings rather than
any single one.

The crawler stops on its own when a host is failing: repeated timeouts, or repeated 429 and 5xx
responses, end the run rather than continuing at the same rate. A single 429 is treated as an
overload signal rather than a retryable blip — it is the server explicitly asking for less. A
numeric `Retry-After` raises the delay to at least what was asked.

When robots.txt supplies `Crawl-delay` or a valid `Request-rate: requests/seconds`, the crawler
uses the stricter interval before fetching declared sitemaps and every later request. The stored
`crawl_delay_applied` value is that effective robots-derived interval; the parsed robots context
retains the two directives separately.

The shared request budget covers robots, sitemap discovery and audit rechecks, page requests,
retries, redirects, captured resources, and browser HTTP routes. Robots directives can raise
the configured delay floor, never lower it.

Rendering launches a headless browser — Chromium with its sandbox enabled — and refuses
root execution. `rendering.browser.engine` selects which Playwright engine runs: `chromium`
(the default), `firefox` or `webkit`. An unknown name is a config error before any browser starts, and a
missing browser binary fails with an actionable `playwright install <engine>` hint rather
than silently substituting another engine. Firefox cannot emulate a mobile viewport
(`is_mobile` is unsupported there), so `rendering.browser.mobile_emulation` is refused for
it; `rendering.browser.touch_emulation` (`has_touch`) is a general context option every
engine accepts. HTTP routes, including popup requests, are fulfilled through the same
validated, pinned HTTP transport; the browser does not continue those requests through its
own DNS resolver. Browser cookies and cross-origin restrictions are preserved. Service
workers are blocked.

`rendering.browser.viewport` selects the `desktop` (1366×768) or `mobile` (390×844) preset;
`rendering.browser.viewport_width` and `rendering.browser.viewport_height` override the
preset's dimensions with an exact pixel pair. The pair is all-or-nothing, bounded to
1 through 16384, and does not imply mobile or touch emulation — those remain explicit flags.
`rendering.browser.page_concurrency` bounds how many browser fetches a rendering escalation
runs at once, probes included, independently of `speed.concurrency` (which keeps governing
HTTP request politeness). The default of 1 is the sequential behaviour older runs had.
Probes and full renders in one run always share the same engine, viewport and emulation, and
the effective values are recorded in the run manifest/fingerprint and in each rendered
document's renderer provenance — including attempts that failed, which record what was
requested rather than inventing a successful render.

The renderer supports GET, HEAD, and OPTIONS, with a 5 MiB limit on each response's encoded
HTTP body. A blocked WebSocket, unsupported method, refused destination, or exceeded response
limit makes the render explicitly unavailable. Persistent browser profiles are currently
unavailable; requesting one returns a named error without opening its directory. Stored DOM
limits and credential-sensitive retention rules still apply separately.

`robots.policy` accepts `respect` (obey), `report_only` (fetch it, report what it would block, crawl
anyway — the honest audit setting), and `ignore` (do not fetch it at all).

Environment overrides: `SEOHEAD_CRAWL_MAX_URLS`, `SEOHEAD_CRAWL_MAX_DEPTH`,
`SEOHEAD_CRAWL_MIN_DELAY`, `SEOHEAD_CRAWL_ROBOTS`, `SEOHEAD_CRAWL_USER_AGENT`.

## Run journal

Every CLI command and MCP call is appended to one JSONL journal, so a session can be
reconstructed after the process exits: which tools ran, against what, how long they took, and
whether they failed.

```bash
SEOHEAD_RUN_LOG=./runs.jsonl seohead crawl-site --url https://example.com/
SEOHEAD_RUN_LOG=off seohead parse --url https://example.com/    # disable
```

Default path is `~/.config/seohead/runs.jsonl`. Journaling wraps the shared handler registry
rather than each interface, so both faces of the toolkit record exactly once and a new tool
cannot be added without being recorded.

Arguments whose names look like credentials — token, key, secret, password, auth — are stored as
`[redacted]`, and long values and lists are shortened rather than dropped. A journal that leaks an
API key would leak it silently, since nothing about a log file suggests it holds secrets.

Each entry carries a `fingerprint` of the call: the same tool with the same arguments produces the
same value regardless of argument order. Reuse is off by default. A tool answers from the journal only
when `SEOHEAD_REUSE_POLICY` names it with a maximum age, e.g.
`SEOHEAD_REUSE_POLICY='{"domain_profile": 86400}'`. The answer must be a successful entry no older than
that age, and it is always marked `reused: true` in the result and in the journal entry.

An unwritable journal never fails a run: a degraded observation is not a failed audit.

## Environment variables

Names only — values are secrets and never belong in a repo, a log or a doc.

Tool behaviour:

| Variable | Purpose |
|---|---|
| `SF_CLI`, `SCREAMINGFROG_CLI` | explicit path to the SF CLI executable for audit mode A (`seohead/sf/core/runner.py`) |
| `SEOHEAD_TECH_DB` | path to an external technology-fingerprint database; not shipped for license reasons (`recon/tech_db.py`) |
| `SEOHEAD_RUN_LOG` | where the run journal is written (default `~/.config/seohead/runs.jsonl`); `off` disables it |
| `SEOHEAD_HTTP_CACHE_DIR` | directory of the crawl HTTP cache (default `~/.cache/seohead/http_cache`); `off`, `0`, `none` or `false` disables the cache. The policy is RFC 9111-style: `no-store` is never stored, unstated freshness is treated as stale, and a stored `ETag`/`Last-Modified` is revalidated with a conditional GET (`304` counts as a revalidation). `User-Agent` is always part of the key (`seohead/crawl/cache.py`) |
| `SEOHEAD_REUSE_POLICY` | opt-in reuse of journal answers, as a JSON object of tool name to maximum age in seconds, e.g. `'{"domain_profile": 86400}'`. Default is empty: nothing is reused. A malformed value disables reuse silently. Reused answers carry `reused: true` and `reused_from_ts`, and results over 20 000 bytes are not stored for reuse (`seohead/core/runlog.py`) |
| `SEOHEAD_SPEND_LOG` | override for the paid-call journal (default `~/.config/seohead/spend.jsonl`) |
| `DATAFORSEO_ENV` | `sandbox` (default) or `prod` for the DataForSEO tools |
| `SEOHEAD_CHROME` | explicit Chrome, Edge or Chromium executable that `metrika-traffic-pdf` uses to print its PDF; otherwise standard install locations and `PATH` are searched |
| `GSC_SERVICE_ACCOUNT_FILE` | private service-account JSON path for the optional GSC service-account path |

Credentials (each wins over its file under `~/.config/`; see
`seohead/data_sources/credentials.py`):

| Variable | File fallback | Used by |
|---|---|---|
| `ARSENKIN_TOKEN` | `~/.config/arsenkin/token` | `keywords-exact` |
| `YANDEX_CLOUD_API_KEY` | `~/.config/yandex-wordstat/api_key` | `keywords-expand`, `keywords-seasonality`, `serp-fetch` |
| `YANDEX_CLOUD_FOLDER_ID` | `~/.config/yandex-wordstat/folder_id` | same |
| `YANDEX_METRIKA_TOKEN` | `~/.config/yandex-metrika/token` | `metrika-*` |
| `DATAFORSEO_LOGIN` | `~/.config/dataforseo/login` | `google-keywords`, `google-serp` |
| `DATAFORSEO_PASSWORD` | `~/.config/dataforseo/password` | same |
| `GSC_ACCESS_TOKEN` | `~/.config/gsc/access_token` | GSC OAuth bearer path |
| `GSC_SERVICE_ACCOUNT_FILE` | `~/.config/gsc/service-account.json` | optional `.[gsc]` service-account path |
| `MIRATEXT_API_KEY` | `~/.config/miratext/api_key` | `miratext-analyze`; free queue by default, paid modes require explicit confirmation |

`seohead sources-doctor` reports which of these are present and where they
are read from — run it before planning any paid collection.

### Search Console service account

Install the optional dependency with `python -m pip install -e ".[gsc]"`. Keep the service
account JSON outside the repository at a private regular file such as
`~/.config/gsc/service-account.json` (mode `0600`), or set
`GSC_SERVICE_ACCOUNT_FILE=/private/path/gsc-service-account.json`. The JSON must be a Google
`service_account` document whose token URI is exactly `https://oauth2.googleapis.com/token`.
`seohead sources-doctor` reports the local state as `service_account_status` (`missing`,
`unsafe_file`, `too_large`, `unreadable`, `malformed_json`, `unsupported_shape`, or
`configured_unverified`) without exposing the document's contents.
SEOHEAD uses `google-auth` to obtain a short-lived token with only
`https://www.googleapis.com/auth/webmasters.readonly`; it does not create or sign JWTs itself.

Add the service account email as an owner or permitted user of each required Search Console
property. A valid service account only authenticates an account: `provider_verify` still reports
property access separately as `verified`, `not_granted`, or `unknown`. Do not put the JSON file,
private key, service-account email, a token, or a property name in a report, fixture, or Git.
Remove the local file and its property permission to revoke this path.

### Search Console OAuth login (browser consent)

Use this path when you want a personal read-only grant instead of a service account, for example
for a property you own in your own Google account. Run `scripts/gsc_oauth_login.py` once per
machine:

1. In Google Cloud Console create an OAuth client of type Desktop app and download its JSON
   to a private path outside the repository. The script rejects any client without the
   `installed` key, which is what Desktop app clients produce.
2. Run `python scripts/gsc_oauth_login.py --client-file /private/path/client_secret.json`.
3. Approve the read-only scope `webmasters.readonly` in the browser. The script receives the code
   on `127.0.0.1` and does not print tokens or secrets.
4. The refresh token is stored in `~/.config/gsc/oauth.json` through `oauth.save_grant("gsc", ...)`.
   The script then lists the Search Console properties the account can read, with their permission
   level.

The script refuses to run if a grant already exists. To reconnect, disconnect first with
`seohead provider-auth gsc disconnect`. Check the state with `seohead sources-doctor`. A listed
property still needs to be verified with `provider-verify` before collection. Do not paste the
client JSON, the refresh token, or the authorization code into chat, reports, or Git.

## Docker alternative

No local Python needed:

```bash
docker compose run --rm seohead sf run --exports-dir /data/exports --out /data/report --tasks
docker compose run --rm seohead headers-check --url https://example.com
```

The image is a multi-stage build on `python:3.12-slim`, runs as non-root user `seohead`, and mounts
`./workspace` as `/data`. It does not bundle Screaming Frog or Chromium. See
[USAGE.md](USAGE.md).

## What is intentionally absent

- **Local installation remains CLI and stdio MCP.** The optional authenticated
  [remote API](REMOTE_API.md) and [durable job backend](REMOTE_JOBS.md) require
  explicit service construction and start no listener on installation. Reports are files.
- **No push deploy.** `git push` deploys nothing — there are no deploy
  workflows, hooks or scripts in this repo.

## Platform-specific launch instructions

See [Windows, Linux, and macOS](PLATFORMS.md) for explicit virtual-environment
commands, MCP executable/argument configuration, Unicode paths, and native
SQLite filesystem guarantees.
