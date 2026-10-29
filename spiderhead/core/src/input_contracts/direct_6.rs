// Catalogue data only.

//! Direct CLI command contracts 6/6 (scan-status .. provider-discover).

use super::{command, form, CommandContract, InputKind};

/// Input contracts for the direct CLI commands in part 6 of 6.
pub(super) const COMMANDS: &[CommandContract] = &[
    command("scan-status", Some("scan_status"), &[
        form(InputKind::ScanArtifact, &["input_path"], ""),
    ], ""),
    command("scan-rendered-routes", Some("scan_rendered_routes"), &[
        form(InputKind::ScanArtifact, &["input_path"], ""),
    ], ""),
    command("scan-snapshot", Some("scan_snapshot"), &[
        form(InputKind::ScanArtifact, &["input_path"], ""),
    ], ""),
    command("scan-export", Some("scan_export"), &[
        form(InputKind::ScanArtifact, &["input_path"], "Also accepts an Export Analyzer audit.json document; links are unavailable there."),
        form(InputKind::Selector, &["records", "fields"], "Optional record-type and field projection validated before any file is written."),
    ], ""),
    command("scan-archive", Some("scan_archive"), &[
        form(InputKind::ScanArtifact, &["input_path"], "Retained scan read offline; no network."),
        form(InputKind::LocalFile, &["out"], "New WARC 1.1 destination; an existing output is refused. A sibling manifest names every skipped URL with its reason."),
        form(InputKind::Selector, &["max_body_bytes"], "Optional per-body decoded-byte cap; larger retained bodies are skipped with reason body_too_large."),
    ], ""),
    command("scan-pin", Some("scan_pin"), &[
        form(InputKind::ScanArtifact, &["input_path"], ""),
    ], ""),
    command("scan-prune", Some("scan_prune"), &[
        form(InputKind::LegacyDirectory, &["directory"], ""),
        form(InputKind::LocalFile, &["plan"], ""),
        form(InputKind::ProjectDirectory, &["project"], "Defaults the directory to project scans/; apply remains explicit."),
    ], ""),
    command("scan-body-diff", Some("scan_body_diff"), &[
        form(InputKind::ScanArtifact, &["left", "right"], ""),
        form(InputKind::Selector, &["url", "representation", "left_representation", "right_representation", "variant_key"], "Selects one exact URL and retained representation from both scans."),
    ], ""),
    command("scan-evidence", Some("scan_evidence"), &[
        form(InputKind::ScanArtifact, &["input_path"], ""),
        form(InputKind::Selector, &["section"], "capabilities, corpus, structured, routes, resources, or timeline."),
    ], ""),
    command("scan-extract", Some("scan_extract"), &[
        form(InputKind::ScanArtifact, &["input_path"], ""),
        form(InputKind::InlineJson, &["rules"], "Closed declarative rules over retained complete bodies."),
        form(InputKind::Selector, &["url"], "Optional exact logical URL."),
    ], ""),
    command("custom-extract-sample", Some("custom_extract_sample"), &[
        form(InputKind::InlineText, &["html"], "Supplied HTML; never fetched."),
        form(InputKind::InlineJson, &["extractors"], "Explicit extractor specifications."),
        form(InputKind::Selector, &["url"], "Optional output label only; never fetched."),
    ], ""),
    command("extract-suggest", Some("extract_suggest"), &[
        form(InputKind::InlineText, &["text"], "Visible text of the element to locate; required in every form."),
        form(InputKind::Selector, &["url"], "One sample page fetch; the command's only request."),
        form(InputKind::LocalFile, &["html"], "Local HTML sample file; no fetch."),
        form(InputKind::ScanArtifact, &["scan"], "Retained scan supplying the sample body."),
        form(InputKind::Selector, &["scan_url", "representation"], "Optional retained document selection."),
        form(InputKind::Selector, &["emit_rule", "rule_id"], "Emit a ready extraction_rules.v1 rule for the best candidate."),
    ], ""),
    command("scan-structured-blocks", Some("scan_structured_blocks"), &[
        form(InputKind::ScanArtifact, &["input_path"], ""),
        form(InputKind::Selector, &["url"], "Exact retained logical URL."),
        form(InputKind::Selector, &["representation"], "static, rendered, or legacy_fragment source."),
        form(InputKind::Selector, &["variant_key"], "Optional exact retained HTTP variant key."),
    ], ""),
    command("scan-fragment-links", Some("scan_fragment_links"), &[
        form(InputKind::ScanArtifact, &["input_path"], ""),
        form(InputKind::Selector, &["state"], "Optional resolved, missing, or skipped occurrence filter."),
        form(InputKind::Selector, &["representation"], "Optional static, rendered, or legacy_fragment source filter."),
    ], ""),
    command("scan-requeue", Some("scan_requeue"), &[
        form(InputKind::ScanArtifact, &["input_path"], ""),
        form(InputKind::Selector, &["where"], "Restricted saved URL/page predicate."),
        form(InputKind::LocalFile, &["backup_path"], "Mandatory new verified backup destination."),
        form(InputKind::ScanArtifact, &["from_scan"], "Optional alternate saved selection source."),
    ], ""),
    command("scan-import-urls", Some("scan_import_urls"), &[
        form(InputKind::ScanArtifact, &["input_path"], ""),
        form(InputKind::LocalFile, &["urls_file"], "Explicit TXT, CSV, XLSX, or XML URL source."),
        form(InputKind::LocalFile, &["backup_path"], "Mandatory new verified backup destination."),
    ], ""),
    command("cloudflare-history", Some("cloudflare_history"), &[
        form(InputKind::ProjectDirectory, &["project"], "Required local project with retained Cloudflare history."),
        form(InputKind::ProviderIdentifier, &["zone_id"], ""),
        form(InputKind::Selector, &["snapshot_id", "limit", "offset", "include_facts", "facts_offset", "facts_limit"], ""),
    ], "Reads retained snapshots only; facts are opt-in and bounded."),
    command("cloudflare-instant-logs", Some("cloudflare_instant_logs"), &[
        form(InputKind::ProjectDirectory, &["project"], "Uses the project's selected Cloudflare zone binding."),
        form(InputKind::Selector, &["confirm_live_capture", "max_seconds", "max_events", "max_bytes", "sample"], "Live capture requires explicit --confirm-live-capture and remains duration/event/byte bounded."),
    ], ""),
    command("cloudflare-logpush-import", Some("cloudflare_logpush_import"), &[
        form(InputKind::LocalFile, &["input_path"], "Caller-supplied UTF-8 Logpush JSON Lines file."),
        form(InputKind::ProjectDirectory, &["project"], "Project owning the local logstore."),
        form(InputKind::Selector, &["max_events"], "Maximum imported records."),
    ], ""),
    command("project-action-log", Some("project_action_log"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["offset", "limit", "client", "outcome", "query"], ""),
    ], "Bounded local read-only history."),
    command("project-agent-heartbeat", Some("project_agent_heartbeat"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
    ], "Records a timestamped interface event; it does not prove the client remains connected."),
    command("project-competitors-scan", Some("project_competitors_scan"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["task_id", "expected_revision", "confirm", "concurrency", "approve_large_crawl", "producer_build", "run_id"], "Network scans require explicit confirmation, current task revision, and any large-crawl approval; URL and concurrency caps remain enforced."),
    ], ""),
    command("project-competitors-scan-cancel", Some("project_competitors_scan_cancel"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["run_id"], ""),
    ], "Requests checkpointed cancellation of the named run."),
    command("project-competitors-scan-status", Some("project_competitors_scan_status"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["run_id"], ""),
    ], "Reads status for the named durable run."),
    command("project-delete", Some("project_delete"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
    ], "Moves the project to sibling Trash; active project/job writers are refused."),
    command("project-export", Some("project_export"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["dataset", "fmt", "compare_manifest", "job_id"], "compare_manifest is required for the compare dataset."),
        form(InputKind::LocalFile, &["output"], "New output beneath the project's reports directory; refuses overwrite."),
    ], ""),
    command("project-export-cancel", Some("project_export_cancel"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["job_id"], ""),
    ], "Requests cancellation at an export checkpoint."),
    command("project-export-status", Some("project_export_status"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["job_id"], ""),
    ], "Reads bounded status for the named export job."),
    command("project-exports-page", Some("project_exports_page"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["offset", "limit"], ""),
    ], "Reads a bounded page of retained export receipts."),
    command("project-settings", Some("project_settings"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
    ], "Reads local project settings and their revision."),
    command("project-settings-proposals", Some("project_settings_proposals"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["offset", "limit"], ""),
    ], "Reads a bounded page of setting proposals."),
    command("project-settings-propose", Some("project_settings_propose"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::InlineJson, &["changes"], "Proposed settings only; proposal does not apply the changes."),
        form(InputKind::Selector, &["expected_revision"], "Current proposal revision for compare-and-swap."),
    ], ""),
    command("project-settings-review", Some("project_settings_review"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["proposal_id", "decision", "expected_revision"], "Explicit local owner decision against the current proposal revision."),
    ], ""),
    command("project-task-comment", Some("project_task_comment"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::InlineText, &["text"], ""),
        form(InputKind::Selector, &["item_id", "actor", "expected_revision"], ""),
        form(InputKind::InlineJson, &["reference"], "Optional retained scan/finding reference."),
    ], ""),
    command("project-update", Some("project_update"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["label", "expected_revision"], "Label update uses the current settings revision."),
    ], ""),
    command("provider-artifact-read", Some("provider_artifact_read"), &[
        form(InputKind::LocalDirectory, &["artifact_dir"], "Caller-owned directory containing the saved provider artifact."),
        form(InputKind::Selector, &["artifact_ref", "offset", "limit"], "Reads only a bounded sanitized sample; no provider request."),
    ], ""),
    command("provider-credential", Some("provider_credential"), &[
        form(InputKind::ProviderIdentifier, &["provider"], ""),
        form(InputKind::Selector, &["component", "action", "confirm"], "Revocation requires explicit confirmation."),
        form(InputKind::StdinSecret, &["value"], "Only action=set; the CLI requires --value-stdin. Secret bytes never belong in argv, logs, or catalogue examples."),
        form(InputKind::Selector, &["replace"], "Only action=set; replacement is explicit."),
    ], ""),
    command("provider-discover", Some("provider_discover"), &[
        form(InputKind::ProviderIdentifier, &["provider"], ""),
        form(InputKind::Domain, &["domain"], "Optional domain-match hint; discovery does not bind or select resources."),
    ], ""),
];
