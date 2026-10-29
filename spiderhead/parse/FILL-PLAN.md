# FILL-PLAN — crate `spiderhead-parse`

Skeleton state: everything compiles, `cargo xtask ci` and `cargo xtask layers` are green, bodies are
`todo!("slice <id>")`. A filler greps `slice: <id>` / `todo!("slice <id>")` to find its work, touches
only its own files, and removes nothing else. `lib.rs` carries `#![allow(unused_variables,
dead_code)]` for the skeleton; the **last slice to merge deletes that line** and fixes the fallout.

Rules (CLAUDE.md protocol): each reference test function -> a Rust test with `// from tests/<file>the reference suite::<test>`;
no `unwrap`/`expect` outside tests; files <= ~600 lines (split private submodules when needed);
reference wins over the SF spec on disagreement. reference path prefix: `spiderhead/checks/`; line numbers
are in the `///` docs. Tests that need a function of another slice that is still `todo!` are written
now and marked `#[ignore = "needs <slice>"]`, then listed in the report; whoever merges last un-ignores.

Stub counts: checks-pack-01-parse 31, checks-pack-02-parse 19, checks-parser-pack-01 50,
checks-parser-pack-02 12 (+2 in `urlutil.rs` counted inside), parse-asset-evidence 7, checks-schema_org 6.

Test-ownership rule: a reference test file is ported by the slice that owns the module under test.
End-to-end tests that go through `parse_html` belong to `checks-parser-pack-01`.

## Merge / dependency order

1. `checks-parser-pack-02` (pure helpers, leaf) and `parse-asset-evidence`, `checks-pack-02-parse::{readability,navigation,markup}` and `checks-pack-01-parse::{duplicate,extraction_rules,boilerplate_report,content_area,link_position}` have no unmerged dependency except `shared::*` (pack-02) — all can be filled in parallel.
2. `checks-pack-01-parse::{markdown_extract,custom_*}` need `content_area` (same slice, same filler).
3. `checks-pack-02-parse::text_normalize` needs `content_area` (pack-01-parse).
4. `checks-parser-pack-01` needs everything above (`shared`, `tokenize`, `content_area`, `link_position`, `markup_facts`, `reading_ease`, `declared_language`, `find_render_blocking_resources`, `urlutil`). It may call them while they are `todo!`; its end-to-end tests stay `#[ignore]` until they merge.
5. `link_context` (pack-01-parse) needs `parser::extract::{link_target_reason,link_info}` (parser-pack-01).
6. `checks-schema_org` needs `parser::meta::{live_jsonld_tags}`, `tokenize::document_base_url`, `urlutil`.

## checks-pack-01-parse (31 stubs, 9 files)

| File | reference | Work |
|---|---|---|
| `content_area.rs` | the reference suite 83-180 | `find_content_root`, `resolve_content_area` (detached stripped copy: build a new `Html` from the selected subtree minus excluded tags/selectors), `extract_area_text`, `ResolvedArea::root`. Invalid include/root selector => fall back exactly as reference. |
| `link_position.rs` | the reference suite 54-150 | default rules, `rules_from_config`, `matched_position` (selector.matches on each ancestor, skip invalid), `content_or_other`, `classify_link` |
| `duplicate.rs` | the reference suite 71-454 | FNV-1a/SimHash/LSH exactly; SHA-1; private band/template/clique helpers; budgets -> `BudgetExceeded` |
| `boilerplate_report.rs` | the reference suite | hash over header/nav/footer markup minus `<template>`; serialisation of the regions must equal bs4's `str(tag)` (attribute quoting/escaping — fixture-test it) |
| `markdown_extract.rs` | the reference suite | own converter over `ElementRef`, uses `content_area` |
| `extraction_rules.rs` | the reference suite | validation messages verbatim; closed glob with step cap; JSON pointer; uses CSS selectors on `scraper` |
| `custom_extract.rs` | the reference suite | css/xpath/regex; XPath crate choice in PARSER-NOTES; budget |
| `custom_search.rs` | the reference suite | 8 scopes, `_visible_text` uses `content_area::TEXT_EXCLUDED_TAGS` |
| `link_context.rs` | the reference suite 30-201 | needs parser extract helpers (step 5) |

Tests (own modules): test_boilerplate_report, test_content_area, test_custom_extract, test_custom_search,
test_duplicate, test_extract_suggest (extraction parts), test_extraction_presets, test_findings_noise,
test_link_position, test_markdown_boilerplate_wiring, test_markdown_extract, test_retained_shop_duplicates,
test_scan_content_search, test_scan_corpus_inputs, test_scan_link_context, test_soft404,
test_batch_extraction_results, test_link_rel_checks, test_scan_crawl_extraction (extraction rules only).
Tests that drive `parse_html` or the store: `#[ignore = "needs checks-parser-pack-01"]` / skip with reason.

## checks-pack-02-parse (19 stubs, 4 files)

| File | reference | Work |
|---|---|---|
| `readability.rs` | the reference suite 53-91 | regexes with Unicode classes; banker's rounding |
| `text_normalize.rs` | the reference suite 54-289 | NFC, Cc/Cf/Cs removal, casefold, whitespace; `declared_language` reads `<html lang>`; manifest helpers over JSON; needs `content_area` for `normalize_document` |
| `markup.rs` | the reference suite 31-403 | raw tag scanner for AMP facts + a11y facts over the tree; `excluded`/`name_text`/`aria_named` are `pub(crate)` for parser `content.rs` |
| `navigation.rs` | the reference suite 9-313 | validators verbatim (error strings are contract); `NavigationCapture` is a pure state machine fed by `CaptureSignal`; Playwright glue is NOT here (render crate) |

Tests: test_markup, test_markup_evidence, test_native_readability, test_native_render_atomic,
test_navigation, test_navigation_handlers, test_render_navigation_live (only the pure validator parts;
live-browser parts skipped with reason), test_scan_analysis_parity, test_semantic_inputs.

## checks-parser-pack-01 (50 stubs, `parser/{mod,meta,content,extract}.rs`)

| File | reference | Work |
|---|---|---|
| `parser/meta.rs` | the reference suite 29-483 | head readers; `extract_jsonld` needs reference-style `json` error text |
| `parser/content.rs` | the reference suite 30-675 | headings, outline (hidden-subtree rules), placement, images, plugins, lorem, trust signals, duplicate ids |
| `parser/extract.rs` | the reference suite 30-580 | links (observations, caps, invalid targets), forms, text, URL sources, resource declarations, frames (document-order indexes instead of the `data-seohead-frame` mutation) |
| `parser/mod.rs` | the reference suite 194-557 | `parse_html` orchestration (option gating, result fields exactly as reference incl. `None`/`""` defaults when an option is off), `fetch_html`/`parse_url` over the `HtmlFetcher` seam (mock in tests; network never touched) |

`ParsedPage` is built field by field; keep the reference defaults of each disabled branch.
Tests (large): the 56 files in the slice's `tests` list that exercise `parse_html`; many need
pack-02 (`document_base_url`, `document_position`) and `content_area` — write them, `#[ignore]` per
rule until merged. reference-only tests (mocking `http_client`, BeautifulSoup internals, benchmark
`test_js_bridge_benchmark`, `test_platform_workflows`) are skipped with a reason in the report.

## checks-parser-pack-02 (12 stubs: `parser/shared.rs`, `parser/tokenize.rs`, `urlutil.rs`)

| File | reference | Work |
|---|---|---|
| `urlutil.rs` | stdlib `urllib.parse` | `urljoin`/`hostname` with reference quirks (consider moving to core later; keep API) |
| `parser/shared.rs` | the reference suite | is_external, ajax scheme, has_ancestor, inert-template test, rel_has, eq_ci, default headers (values listed in the reference suite 28-40) |
| `parser/tokenize.rs` | the reference suite 30-301 | tolerant raw tokenizer (RCDATA/rawtext opaque) for live `<base>` and head-element scans; `document_position` from tree + tokens |

Tests: test_ajax_crawling_scheme, test_base_href, test_double_escaped_entities, test_element_position,
test_head_link_targets, test_parser_charset_directives_schemes, test_user_agent_canon (UA constant only;
`test_platform_workflows` skipped, reference-only).

## parse-asset-evidence (7 stubs, `asset_evidence.rs`)

reference the reference suite symbols: `whitespace_ratio`, `looks_minified`, `is_render_blocking`,
`find_render_blocking_resources`, `find_missing_font_display`, `looks_legacy_transpiled`,
`check_cache_lifetime` (+ private regexes `_MAX_AGE_RE`, `_FONT_FACE_RE`, `_FONT_DISPLAY_OK_RE`,
`_LEGACY_JS_RE`). Calls `parser::shared::{is_inert_template_content, rel_has}` and `urlutil::urljoin`
(pack-02). Tests: the pure parts of the reference suite (fetcher/analyzer tests belong to
the `checks` crate and are skipped with a reason).

## checks-schema_org (6 stubs, `schema_org/mod.rs`)

Public API only: `load_vocab`, `rich_results_rules`, `type_chain`, `normalize_jsonld_ids`,
`json_syntax_hint`, `check_schema`. Data files are already embedded from `data/` (verbatim copies of
the oracle's `spiderhead/data/{schemaorg,rich_results_rules}.json`; rules sha256 `775a9b3d...b261`).
Private machinery (`_flatten`, `_check_node`, `_extract_microdata` 295-360, `_extract_rdfa` 382-884,
`_rich_results`, `_graph_shape`, `_findings`) goes into new private sibling files (<=600 lines each).
Tests: test_schema_org, test_schema_html_markup, test_schema_build, test_jsonld_invalid,
test_fetch_error_reporting (use a mock `HtmlFetcher`). Depends on `parser::meta::live_jsonld_tags`,
`parser::tokenize::document_base_url`.

## Open points for the coordinator

* `urljoin`/`hostname` live in `parse::urlutil`; if another W2 crate needs them, promote to `core::url_normalize`.
* XPath crate choice (PARSER-NOTES) is a one-line workspace dep decision for the pack-01-parse filler.
* `ParsedPage` mirrors the reference the reference suite; the robots types (`RobotsGroup`, ...) are in `models.rs` too because that file is one reference module; if `core-pack-01` already ported them, drop the duplicates when merging.
