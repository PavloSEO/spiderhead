//! Offline sitemap/robots findings over cursor-backed retained scan evidence.
//!
//! (`rules_for`, `pattern_match`, `match_path`, `is_allowed`) are a local port
//! of the reference suite until the checks crate lands — they must
//! stay aligned with the oracle.

mod robots_policy;
mod run_robots;
mod scan_rows;

use rusqlite::OptionalExtension;
use serde_json::Value;
use spiderhead_parse::parser::meta::robots_directives;

use crate::context::disk::DiskError;
use crate::context::{AddOpts, AuditContext, ContextError};
use crate::normalize::{norm_url, Object};
use crate::pyvalue::{str_or_dumps, truthy, truthy_opt};

use robots_policy::blocked;
use run_robots::run_robots;

/// Status-conflict checks declared sitemap URLs can fire.
pub const STATUS_CHECKS: [&str; 5] = [
    "SITEMAP_URL_3XX",
    "SITEMAP_URL_4XX_5XX",
    "SITEMAP_URL_NON_INDEXABLE",
    "SITEMAP_URL_NON_CANONICAL",
    "SITEMAP_URL_BLOCKED_ROBOTS",
];

/// Coverage checks that need a complete reconciliation.
pub const COVERAGE_CHECKS: [&str; 3] = ["SITEMAP_ORPHAN", "URL_NOT_IN_SITEMAP", "SITEMAP_DESYNC"];

/// The caller-side sitemap reconciliation snapshot (the oracle's
/// `reconciliation`). Concrete implementations come from the native-scan
/// analysis layer; the contract mirrors the methods `run_retained_sitemap`
/// calls on it.
pub trait SitemapReconciliation {
    /// `reconciliation._names` — any declared sitemap names exist.
    fn has_names(&self) -> bool;
    /// `reconciliation.available`.
    fn available(&self) -> bool;
    /// `reconciliation.reason`.
    fn reason(&self) -> String;
    /// `reconciliation.crawl_partial`.
    fn crawl_partial(&self) -> bool;
    /// `reconciliation.counts` — e.g. `counts["urls_in_sitemap"]`.
    fn counts(&self) -> &Object;
    /// `reconciliation.summary()` — merged into the result summary.
    fn summary(&self) -> Object;
    /// `reconciliation.iter_declared_facts()` — one object per declared URL
    /// with `url`, `status_code`, `sitemap_url`, `blocked`, `redirect_url`,
    /// `meta_robots`, `x_robots`, `canonical`.
    fn iter_declared_facts(&self) -> Vec<Object>;
    /// `reconciliation.iter_bucket(name)` — the example rows of one bucket.
    fn iter_bucket(&self, name: &str) -> Vec<Value>;
    /// `reconciliation.count_bucket(name)`.
    fn count_bucket(&self, name: &str) -> i64;
    /// `reconciliation.iter_missing_canonical_pages()` — `url`, `status_code`.
    fn iter_missing_canonical_pages(&self) -> Vec<Object>;
    /// `reconciliation.iter_orphan_facts()` — `url`, `sitemap_url`, `status_code`.
    fn iter_orphan_facts(&self) -> Vec<Object>;
}

// -- retained robots policy ---------------------------------------------------

/// `_robots`: the retained robots policy payload, or `None` when the scan
/// holds no fetched policy.
fn robots(ctx: &AuditContext) -> Result<Option<Object>, ContextError> {
    let Some(con) = ctx.scan_con.as_ref() else {
        return Ok(None);
    };
    let raw: Option<String> = con
        .query_row(
            "SELECT payload_json FROM context_items WHERE kind='robots_summary' AND item_key='run'",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(DiskError::from)?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    let payload: Value = serde_json::from_str(&raw).map_err(DiskError::from)?;
    if payload.get("fetch_state").and_then(Value::as_str) == Some("fetched") {
        Ok(Some(payload.as_object().cloned().unwrap_or_default()))
    } else {
        Ok(None)
    }
}

// -- the retained run ---------------------------------------------------------

/// `round(x, 1)`: reference rounds half-to-even on the decimal representation —
/// Rust's `{:.1}` formatting does the same.
fn round1(x: f64) -> f64 {
    format!("{x:.1}").parse().unwrap_or_default()
}

/// `row.get(k) or ""` then `str()` — for the `meta_robots`/`x_robots` columns.
fn or_empty_str(row: &Object, key: &str) -> String {
    row.get(key)
        .filter(|v| truthy(v))
        .map(str_or_dumps)
        .unwrap_or_default()
}

/// `run_retained_sitemap`: emit full findings while retaining only twenty
/// examples per summary bucket.
pub fn run_retained_sitemap(
    ctx: &mut AuditContext,
    reconciliation: Option<&dyn SitemapReconciliation>,
    start_url: &str,
) -> Result<Object, ContextError> {
    if ctx.scan_con.is_none() {
        return Err(ContextError::Value(
            "retained sitemap checks require a saved scan".into(),
        ));
    }
    let robots = robots(ctx)?;
    let mut summary = Object::new();
    summary.insert("evidence_source".into(), Value::from("retained_scan"));
    summary.insert("offline".into(), Value::from(true));
    let roots: Vec<String> = if let Some(con) = ctx.scan_con.as_ref() {
        let mut stmt = con
            .prepare(
                "SELECT u.url FROM context_items c JOIN urls u \
                 ON u.url_id=json_extract(c.payload_json,'$.sitemap_url_id') \
                 WHERE c.kind='sitemap_declaration' ORDER BY json_extract(c.payload_json,'$.ordinal')",
            )
            .map_err(DiskError::from)?;
        let collected = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(DiskError::from)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(DiskError::from)?;
        collected
    } else {
        Vec::new()
    };
    summary.insert(
        "sitemap_url".into(),
        roots
            .first()
            .map(|r| Value::from(r.clone()))
            .unwrap_or(Value::Null),
    );
    summary.insert(
        "sitemap_urls".into(),
        Value::Array(roots.iter().map(|r| Value::from(r.clone())).collect()),
    );
    for check in [
        "SITEMAP_TOO_MANY_URLS",
        "SITEMAP_TOO_LARGE",
        "SITEMAP_URL_DUPLICATED",
        "SITEMAP_STALE_LASTMOD",
    ] {
        ctx.skip(
            check,
            "retained sitemap membership does not include protocol size or lastmod evidence",
        );
    }
    match &robots {
        None => ctx.skip(
            "SITEMAP_NOT_IN_ROBOTS",
            "no retained fetched robots.txt policy",
        ),
        Some(r) => {
            let sitemaps = r
                .get("parsed")
                .and_then(|p| p.get("sitemaps"))
                .cloned()
                .unwrap_or(Value::Null);
            if !truthy(&sitemaps) {
                let mut details = Object::new();
                details.insert(
                    "reason".into(),
                    Value::from("retained robots.txt declares no sitemap"),
                );
                details.insert("evidence_source".into(), Value::from("retained_scan"));
                ctx.add(
                    "SITEMAP_NOT_IN_ROBOTS",
                    AddOpts {
                        target_url: Some(start_url.to_owned()),
                        details,
                        ..AddOpts::default()
                    },
                )?;
            }
        }
    }
    run_robots(ctx, robots.as_ref(), start_url)?;
    if reconciliation.is_none_or(|r| !r.has_names()) {
        let reason = "no saved sitemap declarations";
        for check in STATUS_CHECKS.iter().chain(COVERAGE_CHECKS.iter()) {
            ctx.skip(check, reason);
        }
        ctx.skip("SITEMAP_FETCH_INCOMPLETE", reason);
        summary.insert("reconciliation_available".into(), Value::from(false));
        summary.insert("reconciliation_reason".into(), Value::from(reason));
        return Ok(summary);
    }
    let Some(recon) = reconciliation else {
        return Ok(summary);
    };

    let mut unmeasured = 0i64;
    for row in recon.iter_declared_facts() {
        let target = str_or_dumps(row.get("url").unwrap_or(&Value::Null));
        let status_raw = row.get("status_code").cloned().unwrap_or(Value::Null);
        let status = status_raw.as_i64();
        let mut details = Object::new();
        details.insert("in_sitemap".into(), Value::from(true));
        details.insert(
            "source_url".into(),
            row.get("sitemap_url").cloned().unwrap_or(Value::Null),
        );
        details.insert("target_url".into(), Value::from(target.clone()));
        details.insert("status_code".into(), status_raw.clone());
        details.insert("evidence_source".into(), Value::from("retained_scan"));
        let mut evidence = Object::new();
        evidence.insert(
            "sitemap_url".into(),
            row.get("sitemap_url").cloned().unwrap_or(Value::Null),
        );
        evidence.insert("retained".into(), Value::from(true));
        if truthy_opt(row.get("blocked")) || blocked(robots.as_ref(), &target, start_url) {
            let mut d = details.clone();
            d.insert(
                "reason".into(),
                Value::from("blocked by retained robots.txt policy"),
            );
            ctx.add(
                "SITEMAP_URL_BLOCKED_ROBOTS",
                AddOpts {
                    target_url: Some(target.clone()),
                    status_code: status,
                    details: d,
                    evidence: evidence.clone(),
                    ..AddOpts::default()
                },
            )?;
        }
        if status_raw.is_null() {
            unmeasured += 1;
            continue;
        }
        match status {
            Some(300..=399) => {
                let mut d = details.clone();
                d.insert("reason".into(), Value::from("redirect"));
                d.insert(
                    "redirect_url".into(),
                    row.get("redirect_url").cloned().unwrap_or(Value::Null),
                );
                ctx.add(
                    "SITEMAP_URL_3XX",
                    AddOpts {
                        target_url: Some(target.clone()),
                        status_code: status,
                        details: d,
                        evidence: evidence.clone(),
                        ..AddOpts::default()
                    },
                )?;
            }
            Some(400..=599) => {
                let mut d = details.clone();
                d.insert(
                    "reason".into(),
                    Value::from(if status.is_some_and(|s| s < 500) {
                        "client error"
                    } else {
                        "server error"
                    }),
                );
                ctx.add(
                    "SITEMAP_URL_4XX_5XX",
                    AddOpts {
                        target_url: Some(target.clone()),
                        status_code: status,
                        details: d,
                        evidence: evidence.clone(),
                        ..AddOpts::default()
                    },
                )?;
            }
            Some(200..=299) => {
                let meta = or_empty_str(&row, "meta_robots");
                let x_robots = or_empty_str(&row, "x_robots");
                let directives = robots_directives(&[Some(meta.as_str()), Some(x_robots.as_str())]);
                if directives.iter().any(|d| d == "noindex") {
                    let mut d = details.clone();
                    d.insert("reason".into(), Value::from("noindex"));
                    d.insert("indexability".into(), Value::from("Non-Indexable"));
                    ctx.add(
                        "SITEMAP_URL_NON_INDEXABLE",
                        AddOpts {
                            target_url: Some(target.clone()),
                            status_code: status,
                            details: d,
                            evidence: evidence.clone(),
                            ..AddOpts::default()
                        },
                    )?;
                }
                let canonical = row.get("canonical").cloned().unwrap_or(Value::Null);
                if truthy(&canonical)
                    && norm_url(Some(&target))? != norm_url(Some(&str_or_dumps(&canonical)))?
                {
                    let mut d = details.clone();
                    d.insert(
                        "reason".into(),
                        Value::from("canonical points to another URL"),
                    );
                    d.insert("canonical_url".into(), canonical);
                    ctx.add(
                        "SITEMAP_URL_NON_CANONICAL",
                        AddOpts {
                            target_url: Some(target.clone()),
                            status_code: status,
                            details: d,
                            evidence: evidence.clone(),
                            ..AddOpts::default()
                        },
                    )?;
                }
            }
            _ => {}
        }
    }
    summary.insert("sitemap_status_unmeasured".into(), Value::from(unmeasured));
    if unmeasured > 0 {
        let reason = format!("{unmeasured} declared sitemap URL(s) have no retained response");
        for check in &STATUS_CHECKS[..4] {
            ctx.skip(check, &reason);
        }
    }
    if robots.is_none() {
        ctx.skip(
            "SITEMAP_URL_BLOCKED_ROBOTS",
            "no retained fetched robots.txt policy",
        );
    }
    if !recon.available() {
        let mut details = Object::new();
        details.insert("reason".into(), Value::from(recon.reason()));
        details.insert("evidence_source".into(), Value::from("retained_scan"));
        ctx.add(
            "SITEMAP_FETCH_INCOMPLETE",
            AddOpts {
                target_url: Some(start_url.to_owned()),
                details,
                ..AddOpts::default()
            },
        )?;
        for check in COVERAGE_CHECKS {
            ctx.skip(check, &recon.reason());
        }
        summary.insert("reconciliation_available".into(), Value::from(false));
        summary.insert("reconciliation_reason".into(), Value::from(recon.reason()));
        return Ok(summary);
    }

    for (k, v) in recon.summary() {
        summary.insert(k, v);
    }
    summary.insert("reconciliation_available".into(), Value::from(true));
    for name in [
        "in_sitemap_and_linked",
        "in_sitemap_not_linked",
        "linked_not_in_sitemap",
        "linked_not_comparable",
    ] {
        let mut items = recon.iter_bucket(name);
        items.truncate(20);
        summary.insert(name.into(), Value::Array(items));
        summary.insert(
            format!("{name}_count"),
            Value::from(recon.count_bucket(name)),
        );
    }
    summary.insert("examples_limit".into(), Value::from(20));
    let mut missing = 0i64;
    for row in recon.iter_missing_canonical_pages() {
        let target = str_or_dumps(row.get("url").unwrap_or(&Value::Null));
        let status_raw = row.get("status_code").cloned().unwrap_or(Value::Null);
        if blocked(robots.as_ref(), &target, start_url) {
            continue;
        }
        missing += 1;
        let mut details = Object::new();
        details.insert(
            "source_url".into(),
            roots
                .first()
                .map(|r| Value::from(r.clone()))
                .unwrap_or(Value::Null),
        );
        details.insert("target_url".into(), Value::from(target.clone()));
        details.insert("status_code".into(), status_raw.clone());
        details.insert(
            "reason".into(),
            Value::from("canonical indexable HTML page absent from retained sitemap"),
        );
        details.insert("evidence_source".into(), Value::from("retained_scan"));
        ctx.add(
            "URL_NOT_IN_SITEMAP",
            AddOpts {
                target_url: Some(target),
                status_code: status_raw.as_i64(),
                details,
                ..AddOpts::default()
            },
        )?;
    }
    summary.insert(
        "canonical_pages_not_in_sitemap_count".into(),
        Value::from(missing),
    );
    // Incomplete graph evidence cannot prove that a page has no internal links.
    if recon.crawl_partial() {
        ctx.skip(
            "SITEMAP_ORPHAN",
            "crawl is partial: internal link absence cannot be proven",
        );
        ctx.skip(
            "SITEMAP_DESYNC",
            "crawl is partial: sitemap-versus-link-graph verdict cannot be proven",
        );
    } else {
        let mut orphaned = 0i64;
        let start_norm = norm_url(Some(start_url))?;
        for row in recon.iter_orphan_facts() {
            let target = str_or_dumps(row.get("url").unwrap_or(&Value::Null));
            if norm_url(Some(&target))? == start_norm {
                continue;
            }
            orphaned += 1;
            let status_raw = row.get("status_code").cloned().unwrap_or(Value::Null);
            let mut details = Object::new();
            details.insert("in_sitemap".into(), Value::from(true));
            details.insert(
                "source_url".into(),
                row.get("sitemap_url").cloned().unwrap_or(Value::Null),
            );
            details.insert("target_url".into(), Value::from(target.clone()));
            details.insert("status_code".into(), status_raw.clone());
            details.insert("reason".into(), Value::from("no retained internal link"));
            details.insert("evidence_source".into(), Value::from("retained_scan"));
            ctx.add(
                "SITEMAP_ORPHAN",
                AddOpts {
                    target_url: Some(target),
                    status_code: status_raw.as_i64(),
                    details,
                    ..AddOpts::default()
                },
            )?;
        }
        let linked_missing = summary
            .get("linked_not_in_sitemap_count")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        summary.insert(
            "sitemap_orphan_findings_count".into(),
            Value::from(orphaned),
        );
        let linked_total = summary
            .get("in_sitemap_and_linked_count")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            + linked_missing;
        let crawl_pct = round1(100.0 * linked_missing as f64 / linked_total.max(1) as f64);
        let sitemap_total = recon
            .counts()
            .get("urls_in_sitemap")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .max(1);
        let sitemap_pct = round1(100.0 * orphaned as f64 / sitemap_total as f64);
        let threshold = ctx
            .thresholds
            .get("sitemap_desync_pct_warn")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        if crawl_pct.max(sitemap_pct) >= threshold {
            let mut details = Object::new();
            details.insert(
                "in_crawl_not_in_sitemap".into(),
                Value::from(linked_missing),
            );
            details.insert("in_sitemap_not_in_crawl".into(), Value::from(orphaned));
            details.insert("crawl_not_in_sitemap_pct".into(), Value::from(crawl_pct));
            details.insert("sitemap_not_in_crawl_pct".into(), Value::from(sitemap_pct));
            details.insert(
                "examples_missing_from_sitemap".into(),
                summary
                    .get("linked_not_in_sitemap")
                    .cloned()
                    .unwrap_or(Value::Null),
            );
            details.insert(
                "examples_in_sitemap_not_crawled".into(),
                summary
                    .get("in_sitemap_not_linked")
                    .cloned()
                    .unwrap_or(Value::Null),
            );
            details.insert("evidence_source".into(), Value::from("retained_scan"));
            ctx.add(
                "SITEMAP_DESYNC",
                AddOpts {
                    target_url: Some(start_url.to_owned()),
                    details,
                    ..AddOpts::default()
                },
            )?;
        }
    }
    Ok(summary)
}
