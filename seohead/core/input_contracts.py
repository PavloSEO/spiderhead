"""Code-owned input contracts for public SEOHEAD commands.

The catalogue deliberately describes accepted inputs, not execution.  In
particular, a ``scan_artifact`` is a retained local file: reading it never
replays a crawl or promises that a body was retained.
"""

from __future__ import annotations

from collections.abc import Collection
from dataclasses import dataclass


@dataclass(frozen=True)
class InputForm:
    """One supported input form, named by handler keyword arguments."""

    kind: str
    arguments: tuple[str, ...]
    note: str = ""
    required_with: tuple[str, ...] = ()


@dataclass(frozen=True)
class CommandContract:
    """One public command and its alternative supported input forms."""

    command: str
    handler: str | None
    forms: tuple[InputForm, ...]
    note: str = ""


def _form(
    kind: str, *arguments: str, note: str = "", required_with: tuple[str, ...] = ()
) -> InputForm:
    return InputForm(kind, arguments, note, required_with)


def _command(
    command: str, handler: str | None, *forms: InputForm, note: str = ""
) -> CommandContract:
    return CommandContract(command, handler, forms, note)


# Direct CLI commands.  This module is intentionally data-only: tests at
# the CLI/handler boundary prove the entries stay synchronized without making
# package runtime import either interface layer.
COMMAND_CONTRACTS: tuple[CommandContract, ...] = (
    _command(
        "bi-filter",
        "bi_filter",
        _form("local_directory", "package", "out_dir"),
        _form("selector", "dataset", "columns"),
        _form(
            "inline_json",
            "where",
            "max_rows_per_file",
            "max_bytes_per_file",
            "max_output_bytes",
            "xlsx_max_rows_per_sheet",
        ),
        _form("local_file", "xlsx_out"),
    ),
    _command(
        "scan-navigation",
        "scan_navigation",
        _form("scan_artifact", "input_path"),
        _form("selector", "document_id", "limit", "offset"),
    ),
    _command("project-activity", "project_activity", _form("project_directory", "directory")),
    _command(
        "project-checklist-page",
        "project_checklist_page",
        _form("project_directory", "directory"),
        _form(
            "selector", "offset", "limit", "query", "kind", "state", "sort", "descending", "states"
        ),
    ),
    _command(
        "project-task-detail",
        "project_task_detail",
        _form("project_directory", "directory"),
        _form("selector", "item_id"),
    ),
    _command(
        "project-scans",
        "project_scans",
        _form("project_directory", "directory"),
        _form("selector", "offset", "limit"),
    ),
    _command(
        "provider-auth",
        "provider_auth",
        _form(
            "inline_json",
            "provider",
            "action",
            "grant_file",
            "confirm",
            note="GSC private grant import/status/refresh; cancel a pending browser flow; confirmed disconnect or remote revoke. No secret values returned.",
        ),
    ),
    _command(
        "provider-replay",
        "provider_replay",
        _form(
            "scan_artifact",
            "input_path",
            required_with=("evidence_file", "out_dir"),
            note="Offline join with a private saved provider envelope; raw joins remain in restricted output.",
        ),
    ),
    _command("parse", "parse", _form("live_url", "url"), _form("url_list", "urls")),
    _command(
        "crawl-site",
        "crawl_site",
        _form("live_url", "url"),
        _form("url_list", "urls"),
        _form("local_file", "urls_file", note="TXT, CSV, XLSX, or XML URL input"),
        _form(
            "scan_artifact",
            "resume",
            note="Resumes retained crawl evidence and continues network collection.",
        ),
        _form("local_config", "config"),
        _form(
            "project_directory",
            "project",
            note="Defaults the target and scans/ path; explicit paths, legacy output and resume keep their route.",
        ),
    ),
    _command("crawl-describe-settings", "crawl_describe_settings", _form("no_input")),
    _command("scan-reanalyze", "scan_reanalyze", _form("scan_artifact", "input_path")),
    _command(
        "scan-graph",
        "scan_graph",
        _form("scan_artifact", "input_path"),
        _form("scan_artifact", "compare_to", note="Optional retained comparison source."),
        _form(
            "selector",
            "positions",
            "section",
            "around",
            "depth",
            "aggregate",
            "representation",
            "node_offset",
            "edge_offset",
            "node_limit",
            "edge_limit",
            "max_nodes",
            "max_edges",
            "timeout_seconds",
            "max_bytes",
        ),
        note="Reads a bounded retained graph offline; missing evidence and clipping stay explicit.",
    ),
    _command(
        "scan-sitemap-generate",
        "scan_sitemap_generate",
        _form("scan_artifact", "input_path", required_with=("out_dir", "sitemap_base_url")),
        _form(
            "local_directory", "out_dir", note="Fresh exclusive output directory; never deploys."
        ),
        _form("selector", "sitemap_base_url", "include_images", "exclude"),
        note="Deployment base URL sets output scope; it is never fetched.",
    ),
    _command(
        "log-scan", "log_scan", _form("legacy_directory", "run"), _form("scan_artifact", "run")
    ),
    _command(
        "crawl-diagnose",
        "crawl_diagnose",
        _form("scan_artifact", "scan"),
        _form("legacy_directory", "run"),
        note="Choose one retained source; diagnosis is offline and read-only.",
    ),
    _command(
        "crawl-diagnose-export",
        "crawl_diagnose_export",
        _form("scan_artifact", "scan", required_with=("export",)),
        _form("legacy_directory", "run", required_with=("export",)),
        _form("local_file", "export"),
        note="Choose one retained source and a new redacted export destination; refuses overwrite.",
    ),
    _command(
        "compare-crawls",
        "compare_crawls",
        _form("audit_document", "before", "after", note="Each path may be audit JSON or scan.v1."),
        _form(
            "local_file",
            "correspondence",
            note="Optional closed url-correspondence.v1 JSON declaration; exact URL comparison remains the default.",
        ),
        _form(
            "local_directory",
            "out_dir",
            note="Optional new compare.v2 package; required for audit.v2 populations above 10,000 pages or issues. Refuses overwrite.",
        ),
        _form(
            "selector",
            "compression",
            note="none (default) or explicit gzip with out_dir; every row is retained and compressed/uncompressed bytes are declared.",
        ),
    ),
    _command(
        "verify-fixes",
        "verify_fixes",
        _form("audit_document", "baseline", required_with=("out_dir",)),
        _form(
            "audit_document",
            "after",
            required_with=("baseline", "out_dir"),
            note="Offline verification without recrawling.",
        ),
        _form("selector", "finding_ids", required_with=("baseline", "out_dir")),
        _form(
            "local_file",
            "view",
            required_with=("baseline", "out_dir"),
            note="Saved verification_view.v1 selection.",
        ),
        _form("url_list", "urls", required_with=("baseline", "out_dir")),
        _form("local_file", "urls_file", required_with=("baseline", "out_dir")),
        _form("local_config", "config", note="Required when the baseline redacted credentials."),
    ),
    _command(
        "crawl-enrich",
        "crawl_enrich",
        _form("audit_document", "audit", required_with=("external_csv",)),
        _form("local_file", "external_csv", required_with=("audit",)),
    ),
    _command(
        "crawl-import",
        "crawl_import",
        _form(
            "local_file",
            "manifest_path",
            note="third_party_crawl_manifest.v1 with manifest-relative CSV datasets; output remains foreign crawl evidence",
        ),
    ),
    _command("segment-diff", "segment_diff", _form("audit_document", "audit")),
    _command("redirects-generate", "redirects_generate", _form("inline_json", "redirects")),
    _command("redirects-check", "redirects_check", _form("live_url", "url")),
    _command(
        "sitemap-crawl",
        "sitemap_crawl",
        _form("live_url", "url"),
        _form(
            "project_directory",
            "project",
            note="Optional explicit local observation; same origin only. Declared URLs are not fetched HTML pages.",
        ),
    ),
    _command("images-download", "images_download", _form("url_list", "urls")),
    _command("images-optimize", "images_optimize", _form("local_file", "files")),
    _command("keywords-cluster", "keywords_cluster", _form("inline_json", "params")),
    _command("robots-check", "robots_check", _form("live_url", "url")),
    _command("headers-check", "headers_check", _form("live_url", "url")),
    _command("asset-weight-check", "asset_weight_check", _form("live_url", "url")),
    _command("links-check", "links_check", _form("live_url", "url")),
    _command("hreflang-check", "hreflang_check", _form("live_url", "url")),
    _command("domain-profile", "domain_profile", _form("domain", "domain")),
    _command("cdn-check", "cdn_check", _form("live_url", "url")),
    _command("tech-detect", "tech_detect", _form("live_url", "url")),
    _command("security-check", "security_check", _form("live_url", "url")),
    _command(
        "backlinks-check",
        "backlinks_check",
        _form("domain", "target", required_with=("donors",)),
        _form("url_list", "donors", required_with=("target",)),
        _form("local_file", "donors_file", required_with=("target",)),
    ),
    _command(
        "schema-check",
        "schema_check",
        _form("live_url", "url"),
        _form("inline_html", "html"),
    ),
    _command(
        "schema-build",
        "schema_build",
        _form("live_url", "url"),
        _form("inline_html", "html"),
    ),
    _command(
        "duplicate-check",
        "duplicate_check",
        _form("inline_corpus", "items"),
        _form("scan_artifact", "scan"),
    ),
    _command(
        "ai-bots-check",
        "ai_bots_check",
        _form("live_url", "url"),
        _form("inline_text", "robots_text"),
    ),
    _command("mirror-check", "mirror_check", _form("live_url", "url")),
    _command("llms-txt-check", "llms_txt_check", _form("live_url", "url")),
    _command(
        "citability-check",
        "citability_check",
        _form("live_url", "url"),
        _form("inline_text", "text"),
    ),
    _command(
        "markdown-extract",
        "markdown_extract",
        _form("live_url", "url"),
        _form("inline_html", "html"),
    ),
    _command(
        "boilerplate-report",
        "boilerplate_report",
        _form("inline_corpus", "pages"),
        _form("scan_artifact", "scan"),
    ),
    _command(
        "semantic-inputs",
        "semantic_inputs",
        _form("inline_corpus", "items"),
        _form("scan_artifact", "scan"),
    ),
    _command(
        "semantic-similarity",
        "semantic_similarity",
        _form(
            "inline_corpus",
            "items",
            required_with=("embeddings", "adapter", "cache_path"),
            note="Supplied normalized-vector evidence; no model is loaded or called.",
        ),
        _form(
            "scan_artifact",
            "scan",
            required_with=("embeddings", "adapter", "cache_path"),
            note="Uses the retained semantic corpus and its recorded normalization policy.",
        ),
        _form("inline_json", "embeddings", "adapter"),
        _form("local_file", "cache_path", note="Local SQLite embedding cache."),
        _form("selector", "threshold", "max_candidate_comparisons"),
    ),
    _command(
        "meta-description-drafts",
        "meta_description_drafts",
        _form("inline_corpus", "items", note="Dry-run needs only supplied page HTML."),
        _form("scan_artifact", "scan", note="Uses retained normalized page content offline."),
        _form("inline_json", "context", note="Optional versioned site instructions."),
        _form(
            "inline_json",
            "drafts",
            "executor",
            required_with=("checkpoint_path",),
            note="Optional structured caller/delegated-agent results; no provider call.",
        ),
        _form("local_file", "checkpoint_path", "json_path", "csv_path"),
        _form("selector", "batch_size"),
    ),
    _command(
        "ai-column",
        "ai_column",
        _form("inline_corpus", "items", note="Dry-run needs only supplied normalized page text."),
        _form("scan_artifact", "scan", note="Uses retained normalized page content offline."),
        _form("inline_text", "prompt", note="Required instruction, at most 2000 characters."),
        _form(
            "inline_json",
            "urls",
            "rows",
            note="Optional URL selection and caller-supplied values; no provider call.",
        ),
        _form("local_file", "csv_path", note="Formula-safe CSV of the column values."),
        _form("selector", "column", "max_pages"),
    ),
    _command(
        "social-meta-check",
        "social_meta_check",
        _form("live_url", "url"),
        _form("inline_json", "og", "twitter"),
    ),
    _command("soft404-check", "soft404_check", _form("live_url", "url")),
    _command(
        "log-analyze",
        "log_analyze",
        _form("local_log", "path"),
        _form("selector", "verify_bots", "max_lines", "offset", "limit", "max_dns", "bot", "ip"),
    ),
    _command(
        "logs-import",
        "logs_import",
        _form("local_log", "path"),
        _form("project_directory", "project"),
        _form("selector", "max_lines", "max_files"),
    ),
    _command(
        "logs-summary",
        "logs_summary",
        _form("local_file", "source"),
        _form(
            "selector",
            "view",
            "limit",
            "offset",
            "bot",
            "family",
            "ip",
            "date_from",
            "date_to",
            "status",
            "section",
        ),
    ),
    _command(
        "logs-join",
        "logs_join",
        _form("local_file", "source"),
        _form("scan_artifact", "scan"),
        _form(
            "selector",
            "view",
            "base_url",
            "family",
            "bot",
            "date_from",
            "date_to",
            "limit",
            "offset",
        ),
    ),
    _command(
        "logs-bots",
        "logs_bots",
        _form("local_file", "source"),
        _form(
            "selector",
            "verify_bots",
            "limit",
            "offset",
            "bot",
            "family",
            "ip",
            "verdict",
            "max_dns",
        ),
    ),
    _command("regions-check", "regions_check", _form("live_url", "url")),
    _command(
        "render-check",
        "render_check",
        _form("live_url", "url"),
        _form(
            "inline_json",
            "transport_config",
            note="Optional local/remote Playwright transport selection; endpoint is named by environment variable.",
        ),
    ),
    _command("site-audit", "site_audit", _form("live_url", "url"), _form("url_list", "urls")),
    _command(
        "report-build",
        "report_build",
        _form("audit_document", "audit", note="Audit JSON or a retained scan.v1 artifact."),
        _form(
            "project_directory",
            "project",
            note="Includes validated checklist coverage and optionally applies a saved finding view.",
        ),
        _form("selector", "view", note="Optional saved project finding view; requires project."),
        _form("selector", "offset", note="Optional stable finding-view page offset."),
        _form(
            "selector",
            "pdf_policy",
            note="Explicit overview-v1 for PDF + retained audit.v2 only: bounded PDF with complete mandatory JSON/CSV/manifest companions; never inferred.",
        ),
    ),
    _command("facts-export", "facts_export", _form("inline_json", "sites")),
    _command(
        "marketing-inventory",
        "marketing_inventory",
        _form("inline_json", "documents"),
        _form("selector", "cta_selector", "form_selector", "id_attributes", "id_parameters"),
        _form("local_directory", "out_dir"),
    ),
    _command("keywords-expand", "keywords_expand", _form("provider_query", "phrase")),
    _command("keywords-seasonality", "keywords_seasonality", _form("provider_query", "phrase")),
    _command("keywords-exact", "keywords_exact", _form("provider_query", "keywords")),
    _command(
        "serp-fetch",
        "serp_fetch",
        _form("provider_query", "query"),
        _form("provider_query", "queries"),
    ),
    _command(
        "spend-report", "spend_report", _form("local_log", note="Configured local spend log.")
    ),
    _command("sources-doctor", "sources_doctor", _form("local_config")),
    _command(
        "sources-sync",
        "sources_sync",
        _form("provider_query", "source", "resource"),
        _form("local_file", "db"),
        _form("project_directory", "project"),
        note="Explicit provider read and local SQLite write; records requested coverage and replaces eligible days atomically.",
    ),
    _command(
        "sources-status",
        "sources_status",
        _form("local_file", "db"),
        _form("project_directory", "project"),
    ),
    _command(
        "sources-export",
        "sources_export",
        _form("local_file", "db"),
        _form("project_directory", "project"),
    ),
    _command("regions-tree", "regions_tree", _form("local_config")),
    _command(
        "topvisor-read",
        "topvisor_read",
        _form(
            "inline_json",
            "operation",
            "params",
            note="One bounded page of existing Topvisor data. Default operation is projects; other operations require params.project_id. Follow the provider's nextOffset for further pages. No paid checks or mutations.",
        ),
    ),
    _command("metrika-counters", "metrika_counters", _form("local_config")),
    _command("metrika-setup", "metrika_setup", _form("provider_query", "counter_id")),
    _command("metrika-report", "metrika_report", _form("provider_query", "counter_id")),
    _command(
        "metrika-traffic-pdf",
        "metrika_traffic_pdf",
        _form("provider_query", "counter_id", required_with=("date1", "date2", "out_dir")),
        _form(
            "local_file",
            "document",
            required_with=("out_dir",),
            note="Renders an existing traffic document offline; no Metrika request.",
        ),
        _form("local_config", "brand", note="Optional brand JSON file or inline object."),
        _form("inline_json", "gsc_rows", note="Optional Search Console query rows."),
    ),
    _command(
        "google-keywords",
        "google_keywords",
        _form("provider_query", "keywords"),
        _form("provider_query", "seed"),
    ),
    _command(
        "dataforseo-estimate",
        "dataforseo_estimate",
        _form(
            "inline_json",
            "operation",
            "query_count",
            "row_limit",
            "cache_hits",
            "prices",
            "env",
            "query",
        ),
    ),
    _command(
        "backlinks-profile",
        "backlinks_profile",
        _form("provider_query", "domain"),
        _form("inline_json", "imports", "evidence", "sources"),
        _form("local_file", "db"),
        _form("project_directory", "project"),
    ),
    _command(
        "backlinks-disavow",
        "backlinks_disavow",
        _form("url_list", "urls", "domains"),
        _form("local_file", "output"),
    ),
    _command("google-serp", "google_serp", _form("provider_query", "query")),
    _command("wayback-history", "wayback_history", _form("live_url", "url")),
    _command("cloudflare-traffic", "cloudflare_traffic", _form("provider_query", "zone")),
    _command("crtsh-subdomains", "crtsh_subdomains", _form("domain", "domain")),
    _command("gsc-query", "gsc_query", _form("provider_query", "site_url")),
    _command(
        "webmaster-url-queries",
        "webmaster_url_queries",
        _form("provider_query", "host_id", note="Bounded Yandex URL-to-query evidence."),
    ),
    _command(
        "miratext-analyze",
        "miratext_analyze",
        _form("inline_json", note="Miratext analysis; paid modes require confirmation."),
    ),
    _command(
        "gsc-archive",
        "gsc_archive",
        _form(
            "local_file",
            "database",
            note="Explicit SQLite archive. Only prepare creates a missing file; status and backup are offline.",
        ),
        _form(
            "inline_json",
            "database",
            "action",
            "site_url",
            "start_date",
            "end_date",
            "max_requests",
            "pause",
            "backup_path",
            note="prepare queues inclusive dates; run makes bounded Google API calls; backup requires a new destination.",
        ),
    ),
    _command(
        "crux-report",
        "crux_report",
        _form("provider_query", "url"),
        _form("provider_query", "origin"),
    ),
    _command("indexnow-submit", "indexnow_submit", _form("url_list", "urls")),
    _command(
        "scan-list",
        "scan_list",
        _form("legacy_directory", "directory"),
        _form("project_directory", "project"),
    ),
    _command(
        "project-new",
        "project_new",
        _form("project_directory", "directory"),
        _form("live_url", "target"),
    ),
    _command("project-open", "project_open", _form("project_directory", "directory")),
    _command(
        "project-sources-link",
        "project_sources_link",
        _form("project_directory", "directory"),
        note="Binds a provider resource ID (--service, --resource, optional --label) to the project in sources.json; no network.",
    ),
    _command(
        "project-sources-unlink",
        "project_sources_unlink",
        _form("project_directory", "directory"),
        note="Removes one (--service, --resource) binding from sources.json; no network.",
    ),
    _command(
        "project-sources-list",
        "project_sources_list",
        _form("project_directory", "directory"),
    ),
    _command("project-status", "project_status", _form("project_directory", "directory")),
    _command(
        "project-progress",
        "project_progress",
        _form("project_directory", "directory"),
        _form("inline_json", "limit", "offset", note="Optional bounded progress pagination."),
    ),
    _command(
        "project-observe",
        "project_observe",
        _form("project_directory", "directory"),
        _form("selector", "consumer", note="Optional scoped unread-notice recipient."),
        _form("inline_json", "scan_limit", note="Bounded retained scan-history page."),
        _form(
            "inline_json",
            "run_offset",
            "run_limit",
            note="Per-site terminal run page; all stored running records remain visible.",
        ),
    ),
    _command(
        "project-inbox-submit",
        "project_inbox_submit",
        _form("project_directory", "directory"),
        _form("inline_text", "text"),
        _form("selector", "kind", "references", "author_role"),
    ),
    _command(
        "project-inbox-list",
        "project_inbox_list",
        _form("project_directory", "directory"),
        _form("selector", "consumer", "offset", "limit", "include_acknowledged"),
    ),
    _command(
        "project-inbox-read",
        "project_inbox_read",
        _form("project_directory", "directory"),
        _form("selector", "consumer", "entry_ids", "expected_revision"),
    ),
    _command(
        "project-inbox-acknowledge",
        "project_inbox_acknowledge",
        _form("project_directory", "directory"),
        _form("selector", "consumer", "entry_ids", "expected_revision"),
    ),
    _command(
        "project-inbox-goal",
        "project_inbox_goal",
        _form("project_directory", "directory"),
        _form("selector", "entry_id", "state", "expected_revision"),
    ),
    _command(
        "project-inbox-triage",
        "project_inbox_triage",
        _form("project_directory", "directory"),
        _form(
            "inline_json",
            "entry_id",
            "outcome",
            "actor",
            "expected_revision",
            note="Explicit controller outcome for a specialist note; it never reads, acknowledges, or executes the note.",
        ),
    ),
    _command(
        "project-inbox-unread",
        "project_inbox_unread",
        _form("project_directory", "directory"),
        _form("selector", "consumer", "limit"),
    ),
    _command(
        "remediation-create",
        "remediation_create",
        _form("local_file", "path", note="New ledger path; an existing file is refused."),
        _form("project_directory", "project_dir"),
        _form("selector", "producer_build", note="Full lowercase 40-character Git SHA."),
    ),
    _command(
        "remediation-ingest",
        "remediation_ingest",
        _form("local_file", "ledger"),
        _form("local_file", "scan", note="Read-only saved scan.v1 SQLite artifact."),
    ),
    _command(
        "project-event-append",
        "project_event_append",
        _form("project_directory", "directory"),
        _form("selector", "source", "actor"),
        _form("inline_text", "text"),
    ),
    _command(
        "project-event-page",
        "project_event_page",
        _form("project_directory", "directory"),
        _form("selector", "source", "offset", "limit", "query"),
    ),
    _command("remediation-summary", "remediation_summary", _form("local_file", "ledger")),
    _command(
        "remediation-cases",
        "remediation_cases",
        _form("local_file", "ledger"),
        _form(
            "selector",
            "check",
            "url",
            "finding_key",
            "limit",
            "offset",
            "source_scan_id",
            "group_ref",
            "max_bytes",
            note="Use source_scan_id and group_ref together for a complete ordered group member page; mutually exclusive with finding selectors.",
        ),
    ),
    _command(
        "remediation-transition",
        "remediation_transition",
        _form("local_file", "ledger"),
        _form("selector", "occurrence_key", "state", "actor", "reason", "expected_revision"),
        _form("inline_json", "observation_id", "decided_at"),
    ),
    _command(
        "remediation-record-verification",
        "remediation_record_verification",
        _form("local_file", "ledger", "verification_path"),
        _form("selector", "actor", "expected_revision"),
    ),
    _command(
        "remediation-recheck",
        "remediation_recheck",
        _form("local_file", "ledger", "baseline", "out_dir"),
        _form(
            "audit_document",
            "after",
            note="Optional retained later audit for offline verification.",
        ),
        _form("selector", "occurrence_keys", "actor", "expected_revision", "task_id"),
        _form("local_config", "config", note="Required when the baseline redacted credentials."),
    ),
    _command(
        "remediation-report",
        "remediation_report",
        _form("local_file", "ledger"),
        _form("local_directory", "out_dir"),
        _form("selector", "limit", "offset"),
    ),
    _command(
        "project-facts",
        "project_facts",
        _form("project_directory", "directory"),
        _form(
            "inline_json",
            "facts",
            note="Operator-entered facts; preview by default, recorded with apply.",
        ),
        note="detect fetches the project's own target once after robots.txt; it is never implicit.",
    ),
    _command(
        "project-checklist-init",
        "project_checklist_init",
        _form("project_directory", "directory"),
        _form("inline_json", "template", note="Optional reusable data-only checklist template."),
        _form(
            "inline_json",
            "plan",
            note="Optional agreed scope plan fixing the URL-population and task denominators.",
        ),
    ),
    _command(
        "project-checklist-update",
        "project_checklist_update",
        _form("project_directory", "directory"),
        _form("inline_json", "item", note="Requires expected_revision for optimistic concurrency."),
    ),
    _command(
        "project-checklist-record",
        "project_checklist_record",
        _form("project_directory", "directory"),
        _form("selector", "item_id"),
        _form(
            "inline_json",
            "record",
            note="Requires expected_revision; records supplied evidence only. Automatic checks "
            "take status succeeded, reason and a saved scan artifact; manual items take a reviewer "
            "with signoff or an approved reviewed artifact; not_applicable needs reason, reviewer "
            "and an evidence basis.",
        ),
    ),
    _command("project-view-list", "project_view_list", _form("project_directory", "directory")),
    _command(
        "project-view-show",
        "project_view_show",
        _form("project_directory", "directory"),
        _form("selector", "name"),
    ),
    _command(
        "project-view-save",
        "project_view_save",
        _form("project_directory", "directory"),
        _form("inline_json", "view"),
        _form("selector", "expected_revision", note="Required; use 0 for the first saved view."),
    ),
    _command(
        "project-view-delete",
        "project_view_delete",
        _form("project_directory", "directory"),
        _form("selector", "name"),
        _form("selector", "expected_revision", note="Required; current view config revision."),
    ),
    _command(
        "project-view-rename",
        "project_view_rename",
        _form("project_directory", "directory"),
        _form("selector", "name"),
        _form("selector", "new_name"),
        _form("selector", "expected_revision", note="Required; current view config revision."),
    ),
    _command(
        "findings-view",
        "findings_view",
        _form("project_directory", "directory"),
        _form("selector", "name"),
        _form(
            "audit_document", "audit", note="Audit JSON, inline audit object, or retained scan.v1."
        ),
        _form("selector", "offset", note="Optional stable finding-view page offset."),
    ),
    _command(
        "project-priorities",
        "project_priorities",
        _form("project_directory", "directory"),
        _form(
            "inline_json",
            "policy",
            note="Optional data-only priority policy; preview by default. Apply requires expected_revision.",
        ),
    ),
    _command(
        "project-policy",
        "project_policy",
        _form("project_directory", "directory"),
        _form(
            "inline_json",
            "policy",
            note="Optional data-only policy; preview by default. Apply requires expected_revision.",
        ),
    ),
    _command(
        "project-prepare",
        "project_prepare",
        _form("project_directory", "directory"),
        _form("inline_json", "template", note="Optional data-only project template."),
        _form("inline_json", "competitors", note="Optional bounded competitor inputs."),
    ),
    _command(
        "project-start",
        "project_start",
        _form("project_directory", "directory"),
        _form("live_url", "target"),
        _form("inline_json", "facts", note="Optional supplied project facts."),
        _form("inline_json", "template", note="Optional data-only project template."),
        _form("inline_json", "competitors", note="Optional bounded competitor inputs."),
    ),
    _command(
        "project-competitors-add",
        "project_competitors_add",
        _form("project_directory", "directory"),
        _form(
            "inline_json",
            "competitors",
            note="Required list of candidates with url, source and observed_at.",
        ),
    ),
    _command(
        "schedule-show",
        "schedule_show",
        _form("project_directory", "directory"),
    ),
    _command(
        "schedule-set",
        "schedule_set",
        _form("project_directory", "directory"),
        _form("selector", "expected_revision", note="Required; current schedule revision."),
        _form("inline_json", "settings", note="Partial schedule.json settings to merge."),
    ),
    _command(
        "schedule-run-due",
        "schedule_run_due",
        _form("project_directory", "directory"),
    ),
    _command(
        "schedule-install",
        "schedule_install",
        _form("project_directory", "directory"),
        _form("selector", "kind", note="launchd, cron or windows."),
        _form("local_file", "dest", "python"),
    ),
    _command(
        "schedule-uninstall",
        "schedule_uninstall",
        _form("project_directory", "directory"),
        _form("selector", "kind"),
        _form("local_file", "dest"),
    ),
    _command(
        "project-competitors-list",
        "project_competitors_list",
        _form("project_directory", "directory"),
    ),
    _command(
        "project-competitors-remove",
        "project_competitors_remove",
        _form("project_directory", "directory"),
        _form("live_url", "url", note="The recorded competitor URL to drop from the shortlist."),
    ),
    _command("skill-list", "skill_list", _form("no_input")),
    _command("skill-show", "skill_show", _form("selector", "name")),
    _command("scenario-list", "scenario_list", _form("no_input")),
    _command("scenario-show", "scenario_show", _form("selector", "name")),
    _command("provider-registry", "provider_registry", _form("no_input")),
    _command(
        "provider-readiness",
        "provider_readiness",
        _form(
            "inline_json",
            "provider",
            "operation",
            note="Offline readiness and operation discovery; no provider requests.",
        ),
    ),
    _command(
        "provider-verify",
        "provider_verify",
        _form("provider_identifier", "provider"),
        _form("inline_json", "request", note="Optional read-only target-access request."),
    ),
    _command(
        "provider-collect",
        "provider_collect",
        _form("provider_identifier", "provider"),
        _form("selector", "operation"),
        _form("inline_json", "request"),
        _form("local_directory", "artifact_dir", note="Optional restricted raw-evidence location."),
    ),
    _command(
        "provider-join",
        "provider_join",
        _form("inline_json", "crawl_pages"),
        _form("inline_json", "evidence_rows"),
        _form("inline_json", "adjustments", note="Optional evidence-backed priority adjustments."),
    ),
    _command(
        "evidence-normalize",
        "evidence_normalize",
        _form(
            "local_file",
            "file",
            note="Supplied CSV/XLSX/JSON rows or a saved provider-evidence envelope; fully offline.",
        ),
        _form(
            "inline_json",
            "mapping",
            note="Optional seohead.evidence-mapping.v1 manifest, inline or file path.",
        ),
        _form(
            "local_directory",
            "out_dir",
            note="Optional restricted normalized artifact directory.",
        ),
    ),
    _command(
        "evidence-join",
        "evidence_join",
        _form(
            "scan_artifact",
            "scan",
            note="Alternative crawl side; offline scan read like provider-replay.",
        ),
        _form("audit_document", "audit", note="Alternative crawl side."),
        _form("inline_json", "pages", note="Alternative crawl side, page objects."),
        _form(
            "local_file",
            "evidence",
            "compare",
            note="CSV/XLSX/JSON or saved provider envelope; inline JSON also accepted.",
        ),
        _form(
            "inline_json",
            "mapping",
            "compare_mapping",
            "policy",
            note="Mapping manifests and the declared comparison policy, inline or file path.",
        ),
        _form(
            "local_directory",
            "out_dir",
            note="Optional private join/compatibility artifact directory.",
        ),
    ),
    _command(
        "bi-export",
        "bi_export",
        _form("scan_artifact", "scan", note="Alternative validated scan.v1 source."),
        _form("audit_document", "audit", note="Alternative supported audit JSON source."),
        _form(
            "local_file",
            "provider_joins",
            note="Optional saved issue #781 evidence-join/normalized-evidence JSON files; repeatable.",
        ),
        _form(
            "local_directory",
            "out_dir",
            note="Required new local package directory; existing output is refused.",
        ),
        _form(
            "inline_json",
            "max_rows_per_file",
            "max_bytes_per_file",
            "max_output_bytes",
            "max_scan_bytes",
            "search_metric",
            "xlsx_max_rows_per_sheet",
            note="Optional positive partition/total-output bounds and explicit Search Console clicks or impressions axis; exceeding a hard limit fails without publishing a package.",
        ),
        _form(
            "local_file",
            "xlsx_out",
            required_with=("xlsx_dataset",),
            note="Optional new split XLSX consumer output from one verified package dataset.",
        ),
        _form("selector", "xlsx_dataset", required_with=("xlsx_out",)),
        note="Reads saved artifacts only; no provider calls, crawl, or remote writes.",
    ),
    _command(
        "publication-cohorts",
        "publication_cohorts",
        _form("inline_json", "document", note="seohead.publication-cohort-input.v1 document."),
        _form("local_file", "file", note="Alternative versioned offline cohort input JSON."),
        _form(
            "local_directory",
            "out_dir",
            note="Required new local package directory; existing output is refused.",
        ),
        note="Reads saved normalized evidence only; no provider calls or causal inference.",
    ),
    _command(
        "gsc-progress",
        "gsc_progress",
        _form("inline_json", "document", note="seohead.gsc-progress-input.v1 document."),
        _form("local_file", "file", note="Alternative versioned offline GSC input JSON."),
        _form(
            "local_directory",
            "out_dir",
            note="Required new local package directory; existing output is refused.",
        ),
        note="Reads saved normalized GSC evidence only; no provider calls or rank-placement claims.",
    ),
    _command(
        "bi-sheets-plan",
        "bi_sheets_plan",
        _form("local_directory", "package"),
        _form(
            "inline_json", "max_cells", note="Optional declared capacity, never an API quota check."
        ),
        note="Offline package/checksum/capacity preflight; no Google authentication or write.",
    ),
    _command(
        "bi-bigquery-plan",
        "bi_bigquery_plan",
        _form("local_directory", "package"),
        _form("selector", "dataset", "operation"),
        note="Offline optional-load plan; no project selection, billing, authentication, or write.",
    ),
    _command(
        "bi-destination-apply",
        "bi_destination_apply",
        _form("local_directory", "package"),
        _form("selector", "target", "destination", "operation"),
        _form("inline_json", "apply", note="Requires true and a host-injected authorized client."),
        _form(
            "inline_json",
            "reconcile",
            note="Explicitly reconciles a retained uncertain request before any retry; requires apply=true.",
        ),
        note="Never accepts credentials; a missing host client fails before any destination write.",
    ),
    _command(
        "inspect-url",
        "inspect_url",
        _form("live_url", "url"),
        _form(
            "inline_json",
            "checks",
            note="Optional bounded selection of closed investigation checks.",
        ),
    ),
    _command(
        "audit-workflow",
        "audit_workflow",
        _form("project_directory", "directory"),
        _form("selector", "action", note="status, start, prepare, or report."),
        _form("live_url", "target", note="Required only for action=start."),
        _form(
            "audit_document",
            "audit",
            note="Optional for action=report; omitted, the project's latest complete audited scan is used.",
        ),
    ),
    _command("workflow-status", "workflow_status", _form("project_directory", "directory")),
    _command(
        "workflow-start",
        "workflow_start",
        _form("project_directory", "directory", required_with=("scenario_id", "steps")),
    ),
    _command(
        "workflow-checkpoint",
        "workflow_checkpoint",
        _form(
            "project_directory",
            "directory",
            required_with=("run_id", "step_id", "state", "expected_revision"),
        ),
    ),
    _command(
        "workflow-execute",
        "workflow_execute",
        _form("project_directory", "directory", required_with=("scenario_id", "steps", "outcomes")),
    ),
    _command(
        "workflow-resume",
        "workflow_resume",
        _form("project_directory", "directory", required_with=("run_id", "expected_revision")),
    ),
    _command("monitor-status", "monitor_status", _form("project_directory", "directory")),
    _command(
        "monitor-configure",
        "monitor_configure",
        _form("project_directory", "directory", required_with=("policy",)),
    ),
    _command(
        "monitor-run",
        "monitor_run",
        _form(
            "project_directory",
            "directory",
            required_with=("scan_id", "observations", "expected_revision"),
        ),
        note="Records supplied retained-scan differences only; it starts no schedule or delivery.",
    ),
    _command(
        "monitor-collect",
        "monitor_collect",
        _form("project_directory", "directory", required_with=("expected_revision",)),
        _form("selector", "expected_revision", "apply"),
        note="Preview by default; explicit apply collects only an existing validated bounded claim. No timer or delivery starts.",
    ),
    _command(
        "monitor-local-deliver",
        "monitor_local_deliver",
        _form("project_directory", "directory", required_with=("scan_id", "expected_revision")),
        _form("selector", "scan_id", "expected_revision"),
        note="Records a local:receipt only; no network, external transport or recipient delivery claim.",
    ),
    _command(
        "monitor-schedule",
        "monitor_schedule",
        _form(
            "project_directory",
            "directory",
            required_with=("action", "expected_revision"),
            note="Claims, cancels, backs off, or recovers a local bounded pass; it starts no timer.",
        ),
    ),
    _command(
        "tool-catalog",
        "tool_catalog",
        _form("inline_text", "query", note="Optional bounded discovery query."),
    ),
    _command("scan-inspect", "scan_inspect", _form("scan_artifact", "input_path")),
    _command(
        "scan-content-search",
        "scan_content_search",
        _form("scan_artifact", "input_path"),
        _form(
            "inline_text",
            "query",
            note="Offline retained-content query; package output is bounded and source-identified.",
        ),
    ),
    _command(
        "scan-content-search-page",
        "scan_content_search_page",
        _form(
            "local_file",
            "package",
            note="Read one bounded page from a prior offline search package.",
        ),
    ),
    _command(
        "scan-url-detail",
        "scan_url_detail",
        _form("scan_artifact", "input_path"),
        _form("selector", "url", note="Exact retained native URL; output redacts query values."),
    ),
    _command("scan-url-query", "scan_url_query", _form("scan_artifact", "input_path")),
    _command(
        "scan-url-history",
        "scan_url_history",
        _form("project_directory", "project"),
        _form("selector", "url", note="Exact retained URL text, as scan-url-detail takes it."),
    ),
    _command(
        "scan-link-inspect",
        "scan_link_inspect",
        _form(
            "scan_artifact",
            "input_path",
            note="Offline path, inlinks, occurrence context or one URL's paged links selected by view; mode-specific selectors and limits are required.",
        ),
    ),
    _command("scan-status", "scan_status", _form("scan_artifact", "input_path")),
    _command("scan-rendered-routes", "scan_rendered_routes", _form("scan_artifact", "input_path")),
    _command("scan-snapshot", "scan_snapshot", _form("scan_artifact", "input_path")),
    _command(
        "scan-export",
        "scan_export",
        _form(
            "scan_artifact",
            "input_path",
            note="Also accepts an SF Analyzer audit.json document; links are unavailable there.",
        ),
        _form(
            "selector",
            "records",
            "fields",
            note="Optional record-type and field projection validated before any file is written.",
        ),
    ),
    _command("scan-pin", "scan_pin", _form("scan_artifact", "input_path")),
    _command(
        "scan-prune",
        "scan_prune",
        _form("legacy_directory", "directory"),
        _form("local_file", "plan"),
        _form(
            "project_directory",
            "project",
            note="Defaults the directory to project scans/; apply remains explicit.",
        ),
    ),
    _command(
        "scan-body-diff",
        "scan_body_diff",
        _form("scan_artifact", "left", "right"),
        _form("selector", "url", note="Selects the logical URL within both scans."),
    ),
    _command(
        "scan-evidence",
        "scan_evidence",
        _form("scan_artifact", "input_path"),
        _form(
            "selector",
            "section",
            note="capabilities, corpus, structured, routes, resources, or timeline.",
        ),
    ),
    _command(
        "scan-extract",
        "scan_extract",
        _form("scan_artifact", "input_path"),
        _form(
            "inline_json",
            "rules",
            note="Closed declarative rules over retained complete bodies.",
        ),
        _form("selector", "url", note="Optional exact logical URL."),
    ),
    _command(
        "scan-structured-blocks",
        "scan_structured_blocks",
        _form("scan_artifact", "input_path"),
        _form("selector", "url", note="Exact retained logical URL."),
        _form("selector", "representation", note="static, rendered, or legacy_fragment source."),
        _form("selector", "variant_key", note="Optional exact retained HTTP variant key."),
    ),
    _command(
        "scan-fragment-links",
        "scan_fragment_links",
        _form("scan_artifact", "input_path"),
        _form(
            "selector",
            "state",
            note="Optional resolved, missing, or skipped occurrence filter.",
        ),
        _form(
            "selector",
            "representation",
            note="Optional static, rendered, or legacy_fragment source filter.",
        ),
    ),
    _command(
        "scan-requeue",
        "scan_requeue",
        _form("scan_artifact", "input_path"),
        _form("selector", "where", note="Restricted saved URL/page predicate."),
        _form("local_file", "backup_path", note="Mandatory new verified backup destination."),
        _form("scan_artifact", "from_scan", note="Optional alternate saved selection source."),
    ),
    _command(
        "scan-import-urls",
        "scan_import_urls",
        _form("scan_artifact", "input_path"),
        _form("local_file", "urls_file", note="Explicit TXT, CSV, XLSX, or XML URL source."),
        _form("local_file", "backup_path", note="Mandatory new verified backup destination."),
    ),
)


SF_CONTRACTS: tuple[CommandContract, ...] = (
    _command(
        "sf run",
        None,
        _form("live_url", "crawl"),
        _form("local_file", "load_crawl", note="Saved .seospider crawl; requires licensed SF CLI"),
        _form("local_file", "crawl_list", note="URL-list file for licensed SF live traversal"),
        _form("legacy_directory", "exports_dir"),
        _form(
            "local_config",
            "config",
            note="SF audit configuration; semantic_similarity requires an explicit adapter declaration and supplied vectors. No model is selected by default.",
        ),
        _form("local_file", "auth_config", note="Optional SF authentication profile."),
        _form(
            "inline_text",
            "auth",
            note="Optional HTTP Basic credentials; do not persist or log them.",
        ),
        _form("local_file", "sf_cli", note="Optional explicit licensed SF CLI path."),
        _form("live_url", "sitemap", note="Optional explicit sitemap URL for live rechecks."),
        _form("local_directory", "out", note="Local audit and optional task output directory."),
    ),
    _command(
        "sf tasks",
        None,
        _form("audit_document", "audit_json", note="Audit JSON or a retained scan.v1 artifact."),
        _form("local_config", "config"),
        _form("local_directory", "out", note="Local task output directory."),
    ),
    _command(
        "sf doctor",
        None,
        _form("local_config", "config"),
        _form("local_file", "sf_cli", note="Optional explicit licensed SF CLI path."),
    ),
    _command("sf save-config", None, _form("local_file", "out")),
    _command(
        "mcp",
        None,
        _form("no_input"),
        note="Starts the local stdio server; profile and progress-notification behavior are startup options.",
    ),
)


CONTRACTS = COMMAND_CONTRACTS + SF_CONTRACTS

_KIND_LABELS = {
    "live_url": "Live URL",
    "url_list": "URL list",
    "domain": "Domain",
    "scan_artifact": "Scan artifact",
    "audit_document": "Audit document",
    "legacy_directory": "Local directory",
    "local_file": "Local file",
    "local_log": "Local log",
    "inline_corpus": "Inline corpus",
    "inline_json": "Inline JSON",
    "inline_html": "Inline HTML",
    "inline_text": "Inline text",
    "selector": "Selector",
    "provider_query": "Provider query",
    "provider_identifier": "Provider identifier",
    "local_config": "Local configuration",
    "local_directory": "Local directory",
    "project_directory": "Project directory",
    "no_input": "No direct input",
}


def coverage_gaps(
    commands: Collection[str], sf_subcommands: Collection[str]
) -> tuple[set[str], set[str]]:
    """Return public commands missing from, or stale in, this catalogue.

    Callers provide interface registries so package runtime never imports CLI or
    server modules merely to expose its input metadata.
    """
    expected = set(commands) | {f"sf {name}" for name in sf_subcommands} | {"mcp"}
    catalogued = {contract.command for contract in CONTRACTS}
    return expected - catalogued, catalogued - expected


def render_markdown() -> str:
    """Render the checked-in public input reference."""
    lines = [
        "# Command inputs",
        "",
        "This reference is generated from `seohead.core.input_contracts`. Each row inventories consumed",
        "source inputs rather than inferring them from a command's output. Forms on one row can be",
        "required together; the notes name those relationships.",
        "",
        "A **scan artifact** is a retained local `scan.v1` SQLite file. Read-only analysis and",
        "history operations do not replay a crawl or promise retained page bodies. `crawl-site --resume`",
        "is the explicit exception: it continues network collection. `duplicate-check` and",
        "`boilerplate-report` may instead read one retained scan corpus with `--scan`.",
        "",
        "## Operational-store decision",
        "",
        "`scan.v1` is the authoritative retained evidence store for an artifact-mode crawl.",
        "The HTTP cache remains sharded `http_cache.v3` files because concurrent crawl workers",
        "can independently replace one cache entry. In live cache modes a cache miss is a safe",
        "network fallback; a replay-mode miss remains offline and is reported. The run",
        "journal remains an append-only local `runs.jsonl` record. Neither store is a second scan corpus,",
        "and this decision makes no backend migration.",
        "",
        "## Catalogue",
        "",
        "| Command | Accepted input forms | Notes |",
        "| --- | --- | --- |",
    ]
    scan_commands = {
        "scan-inspect",
        "scan-url-query",
        "scan-link-inspect",
        "scan-status",
        "scan-rendered-routes",
        "scan-snapshot",
        "scan-pin",
        "scan-reanalyze",
        "scan-graph",
        "scan-sitemap-generate",
    }

    def display_arguments(contract: CommandContract, form: InputForm) -> str:
        return ", ".join(
            "scan" if contract.command in scan_commands and argument == "input_path" else argument
            for argument in form.arguments
        )

    for contract in CONTRACTS:
        forms = "<br>".join(
            f"{_KIND_LABELS[form.kind]} (`{display_arguments(contract, form)}`)"
            + (f"; requires `{', '.join(form.required_with)}`" if form.required_with else "")
            if form.arguments
            else _KIND_LABELS[form.kind]
            for form in contract.forms
        )
        notes = "; ".join(filter(None, [contract.note, *(form.note for form in contract.forms)]))
        lines.append(f"| `{contract.command}` | {forms} | {notes or '—'} |")
    lines.append("")
    return "\n".join(lines)
