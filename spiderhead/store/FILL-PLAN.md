# FILL-PLAN — crate `spiderhead-store`

Skeleton by W2. Every reference module has one Rust module; items carry `// slice: <id>` and a `todo!("slice <id>")` body.
Fully implemented already: `schema_sql` (DDL, header pragmas, hashes), `rows::{scan,audit,ledger}` (row structs generated from the DDL), `reader::ScanReader` (read-only scan.v1: header, urls, pages), `error`.

## Rules for fillers

- Edit only the files of your slice (listed below). Do not touch `lib.rs`, `error.rs`, `seams.rs`, `rows/`, `schema_sql.rs`; if you need a new type in another slice's file, add it in your file and note it in the report.
- Delete the module-level `#![allow(unused_variables, ...)]` line of every file you finish.
- reference `dict` became `BTreeMap` (key order is sorted, not insertion): where JSON output order matters (oracle baselines are `sort_keys`, so mostly fine) use `serde_json::Map`/Vec of pairs instead. reference generators became `Vec<T>`; change to an iterator where the data is large.
- Signatures are mechanical translations of the reference annotations (`Result<_, StoreError>` where reference raises or does IO; unannotated or opaque types are `serde_json::Value`). Adjust them freely inside your slice; callers in other slices are stubs, so changing a signature is cheap now and expensive after merge.
- Types from crates not yet merged are in `seams.rs` (`BeautifulSoup`, `Array`) or resolved into `spiderhead_core::...`. Types expected from `spiderhead-parse` (ParsedPage and friends) do not appear in the store stubs: the store only receives their serialised form.
- A test that calls a function of another slice gets `#[ignore = "needs <slice>"]` until that slice is merged.
- Oracle gate: `cargo xtask oracle --slice <id>`; the baseline files are under `oracle/baselines/<site>/` (sites: basic, canonical-hreflang, errors, redirects, robots-sitemap, schema) and the raw files under `oracle/raw-scans/<site>/`.

## Slices

### `crawl-pack-01-store` — wave W2, order 2, cycle group `scc-crawl-pack-01-store`, ~644 reference lines

| reference | Rust file | stubs (`todo!`) | notes |
|---|---|---|---|
| the reference suite | `spiderhead/store/src/crawl_capture.rs` | 8 |  |
| the reference suite | `spiderhead/store/src/crawl_events.rs` | 7 | has reference module-level state/non-literal constants (comments `reference module state`) |
| the reference suite | `spiderhead/store/src/crawl_list_input.rs` | 7 |  |

Total stubs: **22**.

**Tests to port (199 reference test functions in 18 files):** the reference suite (2), the reference suite (1), the reference suite (17), the reference suite (1), the reference suite (15), the reference suite (8), the reference suite (4), the reference suite (26), the reference suite (2), the reference suite (17), the reference suite (10), the reference suite (7), the reference suite (19), the reference suite (33), the reference suite (12), the reference suite (10), the reference suite (5), the reference suite (10)

**Calls into other store slices** (tests that need them: `#[ignore = "needs <slice>"]`):
- `storage-pack-01-store`: `bodies::decode_entity`

**Uses other crates** (check the Rust module exists on `main`; otherwise stub behind a trait):
- `core:core-pack-01`: `utc_iso_z`
- `core:storage-artifacts`: `CRAWL_TIMELINE_V1`

**Oracle baselines to match:** crawl-site.json; scan.tables.json

### `sf-core-pack-01-store` — wave W2, order 2, cycle group `scc-crawl-pack-01-store`, ~1375 reference lines

| reference | Rust file | stubs (`todo!`) | notes |
|---|---|---|---|
| the reference suite | `spiderhead/store/src/corpus_derivations.rs` | 23 |  |
| the reference suite | `spiderhead/store/src/evidence_contract.rs` | 20 |  |

Total stubs: **43**.

**Tests to port (29 reference test functions in 3 files):** the reference suite (1), the reference suite (25), the reference suite (3)

**Calls into other store slices** (tests that need them: `#[ignore = "needs <slice>"]`):
- `storage-pack-02`: `content_evidence::DerivedRows`, `content_evidence::derive_duplicates`, `content_evidence::read`
- `storage-pack-09`: `structured_evidence::read`, `target_probes::load`, `target_probes::observation`

**Uses other crates** (check the Rust module exists on `main`; otherwise stub behind a trait):
- `core:core-norm-url`: `norm_url`
- `formats:sf-core-pack-01`: `_Rows`, `entries`
- `formats:sf-core-registry`: `CHECKS`
- `parse:checks-parser-pack-01`: `robots_directives`
- `parse:parse-hreflang-code-error`: `code_error`

**Oracle baselines to match:** scan-findings-page.json (via evidence contract)

### `storage-__init__` — wave W2, order 2, cycle group `scc-crawl-pack-01-store`, ~1352 reference lines

| reference | Rust file | stubs (`todo!`) | notes |
|---|---|---|---|
| the reference suite | `spiderhead/store/src/scan_artifact.rs` | 39 | has reference module-level state/non-literal constants (comments `reference module state`) |

Total stubs: **39**.

**Tests to port (1128 reference test functions in 103 files):** the reference suite (3), the reference suite (42), the reference suite (11), the reference suite (3), the reference suite (22), the reference suite (11), the reference suite (20), the reference suite (2), the reference suite (6), the reference suite (7), the reference suite (4), the reference suite (26), the reference suite (12), the reference suite (30), the reference suite (39), the reference suite (19), the reference suite (2), the reference suite (31), the reference suite (5), the reference suite (18), the reference suite (8), the reference suite (23), the reference suite (17), the reference suite (7), the reference suite (12), the reference suite (1), the reference suite (17), the reference suite (2), the reference suite (7), the reference suite (12), the reference suite (5), the reference suite (2), the reference suite (2), the reference suite (11), the reference suite (4), the reference suite (14), the reference suite (4), the reference suite (21), the reference suite (15), the reference suite (10) …

**Calls into other store slices** (tests that need them: `#[ignore = "needs <slice>"]`):
- `storage-pack-01-store`: `audit_v2::AuditV2Reader`, `audit_v2::audit_v2_path`
- `storage-pack-06`: `native_scan::NativeScan`, `read_budget::ReadConnection`

**Uses other crates** (check the Rust module exists on `main`; otherwise stub behind a trait):
- `core:core-pack-01`: `fsync_directory`, `open_readonly`, `open_writer`
- `core:storage-artifacts`: `SCAN_V1`

**Oracle baselines to match:** scan.tables.json; raw-scans/*/scan.sqlite (open/validate)

### `storage-pack-01-store` — wave W2, order 2, cycle group `scc-crawl-pack-01-store`, ~2413 reference lines

| reference | Rust file | stubs (`todo!`) | notes |
|---|---|---|---|
| the reference suite | `spiderhead/store/src/analysis_graph.rs` | 14 |  |
| the reference suite | `spiderhead/store/src/analysis_paths.rs` | 10 |  |
| the reference suite | `spiderhead/store/src/analysis_score.rs` | 6 |  |
| the reference suite | `spiderhead/store/src/audit_v2.rs` | 26 | has reference module-level state/non-literal constants (comments `reference module state`) |
| the reference suite | `spiderhead/store/src/bodies.rs` | 15 |  |
| the reference suite | `spiderhead/store/src/body_diff.rs` | 11 |  |

Total stubs: **82**.

**Tests to port (485 reference test functions in 48 files):** the reference suite (7), the reference suite (4), the reference suite (1), the reference suite (22), the reference suite (4), the reference suite (17), the reference suite (25), the reference suite (16), the reference suite (7), the reference suite (16), the reference suite (23), the reference suite (12), the reference suite (17), the reference suite (7), the reference suite (12), the reference suite (17), the reference suite (5), the reference suite (11), the reference suite (4), the reference suite (6), the reference suite (1), the reference suite (30), the reference suite (8), the reference suite (3), the reference suite (8), the reference suite (12), the reference suite (1), the reference suite (3), the reference suite (7), the reference suite (5), the reference suite (7), the reference suite (16), the reference suite (18), the reference suite (7), the reference suite (8), the reference suite (4), the reference suite (23), the reference suite (7), the reference suite (19), the reference suite (12) …

**Calls into other store slices** (tests that need them: `#[ignore = "needs <slice>"]`):
- `storage-__init__`: `scan_artifact::ScanError`, `scan_artifact::open_scan`, `scan_artifact::open_scan_mode`

**Uses other crates** (check the Rust module exists on `main`; otherwise stub behind a trait):
- `core:core-pack-01`: `AnchorGroup`, `DuplicateLinkGroup`, `InlinkCompositionRow`, `filesystem`, `open_readonly`
- `core:crawl-settings`: `MAX_RENDER_PAGE_CONCURRENCY`
- `core:storage-artifacts`: `AUDIT_V2`
- `parse:checks-pack-02-parse`: `expand_navigation`, `retain_navigation`, `validate_navigation`
- `render:checks-render-__init__`: `legacy_fragment_target`

**Oracle baselines to match:** audit.v2.json; scan-graph.json (analysis_*)

### `storage-pack-02` — wave W2, order 2, cycle group `scc-crawl-pack-01-store`, ~1879 reference lines

| reference | Rust file | stubs (`todo!`) | notes |
|---|---|---|---|
| the reference suite | `spiderhead/store/src/browser_artifacts.rs` | 13 |  |
| the reference suite | `spiderhead/store/src/click_depth.rs` | 3 |  |
| the reference suite | `spiderhead/store/src/config_diff.rs` | 3 |  |
| the reference suite | `spiderhead/store/src/content_evidence.rs` | 14 |  |
| the reference suite | `spiderhead/store/src/content_search.rs` | 12 |  |

Total stubs: **45**.

**Tests to port (46 reference test functions in 6 files):** the reference suite (1), the reference suite (11), the reference suite (5), the reference suite (23), the reference suite (5), the reference suite (1)

**Calls into other store slices** (tests that need them: `#[ignore = "needs <slice>"]`):
- `storage-__init__`: `scan_artifact::READ_TIMEOUT_SECONDS`, `scan_artifact::ScanError`, `scan_artifact::open_scan`
- `storage-pack-01-store`: `bodies::read_document`

**Uses other crates** (check the Rust module exists on `main`; otherwise stub behind a trait):
- `core:crawl-settings`: `_flatten`
- `parse:checks-pack-01-parse`: `DEFAULT_TIMEOUT_SECONDS`, `NO_BOILERPLATE_REGIONS`, `_run_with_budget`, `_target_text`, `_validate_selector_syntax`, `boilerplate_hash`, `extract_markdown`, `simhash`

**Oracle baselines to match:** scan.tables.json (documents/context_items)

### `storage-pack-03` — wave W2, order 2, cycle group `scc-crawl-pack-01-store`, ~1982 reference lines

| reference | Rust file | stubs (`todo!`) | notes |
|---|---|---|---|
| the reference suite | `spiderhead/store/src/corpus.rs` | 23 |  |
| the reference suite | `spiderhead/store/src/corpus_inputs.rs` | 7 |  |
| the reference suite | `spiderhead/store/src/corpus_validation.rs` | 8 |  |
| the reference suite | `spiderhead/store/src/credential_context.rs` | 3 |  |
| the reference suite | `spiderhead/store/src/directory_tree.rs` | 4 |  |

Total stubs: **45**.

**Tests to port (173 reference test functions in 14 files):** the reference suite (42), the reference suite (10), the reference suite (6), the reference suite (20), the reference suite (8), the reference suite (7), the reference suite (8), the reference suite (14), the reference suite (7), the reference suite (19), the reference suite (5), the reference suite (9), the reference suite (13), the reference suite (5)

**Calls into other store slices** (tests that need them: `#[ignore = "needs <slice>"]`):
- `crawl-pack-01-store`: `crawl_capture::CaptureEvent`, `crawl_capture::TRANSPORT_PHASES`
- `storage-__init__`: `scan_artifact::READ_TIMEOUT_SECONDS`, `scan_artifact::ScanError`, `scan_artifact::open_scan`, `scan_artifact::open_scan_mode`
- `storage-pack-01-store`: `bodies::DECODER_VERSION`, `bodies::decode_entity`, `bodies::encode_body`, `bodies::read_document`, `bodies::renderer`
- `storage-pack-07`: `resources::media_matches`, `resources::resource_capabilities`, `resources::validate_resources`, `retention::validate_policy`
- `storage-pack-09`: `transport::record`
- `store-crawl-records`: `crawl_records/evidence::indexability`

**Uses other crates** (check the Rust module exists on `main`; otherwise stub behind a trait):
- `core:core-http-cache-control`: `parse_cache_control`
- `core:crawl-settings`: `MAX_RENDER_PAGE_CONCURRENCY`, `redact_sensitive_headers`
- `core:storage-artifacts`: `SCAN_V1`
- `parse:checks-pack-01-parse`: `boilerplate_hash`, `extract_markdown`
- `parse:checks-pack-02-parse`: `normalization_policy`, `normalize_document`, `retain_navigation`

**Oracle baselines to match:** scan.tables.json (bodies/documents); scan-directory-tree.json

### `storage-pack-04` — wave W2, order 2, cycle group `scc-crawl-pack-01-store`, ~2062 reference lines

| reference | Rust file | stubs (`todo!`) | notes |
|---|---|---|---|
| the reference suite | `spiderhead/store/src/discovery_ledger.rs` | 10 |  |
| the reference suite | `spiderhead/store/src/events.rs` | 4 |  |
| the reference suite | `spiderhead/store/src/exports.rs` | 9 |  |
| the reference suite | `spiderhead/store/src/external_checks.rs` | 4 |  |
| the reference suite | `spiderhead/store/src/fragment_links.rs` | 13 |  |
| the reference suite | `spiderhead/store/src/frontier.rs` | 6 |  |
| the reference suite | `spiderhead/store/src/head_links.rs` | 5 |  |

Total stubs: **51**.

**Tests to port (209 reference test functions in 14 files):** the reference suite (2), the reference suite (7), the reference suite (30), the reference suite (39), the reference suite (18), the reference suite (12), the reference suite (1), the reference suite (31), the reference suite (10), the reference suite (7), the reference suite (33), the reference suite (7), the reference suite (4), the reference suite (8)

**Calls into other store slices** (tests that need them: `#[ignore = "needs <slice>"]`):
- `crawl-pack-01-store`: `crawl_events::FORMAT`, `crawl_events::MAX_EVENTS`, `crawl_events::validate`
- `storage-__init__`: `scan_artifact::LATE_PAGE_FIELDS`, `scan_artifact::MAX_RECORD_BYTES`, `scan_artifact::PAGE_BOOLS`, `scan_artifact::ScanError`, `scan_artifact::dump`, `scan_artifact::insert`, `scan_artifact::open_scan`, `scan_artifact::url`
- `storage-pack-01-store`: `analysis_graph::selected_links_cte`, `bodies::read_document`

**Uses other crates** (check the Rust module exists on `main`; otherwise stub behind a trait):
- `core:core-pack-01`: `fsync_directory`
- `core:storage-artifacts`: `CRAWL_TIMELINE_V1`
- `parse:checks-parser-pack-01`: `document_base_url`, `is_inert_template_content`

**Oracle baselines to match:** scan.tables.json (frontier/events/external_checks); scan-export.json

### `storage-pack-05` — wave W2, order 2, cycle group `scc-crawl-pack-01-store`, ~2446 reference lines

| reference | Rust file | stubs (`todo!`) | notes |
|---|---|---|---|
| the reference suite | `spiderhead/store/src/history.rs` | 22 | has reference module-level state/non-literal constants (comments `reference module state`) |
| the reference suite | `spiderhead/store/src/inputs.rs` | 7 | has reference module-level state/non-literal constants (comments `reference module state`) |
| the reference suite | `spiderhead/store/src/issue_counters.rs` | 4 |  |
| the reference suite | `spiderhead/store/src/issue_selection.rs` | 2 |  |
| the reference suite | `spiderhead/store/src/link_context.rs` | 9 |  |
| the reference suite | `spiderhead/store/src/link_positions.rs` | 4 |  |
| the reference suite | `spiderhead/store/src/link_queries.rs` | 10 |  |
| the reference suite | `spiderhead/store/src/markup_evidence.rs` | 2 |  |
| the reference suite | `spiderhead/store/src/native_audit.rs` | 1 |  |

Total stubs: **61**.

**Tests to port (153 reference test functions in 17 files):** the reference suite (15), the reference suite (5), the reference suite (17), the reference suite (2), the reference suite (4), the reference suite (30), the reference suite (7), the reference suite (5), the reference suite (12), the reference suite (5), the reference suite (5), the reference suite (2), the reference suite (5), the reference suite (16), the reference suite (6), the reference suite (10), the reference suite (7)

**Calls into other store slices** (tests that need them: `#[ignore = "needs <slice>"]`):
- `storage-__init__`: `scan_artifact::APPLICATION_ID`, `scan_artifact::MAX_JSON_BYTES`, `scan_artifact::MAX_RECORD_BYTES`, `scan_artifact::READ_TIMEOUT_SECONDS`, `scan_artifact::ScanError`, `scan_artifact::USER_VERSION`, `scan_artifact::audit`, `scan_artifact::dump`, `scan_artifact::expected`, `scan_artifact::loads`, `scan_artifact::open_scan`, `scan_artifact::open_scan_mode`, `scan_artifact::runtime`, `scan_artifact::sha`
- `storage-pack-01-store`: `audit_v2::AuditReadTimeout`, `audit_v2::AuditV2Reader`, `audit_v2::audit_v2_path`, `bodies::read_document`
- `storage-pack-06`: `native_scan::NativeScan`
- `store-crawl-records`: `crawl_records/spider::Scope`, `crawl_records/spider::canonical_key`

**Uses other crates** (check the Rust module exists on `main`; otherwise stub behind a trait):
- `core:core-pack-01`: `filesystem`, `open_readonly`
- `core:crawl-settings`: `manifest`
- `core:storage-artifacts`: `SCAN_V1`
- `formats:sf-core-registry`: `CHECKS`
- `parse:checks-pack-01-parse`: `MAX_ANCHORS`, `SCHEMA_VERSION`, `extract_occurrences`
- `parse:checks-pack-02-parse`: `A11Y_CODES`, `AMP_CODES`

**Oracle baselines to match:** scan-findings-page.json (issue_selection/counters); scan-url-counts.json

### `storage-pack-06` — wave W2, order 2, cycle group `scc-crawl-pack-01-store`, ~1850 reference lines

| reference | Rust file | stubs (`todo!`) | notes |
|---|---|---|---|
| the reference suite | `spiderhead/store/src/native_context.rs` | 3 |  |
| the reference suite | `spiderhead/store/src/native_scan.rs` | 30 | has reference module-level state/non-literal constants (comments `reference module state`) |
| the reference suite | `spiderhead/store/src/read_budget.rs` | 10 |  |
| the reference suite | `spiderhead/store/src/render_summary.rs` | 3 |  |
| the reference suite | `spiderhead/store/src/rendered_routes.rs` | 7 |  |

Total stubs: **53**.

**Tests to port (969 reference test functions in 98 files):** the reference suite (3), the reference suite (6), the reference suite (42), the reference suite (1), the reference suite (4), the reference suite (17), the reference suite (11), the reference suite (4), the reference suite (3), the reference suite (15), the reference suite (1), the reference suite (1), the reference suite (7), the reference suite (5), the reference suite (15), the reference suite (16), the reference suite (15), the reference suite (13), the reference suite (20), the reference suite (2), the reference suite (4), the reference suite (26), the reference suite (2), the reference suite (5), the reference suite (19), the reference suite (1), the reference suite (17), the reference suite (17), the reference suite (2), the reference suite (7), the reference suite (12), the reference suite (6), the reference suite (2), the reference suite (17), the reference suite (2), the reference suite (5), the reference suite (6), the reference suite (8), the reference suite (10), the reference suite (5) …

**Calls into other store slices** (tests that need them: `#[ignore = "needs <slice>"]`):
- `crawl-pack-01-store`: `crawl_events::EventSink`, `crawl_events::MAX_EVENTS`, `crawl_events::validate`
- `storage-__init__`: `scan_artifact::MAX_JSON_BYTES`, `scan_artifact::ScanBackpressure`, `scan_artifact::ScanError`, `scan_artifact::audit`, `scan_artifact::dump`, `scan_artifact::expected`, `scan_artifact::insert`, `scan_artifact::sha`, `scan_artifact::url`, `scan_artifact::validate_audit_document`
- `storage-pack-01-store`: `audit_v2::AuditV2Reader`, `audit_v2::write_audit_v2`
- `storage-pack-02`: `browser_artifacts::validate_context`, `content_evidence::validate_context`
- `storage-pack-03`: `corpus::corpus_summary`, `corpus_validation::timestamp`
- `storage-pack-04`: `events::append`, `events::ensure_schema`, `events::set_coverage`, `external_checks::MAX_EXTERNAL_CHECKS`, `external_checks::summary_item`, `external_checks::validate_check`, `external_checks::validate_summary`, `head_links::validate_context`
- `storage-pack-05`: `link_positions::validate_checkpoint`, `markup_evidence::validate_context`, `native_audit::validate_audit`
- `storage-pack-07`: `resource_graph::validate_coverage_context`, `resources::validate_inventory_context`
- `storage-pack-08`: `scan_bodies::CONTENT_CAPTURE_FIELDS`, `scan_bodies::NativeScanBodiesMixin`, `scan_bodies::content_capture`, `scan_bodies::put_content_evidence`, `scan_links::NativeScanGraphMixin`, `scan_links::put_discovery_ledger`, `scan_links::put_resource_graph`, `sitemaps::context`, `sitemaps::declare`, `sitemaps::finish`, `sitemaps::root_ids`
- `storage-pack-09`: `structured_evidence::validate_context`, `target_probes::validate_context`
- `storage-scan_lease`: `scan_lease::BACKUP_TIMEOUT_SECONDS`, `scan_lease::CommitReceipt`, `scan_lease::FINALIZATION_TIMEOUT_SECONDS`, `scan_lease::Lease`, `scan_lease::MAX_EDGES_PER_PAGE`, `scan_lease::MAX_PAGE_COMMIT_ITEMS`, `scan_lease::NativeScanLeaseMixin`, `scan_lease::SNAPSHOT_RESERVE_BYTES`, `scan_lease::bounded_items`, `scan_lease::digest`, `scan_lease::fsync_directory`, `scan_lease::fsync_file`, `scan_lease::json_chunks`, `scan_lease::utc`
- `storage-scan_schema`: `scan_schema::CONFIG_EXTENSION_DEFAULTS`, `scan_schema::CONFIG_EXTENSION_SCHEMA`, `scan_schema::INSPECT_BYTES_PER_SECOND`, `scan_schema::INSPECT_MIN_TIMEOUT_SECONDS`, `scan_schema::NativeScanSchemaMixin`, `scan_schema::OPTIONAL_BROWSER_SETTINGS`, `scan_schema::RUNTIME_KEYS`, `scan_schema::config_extension_context`, `scan_schema::config_extension_values`, `scan_schema::default_inspect_timeout`, `scan_schema::effective_config_fingerprint`, `scan_schema::frozen_header_config`, `scan_schema::legacy_anonymous_session`, `scan_schema::native_config` …
- `store-crawl-records`: `crawl_records/security_headers::validate_redirect_hop_probe`

**Uses other crates** (check the Rust module exists on `main`; otherwise stub behind a trait):
- `core:core-pack-01`: `filesystem`
- `core:crawl-settings`: `DEFAULTS`, `fingerprint`
- `parse:checks-pack-01-parse`: `validate_result`

**Oracle baselines to match:** scan.tables.json (whole native writer output); scan-status.json

### `storage-pack-07` — wave W2, order 2, cycle group `scc-crawl-pack-01-store`, ~2146 reference lines

| reference | Rust file | stubs (`todo!`) | notes |
|---|---|---|---|
| the reference suite | `spiderhead/store/src/resource_graph.rs` | 16 |  |
| the reference suite | `spiderhead/store/src/resources.rs` | 6 |  |
| the reference suite | `spiderhead/store/src/retained_link_findings.rs` | 8 |  |
| the reference suite | `spiderhead/store/src/retention.rs` | 2 | has reference module-level state/non-literal constants (comments `reference module state`) |
| the reference suite | `spiderhead/store/src/retry.rs` | 12 |  |

Total stubs: **44**.

**Tests to port (197 reference test functions in 14 files):** the reference suite (42), the reference suite (11), the reference suite (4), the reference suite (7), the reference suite (4), the reference suite (15), the reference suite (19), the reference suite (19), the reference suite (20), the reference suite (12), the reference suite (7), the reference suite (19), the reference suite (9), the reference suite (9)

**Calls into other store slices** (tests that need them: `#[ignore = "needs <slice>"]`):
- `crawl-pack-01-store`: `crawl_list_input::read_url_list`
- `storage-__init__`: `scan_artifact::APPLICATION_ID`, `scan_artifact::MAX_RECORD_BYTES`, `scan_artifact::ScanError`, `scan_artifact::insert`, `scan_artifact::open_scan`, `scan_artifact::url`
- `storage-pack-01-store`: `analysis_graph::selected_links_cte`
- `storage-pack-03`: `corpus::corpus_summary`
- `storage-pack-04`: `discovery_ledger::ensure_schema`, `external_checks::ensure_schema`
- `storage-pack-05`: `history::hold_writer_lock`, `history::regular`
- `storage-pack-06`: `native_scan::NativeScan`, `native_scan::reanalysis_capability`
- `storage-pack-10`: `url_query_projection::invalidate`
- `store-asset-delivery`: `asset_delivery::resource_delivery_evidence`
- `store-crawl-records`: `crawl_records/spider::Scope`, `crawl_records/spider::strip_fragment`, `crawl_records/sqlite_adapter::headers`, `crawl_records/throttle::RequestBudgetExhausted`

**Uses other crates** (check the Rust module exists on `main`; otherwise stub behind a trait):
- `core:core-pack-01`: `file_sha256`, `open_readonly`, `unlock`
- `core:storage-artifacts`: `SCAN_V1`, `SCAN_V2`
- `crawl:crawl-collect`: `fetch_one`
- `parse:checks-parser-pack-01`: `document_base_url`

**Oracle baselines to match:** scan.tables.json (resource_refs)

### `storage-pack-08` — wave W2, order 2, cycle group `scc-crawl-pack-01-store`, ~2489 reference lines

| reference | Rust file | stubs (`todo!`) | notes |
|---|---|---|---|
| the reference suite | `spiderhead/store/src/scan_archive.rs` | 13 | has reference module-level state/non-literal constants (comments `reference module state`) |
| the reference suite | `spiderhead/store/src/scan_bodies.rs` | 9 |  |
| the reference suite | `spiderhead/store/src/scan_graph.rs` | 13 |  |
| the reference suite | `spiderhead/store/src/scan_links.rs` | 3 |  |
| the reference suite | `spiderhead/store/src/sitemap_generator.rs` | 12 |  |
| the reference suite | `spiderhead/store/src/sitemaps.rs` | 5 |  |
| the reference suite | `spiderhead/store/src/status.rs` | 1 |  |

Total stubs: **56**.

**Tests to port (80 reference test functions in 8 files):** the reference suite (5), the reference suite (17), the reference suite (9), the reference suite (13), the reference suite (7), the reference suite (5), the reference suite (18), the reference suite (6)

**Calls into other store slices** (tests that need them: `#[ignore = "needs <slice>"]`):
- `crawl-pack-01-store`: `crawl_capture::header_pairs`
- `storage-__init__`: `scan_artifact::MAX_RECORD_BYTES`, `scan_artifact::ScanError`, `scan_artifact::dump`, `scan_artifact::insert`, `scan_artifact::loads`, `scan_artifact::open_scan`, `scan_artifact::open_scan_mode`, `scan_artifact::url`
- `storage-pack-01-store`: `bodies::read_body`, `bodies::read_document`
- `storage-pack-03`: `corpus::store_rendered_document`, `corpus::store_response`
- `storage-pack-04`: `discovery_ledger::store_document_relations`, `exports::unlink_owned`, `exports::write_file`, `frontier::apply_candidates`
- `storage-pack-05`: `link_positions::position_summary`, `link_queries::representation`, `link_queries::spellings`, `link_queries::url`
- `storage-pack-06`: `native_context::put_context`, `rendered_routes::context_items`
- `storage-pack-07`: `resource_graph::store_document`, `resources::put_declarations`
- `storage-pack-09`: `url_links::FRAGMENT_VARIANTS`, `url_links::LinksError`, `url_links::header`, `url_links::ids`, `url_links::open_light`
- `storage-scan_export`: `scan_export::publish_files`
- `storage-scan_lease`: `scan_lease::Lease`, `scan_lease::MAX_EDGES_PER_PAGE`, `scan_lease::bounded_items`, `scan_lease::json_chunks`
- `store-crawl-records`: `crawl_records/content_evidence::capture`, `crawl_records/spider::canonical_key`

**Uses other crates** (check the Rust module exists on `main`; otherwise stub behind a trait):
- `core:core-norm-url`: `norm_url`
- `core:core-pack-01`: `fsync_directory`
- `parse:checks-parser-pack-01`: `document_base_url`, `extract_url_sources`, `image_url_sources`, `robots_directives`

**Oracle baselines to match:** scan-status.json; scan-graph.json; scan.tables.json (bodies)

### `storage-pack-09` — wave W2, order 2, cycle group `scc-crawl-pack-01-store`, ~1707 reference lines

| reference | Rust file | stubs (`todo!`) | notes |
|---|---|---|---|
| the reference suite | `spiderhead/store/src/structured_evidence.rs` | 10 |  |
| the reference suite | `spiderhead/store/src/target_probes.rs` | 8 |  |
| the reference suite | `spiderhead/store/src/transport.rs` | 2 |  |
| the reference suite | `spiderhead/store/src/url_history.rs` | 5 | has reference module-level state/non-literal constants (comments `reference module state`) |
| the reference suite | `spiderhead/store/src/url_links.rs` | 20 |  |

Total stubs: **45**.

**Tests to port (86 reference test functions in 7 files):** the reference suite (6), the reference suite (39), the reference suite (5), the reference suite (17), the reference suite (7), the reference suite (11), the reference suite (1)

**Calls into other store slices** (tests that need them: `#[ignore = "needs <slice>"]`):
- `crawl-pack-01-store`: `crawl_capture::TRANSPORT_PHASES`, `crawl_capture::unavailable_transport_phases`
- `storage-__init__`: `scan_artifact::APPLICATION_ID`, `scan_artifact::ScanError`, `scan_artifact::open_scan_mode`
- `storage-pack-02`: `content_evidence::ContextRows`
- `storage-pack-04`: `head_links::active`
- `storage-pack-05`: `history::catalog`, `link_queries::spellings`, `link_queries::url`
- `store-crawl-records`: `crawl_records/spider::Scope`, `crawl_records/throttle::RequestBudgetExhausted`

**Uses other crates** (check the Rust module exists on `main`; otherwise stub behind a trait):
- `core:core-norm-url`: `norm_url`
- `core:core-pack-01`: `open_readonly`
- `core:storage-artifacts`: `SCAN_FORMATS`
- `crawl:crawl-collect`: `fetch_one`
- `parse:checks-parser-pack-01`: `is_inert_template_content`, `robots_directives`
- `parse:checks-schema_org`: `MAX_HTML_BYTES`, `MAX_MARKUP_NODES`, `check_schema`

**Oracle baselines to match:** scan.tables.json (links/transport)

### `storage-pack-10` — wave W2, order 2, cycle group `scc-crawl-pack-01-store`, ~2119 reference lines

| reference | Rust file | stubs (`todo!`) | notes |
|---|---|---|---|
| the reference suite | `spiderhead/store/src/url_query.rs` | 11 |  |
| the reference suite | `spiderhead/store/src/url_query_filters.rs` | 9 |  |
| the reference suite | `spiderhead/store/src/url_query_presets.rs` | 5 | has reference module-level state/non-literal constants (comments `reference module state`) |
| the reference suite | `spiderhead/store/src/url_query_projection.rs` | 3 |  |
| the reference suite | `spiderhead/store/src/url_query_schema.rs` | 2 | has reference module-level state/non-literal constants (comments `reference module state`) |

Total stubs: **30**.

**Tests to port (104 reference test functions in 6 files):** the reference suite (39), the reference suite (7), the reference suite (5), the reference suite (9), the reference suite (30), the reference suite (14)

**Calls into other store slices** (tests that need them: `#[ignore = "needs <slice>"]`):
- `storage-__init__`: `scan_artifact::APPLICATION_ID`, `scan_artifact::ScanError`
- `storage-pack-01-store`: `audit_v2::AuditReadTimeout`
- `storage-pack-05`: `issue_counters::summarize`, `issue_selection::UnmeasuredChecks`, `issue_selection::materialize`
- `storage-scan_export`: `scan_export::publish_files`, `scan_export::write_csv_bytes`, `scan_export::write_xlsx`

**Uses other crates** (check the Rust module exists on `main`; otherwise stub behind a trait):
- `core:core-pack-01`: `open_readonly`

**Oracle baselines to match:** scan-url-query.json; scan-url-counts.json

### `storage-scan_export` — wave W2, order 2, cycle group `scc-crawl-pack-01-store`, ~1066 reference lines

| reference | Rust file | stubs (`todo!`) | notes |
|---|---|---|---|
| the reference suite | `spiderhead/store/src/scan_export.rs` | 28 |  |

Total stubs: **28**.

**Tests to port (31 reference test functions in 1 files):** the reference suite (31)

**Calls into other store slices** (tests that need them: `#[ignore = "needs <slice>"]`):
- `sf-core-pack-01-store`: `evidence_contract::capability_rows`
- `storage-__init__`: `scan_artifact::ScanError`, `scan_artifact::loads`, `scan_artifact::open_scan`
- `storage-pack-01-store`: `audit_v2::AuditV2Reader`, `audit_v2::audit_v2_path`
- `storage-pack-04`: `exports::link_rows`, `exports::page_rows`, `exports::unlink_owned`, `exports::write_file`
- `storage-pack-05`: `inputs::is_sqlite_input`

**Uses other crates** (check the Rust module exists on `main`; otherwise stub behind a trait):
- `core:core-pack-01`: `fsync_directory`
- `formats:formats-audit-kind`: `_detect_kind`, `neutralize_formula`
- `formats:sf-core-registry`: `CHECKS`

**Oracle baselines to match:** scan-export.json

### `storage-scan_lease` — wave W2, order 2, cycle group `scc-crawl-pack-01-store`, ~1502 reference lines

| reference | Rust file | stubs (`todo!`) | notes |
|---|---|---|---|
| the reference suite | `spiderhead/store/src/scan_lease.rs` | 30 |  |

Total stubs: **30**.

**Tests to port (32 reference test functions in 1 files):** the reference suite (32)

**Calls into other store slices** (tests that need them: `#[ignore = "needs <slice>"]`):
- `storage-__init__`: `scan_artifact::MAX_RECORD_BYTES`, `scan_artifact::PAGE_BOOLS`, `scan_artifact::PAGE_NONNEGATIVE_INTS`, `scan_artifact::ScanError`, `scan_artifact::config`, `scan_artifact::dump`, `scan_artifact::duplicate_ids`, `scan_artifact::heading_outline`, `scan_artifact::insert`, `scan_artifact::invalid_link_targets`, `scan_artifact::link_placement`, `scan_artifact::trust_signals`, `scan_artifact::url`
- `storage-pack-02`: `click_depth::persist`
- `storage-pack-03`: `corpus::store_response`
- `storage-pack-04`: `discovery_ledger::store_seeds`, `frontier::apply_candidates`, `frontier::apply_seeds`
- `storage-pack-06`: `native_context::put_context`, `rendered_routes::context_items`
- `storage-pack-07`: `resources::put_declarations`
- `storage-pack-08`: `scan_bodies::content_capture`, `scan_bodies::put_content_evidence`, `scan_links::put_discovery_ledger`, `scan_links::put_resource_graph`
- `storage-pack-10`: `url_query_projection::persist`
- `storage-scan_schema`: `scan_schema::effective_config_fingerprint`, `scan_schema::read_effective_config`

**Uses other crates** (check the Rust module exists on `main`; otherwise stub behind a trait):
- `core:core-pack-01`: `filesystem`
- `core:crawl-settings`: `fingerprint`

**Oracle baselines to match:** scan.tables.json (resume_state, frontier, decisions)

### `storage-scan_schema` — wave W2, order 2, cycle group `scc-crawl-pack-01-store`, ~1402 reference lines

| reference | Rust file | stubs (`todo!`) | notes |
|---|---|---|---|
| the reference suite | `spiderhead/store/src/scan_schema.rs` | 16 | has reference module-level state/non-literal constants (comments `reference module state`) |

Total stubs: **16**.

**Tests to port (131 reference test functions in 8 files):** the reference suite (42), the reference suite (22), the reference suite (30), the reference suite (3), the reference suite (2), the reference suite (5), the reference suite (12), the reference suite (15)

**Calls into other store slices** (tests that need them: `#[ignore = "needs <slice>"]`):
- `crawl-pack-01-store`: `crawl_capture::REDACTED_NAMES_HEADER`
- `storage-__init__`: `scan_artifact::APPLICATION_ID`, `scan_artifact::FORMAT_VERSION`, `scan_artifact::MAX_RECORD_BYTES`, `scan_artifact::ScanError`, `scan_artifact::USER_VERSION`, `scan_artifact::config`, `scan_artifact::dump`, `scan_artifact::duplicate_ids`, `scan_artifact::expected`, `scan_artifact::has_negative_page_counts`, `scan_artifact::heading_outline`, `scan_artifact::insert`, `scan_artifact::link_placement`, `scan_artifact::objects` …
- `storage-pack-03`: `corpus::corpus_summary`, `corpus_validation::validate_corpus`, `credential_context::credential_verifier`, `credential_context::redact_config`, `credential_context::validate_recorded_credentials`
- `storage-pack-04`: `discovery_ledger::ensure_schema`, `events::ensure_schema`
- `storage-pack-05`: `history::regular`, `native_audit::validate_audit`
- `storage-pack-06`: `native_context::validate_context`, `native_scan::NativeScan`, `native_scan::reanalysis_capability`, `rendered_routes::validate_ledger`
- `storage-pack-07`: `resource_graph::ensure_schema`, `retention::policy_for_config`, `retention::validate_policy`, `retry::upgrade_to_v2`, `retry::v2_tables`, `retry::validate_v2`
- `storage-pack-08`: `sitemaps::declare`, `sitemaps::root_ids`
- `storage-pack-09`: `transport::ensure_schema`
- `storage-scan_lease`: `scan_lease::bounded_items`, `scan_lease::fsync_directory`, `scan_lease::fsync_file`, `scan_lease::utc`

**Uses other crates** (check the Rust module exists on `main`; otherwise stub behind a trait):
- `core:core-pack-01`: `filesystem`, `open_readonly`
- `core:crawl-settings`: `DEFAULTS`, `fingerprint`, `validate`

**Oracle baselines to match:** scan.tables.json (header/config columns)

### `store-asset-delivery` — wave W2, order 2, cycle group `scc-crawl-pack-01-store`, ~41 reference lines

| reference | Rust file | stubs (`todo!`) | notes |
|---|---|---|---|
| the reference suite | `spiderhead/store/src/asset_delivery.rs` | 1 | only symbols: resource_delivery_evidence |

Total stubs: **1**.

**Tests to port (42 reference test functions in 1 files):** the reference suite (42)

**Calls into other store slices** (tests that need them: `#[ignore = "needs <slice>"]`):
- `storage-pack-01-store`: `bodies::decode_entity`
- `storage-pack-07`: `resources::media_matches`

**Uses other crates** (check the Rust module exists on `main`; otherwise stub behind a trait):
- `core:recon-net`: `UA`, `http_client`
- `parse:checks-parser-pack-01`: `_rel_has`, `document_base_url`, `extract_resource_declarations`, `is_inert_template_content`

**Oracle baselines to match:** none directly (covered through its callers; compare with reference on `oracle/raw-scans/*`)

### `store-crawl-records` — wave W2, order 2, cycle group `scc-crawl-pack-01-store`, ~465 reference lines

| reference | Rust file | stubs (`todo!`) | notes |
|---|---|---|---|
| the reference suite | `spiderhead/store/src/crawl_records/content_evidence.rs` | 1 | only symbols: capture |
| the reference suite | `spiderhead/store/src/crawl_records/evidence.rs` | 2 | only symbols: _indexability, _same_page |
| the reference suite | `spiderhead/store/src/crawl_records/security_headers.rs` | 1 | only symbols: REDIRECT_HOP_PROBE_VERSION, validate_redirect_hop_probe |
| the reference suite | `spiderhead/store/src/crawl_records/spider.rs` | 10 | only symbols: LinkEdge, Scope, SegmentRule, _MEDIA_TYPE_TOKEN, _canonical_key, _strip_fragment |
| the reference suite | `spiderhead/store/src/crawl_records/sqlite_adapter.rs` | 1 | only symbols: _headers |
| the reference suite | `spiderhead/store/src/crawl_records/throttle.rs` | 0 | only symbols: RequestBudgetExhausted |

Total stubs: **15**.

**Tests to port (418 reference test functions in 26 files):** the reference suite (6), the reference suite (4), the reference suite (4), the reference suite (27), the reference suite (14), the reference suite (6), the reference suite (6), the reference suite (20), the reference suite (18), the reference suite (7), the reference suite (20), the reference suite (5), the reference suite (44), the reference suite (43), the reference suite (18), the reference suite (13), the reference suite (47), the reference suite (3), the reference suite (6), the reference suite (18), the reference suite (31), the reference suite (10), the reference suite (2), the reference suite (16), the reference suite (20), the reference suite (10)

**Calls into other store slices** (tests that need them: `#[ignore = "needs <slice>"]`):
- `crawl-pack-01-store`: `crawl_events::EventSink`
- `storage-__init__`: `scan_artifact::MAX_RECORD_BYTES`, `scan_artifact::ScanBackpressure`, `scan_artifact::ScanError`
- `storage-pack-01-store`: `bodies::read_document`
- `storage-pack-03`: `corpus::html_body_retention`
- `storage-pack-06`: `native_scan::NativeScan`, `rendered_routes::observations`
- `storage-pack-07`: `resource_graph::capture`
- `storage-pack-09`: `target_probes::capture`

**Uses other crates** (check the Rust module exists on `main`; otherwise stub behind a trait):
- `checks:checks-pack-05`: `blocking_rule`, `is_allowed`, `match_path`, `parse_robots`, `politeness_delay`
- `core:core-http-cache-control`: `ResponseCache`
- `core:core-pack-01`: `ParsedRobots`, `runlog`
- `core:core-url-variants`: `build_variants`
- `core:crawl-pack-01`: `same_robots_identity`
- `core:crawl-settings`: `DEFAULT_SEGMENT`, `checked_url_budget`, `fingerprint`, `request_headers`, `resolve_credential_headers`
- `core:recon-net`: `UA`, `crawl_transport_options`, `http_client`, `normalize_url`, `registrable_domain`
- `crawl:crawl-collect`: `CrawlResult`, `MAX_RESPONSE_BYTES`, `PageRecord`, `_apply_body`, `_resolve_redirect_destination`, `_write`, `fetch_one`
- `crawl:crawl-pack-01-crawl`: `host_bound_auth`, `state`
- `crawl:crawl-pack-02`: `ExternalCheck`, `ExternalPolicy`, `run_external_checks`
- `crawl:crawl-pack-04`: `capture_resources`
- `parse:checks-pack-01-parse`: `evaluate`
- `parse:checks-parser-pack-01`: `robots_directives`

**Oracle baselines to match:** crawl-site.json

### `storage-ledger-ingest` — wave W2, order 3, cycle group `scc-storage-ledger-ingest`, ~1815 reference lines

| reference | Rust file | stubs (`todo!`) | notes |
|---|---|---|---|
| the reference suite | `spiderhead/store/src/ledger/ingest.rs` | 32 | has reference module-level state/non-literal constants (comments `reference module state`) |

Total stubs: **32**.

**Tests to port (58 reference test functions in 4 files):** the reference suite (4), the reference suite (5), the reference suite (30), the reference suite (19)

**Calls into other store slices** (tests that need them: `#[ignore = "needs <slice>"]`):
- `sf-core-pack-01-store`: `evidence_contract::attach_contract`, `evidence_contract::attach_contract_parts`
- `storage-__init__`: `scan_artifact::dump`, `scan_artifact::loads`, `scan_artifact::open_scan`
- `storage-ledger-pack-01`: `ledger/schema::FINGERPRINT`, `ledger/schema::FORMAT_VERSION`, `ledger/schema::LIFECYCLE_STATES`, `ledger/schema::LedgerError`, `ledger/schema::MAX_PAYLOAD_BYTES`, `ledger/schema::MEASURED_OUTCOMES`, `ledger/schema::REVISION`, `ledger/schema::SHA256`, `ledger/schema::TRANSITIONS`, `ledger/schema::USER_VERSION`, `ledger/schema::apply_v2_schema`, `ledger/schema::apply_v3_schema`, `ledger/schema::apply_v4_schema`, `ledger/schema::apply_v5_schema` …
- `storage-pack-01-store`: `audit_v2::AuditV2Reader`

**Uses other crates** (check the Rust module exists on `main`; otherwise stub behind a trait):
- `cli:root-pack-01`: `__version__`
- `core:core-pack-01`: `file_sha256`, `fsync_directory`
- `core:core-project-format-consts`: `_target`, `open_project`
- `core:crawl-settings`: `fingerprint`
- `core:recon-net`: `normalize_domain`
- `formats:sf-core-pack-01`: `_Rows`

**Oracle baselines to match:** none directly (covered through its callers; compare with reference on `oracle/raw-scans/*`)

Ledger has no oracle baseline: build a ledger with the reference writer from `raw-scans/basic` and compare row dumps.

### `storage-ledger-pack-01` — wave W2, order 3, cycle group `scc-storage-ledger-ingest`, ~1521 reference lines

| reference | Rust file | stubs (`todo!`) | notes |
|---|---|---|---|
| the reference suite | `spiderhead/store/src/ledger/mod.rs` | 0 |  |
| the reference suite | `spiderhead/store/src/ledger/queries.rs` | 9 |  |
| the reference suite | `spiderhead/store/src/ledger/schema.rs` | 26 | has reference module-level state/non-literal constants (comments `reference module state`) |

Total stubs: **35**.

**Tests to port (73 reference test functions in 5 files):** the reference suite (4), the reference suite (15), the reference suite (5), the reference suite (30), the reference suite (19)

**Calls into other store slices** (tests that need them: `#[ignore = "needs <slice>"]`):
- `storage-__init__`: `scan_artifact::ScanError`, `scan_artifact::dump`, `scan_artifact::loads`, `scan_artifact::open_scan`
- `storage-ledger-ingest`: `ledger/ingest::DOC_MARKER`, `ledger/ingest::REPRESENTATIONS`, `ledger/ingest::SCOPE_SUBJECT`, `ledger/ingest::UTC`, `ledger/ingest::affected_urls`, `ledger/ingest::bind_source`, `ledger/ingest::check_id`, `ledger/ingest::coverage_state`, `ledger/ingest::create_ledger`, `ledger/ingest::finding_state`, `ledger/ingest::ingest_scan`, `ledger/ingest::ingest_scan_`, `ledger/ingest::insert_finding`, `ledger/ingest::insert_group` …

**Uses other crates** (check the Rust module exists on `main`; otherwise stub behind a trait):
- `core:core-norm-url`: `norm_url`
- `core:core-pack-01`: `file_sha256`, `open_readonly`

**Oracle baselines to match:** none directly (covered through its callers; compare with reference on `oracle/raw-scans/*`)

Ledger has no oracle baseline: build a ledger with the reference writer from `raw-scans/basic` and compare row dumps.

### `store-evidence-from-scan` — wave W2, order 3, cycle group `None`, ~8 reference lines

| reference | Rust file | stubs (`todo!`) | notes |
|---|---|---|---|
| the reference suite | `spiderhead/store/src/evidence_from_scan.rs` | 1 | only symbols: evidence_from_scan |

Total stubs: **1**.

**Tests to port (0 reference test functions in 0 files):** 

**Calls into other store slices** (tests that need them: `#[ignore = "needs <slice>"]`):
- `storage-pack-08`: `status::scan_status`

**Uses other crates** (check the Rust module exists on `main`; otherwise stub behind a trait):
- `core:crawl-settings`: `DEFAULTS`, `MAX_REQUESTS_CEILING`, `MAX_URLS_CEILING`, `validate`
- `core:storage-artifacts`: `SCAN_V1`, `SCAN_V2`

**Oracle baselines to match:** none directly (covered through its callers; compare with reference on `oracle/raw-scans/*`)

## Known risks

- `sql/scan_v1.sql` contains a vendor name inside a column comment. It must stay: SQLite keeps comments in `sqlite_master.sql` and the scan validator compares the object inventory text, so the DDL has to be byte-identical. Do not copy that comment into Rust docs.
- `ScanReader` checks only `application_id`/`user_version`; the full validation (object inventory, scalar storage checks, limitations) belongs to `storage-__init__` (`scan_artifact.rs`).
- reference module-level mutable state (caches, locks, validated-file LRU) is left as `// reference module state` comments; port it as `OnceLock`/`Mutex` statics or struct fields.
- Mixin classes (`NativeScanSchemaMixin`, `NativeScanLeaseMixin`, ...) are separate structs; `native_scan::NativeScan` must compose them (traits or one `impl` split across files).
- Generated `Result` wrapping is heuristic (raise or SQL/IO call in the body). Pure helpers that merely look fallible can drop it.

## Coordinator rule (27.10): key order

reference dicts keep insertion order, and that order reaches JSON outputs (scan exports, audit.v2, CLI/MCP). Wherever a skeleton signature uses `BTreeMap` for data that is serialised or iterated into an output, replace it with `indexmap::IndexMap` or `serde_json::Map` (workspace has `preserve_order`). `BTreeMap` stays only where reference itself sorts.
