// Catalogue data only.

//! Direct CLI command contracts 2/6 (links-check .. sources-doctor).

use super::{command, form, form_req, CommandContract, InputKind};

/// Input contracts for the direct CLI commands in part 2 of 6.
pub(super) const COMMANDS: &[CommandContract] = &[
    command("links-check", Some("links_check"), &[
        form(InputKind::LiveUrl, &["url"], ""),
    ], ""),
    command("hreflang-check", Some("hreflang_check"), &[
        form(InputKind::LiveUrl, &["url"], ""),
    ], ""),
    command("domain-profile", Some("domain_profile"), &[
        form(InputKind::Domain, &["domain"], ""),
    ], ""),
    command("cdn-check", Some("cdn_check"), &[
        form(InputKind::LiveUrl, &["url"], ""),
    ], ""),
    command("tech-detect", Some("tech_detect"), &[
        form(InputKind::LiveUrl, &["url"], ""),
    ], ""),
    command("security-check", Some("security_check"), &[
        form(InputKind::LiveUrl, &["url"], ""),
    ], ""),
    command("backlinks-check", Some("backlinks_check"), &[
        form_req(InputKind::Domain, &["target"], "", &["donors"]),
        form_req(InputKind::UrlList, &["donors"], "", &["target"]),
        form_req(InputKind::LocalFile, &["donors_file"], "", &["target"]),
    ], ""),
    command("schema-check", Some("schema_check"), &[
        form(InputKind::LiveUrl, &["url"], ""),
        form(InputKind::InlineHtml, &["html"], ""),
    ], ""),
    command("schema-build", Some("schema_build"), &[
        form(InputKind::LiveUrl, &["url"], ""),
        form(InputKind::InlineHtml, &["html"], ""),
    ], ""),
    command("duplicate-check", Some("duplicate_check"), &[
        form(InputKind::InlineCorpus, &["items"], ""),
        form(InputKind::ScanArtifact, &["scan"], ""),
    ], ""),
    command("ai-bots-check", Some("ai_bots_check"), &[
        form(InputKind::LiveUrl, &["url"], ""),
        form(InputKind::InlineText, &["robots_text"], ""),
    ], ""),
    command("mirror-check", Some("mirror_check"), &[
        form(InputKind::LiveUrl, &["url"], ""),
    ], ""),
    command("llms-txt-check", Some("llms_txt_check"), &[
        form(InputKind::LiveUrl, &["url"], ""),
    ], ""),
    command("citability-check", Some("citability_check"), &[
        form(InputKind::LiveUrl, &["url"], ""),
        form(InputKind::InlineText, &["text"], ""),
    ], ""),
    command("markdown-extract", Some("markdown_extract"), &[
        form(InputKind::LiveUrl, &["url"], ""),
        form(InputKind::InlineHtml, &["html"], ""),
    ], ""),
    command("markdown-export", Some("markdown_export"), &[
        form(InputKind::LiveUrl, &["url"], ""),
        form(InputKind::LocalFile, &["urls_file"], "TXT, one URL per line"),
        form(InputKind::LiveUrl, &["sitemap"], "Sitemap or sitemap index; a site root resolves via robots.txt"),
    ], ""),
    command("boilerplate-report", Some("boilerplate_report"), &[
        form(InputKind::InlineCorpus, &["pages"], ""),
        form(InputKind::ScanArtifact, &["scan"], ""),
    ], ""),
    command("semantic-inputs", Some("semantic_inputs"), &[
        form(InputKind::InlineCorpus, &["items"], ""),
        form(InputKind::ScanArtifact, &["scan"], ""),
    ], ""),
    command("semantic-similarity", Some("semantic_similarity"), &[
        form_req(InputKind::InlineCorpus, &["items"], "Supplied normalized-vector evidence; no model is loaded or called.", &["embeddings", "adapter", "cache_path"]),
        form_req(InputKind::ScanArtifact, &["scan"], "Uses the retained semantic corpus and its recorded normalization policy.", &["embeddings", "adapter", "cache_path"]),
        form(InputKind::InlineJson, &["embeddings", "adapter"], ""),
        form(InputKind::LocalFile, &["cache_path"], "Local SQLite embedding cache."),
        form(InputKind::Selector, &["threshold", "max_candidate_comparisons"], ""),
    ], ""),
    command("meta-description-drafts", Some("meta_description_drafts"), &[
        form(InputKind::InlineCorpus, &["items"], "Dry-run needs only supplied page HTML."),
        form(InputKind::ScanArtifact, &["scan"], "Uses retained normalized page content offline."),
        form(InputKind::InlineJson, &["context"], "Optional versioned site instructions."),
        form_req(InputKind::InlineJson, &["drafts", "executor"], "Optional structured caller/delegated-agent results; no provider call.", &["checkpoint_path"]),
        form(InputKind::LocalFile, &["checkpoint_path", "json_path", "csv_path"], ""),
        form(InputKind::Selector, &["batch_size"], ""),
    ], ""),
    command("ai-column", Some("ai_column"), &[
        form(InputKind::InlineCorpus, &["items"], "Dry-run needs only supplied normalized page text."),
        form(InputKind::ScanArtifact, &["scan"], "Uses retained normalized page content offline."),
        form(InputKind::InlineText, &["prompt"], "Required instruction, at most 2000 characters."),
        form(InputKind::InlineJson, &["urls", "rows"], "Optional URL selection and caller-supplied values; no provider call."),
        form(InputKind::LocalFile, &["csv_path"], "Formula-safe CSV of the column values."),
        form(InputKind::Selector, &["column", "max_pages"], ""),
    ], ""),
    command("social-meta-check", Some("social_meta_check"), &[
        form(InputKind::LiveUrl, &["url"], ""),
        form(InputKind::InlineJson, &["og", "twitter"], ""),
    ], ""),
    command("soft404-check", Some("soft404_check"), &[
        form(InputKind::LiveUrl, &["url"], ""),
    ], ""),
    command("log-analyze", Some("log_analyze"), &[
        form(InputKind::LocalLog, &["path"], ""),
        form(InputKind::Selector, &["verify_bots", "max_lines", "offset", "limit", "max_dns", "bot", "ip"], ""),
    ], ""),
    command("logs-import", Some("logs_import"), &[
        form(InputKind::LocalLog, &["path"], ""),
        form(InputKind::ProjectDirectory, &["project"], ""),
        form(InputKind::Selector, &["max_lines", "max_files"], ""),
    ], ""),
    command("logs-summary", Some("logs_summary"), &[
        form(InputKind::LocalFile, &["source"], ""),
        form(InputKind::ProjectDirectory, &["project"], "Resolve the project's newest retained log store."),
        form(InputKind::Selector, &["view", "limit", "offset", "bot", "family", "ip", "date_from", "date_to", "status", "section"], ""),
    ], ""),
    command("logs-join", Some("logs_join"), &[
        form(InputKind::LocalFile, &["source"], ""),
        form(InputKind::ScanArtifact, &["scan"], ""),
        form(InputKind::Selector, &["view", "base_url", "family", "bot", "date_from", "date_to", "limit", "offset"], ""),
    ], ""),
    command("logs-bots", Some("logs_bots"), &[
        form(InputKind::LocalFile, &["source"], ""),
        form(InputKind::ProjectDirectory, &["project"], "Resolve the project's newest retained log store."),
        form(InputKind::Selector, &["verify_bots", "limit", "offset", "bot", "family", "ip", "verdict", "max_dns"], ""),
    ], ""),
    command("regions-check", Some("regions_check"), &[
        form(InputKind::LiveUrl, &["url"], ""),
    ], ""),
    command("render-check", Some("render_check"), &[
        form(InputKind::LiveUrl, &["url"], ""),
        form(InputKind::InlineJson, &["transport_config"], "Optional local/remote Playwright transport selection; endpoint is named by environment variable."),
    ], ""),
    command("site-audit", Some("site_audit"), &[
        form(InputKind::LiveUrl, &["url"], ""),
        form(InputKind::UrlList, &["urls"], ""),
    ], ""),
    command("report-build", Some("report_build"), &[
        form(InputKind::AuditDocument, &["audit"], "Audit JSON or a retained scan.v1 artifact."),
        form(InputKind::ProjectDirectory, &["project"], "Includes validated checklist coverage and optionally applies a saved finding view."),
        form(InputKind::Selector, &["view"], "Optional saved project finding view; requires project."),
        form(InputKind::Selector, &["offset"], "Optional stable finding-view page offset."),
        form(InputKind::Selector, &["pdf_policy"], "Explicit overview-v1 for PDF + retained audit.v2 only: bounded PDF with complete mandatory JSON/CSV/manifest companions; never inferred."),
    ], ""),
    command("facts-export", Some("facts_export"), &[
        form(InputKind::InlineJson, &["sites"], ""),
    ], ""),
    command("marketing-inventory", Some("marketing_inventory"), &[
        form(InputKind::InlineJson, &["documents"], ""),
        form(InputKind::Selector, &["cta_selector", "form_selector", "id_attributes", "id_parameters"], ""),
        form(InputKind::LocalDirectory, &["out_dir"], ""),
    ], ""),
    command("keywords-expand", Some("keywords_expand"), &[
        form(InputKind::ProviderQuery, &["phrase"], ""),
    ], ""),
    command("keywords-seasonality", Some("keywords_seasonality"), &[
        form(InputKind::ProviderQuery, &["phrase"], ""),
    ], ""),
    command("keywords-exact", Some("keywords_exact"), &[
        form(InputKind::ProviderQuery, &["keywords"], ""),
    ], ""),
    command("serp-fetch", Some("serp_fetch"), &[
        form(InputKind::ProviderQuery, &["query"], ""),
        form(InputKind::ProviderQuery, &["queries"], ""),
    ], ""),
    command("spend-report", Some("spend_report"), &[
        form(InputKind::LocalLog, &[], "Configured local spend log."),
    ], ""),
    command("sources-doctor", Some("sources_doctor"), &[
        form(InputKind::LocalConfig, &[], ""),
    ], ""),
];
