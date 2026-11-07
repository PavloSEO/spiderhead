//! Integration test: the Rust crawler reproduces the reference baseline's
//! `scan.tables.json` for every synthetic site (the slice's main gate).

use std::path::PathBuf;

use serde_json::Value;
use spiderhead_core::host::SystemHost;
use spiderhead_crawl::dump::scan_tables;
use spiderhead_crawl::oracle;
use spiderhead_crawl::spider::{self, CrawlOptions};
use spiderhead_oracle_diff::{diff, Options};
use spiderhead_oracle_testkit::FixtureServer;
use spiderhead_parse::ParseOptions;
use spiderhead_store::native_scan::NativeScan;
use spiderhead_store::scan_lease::NativeScanLease;
use spiderhead_store::scan_schema::NativeScanSchemaMixin;

fn oracle_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../oracle")
}

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("crawl-parity-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if std::env::var("SPIDERHEAD_PARITY_KEEP").is_ok() {
            println!("keeping {}", self.0.display());
            return;
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Crawl one site with the capture invocation's settings and dump the tables.
fn crawl_and_dump(site: &str, port: u16, work: &std::path::Path) -> Value {
    let server = FixtureServer::start_on(site, port);
    crawl_and_dump_with_seeds(site, &server, work, &[])
}

fn crawl_and_dump_with_seeds(
    site: &str,
    server: &FixtureServer,
    work: &std::path::Path,
    seed_urls: &[String],
) -> Value {
    let base = server.base.clone();
    let _ = site;
    let host = SystemHost;
    let min_delay = 0.02;
    let config_map = spider::capture_config(&host, 200, 50.0).expect("capture config");
    let scan_path = work.join("scan.sqlite");
    let runtime_versions = oracle::runtime_versions();
    let mut scan = NativeScanSchemaMixin::create(
        &scan_path,
        &format!("{base}/"),
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
    .expect("scan create");
    scan.begin_collection(true).expect("begin");
    let options = CrawlOptions {
        config: Value::Object(config_map.clone()),
        interrupt_after_pages: None,
        client: spiderhead_fetch::client::build_client(15.0).expect("client"),
        start_url: format!("{base}/"),
        seed_urls: seed_urls.to_vec(),
        user_agent: spiderhead_core::user_agents::DEFAULT_USER_AGENT.into(),
        robots_token: spiderhead_core::user_agents::DEFAULT_TOKEN.into(),
        robots_policy: "respect".into(),
        min_delay,
        max_delay: 60.0,
        concurrency: 1,
        max_urls: 200,
        max_depth: 5,
        max_requests: 600,
        max_crawl_seconds: 400.0,
        timeout_seconds: 15.0,
        retry_on_timeout: 0,
        retry_server_errors: 0,
        max_response_bytes: 5_242_880,
        entity_capture_limit: 5_242_880,
        max_url_length: 2000,
        parse_options: ParseOptions::default(),
        crawl_redirects: true,
        on_batch: None,
        request_gate: None,
        sitemap_only: false,
        url_list: Vec::new(),
        resumed: false,
        rate_override: None,
    };
    let outcome = tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(spider::crawl_site(&mut scan, &options))
        .expect("crawl");
    assert_eq!(
        outcome.finish_reason, "finished",
        "{}",
        outcome.stopped_reason
    );
    scan.close();
    scan_tables(&scan_path, &base, work.to_str().unwrap_or("")).expect("dump")
}

fn baseline_path(site: &str) -> std::path::PathBuf {
    oracle_root()
        .join("baselines")
        .join(site)
        .join("scan.tables.json")
}

fn baseline(site: &str) -> Value {
    spiderhead_oracle_testkit::fixture_json(&baseline_path(site))
}

/// A bless run also republishes the stored capture: `raw-scans/<site>` must hold
/// the same capture the `scan.tables.json` baseline describes, or the read-path
/// tests and the live parity replay diverge.
fn refresh_raw_capture(site: &str, scan_path: &std::path::Path) {
    let dir = oracle_root().join("raw-scans").join(site);
    let target = dir.join("scan.sqlite");
    // Publish atomically: other test binaries bless downstream fixtures from the
    // same file in the same suite pass and must never observe a partial copy.
    let tmp = dir.join("scan.sqlite.tmp");
    std::fs::copy(scan_path, &tmp).expect("raw scan copy");
    std::fs::rename(&tmp, &target).expect("raw scan publish");
    let _ = std::fs::remove_file(dir.join("scan.sqlite.audit-v2.sqlite"));
    let (header, collections) =
        spiderhead_checks::audit::audit_v2_document(&target).expect("audit document");
    spiderhead_checks::audit::write_audit_v2(&target, &header, &collections)
        .expect("audit companion");
}

/// True when the stored capture already matches the fresh dump. Republishing an
/// equivalent capture would only churn volatile fields (timings, scan uuid) and
/// invalidate the byte fixtures the other tests bless in the same pass.
fn capture_is_fresh(site: &str, port: u16, actual: &Value) -> bool {
    let target = oracle_root()
        .join("raw-scans")
        .join(site)
        .join("scan.sqlite");
    if !target.exists() {
        return false;
    }
    let base = format!("http://127.0.0.1:{port}");
    match scan_tables(&target, &base, "") {
        Ok(stored) => diff(&stored, actual, &Options::default()).is_empty(),
        Err(_) => false,
    }
}

fn assert_parity(site: &str, port: u16) {
    let work = Scratch::new(site);
    let actual = crawl_and_dump(site, port, &work.0);
    if spiderhead_oracle_diff::bless_enabled() {
        if !capture_is_fresh(site, port, &actual) {
            refresh_raw_capture(site, &work.0.join("scan.sqlite"));
        }
        spiderhead_oracle_diff::bless_json(&baseline_path(site), &actual);
        return;
    }
    let expected = baseline(site);
    let divergences = diff(&expected, &actual, &Options::default());
    assert!(
        divergences.is_empty(),
        "{site}: {} divergence(s)\n{}",
        divergences.len(),
        spiderhead_oracle_diff::report(&divergences, 40)
    );
}

#[test]
fn parity_basic() {
    assert_parity("basic", 18701);
}

#[test]
fn parity_errors() {
    assert_parity("errors", 18702);
}

#[test]
fn parity_redirects() {
    assert_parity("redirects", 18703);
}

#[test]
fn parity_canonical_hreflang() {
    assert_parity("canonical-hreflang", 18704);
}

#[test]
fn parity_schema() {
    assert_parity("schema", 18705);
}

#[test]
fn parity_robots_sitemap() {
    // The capture passes an explicit sitemap seed for this site.
    let work = Scratch::new("robots-sitemap");
    let server = FixtureServer::start_on("robots-sitemap", 18706);
    let base = server.base.clone();
    let actual = crawl_and_dump_with_seeds(
        "robots-sitemap",
        &server,
        &work.0,
        &[format!("{base}/sitemap-ok.xml")],
    );
    if spiderhead_oracle_diff::bless_enabled() {
        if !capture_is_fresh("robots-sitemap", 18706, &actual) {
            refresh_raw_capture("robots-sitemap", &work.0.join("scan.sqlite"));
        }
        spiderhead_oracle_diff::bless_json(&baseline_path("robots-sitemap"), &actual);
        return;
    }
    let expected = baseline("robots-sitemap");
    let divergences = diff(&expected, &actual, &Options::default());
    assert!(
        divergences.is_empty(),
        "robots-sitemap: {} divergence(s)\n{}",
        divergences.len(),
        spiderhead_oracle_diff::report(&divergences, 40)
    );
}

// The resume gate: an interrupted run reopened via `recover_inflight` reaches
// the same scan as an uninterrupted one (modulo the runtime block, whose
// request count legitimately includes the second robots probe).
#[test]
fn interrupted_crawl_resumes_to_the_same_scan() {
    let work = Scratch::new("resume");
    let scan_path = work.0.join("scan.sqlite");
    let server = FixtureServer::start_on("basic", 18711);
    let base = server.base.clone();

    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let build_options = |interrupt: Option<i64>, resumed: bool| CrawlOptions {
        config: Value::Object(capture_config_map()),
        interrupt_after_pages: interrupt,
        client: spiderhead_fetch::client::build_client(15.0).expect("client"),
        start_url: format!("{base}/"),
        seed_urls: Vec::new(),
        user_agent: spiderhead_core::user_agents::DEFAULT_USER_AGENT.into(),
        robots_token: spiderhead_core::user_agents::DEFAULT_TOKEN.into(),
        robots_policy: "respect".into(),
        min_delay: 0.02,
        max_delay: 60.0,
        concurrency: 1,
        max_urls: 200,
        max_depth: 5,
        max_requests: 600,
        max_crawl_seconds: 400.0,
        timeout_seconds: 15.0,
        retry_on_timeout: 0,
        retry_server_errors: 0,
        max_response_bytes: 5_242_880,
        entity_capture_limit: 5_242_880,
        max_url_length: 2000,
        parse_options: ParseOptions::default(),
        crawl_redirects: true,
        on_batch: None,
        request_gate: None,
        sitemap_only: false,
        url_list: Vec::new(),
        resumed,
        rate_override: None,
    };

    // Uninterrupted reference run.
    let reference_path = work.0.join("reference.sqlite");
    let runtime_versions = oracle::runtime_versions();
    let mut reference = NativeScanSchemaMixin::create(
        &reference_path,
        &format!("{base}/"),
        &capture_config_map(),
        None,
        oracle::WRITER_VERSION,
        oracle::WRITER_REVISION,
        &runtime_versions,
        &oracle::limitations(),
        None,
        &[],
        "scan.v1",
    )
    .expect("reference create");
    reference.begin_collection(true).expect("begin");
    runtime
        .block_on(spider::crawl_site(
            &mut reference,
            &build_options(None, false),
        ))
        .expect("reference crawl");
    reference.close();

    // Interrupted run: park after three pages, then reopen and finish.
    let mut interrupted = NativeScanSchemaMixin::create(
        &scan_path,
        &format!("{base}/"),
        &capture_config_map(),
        None,
        oracle::WRITER_VERSION,
        oracle::WRITER_REVISION,
        &runtime_versions,
        &oracle::limitations(),
        None,
        &[],
        "scan.v1",
    )
    .expect("interrupted create");
    interrupted.begin_collection(true).expect("begin");
    let outcome = runtime.block_on(spider::crawl_site(
        &mut interrupted,
        &build_options(Some(3), false),
    ));
    assert_eq!(
        outcome.expect("interrupted crawl").finish_reason,
        "interrupted"
    );
    interrupted.close();

    let mut resumed = reopen_for_resume(&scan_path);
    runtime
        .block_on(spider::crawl_site(&mut resumed, &build_options(None, true)))
        .expect("resumed crawl");
    resumed.close();

    let ignore = Options {
        ignore_paths: vec!["/resume_state".to_string()],
        ..Default::default()
    };
    let actual = scan_tables(&scan_path, &base, work.0.to_str().unwrap_or("")).expect("dump");
    let expected =
        scan_tables(&reference_path, &base, work.0.to_str().unwrap_or("")).expect("dump");
    let divergences = diff(&expected, &actual, &ignore);
    assert!(
        divergences.is_empty(),
        "resume: {} divergence(s)\n{}",
        divergences.len(),
        spiderhead_oracle_diff::report(&divergences, 40)
    );
}

fn capture_config_map() -> serde_json::Map<String, Value> {
    let host = SystemHost;
    spider::capture_config(&host, 200, 50.0).expect("capture config")
}

fn reopen_for_resume(path: &std::path::Path) -> NativeScan {
    let connection = rusqlite::Connection::open(path).expect("resume reopen");
    let scan = NativeScan::new(path, connection, None).expect("resume open");
    scan.recover_inflight().expect("recover inflight");
    scan
}
