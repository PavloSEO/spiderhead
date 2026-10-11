# Scan storage (`scan.v1` and `audit.v2`)

A scan's collection artifact is one ordinary SQLite file for one crawl. Its format identifier is
`scan.v1`; its SQLite file signature is the normal `SQLite format 3\000` header,
and its identity is also recorded as `application_id=1397051208` (`SEOH`) and
`user_version=1`. A reader must require all three identifiers to agree before it
treats a file as a scan artifact.

Large saved audits can use an adjacent, versioned `audit.v2` SQLite companion named
`<scan-path>.audit-v2.sqlite`. It keeps the frozen `scan.v1`/`scan.v2` schema unchanged;
the companion binds to the scan UUID, evidence revision, and analyzer build. Scan history
counts the companion's disk bytes, excludes it from the scan-file catalogue, snapshots it
as a validated pair, and prunes it with its scan. Copy both files when moving an artifact.
The companion contains complete ordered collections, not a sample or a replacement for
missing evidence.

The legacy importer packages an existing crawl directory. A URL-mode `crawl-site`
run writes a native SQLite artifact under `./scans/` by default; `--scan-out` chooses
an explicit file. An explicit `--out-dir` or configured `output.dir` selects the
legacy directory workflow. Both retain the existing audit/report contract within
their declared materialization limits.
The directory collector writes page/link/form evidence incrementally. Its
materialized audit is currently limited to 10,000 pages, 20,000 forms and
1,500,000 links; a larger corpus retains its JSONL evidence and reports
`audit_available: false` with exact counts and the exceeded bound.
Use one file per imported run; do not merge runs or write an imported artifact
concurrently.

Scan artifacts are immutable evidence: the `ledger.v1` remediation ledger
([LEDGER.md](LEDGER.md)) reads a scan without writing it and tracks findings,
occurrences and their observation history in a separate file.

## Start here

For the operator capture-to-prune route and the current release-capacity status,
read [SQLITE_ACCEPTANCE.md](SQLITE_ACCEPTANCE.md). It carries the measured release
record, including the two limits that record publishes by name; the older
development profiles below are separate measurements, not a release-pass claim.

Use the storage module after it is installed with SEOHEAD Tools:

```bash
python -m seohead.storage import-run RUN_DIR --out scan.sqlite --producer-build SHA
python -m seohead.storage inspect scan.sqlite
python -m seohead.storage export-run scan.sqlite --out-dir NEW_DIR
seohead report-build --audit scan.sqlite --format md --out report.md
seohead compare-crawls --before before.sqlite --after after.sqlite
seohead sf tasks --json scan.sqlite --out tasks
```

`--producer-build` names the build that produced `RUN_DIR`. It is required because current legacy
output does not record that source build. It is provenance for the original
crawler, never the revision of the checkout that imports the directory. `inspect`
reports recorded format, provenance, lifecycle, capability states, and
partialness. `report-build`, `compare-crawls`, and `sf tasks` resolve a scan's
internal audit as their input and make no network request.

`export-run` requires a new, absent output directory. It writes only
`pages.jsonl`, `links.jsonl`, and `audit.json`; `audit.json` is published last as
the completion marker. The JSONL files are deterministic UTF-8 with sorted keys,
compact JSON, and one newline per row. Pages follow committed page order. The link export uses the existing static-plus-new-rendered-destination union in global `link_id` order; the database itself retains every raw/rendered occurrence. SQL `NULL` in a
late page field is omitted so an older source's absent field remains absent rather
than becoming a measured default. The saved audit is copied as its exact UTF-8
bytes. The export contains no response bodies, raw HTML, forms, robots state,
start-page evidence, sitemap-response corpus, or resume checkpoint.

The native default is `storage.body_mode=captured_entity_bytes`; the only other
supported value is `off`. The recorded retention policy, body state, and
capability state determine what a particular scan actually retained.

## Native capacity admission and retained experimental profiles

Explicit native SQLite site scans admit up to **1,000,000 URLs** through settings,
CLI, MCP, remote submission and workers. Values above the shared ceiling fail
before collection; request budgets may separately reach 2,000,000 attempts to
cover robots, sitemaps, redirects and retries. Default request and politeness
settings stay unchanged. Admission alone does not establish capacity: see the
[source-bound end-to-end gate](MILLION_CRAWL_ACCEPTANCE.md).

The compatible `storage.capacity_profile="experimental_synthetic"` marker is
reserved for direct `NativeScan` storage fixtures. Its limit is also 1,000,000;
public crawl handlers still refuse the marker. Existing stored fingerprints,
including scans written before the marker existed, remain valid.

Materialized legacy APIs (explicit URL lists and direct eager spider calls) keep
an explicit 50,000-URL safety ceiling. Their audit bridge has separate limits.
They cannot inherit native SQLite capacity results. A complete streamed list-mode
workflow and its capacity acceptance remain tracked separately from native site
and sitemap collection; no population is silently reduced to make it pass.

The historical 50,000-page attempts below retain their original failed budgets.
Their outcomes and sparse direct-storage measurements do not become complete
end-to-end evidence when admission changes. Each new measurement must preserve its
source commits, resource budgets, interruption/recovery and full consumer counts.

## Explicit local history operations

The `scan` CLI group and matching `seo_scan_*` MCP tools operate on individual,
validated `scan.v1` artifacts. They are deliberately not a catalog service: no daemon
discovers scans, no command migrates an artifact, and no operation deletes bodies
separately from their SQLite file.

`scan list` accepts one existing directory and considers scan `*.sqlite` files;
it excludes `*.audit-v2.sqlite` companions from the scan count. For each candidate
it validates the SQLite identity and scan schema while
reading metadata only; it never reads retained body BLOBs. The directory scan is
capped at 10,000 candidate files and 64 MiB of accumulated metadata. Invalid,
foreign, inaccessible, or changed candidates appear in `errors` rather than being
silently accepted. The result is ordered by finish (or creation) time and includes
the aggregate disk use and history warning threshold. `disk_bytes` includes any
audit-v2 companion.

`scan inspect` validates one scan.v1 or native scan.v2 artifact, then exposes a
paginated read-only view of one whitelist table: `pages`, `links`, `forms`, `decisions`, `frontier`,
`query_variants`, `context_items`, `responses`, `documents`, `resource_refs`, or
`audit`. It never exposes the `bodies` table. `limit` is at most 1,000; the
serialized returned rows are also bounded by `max_bytes`, up to 8 MiB. When a row
or page would exceed the budget, the result says `truncated: true` and
`has_more: true` instead of loading a partial BLOB or claiming the table ended.

`scan snapshot` validates the source and copies it through SQLite's Backup API.
When an audit-v2 companion exists, it validates and copies that companion too,
publishing the companion before the scan so a visible snapshot has its audit.
`--out` may be a new filename or an existing directory. For a directory, the tool
creates `YYYYMMDDTHHMMSSZ_host_shortUUID.sqlite` in UTC. Both forms are no-clobber:
an existing file, symlink, or generated-name collision is refused. A snapshot is
a consistent portable file; it does not change whether the original scan finished
or has a usable audit.

`scan pin` and `scan prune` are the only history mutations. Pinning holds the
artifact writer lock and uses SQLite DELETE journal mode; it updates only the
`scan.pinned` value. That changes the SQLite container hash, but leaves the audit,
retained body rows, and evidence revision unchanged.

Pruning is two steps. A preview produces a self-contained JSON plan and makes no
deletion. Its defaults choose only scans that are finished, unpinned, not
`crawl_partial`, not `corpus_partial`, older than 30 days, and outside the newest
five scans with the same host and configuration. Scans with an active writer lock
are excluded. The explicit apply call accepts the exact preview object, including
the CLI/MCP stdout envelope written to a JSON file. Before unlinking, it validates
the reviewed candidates again and recomputes their current group rank. A changed
directory, inode, metadata, lock state, or retention rank refuses the plan.

`scan body-diff` first validates both artifacts, then selects the exact logical URL,
representation, and HTTP variant. Its default comparison is SHA-256 equality of
complete retained bodies. If variants are ambiguous, fidelity differs, evidence is
missing or truncated, or the representation is absent, the result is explicitly
`not_comparable` or `missing_evidence`. `--text` is opt-in, only for compatible
textual fidelity, and is bounded before materialization by `max_bytes` and
`max_lines`; it produces no network request, replay, reanalysis, or SEO score.
Static comparisons also support retained JS/CSS responses. A page's active raw
inventory takes precedence over later diagnostic responses, and rendered DOM
comparisons require compatible recorded renderer settings and transforms.

`scan fragment-links` validates one artifact, then evaluates every
fragment-bearing `<a href>` in retained complete HTML/DOM documents — same-page
`#target` anchors and cross-page `/page#target` links alike. The fragment is
matched the way the WHATWG scroll-to-the-fragment algorithm resolves it: the
serialized fragment against element ids and legacy `<a name>` targets first,
then the percent-decoded, UTF-8-decoded value against both (`+` stays a
literal plus, malformed escapes pass through, invalid UTF-8 becomes U+FFFD),
then the `top` fallback; an empty `#` resolves to the document top. Static, rendered and legacy-fragment representations are
evaluated independently — an id that exists only in rendered DOM resolves the
rendered link, never the static one. Relative hrefs resolve against the
recorded effective `<base href>`; stored redirect and final-URL hops are
followed inside the retained corpus; a different query string is a different
document. Each occurrence is `resolved`, `missing` (the only state that can
become a `BROKEN_BOOKMARK` finding) or `skipped` with a named reason: a
destination absent from the scan, a non-HTML, truncated, omitted, failed or
budget-exhausted body, an unsupported URL scheme, an href the URL resolver
refuses, or an unverifiable `#:~:` text directive is always a named skip,
never a broken bookmark. Inert
`<template>` content contributes neither anchors nor targets. Results are
deterministically ordered by page ordinal, representation and in-document
anchor order, and paginated with `offset`/`limit` plus `total`/`returned`/
`next_offset`; the coverage block says when extraction or result caps omitted
evidence or occurrences ended in named skips (counted per reason in
`skip_reasons`), so a partial inventory can never read as a complete one. No
destination is fetched and the artifact is not modified.

A scan made partial only by recovery of a truncated JSONL tail cannot be represented
faithfully by these three files when its unchanged audit says the crawl was complete.
That export is refused; use the original SQLite artifact, which keeps the recovery
provenance. Export is allowed when the retained audit already records partialness.

The new directory is private while it is staged. Existing directories, files, and
symlinks are refused; a failed export removes only the files it created. Reimport
the three-file directory with the same source producer build if a legacy-shaped
input is needed again.

The import keeps the source directory unchanged. A malformed middle JSONL row,
incompatible artifact version, duplicate conflict, or inconsistent cross-file
population is refused. A single truncated final JSONL line may be recovered only
with explicit partialness. Missing fields are unavailable evidence, never clean
measurements.

For a legacy import, `scan.writer_version` is the original
`audit.tool.version`, while `scan.writer_revision` is the explicitly supplied
full 40-hex `--producer-build` source SHA. `audit.analyzer_revision` is the same
original producer build. These fields describe the source that collected and
analyzed the evidence; they do not name the importer checkout and do not make a
cryptographic attestation. The effective configuration comes from the exact
`run.crawl_config` manifest or an explicit `--config` JSON. If neither exists the
import is refused; if both exist they must agree. The importer records no separate
importer-version field because `scan.v1` has no such column.

## Scan data export (scan_export.v1)

`scan export` (also the flat `scan-export` form and the `seo_scan_export` MCP
tool) serializes retained scan data — not a report — under the versioned
`scan_export.v1` contract. It is a read-only, offline operation that accepts a
validated `scan.v1` SQLite artifact or an SF Analyzer `audit.json` document
(`schema_version` "2.0"). Site-audit reports are refused: they are rendered
output, not retained evidence. The three-file `export-run` output above is a
different, legacy round-trip format and is unchanged.

```bash
seohead scan-export --scan native.sqlite --out export.json --format json
seohead scan-export --scan native.sqlite --out export.xml --format xml
seohead scan-export --scan audit.json --out export.csv --format csv --records pages,findings
```

### Envelope

JSON and XML carry the contract structurally; the CSV manifest file and the
XLSX Summary sheet carry the same fields as key/value rows:

- `format` — the literal `scan_export.v1`.
- `provenance` — run identity: scan UUID, source kind, start URL, lifecycle,
  finish reason, partialness flags, writer version/revision, timestamps, and
  the saved audit's presence state. Field selection never removes it.
- `projection` — the record fields actually emitted, per record type.
- `statistics` — `record_types` exported, exact `rows` per type, and `run`
  totals from the saved audit (or an explicit `unavailable` state).
- `coverage` — per-record-type `state`/`reason`/`exported`/`source_rows`; the
  check inventory (`ran`, `skipped`, `disabled`, `unaccounted` with reasons and
  the saved `check_coverage` totals); declared `capabilities` and `limitations`.
- `records` — the projected rows in deterministic source order.

`--records` selects record types (default: every type the input can provide).
`--fields TYPE=f1,f2` (repeatable) projects record fields; a field a source
never recorded exports as an explicit absent value — JSON `null`, XML
`state="absent"`, CSV/XLSX `\N` — always distinguishable from an empty string
or a measured zero. An `audit.json` input retains no link records: `links` is then
`unavailable` in coverage, skipped by the default selection, and an explicit
`--records links` request fails rather than emitting an empty table.

### Record fields

For a `scan.v1` artifact, `pages` supports the retained projection:
`url`, `status_code`, `content_type`, `size_bytes`, `response_time`,
`redirect_url`, `title`, `meta_description`, `h1`, `h1_2`, `h2`, `canonical`,
`meta_robots`, `x_robots`, `og_title`, `og_description`, `og_image`, `og_url`,
`word_count`, `text_ratio`, `content_frames`, `content_frames_same_origin`,
`crawl_depth`, `content_encoding`, `charset`, `doctype`, `viewport`,
`meta_refresh`, `http_refresh`, `meta_description_count`, `h1_alt_text`,
`lorem_ipsum_count`, `images_total`, `images_missing_alt_attr`,
`images_max_alt_length`, `plugin_elements`, `meta_fragment`,
`ajax_scheme_outlinks`, `title_outside_head`, `meta_description_outside_head`,
`canonical_outside_head`, `directives_outside_head`, `hreflang_outside_head`,
`hreflang`, `heading_outline`, `link_placement`, `canonical_chain`,
`final_canonical`, `head_count`, `body_count`, `head_not_first`,
`invalid_head_elements`, `outlinks`, `external_outlinks`, `jsonld_blocks_found`,
`jsonld_blocks_parsed`, `error`, `error_kind`, `cache_status`,
`body_unavailable`, `representation`, `redirect_chain`, `final_url`.

For an `audit.json` document, `pages` supports: `url`, `status_code`,
`indexability`, `indexability_status`, `content_type`, `metrics`, `issues`,
`issue_ids`.

`links` (scan.v1 only) supports: `source`, `destination`, `anchor`, `nofollow`,
`position`, `rel`, `target`, `raw_href`.

`findings` supports: `id`, `check`, `severity`, `source`, `message`,
`target_url`, `status_code`, `occurrences_count`, `locations`, `details`,
`fix_hint`, `evidence`.

Unknown record types or field names are rejected before any output file is
created, with an error naming the invalid selection and the supported values.

### XML shape

The XML export is UTF-8, deterministic, and generated serialization only —
the exporter never parses caller-supplied XML, DTDs, stylesheets, or entities.
The root element is `scan-export` in namespace
`https://github.com/PavloSEO/seotools/schema/scan_export.v1` with attribute
`format="scan_export.v1"`. Element and attribute names are the JSON contract
names verbatim: `<provenance>`, `<projection>`, `<statistics>`, `<coverage>`,
then `<records>` containing `<pages><page>`, `<links><link>`, and
`<findings><finding>` elements whose children are the selected field names.
Scalar values carry `type="string"`, `type="boolean"`, `type="integer"`, or
`type="number"`; booleans are `true`/`false`; absent values carry
`state="absent"`; nested objects and arrays serialize as JSON text with
`format="json"`. Metadata lists use repeated `<item>` elements. Literal
carriage returns use numeric character references so parsing preserves them;
XML 1.0 forbidden control characters fail with a record/field path before
any final export is published. Metadata mapping keys that are not safe XML
element names use `<entry key="original key">` so arbitrary retained JSON keys
do not make the document malformed or lose their spelling.

### Output files and limits

JSON writes one `*.json` file, XML one `*.xml`, XLSX one `*.xlsx`. CSV treats
`--out` as a base name and writes `BASE.pages.csv`, `BASE.links.csv`,
`BASE.findings.csv` (only selected types) plus `BASE.manifest.csv` describing
the file set, envelope, and exact row counts. CSV uses `;` delimiters and a
UTF-8 BOM like the other CSV outputs. Every target is written through a
sibling temp file and linked into place; existing destinations are refused,
and a failure removes exactly the files it created — no plausible partial
artifact remains and the result is `ok: false`.

CSV and XLSX data cells and envelope values use the same reversible text
encoding: `\N` means absent (`null`), `\E` means an empty string, a leading
`\\` represents one literal leading backslash, and `\F` prefixes an original
string beginning with `=`, `+`, `-`, `@`, tab, or carriage return. Decode these
tokens in that order after reading a cell; for example, source `\N` exports as
`\\N`, and source `=SUM(1,1)` exports as `\F=SUM(1,1)`. Other text is unchanged.
The `\F` prefix keeps spreadsheet software from evaluating source text as a
formula. JSON and XML retain the original values without these escapes.

Records stream from the validated storage iterators; neither format builds a
second in-memory copy of the scan. XLSX uses openpyxl's write-only workbook and
XLSX splits records into numbered sheets at 1,048,575 data rows each (the
1,048,576 limit includes the header) and lists each sheet's record ranges on a
Partitions sheet. It refuses rather than truncates when a sheet would exceed
16,384 columns or a cell exceeds 32,767 characters. Finding filters, segment
selection, and an export index manifest remain open scope (#759). CSV and XLSX encode formula-leading
cell text safely (CWE-1236); JSON and XML carry raw retained values.

## Offline reanalysis

```bash
seohead scan reanalyze --scan old.sqlite --out derived.sqlite --producer-build SOURCE_SHA
```

Here `SOURCE_SHA` is the full lowercase Git SHA of the **current analyzer build**.
The source artifact already records the original capture build. Reanalysis needs
retained inputs; a legacy import with no response bodies cannot supply them.

The `scan reanalyze` CLI subcommand creates a new
derived scan from a Backup API copy of retained evidence. It never overwrites the
input: the output has a new scan UUID, records its parent UUID, and records the
current analyzer build. It preserves the capture configuration; there is no
configuration override because mixing parser results from a different capture
scope would make the derived audit incomparable.

The saved `analysis.finding_exclusions` policy is part of that configuration and
audit provenance. It changes the active findings, score, and tasks while keeping
full suppressed records with their rules and reasons in the audit. Reanalysis
reuses the saved policy against retained evidence; it makes no site request.

Reanalysis uses only retained static HTML and rendered DOM with the existing
parser and check registry. It makes no HTTP, DNS, browser, provider, or resource
fetch request. Missing required retained bodies fail clearly before publishing an
output. Missing live-protocol context becomes a named skip, not a network
fallback. The small MIT fixture bridge supports this route; it does not replace
the broader real-corpus coverage work tracked by #98.

The closed `reanalysis_provenance/run` context records the immediate parent and
original capture UUID, their builds/runtime versions and configuration digests,
the source audit hash, and the source/derived evidence revisions. Its
`capture_run` object preserves the original lifecycle, stop reason, collection
partialness, and creation/finish timestamps across repeated analyses. Each
derivation advances the evidence revision once. Unfinished source frontier work
remains historical and marks the derived collection scope partial; reanalysis
does not resume collection. Previously recorded unavailable capability states
remain unchanged when an older file is opened read-only.

## What is in the file

`scan` is the single run header: format, producer provenance, effective
configuration, lifecycle, capability states, retention policy, and partialness.
`urls` interns exact URL strings. `pages` and `links` are the primary crawl
observations; link occurrences retain their original order and are never
deduplicated. `forms`, `decisions`, `frontier`, `query_variants`, `resume_state`,
and `context_items` hold the native storage core's recovery and collection lanes
(empty in legacy imports). `responses`,
`documents` and `bodies` hold captured HTTP/document provenance. `resource_refs` records
direct script/stylesheet declarations and their capture state. Fetching referenced
resource bodies is a separate explicit option. An inline audit is stored in
`audit.document_json`; an audit-v2 audit is stored in the adjacent companion.
Report formats render the saved document and do not compute new findings.

Native audits bind hreflang relations to the selected retained document and
its `language_evidence` context item. Their
`summary.saved_corpus_derivations.internationalization` records raw labels and
hrefs, normalized target identities, duplicate/conflicting label context,
target status/indexability, each return-link verdict, and relation coverage.
A complete target document with an empty declaration set proves a missing
return link; an uncrawled target, unavailable body, ambiguous URL variant or
partial population remains unmeasured. The audit emits
`HREFLANG_MISSING_RETURN_LINK`, `HREFLANG_BROKEN_TARGET` and
`HREFLANG_NOINDEX_TARGET` only from the corresponding measured evidence.
Report builders read these saved results without fetching pages again.

Existing report and comparison routes can take a scan path directly. The MCP
`seo_report_build`, `seo_compare_crawls`, and SF audit summary, issues, and tasks
tools accept the same path. If an `audit.json` next to a scan is different or
unreadable, the shared resolver keeps using the scan's internal audit and emits an
`input_diagnostics` notice where the response shape permits it. The issue-list
tool remains a list, so it records that notice as a `RuntimeWarning` rather than
inserting a fabricated issue.

The complete DDL is [the SQL reference](../seohead/storage/scan_v1.sql). The
existing audit document remains governed by
[the audit JSON Schema](../seohead/sf/schema/audit.schema.json). The precise JSON
and cross-table validation contract is part of `scan.v1`; readers must reject
unknown/newer versions and missing required fields rather than guessing.

Raw HTTP entities, decoded documents, and rendered DOM are distinct evidence. A
rendered DOM never substitutes for raw HTTP bytes. The legacy importer retains no
bodies. Native capture fetched HTML entity bytes and separately captured DOM
bytes when rendering provides them, with SHA-256 deduplication and `identity` or
`zlib` level-6 storage (compression is used only when smaller). A body record is
consistency evidence, not a signature or an anti-tampering claim.

Native defaults are 5 MiB decoded bytes per body, 10 GiB stored bodies per scan,
1 GiB free-space reserve, and a recorded 20 GiB history-warning threshold. Explicit
`scan list`, `inspect`, `snapshot`, `pin`, `prune`, and `body-diff` operations manage
individual artifacts; no background history service or automatic deletion exists. Capture processes one
body at a time. The native fetch clamp is a 64 MiB hard limit: a larger response
is marked truncated rather than retained, even if a configured policy limit is
larger; rendering fails rather than silently keeping an over-limit DOM. `off`,
`no-store`, credentialed, unsupported, failed, truncated, and budget-exhausted
captures each retain their named state/reason. A capture is credentialed when the
run was configured to send a credential header for that host through
`http.credential_headers`, never because the shared client's cookie jar carried a
cookie the site itself set: a session cookie is the ordinary state of the web, and
reading one as the operator's credential discarded 33 001 of 40 920 page bodies on a
public site with nothing configured at all. Redacted header lists keep the names of
the headers redaction removed, under `x-seohead-redacted-headers` and never their
values, so a retained response and a suppressed one are distinguishable in the
artifact. `evidence.retain_no_store_acknowledged=false` is the default: an anonymous
`Cache-Control: no-store` response is omitted. Setting it to `true` explicitly permits
the same bounded SQLite evidence retention and is recorded in the scan configuration and
all derived artifacts; it never permits retention from `http.credential_headers` or a
persistent browser profile. A crawl reports how many fetched HTML page bodies it retained and the
reasons it dropped the rest in `html_bodies`, because a `partial` capability flag is
the same word for one missing body and for four fifths of them. The same rule
applies in the JavaScript rendering lane: a rendered DOM is credentialed when the run
was configured to send a credential header through `http.credential_headers` or to
reuse a persistent browser profile, never because the browser carried back a cookie
the site itself set to a same-origin subresource. A crawl reports how many rendered
DOMs it retained and the reasons it dropped the rest in `rendered_bodies`, for the
same reason `html_bodies` exists -- both counts are derived from the artifact, so a
finished scan still answers the question afterwards. Every rendered document's
`renderer` provenance records the requested and effective engine, its reported
version, the effective viewport and the configured page concurrency -- a failed
attempt records what was requested with `engine: "unknown"` rather than inventing a
successful render. Native SQLite mode requires
`cache.mode=off` before collection; it never changes or deletes the old directory
cache, which remains part of the directory workflow.

`scan_decoder.v1` records entity decoding. A static document's logical URL stays
separate from the effective navigation URL; legacy-fragment navigation is recorded
as its explicit transform. Direct script and stylesheet fetching requires the explicit resource setting below.
Offline network replay remains unavailable. Reanalysis is the retained-evidence
operation described above and never falls back to the network.

## Direct script and stylesheet capture

The `resource_refs` lane records every declared script and stylesheet
occurrence for a selected static, rendered, or legacy-fragment document. It
keeps repeated declarations and uses separate occurrence ordinals per resource
kind. A single fetched URL may serve many references; references are not
deduplicated. The active extraction records `resource_inventory` under
`document:<id>` with the exact payload
`{"document_id": <id>, "state": "complete|partial|unavailable", "omitted": <n>}`.
`complete` with zero declarations means the document inventory was measured and
empty. `unavailable` means it was not measured; it is never a clean empty
inventory. A later extraction for the same logical document representation
supersedes the current coverage marker without deleting historical evidence.

Resource network collection is opt-in through `resources.fetch=true`. Its
default bounds are 20,000 HTTP attempts per scan, including retries and
redirects, and 5 MiB content-decoded bytes per resource response. Resource
attempts share the crawl's total time limit, retained-body store limit, and
free-space reserve. They do not consume the page URL limit or add resources to
the page frontier. Every redirect hop stays within the crawl start origin (scheme, host and effective port) under guarded transport. One request is made per URL and kind-specific `Accept` variant, while
every referring occurrence remains in `resource_refs`.

When fetching is disabled, declarations have the named `resources_disabled`
state. Other named omissions include `not_fetched`, scope or robots exclusion,
request/body budget exhaustion, fetch failure, and body unavailability. Resource
bodies are complete only when every declaration in the measured scope has a
successful complete response; declaration coverage is independent of whether
bodies were fetched. CSS `@import`, JavaScript modules, third-party resources,
and browser-network response capture are outside this slice. Resource inventory
and capture add no SEO findings, and they do not enable network replay.

The writer records `resource_commit` with
`{"digest": <64 lowercase hex>, "requests_used": <n>}` alongside inventory
context. These are operational consistency metadata, not signatures or an
anti-tampering claim.

Credential material is re-supplied out of band. The closed `credential_context`
payload is exactly `{"verifier": null|<64 lowercase hex>, "implicit_state": bool}`:
environment references and profile paths are redacted. Global `http.headers` refuses
credential-bearing names; a legacy mapping is defensively redacted in any new
manifest or effective-config snapshot. A changed explicit verifier
refuses resume. Credentialed or persistent-browser state cannot be resumed safely and is
refused conservatively. A server-set cookie from an otherwise anonymous
crawl is recorded only as `anonymous_session`, never with its value. Its resume starts a
fresh anonymous session and records `anonymous_session_resume`; it is allowed because it
does not restore operator-supplied access.

For legacy imports, the only populated `context_items` lane is
`legacy_import_provenance`; it exports no restore checkpoint or equivalent resume
state. These historical imported files have no retained bodies or resources and
cannot be reanalyzed; native captures use their own validated lanes.

### Historical page fields

The `pages` projection follows the prerelease `crawl.v1` `PageRecord`.
Later-added fields are nullable for legacy compatibility: `content_frames`,
`content_frames_same_origin`, ordered `hreflang_json`, `heading_outline_json`,
`link_placement_json`, `canonical_chain_json`, `final_canonical`, `body_unavailable`,
`meta_refresh`, `http_refresh`, `meta_description_count`, `h1_alt_text`,
`lorem_ipsum_count`, `images_total`, `images_missing_alt_attr`,
`images_max_alt_length`, `plugin_elements`, `meta_fragment`,
`ajax_scheme_outlinks`, and `og_url`. The first two are parser observations about frames in the
resolved content area. `hreflang_json` preserves the document's alternate
declarations. `heading_outline_json` preserves every `h1`-`h6` with text in DOM
order as `{"level", "text", "region"}` objects; `region` uses the same taxonomy
link positions do (`nav`, `header`, `sidebar`, `footer`, `content`, `other`) and
is the empty string when the document offered no landmark and no position rule
matched, which is an unmeasured region rather than a measured one.
`link_placement_json` preserves the two link defects a page region cannot show:
the anchors this page wraps in an `h1`-`h6` (`in_heading`, each with its level,
destination and anchor text) and the image links on it that carry no anchor text,
no `alt`, and no `aria-label` or `title` (`image_no_text`). Both lists are capped
by the parser and each carries an `*_total` count beside it, so a truncated list
reads as truncated rather than as the whole story. `NULL` here means the page's
anchors were never inspected for this, which is not the same as finding none.
`og_url` records the page's own `<meta property="og:url">` beside the three
`og_*` columns above; it is nullable because a crawl written before it was
collected never read the tag, and the Open Graph check names `og:url` missing
whenever this reads empty.

### Per-occurrence link context

`seohead.storage.link_context.context_for_link(scan, link_id)` and
`contexts_for_document(scan, document_id, offset=0, limit=100)` derive the
versioned `link_occurrence_context.v1` result **on demand** from one complete
retained HTML body or rendered DOM. They do not change `scan.v1`/`scan.v2`,
fetch a URL or touch the frontier. Every result cites its scan UUID, evidence
revision, `link_id`, source document, representation and stored ordinal. Repeated
source-target links remain separate. Document-level queries preserve source URL
and representation even when the document contains zero links. Raw and rendered documents are read and
reported independently; a rendered position never fills an unavailable raw
position. A missing, omitted, truncated, non-HTML or over-budget body returns
`unavailable` with a reason rather than a fabricated placement or heading.

The extractor uses the recorded content-root and position rules. A matched
selector is positive placement evidence; a content-root match is labelled as
inference and names the root-selection strategy. The bounded DOM path uses
`nth-of-type` segments without page-specific IDs or classes. For content links,
the nearest preceding heading in the same section/article wins; otherwise the
nearest preceding content heading is used. Nav/footer links do not borrow a
content heading. Heading text and selector excerpts are capped at 160
characters, DOM paths at 12 levels/512 characters, and truncation is explicit.

The reader replays the saved link-storage filter against the document and
checks every retained occurrence in ordinal order, including `raw_href`, `rel`
and `target` when attribute capture was enabled. If old parser behavior or
missing configuration prevents an exact replay, the document context is
`unavailable`; it never guesses which duplicate anchor was stored. It processes
one document at a time, with at most 20,000 eligible anchors, a default 5 MiB
decoded-body budget (hard cap 8 MiB), and a 500-row/1 MiB serialized-item
page default (hard item-byte cap 8 MiB). Byte-limited pages stop before a whole occurrence and
return the next offset. Omitted
anchors make document coverage `partial`; a single-link result also carries that
coverage. `seohead scan-link-inspect --view context` and the matching
`seo_scan_link_inspect` MCP tool expose this result by `link_id` or `document_id`.

`body_unavailable` records why collection could not parse a page
body (for example, an oversized response); it does **not** describe whether this
artifact retained that body. `meta_refresh` and `http_refresh` retain the markup
and HTTP declarations as written.

The table is derived from the importer mapping and is a compatibility contract:
an omitted key from an older JSONL row becomes SQL `NULL`, never an invented clean
value. The documentation test compares this table to `_LATE_PAGE_FIELDS`.

| PageRecord field | `pages` column |
|---|---|
| `content_frames` | `content_frames` |
| `content_frames_same_origin` | `content_frames_same_origin` |
| `hreflang` | `hreflang_json` |
| `heading_outline` | `heading_outline_json` |
| `link_placement` | `link_placement_json` |
| `canonical_chain` | `canonical_chain_json` |
| `final_canonical` | `final_canonical` |
| `body_unavailable` | `body_unavailable` |
| `meta_refresh` | `meta_refresh` |
| `http_refresh` | `http_refresh` |
| `meta_description_count` | `meta_description_count` |
| `h1_alt_text` | `h1_alt_text` |
| `lorem_ipsum_count` | `lorem_ipsum_count` |
| `images_total` | `images_total` |
| `images_missing_alt_attr` | `images_missing_alt_attr` |
| `images_max_alt_length` | `images_max_alt_length` |
| `plugin_elements` | `plugin_elements` |
| `meta_fragment` | `meta_fragment` |
| `ajax_scheme_outlinks` | `ajax_scheme_outlinks` |
| `og_url` | `og_url` |
| `amphtml` | `amphtml` |
| `mobile_alternate_broken` | `mobile_alternate_broken` |

`NULL` means the field was absent from an older crawl record; it never means a
measured empty value or zero count. Current imports validate and store actual
field types. Imported older records therefore leave the `pages` capability partial
independently of the run's `crawl_partial` state.

`trust_signals_json` sits outside that table deliberately. The parser's
byline/date/article evidence (issue #823) is an optional record field rather
than a late one: an absent key in a `pages.jsonl` row is a legal present-day
state -- the page's body was never parsed for trust signals -- not proof that
the record predates the field, so it feeds neither the required-field check
nor the legacy-fields report. `NULL` here means the page never yielded parsed
body evidence, which is unmeasured, not a measured "nothing found". A measured
absence is the object with empty lists.

This is a prerelease `scan.v1` schema synchronized with that current record
contract. There is no automatic migration. A prototype SQLite file with the old
DDL is refused and must be explicitly reimported from its legacy source. The
legacy importer can preserve a pre-merge 43-field JSONL record losslessly by
recording all twenty later observations as `NULL`; it does not invent defaults that
claim a measurement.

## Read safely with the Python standard library

Open the artifact read-only. Do not load extensions, execute SQL supplied by an
untrusted party, or use a writable connection merely to inspect a file. A reader
should set a finite progress handler so a costly query can be cancelled.

```python
import sqlite3

from pathlib import Path
import time

if sqlite3.sqlite_version_info < (3, 31, 0):
    raise RuntimeError("SQLite >= 3.31 is required")
path = Path("scan.sqlite").resolve()
con = sqlite3.connect(path.as_uri() + "?mode=ro", uri=True)
con.execute("PRAGMA trusted_schema = OFF")
deadline = time.monotonic() + 30
con.set_progress_handler(lambda: int(time.monotonic() > deadline), 10_000)
try:
    if con.execute("PRAGMA application_id").fetchone()[0] != 1397051208:
        raise ValueError("not a SEOH artifact")
    if con.execute("PRAGMA user_version").fetchone()[0] != 1:
        raise ValueError("unsupported version; no automatic migration")
    if con.execute("SELECT format_version FROM scan").fetchone()[0] != "scan.v1":
        raise ValueError("inconsistent format version")
    row = con.execute(
        "SELECT scan_uuid, format_version, source_kind, lifecycle, crawl_partial "
        "FROM scan WHERE singleton = 1"
    ).fetchone()
    print(row)
finally:
    con.close()
```

SQLite 3.31 or newer is required for `trusted_schema=OFF`. The importer and reader
must report an actionable error if Python lacks `sqlite3` or the SQLite runtime is
too old; no extension, JSON1, or STRICT-table dependency is required.

These three read-only queries work with stock `sqlite3`:

```sql
-- Run identity and declared capability state.
SELECT scan_uuid, format_version, writer_revision, source_kind, lifecycle,
       crawl_partial, corpus_partial, capabilities_json
FROM scan WHERE singleton = 1;

-- Count pages by recorded HTTP status without loading page bodies.
SELECT status_code, COUNT(*) AS pages
FROM pages GROUP BY status_code ORDER BY pages DESC, status_code;

-- Most-linked destinations; every stored link occurrence contributes once.
SELECT u.url, COUNT(*) AS inlink_occurrences
FROM links AS l JOIN urls AS u ON u.url_id = l.destination_url_id
GROUP BY l.destination_url_id ORDER BY inlink_occurrences DESC, u.url LIMIT 20;
```

## Bounded body read

This standard-library example reads one validated complete body. Point A imports
have no body rows. Bound decompression before processing bytes; do not read every
body into memory.

```python
import hashlib
import zlib

MAX_DECODED_BYTES = 5 * 1024 * 1024


def decode_body(codec: str, data: bytes, decoded_bytes: int, sha256: str) -> bytes:
    if decoded_bytes < 0 or decoded_bytes > MAX_DECODED_BYTES:
        raise ValueError("body exceeds this reader's limit")
    if codec == "identity":
        decoded = data
    elif codec == "zlib":
        decoder = zlib.decompressobj()
        decoded = decoder.decompress(data, MAX_DECODED_BYTES + 1)
        if (
            len(decoded) > MAX_DECODED_BYTES
            or decoder.unconsumed_tail
            or not decoder.eof
            or decoder.unused_data
        ):
            raise ValueError("invalid or oversized decoded body")
    else:
        raise ValueError("unsupported scan.v1 body codec")
    if len(decoded) != decoded_bytes or len(decoded) > MAX_DECODED_BYTES:
        raise ValueError("invalid or oversized decoded body")
    if hashlib.sha256(decoded).hexdigest() != sha256:
        raise ValueError("body digest mismatch")
    return decoded
```

For a live writer, do not copy the main `.sqlite` file alone: committed rows can be
in its WAL. The native core's `NativeScan.snapshot()` uses SQLite's Backup API to
publish a validated single-file copy; `scan snapshot` exposes that no-clobber path
for an explicit operator-requested destination.
Until a file is finalized or snapshot-created, retain
its WAL/SHM beside it and treat lifecycle/partialness as part of the evidence.
Future format changes use an explicit migration into a new file; readers do not
silently add columns or reinterpret a newer version.

## Legacy import boundaries

The importer accepts current `crawl.v1` PageRecord/LinkEdge fields and a validated
`audit.json` schema `2.0`. Nonstandard JSON numbers (`NaN`/`Infinity`) are
refused without rewriting the old report. It refuses unknown fields instead of silently losing
them. It preserves exact audit UTF-8 bytes, page order, nullable flags and repeated
link occurrences. Only one physically truncated final JSONL record may be
recovered, with partialness recorded; it must not hide a missing audit page.

The reader validates the exact schema, foreign keys, scalar/JSON types, revision,
page population, ordering and audit digest in one read transaction. For a legacy import it
refuses populated native lanes; native files have a separate validator for their
collection state and current audit. This verifies consistency, not authenticity or protection from an
editor who can recompute a checksum. No Merkle tree, signature or key management
is provided.

Limits are 64 MiB per audit/config input, 8 MiB per JSONL record and per metadata
value, and a 30-second SQL validation/query deadline. Import only completed source
directories; inputs are hashed from the exact bytes consumed. Failed imports
publish no destination and remove their private temporary file. An existing
file or symlink is never overwritten. Reading never upgrades or migrates a file.

Report parity uses the same existing renderer on the unchanged saved audit. Tests
compare complete bytes for JSON, Markdown, both CSV files, XLSX and DOCX; Office
creation/ZIP timestamps are held equal in both test branches. Normal independent
Office builds can differ in timestamps even for the same original audit.

Import and export remain explicit operations. URL-mode `seohead crawl-site` uses
a native artifact by default; the explicit legacy directory route remains available.
For a legacy imported three-file directory, native resume, retained bodies/resources,
and reanalysis remain unavailable because that source never retained the needed
corpus. Audit-level findings and context already saved in the exact audit remain
available; the missing raw crawl corpus cannot be reconstructed from the three
exported files.

## Native collection

```bash
seohead crawl-site --url https://example.com --max-urls 50 --scan-out native.sqlite
seohead report-build --audit native.sqlite --format md --out native-report.md
```

The collector derives provenance from its current source build. A clean distribution build embeds
its producer Git revision and a relative hash manifest for the packaged runtime files; an installed
wheel validates that metadata against its own wheel `RECORD` before using it. This is only
same-package consistency, not a cryptographic attestation or tamper-proof claim. A dirty or
unverifiable source checkout still requires an explicit `--producer-build`
SHA; the runtime never borrows an enclosing repository's revision. The producing version/revision,
runtime versions, full effective configuration and result-affecting fingerprint are stored in the
scan. A different configuration or producing build refuses resume.

`python -m seohead.storage inspect native.sqlite` also validates a capture with
no audit and reports `audit_available`; report commands still require an audit.
`seohead crawl-site --resume <scan>` continues an interrupted scan from its stored
frontier and throttle state, reading the start URL and the effective configuration
back from the artifact rather than from the command line; repeating the original
command with the same `--scan-out` path and identical settings resumes it too.
A finished file is immutable and cannot be overwritten or resumed for writing. Use a new destination
for a new run. `--scan-out` cannot be combined with `--out-dir` or URL-list mode;
SQLite mode currently requires `cache.mode=off`. List inputs and cache live/replay
require the explicit legacy directory route; unsupported combinations refuse before
collection instead of falling back silently. The returned `scan` path and run journal
identify the generated filename. Credentials are re-supplied out
of band and resumability is governed by the redacted credential context above. The MCP
`seo_crawl_site` exposes the same `scan_out`, `resume` and `producer_build` parameters.
Native capture retains bounded HTML entities and rendered DOMs under the recorded
body policy; responses disabled by policy, credentials, `no-store`, media type,
size or store budget keep a named omission instead. The raw start-page HTML is
read from that retained static document for the rendering gate. A legacy three-file
import cannot recreate discarded bodies, response provenance or native resume state.

In URL mode, `scope.include_extensions` and `scope.exclude_extensions` record
pre-request frontier decisions while keeping the referring `links` rows. The
response media filters record a per-URL decision after headers arrive; a filtered
response retains its status, media type, headers and redirect evidence while its
entity is omitted. `pages.body_unavailable` carries the media filter reason so
body-derived audit checks remain `skipped`. The saved crawl config includes all
four filter lists, so a resume with changed rules is refused before reusing its
frontier. The `resources.fetch` lane remains separately governed by its own MIME
and byte budgets.

The collector keeps only its bounded worker batch and page observations in
Python; page/link/form records, seen identities, queue, query variants and
recovery state live in SQLite. Parser caps retain at most 20,000 link observations and 2,000 form observations
per page. Full valid link totals remain PageRecord measurements;
omitted occurrences/discovery produce an explicit partial state. Seeds and query
reservations retain legacy ordering, including the different seen/query order
for sitemap seeds and ordinary links. Interrupted work can be fetched again;
committed pages are not issued again.

The native analyzer consumes SQL graph projections and disk-backed page, finding
and group collections without rebuilding `all_inlinks` or a complete Python edge
list. Its audit.v2 path is independent of the **legacy materialized audit bridge**,
which is limited to 10,000 pages, 20,000 forms and 1,500,000 links. Beyond those
bounds the legacy bridge preserves collection evidence with `audit_available=false`
and a recorded reason. These limits do not cap the native audit.v2 page population
and are not a memory guarantee for arbitrary field lengths or finding populations.
Separately measured page, form and payload populations are recorded in
[SQLITE_ACCEPTANCE.md](SQLITE_ACCEPTANCE.md); they do not establish a
one-million-URL collector capacity. Sitemap capture streams membership
in chunks of 256, with the existing per-root expansion limits; it no longer
allocates the full declared-URL list before an admission check.

The inline `audit.document_json` contract still has its existing 64 MiB limit.
Its writer checks the exact serialized size before replacing the row. Audit-v2
stores ordered collection rows in the adjacent SQLite companion, with a 64 MiB
header ceiling and an 8 MiB ceiling per atomic row; the collection population has no
64 MiB total limit. Storage revision 3 normalizes group URL arrays into ordered
group_members rows, so a logical group can exceed the atomic-row limit without
losing a member or reconstructing its complete URL list. The group row retains a
count-bearing reference; every member has its group ordinal and member ordinal.
Each row has a stable ordinal, and a SHA-256 digest covers
the storage revision, header, binding, collection paths, order, and exact JSON row
and group-member text. Readers
validate the scan binding, SQLite schema, foreign keys, row counts, ordering,
and digest before exposing any collection. The four reconciliation arrays under
`summary.sitemap` are separate collections too; they must not be truncated into
an apparently complete summary.

New readers also accept storage revision 2 without changing its bytes or legacy
digest. Old readers refuse revision 3 explicitly. AuditV2Reader.iter_collection
returns group URLs as a lazy sequence for revision 3; iter_group_members streams
the complete membership, while group_members_page exposes count- and byte-bounded
pages with exact total, returned and has_more. JSON export reconstructs the
complete array incrementally; only the explicitly bounded compatibility
materializer builds an ordinary JSON document. Offline reanalysis records the
source companion digest as source_audit_sha256.

`report-build` and `compare-crawls` accept an audit-v2 scan path. JSON output
streams the complete document; CSV, XLSX, Markdown and DOCX consume ordered rows
without constructing a second audit dictionary. DOCX retains its established
40-findings-per-severity and 60-page display limits while showing the exact
stored totals. The compatibility `read_audit`, `resolve_audit_input`, and
`export-run` document paths still materialize only up to 64 MiB and fail with a
named size error above it. They never return a partial document or infer a clean
result. A producer that chooses audit-v2 must do so before building the legacy
complete audit dictionary; collection-to-header selection remains the producer's
responsibility.

A resumed scan without the required retained static start-page HTML has a named
no-audit guard; it cannot invent a clean rendering verdict.
An already-current audit can be reused when only file finalization was blocked.
Current audits feed the existing report, comparison and task APIs; no automatic
`audit.json` or task sidecars are written in SQLite mode. A zero configured delay
is recorded as the schema-supported rate string `unbounded`, never JSON Infinity.

### Collector capacity measurement

The figures in this and the following capacity sections are development-profile
observations from separate, narrower profilers. They are not the release
acceptance record, and they do not include the two measured limits it publishes.
Read [SQLITE_ACCEPTANCE.md](SQLITE_ACCEPTANCE.md) before citing any number here as
a capacity result.

The offline profiler uses 10,000 seeded pages, generated HTML, and injected
transport without sockets. On macOS 26.6.2 arm64, Python 3.14.6 and SQLite 3.53.3:

| Stored links | Peak collector RSS | Collection time |
|---:|---:|---:|
| 300,000 (30/page) | 241.05 MiB | 61.69 s |
| 1,500,000 (150/page) | 241.95 MiB | 244.55 s |

The fixed-page increase was 0.90 MiB; both runs met the 256 MiB peak and 128 MiB
increase budgets. Counts and ordered link digests were checked from SQLite. macOS
reports `ru_maxrss` in bytes; the profiler normalizes Linux KiB separately. This
measures collection only: the analyzer compatibility bridge, report rendering,
sitemap expansion, large page fields and retained bodies are outside this result.
No larger default crawl ceiling follows from these measurements.

For metadata-only native scans (`storage.body_mode=off`) with no captured
responses, documents, bodies, or resource refs, the writer now derives corpus
capabilities once from the existing full SQL calculation and reuses that exact
summary after checking those evidence tables remain empty. It updates the same
scan metadata inside each page transaction. Any body, render, or resource
evidence returns to the full calculation; reopening a writer derives the
invariant again, and inspection still validates against all stored rows. A
bounded 2,048-page sparse cProfile reduced `corpus_summary` and reanalysis
queries from 2,048 calls each to one each. This is a sparse-writer improvement,
not a dense-body or million-URL capacity result; staged #815/#818 acceptance
still gates any crawl ceiling change.

Reproduce with `python scripts/profile_scan_collector.py`.
The profiler emits progress and JSON with platform/runtime versions and source
file hashes. It keeps no full edge graph in Python. See
[the profiler](../scripts/profile_scan_collector.py) for the exact fixture.

### SQL graph and sitemap projections

The read-only Python core also provides
`seohead.storage.link_queries.shortest_observed_path(scan, seed, target)` and
`reverse_inlinks(scan, target)`. The first uses indexed source-link reads and a
bounded FIFO walk; each returned hop carries the exact `links.link_id`, source
document ID, representation, and stored ordinal. Equal-hop ties use source
queue order, then static/rendered/legacy representation, occurrence ordinal,
and link ID. Its default ceilings are 10,000 visited nodes, 200,000 examined
edges, 20 hops, and 15 seconds; callers may lower them or raise them only to the
documented hard caps in the Python signature. Nofollow and out-of-scope links
are excluded from traversal. URL identity follows the crawler's fragmentless
exact URL rule; it does not fold a page's trailing slash or path case.

`reverse_inlinks` returns retained occurrences, including duplicates and
fragment links to the selected target, in stable link-ID pages. Its opaque
`cursor` is bound to the scan UUID, evidence revision, target and representation;
using it with another query is refused. Defaults are 100 rows, 1 MiB serialized
item bytes, and 15 seconds. Rows preserve raw/rendered identity. A blank
position is unmeasured. Each result includes the scan UUID and evidence revision
so clients can avoid combining pages from different scan snapshots. The shared
`scan-link-inspect` CLI and `seo_scan_link_inspect` MCP entry points expose the
same core with `--view path`, `--view inlinks` or `--view links` (the paged links of one URL, looked up
through the `urls` index) and bounded output.

A found path proves only that these retained edges connect the two URLs.
`unreachable_in_observed_graph` does not prove a site-wide orphan. The result
always marks global reachability unknown and carries partial or unavailable
link-coverage reasons. Budget exhaustion returns `limit_reached` rather than an
absence conclusion. Neither query fetches URLs, rewrites the artifact, or
builds a second permanent graph.

Native scan graph projections now read a validated `scan.v1` artifact through a
read-only SQLite snapshot. They use bounded cursors and connection-local,
file-backed temporary tables; they do not rebuild the stored graph as a Python
edge list, set, or DataFrame. The recorded destination text remains an edge
fact. A destination enters the composition population only when its existing
crawler canonical identity belongs to a recorded crawled page; repeated DOM
occurrences remain stored, while composition alone counts one distinct
`(destination, source, position)` triple. Unclassified occurrences remain
unmeasured rather than becoming content links. Destinations outside the
crawled-page population are disclosed separately and do not create an internal
linking conclusion.

Sitemap membership records the selected **expanded root** after the existing
sitemap tool follows a root and its nested indexes. It does not claim that a
member came directly from one XML document line. Member order is the run-wide
post-expansion normalization/deduplication order across selected roots, while
the original retained URL spelling remains the report value. A selected root
with a complete expansion and zero members is a measured empty sitemap. A scan
with no saved selected roots is `unavailable` rather than a zero-sized sitemap;
partial root expansion is also unavailable for a reconciliation conclusion.

The same SF analyzer checks now consume a narrow graph access contract for
generic anchors, inlink composition, PageRank, and discovery paths. SQL keeps
the edge inventory, uncrawled internal graph nodes, scores and BFS predecessors
in file-backed temporary tables. Existing check eligibility, thresholds,
messages and output ordering remain shared with the SF export path. Only
requested score lookups, bounded location examples and emitted paths enter
Python. Native links still have no measured resource Type, so the resource
inventory check retains its existing named skip. Response bodies, raw HTML,
rendered DOM, JavaScript and CSS bodies remain unavailable.

#### SQL graph profile

The SQL graph profiler (scripts/profile_scan_graph.py) constructs a deterministic 10,000-page native-writer fixture
with classified `content`/`footer` links and a complete 10,000-member selected
expanded root. Build, graph, and sitemap readers run in separate
subprocesses, so graph/sitemap RSS does not inherit fixture-construction RSS.
It measures no `all_inlinks` DataFrame, report rendering, or body lane. Run
`python scripts/profile_scan_graph.py --profile` to reproduce the JSON and
incremental progress output.

On macOS 26.6.2 arm64 with Python 3.14.6 and SQLite 3.53.3, the separate
reader subprocesses produced these results. macOS `ru_maxrss` is bytes; the
script normalizes it to MiB and records Linux KiB separately. Each graph digest
matched a one-row-at-a-time expected digest; each sitemap run had exactly 10,000
`in_sitemap_and_linked` members and no other bucket.

| Stored links | Graph reader peak RSS | Graph seconds | Composition rows / distinct triples | 10,000-member sitemap reader peak RSS / seconds |
|---:|---:|---:|---:|---:|
| 300,000 | 247.97 MiB | 1.819 | 10,000 / 300,000 | 248.28 MiB / 2.680 |
| 1,500,000 | 248.22 MiB | 6.608 | 10,000 / 1,500,000 | 248.30 MiB / 14.068 |

The graph-reader peak increased 0.25 MiB over the fixed-page edge-density
change. This is a cursor-projection measurement, not an end-to-end crawler,
analyzer, report, body, or arbitrary-site capacity claim.

### Saved-scan analysis and report capacity

Run `python scripts/profile_scan_analysis.py --pages 10000` for the full saved-scan
analysis profile. It uses complete synthetic PageRecords and a balanced graph at
300,000 and 1,500,000 links. Only collection and the sitemap protocol request
are injected: the whole stage runs CLI dispatch, saves the native audit,
reopens the file, and creates a Markdown report. Separate subprocesses measure
page projection, graph calculations, audit plus task derivation, and all five
report formats. Every run contains 10,000 pages, 10,000 score/composition
results, and 20,011 audit findings; counts and outcome digests are recorded.

Observed on macOS 26.6.2 arm64, Python 3.14.6 and SQLite 3.53.3:

| Stored links | Pages RSS | Graph RSS | Audit/tasks RSS | Five report formats RSS | CLI + saved scan + Markdown RSS |
|---:|---:|---:|---:|---:|---:|
| 300,000 | 273.61 MiB | 525.98 MiB | 453.44 MiB | 459.44 MiB | 551.39 MiB |
| 1,500,000 | 273.69 MiB | 469.06 MiB | 453.62 MiB | 419.39 MiB | 505.06 MiB |

The audit/task increase was 0.18 MiB; graph and whole-route peaks decreased in
this pair. Each is below the 128 MiB edge-growth budget, and both whole-route
peaks are below 1 GiB. Process peaks vary with allocation and garbage collection;
these measurements are evidence for this fixture, not arbitrary-site guarantees.
The whole CLI stages took 19.163 and 69.425 seconds respectively.

Output volume remains a separate bound. The earlier blank-field ring fixture
generated 129,874 findings and 116,868,817 bytes of audit JSON before pretty
printing, beyond the 64 MiB saved-audit limit. It is an output-stress case, not
a passing saved-scan CLI example. Narrow the capture scope when the full audit
cannot fit; the oversized-audit result must never be mistaken for a clean report.

## Native transactions and recovery

`seohead.storage.native_scan.NativeScan` is an internal Python storage API for a
native `scan.v1` file used by the default URL collector. Its
`create`, `open`, `enqueue`, `claim`, `commit_page`, `recover_inflight`, `interrupt`,
`inspect`, `finish_without_audit`, `resume_or_finalize`, and `snapshot` operations
are exercised with offline page observations.
Fetch workers must return their bounded results to the one writer; they must not
share its connection or start additional writers. The directory crawler and
legacy-import validator remain separate; CLI/MCP select the same default native route.

A committed page unit contains its page projection, ordered duplicate link/form
occurrences, decisions, accepted query variants, frontier updates, runtime state,
and one evidence revision. A rejected query variant keeps its exact URL decision
and excluded frontier identity without consuming its query reservation. Frontier
order and committed-page order are separate contiguous sequences. A retry of an
already committed lease succeeds only when its whole input digest matches; a
changed retry is refused. This digest is idempotency bookkeeping, not a signature
or protection against an editor who can recompute it.

The core records the producing build, complete validated effective configuration,
result-affecting configuration fingerprint, runtime versions, and unavailable
body/resource/reanalysis states. A different start URL or configuration refuses
resume. Missing, foreign, newer, malformed, and terminal files cannot become live
writers. Validation occurs read-only before writer setup; reading never upgrades
the file. `NativeScan.inspect()` validates native metadata and references in one
bounded read transaction. Report/compare/task routes require a current audit
whose hash, revision, build, configuration, and page population agree with the
scan. A capture with no admitted audit is inspectable evidence, never a clean
report.

Only the process holding the lifetime POSIX advisory lock may requeue orphaned
inflight URLs. The lock lives at `<scan>.writer.lock`, separate from SQLite's own
database locks on macOS/Linux. Keep its inode in place; unlinking/recreating an
active lock could let another writer acquire a different inode. A second writer
is refused. A committed URL stays done; fetched but uncommitted work may be
fetched again after a crash. Exactly-once HTTP execution is not promised.

Live writes use WAL, FULL synchronous mode, foreign keys, a finite busy timeout,
an 8 MiB SQLite page cache, and file-backed temporary work on a local filesystem.
Transaction failure rolls back the complete pending unit. If the filesystem also
prevents saving an error state, the caller still receives the failure and retains
the last committed artifact; no success is inferred from an unchanged header.
Initial queue chunks and each page observation lane are limited to 20,000 items
and 8 MiB of JSON; the page record is limited to 8 MiB and the complete commit
input to 64 MiB. Oversized input is refused atomically. The collector adapter must
record any deliberately retained prefix as partial evidence; this core does not
silently truncate observations. A WAL above 64 MiB triggers a bounded checkpoint
before accepting more work. A reader holding a read transaction makes that
checkpoint answer busy, so the write retries it with a backoff for up to 30
seconds and records the wait in the artifact's limitations: reading a scan while
it is collected is what `scan-list` and `scan-inspect` are for, and a WAL reader
that cannot block the writer must not be able to end its run either. Only a
checkpoint still blocked at that deadline is backpressure: the collector stops
claiming work and leaves `lifecycle=interrupted` with
`finish_reason=storage_backpressure` instead of raising, and a write that hits it
between pages still leaves the same interrupted header with its reason. Marking a
run interrupted is never itself refused for WAL pressure, so a partial capture
cannot read as one still running. Do not actively write in a network filesystem
or cloud-synchronized directory.

`snapshot()` admits space for the logical database, observed WAL/SHM, a temporary
margin, and a 1 GiB reserve. It uses a bounded Backup API operation, validates the
copy, closes it in DELETE journal mode, fsyncs it, and publishes with a no-clobber
hard link. Existing destinations and symlinks are refused. Only the operation's
own temporary output is cleaned on failure. A snapshot of an unfinished scan
remains unfinished and has no audit, even though it is a portable single file.
The default backup deadline is 60 seconds. Finalization has a separate 10-second
deadline; a reader that blocks checkpointing leaves `lifecycle=interrupted` and
`finish_reason=finalization_blocked`, preserving collection completeness separately.
After the reader closes, `resume_or_finalize()` with an empty frontier only
finalizes the file. A successful finalization marks the capture `finished` and
stamps `finished_at` once no queued or inflight frontier work remains; omitted
observations are carried by `crawl_partial`, so a drained queue with partial
evidence still finishes instead of reading as interrupted. An explicit stop --
aborted before it accepted work, cancelled, or halted by the error circuit --
keeps `interrupted` and its recorded reason even after the queue drains, so
cancel, error, and resume paths never receive a false finished timestamp; only
a `finalization_blocked` retry may still finish. WAL/SHM files are never
manually deleted.

The current native lanes use these versioned context rows:

| Kind / key | Exact `scan_context.v1` payload |
|---|---|
| `robots_blocked_url` / `url:<url_id>` | `{"url_id":positive_integer,"token":string,"policy":"respect or report_only"}` |
| `seed_url` / `url:<url_id>` | `{"url_id":positive_integer,"depth":0,"source":"sitemap"}` |
| `robots_summary` / `run` | `{"policy":string,"token":string,"fetch_state":"fetched or unavailable or not_fetched","final_response_id":null,"note":string,"parsed":{"groups":array,"sitemaps":array}}`; each group accepts the legacy four keys plus optional `request_rate_delay`. |
| `native_commit` / queue ordinal as decimal text | `{"digest":lowercase_sha256}` |
| `credential_context` / `run` | `{"verifier":null or lowercase_sha256,"implicit_state":boolean}`; it records redacted resume compatibility, never a credential value. |
| `reanalysis_provenance` / `run` | The closed parent/capture UUID, source/derived evidence revisions, source audit/build/runtime/config identities, capture build/runtime/config identities, and capture lifecycle. Its exact validator is [`validate_context`](../seohead/storage/native_context.py); reason is `offline reanalysis`. |
| `resource_inventory` / `document:<document_id>` | `{"document_id":positive_integer,"state":"complete or partial or unavailable","omitted":nonnegative_integer}`. |
| `resource_commit` / `resource:<ordinal>` | `{"digest":lowercase_sha256,"requests_used":nonnegative_integer}`. |
| `sitemap_declaration` / `ordinal:<root_ordinal>` | `{"sitemap_url_id":positive_integer,"source":"explicit or robots","ordinal":nonnegative_integer}`. This names one selected expanded root, including its nested sitemap indexes. |
| `sitemap_declared_url` / `sitemap:<sitemap_url_id>:ordinal:<global_ordinal>` | `{"sitemap_url_id":positive_integer,"url_id":positive_integer,"ordinal":nonnegative_integer}`. `ordinal` is run-wide post-expansion normalization/deduplication order across roots, never a direct XML-line claim. |
| `sitemap_fetch_summary` / `url:<sitemap_url_id>` | `{"sitemap_url_id":positive_integer,"response_ids":[positive_integer],"complete":boolean,"reason":string}`. Current sitemap-fetch capture has `response_ids: []`; response provenance belongs to the later response/body lane. |

Unknown context kinds/keys and extra payload fields are refused. A robots context
references this scan's URL and measured policy; `native_commit` belongs to a done
frontier row. Sitemap reconciliation stops at the first selected root whose
summary is partial or failed: it returns an unavailable state rather than a
shortened replay that could label absent membership as an orphan or a clean empty
sitemap. Exclusion counts derive from decision occurrences. Raw start-page HTML
is not hidden in a context row: it requires the later document/body lane.
This native-core context table predates the later body/resource lanes. Network
replay remains unavailable; retained-evidence reanalysis is documented above.


### Streamed report compatibility

Saved finding views are currently unavailable for streamed audit.v2 report inputs.
The report builder refuses that combination explicitly rather than silently ignoring
the selected view. Project checklist coverage is bound on the streaming path too:
`report-build --project` and `audit-workflow --action report` attach the same
checklist snapshot as inline reports, and a budget-capped (`stopped_by_budget`)
scan is accepted as a scope-limited audit. Streamed JSON with `--project` is refused,
because the JSON is written verbatim. Bounded inline audit reports retain
their view/language support.

For streamed PDF output, explicitly pass `pdf_policy="overview-v1"` (CLI
`--pdf-policy overview-v1`). It creates a bounded PDF view with complete JSON/CSV
companions and a hashed manifest; no policy is selected by default. See
[PDF_OVERVIEW.md](PDF_OVERVIEW.md) for counts, limits and publication guarantees.

## Offline link positions, graph and sitemap output

```bash
seohead scan-graph --scan scan.sqlite --section /catalog/ --positions content,breadcrumbs
seohead scan-graph --scan scan.sqlite --around https://example.com/product/ --depth 2
seohead scan-graph --scan before.sqlite --compare-to after.sqlite --aggregate sections
seohead scan-reanalyze --scan old.sqlite --out batch-1.sqlite --step links.position --max-documents 100
seohead scan-reanalyze --scan batch-1.sqlite --out batch-2.sqlite --step links.position --max-documents 100
seohead scan-sitemap-generate --scan scan.sqlite --out-dir generated-sitemaps \
  --sitemap-base-url https://example.com/ --include-images --exclude '*[?]*'
```

Graph results are versioned JSON chunks with explicit totals, node/edge budgets and clipping.
`not_classified` is a retained position state, not evidence that a link belongs to body content.
Position backfill only updates the new derived scan; complete batches can be resumed by using
the previous output as the next source. A process interruption before atomic publication requires
restarting that unpublished batch. The frozen `scan.v1` schema is unchanged.

Sitemap generation writes local files into a new directory and refuses existing destinations.
Deployment URL scope determines eligible page URLs; the sitemap index contains absolute URLs
for the generated files. Unknown modification dates are omitted, and optional image coverage
reports unavailable/truncated bodies rather than treating them as empty pages.

Graph comparison applies section, position and representation filters across both scans.
The `around` option limits the displayed current graph; it does not narrow comparison scope.
