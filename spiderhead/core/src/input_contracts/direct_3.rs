// Catalogue data only.

//! Direct CLI command contracts 3/6 (sources-sync .. project-inbox-goal).

use super::{command, form, form_req, CommandContract, InputKind};

/// Input contracts for the direct CLI commands in part 3 of 6.
pub(super) const COMMANDS: &[CommandContract] = &[
    command("sources-sync", Some("sources_sync"), &[
        form(InputKind::ProviderQuery, &["source", "resource"], ""),
        form(InputKind::LocalFile, &["db"], ""),
        form(InputKind::ProjectDirectory, &["project"], ""),
    ], "Explicit provider read and local SQLite write; records requested coverage and replaces eligible days atomically."),
    command("sources-status", Some("sources_status"), &[
        form(InputKind::LocalFile, &["db"], ""),
        form(InputKind::ProjectDirectory, &["project"], ""),
    ], ""),
    command("sources-export", Some("sources_export"), &[
        form(InputKind::LocalFile, &["db"], ""),
        form(InputKind::ProjectDirectory, &["project"], ""),
    ], ""),
    command("regions-tree", Some("regions_tree"), &[
        form(InputKind::LocalConfig, &[], ""),
    ], ""),
    command("topvisor-read", Some("topvisor_read"), &[
        form(InputKind::InlineJson, &["operation", "params"], "One bounded page of existing Topvisor data. Default operation is projects; other operations require params.project_id. Follow the provider's nextOffset for further pages. No paid checks or mutations."),
    ], ""),
    command("metrika-counters", Some("metrika_counters"), &[
        form(InputKind::LocalConfig, &[], ""),
    ], ""),
    command("metrika-setup", Some("metrika_setup"), &[
        form(InputKind::ProviderQuery, &["counter_id"], ""),
    ], ""),
    command("metrika-report", Some("metrika_report"), &[
        form(InputKind::ProviderQuery, &["counter_id"], ""),
    ], ""),
    command("metrika-traffic-pdf", Some("metrika_traffic_pdf"), &[
        form_req(InputKind::ProviderQuery, &["counter_id"], "", &["date1", "date2", "out_dir"]),
        form_req(InputKind::LocalFile, &["document"], "Renders an existing traffic document offline; no Metrika request.", &["out_dir"]),
        form(InputKind::LocalConfig, &["brand"], "Optional brand JSON file or inline object."),
        form(InputKind::InlineJson, &["gsc_rows"], "Optional Search Console query rows."),
    ], ""),
    command("google-keywords", Some("google_keywords"), &[
        form(InputKind::ProviderQuery, &["keywords"], ""),
        form(InputKind::ProviderQuery, &["seed"], ""),
    ], ""),
    command("dataforseo-estimate", Some("dataforseo_estimate"), &[
        form(InputKind::InlineJson, &["operation", "query_count", "row_limit", "cache_hits", "prices", "env", "query"], ""),
    ], ""),
    command("backlinks-profile", Some("backlinks_profile"), &[
        form(InputKind::ProviderQuery, &["domain"], ""),
        form(InputKind::InlineJson, &["imports", "evidence", "sources"], ""),
        form(InputKind::LocalFile, &["db"], ""),
        form(InputKind::ProjectDirectory, &["project"], ""),
    ], ""),
    command("backlinks-disavow", Some("backlinks_disavow"), &[
        form(InputKind::UrlList, &["urls", "domains"], ""),
        form(InputKind::LocalFile, &["output"], ""),
    ], ""),
    command("google-serp", Some("google_serp"), &[
        form(InputKind::ProviderQuery, &["query"], ""),
    ], ""),
    command("wayback-history", Some("wayback_history"), &[
        form(InputKind::LiveUrl, &["url"], ""),
    ], ""),
    command("cloudflare-traffic", Some("cloudflare_traffic"), &[
        form(InputKind::ProviderQuery, &["zone"], ""),
    ], ""),
    command("crtsh-subdomains", Some("crtsh_subdomains"), &[
        form(InputKind::Domain, &["domain"], ""),
    ], ""),
    command("gsc-query", Some("gsc_query"), &[
        form(InputKind::ProviderQuery, &["site_url"], ""),
    ], ""),
    command("webmaster-url-queries", Some("webmaster_url_queries"), &[
        form(InputKind::ProviderQuery, &["host_id"], "Bounded Yandex URL-to-query evidence."),
    ], ""),
    command("miratext-analyze", Some("miratext_analyze"), &[
        form(InputKind::InlineJson, &[], "Miratext analysis; paid modes require confirmation."),
    ], ""),
    command("gsc-archive", Some("gsc_archive"), &[
        form(InputKind::LocalFile, &["database"], "Explicit SQLite archive. Only prepare creates a missing file; status and backup are offline."),
        form(InputKind::InlineJson, &["database", "action", "site_url", "start_date", "end_date", "max_requests", "pause", "backup_path"], "prepare queues inclusive dates; run makes bounded Google API calls; backup requires a new destination."),
    ], ""),
    command("crux-report", Some("crux_report"), &[
        form(InputKind::ProviderQuery, &["url"], ""),
        form(InputKind::ProviderQuery, &["origin"], ""),
    ], ""),
    command("indexnow-submit", Some("indexnow_submit"), &[
        form(InputKind::UrlList, &["urls"], ""),
    ], ""),
    command("scan-list", Some("scan_list"), &[
        form(InputKind::LegacyDirectory, &["directory"], ""),
        form(InputKind::ProjectDirectory, &["project"], ""),
    ], ""),
    command("project-new", Some("project_new"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::LiveUrl, &["target"], ""),
    ], ""),
    command("project-open", Some("project_open"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
    ], ""),
    command("project-archive", Some("project_archive"), &[
        form(InputKind::ProjectDirectory, &["project"], ""),
        form(InputKind::LocalFile, &["out"], ""),
        form(InputKind::Selector, &["dry_run", "exclude_bodies", "exclude_bodies_older_than_days", "keep_newest_scans"], ""),
    ], ""),
    command("project-restore", Some("project_restore"), &[
        form(InputKind::LocalFile, &["archive", "to"], ""),
    ], ""),
    command("project-archive-list", Some("project_archive_list"), &[
        form(InputKind::LocalFile, &["archive"], "A project archive; every member is verified against the manifest, nothing is unpacked."),
    ], ""),
    command("project-sources-link", Some("project_sources_link"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
    ], "Binds a provider resource ID (--service, --resource, optional --label) to the project in sources.json; no network."),
    command("project-sources-unlink", Some("project_sources_unlink"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
    ], "Removes one (--service, --resource) binding from sources.json; no network."),
    command("project-sources-list", Some("project_sources_list"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
    ], ""),
    command("project-status", Some("project_status"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
    ], ""),
    command("project-progress", Some("project_progress"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::InlineJson, &["limit", "offset"], "Optional bounded progress pagination."),
    ], ""),
    command("project-observe", Some("project_observe"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["consumer"], "Optional scoped unread-notice recipient."),
        form(InputKind::InlineJson, &["scan_limit"], "Bounded retained scan-history page."),
        form(InputKind::InlineJson, &["run_offset", "run_limit"], "Per-site terminal run page; all stored running records remain visible."),
        form(InputKind::Selector, &["since_revision"], "Optional opaque observer revision for a compact unchanged response."),
    ], ""),
    command("project-inbox-submit", Some("project_inbox_submit"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::InlineText, &["text"], ""),
        form(InputKind::Selector, &["kind", "references", "author_role"], ""),
    ], ""),
    command("project-inbox-list", Some("project_inbox_list"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["consumer", "offset", "limit", "include_acknowledged"], ""),
    ], ""),
    command("project-inbox-read", Some("project_inbox_read"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["consumer", "entry_ids", "expected_revision"], ""),
    ], ""),
    command("project-inbox-acknowledge", Some("project_inbox_acknowledge"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["consumer", "entry_ids", "expected_revision"], ""),
    ], ""),
    command("project-inbox-goal", Some("project_inbox_goal"), &[
        form(InputKind::ProjectDirectory, &["directory"], ""),
        form(InputKind::Selector, &["entry_id", "state", "expected_revision"], ""),
    ], ""),
];
