# Testing

## How to run

```bash
.venv/bin/pytest              # full suite, ~3 s
.venv/bin/pytest -q           # quieter
.venv/bin/pytest tests/test_recon.py -q          # one file
.venv/bin/pytest tests/test_rules.py -k thin -q  # one test by name
```

If the venv does not exist, see [SETUP.md](SETUP.md) — nothing is installed
by the tests themselves. The first public snapshot contained **458 offline tests**; the current
suite contains **over 1500 offline tests**. Runtime varies
with Python version and installed extras. CI (`.github/workflows/ci.yml`) runs the same suite on
Python 3.10, 3.12, and 3.13 plus the gates described below.

PDF structure and extraction tests need the optional parser and run separately from the
core test matrix:

```bash
uv sync --locked --extra dev --extra pdf
.venv/bin/pytest tests/test_pdf_validation.py -q
```

## The one rule: offline or it does not exist

No test goes to the network. Mocking is done with `monkeypatch`, fake
clients and fixtures — e.g. `test_data_sources.py` feeds `_FakeClient`
objects into the Arsenkin batch runner; `test_llms_txt.py` patches the HTTP
client factory. Verdict logic is deliberately factored into pure functions
(`compare()` in `seohead/checks/render.py`, `_findings()` in `recon/regions.py`,
`detect_bot()` in `seohead/checks/logs.py`) so tests never need a browser or a
socket.

Why: a suite that needs the internet goes green on the developer's machine,
falls apart on CI, and eventually nobody trusts it. The price is that the
thin network/browser wrappers themselves are only covered indirectly.

**A new recon or data-source tool without an offline test is not accepted**
(see [ARCHITECTURE.md](ARCHITECTURE.md)).

## What the suite covers

Grouped by area (file names under `tests/`):

- **SF audit core**: `test_loader.py` (export discovery/normalization),
  `test_rules.py`, `test_check_coverage.py`, `test_inlinks.py`,
  `test_heuristics.py`, `test_normalize.py`, `test_context.py`,
  `test_aggregate.py`, `test_sitemap_coverage.py`, `test_sitemap_txt.py` —
  the 281 checks over the synthetic crawl in `tests/fixtures/`.
- **Audit outputs**: `test_reporters.py` (audit.json validates against
  `sf/schema/audit.schema.json` — the contract test), `test_sf_config.py`
  (threshold/profile resolution), `test_tasks.py` (backlog building).
- **Mode A plumbing**: `test_sf_discovery.py`, `test_runner_throttle.py`
  (the generated `.seospiderconfig` rate limit), `test_url_sources.py`.
- **Live tools**: `test_tools_core.py` (parse/robots/redirects/…),
  `test_schema_org.py` + `test_schema_build.py` + `test_page_facts.py` +
  `test_page_type.py` (the Schema.org stack incl. the 1010-type
  vocabulary and the page classifier),
  `test_duplicate.py` (simhash + LSH), `test_logs.py`, `test_render.py`
  (raw-vs-DOM `compare()`), `test_soft404.py`, `test_social_meta.py`,
  `test_llms_txt.py`, `test_citability.py`.
- **Recon**: `test_recon.py` (normalization, date/whois parsing, CDN and
  tech signatures, security scoring), `test_tech_db.py` +
  `test_tech_categories.py` + `test_tech_coverage.py` (fingerprint
  database), `test_mirrors.py`, `test_ai_bots.py`, `test_regions.py`.
- **Bounded site audit & reports**: `test_site_audit.py` (the
  `seohead.site-audit/1` document assembly, severity rules, `tools_failed`
  semantics).
- **External data sources**: `test_data_sources.py` — credentials
  resolution, the spend journal, Arsenkin/Yandex/Metrika/DataForSEO
  clients against fakes, the DataForSEO geo guard.
- **Interface wiring**: `test_registration.py` (HANDLERS <-> CLI <-> MCP
  cross-check), `test_mcp.py` (starts a real stdio MCP server; skipped
  without the `mcp` package), `test_cli_stdin.py` (stdin handling,
  source-flag precedence).
- **Docs**: `test_docs_drift.py` — recounts tool/skill/check numbers in
  README, skills, and docs, and fails on references to
  non-existent commands. Documentation must rot loudly.
  `test_docs_commands_execute.py` goes one step further: it extracts every
  `seohead ...` invocation actually shown in README/docs/skills/examples
  (`scripts/doc_commands.py`) and runs each one for real against a loopback
  fixture site and copies of `docs/examples/` — a renamed or removed flag fails
  here even if no other test happens to exercise it. The handful that need
  real infrastructure (RDAP/DNS, a licensed SF binary, a paid provider
  credential, the never-returning `mcp` server) are at least parsed against
  the live argument parser instead.
- **Safety nets**: `test_public_safety.py` covers private-network URL
  blocking and image mutation safeguards.

## CI gates beyond pytest

From `.github/workflows/ci.yml` (all run on every push/PR to `main`):

1. **Collector/analyzer boundary**: fails if `seohead/crawl` imports `seohead.sf`, or `seohead/sf` imports `seohead.crawl`.
2. **Audit on examples**: `seohead sf run --exports-dir docs/examples/exports`
   must produce issues — the sample crawl must keep finding its planted
   problems.
3. **Faces come up**: `seohead --version`, `sf-analyzer --version`, and the
   MCP server must build and list tools (asserts `seo_parse`,
   `seo_domain_profile`, `sf_audit_run` among them).
4. **PDF acceptance**: the `pdf-validation` job runs synthetic page, extraction, model-count,
   page-limit, byte-limit, and missing-dependency checks on Ubuntu 24.04 with its preinstalled
   Chrome. It installs only the optional `pdf` Python extra in the job environment.

## What is deliberately not covered, and why

- **Real network behaviour of paid APIs** (Arsenkin, Yandex Cloud,
  Metrika, DataForSEO): clients are tested against fakes with representative response shapes.
  Live paid calls are deliberately excluded from the offline suite; DataForSEO defaults to its
  sandbox so integration checks can be performed without an accidental production charge.
- **The crawl browser itself**: Playwright runs are not part of the core suite;
  `render-check` is covered at the `compare()` level. `playwright` is not
  installed in the core CI matrix. PDF printing has a separate Chrome smoke job.
- **A real SF CLI crawl (mode A)**: only the generated config and discovery
  logic are tested; launching Screaming Frog requires a license.
- **CLI flag -> kwarg mapping for every command**: covered for stdin
  behaviour and spot-checked; there is no exhaustive per-command flag test.

## Missing tests, in the order they should be written

Descriptions only — write them when the corresponding code changes or
before extending that area.

1. **Per-command CLI smoke** (`seohead <cmd> --help` for all 161): the
   cheapest possible net against argument-parser typos — the exact class of
   the `soft404-check` bug that once left a tool dead with tests green.
   `test_docs_commands_execute.py` now covers this for every command that
   happens to appear in a documented example; this item is the remainder —
   every command, whether or not any doc shows it.
2. **Flag-mapping test**: for each command in `COMMANDS`, assert that the
   flags added in `_add_flags` survive `_build_kwargs` into handler kwargs.
   Today a flag that is silently dropped is invisible to the suite.
3. **DataForSEO sandbox integration test** (marked network, skipped by
   default): one real sandbox call per tool to verify shapes against the
   fake-based tests once an account exists.
4. **`report-build` over `docs/examples/reports/full.json` in every format**:
   the generators compute nothing, and a test should pin that property
   (same numbers in xlsx/docx/csv as in the source JSON).
5. **Spend-journal ordering**: an explicit test that `spend.record()` is called before response
   parsing in every paid handler; the invariant is documented in `AGENTS.md` and currently pinned
   most directly for Arsenkin.

## Fixtures

`tests/fixtures/` — a synthetic site export set with deliberate problems
(broken links, duplicates, thin content, sitemap desync). `conftest.py`
copies it into a temp dir per test (`exports_dir` fixture) and exposes a
ready `result` fixture that runs the full audit. Never edit fixtures to
make a check pass — the fixture is the ground truth the CI gate audits
against.

`tests/doc_fixtures/` — a small static site (`site/`) plus a loopback
`ThreadingHTTPServer` (`site_server.py`) that serves it, used only by
`test_docs_commands_execute.py` so live-URL commands from the docs have
something local and offline to point at.
