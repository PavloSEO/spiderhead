# Provider evidence — connect, verify, collect, join, and revoke

## The question

> Which external evidence can change the order of this technical backlog, and what would make it
> safe to use?

A token or API key only says that a local machine has configuration. It does not establish which
account it reaches, which property the account may read, whether a response was sampled, or
whether the data belongs to this audit. Keep those questions as separate steps.

## The chain

**1. Inspect configuration without exposing it.**

Use `provider_registry()` to see each provider's credential components, access class, operations,
quota mode, and privacy class. Use `provider_doctor()` to distinguish `not_configured`,
`credential_present`, and `not_required`. Neither operation contacts a provider, and neither says
that credentials have been verified.

**2. Connect a read-only grant deliberately.**

Store secrets outside the repository in the configured local credential location. GSC accepts
either a durable OAuth grant or the optional `.[gsc]` service-account path. A service-account JSON
is a private `0600` file such as `~/.config/gsc/service-account.json` (or a synthetic private path
named by `GSC_SERVICE_ACCOUNT_FILE`); its service-account email must separately be granted access
to each required property. Both paths request only `webmasters.readonly`; OAuth refresh tokens,
client secrets, service-account private keys, and raw property names remain local. DataForSEO backlinks remain disabled until account eligibility, a
cache key, a spend ceiling, production approval, and cost approval are all declared. IndexNow is a
separately confirmed write action and is not part of provider collection.

**3. Verify the account and the selected target separately.**

Call `provider_verify(provider, request)` as one explicit read. Its `authenticated_account` field
means the provider accepted the credential. Its `target_access` field is independently
`verified`, `not_granted`, `not_requested`, or `unknown`; an account listing does not prove access
to an arbitrary requested property, host, counter, or site. Unknown scopes and quota state stay
`unknown` rather than being reported as empty or unlimited.

Two provider checks have narrow, unbilled contracts. `yandex_cloud` calls only
Wordstat `GetRegionsTree`; success verifies that the configured API key/folder can access that
metadata method, while paid Wordstat methods and Web Search remain separately unverified. Yandex
documents the region tree as free ([pricing update](https://yandex.cloud/en/blog/digest-april-2026))
and requires the Search API service-account role for this call
([method reference](https://yandex.cloud/en/docs/search-api/api-ref/grpc/Wordstat/),
[GetRegionsTree instructions](https://aistudio.yandex.ru/en/docs/search-api/operations/wordstat-getregiontree)).
`dataforseo_backlinks` reads only the free `appendix/user_data` account metadata endpoint; it
returns the configured environment and whether quota metadata was available, never the account
balance, rates or prices ([DataForSEO endpoint](https://docs.dataforseo.com/v3/appendix-user-data/)).
Neither result proves that a paid collection is approved or should be started.

Cloudflare token verification calls `GET /user/tokens/verify`; an active response verifies the
token itself, not its zone/account scopes or access to a selected zone. See the
[Cloudflare Verify Token reference](https://developers.cloudflare.com/api/resources/user/subresources/tokens/methods/verify/).
Cloudflare accepts the token from `CLOUDFLARE_API_TOKEN`, the standard
`~/.config/cloudflare/api-token` file, or the file selected by
`SEOHEAD_CLOUDFLARE_TOKEN_FILE`; readiness returns the selected reference without its path or value.

`project-sources-list` also reads the local `sources.sqlite` and `cloudflare.sqlite` ledgers. Each
selected binding keeps its exact provider resource ID and reports a lag-adjusted 28-day coverage state. Local history
resources without a project binding appear as `not_selected`. A failed sync without retained
provider permission detail remains access `unknown`; it is not guessed to mean revoked access.

The local stdio MCP server has no trusted caller identity, so OAuth mutations and credential
revocation are denied through MCP even when the tool appears in its schema. Use the explicit local
CLI workflow for those owner-only actions. MCP can read credential status and provider evidence.

**4. Collect one bounded operation.**

Call `provider_collect(provider, operation, request, artifact_dir=...)`. It produces a
`seohead.provider-evidence.v1` envelope with a status of `complete`, `partial`, `failed`, `skipped`,
`no_field_data` or `not_configured`; a status outside that set is mapped to `complete` or `failed`. GSC analytics is locally row-bounded; URL Inspection is a declared sample, never an
index census; PSI is a bounded mobile-first lab sample; CrUX History is field data; GA4 sessions
are not search clicks; Metrika raw Logs have no route. A supplied `artifact_dir` keeps raw rows and
identifiers in a restricted local artifact. The public envelope has a null target reference and
redacts filter values.

**5. Join after collection, without changing a crawl.**

Call `provider_join(crawl_pages, evidence_rows, review_external_only=...)`. Exact URL matching is
the default. The result preserves `matched`, `crawl_only`, `external_only`, and `unkeyable`
populations. `list_crawl_candidates` exists only after `review_external_only=true`; it does not
change a frontier or call an external-only URL orphaned by itself.

A priority adjustment must name its source fields, period, coverage, and adjustment. `partial`,
`sampled`, `truncated`, `unmatched`, and `privacy_thresholded` evidence is unavailable for that
purpose, never zero. An accepted adjustment has `technical_severity_changed: false`: it changes
work order, not the underlying technical finding.

**6. Refresh or revoke without hiding a change.**

Refresh a durable GSC grant only for an explicit provider operation. If a grant expires or a
provider returns an authorization failure, record `failed` or `not_configured`; do not reuse old
results as current evidence. Revoke the grant at the provider, then remove the restricted local
grant file and rerun `provider_doctor()` to confirm `not_configured`. Retained evidence keeps its
retrieval time and local artifact reference, so it remains historical rather than silently current.

## What comes out

A provider evidence envelope proves what was asked, when it was retrieved, its coverage limits,
and whether it is usable. It never proves a clean technical result just because a provider was not
configured, a target was not granted, or a result was truncated.

## Covers

- **Search Console** — Orphan URLs · URL is on Google, But Has Issues · AMP URL is Invalid · Rich Result is Invalid · Indexable URL Not Indexed · User-Declared Canonical Not Selected · No Search Analytics Data · Non-Indexable with Search Analytics Data · URL Is Not on Google
- **Analytics** — Orphan URLs · Bounce Rate Above 70% · No GA Data · Non-Indexable with GA Data

Retained provider evidence can add these page-level findings to the normal SF issue catalogue.
Absence findings require complete retained coverage and a complete crawl; bounded URL Inspection
findings describe only the inspected sample. GA4 does not provide the per-URL bounce metric in
the project-history join, while Metrika does.

## What it cannot answer

- **Whether an authenticated account owns a requested target.** Only `target_access: verified`
  establishes that selected target.
- **Whether a sampled or thresholded traffic value is zero.** It is unavailable prioritization
  evidence.
- **Whether a URL is orphaned.** External-only means only that the current crawl did not match it.
- **Whether IndexNow caused indexing.** A receipt records a submission, not an indexing outcome.

## Durable GSC grant lifecycle

Obtain a grant with only `webmasters.readonly` through Google's OAuth consent
flow, then store the JSON outside the repository with mode `0600`. Its fields
are `refresh_token`, `client_id`, `client_secret`, and `scopes` (a one-element
list containing `https://www.googleapis.com/auth/webmasters.readonly`). Do not
paste these values into command-line arguments, reports, or agent messages.

```bash
seohead provider-auth --provider gsc --action connect --grant-file ./private-gsc-grant.json
seohead provider-auth --provider gsc --action status
seohead provider-auth --provider gsc --action refresh
```

Connect imports an already-consented grant; it does not verify property access.
The explicit refresh command returns only expiry/scope metadata. Subsequent GSC
reads refresh the saved grant when no explicit bearer is configured. A rejected
or expired grant requires renewed consent. Existing grants are never overwritten
by connect. To remove only the local grant, use `--action disconnect --confirm`.
To revoke it at Google and then remove it locally, use `--action revoke --confirm`;
a failed remote revocation preserves the local grant. These actions do not change
environment bearer tokens or service-account keys.

Google documents consent, refresh and revocation in its
[OAuth web-server flow](https://developers.google.com/identity/protocols/oauth2/web-server).

## Replay an enrichment without another API call

Collection with `--artifact-dir` writes a private envelope containing provider
provenance and raw rows. The normal response contains redacted evidence only.
Keep the envelope private; do not put it in a client report.

```bash
seohead provider-replay --scan ./scans/audit.sqlite \
  --evidence-file ./private/provider-collection.json --out-dir ./private/joins
```

Use the actual saved filename from the artifact directory. GSC page dimensions
are mapped to their recorded URL; other providers use `url` unless an explicit
`--url-column` names the URL field. Relative or absent keys remain unkeyable.
The saved join records scan UUID, source-file checksum, period and provider
coverage. Its response gives matched and unmatched population counts, never a
new crawl or an automatic priority change. `--review-external-only` writes
separate candidate URLs in the private join; admitting them requires a later
explicit list crawl.

## Normalize imported evidence for a scan join

`evidence-normalize` turns a supplied CSV/XLSX/JSON export — or a saved private
provider envelope — into one `seohead.normalized-evidence.v1` document, fully
offline. An optional `seohead.evidence-mapping.v1` manifest declares the
source, URL field or dimension, metrics and units, dimensions, inclusive
period, timezone, attribution and collection coverage; every resolved field
records whether it came from the envelope, the manifest, a provider contract,
or remains unknown. A supplied `0` stays a measured zero; an empty cell, a
missing key, JSON `null`, a formula cell, a suppressed placeholder and a
never-collected row each stay a distinct unavailable state — never zero. GSC
page dimensions key on their absolute URL; GA4 `landingPagePlusQueryString`
values stay unkeyable until `--site-origin` declares the binding explicitly.
Rows keep their declared grain: a page+query row is never collapsed into its
page, and duplicate natural keys are marked ambiguous, not summed.

```bash
seohead evidence-normalize --file ./gsc.csv
```

`evidence-join` joins the normalized document to `--pages`, `--scan`, or
`--audit` under the strict `external_join.v1` key policy; the
`--ignore-query`/`--ignore-scheme`/`--casefold-path` relaxations are explicit
opt-ins, never defaults. The result preserves `matched`, `crawl_only`,
`external_only` and `unkeyable` populations with their provenance, and reports
normalization collisions on both sides instead of multiplying metrics across
rows that share a key.

```bash
seohead evidence-join --scan ./scans/audit.sqlite --evidence ./gsc.csv
```

When the saved scan has more than 100,000 retained URLs, add an explicit private output directory:

```bash
seohead evidence-join --scan ./scans/audit.sqlite --evidence ./gsc.csv --out-dir ./private-evidence
```

The durable join is a local `seohead.evidence-join-sqlite.v1` artifact. Its URL provenance is a
cursor-backed edge table, so a provider row with many matching crawl URLs is not converted into a
large JSON list. The response gives its exact `join_store.file_name`; pass that real file in the
same private directory to `bi-export --provider-join` and keep it beside the BI package for
provenance review.

`--compare` plus a declared `--policy` adds a pure compatibility decision
(`compatible`, `incompatible` or `unknown`) across inclusive period, timezone
boundary, reporting identity, attribution, search engine, and grain. GSC
clicks or impressions and GA4 sessions stay distinct labeled metrics on their
own axes — a declared `cross_source: "juxtapose"` policy may juxtapose them on
one URL and a compatible window, but never sums them or treats sessions as
clicks. A cross-source quadrant is eligible only when both sources supplied a
measured numeric value at the same URL key under a declared boundary policy;
unavailable values cannot enter it. Restricted sources return counts and
redacted provenance only; full populations live inside an explicit `--out-dir`
private artifact.

## Yandex Webmaster operations

`provider-collect --provider yandex_webmaster` reads API v4 only. `user_id` is resolved from the
token when the request omits it; `host_id` has the form `https:example.com:443`. Operations:
`hosts`, `summary` (SQI, pages in search, problems), `diagnostics`, `sqi_history`,
`search_history` (shows and clicks by day), `search_performance` (popular queries),
`query_history` (one query by day; `query_id` from `search_performance`),
`in_search_history`, `events_history`, `indexing_history`, `important_urls`, `sitemaps`
(nested files via `params={"parent_id": "..."}`), `crawl` (appeared/removed samples), `indexing`
(downloaded pages by HTTP code), `broken_links_samples`, `broken_links_history`,
`external_links_history`.
`params` becomes the query string — a list repeats the key, e.g.
`{"query_indicator": ["TOTAL_SHOWS", "TOTAL_CLICKS"], "date_from": "2025-10-01"}`; with
`"paginate": true` the query and sample lists are read page by page (500 per page for queries,
100 for the sample lists — the documented API caps — up to `max_rows`, 50 000 by default; the
last page requests only the remaining row budget).
Each paged response is validated against the documented `count` and list shape; `truncated`
marks rows left unread (a `max_rows` cut, a single first page, or a page that ended before
`count`) and reports `partial`, while a page without the documented list or count, or whose
`count` is below the rows it returned, fails —
a paged operation never reports a complete zero-row collection while rows remain.

## Bing Webmaster response and page coverage

Bing site, crawl-issue and query-stat responses contain an array of row objects. A null,
scalar or malformed row container is failed evidence; an explicit empty array is a valid
zero-row response. The saved result preserves the original `data` and exposes the same
source rows in `rows`, with an exact `returned` count. Numeric zero and null metric values
remain distinct, and raw rows stay in the restricted artifact.

The [GetLinkCounts JSON contract](https://learn.microsoft.com/en-us/dotnet/api/microsoft.bing.webmaster.api.interfaces.iwebmasterapi.getlinkcounts?view=bing-webmaster-dotnet)
instead wraps its row array in `Links` with `TotalPages`. SEOHEAD reads one explicitly requested,
zero-based `page`; the saved result retains `page`, `total_pages` and `next_page`. Any response
from a multi-page population is partial/truncated, including the last page because earlier
pages were not collected by that call. An empty selected page does not prove an empty link
population. A valid zero-page envelope has empty `Links`; inconsistent page metadata fails
without inventing a complete result. This operation never starts a pagination loop implicitly.
