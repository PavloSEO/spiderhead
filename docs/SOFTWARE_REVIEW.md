# Software review pack

Evidence for an internal security, legal, or procurement review of SEOHEAD Tools
as self-hosted software. Every artifact described here is generated from a
tagged build and is verifiable locally. This document states what the software
does today; it does not promise zero vulnerabilities, perpetual support, or
regulatory compliance.

## What the pack contains

| File | Contents |
|---|---|
| `dependency-inventory.json` | One profile per install group (`core` plus every optional-dependency group in `pyproject.toml`). Each profile records the declared requirements verbatim, the packages resolved and installed in the generating environment (name, version, license, and which metadata field the license came from), declared requirements excluded by environment markers, and declared requirements not installed in that environment. Resolved versions are measured, never guessed. |
| `release-provenance.json` | The release-level record: tag, package identity, each artifact's SHA-256 and size, the package name/version its filename declares, the `Name`/`Version` fields of its `*.dist-info/METADATA` or `PKG-INFO` — the identity an installer actually reads — and what the embedded build manifest inside each wheel and sdist proved — the producing Git revision, the per-file source hashes it validated, and whether the version it records matches this release. |
| `SHA256SUMS.txt` | SHA-256 over the two files above and every built distribution, in the standard `sha256sum -c` format. The layout is flat so a downloaded set verifies without recreating a directory tree. |

Two provenance layers exist and should not be blurred. The embedded manifest
(`seohead/_build_provenance.json` inside each distribution, validated at build
and again at runtime by `seohead/_build/provenance.py`) is per-file source
provenance. `release-provenance.json` summarizes it for the release and adds the
tag and artifact digests. Neither is a cryptographic attestation or a
signature.

## How it is produced and verified

```bash
python -m build
python scripts/generate_review_pack.py generate --dist dist --tag v3.0.0 --out review-pack
python scripts/generate_review_pack.py verify --pack review-pack
sha256sum -c review-pack/SHA256SUMS.txt   # inside the pack directory
```

- The tag must equal `v<version>` from `pyproject.toml` or generation refuses —
  the same consistency `.github/workflows/release.yml` enforces before a real
  release.
- `verify` re-checks document formats, tag/version/package consistency, every
  recorded hash, and what is inside each distribution — re-extracting each
  wheel and sdist to confirm the embedded manifest still validates, that the
  package version it records matches `pyproject.toml`, and that exactly one
  `METADATA`/`PKG-INFO` declares a `Name`/`Version` equal to `pyproject.toml`
  after normalization. Filename, metadata, and embedded manifest are judged
  independently, so a stale or foreign distribution in the set fails
  verification rather than reporting a clean result.
- CI regenerates and verifies the pack on every run (`.github/workflows/ci.yml`,
  the `review-pack` job) using a disposable tag value derived from
  `pyproject.toml`. It never pushes a tag or creates a release. On a real tag
  push, `release.yml` regenerates, verifies, and attaches the pack files to the
  GitHub release.

**Sentinel scan.** Any environment variable whose name starts with
`SEOHEAD_REVIEW_CANARY_` is treated as a planted credential or client marker.
If its value appears in a pack file or in the generator's own diagnostics, the
command fails and names the variable — never the value. CI sets two sentinels
(credential-shaped and client-domain-shaped) on every run, which is how
"diagnostics omit credentials and client data" stays a tested property.

## Outbound destinations

Nothing leaves the machine unless the operator runs the feature. There is no
background traffic: no telemetry collector, no crash reporter, no daemon. The
update check is caller-invoked and requires explicit consent; the core does not
start a scheduler. The MCP server is a stdio child process, not a network service.

| Destination | Reached by | What is sent | Optional? |
|---|---|---|---|
| Operator-supplied target websites | `crawl-site`, `parse`, `headers-check`, `robots-check`, `sitemap-crawl`, `links-check`, `hreflang-check`, `redirects-check`, `asset-weight-check`, `images-download`, `render-check`, `soft404-check`, `security-check`, `mirror-check`, `backlinks-check`, `tech-detect`, `llms-txt-check`, `citability-check`, `site-audit`, `sf run --crawl` (via the installed Screaming Frog binary) | HTTP(S) requests with the SEOHEAD user agent; optional host-bound credential headers. `crawl-site` sub-resource fetches stay within the crawl start origin; URL-list tools such as `images-download` and `backlinks-check` fetch each supplied URL on whatever public hosts the operator lists | Every call is operator-invoked |
| DNS-over-HTTPS resolvers `cloudflare-dns.com`, `dns.google`; the OS resolver | `domain-profile`, `cdn-check`, `regions-check`, `log-analyze --verify-bots`, every target fetch | The names being looked up | Operator-invoked; `--verify-bots` is an explicit flag |
| RDAP `rdap.org` and the registry it delegates to; WHOIS servers via the system `whois` binary (incl. `whois.tcinet.ru` for .ru/.su and the Cyrillic .rf zone in punycode) | `domain-profile` | The queried domain | Operator-invoked |
| Direct TLS handshake to the target host | `domain-profile` certificate check | TLS ClientHello with the target hostname | Operator-invoked |
| `api.dataforseo.com` / `sandbox.dataforseo.com` | `google-keywords`, `google-serp`, `provider-collect dataforseo_backlinks` | Keywords/queries plus the provider credential | Paid; sandbox is the default, `DATAFORSEO_ENV=prod` is explicit |
| `searchapi.api.cloud.yandex.net`, `operation.api.cloud.yandex.net` | `keywords-expand`, `keywords-seasonality`, `serp-fetch`, `regions-tree`, `provider-collect yandex_cloud` | Keywords, region IDs, SERP queries plus the API key | Paid; asynchronous endpoint only |
| `arsenkin.ru/api/tools` | `keywords-exact`, `provider-collect arsenkin` | Keywords plus the account token | Paid credits |
| `api-metrika.yandex.net` | `metrika-counters`, `metrika-setup`, `metrika-report`, `metrika-traffic-pdf`, `provider-collect metrika` | Counter IDs, report parameters, counter settings plus the OAuth token | Credentialed |
| `api.webmaster.yandex.net` | `provider-collect yandex_webmaster` | Site and operation parameters plus the OAuth token | Credentialed |
| `www.googleapis.com/webmasters/v3`, `searchconsole.googleapis.com` | `gsc-query`, `provider-collect gsc` | Site URL, query parameters plus the OAuth credential | Credentialed |
| `analyticsdata.googleapis.com` | `provider-collect ga4` | Property and report parameters plus the OAuth token | Credentialed |
| `chromeuxreport.googleapis.com`, `www.googleapis.com/pagespeedonline` | `crux-report`, `provider-collect crux`, `provider-collect pagespeed` | The URLs/origins queried plus the API key | Credentialed |
| `ssl.bing.com/webmaster/api.svc` | `provider-collect bing_webmaster` | Site and report parameters plus the API key | Credentialed |
| `oauth2.googleapis.com/token`, `oauth2.googleapis.com/revoke` | `provider-auth` grant lifecycle | The stored refresh token during exchange/revocation | Credentialed |
| `api.indexnow.org/indexnow` | `indexnow-submit` | The submitted URL list plus the submission key | Confirmed write; disabled by default |
| `web.archive.org/cdx/search/cdx` | `wayback-history` | The queried URL | Public service, no credential |
| `crt.sh` | `crtsh-subdomains` | The queried domain | Public service, no credential |
| `api.github.com` and `github.com` release assets | `update-check`, `update-download` | Public release metadata, then one explicitly selected asset name | Explicit caller consent; downloads are checksum-verified and never installed |

What is **not** sent anywhere: crawl evidence, scan artifacts, retained page
bodies, report files, the run journal, the spend ledger, or provider
credentials to any destination other than their own service. Offline
operations — export analysis, reanalysis, comparisons, report rendering, scan
storage, and this pack's own generation and verification — make no network
request at all.

## Local/remote boundary

- The toolkit opens no inbound port and ships no hosted control plane; the local
  interfaces are the `seohead` CLI, the stdio MCP server and the optional `seohead watch` terminal observer.
- User-controlled URL requests resolve once, refuse private and non-public
  targets, and connect to the vetted address while retaining the original
  hostname for SNI and certificate verification. Private targets require
  `SEOHEAD_ALLOW_PRIVATE_NETWORKS=1` or a named host in
  `SEOHEAD_ALLOW_PRIVATE_HOSTS`.
- Redirect targets are re-validated the same way; URLs with embedded
  credentials are rejected.
- Global `http.headers` refuses credential-bearing header names; authenticated
  crawl traffic uses host-bound `http.credential_headers` entries that reference
  environment variables rather than storing values.
- Rendering runs a sandboxed browser that refuses root, routes requests through
  the same validated transport, and blocks service workers.

## Credential and artifact storage

- Provider credentials come from environment variables or per-provider files
  under `~/.config/` (see `seohead/data_sources/credentials.py` and the table in
  [SETUP.md](SETUP.md)). Only names and paths appear in docs and error
  messages; values are never printed or logged. `seohead sources-doctor`
  reports which are present and where they are read from.
- The Google Search Console OAuth grant lives at `~/.config/gsc/oauth.json`
  with owner-only permissions.
- Local state: the run journal `~/.config/seohead/runs.jsonl`, the paid-call
  ledger `~/.config/seohead/spend.jsonl`, `config.json` and crawl cache beside
  the working directory, `audit.seospiderconfig`, project workspaces, report
  outputs wherever `--out` points, and `scan.v1` SQLite artifacts under
  `./scans/` (see [STORAGE.md](STORAGE.md) for contents and retention).
- **The run journal is not credential-free evidence.** Arguments whose names
  look like credentials are stored as `[redacted]`, but ordinary values —
  including the URLs and paths a run touched — are recorded verbatim. Treat
  journals and scan artifacts as client-identifying material: keep them out of
  review packs and public reports, exactly like Metrika log exports, which may
  carry visitor personal identifiers.

## Telemetry

None shipped. There is no telemetry collector, analytics beacon, phone-home,
automatic updater, or remote crash reporting. The run journal and spend ledger
are local files and nothing transmits them.

## Security reporting and maintenance

- Report suspected vulnerabilities privately through GitHub:
  <https://github.com/PavloSEO/seohead-tools/security/advisories/new> — details and
  the out-of-scope list are in [SECURITY.md](../SECURITY.md).
- Maintenance status: security fixes target the latest `3.x` release and
  `main`; versions older than `3.0.0` are not supported. This is a current
  statement, not a perpetual-support commitment.
- The project is MIT-licensed source; being open source is a transparency
  property, not evidence of security. Evaluate the evidence pack, the CI gates,
  and the test suite on their own merits.

## Reviewing release changes

- `CHANGELOG.md` is assembled from one fragment per change under
  `changelog.d/`; `git log`/GitHub compare between tags shows the source diff.
- Regenerating the pack for the same tag reproduces the inventory, provenance
  record, and `SHA256SUMS.txt` when the built distributions and the installed
  environment are the same — resolved versions are measured from the generating
  environment, not pinned, so a build that resolved different dependency
  versions records the difference honestly. `verify` fails on any
  tag/version/hash inconsistency inside a pack.
- The installed package self-verifies its embedded manifest against wheel
  `RECORD`, so a wheel whose packaged files were altered fails runtime
  validation rather than silently running.

## Operator runbooks

This pack documents evidence, not operations. Install, upgrade, headless
runtime, public exposure, TLS, backup, and restore live in the runbook issues
and their docs — linked here rather than duplicated:

- Headless installation and upgrade runbook — issue
  [#783](https://github.com/PavloSEO/seotools/issues/783) (setup basics are in
  [SETUP.md](SETUP.md) today).
- Domain, TLS, reverse-proxy, backup and restore runbook — issue
  [#787](https://github.com/PavloSEO/seotools/issues/787).

Related: [ARCHITECTURE.md](ARCHITECTURE.md) (data flow and invariants),
[STORAGE.md](STORAGE.md) (artifact contents and retention),
[SECURITY.md](../SECURITY.md), [PROVENANCE.md](legal/PROVENANCE.md),
[THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md).
