# Changelog

## Unreleased

- Crawl: `crawl-site` takes URL lists through `--urls` and `--urls-file`; `resources.fetch` runs on the native crawl (scan.v2); the external checks phase runs on the native crawl.
- Reports: the audit PDF is rendered natively; `boilerplate-report` groups a retained corpus by chrome hashes; reports can be pinned.
- Sources: Microsoft Clarity is available as a provider; `google-keywords` refreshes the price snapshot when a spend ceiling needs it.
- Semantics: `cluster-google` tools drop Google tracking (goto) tokens from SERP overlap.
- CLI: `version` command.
- CI: fixtures are host-independent; the Linux CI leg gates the build.

## 0.9.1

- Semantics: Google and DataForSEO data are stored per language and location and linked to topics across engines; `import-google-csv` loads per-language keyword tables; `cluster-google` and `clusters-google` cluster Google phrases by SERP; non-Latin queries are kept during normalisation.
- Crawl: content-encoded response bodies are decoded before parsing; `--set`, JavaScript rendering and browser lookup on `PATH` are wired; `--config-help` prints the crawl settings reference.
- Reports and checks: `scan-broken-links` writes a CSV/XLSX broken links and resources report; `scan-extract` pages by offset with total and next offset; `INTERNAL_LINK_NO_ANCHOR` skips links with an image alt, `aria-label` or `title`.
- Logs: format detection for nginx tails, custom `log_format`, tabular and cloud log layouts.
- MCP: a bare `mcp` invocation starts the stdio server.
- Release workflow: pushing a `vX.Y.Z` tag builds the macOS arm64 CLI and creates a draft GitHub Release with `SHA256SUMS`.
- `spiderhead update`: verified in-place update of the installed binary (atomic replace, no change when current); `update-check` accepts `SHA256SUMS` as the checksum file.
- Large frozen test fixtures are stored compressed.

## 0.9.0

First public snapshot. Earlier history was a raw prototype and is not published; history starts at 0.9.

- CLI with 250+ commands: log analysis, crawl, parse, projects, data sources, semantics, reports.
- Log analysis: import, summary and bot reports with IP-range, reverse-DNS and ASN verification.
- `spiderhead mcp` runs the stdio MCP server from the same binary; `spiderhead-mcp` stays as an alias.
- Project model: `seo/` folder, `spiderhead init [--site URL]`, autodiscovery from the current directory, `schema_version` in `project.json`.
- Desktop app (early): launcher with log analysis enabled, menu actions to install the command in the terminal and to connect an MCP client.
