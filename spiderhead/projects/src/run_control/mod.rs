//! Cooperative pause/stop and live rate requests for durable native crawl runs

use spiderhead_core::filesystem::{PrivateMode, StatCompat};
use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::{value, ProjectsError, ProjectsResult};
use crate::workspace::open_project;

mod checked_pattern;
pub use checked_pattern::*;

const NAME: &str = ".run-control.sqlite";
/// A hard ceiling for operator-set rates: above it the crawl is a load test.
pub const MAX_RATE: f64 = 50.0;
/// Concurrency ceiling.
pub const MAX_CONCURRENCY: i64 = 32;
/// Maximum number of operator-injected URLs per request.
pub const MAX_STEER_URLS: usize = 16;

fn stamp() -> String {
    crate::pytime::zulu_seconds()
}

/// A requested crawl boundary that the scan writer checkpoints.
#[derive(Debug)]
pub struct RunControlInterrupt {
    /// `pause` or `stop`.
    pub action: String,
}

impl RunControlInterrupt {
    /// Validated constructor (`action in {"pause", "stop"}`).
    pub fn new(action: &str) -> ProjectsResult<Self> {
        if !["pause", "stop"].contains(&action) {
            return Err(value("run control action must be pause or stop"));
        }
        Ok(Self {
            action: action.to_owned(),
        })
    }
}

impl fmt::Display for RunControlInterrupt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.action)
    }
}

impl std::error::Error for RunControlInterrupt {}

/// The pacer lane `pending_rate` feeds a new operator delay into.
pub trait OriginPacer {
    /// Follow a live operator rate change (`set_min_delay`).
    fn set_min_delay(&mut self, min_delay_seconds: f64);
}

impl OriginPacer for crate::origin_pacing::ProjectOriginPacer {
    fn set_min_delay(&mut self, min_delay_seconds: f64) {
        crate::origin_pacing::ProjectOriginPacer::set_min_delay(self, min_delay_seconds)
    }
}

/// Run-observation reporter that also polls for one-shot control requests.
pub struct ControlledRunReporter {
    /// Project directory.
    pub directory: PathBuf,
    /// Run id being reported on.
    pub run_id: String,
    /// The wrapped run-observation reporter.
    pub reporter: crate::run_observation::NativeRunReporter,
    /// The consumed control action, once seen.
    pub control_action: Option<String>,
    /// Optional shared pacer the rate request feeds (shared across threads,
    /// because the reporter runs inside the crawl worker).
    pub pacer: Option<Box<dyn OriginPacer + Send>>,
    initial_rate: Option<(f64, Option<i64>)>,
}

impl ControlledRunReporter {
    /// Constructor arguments: directory, run id, downstream reporter, pacer, initial rate.
    pub fn new(
        directory: &Path,
        run_id: &str,
        downstream: Option<Box<dyn FnMut(i64, i64) + Send>>,
        pacer: Option<Box<dyn OriginPacer + Send>>,
        initial_rate: Option<(f64, Option<i64>)>,
    ) -> Self {
        Self {
            directory: directory.to_path_buf(),
            run_id: run_id.to_owned(),
            reporter: crate::run_observation::NativeRunReporter::new(
                directory, run_id, downstream, "native",
            ),
            control_action: None,
            pacer,
            initial_rate,
        }
    }

    /// Return `(min_delay, concurrency)` once per new operator request.
    pub fn pending_rate(&mut self) -> Option<(f64, Option<i64>)> {
        let rate = self.initial_rate.take();
        let rate = match rate {
            Some(rate) => Some(rate),
            None => consume_rate(&self.directory, &self.run_id).ok().flatten(),
        };
        if let (Some(rate), Some(pacer)) = (rate, self.pacer.as_mut()) {
            pacer.set_min_delay(rate.0);
        }
        rate
    }

    /// The pending scope-narrowing request, returned once, if any.
    pub fn pending_scope(&self) -> Option<Value> {
        consume_scope(&self.directory, &self.run_id).ok().flatten()
    }

    /// The pending operator URL injection, returned once, if any.
    pub fn pending_urls(&self) -> Option<Vec<String>> {
        consume_urls(&self.directory, &self.run_id).ok().flatten()
    }

    fn check(&mut self) -> Result<(), RunControlInterrupt> {
        if self.control_action.is_some() {
            return Ok(());
        }
        let Ok(action) = consume(&self.directory, &self.run_id) else {
            return Ok(());
        };
        if let Some(action) = action {
            self.control_action = Some(action.clone());
            return Err(RunControlInterrupt { action });
        }
        Ok(())
    }

    /// Check for control, then forward `(fetched, queued)`.
    pub fn call(&mut self, fetched: i64, queued: i64) -> Result<(), RunControlInterrupt> {
        self.check()?;
        self.reporter.call(fetched, queued);
        Ok(())
    }

    /// Check for control, then forward structured counters.
    pub fn observe_counts(&mut self, counters: &Value) -> Result<(), RunControlInterrupt> {
        self.check()?;
        self.reporter.observe_counts(counters);
        Ok(())
    }

    /// Record a phase boundary.
    pub fn enter(&mut self, name: &str, code: &str) {
        self.reporter.enter(name, code);
    }

    /// The wrapped reporter's current view.
    pub fn counters(&self) -> Value {
        self.reporter.counters()
    }
}

fn project_root(directory: &Path) -> ProjectsResult<PathBuf> {
    let info = open_project(directory, None)?;
    Ok(PathBuf::from(
        info["path"].as_str().unwrap_or_default().to_owned(),
    ))
}

fn connect(root: &Path) -> ProjectsResult<rusqlite::Connection> {
    let path = root.join(NAME);
    if path.is_symlink() {
        return Err(value("run control store must not be a symlink"));
    }
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .private_mode(0o600)
        .open(&path)
    {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(ProjectsError::Io(e)),
    }
    let meta = std::fs::symlink_metadata(&path).map_err(ProjectsError::Io)?;
    if meta.file_type().is_symlink() || !meta.is_file() || meta.mode() & 0o077 != 0 {
        return Err(value("run control store must be a private regular file"));
    }
    let con = rusqlite::Connection::open(&path).map_err(|e| value(e.to_string()))?;
    con.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| value(e.to_string()))?;
    con.execute_batch(
        "CREATE TABLE IF NOT EXISTS requests (run_id TEXT PRIMARY KEY, action TEXT NOT NULL, \
         requested_at TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'pending');\
         CREATE TABLE IF NOT EXISTS rates (run_id TEXT PRIMARY KEY, min_delay REAL NOT NULL, \
         concurrency INTEGER, requested_at TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'pending');\
         CREATE TABLE IF NOT EXISTS scopes (run_id TEXT PRIMARY KEY, exclude_pattern TEXT, \
         include_pattern TEXT, requested_at TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'pending');\
         CREATE TABLE IF NOT EXISTS urls (run_id TEXT PRIMARY KEY, urls_json TEXT NOT NULL, \
         requested_at TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'pending');",
    )
    .map_err(|e| value(e.to_string()))?;
    let columns: Vec<String> = {
        let mut stmt = con
            .prepare("PRAGMA table_info(requests)")
            .map_err(|e| value(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| value(e.to_string()))?;
        rows.collect::<Result<_, _>>()
            .map_err(|e| value(e.to_string()))?
    };
    if !columns.iter().any(|c| c == "state") {
        con.execute(
            "ALTER TABLE requests ADD COLUMN state TEXT NOT NULL DEFAULT 'pending'",
            [],
        )
        .map_err(|e| value(e.to_string()))?;
    }
    Ok(con)
}

/// True if the request was consumed; otherwise delete only a still-pending request.
fn inactive_request_receipt(root: &Path, run_id: &str, action: &str) -> ProjectsResult<bool> {
    let con = connect(root)?;
    con.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| value(e.to_string()))?;
    let result = (|| -> ProjectsResult<bool> {
        let row: Option<(String, String)> = con
            .query_row(
                "SELECT action, state FROM requests WHERE run_id=?1",
                rusqlite::params![run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();
        if row.as_ref().map(|(a, s)| (a.as_str(), s.as_str())) == Some((action, "applied")) {
            return Ok(true);
        }
        if row.as_ref().map(|(a, s)| (a.as_str(), s.as_str())) == Some((action, "pending")) {
            con.execute(
                "DELETE FROM requests WHERE run_id=?1 AND action=?2 AND state='pending'",
                rusqlite::params![run_id, action],
            )
            .map_err(|e| value(e.to_string()))?;
            let state: Option<String> = con
                .query_row(
                    "SELECT state FROM requests WHERE run_id=?1",
                    rusqlite::params![run_id],
                    |row| row.get(0),
                )
                .ok();
            if state.as_deref() == Some("applied") {
                return Ok(true);
            }
        }
        Ok(false)
    })();
    match result {
        Ok(value) => {
            let _ = con.execute_batch("COMMIT");
            Ok(value)
        }
        Err(e) => {
            let _ = con.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// The project root when `run_id` is a live native crawl writing its scan.
fn active_root(directory: &Path, run_id: &str) -> ProjectsResult<PathBuf> {
    if run_id.is_empty() || run_id.chars().count() > 64 {
        return Err(value(
            "run_id must be non-empty text no longer than 64 characters",
        ));
    }
    let root = project_root(directory)?;
    let rows = crate::run_observation::status(&root, crate::run_observation::MAX_RUNS as i64)?;
    let items = rows["items"].as_array().cloned().unwrap_or_default();
    let run = items
        .iter()
        .find(|item| item["id"].as_str() == Some(run_id));
    let Some(run) = run else {
        return Err(value(
            "run_id must identify an active native crawl in this project",
        ));
    };
    if run["state"].as_str() != Some("running")
        || run["source_kind"].as_str() != Some("native")
        || run["pid_state"].as_str() != Some("live")
    {
        return Err(value(
            "run_id must identify an active native crawl in this project",
        ));
    }
    let Some(artifact) = run.get("artifact").and_then(Value::as_str) else {
        return Err(value("native crawl has no durable scan to pause or stop"));
    };
    let unresolved = root.join(artifact);
    if unresolved.is_symlink() {
        return Err(value("native crawl scan path must not be a symlink"));
    }
    let path = unresolved.canonicalize().unwrap_or(unresolved);
    let resolved_root = root.canonicalize().unwrap_or_else(|_| root.clone());
    if !path.starts_with(&resolved_root) || !path.is_file() {
        return Err(value("native crawl scan path is unsafe or unavailable"));
    }
    let scan = spiderhead_store::scan_artifact::open_scan(&path, false, None)
        .map_err(|e| value(e.to_string()))?;
    let header: Option<String> = scan
        .query_row("SELECT lifecycle FROM scan WHERE singleton=1", [], |row| {
            row.get(0)
        })
        .ok();
    drop(scan);
    if header.as_deref() != Some("running") {
        return Err(value("native crawl scan is not running"));
    }
    Ok(root)
}

/// Validate an operator rate; return `(min_delay_seconds, concurrency)`.
pub fn checked_rate(rate: f64, concurrency: Option<i64>) -> ProjectsResult<(f64, Option<i64>)> {
    if !rate.is_finite() {
        return Err(value("rate must be a number of URLs per second"));
    }
    if !(rate > 0.0 && rate <= MAX_RATE) {
        return Err(value(format!(
            "rate must be above 0 and at most {MAX_RATE} URLs per second"
        )));
    }
    if let Some(c) = concurrency {
        if !(1..=MAX_CONCURRENCY).contains(&c) {
            return Err(value(format!(
                "concurrency must be an integer from 1 to {MAX_CONCURRENCY}"
            )));
        }
    }
    Ok((1.0 / rate, concurrency))
}

/// Queue a live rate change; a newer request replaces a pending one.
pub fn request_rate(
    directory: &Path,
    run_id: &str,
    rate: f64,
    concurrency: Option<i64>,
) -> ProjectsResult<Value> {
    let (min_delay, concurrency) = checked_rate(rate, concurrency)?;
    let root = active_root(directory, run_id)?;
    let con = connect(&root)?;
    con.execute(
        "INSERT INTO rates(run_id,min_delay,concurrency,requested_at,state) \
         VALUES(?1,?2,?3,?4,'pending') ON CONFLICT(run_id) DO UPDATE SET \
         min_delay=excluded.min_delay, concurrency=excluded.concurrency, \
         requested_at=excluded.requested_at, state='pending'",
        rusqlite::params![run_id, min_delay, concurrency, stamp()],
    )
    .map_err(|e| value(e.to_string()))?;
    Ok(json!({
        "ok": true,
        "run_id": run_id,
        "state": "rate_requested",
        "max_urls_per_second": rate,
        "concurrency": concurrency,
    }))
}

/// Consume the pending rate change for `run_id`, if any.
pub fn consume_rate(directory: &Path, run_id: &str) -> ProjectsResult<Option<(f64, Option<i64>)>> {
    let root = project_root(directory)?;
    if !root.join(NAME).exists() {
        return Ok(None);
    }
    let con = connect(&root)?;
    con.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| value(e.to_string()))?;
    let result = (|| -> ProjectsResult<Option<(f64, Option<i64>)>> {
        let row: Option<(f64, Option<i64>)> = con
            .query_row(
                "SELECT min_delay, concurrency FROM rates WHERE run_id=?1 AND state='pending'",
                rusqlite::params![run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();
        if row.is_some() {
            con.execute(
                "UPDATE rates SET state='applied' WHERE run_id=?1",
                rusqlite::params![run_id],
            )
            .map_err(|e| value(e.to_string()))?;
        }
        Ok(row)
    })();
    match result {
        Ok(row) => {
            con.execute_batch("COMMIT")
                .map_err(|e| value(e.to_string()))?;
            Ok(row)
        }
        Err(e) => {
            let _ = con.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Validate one operator URL-injection request.
pub fn checked_operator_urls(urls: &Value) -> ProjectsResult<Vec<String>> {
    let Some(list) = urls.as_array().filter(|l| !l.is_empty()) else {
        return Err(value("add-urls requires a nonempty list of URLs"));
    };
    if list.len() > MAX_STEER_URLS {
        return Err(value(format!(
            "add-urls accepts at most {MAX_STEER_URLS} URLs per request"
        )));
    }
    let mut cleaned: Vec<String> = Vec::with_capacity(list.len());
    for url in list {
        let raw = url.as_str().map(str::trim).filter(|s| !s.is_empty());
        let Some(raw) = raw else {
            return Err(value("operator URLs must be nonempty text"));
        };
        let candidate = raw.split('#').next().unwrap_or(raw).to_owned();
        let parts = match crate::urls::urlsplit(&candidate) {
            Ok(parts) => parts,
            Err(_) => {
                return Err(value(format!(
                    "operator URL must be an absolute http(s) URL: {raw}"
                )))
            }
        };
        if !["http", "https"].contains(&parts.scheme.to_lowercase().as_str())
            || parts.netloc.is_empty()
        {
            return Err(value(format!(
                "operator URL must be an absolute http(s) URL: {raw}"
            )));
        }
        cleaned.push(candidate);
    }
    let mut unique = Vec::new();
    for url in cleaned {
        if !unique.contains(&url) {
            unique.push(url);
        }
    }
    if unique.len() > MAX_STEER_URLS {
        return Err(value(format!(
            "add-urls accepts at most {MAX_STEER_URLS} distinct URLs per request"
        )));
    }
    Ok(unique)
}
