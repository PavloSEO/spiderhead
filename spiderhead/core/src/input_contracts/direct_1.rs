// Catalogue data only.

//! Direct CLI command contracts 1/6 (bi-filter .. asset-weight-check).

use super::{command, form, form_req, CommandContract, InputKind};

/// Input contracts for the direct CLI commands in part 1 of 6.
pub(super) const COMMANDS: &[CommandContract] = &[
    command("bi-filter", Some("bi_filter"), &[
        form(InputKind::LocalDirectory, &["package", "out_dir"], ""),
        form(InputKind::Selector, &["dataset", "columns"], ""),
        form(InputKind::InlineJson, &["where", "max_rows_per_file", "max_bytes_per_file", "max_output_bytes", "xlsx_max_rows_per_sheet"], ""),
        form(InputKind::LocalFile, &["xlsx_out"], ""),
    ], ""),
    command("scan-navigation", Some("scan_navigation"), &[
        form(InputKind::ScanArtifact, &["input_path"], ""),
        form(InputKind::Selector, &["document_id", "limit", "offset"], ""),
    ], ""),
    command("doctor", Some("doctor"), &[
        form(InputKind::ProjectDirectory, &["projects_directory"], "Optional directory access check."),
        form(InputKind::LocalConfig, &["config_path"], ""),
        form(InputKind::LocalFile, &["sf_cli"], "Optional crawler executable path."),
    ], ""),
    command("update-check", Some("update_check"), &[
        form(InputKind::Selector, &["consent", "automatic", "max_releases"], "Network access requires consent; no scheduler starts."),
    ], ""),
    command("update-download", Some("update_download"), &[
        form(InputKind::Selector, &["consent", "tag", "asset_name", "max_bytes"], "Downloads only an asset from a previously checked release."),
        form(InputKind::LocalFile, &["out"], "New destination file; the package is not installed."),
    ], ""),
    command("project-activity", Some("project_activity"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["since_revision"], "Optional opaque observer revision for a compact unchanged response."),
    ], ""),
    command("project-checklist-page", Some("project_checklist_page"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["offset", "limit", "query", "kind", "state", "sort", "descending", "states"], ""),
    ], ""),
    command("project-task-detail", Some("project_task_detail"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["item_id"], ""),
    ], ""),
    command("project-scans", Some("project_scans"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["offset", "limit"], ""),
    ], ""),
    command("provider-auth", Some("provider_auth"), &[
        form(InputKind::InlineJson, &["provider", "action", "grant_file", "confirm"], "GSC private grant import/status/refresh; cancel a pending browser flow; confirmed disconnect or remote revoke. No secret values returned."),
    ], ""),
    command("provider-replay", Some("provider_replay"), &[
        form_req(InputKind::ScanArtifact, &["input_path"], "Offline join with a private saved provider envelope; raw joins remain in restricted output.", &["evidence_file", "out_dir"]),
    ], ""),
    command("parse", Some("parse"), &[
        form(InputKind::LiveUrl, &["url"], ""),
        form(InputKind::UrlList, &["urls"], ""),
    ], ""),
    command("crawl-site", Some("crawl_site"), &[
        form(InputKind::LiveUrl, &["url"], ""),
        form(InputKind::UrlList, &["urls"], ""),
        form(InputKind::LocalFile, &["urls_file"], "TXT, CSV, XLSX, or XML URL input"),
        form(InputKind::ScanArtifact, &["resume"], "Resumes retained crawl evidence and continues network collection."),
        form(InputKind::LocalConfig, &["config"], ""),
        form(InputKind::LocalProfile, &["profile"], "Selects a saved local crawl profile by name."),
        form(InputKind::ProjectDirectory, &["project"], "Defaults the target and scans/ path; explicit paths, legacy output and resume keep their route."),
    ], ""),
    command("crawl-describe-settings", Some("crawl_describe_settings"), &[
        form(InputKind::NoInput, &[], ""),
    ], ""),
    command("crawl-control", Some("crawl_control"), &[
        form(InputKind::ProjectDirectory, &["project"], ""),
        form(InputKind::Selector, &["run_id", "action"], ""),
        form(InputKind::Selector, &["run_id", "action", "exclude", "include", "urls"], "Live steering of an active crawl: narrow-scope or add-urls (#1485)."),
        form(InputKind::ScanArtifact, &["scan", "action", "exclude", "include", "urls"], "Paused artifact steering instead of a live run."),
    ], ""),
    command("scan-reanalyze", Some("scan_reanalyze"), &[
        form(InputKind::ScanArtifact, &["input_path"], ""),
    ], ""),
    command("scan-graph", Some("scan_graph"), &[
        form(InputKind::ScanArtifact, &["input_path"], ""),
        form(InputKind::ScanArtifact, &["compare_to"], "Optional retained comparison source."),
        form(InputKind::Selector, &["positions", "section", "around", "depth", "aggregate", "representation", "node_offset", "edge_offset", "node_limit", "edge_limit", "max_nodes", "max_edges", "timeout_seconds", "max_bytes"], ""),
    ], "Reads a bounded retained graph offline; missing evidence and clipping stay explicit."),
    command("scan-sitemap-generate", Some("scan_sitemap_generate"), &[
        form_req(InputKind::ScanArtifact, &["input_path"], "", &["out_dir", "sitemap_base_url"]),
        form(InputKind::LocalDirectory, &["out_dir"], "Fresh exclusive output directory; never deploys."),
        form(InputKind::Selector, &["sitemap_base_url", "include_images", "exclude"], ""),
    ], "Deployment base URL sets output scope; it is never fetched."),
    command("log-scan", Some("log_scan"), &[
        form(InputKind::LegacyDirectory, &["run"], ""),
        form(InputKind::ScanArtifact, &["run"], ""),
    ], ""),
    command("crawl-diagnose", Some("crawl_diagnose"), &[
        form(InputKind::ScanArtifact, &["scan"], ""),
        form(InputKind::LegacyDirectory, &["run"], ""),
    ], "Choose one retained source; diagnosis is offline and read-only."),
    command("crawl-diagnose-export", Some("crawl_diagnose_export"), &[
        form_req(InputKind::ScanArtifact, &["scan"], "", &["export"]),
        form_req(InputKind::LegacyDirectory, &["run"], "", &["export"]),
        form(InputKind::LocalFile, &["export"], ""),
    ], "Choose one retained source and a new redacted export destination; refuses overwrite."),
    command("compare-crawls", Some("compare_crawls"), &[
        form(InputKind::AuditDocument, &["before", "after"], "Each path may be audit JSON or scan.v1."),
        form(InputKind::LocalFile, &["correspondence"], "Optional closed url-correspondence.v1 JSON declaration; exact URL comparison remains the default."),
        form(InputKind::LocalDirectory, &["out_dir"], "Optional new compare.v2 package; required for audit.v2 populations above 10,000 pages or issues. Refuses overwrite."),
        form(InputKind::Selector, &["compression", "resume"], "none (default) or explicit gzip with out_dir; every row is retained and compressed/uncompressed bytes are declared. Resume requires an exact matching persistent checkpoint."),
    ], ""),
    command("compare-findings", Some("compare_findings"), &[
        form(InputKind::AuditDocument, &["before", "after"], "Each source may be audit JSON or scan.v1 SQLite."),
    ], "Offline and read-only: severity-aware findings delta, never a fixed-finding claim."),
    command("compare-read", Some("compare_read"), &[
        form(InputKind::LocalFile, &["manifest_path"], "compare.v2 compare.json manifest"),
        form(InputKind::Selector, &["file"], "one retained comparison stream"),
        form(InputKind::Selector, &["offset"], ""),
        form(InputKind::Selector, &["limit"], ""),
        form(InputKind::Selector, &["sort"], ""),
        form(InputKind::Selector, &["direction"], ""),
        form(InputKind::Selector, &["contains"], "optional URL substring filter"),
        form(InputKind::Selector, &["segment"], "named segment; only available for segment_changes"),
        form(InputKind::Selector, &["change"], "change type; only available for segment_changes"),
        form(InputKind::Selector, &["max_bytes"], ""),
    ], ""),
    command("compare-report", Some("compare_report"), &[
        form(InputKind::LocalFile, &["manifest_path"], "compare.v2 compare.json manifest"),
        form(InputKind::LocalFile, &["out_file"], "new Markdown path; refuses overwrite"),
        form(InputKind::Selector, &["max_bytes"], ""),
    ], ""),
    command("compare-status", Some("compare_status"), &[
        form(InputKind::LocalDirectory, &["out_dir"], "comparison output path; reads active checkpoint or completed cache"),
    ], ""),
    command("compare-url", Some("compare_url"), &[
        form(InputKind::AuditDocument, &["before", "after"], "Each source may be audit JSON or scan.v1 SQLite."),
        form(InputKind::Selector, &["url"], "Exact retained URL text."),
        form(InputKind::Selector, &["max_bytes"], ""),
        form(InputKind::Selector, &["max_lines"], ""),
        form(InputKind::Selector, &["left_representation", "right_representation"], ""),
    ], ""),
    command("verify-fixes", Some("verify_fixes"), &[
        form_req(InputKind::AuditDocument, &["baseline"], "", &["out_dir"]),
        form_req(InputKind::AuditDocument, &["after"], "Offline verification without recrawling.", &["baseline", "out_dir"]),
        form_req(InputKind::Selector, &["finding_ids"], "", &["baseline", "out_dir"]),
        form_req(InputKind::LocalFile, &["view"], "Saved verification_view.v1 selection.", &["baseline", "out_dir"]),
        form_req(InputKind::UrlList, &["urls"], "", &["baseline", "out_dir"]),
        form_req(InputKind::LocalFile, &["urls_file"], "", &["baseline", "out_dir"]),
        form(InputKind::LocalConfig, &["config"], "Required when the baseline redacted credentials."),
    ], ""),
    command("crawl-enrich", Some("crawl_enrich"), &[
        form_req(InputKind::AuditDocument, &["audit"], "", &["external_csv"]),
        form_req(InputKind::LocalFile, &["external_csv"], "", &["audit"]),
    ], ""),
    command("crawl-import", Some("crawl_import"), &[
        form(InputKind::LocalFile, &["manifest_path"], "third_party_crawl_manifest.v1 with manifest-relative CSV datasets; output remains foreign crawl evidence"),
    ], ""),
    command("segment-diff", Some("segment_diff"), &[
        form(InputKind::AuditDocument, &["audit"], ""),
    ], ""),
    command("redirects-generate", Some("redirects_generate"), &[
        form(InputKind::InlineJson, &["redirects"], ""),
    ], ""),
    command("redirects-check", Some("redirects_check"), &[
        form(InputKind::LiveUrl, &["url"], ""),
    ], ""),
    command("sitemap-crawl", Some("sitemap_crawl"), &[
        form(InputKind::LiveUrl, &["url"], ""),
        form(InputKind::ProjectDirectory, &["project"], "Optional explicit local observation; same origin only. Declared URLs are not fetched HTML pages."),
    ], ""),
    command("images-download", Some("images_download"), &[
        form(InputKind::UrlList, &["urls"], ""),
    ], ""),
    command("images-optimize", Some("images_optimize"), &[
        form(InputKind::LocalFile, &["files"], ""),
        form(InputKind::ScanArtifact, &["scan"], ""),
        form(InputKind::Selector, &["max_images"], ""),
    ], ""),
    command("keywords-cluster", Some("keywords_cluster"), &[
        form(InputKind::InlineJson, &["params"], ""),
    ], ""),
    command("robots-check", Some("robots_check"), &[
        form(InputKind::LiveUrl, &["url"], ""),
    ], ""),
    command("headers-check", Some("headers_check"), &[
        form(InputKind::LiveUrl, &["url"], ""),
    ], ""),
    command("asset-weight-check", Some("asset_weight_check"), &[
        form(InputKind::LiveUrl, &["url"], ""),
    ], ""),
];
