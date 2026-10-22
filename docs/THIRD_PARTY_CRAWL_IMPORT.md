# Third-party crawl CSV import

`seohead crawl-import` reads a local, explicitly mapped CSV bundle and returns a
`third_party_crawl.v1` document. It does not crawl, change its inputs, or write a
native SQLite scan. The result keeps the source product and export identity and
reports coverage for each normalized field. It is not `scan.v1`, `audit.json`, or
an Export Analyzer export, and it cannot be passed to `sf run` as one.

## Manifest

Pass a `third_party_crawl_manifest.v1` JSON file. Dataset paths are relative to
the manifest directory. The importer does no filename or header guessing; map
source headers to the closed field names below.

```json
{
  "schema_version": "third_party_crawl_manifest.v1",
  "source": {
    "product": "ExampleSpider",
    "version": "1.0",
    "format": "csv_bundle",
    "format_version": "1",
    "exported_at": "2026-10-03T10:00:00Z",
    "crawl_state": "unknown"
  },
  "datasets": {
    "pages": {
      "file": "pages.csv",
      "columns": {
        "url": "Address",
        "status_code": "Response Code",
        "title": "Title",
        "canonical_url": "Canonical"
      }
    },
    "links": {
      "file": "links.csv",
      "columns": {"source_url": "From", "destination_url": "To", "anchor": "Anchor"}
    },
    "statuses": {"file": null, "reason": "the source did not export a status table"},
    "redirects": {"file": "redirects.csv", "columns": {"source_url": "From", "destination_url": "To", "status_code": "Code", "hop": "Hop"}}
  }
}
```

`source.product`, `version`, `format`, and `format_version` identify the foreign
producer and export. `crawl_state` is an optional source claim: `complete`,
`partial`, or `unknown`. The output marks it as source-claimed and unverified.
`exported_at` and `crawl_state_reason` are optional strings.

The manifest may omit a dataset; it is then reported as unavailable with reason
`dataset was not declared in the manifest`. To explain an intentionally missing
table, declare it with `file: null` and a non-empty `reason`. A mapped field whose
source header is absent is unavailable; a field with no mapping is
`not_exported`. These states remain distinct from an empty result.

## Canonical datasets

All CSV cells begin as strings. The adapter converts integer fields strictly and
keeps invalid source text under `unparsed_values` with a row-level error. Blank
cells become null and are counted as missing. No unmapped source column is
guessed; its header remains listed under dataset provenance.

| Dataset | Supported fields and types |
|---|---|
| `pages` | `url` (URL), `status_code` (integer 100–599), `content_type` (string), `title` (string), `canonical_url` (URL), `indexability` (string) |
| `links` | `source_url` (URL), `destination_url` (URL), `anchor` (string), `rel` (string), `position` (string), `status_code` (integer 100–599) |
| `statuses` | `url` (URL), `status_code` (integer 100–599), `status_text` (string) |
| `redirects` | `source_url` (URL), `destination_url` (URL), `status_code` (integer 100–599), `hop` (non-negative integer) |

URL fields preserve the exact source string and add a separate `*_key`. The key
uses SEOHEAD's current URL identity normalization: lower-case host, drop default
ports and fragments, and remove trailing slashes from non-root paths; query and
path case remain. Invalid or credential-bearing URLs remain visible as an error
and have no normalized key; credential-bearing values are omitted from the
returned document.

Every field reports `complete`, `partial`, `unavailable`, or `not_exported`, with
row, present, missing, and invalid counts. `complete` means only that all rows in
the supplied CSV had a valid mapped value. It does not mean that the source
crawled the whole site or captured every record. Duplicate normalized identities
are counted, and all duplicate observations stay in `records`.

## Run and limits

```bash
seohead crawl-import --manifest third_party_crawl/full/manifest.json
```

The same handler is available as MCP `seo_crawl_import(manifest_path=...)` and
through `--input '{"manifest_path":"./export/import.json"}'`. The command is
offline and read-only. It returns the normalized rows along with source identity,
file hashes, dataset counts, duplicate counts, field coverage, and whether the
source claimed a complete or partial crawl.

Version 1 accepts UTF-8 CSV files only. The JSON manifest is limited to 64 KiB;
all CSV datasets together are limited to 32 MiB and each dataset to 100,000 data
rows. Paths must be relative to the manifest directory, remain within it, and
resolve through regular non-symlink files. Oversized, malformed, ambiguous, or
unsafe input fails explicitly; rows are never silently truncated. Run
`seohead crawl-enrich` separately when joining a crawl/audit to URL-keyed traffic
or search data; that join is not crawl ingestion.

Synthetic complete and partial examples used by offline tests live under
`tests/fixtures_third_party_crawl/`.
