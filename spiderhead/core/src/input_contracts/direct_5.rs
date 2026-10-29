// Catalogue data only.

//! Direct CLI command contracts 5/6 (provider-verify .. scan-link-inspect).

use super::{command, form, form_req, CommandContract, InputKind};

/// Input contracts for the direct CLI commands in part 5 of 6.
pub(super) const COMMANDS: &[CommandContract] = &[
    command("provider-verify", Some("provider_verify"), &[
        form(InputKind::ProviderIdentifier, &["provider"], ""),
        form(InputKind::InlineJson, &["request"], "Optional read-only target-access request."),
    ], ""),
    command("provider-collect", Some("provider_collect"), &[
        form(InputKind::ProviderIdentifier, &["provider"], ""),
        form(InputKind::Selector, &["operation"], ""),
        form(InputKind::InlineJson, &["request"], ""),
        form(InputKind::LocalDirectory, &["artifact_dir"], "Optional restricted raw-evidence location."),
    ], ""),
    command("provider-join", Some("provider_join"), &[
        form(InputKind::InlineJson, &["crawl_pages"], ""),
        form(InputKind::InlineJson, &["evidence_rows"], ""),
        form(InputKind::InlineJson, &["adjustments"], "Optional evidence-backed priority adjustments."),
    ], ""),
    command("evidence-normalize", Some("evidence_normalize"), &[
        form(InputKind::LocalFile, &["file"], "Supplied CSV/XLSX/JSON rows or a saved provider-evidence envelope; fully offline."),
        form(InputKind::InlineJson, &["mapping"], "Optional seohead.evidence-mapping.v1 manifest, inline or file path."),
        form(InputKind::LocalDirectory, &["out_dir"], "Optional restricted normalized artifact directory."),
    ], ""),
    command("evidence-join", Some("evidence_join"), &[
        form(InputKind::ScanArtifact, &["scan"], "Alternative crawl side; offline scan read like provider-replay."),
        form(InputKind::AuditDocument, &["audit"], "Alternative crawl side."),
        form(InputKind::InlineJson, &["pages"], "Alternative crawl side, page objects."),
        form(InputKind::LocalFile, &["evidence", "compare"], "CSV/XLSX/JSON or saved provider envelope; inline JSON also accepted."),
        form(InputKind::InlineJson, &["mapping", "compare_mapping", "policy"], "Mapping manifests and the declared comparison policy, inline or file path."),
        form(InputKind::LocalDirectory, &["out_dir"], "Optional private join/compatibility artifact directory."),
    ], ""),
    command("bi-export", Some("bi_export"), &[
        form(InputKind::ScanArtifact, &["scan"], "Alternative validated scan.v1 source."),
        form(InputKind::AuditDocument, &["audit"], "Alternative supported audit JSON source."),
        form(InputKind::LocalFile, &["provider_joins"], "Optional saved issue #781 evidence-join/normalized-evidence JSON files; repeatable."),
        form(InputKind::LocalDirectory, &["out_dir"], "Required new local package directory; existing output is refused."),
        form(InputKind::InlineJson, &["max_rows_per_file", "max_bytes_per_file", "max_output_bytes", "max_scan_bytes", "search_metric", "xlsx_max_rows_per_sheet"], "Optional positive partition/total-output bounds and explicit Search Console clicks or impressions axis; exceeding a hard limit fails without publishing a package."),
        form_req(InputKind::LocalFile, &["xlsx_out"], "Optional new split XLSX consumer output from one verified package dataset.", &["xlsx_dataset"]),
        form_req(InputKind::Selector, &["xlsx_dataset"], "", &["xlsx_out"]),
    ], "Reads saved artifacts only; no provider calls, crawl, or remote writes."),
    command("publication-cohorts", Some("publication_cohorts"), &[
        form(InputKind::InlineJson, &["document"], "spiderhead.publication-cohort-input.v1 document."),
        form(InputKind::LocalFile, &["file"], "Alternative versioned offline cohort input JSON."),
        form(InputKind::LocalDirectory, &["out_dir"], "Required new local package directory; existing output is refused."),
    ], "Reads saved normalized evidence only; no provider calls or causal inference."),
    command("gsc-progress", Some("gsc_progress"), &[
        form(InputKind::InlineJson, &["document"], "spiderhead.gsc-progress-input.v1 document."),
        form(InputKind::LocalFile, &["file"], "Alternative versioned offline GSC input JSON."),
        form(InputKind::LocalDirectory, &["out_dir"], "Required new local package directory; existing output is refused."),
    ], "Reads saved normalized GSC evidence only; no provider calls or rank-placement claims."),
    command("bi-sheets-plan", Some("bi_sheets_plan"), &[
        form(InputKind::LocalDirectory, &["package"], ""),
        form(InputKind::InlineJson, &["max_cells"], "Optional declared capacity, never an API quota check."),
    ], "Offline package/checksum/capacity preflight; no Google authentication or write."),
    command("bi-bigquery-plan", Some("bi_bigquery_plan"), &[
        form(InputKind::LocalDirectory, &["package"], ""),
        form(InputKind::Selector, &["dataset", "operation"], ""),
    ], "Offline optional-load plan; no project selection, billing, authentication, or write."),
    command("bi-destination-apply", Some("bi_destination_apply"), &[
        form(InputKind::LocalDirectory, &["package"], ""),
        form(InputKind::Selector, &["target", "destination", "operation"], ""),
        form(InputKind::InlineJson, &["apply"], "Requires true and a host-injected authorized client."),
        form(InputKind::InlineJson, &["reconcile"], "Explicitly reconciles a retained uncertain request before any retry; requires apply=true."),
    ], "Never accepts credentials; a missing host client fails before any destination write."),
    command("inspect-url", Some("inspect_url"), &[
        form(InputKind::LiveUrl, &["url"], ""),
        form(InputKind::InlineJson, &["checks"], "Optional bounded selection of closed investigation checks."),
    ], ""),
    command("audit-workflow", Some("audit_workflow"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["action"], "status, start, prepare, or report."),
        form(InputKind::LiveUrl, &["target"], "Required only for action=start."),
        form(InputKind::AuditDocument, &["audit"], "Optional for action=report; omitted, the project's latest complete audited scan is used."),
    ], ""),
    command("workflow-status", Some("workflow_status"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
    ], ""),
    command("workflow-start", Some("workflow_start"), &[
        form_req(InputKind::ProjectDirectory, &["directory"], "", &["scenario_id", "steps"]),
    ], ""),
    command("workflow-checkpoint", Some("workflow_checkpoint"), &[
        form_req(InputKind::ProjectDirectory, &["directory"], "", &["run_id", "step_id", "state", "expected_revision"]),
    ], ""),
    command("workflow-execute", Some("workflow_execute"), &[
        form_req(InputKind::ProjectDirectory, &["directory"], "", &["scenario_id", "steps", "outcomes"]),
    ], ""),
    command("workflow-resume", Some("workflow_resume"), &[
        form_req(InputKind::ProjectDirectory, &["directory"], "", &["run_id", "expected_revision"]),
    ], ""),
    command("monitor-status", Some("monitor_status"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
    ], ""),
    command("monitor-configure", Some("monitor_configure"), &[
        form_req(InputKind::ProjectDirectory, &["directory"], "", &["policy"]),
    ], ""),
    command("monitor-run", Some("monitor_run"), &[
        form_req(InputKind::ProjectDirectory, &["directory"], "", &["scan_id", "observations", "expected_revision"]),
    ], "Records supplied retained-scan differences only; it starts no schedule or delivery."),
    command("monitor-collect", Some("monitor_collect"), &[
        form_req(InputKind::ProjectDirectory, &["directory"], "", &["expected_revision"]),
        form(InputKind::Selector, &["expected_revision", "apply"], ""),
    ], "Preview by default; explicit apply collects only an existing validated bounded claim. No timer or delivery starts."),
    command("monitor-local-deliver", Some("monitor_local_deliver"), &[
        form_req(InputKind::ProjectDirectory, &["directory"], "", &["scan_id", "expected_revision"]),
        form(InputKind::Selector, &["scan_id", "expected_revision"], ""),
    ], "Records a local:receipt only; no network, external transport or recipient delivery claim."),
    command("monitor-schedule", Some("monitor_schedule"), &[
        form_req(InputKind::ProjectDirectory, &["directory"], "Claims, cancels, backs off, or recovers a local bounded pass; it starts no timer.", &["action", "expected_revision"]),
    ], ""),
    command("tool-catalog", Some("tool_catalog"), &[
        form(InputKind::InlineText, &["query"], "Optional bounded discovery query."),
        form(InputKind::Selector, &["limit", "offset", "include_arguments"], ""),
    ], ""),
    command("url-check-batch", Some("url_check_batch"), &[
        form(InputKind::UrlList, &["urls"], ""),
        form(InputKind::Selector, &["checks"], ""),
        form(InputKind::InlineJson, &["options"], "Shared options for the selected checks."),
        form(InputKind::ScanArtifact, &["scan"], ""),
        form(InputKind::Selector, &["offset", "max_urls"], ""),
    ], ""),
    command("scan-inspect", Some("scan_inspect"), &[
        form(InputKind::ScanArtifact, &["input_path"], ""),
    ], ""),
    command("scan-content-search", Some("scan_content_search"), &[
        form(InputKind::ScanArtifact, &["input_path"], ""),
        form(InputKind::InlineText, &["query"], "Offline retained-content query; package output is bounded and source-identified."),
        form(InputKind::Selector, &["url"], "Optional exact logical URL filter."),
    ], ""),
    command("scan-content-search-page", Some("scan_content_search_page"), &[
        form(InputKind::LocalFile, &["package"], "Read one bounded page from a prior offline search package."),
    ], ""),
    command("scan-url-detail", Some("scan_url_detail"), &[
        form(InputKind::ScanArtifact, &["input_path"], ""),
        form(InputKind::Selector, &["url", "body_offset", "body_length", "max_body_bytes"], "Exact retained native URL; bounded source HTML and redacted query values."),
    ], ""),
    command("scan-body-read", Some("scan_body_read"), &[
        form(InputKind::ScanArtifact, &["input_path"], ""),
        form(InputKind::Selector, &["url", "representation", "variant_key", "offset", "length", "max_decoded_bytes"], "Exact retained URL; body output is bounded and read-only."),
    ], ""),
    command("scan-findings-page", Some("scan_findings_page"), &[
        form(InputKind::ProjectDirectory, &["project"], ""),
        form(InputKind::Selector, &["scan_uuid", "offset", "limit", "query", "url", "sort", "descending"], ""),
    ], ""),
    command("scan-url-counts", Some("scan_url_counts"), &[
        form(InputKind::ScanArtifact, &["input_path"], ""),
    ], ""),
    command("scan-config-diff", Some("scan_config_diff"), &[
        form(InputKind::ScanArtifact, &["before_path"], ""),
        form(InputKind::ScanArtifact, &["after_path"], ""),
    ], ""),
    command("extraction-presets", Some("extraction_presets"), &[
        form(InputKind::NoInput, &[], ""),
    ], ""),
    command("scan-url-query", Some("scan_url_query"), &[
        form(InputKind::ScanArtifact, &["input_path"], ""),
    ], ""),
    command("scan-directory-tree", Some("scan_directory_tree"), &[
        form(InputKind::ScanArtifact, &["input_path"], "Retained URL paths only; no network access."),
    ], ""),
    command("headers-analyze", Some("headers_analyze"), &[
        form(InputKind::ScanArtifact, &["input_path"], "Reads saved redacted response headers only."),
    ], ""),
    command("scan-url-history", Some("scan_url_history"), &[
        form(InputKind::ProjectDirectory, &["project"], ""),
        form(InputKind::Selector, &["url"], "Exact retained URL text, as scan-url-detail takes it."),
    ], ""),
    command("scan-link-inspect", Some("scan_link_inspect"), &[
        form(InputKind::ScanArtifact, &["input_path"], "Offline path, inlinks, occurrence context or one URL's paged links selected by view; mode-specific selectors and limits are required."),
    ], ""),
];
