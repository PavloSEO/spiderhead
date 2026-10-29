//! The stored-scan audit orchestration (the reference suite
//! `_audit_crawl_result`): `skip_unsupported`, `run_rules`, `run_inlinks`,
//! `run_eeat`, `run_heuristics`, retained sitemap, link/form predicates,
//! console/security/redirect-hop evidence, retained-link findings, fragment
//! links, and the `run`/`summary` metadata block.

use std::collections::HashSet;
use std::path::Path;

use rusqlite::OptionalExtension;
use serde_json::{Map, Value};
use spiderhead_formats::check_registry;

use super::context::AuditContext;
use super::rules;

/// `NATIVE_RESOLVED_CHECKS` (the reference suite) — measured from the stored scan.
const NATIVE_RESOLVED: &[&str] = &[
    "BODY_MIME_MISMATCH",
    "IMG_OVER_KB",
    "IMG_MISSING_ALT",
    "IMG_MISSING_DIMENSIONS",
    "HREFLANG_ERROR",
    "SITEMAP_URL_4XX_5XX",
    "SITEMAP_URL_3XX",
    "SITEMAP_URL_NON_INDEXABLE",
];

/// `mobile_checks.CHECKS`.
const MOBILE_RENDER: &[&str] = &[
    "MOBILE_CONTENT_OVERFLOW",
    "MOBILE_TARGET_TOO_SMALL",
    "MOBILE_TEXT_TOO_SMALL",
];

/// `render_checks.CHECKS`.
const RENDER_ELEMENT: &[&str] = &[
    "CANONICAL_MISMATCH",
    "CANONICAL_ONLY_IN_RENDERED_HTML",
    "H1_ONLY_IN_RENDERED_HTML",
    "H1_UPDATED_BY_JAVASCRIPT",
    "META_DESCRIPTION_ONLY_IN_RENDERED_HTML",
    "META_DESCRIPTION_UPDATED_BY_JAVASCRIPT",
    "NOFOLLOW_ONLY_IN_RAW_HTML",
    "NOINDEX_ONLY_IN_RAW_HTML",
    "PAGE_TITLE_ONLY_IN_RENDERED_HTML",
    "PAGE_TITLE_UPDATED_BY_JAVASCRIPT",
    "RENDER_RESOURCE_BLOCKED",
];

/// `retained_link_findings.CHECK_IDS`.
const RETAINED: &[&str] = &[
    "BROKEN_INTERNAL_LINK",
    "LINK_TO_5XX",
    "INTERNAL_LINK_TO_REDIRECT",
    "REDIRECT_CHAIN",
    "REDIRECT_LOOP",
    "INTERNAL_LINK_NO_ANCHOR",
    "EXTERNAL_NO_RESPONSE",
];

const HREFLANG_NONE: &str = "the native crawl retained no hreflang declarations";

/// The exported frame names available to `skip_unsupported` on a stored native
/// scan (`ctx.available_exports`).
fn available_exports(ctx: &AuditContext) -> HashSet<String> {
    let mut available: HashSet<String> = ["internal_all", "all_inlinks"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let declarations = ctx
        .native_hreflang
        .as_ref()
        .and_then(|v| v.get("declarations"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if declarations > 0 {
        available.insert("all_hreflang".to_owned());
    }
    available
}

/// `native_check_ids`: checks a stored scan measures itself.
pub fn native_check_ids(ctx: &AuditContext) -> HashSet<String> {
    let mut ids: HashSet<String> = HashSet::new();
    if has_column(ctx, "structured_data_parsed") {
        ids.insert("STRUCTURED_DATA_MISSING".to_owned());
    }
    let Some(con) = ctx.scan.as_ref() else {
        return ids;
    };
    ids.extend(
        NATIVE_RESOLVED
            .iter()
            .chain(MOBILE_RENDER)
            .chain(RENDER_ELEMENT)
            .chain(RETAINED)
            .copied()
            .map(str::to_owned),
    );
    let has_table = |name: &str| -> bool {
        con.query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .ok()
        .flatten()
        .is_some()
    };
    if has_table("responses") {
        ids.insert("MISSING_HSTS".to_owned());
    }
    let has_resources = has_table("resource_graph_occurrences")
        && con
            .query_row(
                "SELECT 1 FROM resource_graph_occurrences LIMIT 1",
                [],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .ok()
            .flatten()
            .is_some();
    if has_resources {
        ids.insert("MIXED_CONTENT".to_owned());
    }
    ids
}

/// `ctx.skip_unsupported(available, native)`: skip every check whose declared
/// export frame is absent, in catalogue order; a check in `native` is
/// measured from the stored scan itself and is not declared.
pub fn skip_unsupported_set(
    ctx: &mut AuditContext,
    available: &HashSet<String>,
    native: &HashSet<String>,
) {
    let Ok(registry) = check_registry::builtin() else {
        return;
    };
    let available_refs: HashSet<&str> = available.iter().map(String::as_str).collect();
    for requirement in registry.requires() {
        let check_id = requirement.id.as_str();
        if native.contains(check_id) {
            continue;
        }
        if ctx.scan.is_some()
            && !registry
                .missing_requirements(check_id, &available_refs)
                .is_empty()
        {
            ctx.skip(check_id, "not measured by this native crawl");
        }
    }
}

/// `ctx.skip_unsupported(available, native)` on a stored scan: the run's own
/// frame set and native resolution decide availability.
pub fn skip_unsupported(ctx: &mut AuditContext) {
    let available = available_exports(ctx);
    let native = native_check_ids(ctx);
    skip_unsupported_set(ctx, &available, &native);
}

/// `_has_column` reduced to native evidence: the field is measured iff some page
/// record carries a non-empty value (blank columns never prove a check ran).
fn has_column(ctx: &AuditContext, field: &str) -> bool {
    ctx.pages.iter().any(|p| {
        p.rec
            .get(field)
            .is_some_and(|v| !(v.is_null() || v.as_str().is_some_and(str::is_empty)))
    })
}

/// `ctx.skip` for each `(check, reason)` pair — the stub for a rule family that
/// cannot produce evidence on this scan yet (ported in the `gen/` pass).
fn stub(ctx: &mut AuditContext, checks: &[(&str, &str)]) {
    for (check, reason) in checks {
        ctx.skip(check, reason);
    }
}

/// `run_rules` (the reference suite): `ALL_CHECKS` in catalogue order. Ported
/// checks run their real implementation; unported families emit the same skips
/// the reference baseline emits on a stored native scan.
pub fn run_rules(ctx: &mut AuditContext) {
    // 0. check_lighthouse_reports — supplied per-URL Lighthouse JSON, when any.
    rules::asset_checks::check_lighthouse_reports(ctx);
    // 1. check_render_assets, 2. check_resource_assets (with page weight), 3. check_network_payload.
    rules::asset_checks::check_render_assets(ctx);
    rules::asset_checks::check_resource_assets(ctx);
    rules::asset_checks::check_network_payload(ctx);
    // 4. check_document_markup (ported).
    rules::markup::check_document_markup(ctx);
    // 5. check_body_mime.
    rules::native::check_body_mime(ctx);
    // 6. check_native_structured_and_mixed (ported).
    rules::content::check_structured_data_missing(ctx);
    rules::content::check_mixed_content(ctx);
    // 7. check_response_codes (ported).
    rules::indexation::check_response_codes(ctx);
    // 8. check_render_blocking.
    rules::render_blocking::check_render_blocking(ctx);
    // 9. check_indexability (ported).
    rules::indexation::check_indexability(ctx);
    // 10. check_redirect_type (ported).
    rules::indexation::check_redirect_type(ctx);
    // 11-13. titles/descriptions/headings (ported).
    rules::content_meta::check_titles(ctx);
    stub(
        ctx,
        &[
            (
                "TITLE_PIXELS_OVER",
                "no title_px column in Internal:All (SF-derived pixel-width column; native crawls leave it unmeasured)",
            ),
            (
                "TITLE_PIXELS_BELOW",
                "no title_px column in Internal:All (SF-derived pixel-width column; native crawls leave it unmeasured)",
            ),
        ],
    );
    rules::content_meta::check_descriptions(ctx);
    stub(
        ctx,
        &[
            (
                "DESC_PIXELS_OVER",
                "no desc_px column in Internal:All (SF-derived pixel-width column; native crawls leave it unmeasured)",
            ),
            (
                "DESC_PIXELS_BELOW",
                "no desc_px column in Internal:All (SF-derived pixel-width column; native crawls leave it unmeasured)",
            ),
        ],
    );
    rules::content_meta::check_headings(ctx);
    // 14. check_heading_outline; the h2 requirement is off in every stored run.
    stub(
        ctx,
        &[(
            "H2_MISSING",
            "requirements.require_h2 is false; the check was not evaluated",
        )],
    );
    rules::content_meta::check_heading_outline(ctx);
    // 15-16. invalid link targets + link placement (both ported).
    rules::links::check_invalid_link_targets(ctx);
    rules::links::check_link_placement(ctx);
    // 17. check_canonical_directives (ported).
    rules::indexation::check_canonical_directives(ctx);
    // 18. check_canonical_policy (ported).
    rules::indexation::check_canonical_policy(ctx);
    // 19-20. content + url/perf (both ported).
    rules::content::check_content(ctx);
    rules::content::check_url_and_perf(ctx);
    // 21. check_schema (ported).
    rules::content_meta::check_schema(ctx);
    // 22. check_duplicates (ported).
    rules::content::check_duplicates(ctx);
    // 23. check_url_extra (ported).
    rules::content::check_url_extra(ctx);
    // 24. check_content_quality (ported).
    rules::content::check_content_quality(ctx);
    stub(
        ctx,
        &[
            (
                "SPELLING_ERRORS",
                "no Spelling Errors column (enable spell-check in SF)",
            ),
            (
                "GRAMMAR_ERRORS",
                "no Grammar Errors column (enable grammar-check in SF)",
            ),
        ],
    );
    // 25. check_directives_extra (ported).
    rules::indexation::check_directives_extra(ctx);
    // 26. check_canonical_extra (ported).
    rules::indexation::check_canonical_extra(ctx);
    // 27. check_canonical_annotations (ported).
    rules::indexation::check_canonical_annotations(ctx);
    // 28. check_canonical_chain (ported).
    rules::indexation::check_canonical_chain(ctx);
    // 29-32. canonical target checks (ported).
    rules::indexation::check_canonical_to_redirect(ctx);
    rules::indexation::check_canonical_target_error(ctx);
    rules::indexation::check_canonical_homepage_group(ctx);
    rules::indexation::check_unlinked_canonical(ctx);
    // 33-35. pagination (ported); the head/anchor comparison lands later, in
    // `run_inlinks` (`check_pagination_declarations`).
    rules::pagination::check_pagination(ctx);
    rules::pagination::check_pagination_series(ctx);
    rules::pagination::check_pagination_sequence(ctx);
    // 36. check_links_extra (ported).
    rules::links::check_links_extra(ctx);
    // 37. check_tech_extra (ported).
    rules::technical::check_tech_extra(ctx);
    // 38. check_amp_pairing (ported).
    rules::technical::check_amp_pairing(ctx);
    // 39-41. native page/image evidence (ported).
    rules::native::check_native_page_evidence(ctx);
    rules::native::check_native_image_resources(ctx);
    rules::native::check_native_image_size(ctx);
    // 42. check_native_script_redirects.
    rules::native::check_native_script_redirects(ctx);
    // 43. mobile render checks.
    rules::mobile_checks::check_mobile_render_layout(ctx);
    // 44. rendered-element checks.
    rules::render_checks::check_rendered_elements(ctx);
    // 45. native export aggregates (ported).
    rules::native::check_native_export_aggregates(ctx);
    // 46. check_ajax_crawling_scheme (ported).
    rules::indexation::check_ajax_crawling_scheme(ctx);
    // 47-54. charset/doctype/viewport/mobile-alternate/compression/
    // element-position/document-skeleton/html-structure (all ported).
    rules::technical::check_charset(ctx);
    rules::technical::check_doctype(ctx);
    rules::technical::check_viewport(ctx);
    rules::technical::check_mobile_alternate(ctx);
    rules::technical::check_compression(ctx);
    rules::technical::check_element_position(ctx);
    rules::technical::check_document_skeleton(ctx);
    rules::technical::check_html_structure(ctx);
    // 55. check_url_hygiene (ported).
    rules::content::check_url_hygiene(ctx);
    // 56. check_placeholder_markers.
    rules::content::check_placeholder_markers(ctx);
    // 57. check_og (ported): no OG columns on a native scan → the same named
    // skip the export-fallback used to stamp.
    rules::content_meta::check_og(ctx);
    // 58. TITLE_MULTIPLE is emitted by the SF issue-map and native export
    // aggregates only — no per-page rule evaluates it on this path.
    stub(
        ctx,
        &[("TITLE_MULTIPLE", "not measured by this native crawl")],
    );
    // 58b. check_redirect_chains (ported export path; a stored scan returns early).
    rules::indexation::check_redirect_chains(ctx);
}

/// `run_inlinks` (the reference suite): export-shaped skips first, then the
/// graph passes over the stored `AnalysisGraph`.
pub fn run_inlinks(ctx: &mut AuditContext) {
    // `_process_export` on a stored scan only classifies retained external
    // outcomes — the internal side is emitted by the retained occurrence pass.
    for external in ["BROKEN_EXTERNAL_LINK", "EXTERNAL_LINK_TO_REDIRECT"] {
        ctx.skip(
            external,
            "not measured by this native check: external response status aggregates are not projected",
        );
    }
    rules::inlinks_graph::check_anchor_text(ctx);
    // check_hreflang_targets / unlinked_targets / noindex_targets.
    rules::hreflang::check_hreflang_targets(ctx);
    // `check_hreflang_unlinked_targets` is export-only: a stored scan never
    // carries the all_inlinks Inlinks column it reads.
    let no_declarations = ctx
        .native_hreflang
        .as_ref()
        .and_then(|v| v.get("declarations"))
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty);
    if no_declarations {
        ctx.skip("HREFLANG_UNLINKED_TARGET", HREFLANG_NONE);
    } else {
        ctx.skip(
            "HREFLANG_UNLINKED_TARGET",
            "not measured by this native crawl",
        );
    }
    rules::hreflang::check_hreflang_noindex(ctx);
    // check_hreflang_quality + reciprocity (ported).
    rules::hreflang::check_hreflang(ctx);
    // check_hreflang_confirmation_consistency.
    rules::hreflang::check_hreflang_consistency(ctx);
    rules::inlinks_graph::check_link_score(ctx);
    rules::inlinks_graph::check_inlink_composition(ctx);
    rules::inlinks_graph::check_discovery_path(ctx);
    // check_insecure_subresources.
    rules::inlinks_graph::check_insecure_subresources(ctx);
    // check_pagination_declarations (ported; the stored branch always defers
    // PAGINATION_URL_NOT_IN_ANCHOR — the head/anchor comparison is unmeasured).
    rules::pagination::check_pagination_declarations(ctx);
    // check_internal_link_graph is last: it opens its own path session.
    rules::inlinks_graph::check_internal_link_graph(ctx);
}

/// `run_heuristics` — html weight, DOM, duplication, semantic, templated titles.
pub fn run_heuristics(ctx: &mut AuditContext) {
    rules::heuristics::check_html_weight(ctx);
    rules::heuristics::check_dom(ctx);
    rules::heuristics::check_content_duplication(ctx);
    rules::semantic::check_semantic_content(ctx);
    rules::semantic::check_semantic_lexical(ctx);
    rules::heuristics::check_templated_titles(ctx);
}

/// The link/form predicate block of `_audit_crawl_result`, then console,
/// security headers, the redirect-hop probe, retained findings, and fragment
/// evaluation.
pub fn run_late(ctx: &mut AuditContext, scan_path: &Path) -> Map<String, Value> {
    // The `link_position` block: stored-graph
    // composition feeds INLINK_BOILERPLATE_ONLY; a partial crawl withholds the
    // "never linked from body content" verdict.
    rules::link_composition::check_link_composition(ctx);

    // The stored link/form predicate block of `_audit_crawl_result`: localhost
    // outlinks, robots-blocked targets, nofollow predicates, insecure forms and
    // the `link_attributes.capture`-gated rel checks.
    let link_rel = rules::link_predicates::run_link_predicates(ctx);

    // JS console errors from retained render sidecars.
    if let Some(con) = ctx.scan.take() {
        match spiderhead_store::browser_artifacts::console_error_pages(&con, scan_path) {
            Ok(console) => {
                let unreadable = console
                    .get("unreadable")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let captured = console.get("captured").and_then(Value::as_i64).unwrap_or(0);
                let pages = console
                    .get("pages")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                ctx.scan = Some(con);
                if unreadable > 0 && pages.is_empty() {
                    ctx.skip(
                        "JS_CONSOLE_ERRORS",
                        &format!(
                            "{unreadable} retained browser console record(s) could not be read"
                        ),
                    );
                } else if captured == 0 {
                    ctx.skip(
                        "JS_CONSOLE_ERRORS",
                        "no browser console was retained; enable rendering.artifacts.console_errors",
                    );
                }
                for item in pages {
                    let target = item
                        .get("target_url")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    let mut d = Map::new();
                    if let Some(details_obj) = item.as_object() {
                        for (k, v) in details_obj {
                            if k != "target_url" {
                                d.insert(k.clone(), v.clone());
                            }
                        }
                    }
                    ctx.add("JS_CONSOLE_ERRORS", target.as_deref(), d);
                }
            }
            Err(_) => {
                ctx.scan = Some(con);
                ctx.skip(
                    "JS_CONSOLE_ERRORS",
                    "no browser console was retained; enable rendering.artifacts.console_errors",
                );
            }
        }
    }
    // Security headers from the retained per-URL responses (ported).
    rules::stored::check_security_headers(ctx);
    // The opt-in redirect-hop HSTS probe: a stored audit re-judges the retained
    // probe payload instead of probing the site again.
    rules::stored::check_redirect_hop_hsts(ctx);
    // Retained link findings + the external-phase coverage statement.
    rules::retained_links::check_retained_links(ctx);
    if let Some(con) = ctx.scan.as_ref() {
        let has_external = con
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='external_checks'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .ok()
            .flatten()
            .is_some();
        let measurable = has_external
            && con
                .query_row(
                    "SELECT 1 FROM external_checks WHERE outcome IN ('fetched','failed') LIMIT 1",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .optional()
                .ok()
                .flatten()
                .is_some();
        if !has_external {
            ctx.skip(
                "EXTERNAL_NO_RESPONSE",
                "native external destination checks were not captured; enable discovery.external.crawl",
            );
        } else if !measurable {
            ctx.skip(
                "EXTERNAL_NO_RESPONSE",
                "captured external phase attempted no measurable destinations; its summary records skipped or blocked targets",
            );
        }
    }
    link_rel
}

/// `fragment_links.evaluate` + `BROKEN_BOOKMARK` emission; the evaluation itself
/// becomes `summary.fragment_links`.
pub fn run_fragment(ctx: &mut AuditContext) -> Map<String, Value> {
    let Some(con) = ctx.scan.take() else {
        ctx.skip(
            "BROKEN_BOOKMARK",
            "input retains no HTML/DOM bodies; fragment targets are measured only from a native retained scan",
        );
        return Map::new();
    };
    let body_limit: i64 = ctx
        .config
        .get("storage")
        .and_then(|s| s.get("max_body_bytes"))
        .and_then(Value::as_i64)
        .filter(|n| *n > 0)
        .unwrap_or(spiderhead_store::fragment_links::DEFAULT_MAX_DECODED_BYTES);
    let evaluation = match spiderhead_store::fragment_links::evaluate(&con, body_limit, 100_000) {
        Ok(ev) => ev,
        Err(_) => {
            ctx.scan = Some(con);
            return Map::new();
        }
    };
    ctx.scan = Some(con);
    let evaluated = evaluation
        .get("coverage")
        .and_then(|c| c.get("source_documents_evaluated"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if evaluated > 0 {
        let skipped = evaluation
            .get("states")
            .and_then(|s| s.get("skipped"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let coverage_state = evaluation
            .get("coverage")
            .and_then(|c| c.get("state"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let items = spiderhead_store::fragment_links::findings(&evaluation);
        let no_findings = items.is_empty();
        for item in items {
            let target = item
                .get("target_url")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let mut d = Map::new();
            for key in [
                "fragment",
                "decoded_fragment",
                "destination_representation",
                "locations_omitted",
            ] {
                if let Some(v) = item.get(key) {
                    d.insert(key.into(), v.clone());
                }
            }
            d.insert("occurrences_skipped".into(), Value::from(skipped));
            d.insert("coverage".into(), Value::from(coverage_state.clone()));
            let locations = item
                .get("locations")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let occurrences = item
                .get("occurrences_count")
                .and_then(Value::as_i64)
                .unwrap_or(1);
            ctx.add_located(
                "BROKEN_BOOKMARK",
                target.as_deref().unwrap_or(""),
                None,
                occurrences,
                locations,
                d,
            );
        }
        if no_findings && coverage_state != "complete" {
            ctx.skip(
                "BROKEN_BOOKMARK",
                "fragment evidence is partial; summary.fragment_links names each \
                 skipped occurrence and unavailable source",
            );
        }
    } else {
        ctx.skip(
            "BROKEN_BOOKMARK",
            "scan retains no complete HTML document for fragment resolution",
        );
    }
    evaluation
}
