//! Handlers of the server-log group (the reference suite): one-shot analysis
//! of an access log and the retained log store (import, summaries, bot evidence,
//! the crawl/log join). The domain is `spiderhead-log-analyzer`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use spiderhead_log_analyzer::analyze::{analyze_log, AnalyzeOptions};
use spiderhead_log_analyzer::bot_evidence::{bot_verification, BotQuery};
use spiderhead_log_analyzer::dns::{SystemClock, Verifier};
use spiderhead_log_analyzer::import::{
    import_logs, ImportContext, ImportOptions, ProjectManifest, ProjectManifestLoader,
};
use spiderhead_log_analyzer::join::{
    join_log_scan, JoinQuery, ScanHeader, ScanOpener, ScanPage, ScanSource,
};
use spiderhead_log_analyzer::presets::PresetSet;
use spiderhead_log_analyzer::rangecache::{HttpGet, RangeCache, UrlFetcher};
use spiderhead_log_analyzer::ranges::RangeBook;
use spiderhead_log_analyzer::store::latest_source;
use spiderhead_log_analyzer::summary::{log_summary, Filter, SummaryQuery};
use spiderhead_log_analyzer::system::{free_space, SystemResolver};
use spiderhead_log_analyzer::verify::{
    bot_verification_all, bot_verification_page, AllQuery, Engine, Mode, PageQuery, Whois,
};

use crate::error::ToolError;

use super::{kwargs, need_str, opt_bool, opt_int, opt_str};

fn verifier() -> Verifier {
    Verifier::new(Arc::new(SystemResolver), Arc::new(SystemClock))
}

pub(super) fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or_default()
}

fn failed(error: impl std::fmt::Display) -> ToolError {
    ToolError::Failed(error.to_string())
}

/// `log_analyze`: parse one access log and compute aggregates; only bot
/// verification touches the network.
pub fn log_analyze(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let Some(path) = opt_str(&args, "path").filter(|path| !path.is_empty()) else {
        return Err(ToolError::Failed(
            "path required (web server access-log file)".to_owned(),
        ));
    };
    let defaults = AnalyzeOptions::default();
    let options = AnalyzeOptions {
        verify_bots: opt_bool(&args, "verify_bots").unwrap_or(false),
        max_lines: opt_int(&args, "max_lines").unwrap_or(defaults.max_lines),
        offset: opt_int(&args, "offset").unwrap_or(defaults.offset),
        limit: opt_int(&args, "limit").unwrap_or(defaults.limit),
        max_dns: opt_int(&args, "max_dns").unwrap_or(defaults.max_dns),
        bot: opt_str(&args, "bot").map(str::to_owned),
        ip: opt_str(&args, "ip").map(str::to_owned),
        ..defaults
    };
    analyze_log(path, &options, &verifier()).map_err(failed)
}

/// The project manifest the importer needs, read through the project workspace.
pub(super) struct Workspaces;

impl ProjectManifestLoader for Workspaces {
    fn load(&self, project: &str) -> Result<ProjectManifest, String> {
        let (root, document) = spiderhead_projects::workspace::load(Path::new(project))
            .map_err(|error| error.to_string())?;
        Ok(ProjectManifest {
            root,
            project_uuid: document["project_uuid"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            target: document["site"]["target"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        })
    }
}

/// `logs_import`: import a log file or directory into a new retained artifact.
pub fn logs_import(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 4] = ["path", "project", "max_lines", "max_files"];
    let path = need_str(&args, "path", "logs-import", &ACCEPTED)?;
    let project = need_str(&args, "project", "logs-import", &ACCEPTED)?;
    let defaults = ImportOptions::default();
    let options = ImportOptions {
        max_lines: opt_int(&args, "max_lines").unwrap_or(100_000_000),
        max_files: opt_int(&args, "max_files").unwrap_or(defaults.max_files),
        // Extensions over the oracle's arguments, both optional. Per-request rows are
        // opt-in here so the default output stays identical to the oracle's.
        keep_requests: opt_bool(&args, "keep_requests").unwrap_or(false),
        append: opt_bool(&args, "append").unwrap_or(defaults.append),
        ..defaults
    };
    let context = ImportContext {
        loader: &Workspaces,
        free_space: &free_space,
        now: &now,
    };
    Ok(import_logs(&context, path, project, options))
}

/// Exactly one of `source` / `project`; a project resolves to its newest log store.
fn resolve_source(source: Option<&str>, project: Option<&str>) -> Result<String, ToolError> {
    let source = source.filter(|text| !text.is_empty());
    let project = project.filter(|text| !text.is_empty());
    match (source, project) {
        (Some(_), Some(_)) => Err(ToolError::Failed(
            "pass either source or project, not both".to_owned(),
        )),
        (None, None) => Err(ToolError::Failed(
            "a retained log store is required: --source ARTIFACT or --project DIRECTORY".to_owned(),
        )),
        (Some(source), None) => Ok(source.to_owned()),
        (None, Some(project)) => {
            let (root, _) =
                spiderhead_projects::workspace::load(Path::new(project)).map_err(failed)?;
            latest_source(&root).map_err(failed)
        }
    }
}

fn text(args: &serde_json::Map<String, Value>, name: &str) -> String {
    opt_str(args, name).unwrap_or_default().to_owned()
}

/// `logs_summary`: one page of retained log aggregates.
pub fn logs_summary(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let source = resolve_source(opt_str(&args, "source"), opt_str(&args, "project"))?;
    let query = SummaryQuery {
        view: opt_str(&args, "view").unwrap_or("urls").to_owned(),
        limit: opt_int(&args, "limit").unwrap_or(100),
        offset: opt_int(&args, "offset").unwrap_or_default(),
        filter: Filter {
            bot: text(&args, "bot"),
            family: text(&args, "family"),
            ip: text(&args, "ip"),
            date_from: text(&args, "date_from"),
            date_to: text(&args, "date_to"),
            status: opt_int(&args, "status"),
            section: text(&args, "section"),
        },
    };
    Ok(log_summary(&source, &query))
}

/// HTTPS GET for range lists: small bodies only, bounded time, no plain HTTP.
struct Https;

impl HttpGet for Https {
    fn get(&self, url: &str) -> Result<String, String> {
        if !url.starts_with("https://") {
            return Err("range lists are fetched over https only".to_owned());
        }
        let url = url.to_owned();
        // A private current-thread runtime on its own thread: handlers are synchronous and
        // may be called from inside the server's runtime.
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            runtime.block_on(async {
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(20))
                    .redirect(reqwest::redirect::Policy::limited(3))
                    .build()
                    .map_err(|e| e.to_string())?;
                let reply = client.get(&url).send().await.map_err(|e| e.to_string())?;
                if !reply.status().is_success() {
                    return Err(format!("HTTP {}", reply.status().as_u16()));
                }
                let body = reply.bytes().await.map_err(|e| e.to_string())?;
                if body.len() > 8 * 1024 * 1024 {
                    return Err("range list is larger than 8 MiB".to_owned());
                }
                String::from_utf8(body.to_vec()).map_err(|e| e.to_string())
            })
        })
        .join()
        .map_err(|_| "download thread panicked".to_owned())?
    }
}

/// Multi-step verification (`verify_mode`, `ranges_dir`, `presets_dir`, `status`, `group`):
/// ranges, then reverse+forward DNS, then ASN. Extension over the oracle's arguments.
fn logs_bots_verified(
    args: &serde_json::Map<String, Value>,
    source: &str,
) -> Result<Value, ToolError> {
    let verify_all = opt_bool(args, "verify_all").unwrap_or(false);
    let default_mode = if verify_all { "full" } else { "cached" };
    let mode =
        Mode::from_name(opt_str(args, "verify_mode").unwrap_or(default_mode)).map_err(failed)?;
    let presets = match opt_str(args, "presets_dir").filter(|d| !d.is_empty()) {
        Some(dir) => PresetSet::with_user_dir(Path::new(dir)).map_err(failed)?,
        None => PresetSet::builtin().clone(),
    };
    let ranges_dir = match opt_str(args, "ranges_dir").filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(source)
            .parent()
            .and_then(Path::parent)
            .map_or_else(
                || PathBuf::from("log-ranges"),
                |root| root.join("log-ranges"),
            ),
    };
    let cache = RangeCache::new(ranges_dir);
    let mut book = RangeBook::new();
    let http = Https;
    let fetcher = UrlFetcher {
        presets: &presets,
        http: &http,
    };
    let loaded = cache.load(
        &mut book,
        mode.fetch_ranges().then_some(&fetcher as _),
        &presets.providers(),
        now(),
    );
    let verifier = verifier();
    let whois = Whois::default();
    let engine = Engine {
        presets: &presets,
        book: &book,
        verifier: &verifier,
        asn: Some(&whois),
        clock: &SystemClock,
    };
    if verify_all {
        let defaults = AllQuery::default();
        let progress = |done: usize, total: usize| {
            eprintln!("logs-bots verify: {done}/{total} pairs");
        };
        let query = AllQuery {
            mode,
            group: text(args, "group"),
            workers: opt_int(args, "workers").unwrap_or(defaults.workers),
            max_dns: opt_int(args, "max_dns").unwrap_or(defaults.max_dns),
            dns_per_second: opt_int(args, "dns_per_second").unwrap_or(defaults.dns_per_second),
        };
        let mut out =
            bot_verification_all(source, &engine, &query, now(), &progress).map_err(failed)?;
        out["ranges"] = Value::Array(loaded.iter().map(|l| l.to_json()).collect());
        return Ok(out);
    }
    let defaults = PageQuery::default();
    let query = PageQuery {
        mode,
        status: text(args, "status"),
        group: text(args, "group"),
        limit: opt_int(args, "limit").unwrap_or(defaults.limit),
        offset: opt_int(args, "offset").unwrap_or(defaults.offset),
        max_dns: opt_int(args, "max_dns").unwrap_or(defaults.max_dns),
    };
    let mut page = bot_verification_page(source, &engine, &query, now()).map_err(failed)?;
    page["ranges"] = Value::Array(loaded.iter().map(|l| l.to_json()).collect());
    Ok(page)
}

/// `logs_bots`: a bounded per-IP bot page; verification is opt-in.
pub fn logs_bots(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    let source = resolve_source(opt_str(&args, "source"), opt_str(&args, "project"))?;
    if [
        "verify_mode",
        "verify_all",
        "ranges_dir",
        "presets_dir",
        "status",
        "group",
    ]
    .iter()
    .any(|key| args.get(*key).is_some_and(|v| !v.is_null()))
    {
        return logs_bots_verified(&args, &source);
    }
    let defaults = BotQuery::default();
    let query = BotQuery {
        verify_bots: opt_bool(&args, "verify_bots").unwrap_or(false),
        limit: opt_int(&args, "limit").unwrap_or(defaults.limit),
        offset: opt_int(&args, "offset").unwrap_or(defaults.offset),
        bot: text(&args, "bot"),
        family: text(&args, "family"),
        ip: text(&args, "ip"),
        verdict: text(&args, "verdict"),
        max_dns: opt_int(&args, "max_dns").unwrap_or(defaults.max_dns),
    };
    bot_verification(&source, &query, &verifier(), now()).map_err(failed)
}

/// Opens a finished native scan read-only for the crawl/log join.
struct NativeScans;

struct NativeScan {
    path: PathBuf,
}

impl ScanOpener for NativeScans {
    fn open(&self, path: &str) -> Result<Box<dyn ScanSource>, String> {
        let path = PathBuf::from(path);
        spiderhead_store::scan_artifact::open_scan(&path, false, None)
            .map(|_| Box::new(NativeScan { path }) as Box<dyn ScanSource>)
            .map_err(|error| super::plain(&error))
    }
}

impl ScanSource for NativeScan {
    fn header(&self) -> Result<ScanHeader, String> {
        let con = spiderhead_store::scan_artifact::open_scan(&self.path, false, None)
            .map_err(|error| super::plain(&error))?;
        con.query_row("SELECT * FROM scan WHERE singleton=1", [], |row| {
            let text = |name: &str| {
                row.get::<_, Option<String>>(name)
                    .map(Option::unwrap_or_default)
            };
            let capabilities: String = text("capabilities_json")?;
            Ok(ScanHeader {
                source_kind: text("source_kind")?,
                lifecycle: text("lifecycle")?,
                crawl_partial: row.get::<_, Option<i64>>("crawl_partial")?.unwrap_or(0) != 0,
                capabilities: serde_json::from_str(&capabilities).unwrap_or(Value::Null),
                start_url: text("start_url")?,
                scan_uuid: json!(row.get::<_, Option<String>>("scan_uuid")?),
                evidence_revision: row
                    .get::<_, Option<i64>>("evidence_revision")?
                    .map_or(Value::Null, |value| json!(value)),
            })
        })
        .map_err(|error| error.to_string())
    }

    fn pages(&self, each: &mut dyn FnMut(ScanPage)) -> Result<(), String> {
        let con = spiderhead_store::scan_artifact::open_scan(&self.path, false, None)
            .map_err(|error| super::plain(&error))?;
        let mut statement = con
            .prepare(
                "SELECT u.url,p.status_code,p.meta_robots,p.x_robots FROM pages p JOIN urls u USING(url_id)",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok(ScanPage {
                    url: row.get(0)?,
                    status_code: row.get(1)?,
                    meta_robots: row.get(2)?,
                    x_robots: row.get(3)?,
                })
            })
            .map_err(|error| error.to_string())?;
        for row in rows {
            each(row.map_err(|error| error.to_string())?);
        }
        Ok(())
    }
}

/// `logs_join`: join retained bot observations to a finished native scan.
pub fn logs_join(args: &Value) -> Result<Value, ToolError> {
    let args = kwargs(args);
    const ACCEPTED: [&str; 10] = [
        "source",
        "scan",
        "view",
        "base_url",
        "family",
        "bot",
        "date_from",
        "date_to",
        "limit",
        "offset",
    ];
    let source = need_str(&args, "source", "logs-join", &ACCEPTED)?;
    let scan = need_str(&args, "scan", "logs-join", &ACCEPTED)?;
    let defaults = JoinQuery::default();
    let query = JoinQuery {
        view: opt_str(&args, "view").unwrap_or(&defaults.view).to_owned(),
        base_url: text(&args, "base_url"),
        bot: text(&args, "bot"),
        family: text(&args, "family"),
        date_from: text(&args, "date_from"),
        date_to: text(&args, "date_to"),
        limit: opt_int(&args, "limit").unwrap_or(defaults.limit),
        offset: opt_int(&args, "offset").unwrap_or(defaults.offset),
    };
    Ok(join_log_scan(source, scan, &NativeScans, &query))
}
