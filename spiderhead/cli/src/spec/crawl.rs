//! crawl command declarations (the clap builder tree).

use clap::builder::Command;

use super::{c, lst, pos, sw, v, ArgExt, Py};

/// Every top-level command of the bucket, fully nested.
pub(crate) fn commands() -> Vec<Command> {
    vec![
        c("crawl-site", "Crawl a site from a start URL, or fetch an explicit list, then audit it.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("url", "--url", "target URL", Py::TEXT))
        .arg(sw("approve_large_crawl", "--approve-large-crawl", "approve exceeding project budgets"))
        .arg(v("user_agent", "--user-agent", "user agent preset or custom string", Py::TEXT))
        .arg(v("observer_run_id", "--observer-run-id", "==SUPPRESS==", Py::TEXT).hid())
        .arg(v("project", "--project", "project directory: default target and scan location", Py::TEXT))
        .arg(v("urls", "--urls", "comma-separated URL list: list mode, no discovery", Py::TEXT))
        .arg(v("urls_file", "--urls-file", "list mode: TXT/CSV/XLSX/XML URL file", Py::TEXT))
        .arg(v("max_urls", "--max-urls", "URL cap: 1..1,000,000; 0=full SQLite (default 200; with --project, unbounded 0 or over the default 1000-page threshold needs --approve-large-crawl)", Py::INT))
        .arg(v("retry_server_errors", "--retry-server-errors", "retry page-level 5xx up to N times (0-3, default 0); each try is a real request", Py::INT))
        .arg(v("out_dir", "--out-dir", "legacy directory output; disables default scan", Py::TEXT))
        .arg(v("scan_out", "--scan-out", "SQLite output: site crawl or URL list", Py::TEXT).vm("FILE"))
        .arg(v("resume", "--resume", "resume stored URL/settings; only rate overrides (--max-urls-per-second, --min-delay)", Py::TEXT).vm("FILE"))
        .arg(v("producer_build", "--producer-build", "original source build for SQLite capture", Py::TEXT).vm("SHA"))
        .arg(v("config", "--config", "path to a crawler config file (JSON)", Py::TEXT))
        .arg(v("profile", "--profile", "use a saved profile; --set still applies", Py::TEXT).vm("NAME"))
        .arg(v("save_profile", "--save-profile", "validate and save --config as NAME; does not crawl", Py::TEXT).vm("NAME"))
        .arg(v("robots", "--robots", "robots.txt policy", Py::TEXT.ch(&["respect", "report_only", "ignore"])))
        .arg(v("sitemap", "--sitemap", "seed/reconcile XML URLs; discovery: --config", Py::TEXT))
        .arg(sw("sitemap_only", "--sitemap-only", "SQLite: explicit --sitemap members only"))
        .arg(sw("config_help", "--config-help", "list every crawler configuration setting"))
        .arg(sw("quiet", "--quiet", "no crawl lines on stderr; stdout unchanged").sh('q'))
        .arg(v("max_urls_per_second", "--max-urls-per-second", "maximum request rate per host", Py::FLOAT).vm("N"))
        .arg(lst("set", "--set", "set dotted PATH=VALUE; repeatable (--config-help)").id_("set_settings").vm("PATH=VALUE"))
        .arg(v("max_depth", "--max-depth", "maximum link depth (legacy setting override)", Py::INT))
        .arg(v("min_delay", "--min-delay", "minimum delay between requests in seconds", Py::FLOAT)),
        c("crawl-control", "Pause, stop, change the rate of, or steer an active native project crawl.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("project", "--project", "project containing the active crawl", Py::TEXT))
        .arg(v("run_id", "--run-id", "active project run ID", Py::TEXT))
        .arg(v("action", "--action", "", Py::TEXT.ch(&["pause", "stop", "set_rate", "narrow-scope", "add-urls"])).req())
        .arg(v("rate", "--rate", "set_rate: new maximum URLs per second for the running crawl", Py::FLOAT).vm("N"))
        .arg(v("concurrency", "--concurrency", "set_rate: optional new concurrency ceiling", Py::INT).vm("N"))
        .arg(v("exclude", "--exclude", "narrow-scope: regex appended to the crawl's exclude patterns", Py::TEXT).vm("REGEX"))
        .arg(v("include", "--include", "narrow-scope: regex replacing the include allowlist", Py::TEXT).vm("REGEX"))
        .arg(v("urls", "--urls", "add-urls: comma-separated URLs to queue (max 16)", Py::TEXT).vm("URLS"))
        .arg(v("scan", "--scan", "steer this paused scan artifact instead of a live run", Py::TEXT).vm("FILE")),
        c("crawl-describe-settings", "Every crawl-site config setting: path, type, default, and description.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT)),
        c("crawl-diagnose", "Explain a native crawl from retained evidence without fetching the site.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("scan", "--scan", "saved native scan.v1/v2 SQLite artifact", Py::TEXT))
        .arg(v("run", "--run", "legacy crawl output directory with audit.json", Py::TEXT))
        .arg(v("max_decisions", "--max-decisions", "decision sample size (1..20)", Py::INT).def("20")),
        c("crawl-diagnose-export", "Write a new redacted diagnostic file only when its path was explicit.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("scan", "--scan", "saved native scan.v1/v2 SQLite artifact", Py::TEXT))
        .arg(v("run", "--run", "legacy crawl output directory with audit.json", Py::TEXT))
        .arg(v("max_decisions", "--max-decisions", "decision sample size (1..20)", Py::INT).def("20"))
        .arg(v("export", "--export", "write a new redacted JSON diagnostic file (never overwrite)", Py::TEXT)),
        c("compare-crawls", "Diff two audits: which findings were fixed, which are new, which pages\ndropped out of th")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("compression", "--compression", "explicit NDJSON compression; gzip requires --out-dir", Py::TEXT.ch(&["none", "gzip"])))
        .arg(v("out_dir", "--out-dir", "new local compare.v2 package; required for large retained audits", Py::TEXT))
        .arg(v("correspondence", "--correspondence", "url-correspondence.v1 JSON file declaring origin and URL pairs", Py::TEXT))
        .arg(sw("force", "--force", "compare known-different effective crawl settings"))
        .arg(v("before", "--before", "path to the earlier audit.json or scan.v1 SQLite artifact", Py::TEXT))
        .arg(v("after", "--after", "path to the later audit.json or scan.v1 SQLite artifact", Py::TEXT))
        .arg(sw("resume", "--resume", "resume the matching local comparison checkpoint for --out-dir")),
        c("compare-findings", "Classify the findings delta between two audits by (check, target_url).")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("before", "--before", "path to the earlier audit.json or scan.v1 SQLite artifact", Py::TEXT))
        .arg(v("after", "--after", "path to the later audit.json or scan.v1 SQLite artifact", Py::TEXT)),
        c("compare-read", "Read a bounded page from a validated compare.v2 file.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("manifest", "--manifest", "compare.v2 compare.json manifest", Py::TEXT).id_("manifest_path"))
        .arg(v("file", "--file", "", Py::TEXT.ch(&["entered", "left", "appeared", "disappeared", "unchanged", "by_check", "facts", "correspondence", "segment_changes"])))
        .arg(v("offset", "--offset", "", Py::INT))
        .arg(v("limit", "--limit", "", Py::INT))
        .arg(v("sort", "--sort", "", Py::TEXT.ch(&["url", "check", "severity"])))
        .arg(v("direction", "--direction", "", Py::TEXT.ch(&["asc", "desc"])))
        .arg(v("contains", "--contains", "", Py::TEXT))
        .arg(v("segment", "--segment", "", Py::TEXT))
        .arg(v("change", "--change", "", Py::TEXT.ch(&["entered", "left", "appeared", "disappeared", "unchanged"])))
        .arg(v("max_bytes", "--max-bytes", "", Py::INT)),
        c("compare-report", "Write a bounded Markdown summary to a new path from a validated compare.v2 package.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("manifest", "--manifest", "compare.v2 compare.json manifest", Py::TEXT).id_("manifest_path"))
        .arg(v("out_file", "--out-file", "new Markdown report path; refuses overwrite", Py::TEXT))
        .arg(v("max_bytes", "--max-bytes", "", Py::INT)),
        c("compare-status", "Read the current checkpoint or completed manifest for a comparison output path.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("out_dir", "--out-dir", "compare.v2 destination or checkpoint base path", Py::TEXT)),
        c("compare-url", "Return exact-URL before/after facts and finding deltas without absence claims.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("before", "--before", "earlier audit.json or native scan SQLite", Py::TEXT))
        .arg(v("after", "--after", "later audit.json or native scan SQLite", Py::TEXT))
        .arg(v("url", "--url", "exact URL to compare", Py::TEXT))
        .arg(v("max_bytes", "--max-bytes", "", Py::INT))
        .arg(v("max_lines", "--max-lines", "", Py::INT))
        .arg(v("left_representation", "--left-representation", "", Py::TEXT.ch(&["static", "rendered", "legacy_fragment"])))
        .arg(v("right_representation", "--right-representation", "", Py::TEXT.ch(&["static", "rendered", "legacy_fragment"]))),
        c("crawl-enrich", "Join an existing crawl's pages to an offline traffic/search CSV.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("audit", "--audit", "crawl audit JSON or SQLite scan", Py::TEXT))
        .arg(v("external_csv", "--external-csv", "URL-keyed traffic or search CSV", Py::TEXT))
        .arg(v("url_column", "--url-column", "external CSV URL column (default: url)", Py::TEXT).def("url"))
        .arg(sw("ignore_query", "--ignore-query", "join URLs without query strings"))
        .arg(sw("ignore_scheme", "--ignore-scheme", "join HTTP and HTTPS URLs"))
        .arg(sw("casefold_path", "--casefold-path", "case-fold URL paths for the join"))
        .arg(v("out_urls", "--out-urls", "write reliable external-only URLs as a list-mode input file", Py::TEXT))
        .arg(v("visits_column", "--visits-column", "CSV column with visit counts (analytics findings)", Py::TEXT))
        .arg(v("bounce_column", "--bounce-column", "CSV column with bounce rate, as a percentage or fraction (analytics findings)", Py::TEXT)),
        c("crawl-import", "Read an explicitly mapped third-party CSV crawl bundle offline.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("manifest", "--manifest", "versioned third-party crawl manifest JSON (CSV files stay beside it)", Py::TEXT)),
        c("segment-diff", "Cross-segment counterpart diff (#358): which pages of ``source`` have a\ncounterpart in `")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("audit", "--audit", "path to the audit.json or scan.v1 SQLite artifact to diff", Py::TEXT))
        .arg(v("source", "--source", "segment name that should have a counterpart", Py::TEXT))
        .arg(v("target", "--target", "segment name to look for the counterpart in", Py::TEXT)),
        c("redirects-generate", "Redirects generate.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("format", "--format", "apache-rewrite-rule|apache-redirect|nginx|csv|custom", Py::TEXT)),
        c("redirects-check", "Redirects check.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("url", "--url", "target URL", Py::TEXT)),
        c("sitemap-crawl", "Expand one sitemap, or discover one from a site root through robots.txt.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("url", "--url", "target URL", Py::TEXT))
        .arg(v("concurrency", "--concurrency", "parallel fetches (default 3)", Py::INT))
        .arg(v("project", "--project", "explicit local project for sitemap observation", Py::TEXT)),
        c("monitor-configure", "Monitor configure.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("directory", "--directory", "project directory", Py::TEXT)),
        c("monitor-run", "Monitor run.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("directory", "--directory", "project directory", Py::TEXT)),
        c("monitor-collect", "Preview or explicitly collect one existing bounded monitoring claim.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("directory", "--directory", "project directory", Py::TEXT))
        .arg(v("expected_revision", "--expected-revision", "current monitoring revision", Py::INT))
        .arg(sw("apply", "--apply", "explicitly fetch the existing claimed plan; preview by default")),
        c("monitor-local-deliver", "Record a local monitoring receipt without an external transport.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("directory", "--directory", "project directory", Py::TEXT))
        .arg(v("expected_revision", "--expected-revision", "current monitoring revision", Py::INT))
        .arg(v("scan_id", "--scan-id", "retained monitor run identifier for a local receipt", Py::TEXT)),
        c("monitor-status", "Monitor status.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("directory", "--directory", "project directory", Py::TEXT)),
        c("monitor-schedule", "Record a local monitor runner claim without starting a background service.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("directory", "--directory", "project directory", Py::TEXT)),
        c("schedule-show", "Read the project's scheduled-scan settings, last run and next due time.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("directory", "--directory", "project directory", Py::TEXT)),
        c("schedule-set", "Merge a partial update into the project's schedule settings (revisioned).")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("directory", "--directory", "project directory", Py::TEXT))
        .arg(v("expected_revision", "--expected-revision", "current schedule revision", Py::INT)),
        c("schedule-run-due", "Schedule run due.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("directory", "--directory", "project directory", Py::TEXT)),
        c("schedule-install", "Write a launchd plist, cron line or Task Scheduler command for run-due. Explicit only.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("directory", "--directory", "project directory", Py::TEXT))
        .arg(v("kind", "--kind", "", Py::TEXT.ch(&["launchd", "cron", "windows"])).req())
        .arg(v("dest", "--dest", "file to write (launchd default: ~/Library/LaunchAgents)", Py::TEXT))
        .arg(v("executable", "--executable", "executable for the trigger (default: this one)", Py::TEXT)),
        c("schedule-uninstall", "Schedule uninstall.")
        .arg(v("input", "--input", "JSON object mapped onto the handler arguments", Py::TEXT))
        .arg(v("directory", "--directory", "project directory", Py::TEXT))
        .arg(v("kind", "--kind", "", Py::TEXT.ch(&["launchd", "cron", "windows"])).req())
        .arg(v("dest", "--dest", "file to write (launchd default: ~/Library/LaunchAgents)", Py::TEXT)),
        c("crawl-profile", "inspect or delete saved crawl profiles")
        .subcommand(c("list", "list saved crawl profile names"))
        .subcommand(c("show", "print one saved crawl profile as JSON")
        .arg(pos("name", "").vm("NAME")))
        .subcommand(c("delete", "delete a saved crawl profile")
        .arg(pos("name", "").vm("NAME"))),
    ]
}
