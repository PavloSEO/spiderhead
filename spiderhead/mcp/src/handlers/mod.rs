//! Shared command handlers: one implementation per command, called by the CLI
//! surface and by this server's `tools/call`.
//!
//! Every handler takes the command's keyword arguments as one JSON object and
//! answers the handler's result document; a handler-level failure (the reference
//! `ValueError` at this boundary) comes back as [`ToolError::Failed`], while a
//! handler that reports `ok: false` in the document is an `Ok` — the CLI turns
//! that into exit code 1 and the MCP surface returns it as `structuredContent`,
//! exactly like the reference two surfaces do over the same handler layer.

pub mod audit;
pub mod bi;
pub mod checks;
pub mod compare;
pub mod content;
pub mod control;
pub mod crawl;
pub mod crawl_ops;
pub mod crawl_resume;
pub mod logs;
pub mod net;
pub mod parse;
pub mod project_ops;
pub mod projects;
pub mod reports;
pub mod scan;
pub mod scan_content;
pub mod scan_evidence;
pub mod scan_headers;
pub mod scan_links;
pub mod scan_ops;
pub mod semantics;
pub mod sf;
pub mod sources;
pub mod sources_cloudflare;
pub mod sources_dfs;
pub mod sources_evidence;
pub mod sources_metrika;
pub mod sources_net;
pub mod sources_providers;
pub mod sources_traffic;
pub mod sources_yandex;
pub mod update;
pub mod verification;
pub mod workflow;

use serde_json::{Map, Value};

use crate::error::ToolError;

/// The handler for one command: kwargs object in, result document out.
pub type HandlerFn = fn(&Value) -> Result<Value, ToolError>;

/// The wired handlers keyed by reference handler name (`scan_status`, `parse`, ...).
pub fn registry() -> Vec<(&'static str, HandlerFn)> {
    vec![
        ("scan_status", scan::scan_status),
        ("scan_list", scan::scan_list),
        ("scan_reanalyze", scan::scan_reanalyze),
        ("scan_url_query", scan::scan_url_query),
        ("scan_url_counts", scan::scan_url_counts),
        ("scan_graph", scan::scan_graph),
        ("scan_directory_tree", scan::scan_directory_tree),
        ("scan_inspect", scan::scan_inspect),
        ("scan_body_read", scan::scan_body_read),
        ("scan_export", scan::scan_export),
        ("scan_findings_page", scan::scan_findings_page),
        ("duplicate_check", scan::duplicate_check),
        ("scan_rendered_routes", scan_ops::scan_rendered_routes),
        ("scan_snapshot", scan_ops::scan_snapshot),
        ("scan_pin", scan_ops::scan_pin),
        ("scan_prune", scan_ops::scan_prune),
        ("scan_config_diff", scan_ops::scan_config_diff),
        ("scan_body_diff", scan_ops::scan_body_diff),
        ("scan_navigation", scan_ops::scan_navigation),
        ("extraction_presets", scan_ops::extraction_presets),
        ("custom_extract_sample", scan_ops::custom_extract_sample),
        ("scan_archive", scan_ops::scan_archive),
        ("scan_sitemap_generate", scan_ops::scan_sitemap_generate),
        ("scan_requeue", scan_ops::scan_requeue),
        ("scan_import_urls", scan_ops::scan_import_urls),
        ("scan_fragment_links", scan_ops::scan_fragment_links),
        ("scan_link_inspect", scan_links::scan_link_inspect),
        ("scan_url_detail", scan_ops::scan_url_detail),
        ("scan_url_history", scan_ops::scan_url_history),
        ("headers_analyze", scan_headers::headers_analyze),
        ("scan_evidence", scan_evidence::scan_evidence),
        ("scan_extract", scan_evidence::scan_extract),
        ("scan_content_search", scan_content::scan_content_search),
        (
            "scan_content_search_page",
            scan_content::scan_content_search_page,
        ),
        ("crawl_site", crawl::crawl_site),
        ("sitemap_crawl", crawl::sitemap_crawl),
        ("cdn_check", checks::cdn_check),
        ("headers_check", checks::headers_check),
        ("tech_detect", checks::tech_detect),
        ("security_check", checks::security_check),
        ("regions_check", checks::regions_check),
        ("ai_bots_check", checks::ai_bots_check),
        ("robots_check", checks::robots_check),
        ("redirects_check", checks::redirects_check),
        ("links_check", checks::links_check),
        ("hreflang_check", checks::hreflang_check),
        ("domain_profile", checks::domain_profile),
        ("llms_txt_check", checks::llms_txt_check),
        ("site_audit", audit::site_audit),
        ("sf_audit_run", sf::sf_audit_run),
        ("sf_audit_summary", sf::sf_audit_summary),
        ("sf_audit_issues", sf::sf_audit_issues),
        ("sf_list_exports", sf::sf_list_exports),
        ("sf_audit_tasks", sf::sf_audit_tasks),
        ("sf_snippet_edits", sf::sf_snippet_edits),
        ("segment_diff", audit::segment_diff),
        ("verify_fixes", verification::verify_fixes),
        ("compare_crawls", compare::compare_crawls),
        ("compare_findings", compare::compare_findings),
        ("compare_read", compare::compare_read),
        ("compare_status", compare::compare_status),
        ("compare_report", compare::compare_report),
        ("compare_url", compare::compare_url),
        ("crawl_import", compare::crawl_import),
        ("crawl_describe_settings", reports::crawl_describe_settings),
        ("report_build", reports::report_build),
        ("facts_export", reports::facts_export),
        ("project_new", projects::project_new),
        ("project_open", projects::project_open),
        ("project_status", projects::project_status),
        ("project_update", projects::project_update),
        ("project_delete", projects::project_delete),
        ("project_sources_link", projects::project_sources_link),
        ("project_sources_unlink", projects::project_sources_unlink),
        ("project_sources_list", projects::project_sources_list),
        ("project_action_log", projects::project_action_log),
        ("project_inbox_submit", projects::project_inbox_submit),
        ("project_inbox_list", projects::project_inbox_list),
        ("project_inbox_read", projects::project_inbox_read),
        (
            "project_inbox_acknowledge",
            projects::project_inbox_acknowledge,
        ),
        ("project_inbox_goal", projects::project_inbox_goal),
        ("project_inbox_triage", projects::project_inbox_triage),
        ("project_inbox_unread", projects::project_inbox_unread),
        ("project_export", projects::project_export),
        ("project_export_status", projects::project_export_status),
        ("project_export_cancel", projects::project_export_cancel),
        ("project_exports_page", projects::project_exports_page),
        ("monitor_status", workflow::monitor_status),
        ("monitor_configure", workflow::monitor_configure),
        ("monitor_run", workflow::monitor_run),
        ("monitor_collect", workflow::monitor_collect),
        ("monitor_local_deliver", workflow::monitor_local_deliver),
        ("monitor_schedule", workflow::monitor_schedule),
        ("remediation_summary", workflow::remediation_summary),
        ("remediation_create", workflow::remediation_create),
        ("remediation_ingest", workflow::remediation_ingest),
        ("remediation_cases", workflow::remediation_cases),
        ("remediation_transition", workflow::remediation_transition),
        (
            "remediation_record_verification",
            workflow::remediation_record_verification,
        ),
        ("remediation_recheck", workflow::remediation_recheck),
        ("remediation_report", workflow::remediation_report),
        ("redirects_generate", content::redirects_generate),
        ("markdown_extract", content::markdown_extract),
        ("boilerplate_report", content::boilerplate_report),
        ("schema_check", content::schema_check),
        ("citability_check", content::citability_check),
        ("social_meta_check", content::social_meta_check),
        ("doctor", control::doctor),
        ("update_check", update::update_check),
        ("update_download", update::update_download),
        ("tool_catalog", control::tool_catalog),
        ("skill_list", control::skill_list),
        ("skill_show", control::skill_show),
        ("scenario_list", control::scenario_list),
        ("scenario_show", control::scenario_show),
        ("workflow_start", control::workflow_start),
        ("workflow_checkpoint", control::workflow_checkpoint),
        ("workflow_status", control::workflow_status),
        ("workflow_execute", control::workflow_execute),
        ("workflow_resume", control::workflow_resume),
        ("audit_workflow", control::audit_workflow),
        ("bi_filter", bi::bi_filter),
        ("bi_export", bi::bi_export),
        ("bi_sheets_plan", bi::bi_sheets_plan),
        ("bi_bigquery_plan", bi::bi_bigquery_plan),
        ("bi_destination_apply", bi::bi_destination_apply),
        ("publication_cohorts", bi::publication_cohorts),
        ("gsc_progress", bi::gsc_progress),
        ("project_settings", project_ops::project_settings),
        (
            "project_settings_propose",
            project_ops::project_settings_propose,
        ),
        (
            "project_settings_proposals",
            project_ops::project_settings_proposals,
        ),
        (
            "project_settings_review",
            project_ops::project_settings_review,
        ),
        (
            "project_agent_heartbeat",
            project_ops::project_agent_heartbeat,
        ),
        ("project_progress", project_ops::project_progress),
        ("project_event_append", project_ops::project_event_append),
        ("project_event_page", project_ops::project_event_page),
        ("project_observe", project_ops::project_observe),
        ("project_activity", project_ops::project_activity),
        (
            "project_checklist_page",
            project_ops::project_checklist_page,
        ),
        ("project_task_detail", project_ops::project_task_detail),
        ("project_scans", project_ops::project_scans),
        ("project_facts", project_ops::project_facts),
        (
            "project_checklist_init",
            project_ops::project_checklist_init,
        ),
        (
            "project_checklist_update",
            project_ops::project_checklist_update,
        ),
        (
            "project_checklist_record",
            project_ops::project_checklist_record,
        ),
        ("project_task_comment", project_ops::project_task_comment),
        ("project_priorities", project_ops::project_priorities),
        ("project_policy", project_ops::project_policy),
        ("project_view_list", project_ops::project_view_list),
        ("project_view_show", project_ops::project_view_show),
        ("project_view_save", project_ops::project_view_save),
        ("project_view_delete", project_ops::project_view_delete),
        ("project_view_rename", project_ops::project_view_rename),
        ("findings_view", project_ops::findings_view),
        (
            "project_competitors_add",
            project_ops::project_competitors_add,
        ),
        (
            "project_competitors_list",
            project_ops::project_competitors_list,
        ),
        (
            "project_competitors_remove",
            project_ops::project_competitors_remove,
        ),
        ("project_archive", project_ops::project_archive),
        ("project_restore", project_ops::project_restore),
        ("project_archive_list", project_ops::project_archive_list),
        ("schedule_show", project_ops::schedule_show),
        ("schedule_run_due", project_ops::schedule_run_due),
        ("schedule_set", project_ops::schedule_set),
        ("schedule_install", project_ops::schedule_install),
        ("schedule_uninstall", project_ops::schedule_uninstall),
        ("crawl_control", crawl_ops::crawl_control),
        ("crawl_enrich", crawl_ops::crawl_enrich),
        ("crawl_diagnose", crawl_ops::crawl_diagnose),
        ("crawl_diagnose_export", crawl_ops::crawl_diagnose_export),
        ("project_prepare", crawl_ops::project_prepare),
        ("project_start", crawl_ops::project_start),
        (
            "project_competitors_scan",
            crawl_ops::project_competitors_scan,
        ),
        (
            "project_competitors_scan_status",
            crawl_ops::project_competitors_scan_status,
        ),
        (
            "project_competitors_scan_cancel",
            crawl_ops::project_competitors_scan_cancel,
        ),
        ("log_analyze", logs::log_analyze),
        ("logs_import", logs::logs_import),
        ("logs_summary", logs::logs_summary),
        ("logs_bots", logs::logs_bots),
        ("logs_join", logs::logs_join),
        ("parse", parse::parse),
        ("semantics_status", semantics::semantics_status),
        ("semantics_run", semantics::semantics_run),
        ("semantics_paid", semantics::semantics_paid),
        ("spend_report", sources::spend_report),
        ("sources_doctor", sources::sources_doctor),
        ("provider_registry", sources::provider_registry),
        ("provider_readiness", sources::provider_readiness),
        ("sources_status", sources::sources_status),
        ("sources_export", sources::sources_export),
        ("wayback_history", sources_net::wayback_history),
        ("crtsh_subdomains", sources_net::crtsh_subdomains),
        ("regions_tree", sources_net::regions_tree),
        ("indexnow_submit", sources_net::indexnow_submit),
        ("topvisor_read", sources_net::topvisor_read),
        ("crux_report", sources_net::crux_report),
        ("miratext_analyze", sources_net::miratext_analyze),
        ("dataforseo_estimate", sources_dfs::dataforseo_estimate),
        ("google_keywords", sources_dfs::google_keywords),
        ("google_serp", sources_dfs::google_serp),
        ("keywords_expand", sources_yandex::keywords_expand),
        ("keywords_seasonality", sources_yandex::keywords_seasonality),
        ("serp_fetch", sources_yandex::serp_fetch),
        ("keywords_exact", sources_yandex::keywords_exact),
        ("metrika_counters", sources_metrika::metrika_counters),
        ("metrika_setup", sources_metrika::metrika_setup),
        ("metrika_report", sources_metrika::metrika_report),
        ("metrika_traffic_pdf", sources_metrika::metrika_traffic_pdf),
        ("gsc_query", sources_traffic::gsc_query),
        (
            "webmaster_url_queries",
            sources_traffic::webmaster_url_queries,
        ),
        ("sources_sync", sources_traffic::sources_sync),
        ("cloudflare_traffic", sources_cloudflare::cloudflare_traffic),
        ("cloudflare_history", sources_cloudflare::cloudflare_history),
        (
            "cloudflare_logpush_import",
            sources_cloudflare::cloudflare_logpush_import,
        ),
        (
            "cloudflare_instant_logs",
            sources_cloudflare::cloudflare_instant_logs,
        ),
        ("evidence_normalize", sources_evidence::evidence_normalize),
        (
            "provider_artifact_read",
            sources_evidence::provider_artifact_read,
        ),
        ("backlinks_check", sources_evidence::backlinks_check),
        ("backlinks_disavow", sources_evidence::backlinks_disavow),
        ("backlinks_profile", sources_evidence::backlinks_profile),
        (
            "provider_credential",
            sources_providers::provider_credential,
        ),
        ("provider_discover", sources_providers::provider_discover),
        ("provider_verify", sources_providers::provider_verify),
        ("provider_collect", sources_providers::provider_collect),
        ("provider_join", sources_providers::provider_join),
        ("provider_replay", sources_providers::provider_replay),
        ("provider_auth", sources_providers::provider_auth),
    ]
}

/// Run one handler by reference handler name; unknown names are [`ToolError::NotYetPorted`].
pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    let empty = Value::Object(Map::new());
    let args = if args.is_null() { &empty } else { args };
    let Some(handler) = registry().into_iter().find(|(key, _)| *key == name) else {
        return Err(ToolError::NotYetPorted(name.to_string()));
    };
    handler.1(args)
}

/// The command's keyword arguments, dropping keys the caller left unset (`None`
/// in the reference `_build_kwargs`; an explicit JSON `null` means the same thing on
/// both surfaces). A missing object is an empty one.
pub fn kwargs(args: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    if let Some(object) = args.as_object() {
        for (key, value) in object {
            if !value.is_null() {
                out.insert(key.clone(), value.clone());
            }
        }
    }
    out
}

/// One argument with `None` collapsed: reference handlers receive `**kwargs` of set
/// options only, so an explicit `null` means "not passed".
pub fn opt<'a>(args: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    args.get(name).filter(|value| !value.is_null())
}

/// One string argument, `None`-collapsed.
pub fn opt_str<'a>(args: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    opt(args, name).and_then(Value::as_str)
}

/// One integer argument, `None`-collapsed; a JSON float is not an int (reference `type(x) is int`).
pub fn opt_int(args: &Map<String, Value>, name: &str) -> Option<i64> {
    opt(args, name).and_then(Value::as_i64)
}

/// One float argument, `None`-collapsed.
pub fn opt_float(args: &Map<String, Value>, name: &str) -> Option<f64> {
    opt(args, name).and_then(Value::as_f64)
}

/// One boolean argument, `None`-collapsed.
pub fn opt_bool(args: &Map<String, Value>, name: &str) -> Option<bool> {
    opt(args, name).and_then(Value::as_bool)
}

/// A required string argument; the message is the CLI's own missing-option error.
pub fn need_str<'a>(
    args: &'a Map<String, Value>,
    name: &str,
    command: &str,
    accepted: &[&str],
) -> Result<&'a str, ToolError> {
    opt_str(args, name).ok_or_else(|| missing_option(command, name, accepted))
}

/// `"<cmd> is missing a required option: --<flag> (accepted: ...)"` — the text
/// the reference CLI boundary derives from the handler's `TypeError`.
pub fn missing_option(command: &str, name: &str, accepted: &[&str]) -> ToolError {
    let list: Vec<String> = accepted
        .iter()
        .map(|name| format!("--{}", name.replace('_', "-")))
        .collect();
    ToolError::Failed(format!(
        "{command} is missing a required option: --{} (accepted: {})",
        name.replace('_', "-"),
        list.join(", ")
    ))
}

/// The plain message of a store error: the reference CLI prints `str(exc)` of the
/// original `ValueError`/`ScanError`, without this crate's error-prefix wording.
pub fn plain(error: &spiderhead_store::error::StoreError) -> String {
    use spiderhead_store::error::StoreError;
    match error {
        StoreError::Sqlite(inner) => inner.to_string(),
        StoreError::Io(inner) => spiderhead_core::common::python_os_error(inner, None),
        StoreError::Json(inner) => inner.to_string(),
        StoreError::Invalid(message) => message.clone(),
        StoreError::ScanError(e) => e.message.clone(),
        other => other.to_string(),
    }
}

/// Store error → [`ToolError::Failed`] with the plain reference exception text.
pub fn failed(error: spiderhead_store::error::StoreError) -> ToolError {
    ToolError::Failed(plain(&error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_input_names_the_option() {
        let error = dispatch("scan_status", &json!({})).unwrap_err();
        assert_eq!(
            error.to_string(),
            "scan-status is missing a required option: --input-path (accepted: --input-path, --full-validation)"
        );
    }

    #[test]
    fn parse_requires_a_target() {
        let error = dispatch("parse", &json!({})).unwrap_err();
        assert_eq!(error.to_string(), "url or urls[] required");
    }

    #[test]
    fn unknown_handler_is_not_yet_ported() {
        let error = dispatch("no_such_handler", &json!({})).unwrap_err();
        assert_eq!(
            error.to_string(),
            "no_such_handler is not yet available in this build"
        );
    }
}
