//! `crawl-site`: crawls one start URL into a `scan.v1` SQLite file.
//!
//! The flags match the reference capture: `--max-urls-per-second 50 --max-urls 200`.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use spiderhead_core::crawl_settings;
use spiderhead_core::host::SystemHost;
use spiderhead_crawl::oracle;
use spiderhead_crawl::spider::{self, CrawlOptions};
use spiderhead_parse::ParseOptions;
use spiderhead_store::native_scan::NativeScan;
use spiderhead_store::scan_lease::NativeScanLease;
use spiderhead_store::scan_schema::NativeScanSchemaMixin;

const USAGE: &str = "usage: crawl-site --url <start-url> --scan-out <path> [--max-urls N] \
[--max-urls-per-second R] [--sitemap URL]";

fn main() -> std::process::ExitCode {
    match run() {
        Ok(summary) => {
            println!("{}", serde_json::to_string(&summary).unwrap_or_default());
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("crawl-site: {error}");
            std::process::ExitCode::from(1)
        }
    }
}

struct Args {
    url: String,
    scan_out: PathBuf,
    max_urls: i64,
    max_urls_per_second: f64,
    sitemaps: Vec<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut url = None;
    let mut scan_out = None;
    let mut max_urls = 200i64;
    let mut rate = 50.0f64;
    let mut sitemaps = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--url" => url = Some(args.next().ok_or("--url needs a value")?),
            "--scan-out" => {
                scan_out = Some(PathBuf::from(
                    args.next().ok_or("--scan-out needs a value")?,
                ))
            }
            "--max-urls" => {
                max_urls = args
                    .next()
                    .ok_or("--max-urls needs a value")?
                    .parse()
                    .map_err(|_| "--max-urls must be an integer")?;
            }
            "--max-urls-per-second" => {
                rate = args
                    .next()
                    .ok_or("--max-urls-per-second needs a value")?
                    .parse()
                    .map_err(|_| "--max-urls-per-second must be a number")?;
            }
            "--sitemap" => sitemaps.push(args.next().ok_or("--sitemap needs a value")?),
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other:?}\n{USAGE}")),
        }
    }
    Ok(Args {
        url: url.ok_or("missing --url")?,
        scan_out: scan_out.ok_or("missing --scan-out")?,
        max_urls,
        max_urls_per_second: rate,
        sitemaps,
    })
}

fn run() -> Result<Value, String> {
    let args = parse_args()?;
    let host = SystemHost;
    // Effective config: defaults plus the CLI overrides.
    let config_map = spider::capture_config(&host, args.max_urls, args.max_urls_per_second)?;
    let _min_delay = spider::bin_load_delay(args.max_urls_per_second);

    let runtime_versions = oracle::runtime_versions();
    let resumed = args.scan_out.exists();
    let mut scan = if resumed {
        reopen(&args.scan_out)?
    } else {
        NativeScanSchemaMixin::create(
            &args.scan_out,
            &args.url,
            &config_map,
            None,
            oracle::WRITER_VERSION,
            oracle::WRITER_REVISION,
            &runtime_versions,
            &oracle::limitations(),
            None,
            &[],
            "scan.v1",
        )
        .map_err(|e| e.to_string())?
    };
    scan.begin_collection(true).map_err(|e| e.to_string())?;

    let flat = crawl_settings::flatten(&Value::Object(config_map.clone()));
    let text = |key: &str, default: &str| -> String {
        flat.get(key)
            .and_then(Value::as_str)
            .unwrap_or(default)
            .to_string()
    };
    let number = |key: &str, default: f64| -> f64 {
        flat.get(key).and_then(Value::as_f64).unwrap_or(default)
    };
    let int = |key: &str, default: i64| -> i64 {
        flat.get(key).and_then(Value::as_i64).unwrap_or(default)
    };
    let user_agent = text("http.user_agent", "SpiderHeadBot");
    let options = CrawlOptions {
        config: Value::Object(config_map.clone()),
        interrupt_after_pages: None,
        client: spiderhead_fetch::client::build_client(number("http.timeout_seconds", 15.0))?,
        start_url: args.url.clone(),
        seed_urls: args.sitemaps.clone(),
        user_agent: user_agent.clone(),
        robots_token: text("robots.user_agent_token", "SpiderHeadBot"),
        robots_policy: text("robots.policy", "respect"),
        min_delay: number("speed.min_delay_seconds", 0.5),
        max_delay: number("speed.max_delay_seconds", 60.0),
        concurrency: int("speed.concurrency", 1).max(1) as u32,
        max_urls: int("limits.max_urls", 200),
        max_depth: int("limits.max_depth", 5),
        max_requests: int("limits.max_requests", 20000),
        max_crawl_seconds: number("limits.max_crawl_seconds", 0.0),
        timeout_seconds: number("http.timeout_seconds", 15.0),
        retry_on_timeout: int("http.retry_on_timeout", 0) as u32,
        retry_server_errors: int("fetch.retry_server_errors", 0) as u32,
        max_response_bytes: int("limits.max_response_bytes", 5_242_880),
        entity_capture_limit: int("storage.max_body_bytes", 5_242_880),
        max_url_length: int("limits.max_url_length", 2000) as usize,
        parse_options: ParseOptions::default(),
        crawl_redirects: true,
        on_batch: None,
        request_gate: None,
        sitemap_only: false,
        resumed,
        rate_override: None,
    };
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let outcome = runtime.block_on(spider::crawl_site(&mut scan, &options))?;
    Ok(json!({
        "finish_reason": outcome.finish_reason,
        "stopped_reason": outcome.stopped_reason,
        "robots_blocked": outcome.robots_blocked,
        "max_depth_reached": outcome.max_depth_reached,
    }))
}

/// Resume path: reopen the existing scan and requeue anything still in flight.
fn reopen(path: &Path) -> Result<NativeScan, String> {
    let connection =
        rusqlite::Connection::open(path).map_err(|e| format!("scan reopen failed: {e}"))?;
    let _ = connection.busy_timeout(std::time::Duration::from_secs(10));
    let scan = NativeScan::new(path, connection, None).map_err(|e| e.to_string())?;
    scan.recover_inflight().map_err(|e| e.to_string())?;
    Ok(scan)
}
