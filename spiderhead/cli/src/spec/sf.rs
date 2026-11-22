//! sf command declarations (the clap builder tree).

use clap::builder::{ArgGroup, Command};

use super::{c, pos, sw, swf, swx, v, ArgExt, Py};

/// Every top-level command of the bucket, fully nested.
pub(crate) fn commands() -> Vec<Command> {
    vec![
        c("sf", "Screaming Frog crawl audit (run | tasks | doctor | save-config | snippet-edits)").long_about("Generate a machine-readable SEO audit from a Screaming Frog crawl (audit.json + audit.md).").version(crate::TOOLKIT_VERSION)
        .subcommand(c("run", "Run an audit")
        .arg(v("load_crawl", "--load-crawl", "Mode A: open a saved crawl through the licensed SF CLI", Py::TEXT).vm("FILE.seospider"))
        .arg(v("crawl", "--crawl", "Mode A: crawl a site from scratch through the licensed SF CLI", Py::TEXT).vm("URL"))
        .arg(v("crawl_list", "--crawl-list", "Mode A: crawl a URL list through the licensed SF CLI", Py::TEXT).vm("FILE"))
        .arg(v("exports_dir", "--exports-dir", "Mode B: read an existing directory of SF CSV/XLSX exports", Py::TEXT).vm("DIR"))
        .arg(v("out", "--out", "Report output directory (default: ./report)", Py::TEXT).def("report"))
        .arg(v("project", "--project", "validated local project workspace for read-only live observation", Py::TEXT))
        .arg(sw("gsc_inspect", "--gsc-inspect", "explicitly inspect up to 50 URLs with the bound Search Console property"))
        .arg(v("profile", "--profile", "Audit/export profile: full (default; verifies the supported export manifest), lite, or custom", Py::TEXT.ch(&["lite", "full", "custom"])))
        .arg(v("config", "--config", "Path to config.json", Py::TEXT).def("config.json"))
        .arg(v("auth_config", "--auth-config", "SF form-authentication profile (.seospiderauthconfig) for protected staging or production environments", Py::TEXT))
        .arg(v("auth", "--auth", "HTTP Basic credentials in USER:PASS form; may be visible in shell history/process lists, so prefer an isolated transient session", Py::TEXT).vm("USER:PASS"))
        .arg(v("sf_cli", "--sf-cli", "Explicit path to the Screaming Frog CLI/launcher executable", Py::TEXT))
        .arg(v("max_urls_per_second", "--max-urls-per-second", "Limit the Mode A crawl request rate (0.1..1000 URLs/s; 1-2 is a polite starting point). Requires a readable base .seospiderconfig; the tool writes and verifies a derived config without changing the base.", Py::FLOAT).vm("N"))
        .arg(v("sitemap", "--sitemap", "Explicit sitemap.xml URL (otherwise discover it from robots.txt)", Py::TEXT))
        .arg(sw("fetch_all_inlinks", "--fetch-all-inlinks", "Fetch the large All Inlinks export in Mode A"))
        .arg(swx("live_recheck", "--live-recheck", "Enable opt-in network rechecks for sitemap and robots data"))
        .arg(swf("no_live_recheck", "--no-live-recheck", ""))
        .arg(v("format", "--format", "Comma-separated report formats to generate: json,md", Py::TEXT).def("json,md"))
        .arg(sw("tasks", "--tasks", "Also generate the prioritized tasks.json + tasks.md backlog"))
        .arg(v("fail_on", "--fail-on", "Return a non-zero exit code when this severity is present (for CI)", Py::TEXT.ch(&["critical", "warning", "none"])).def("none"))
        .arg(sw("verbose", "--verbose", "").sh('v'))
        .arg(sw("quiet", "--quiet", "").sh('q')).group(ArgGroup::new("sf-run-mutex").required(true).multiple(false).args(["load_crawl", "crawl", "crawl_list", "exports_dir"])))
        .subcommand(c("tasks", "Build a prioritized task backlog from audit.json")
        .arg(v("json", "--json", "Path to audit.json or a validated scan.v1 SQLite artifact", Py::TEXT).id_("audit_json").req())
        .arg(v("out", "--out", "Output directory for tasks.json and tasks.md", Py::TEXT).def("report"))
        .arg(v("config", "--config", "Path to config.json containing the task-pipeline settings", Py::TEXT).def("config.json"))
        .arg(sw("quiet", "--quiet", "").sh('q')))
        .subcommand(c("doctor", "Diagnose SF CLI discovery and dependency availability")
        .arg(v("config", "--config", "Path to config.json", Py::TEXT).def("config.json"))
        .arg(v("sf_cli", "--sf-cli", "Explicit SF CLI path to validate before searching elsewhere", Py::TEXT)))
        .subcommand(c("save-config", "Copy the most recent Screaming Frog crawl configuration to a base config file")
        .arg(v("out", "--out", "Where to write the base config (default: audit.seospiderconfig)", Py::TEXT).def("audit.seospiderconfig"))
        .arg(sw("force", "--force", "Overwrite the destination when it already exists")))
        .subcommand(c("snippet-edits", "Import snippet edits from CSV and export review tasks")
        .arg(v("csv", "--csv", "CSV with url and title/description columns", Py::TEXT).req())
        .arg(v("out", "--out", "New JSON task file; existing files are kept", Py::TEXT).req())
        .arg(v("metrics_json", "--metrics-json", "Optional JSON map of named local font profiles for measured pixel widths", Py::TEXT))
        .arg(v("scan", "--scan", "Optional saved scan.v1 artifact for retained-page context", Py::TEXT))
        .arg(v("query", "--query", "Explicit search query for a retained-page text excerpt", Py::TEXT))
        .arg(v("representation", "--representation", "", Py::TEXT.ch(&["static", "rendered", "legacy_fragment"])).def("static"))
        .arg(v("variant_key", "--variant-key", "Optional exact retained browser variant", Py::TEXT))
        .arg(v("content_area_json", "--content-area-json", "Optional content-area configuration JSON", Py::TEXT))
        .arg(v("max_body_bytes", "--max-body-bytes", "", Py::INT).def("8388608")))
        .subcommand(c("import", "Inventory a saved Screaming Frog crawl bundle (.seospider)")
        .arg(pos("bundle", "Path to the saved crawl: a .seospider directory or archive").vm("SAVED_CRAWL"))
        .arg(sw("inspect", "--inspect", "Print a JSON inventory of the bundle's Derby database: tables, row counts, database version, and the start URL when visible"))
        .arg(v("derby_jar", "--derby-jar", "Explicit derby.jar (default: searched in the Screaming Frog installation, ~/Downloads, and common cache locations)", Py::TEXT).vm("PATH"))
        .arg(sw("verbose", "--verbose", "").sh('v'))),
    ]
}
