//! The audit.v2 document: `run`/`summary` assembly (reference the reference suite
//! second half + `AuditResult.audit_v2_parts`), evidence-contract and saved-corpus
//! attachment, and writing through `store::audit_v2::write_audit_v2`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use indexmap::IndexMap;
use serde_json::{Map, Value};
use spiderhead_formats::check_registry;

use super::context::AuditContext;
use super::model::{Issue, SkippedCheck};

/// Checks withheld when the crawl did not reach every URL (`UNLINKED_FINDING_CHECKS`,
/// `GRAPH_WIDE_FINDING_CHECKS`, `DEPTH_FINDING_CHECKS`).
const UNLINKED: &[&str] = &[
    "ORPHAN_PAGE",
    "SITEMAP_ORPHAN",
    "UNLINKED_CANONICAL",
    "HREFLANG_UNLINKED_TARGET",
    "UNLINKED_PAGINATION_SERIES",
];

/// See `UNLINKED`.
const GRAPH_WIDE: &[&str] = &[
    "LOW_LINK_SCORE",
    "ONLY_NOFOLLOW_INLINKS",
    "ONLY_NONINDEXABLE_SOURCE_INLINKS",
    "DEEP_DISCOVERY_PATH",
];

/// See `UNLINKED`.
const DEPTH: &[&str] = &["DEEP_CLICK_DEPTH"];

/// `PARTIAL_DEPTH_REASON` (the reference suite).
const PARTIAL_DEPTH_REASON: &str = "crawl is partial: a click-depth verdict cannot be proven \
     when the crawl did not reach every URL, since the part never fetched may hold a shorter route";

/// `PARTIAL_CRAWL_RATIO`.
const PARTIAL_CRAWL_RATIO: f64 = 0.2;

/// `MIN_COVERAGE_TO_SCORE`.
const MIN_COVERAGE_TO_SCORE: f64 = 0.5;

/// `IMPLAUSIBLE_SHARE` and `IMAGE_TARGETED_CHECKS` (the reference suite).
const IMPLAUSIBLE_SHARE: f64 = 0.5;
const IMAGE_TARGETED: &[&str] = &[
    "IMG_OVER_KB",
    "IMG_MISSING_DIMENSIONS",
    "IMG_MISSING_ALT",
    "IMG_MISSING_ALT_ATTRIBUTE",
    "IMG_ALT_TOO_LONG",
    "IMG_BROKEN",
    "IMG_INTRINSIC_OVERSIZED",
    "IMG_UPSCALED",
];

/// `_crawl_validity`: a run with no corpus or no HTML page cannot be scored.
fn crawl_validity(n_pages: usize, no_response: bool, urls_crawled: usize) -> (bool, String) {
    if urls_crawled == 0 {
        return (false, "no URLs were crawled".to_owned());
    }
    if n_pages == 0 {
        return (
            false,
            if no_response {
                "the crawl got no response from the site".to_owned()
            } else {
                "no HTML pages were crawled".to_owned()
            },
        );
    }
    (true, String::new())
}

/// `_health_score`: `100 - penalty/pages*10` clamped to [0,100], `None` without pages.
fn health_score(
    by_severity: &BTreeMap<String, i64>,
    n_pages: usize,
    weights: &Map<String, Value>,
) -> Option<i64> {
    if n_pages == 0 {
        return None;
    }
    let penalty: f64 = weights
        .iter()
        .map(|(sev, w)| {
            by_severity.get(sev).copied().unwrap_or(0) as f64 * w.as_f64().unwrap_or(0.0)
        })
        .sum();
    let score = 100.0 - (penalty / n_pages as f64) * 10.0;
    Some(score.round().clamp(0.0, 100.0) as i64)
}

/// `_implausible_checks`: checks covering >50% of crawled pages, by distinct target page.
fn implausible_checks(issues: &[Issue], n_pages: usize) -> Vec<Value> {
    if n_pages == 0 {
        return Vec::new();
    }
    let mut pages_by_check: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for issue in issues {
        if IMAGE_TARGETED.contains(&issue.check.as_str()) {
            continue;
        }
        let mut targets = BTreeSet::new();
        if let Some(url) = &issue.target_url {
            targets.insert(url.clone());
        }
        for loc in &issue.locations {
            if let Some(u) = loc.get("url").and_then(Value::as_str) {
                targets.insert(u.to_owned());
            }
        }
        if !targets.is_empty() {
            pages_by_check
                .entry(&issue.check)
                .or_default()
                .extend(targets);
        }
    }
    let mut flagged: Vec<Value> = pages_by_check
        .into_iter()
        .filter(|(_, urls)| urls.len() as f64 / n_pages as f64 > IMPLAUSIBLE_SHARE)
        .map(|(check, urls)| {
            let mut row = Map::new();
            row.insert("check".into(), Value::from(check));
            row.insert("pages".into(), Value::from(urls.len() as i64));
            row.insert(
                "share".into(),
                Value::from((urls.len() as f64 / n_pages as f64 * 1000.0).round() / 1000.0),
            );
            Value::Object(row)
        })
        .collect();
    flagged.sort_by(|a, b| {
        let key = |v: &Value| {
            (
                -(v["share"].as_f64().unwrap_or(0.0) * 1e9) as i64,
                v["check"].as_str().unwrap_or("").to_owned(),
            )
        };
        key(a).cmp(&key(b))
    });
    flagged
}

/// The finished audit (`reference AuditResult`): `run` is mutated with the fields
/// aggregation itself owns (`crawl_valid`, `crawl_invalid_reason`, `crawl_partial`,
/// `requires_rendering`).
pub struct AuditDoc {
    /// `issues` after dedupe/sort/id/fingerprint assignment.
    pub issues: Vec<Issue>,
    /// Completed summary block.
    pub summary: Map<String, Value>,
    /// Suppressed findings (empty without a configured policy).
    pub suppressed_issues: Vec<Value>,
    /// `checks_skipped` entries, filtered to declared ids.
    pub skipped: Vec<SkippedCheck>,
    /// `checks_disabled` entries.
    pub disabled: Vec<SkippedCheck>,
}

/// The audit.v2 document pair: JSON header plus ordered collection rows.
pub type AuditParts = (Map<String, Value>, IndexMap<String, Vec<Value>>);

/// `aggregate()` second half: coverage, score, and every summary block the
/// orchestration fills. `run` must already carry `crawl_partial` (initial value),
/// `requires_rendering` and `requires_rendering_reason` as the handler set them.
pub fn finalize(ctx: &mut AuditContext, run: &mut Map<String, Value>) -> AuditDoc {
    let sitemap_summary = ctx.sitemap_summary.clone();
    let urls_crawled = ctx.pages.len();
    let n_pages = ctx.html_pages().len();
    let no_response = ctx.issues.iter().any(|i| i.check == "NO_RESPONSE");
    let (crawl_valid, invalid_reason) = crawl_validity(n_pages, no_response, urls_crawled);
    let urls_in_sitemap = sitemap_summary
        .get("urls_in_sitemap")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let sitemap_partial = urls_in_sitemap > 0
        && crawl_valid
        && (urls_crawled as f64) < urls_in_sitemap as f64 * PARTIAL_CRAWL_RATIO;
    let crawl_partial = run
        .get("crawl_partial")
        .and_then(Value::as_bool)
        .unwrap_or(ctx.crawl_partial)
        || sitemap_partial;
    if crawl_partial {
        let withhold = |checks: &[&str], reason: &str, ctx: &mut AuditContext| {
            let mut fired: Vec<String> = checks
                .iter()
                .filter(|c| ctx.issues.iter().any(|i| &i.check == *c) || !ctx.fired(c))
                .map(|c| (*c).to_owned())
                .collect();
            fired.sort();
            fired.dedup();
            for check in fired {
                if checks.contains(&check.as_str()) {
                    ctx.retract(&check, reason);
                }
            }
        };
        withhold(
            UNLINKED,
            "crawl is partial: an 'unlinked' finding cannot be proven when the crawl did not reach every URL",
            ctx,
        );
        withhold(
            GRAPH_WIDE,
            "crawl is partial: a whole-graph finding cannot be proven when the crawl did not reach every URL",
            ctx,
        );
        if !ctx
            .skipped
            .iter()
            .any(|s| s.id == "CANONICAL_HOMEPAGE_GROUP")
        {
            ctx.retract(
                "CANONICAL_HOMEPAGE_GROUP",
                "crawl is partial: a site-wide canonical pattern cannot be established from an incomplete page population",
            );
        }
        withhold(DEPTH, PARTIAL_DEPTH_REASON, ctx);
        // Re-aggregate after the withheld findings are gone.
        super::aggregate::aggregate(ctx);
    }

    // ids and backlinks were assigned by `aggregate(ctx)`; recount what survived.
    let mut by_severity: BTreeMap<String, i64> = BTreeMap::new();
    let mut by_check: BTreeMap<String, i64> = BTreeMap::new();
    let mut fired_ids: BTreeSet<&str> = BTreeSet::new();
    for issue in &ctx.issues {
        *by_severity.entry(issue.severity.clone()).or_default() += 1;
        *by_check.entry(issue.check.clone()).or_default() += 1;
        fired_ids.insert(issue.check.as_str());
    }
    let registry = check_registry::builtin().ok();
    let all_ids: BTreeSet<&str> = registry
        .map(|r| r.checks().iter().map(|c| c.id.as_str()).collect())
        .unwrap_or_default();
    let checks_total = all_ids.len() as i64;
    let declared_ids: BTreeSet<String> = ctx
        .skipped
        .iter()
        .map(|s| s.id.clone())
        .filter(|id| !fired_ids.contains(id.as_str()))
        .collect();
    let silent_ids: Vec<&str> = all_ids
        .iter()
        .filter(|id| !fired_ids.contains(*id) && !declared_ids.iter().any(|d| d == *id))
        .cloned()
        .collect();
    let checks_skipped_n = declared_ids.len() as i64;
    let checks_available = checks_total - checks_skipped_n;

    let weights = ctx
        .config
        .get("scoring")
        .and_then(|s| s.get("weights"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_else(|| {
            let mut w = Map::new();
            w.insert("critical".into(), Value::from(5));
            w.insert("warning".into(), Value::from(2));
            w.insert("notice".into(), Value::from(0.5));
            w.insert("opportunity".into(), Value::from(0.25));
            w
        });

    let mut summary = Map::new();
    let mut totals = Map::new();
    totals.insert("urls_crawled".into(), Value::from(urls_crawled as i64));
    totals.insert("html_pages".into(), Value::from(n_pages as i64));
    totals.insert(
        "html_indexable".into(),
        Value::from(ctx.indexable_html_pages().len() as i64),
    );
    totals.insert("issues_total".into(), Value::from(ctx.issues.len() as i64));
    totals.insert("groups_total".into(), Value::from(ctx.groups.len() as i64));
    let mut repr: BTreeMap<String, i64> = BTreeMap::new();
    for page in &ctx.pages {
        let key = page
            .metrics
            .get("representation")
            .and_then(Value::as_str)
            .unwrap_or("static");
        *repr.entry(key.to_owned()).or_default() += 1;
    }
    totals.insert(
        "pages_by_representation".into(),
        Value::Object(repr.into_iter().map(|(k, v)| (k, Value::from(v))).collect()),
    );
    summary.insert("totals".into(), Value::Object(totals));
    let mut sev_block = Map::new();
    for sev in ["critical", "warning", "notice"] {
        sev_block.insert(
            sev.into(),
            Value::from(by_severity.get(sev).copied().unwrap_or(0)),
        );
    }
    summary.insert("by_severity".into(), Value::Object(sev_block));
    let mut checks_sorted: Vec<(String, i64)> = by_check.into_iter().collect();
    checks_sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    summary.insert(
        "by_check".into(),
        Value::Object(
            checks_sorted
                .into_iter()
                .map(|(k, v)| (k, Value::from(v)))
                .collect(),
        ),
    );
    summary.insert(
        "implausible_checks".into(),
        Value::Array(implausible_checks(&ctx.issues, n_pages)),
    );
    summary.insert(
        "health_score".into(),
        health_score(&by_severity, n_pages, &weights).map_or(Value::Null, Value::from),
    );

    run.insert("crawl_valid".into(), Value::from(crawl_valid));
    run.insert(
        "crawl_invalid_reason".into(),
        if invalid_reason.is_empty() {
            Value::Null
        } else {
            Value::from(invalid_reason.clone())
        },
    );
    if !crawl_valid {
        summary.insert("health_score".into(), Value::Null);
        summary.insert(
            "health_score_reason".into(),
            Value::from(invalid_reason.clone()),
        );
    }
    let requires_rendering = run
        .get("requires_rendering")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    run.insert("requires_rendering".into(), Value::from(requires_rendering));
    if requires_rendering && !summary["health_score"].is_null() {
        summary.insert("health_score".into(), Value::Null);
        let reason = run
            .get("requires_rendering_reason")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("the run requires JavaScript rendering before it can be scored");
        summary.insert("health_score_reason".into(), Value::from(reason));
    }

    let coverage = if checks_total > 0 {
        Some(((checks_available as f64 / checks_total as f64) * 1000.0).round() / 1000.0)
    } else {
        None
    };
    let mut cov = Map::new();
    cov.insert("checks_total".into(), Value::from(checks_total));
    cov.insert("checks_fired".into(), Value::from(fired_ids.len() as i64));
    cov.insert("checks_skipped".into(), Value::from(checks_skipped_n));
    cov.insert("checks_disabled".into(), Value::from(0));
    cov.insert("checks_disabled_ids".into(), Value::Array(Vec::new()));
    cov.insert("checks_silent".into(), Value::from(silent_ids.len() as i64));
    cov.insert(
        "checks_silent_ids".into(),
        Value::Array(silent_ids.iter().map(|id| Value::from(*id)).collect()),
    );
    cov.insert("coverage".into(), coverage.map_or(Value::Null, Value::from));
    summary.insert("check_coverage".into(), Value::Object(cov));
    if coverage.unwrap_or(0.0) < MIN_COVERAGE_TO_SCORE && !summary["health_score"].is_null() {
        summary.insert("health_score".into(), Value::Null);
        summary.insert(
            "health_score_reason".into(),
            Value::from(format!(
                "only {checks_available} of {checks_total} checks could run ({:.0}% coverage); too little evidence to score",
                coverage.unwrap_or(0.0) * 100.0
            )),
        );
    }
    if checks_skipped_n > 0 {
        summary.insert(
            "health_score_basis".into(),
            Value::from(format!(
                "{checks_available} of {checks_total} checks could run; the score is not comparable to a run with full evidence"
            )),
        );
    }
    run.insert("crawl_partial".into(), Value::from(crawl_partial));
    if sitemap_partial {
        summary.insert(
            "health_score_scope".into(),
            Value::from(format!(
                "{urls_crawled} of {urls_in_sitemap} sitemap URLs crawled — the score describes the crawled subset, not the whole site"
            )),
        );
    }
    if !ctx.internal_linking.is_empty() {
        let mut internal = ctx.internal_linking.clone();
        if crawl_partial
            && internal
                .get("measured")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            let mut cd = Map::new();
            cd.insert("measured".into(), Value::from(false));
            cd.insert("reason".into(), Value::from(PARTIAL_DEPTH_REASON));
            internal.insert("click_depth".into(), Value::Object(cd));
        }
        summary.insert("internal_linking".into(), Value::Object(internal));
    }
    if !ctx.asset_measurements.is_empty() {
        summary.insert(
            "asset_measurements".into(),
            Value::Object(ctx.asset_measurements.clone().into_iter().collect()),
        );
    }
    if !ctx.trust_evidence.is_empty() {
        let mut trust = ctx.trust_evidence.clone();
        if crawl_partial {
            trust.insert(
                "crawl_scope".into(),
                Value::from(
                    "partial crawl: 'not_discovered' and 'not measured' states describe only the fetched set, not the whole site",
                ),
            );
        }
        summary.insert("trust_evidence".into(), Value::Object(trust));
    }
    if let Some(evidence) = &ctx.semantic_evidence {
        summary.insert("semantic_similarity".into(), evidence.clone());
    }
    if let Some(evidence) = &ctx.semantic_lexical_evidence {
        summary.insert("semantic_lexical".into(), evidence.clone());
    }
    if !sitemap_summary.is_empty() {
        summary.insert("sitemap".into(), Value::Object(sitemap_summary));
    }
    if !ctx.link_rel.is_empty() {
        summary.insert("link_rel".into(), Value::Object(ctx.link_rel.clone()));
    }
    if !ctx.fragment_links.is_empty() {
        // The streaming handler projects the evaluation down to its three
        // audit-facing keys.
        let mut fragment = Map::new();
        for key in ["analysis", "states", "coverage"] {
            if let Some(v) = ctx.fragment_links.get(key) {
                fragment.insert(key.into(), v.clone());
            }
        }
        summary.insert("fragment_links".into(), Value::Object(fragment));
    }

    let max_pages = ctx
        .config
        .get("output")
        .and_then(|o| o.get("max_pages_in_json"))
        .and_then(Value::as_i64)
        .unwrap_or(100000) as usize;
    ctx.pages.truncate(max_pages);

    let skipped: Vec<SkippedCheck> = ctx
        .skipped
        .iter()
        .filter(|s| declared_ids.contains(&s.id))
        .cloned()
        .collect();

    AuditDoc {
        issues: std::mem::take(&mut ctx.issues),
        summary,
        suppressed_issues: Vec::new(),
        skipped,
        disabled: Vec::new(),
    }
}

/// `AuditResult.audit_v2_parts`: a JSON header plus the ordered collections.
/// `link_rel` and `fragment_links` land in `summary` before this is called, as the
/// streaming handler does.
pub fn audit_v2_parts(
    run: &Map<String, Value>,
    summary: &Map<String, Value>,
    doc: &AuditDoc,
    ctx: &AuditContext,
    version: &str,
) -> (Map<String, Value>, IndexMap<String, Vec<Value>>) {
    let mut run = run.clone();
    run.insert(
        "checks_skipped".into(),
        Value::Array(
            doc.skipped
                .iter()
                .map(|s| {
                    let mut m = Map::new();
                    m.insert("id".into(), Value::from(s.id.clone()));
                    m.insert("reason".into(), Value::from(s.reason.clone()));
                    Value::Object(m)
                })
                .collect(),
        ),
    );
    run.insert("checks_disabled".into(), Value::Array(Vec::new()));
    let mut summary = summary.clone();
    if let Some(Value::Object(sitemap)) = summary.get("sitemap").cloned() {
        summary.insert("sitemap".into(), Value::Object(sitemap));
    }
    let mut header = Map::new();
    header.insert("schema_version".into(), Value::from("2.0"));
    let mut tool = Map::new();
    tool.insert("name".into(), Value::from("Export Analyzer"));
    tool.insert("version".into(), Value::from(version));
    tool.insert("generated_by".into(), Value::from("SEOHead"));
    header.insert("tool".into(), Value::Object(tool));
    header.insert("run".into(), Value::Object(run));
    header.insert("summary".into(), Value::Object(summary.clone()));
    header.insert("issues".into(), Value::Array(Vec::new()));
    header.insert("pages".into(), Value::Array(Vec::new()));
    header.insert("groups".into(), Value::Array(Vec::new()));

    let mut collections: IndexMap<String, Vec<Value>> = IndexMap::new();
    collections.insert(
        "/issues".into(),
        doc.issues
            .iter()
            .map(|i| Value::Object(i.to_json()))
            .collect(),
    );
    collections.insert(
        "/pages".into(),
        ctx.pages
            .iter()
            .map(|p| Value::Object(p.to_json()))
            .collect(),
    );
    collections.insert(
        "/groups".into(),
        ctx.groups
            .iter()
            .map(|g| Value::Object(g.to_json()))
            .collect(),
    );
    if !doc.suppressed_issues.is_empty() {
        header.insert("suppressed_issues".into(), Value::Array(Vec::new()));
        collections.insert("/suppressed_issues".into(), doc.suppressed_issues.clone());
    }
    if let Some(Value::Object(sitemap)) = summary.get("sitemap").cloned() {
        for (name, values) in sitemap {
            if let Value::Array(rows) = values {
                let pointer = format!(
                    "/summary/sitemap/{}",
                    name.replace('~', "~0").replace('/', "~1")
                );
                set_pointer(&mut header, &pointer, Value::Array(Vec::new()));
                collections.insert(pointer, rows);
            }
        }
    }
    (header, collections)
}

/// `models._set_collection`/`audit_v2.set_pointer` on the in-memory header: create
/// intermediate objects for the pointer path and assign the leaf.
fn set_pointer(root: &mut Map<String, Value>, pointer: &str, value: Value) {
    let parts: Vec<String> = pointer[1..]
        .split('/')
        .map(|p| p.replace("~1", "/").replace("~0", "~"))
        .collect();
    let mut current = root;
    for part in &parts[..parts.len() - 1] {
        current = current
            .entry(part.clone())
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .expect("pointer part is an object");
    }
    current.insert(parts[parts.len() - 1].clone(), value);
}

/// The `run` block the native audit handler assembles for a stored scan
/// (`_audit_crawl_result`): the scan row supplies identity and lifecycle, the
/// retained effective config supplies `crawl_config` and the resolved rate.
fn build_run(
    ctx: &AuditContext,
    scan: &super::load::ScanRun,
    con: &rusqlite::Connection,
) -> Result<Map<String, Value>, String> {
    let created_at: String = con
        .query_row("SELECT created_at FROM scan WHERE singleton=1", [], |r| {
            r.get(0)
        })
        .unwrap_or_default();
    let effective = Value::Object(ctx.config.clone());
    let crawl_config =
        spiderhead_core::crawl_settings::manifest(&effective, &spiderhead_core::host::SystemHost)
            .map_err(|e| e.to_string())?;
    let rate = spiderhead_core::crawl_settings::effective_request_rate(&effective);
    let mut run = Map::new();
    run.insert("cache_replay".into(), Value::from(ctx.cache_replay));
    run.insert("cache_stats".into(), Value::Object(ctx.cache_stats.clone()));
    run.insert("collector".into(), Value::from("spiderhead.crawl"));
    run.insert(
        "config_fingerprint".into(),
        Value::from(scan.config_fingerprint.clone()),
    );
    run.insert("corpus_partial".into(), Value::from(scan.corpus_partial));
    run.insert(
        "crawl_config".into(),
        Value::Object(crawl_config.into_iter().collect()),
    );
    run.insert(
        "crawl_finish_reason".into(),
        Value::from(scan.finish_reason.clone()),
    );
    run.insert("crawl_invalid_reason".into(), Value::Null);
    run.insert("crawl_partial".into(), Value::from(ctx.crawl_partial));
    run.insert("crawl_resumed".into(), Value::from(ctx.crawl_resumed));
    run.insert("crawl_stopped_reason".into(), Value::from(""));
    run.insert(
        "effective_max_requests_per_second".into(),
        if rate.is_infinite() {
            Value::from("unbounded")
        } else {
            Value::from(rate)
        },
    );
    run.insert(
        "external_checks".into(),
        scan.external_checks.clone().unwrap_or(Value::Null),
    );
    run.insert("generated_at".into(), Value::from(created_at));
    run.insert(
        "input_mode".into(),
        Value::from(if scan.list_mode { "list" } else { "crawl" }),
    );
    run.insert("render_escalation".into(), Value::Null);
    run.insert("requires_rendering".into(), Value::from(false));
    run.insert("requires_rendering_reason".into(), Value::from(""));
    run.insert("scan_uuid".into(), Value::from(scan.scan_uuid.clone()));
    run.insert(
        "source".into(),
        if scan.start_url.is_empty() {
            Value::Null
        } else {
            Value::from(scan.start_url.clone())
        },
    );
    run.insert("source_kind".into(), Value::from(scan.source_kind.clone()));
    Ok(run)
}

/// The streaming handler's complete assembly: `run` + `finalize` +
/// `audit_v2_parts` + `attach_contract_parts` + `attach_saved_corpus_header`,
/// then `write_audit_v2`. Returns `(header, collections)` — the caller decides
/// whether the companion sqlite is also written.
pub fn assemble(ctx: &mut AuditContext, _scan_path: &Path) -> Result<AuditParts, String> {
    let con = ctx.scan.take().ok_or("no scan connection")?;
    let result = assemble_inner(ctx, &con);
    ctx.scan = Some(con);
    result
}

/// `assemble` over the borrowed scan connection (`ctx.scan` is parked outside).
fn assemble_inner(
    ctx: &mut AuditContext,
    con: &rusqlite::Connection,
) -> Result<AuditParts, String> {
    let scan = super::load::scan_run(con).map_err(|e| e.to_string())?;
    let writer_version: String = con
        .query_row(
            "SELECT writer_version FROM scan WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let mut run = build_run(ctx, &scan, con)?;
    let doc = finalize(ctx, &mut run);
    let (mut header, mut collections) =
        audit_v2_parts(&run, &doc.summary.clone(), &doc, ctx, &writer_version);
    // `_segment_counts` only exists when the operator declared segments.
    header.insert("segments".into(), Value::Object(Map::new()));
    let issues = collections.swap_remove("/issues").unwrap_or_default();
    let issue_rows = spiderhead_formats::models::Rows::new(move || {
        let rows = issues.clone();
        Box::new(rows.into_iter())
    });
    let (header, annotated) = spiderhead_store::evidence_contract::attach_contract_parts(
        &header,
        &issue_rows,
        Some(&scan.scan_uuid),
        Some(con),
    )
    .map_err(|e| e.to_string())?;
    let mut lifted: Vec<(String, spiderhead_formats::models::Rows)> = Vec::new();
    let derived: Option<&Map<String, Value>> = ctx.saved_corpus.as_ref().and_then(Value::as_object);
    let header = spiderhead_store::evidence_contract::attach_saved_corpus_header(
        &header,
        con,
        derived,
        Some(&mut lifted),
        &spiderhead_store::corpus_derivations::DefaultSources,
    )
    .map_err(|e| e.to_string())?;
    let annotated: Vec<Value> = annotated.iter().collect();
    collections.insert("/issues".into(), annotated);
    for (pointer, rows) in lifted {
        collections.insert(pointer, rows.iter().collect());
    }
    Ok((header, collections))
}

/// Run the whole audit over a stored native scan and write `<scan>.audit-v2.sqlite`.
/// Returns the companion path. `version` is the analyzer version string (the scan's
/// `writer_version`, as `bind_to_scan` validates).
pub fn write_audit_v2(
    scan_path: &Path,
    header: &Map<String, Value>,
    collections: &IndexMap<String, Vec<Value>>,
) -> Result<std::path::PathBuf, String> {
    let con = rusqlite::Connection::open_with_flags(
        scan_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| e.to_string())?;
    let (uuid, revision, writer_version, writer_revision) = con
        .query_row(
            "SELECT scan_uuid, evidence_revision, writer_version, writer_revision FROM scan WHERE singleton=1",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)),
        )
        .map_err(|e| e.to_string())?;
    drop(con);
    let mut binding = Map::new();
    binding.insert("scan_uuid".into(), Value::from(uuid));
    binding.insert("evidence_revision".into(), Value::from(revision));
    binding.insert("analyzer_version".into(), Value::from(writer_version));
    binding.insert("analyzer_revision".into(), Value::from(writer_revision));
    spiderhead_store::audit_v2::write_audit_v2(scan_path, header, collections, &binding)
        .map_err(|e| e.to_string())
}
