# Saved-audit comparison and targeted verification

Small comparisons keep the `compare.v1` inline result. Pass an explicit, new
`out_dir` to the core `compare` function or `compare_crawls` handler (`--out-dir`
in the CLI) to write a `compare.v2` directory. An audit.v2 input with more than
10,000 pages or issues requires this explicit file destination. Without it the
operation refuses before reading the collections; it never silently samples or
creates files in an implicit directory.

## Complete comparison files

The disk-backed path streams both validated audit sources into a temporary SQLite
index, with an 8 MiB page cache and file-backed temporary sorting. Working memory
scales with an individual retained row, header, and bounded correspondence
declaration rather than the URL/finding population. It does not materialize
an audit.v2 document or load all finding keys into a Python set.

The new output directory contains:

- `compare.json`: `compare.v2` manifest, exact source audit SHA-256 and scan UUID,
  timestamps, page counts, compatibility bases, warnings and conservation totals.
- `entered.ndjson`, `left.ndjson`, `appeared.ndjson`, `disappeared.ndjson`: complete
  original finding rows, ordered by check and exact original target URL.
- `unchanged.ndjson`: both original rows, as `before` and `after`, for each shared
  finding key. A shared key does **not** assert equal evidence. Both evidence
  objects remain available even when details or suppression fields changed.
- `by_check.ndjson`: full counts for the four delta categories per check.
- With correspondence: `correspondence.ndjson` and `facts.ndjson`, including
  unmatched declarations and measured/absent/unavailable page facts.

Each file entry has a relative `path`, `format`, `rows`, `bytes`, and SHA-256.
The returned result includes the absolute `manifest` and `out_dir` paths.
The manifest is published last, after all streams have been written and flushed.
Existing destination directories are rejected, including empty directories.
Temporary SQL data is removed; it is not part of the artifact contract.

All findings are retained, including rows marked suppressed. No saved view is
implicitly applied. Consumers can filter the exported rows after checking source
identity; filtered input is not a substitute for a full comparison denominator.

The following equalities are checked before publication:

```text
before_issues = left + disappeared + unchanged
after_issues  = entered + appeared + unchanged
```

`conservation.state=complete` proves these row accounting equalities, not crawl
completeness or successful remediation. Invalid/partial runs and incompatible or
unknown scope, configuration, representation, corpus and provider bases remain
explicit in warnings and compatibility rows. Different recorded crawl settings
still require `force=True`. Missing observations are not verified fixes.

The corpus basis remains `unknown` if either audit lacks a boolean
`run.corpus_partial`; two missing flags are not evidence of equal completeness.
Known partial corpora remain warned even when both sides have the same partial
state. Legacy inputs are never assigned `false` by the comparison consumer.

`measurement_gaps` retains each side's skipped/disabled check ID and reason.
`left` requires the URL to be fetched by both crawls. A finding whose URL the later crawl
never fetched (partial crawl, budget stop) lands in `disappeared`, which means not rechecked, never fixed.

The legacy four delta categories are still observational row differences:
for example, an after-run that skipped `SITEMAP_STALE_LASTMOD` may put an old
finding in `left` or `disappeared`, but its explicit unmeasured-check warning
prevents interpreting that bucket as proof of a fix. Use targeted verification
with measured clean check coverage for a remediation verdict.

## Optional lossless gzip

Pass `compression="gzip"` with an explicit `out_dir` (`--compression gzip` in the
CLI) to compress every complete NDJSON stream with the Python standard library.
The default `compression="none"` preserves existing plain output. Gzip headers
omit filename and clock values, so repeated exports have identical stored bytes.
There is no uncompressed intermediate export and no row omission or truncation.

Compressed entries use `format="ndjson.gz"`, `compression="gzip"`, and a
`.ndjson.gz` filename. `bytes` and `sha256` describe the compressed file;
`uncompressed_bytes` describes the original complete JSONL byte stream, and
`rows` remains the full uncompressed row count. Source identities, ordering,
summary and conservation semantics are unchanged.

`seohead.sf.core.compare_store.iter_compare_rows(manifest_path, name)` supports
both formats. It verifies stored bytes and their hash before yielding, then
streams decompression and checks JSONL framing, gzip integrity, full row counts
and decoded byte counts. Exhaust the iterator before accepting the file.
Truncation or tampering raises `CompareError`. The manifest is the atomic commit
marker: it is published only after all complete flushed streams are in place;
failed exports expose no completed package.

## Identity and correspondence

URL identity is exact: no normalization, redirect inference, path decoding, or
content matching. Duplicate page URLs and duplicate `(check, target_url)` finding
keys fail instead of choosing a first or last row. Correspondence is applied only
to declared, measured pairs. Mappings that collide with an unchanged baseline URL
also fail; they cannot silently collapse the denominator.

The existing `url-correspondence.v1` declaration is deliberately small: at most
10,000 explicit pairs and 10,000 origins, and a JSON declaration file at most
16 MiB. Use `origin_map` for whole-origin migrations; it can map the full page
population through the SQL index. Larger explicit declarations are refused,
never truncated. These limits do not cap the audit population or result streams.

## Bounded rechecks

`verify_fixes` retains its targeted selection limits: 500 crawl targets and 5,000
findings. It streams audit.v2 `/issues` and `/pages` and retains only the selected
population. Saved verification views select a union of finding IDs, URLs and
checks. Selection beyond the limits fails; it is never represented by a sample.
Duplicate selected finding IDs/keys or page URLs fail, including duplicates in
the after audit that would otherwise make a compact selection grow unbounded.

The exact validated source binding supplies audit hash and scan UUID. A conflicting
UUID in the audit header is refused. Offline verification requires a distinct
scan UUID and a later timezone-aware observation timestamp. Reanalysis of the
same scan is not fresh evidence. An invalid after crawl cannot resolve findings.
A partial crawl can verify an actually measured selected page only when check
coverage, policy, response and representation support that local verdict; missing
pages and unmeasured or graph-wide checks remain `not_verifiable`.

## Reading and inspecting a comparison

Use `compare-read` (MCP `seo_compare_read`) to page a complete compare.v2 stream by
`file`, with a stable offset, a bounded row count and byte budget, a URL substring
filter, and sorting by URL, check or severity. The reader validates the manifest,
checksum, row framing and complete stored stream before returning a page. Sorting
uses a temporary disk-backed SQLite index; a page read can therefore take time
proportional to the selected stream. The output remains capped at 500 rows and 8 MiB.

Use `compare-url` (MCP `seo_compare_url`) for one exact URL. It returns before and
after page facts, measured field deltas, and finding changes. Missing URLs yield
`not_rechecked`; unavailable facts stay unavailable. For two native scan.v1 SQLite
inputs it also requests a text diff capped at 200 lines and 64 KiB per side from
the retained-body diff primitive. It makes no network request.

`scan-url-history` now includes title, meta description, H1, word count, observed
inlinks/outlinks, response time and HTML size alongside status, indexability,
canonical, redirect and body identity. Adjacent `transitions` compare fields only
when both scans contain a readable URL observation; gaps are `not_comparable`.

The compare.v2 manifest includes source-declared segment page/issue count deltas
when both audits retained segment summaries. If either side lacks them, segment
breakdown is explicitly unavailable. Comparison generation is still a synchronous
operation; progress checkpoints, resumable generation and a reusable comparison
cache are not provided by this interface.

Use `compare-report --manifest compare/compare.json --out-file comparison.md` to
write a new Markdown summary. It reports aggregate deltas, source-declared segment
counts, per-check counts and bounded caveats; it includes no page URLs. The report
is limited to 64 KiB and 500 checks, and an existing destination is refused.
