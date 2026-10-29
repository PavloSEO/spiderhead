//! The offline audit engine: scan.v1 pages in, findings out.
//!
//! reference source: `spiderhead/sf/core` (rule engine, context) driven by
//! the reference suite. Rules are grouped in modules named after their
//! reference counterparts; the context records findings and skips exactly as `AuditContext` does.

mod aggregate;
mod context;
mod document;
mod load;
mod model;
mod orchestrate;
pub mod robots;
pub mod rules;

use serde_json::Value;

pub use context::{AuditContext, Thresholds};
pub use document::{assemble, audit_v2_parts, write_audit_v2, AuditDoc};
pub use load::{load_pages, scan_run, LoadError, ScanRun};
pub use model::{norm_url, Group, Issue, Page, SkippedCheck};
pub use orchestrate::{native_check_ids, skip_unsupported_set};

/// Fill run metadata and effective config from the stored scan (`hydrate`):
/// `ctx.start_url`, the run columns, `ctx.config` and the retained
/// `internationalization` block (`ctx.native_hreflang`).
fn hydrate(ctx: &mut AuditContext) -> Result<(), LoadError> {
    let con = ctx.scan.as_ref().ok_or(LoadError::Missing)?;
    let run = scan_run(con)?;
    ctx.start_url = Some(run.start_url.clone()).filter(|u| !u.is_empty());
    ctx.finish_reason = run.finish_reason.clone();
    ctx.crawl_partial = run.crawl_partial;
    ctx.crawl_resumed = run.crawl_resumed;
    ctx.cache_replay = run.cache_replay;
    ctx.cache_stats = run.cache_stats.clone();
    ctx.config = spiderhead_store::scan_schema::read_effective_config(con)
        .map_err(|e| LoadError::Store(e.to_string()))?;
    if let Some(corpus) = ctx.saved_corpus.as_ref().and_then(|v| v.as_object()) {
        if let Some(Value::Object(i18n)) = corpus.get("internationalization") {
            ctx.native_hreflang = Some(Value::Object(i18n.clone()));
            // The corpus owner's self_codes index: declared, active, valid
            // self-referencing rows only — the set the confirmation pass reads
            // when the derivation ran.
            let mut self_codes = std::collections::BTreeMap::new();
            for item in i18n
                .get("declarations")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let code = item
                    .get("lang")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_lowercase();
                let source = item
                    .get("source_identity")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let declared = item
                    .get("declaration_state")
                    .and_then(Value::as_str)
                    .is_some_and(|v| v == "declared")
                    && item
                        .get("source_representation_state")
                        .and_then(Value::as_str)
                        .is_some_and(|v| v == "active");
                if declared
                    && source
                        == item
                            .get("target_identity")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                    && !code.is_empty()
                    && code != "x-default"
                {
                    self_codes
                        .entry(source.to_owned())
                        .or_insert_with(std::collections::BTreeSet::new)
                        .insert(code);
                }
            }
            ctx.corpus_self_codes = Some(self_codes);
        }
    }
    Ok(())
}

/// Audit the scan at `path`: load its pages and run the complete reference
/// orchestration (`_audit_crawl_result`): `skip_unsupported`, `run_rules`,
/// `run_inlinks`, `run_eeat`, `run_heuristics`, retained sitemap, the
/// link/console/security/retained/fragment phases, then `aggregate`.
pub fn audit_scan(path: &std::path::Path) -> Result<AuditContext, LoadError> {
    let pages = load_pages(path)?;
    let mut ctx = AuditContext::new(pages);
    let con =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    // `AnalysisGraph` requires file-backed temporary storage (`_graph_access`).
    con.execute_batch("PRAGMA temp_store=FILE")?;
    ctx.scan = Some(con);
    ctx.saved_corpus = spiderhead_store::corpus_derivations::derive(
        ctx.scan.as_ref().ok_or(LoadError::Missing)?,
        0.92,
        false,
    )
    .ok()
    .map(serde_json::Value::Object);
    ctx.fields = context::NATIVE_FIELDS
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    hydrate(&mut ctx)?;
    // The retained renderer/navigation readers error without them.
    rules::install_render_checks();
    orchestrate::skip_unsupported(&mut ctx);
    orchestrate::run_rules(&mut ctx);
    // `ctx.graph_access` is set only for the inlinks/eeat phases and only when
    // `stored_graph_available` holds (non-list scan with a non-empty `links`).
    ctx.graph_access = ctx
        .scan
        .as_ref()
        .is_some_and(rules::inlinks_graph::stored_graph_available);
    orchestrate::run_inlinks(&mut ctx);
    rules::eeat::run_eeat(&mut ctx);
    ctx.graph_access = false;
    orchestrate::run_heuristics(&mut ctx);
    ctx.sitemap_summary = rules::sitemap::run_retained_sitemap(&mut ctx);
    ctx.link_rel = orchestrate::run_late(&mut ctx, path);
    ctx.fragment_links = orchestrate::run_fragment(&mut ctx);
    aggregate::aggregate(&mut ctx);
    Ok(ctx)
}

/// The full audit document for the stored scan: `aggregate` output finalized
/// (`run`/`summary`, coverage, withheld checks) and shaped as audit.v2 parts —
/// the JSON header plus the ordered collections — with the evidence contract and
/// saved-corpus derivations attached, exactly as the reference streaming handler.
pub fn audit_v2_document(path: &std::path::Path) -> Result<document::AuditParts, String> {
    let mut ctx = audit_scan(path).map_err(|e| e.to_string())?;
    document::assemble(&mut ctx, path)
}
