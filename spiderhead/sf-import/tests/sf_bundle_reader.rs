//! discovery, bundle materialization and failure-instruction tests. The
//! jpype integration tail (`_derby_stack`, `_create_mini_crawl`) needs a real
//! JVM and stays untranslated, exactly like the reference skip.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde_json::json;
use spiderhead_sf_import::bundle_reader as br;

// --- fake JDBC layer ---------------------------------------------------------

/// One result cell: string, integer or SQL NULL.
#[derive(Clone)]
#[allow(dead_code)] // SQL NULL exists on the JDBC seam even when no test uses it
enum Col {
    Str(&'static str),
    Int(i64),
    Null,
}

type Rows = Vec<Vec<Col>>;
type Routed = Result<(Vec<&'static str>, Rows), String>;

struct FakeResultSet {
    columns: Vec<String>,
    rows: Rows,
    pos: usize,
}

impl br::JdbcResultSet for FakeResultSet {
    fn next(&mut self) -> Result<bool, br::ConnectError> {
        if self.pos < self.rows.len() {
            self.pos += 1;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn get_string(&mut self, index: usize) -> Result<Option<String>, br::ConnectError> {
        Ok(match &self.rows[self.pos - 1][index - 1] {
            Col::Str(s) => Some((*s).to_owned()),
            Col::Int(i) => Some(i.to_string()),
            Col::Null => None,
        })
    }

    fn get_long(&mut self, index: usize) -> Result<i64, br::ConnectError> {
        match &self.rows[self.pos - 1][index - 1] {
            Col::Int(i) => Ok(*i),
            Col::Str(s) => s.parse::<i64>().map_err(|e| conn_err(e.to_string())),
            Col::Null => Err(conn_err("int(None)")),
        }
    }

    fn column_labels(&self) -> Vec<String> {
        self.columns.clone()
    }
}

fn conn_err(message: impl Into<String>) -> br::ConnectError {
    br::ConnectError {
        message: message.into(),
        sql_state: String::new(),
    }
}

struct Log {
    sql: Vec<String>,
    timeouts: Vec<i64>,
}

struct FakeStatement {
    router: Rc<dyn Fn(&str) -> Routed>,
    log: Rc<RefCell<Log>>,
}

impl br::JdbcStatement for FakeStatement {
    fn set_query_timeout(&mut self, seconds: i64) {
        self.log.borrow_mut().timeouts.push(seconds);
    }

    fn execute_query(&mut self, sql: &str) -> Result<Box<dyn br::JdbcResultSet>, br::ConnectError> {
        self.log.borrow_mut().sql.push(sql.to_owned());
        let (columns, rows) = (self.router)(sql).map_err(conn_err)?;
        Ok(Box::new(FakeResultSet {
            columns: columns.iter().map(|c| c.to_string()).collect(),
            rows,
            pos: 0,
        }))
    }

    fn close(&mut self) {}
}

struct FakeConnection {
    router: Rc<dyn Fn(&str) -> Routed>,
    log: Rc<RefCell<Log>>,
    closed: std::cell::Cell<bool>,
    read_only: std::cell::Cell<Option<bool>>,
}

impl FakeConnection {
    fn new(router: impl Fn(&str) -> Routed + 'static) -> (Self, Rc<RefCell<Log>>) {
        let log = Rc::new(RefCell::new(Log {
            sql: Vec::new(),
            timeouts: Vec::new(),
        }));
        (
            Self {
                router: Rc::new(router),
                log: log.clone(),
                closed: std::cell::Cell::new(false),
                read_only: std::cell::Cell::new(None),
            },
            log,
        )
    }
}

impl br::JdbcConnection for FakeConnection {
    fn create_statement(&self) -> Box<dyn br::JdbcStatement> {
        Box::new(FakeStatement {
            router: self.router.clone(),
            log: self.log.clone(),
        })
    }

    fn database_product_version(&self) -> String {
        "10.17.1.0 - (fake)".to_owned()
    }

    fn set_read_only(&mut self, value: bool) {
        self.read_only.set(Some(value));
    }

    fn close(&mut self) {
        self.closed.set(true);
    }
}

/// `_sf_router`: the canned catalog of a small crawl bundle.
#[derive(Default)]
struct Route {
    tables: Vec<(&'static str, &'static str)>,
    counts: HashMap<(String, String), i64>,
    url_columns: Vec<(&'static str, Vec<&'static str>)>,
    start_url: Option<&'static str>,
    depth_zero: Option<&'static str>,
    version_row: Option<Rows>,
}

fn sf_router(route: Route) -> impl Fn(&str) -> Routed {
    move |sql: &str| -> Routed {
        if sql.contains("SYS.SYSCOLUMNS") {
            let mut rows: Rows = Vec::new();
            for (table, cols) in &route.url_columns {
                for col in cols {
                    rows.push(vec![
                        Col::Str(Box::leak((*table).to_string().into_boxed_str())),
                        Col::Str(Box::leak((*col).to_string().into_boxed_str())),
                    ]);
                }
            }
            return Ok((vec!["TABLENAME", "COLUMNNAME"], rows));
        }
        if sql.contains("SYS.SYSTABLES") {
            let rows = route
                .tables
                .iter()
                .map(|(s, t)| vec![Col::Str(s), Col::Str(t)])
                .collect();
            return Ok((vec!["SCHEMANAME", "TABLENAME"], rows));
        }
        if sql.contains("COUNT(*)") {
            let Some(pos) = sql.find("FROM \"") else {
                return Err(format!("unexpected count target: {sql}"));
            };
            let tail = &sql[pos + 5..];
            let mut parts = tail.split("\".\"");
            let schema = parts.next().unwrap_or_default().trim_matches('"');
            let table = parts.next().unwrap_or_default().trim_matches('"');
            let Some(total) = route
                .counts
                .get(&(schema.to_owned(), table.to_owned()))
                .copied()
            else {
                return Err(format!("unexpected count target: {sql}"));
            };
            return Ok((vec!["1"], vec![vec![Col::Int(total)]]));
        }
        if sql.contains("ROOT_URL") && sql.contains("FETCH FIRST") {
            let rows = route
                .start_url
                .map(|u| vec![vec![Col::Str(u)]])
                .unwrap_or_default();
            return Ok((vec!["ROOT_URL"], rows));
        }
        if sql.contains("CRAWL_DEPTH = 0") {
            let rows = route
                .depth_zero
                .map(|u| vec![vec![Col::Str(u)]])
                .unwrap_or_default();
            return Ok((vec!["ENCODED_URL"], rows));
        }
        if sql.contains("WAITING") && sql.contains("ENCODED_URL") {
            return Ok((vec!["ENCODED_URL"], vec![]));
        }
        if sql.starts_with("SELECT *") {
            let Some(rows) = route.version_row.clone() else {
                return Err(format!("unexpected probe: {sql}"));
            };
            return Ok((vec!["RECID", "MAJOR_VERSION"], rows));
        }
        Err(format!("unexpected sql: {sql}"))
    }
}

fn base_route() -> Route {
    Route {
        tables: vec![("APP", "URLS"), ("APP", "LINKS"), ("APP", "WAITING_URLS")],
        counts: HashMap::from([
            (("APP".to_owned(), "URLS".to_owned()), 87),
            (("APP".to_owned(), "LINKS".to_owned()), 220),
            (("APP".to_owned(), "WAITING_URLS".to_owned()), 3),
        ]),
        ..Route::default()
    }
}

/// `_opened(conn, tmp_path)`: a directory-kind connection over the fake.
fn opened(conn: FakeConnection, temp: &Path) -> br::BundleConnection {
    br::BundleConnection::new(
        Box::new(conn),
        temp.join("db"),
        1234,
        temp.to_path_buf(),
        "directory",
    )
}

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

// --- inventory over the fake connection --------------------------------------

#[test]
fn inventory_lists_every_table_with_counts() {
    let tmp = tempdir();
    let (conn, _log) = FakeConnection::new(sf_router(base_route()));
    let report = br::inventory(&opened(conn, tmp.path()), "crawl.seospider").expect("inventory");
    assert_eq!(
        report["tables"],
        json!([
            {"name": "APP.URLS", "rows": 87},
            {"name": "APP.LINKS", "rows": 220},
            {"name": "APP.WAITING_URLS", "rows": 3},
        ])
    );
    assert_eq!(report["tables_total"], json!(3));
    assert_eq!(report["rows_counted"], json!(3));
    assert!(report["derby_version"]
        .as_str()
        .unwrap()
        .starts_with("10.17"));
    assert_eq!(report["db_bytes"], json!(1234));
    assert_eq!(report["bundle"], json!("crawl.seospider"));
}

#[test]
fn row_counts_use_quoted_qualified_names_and_timeouts() {
    let tmp = tempdir();
    let (conn, log) = FakeConnection::new(sf_router(base_route()));
    let _ = br::inventory(&opened(conn, tmp.path()), "x").expect("inventory");
    let log = log.borrow();
    assert!(log
        .sql
        .iter()
        .any(|s| s == "SELECT COUNT(*) FROM \"APP\".\"URLS\""));
    assert_eq!(log.timeouts, vec![br::QUERY_TIMEOUT_SECONDS; log.sql.len()]);
}

#[test]
fn row_count_cap_keeps_listing_but_stops_counting() {
    let tmp = tempdir();
    let many: Vec<(&'static str, String)> = (0..25).map(|i| ("APP", format!("T{i:02}"))).collect();
    let mut route = Route::default();
    for (i, (s, t)) in many.iter().enumerate() {
        let leaked: &'static str = Box::leak(t.clone().into_boxed_str());
        route.tables.push((*s, leaked));
        route.counts.insert((s.to_string(), t.clone()), i as i64);
    }
    let (conn, log) = FakeConnection::new(sf_router(route));
    let report = br::inventory(&opened(conn, tmp.path()), "x").expect("inventory");
    assert_eq!(report["tables_total"], json!(25));
    assert_eq!(report["rows_counted"], json!(br::ROW_COUNT_LIMIT));
    assert!(report["notes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n.as_str().unwrap().contains("capped at 20")));
    let counted = log
        .borrow()
        .sql
        .iter()
        .filter(|s| s.contains("COUNT(*)"))
        .count();
    assert_eq!(counted, br::ROW_COUNT_LIMIT);
}

#[test]
fn start_url_prefers_the_frontier_root() {
    let tmp = tempdir();
    let mut route = base_route();
    route.url_columns = vec![("WAITING_URLS", vec!["ROOT_URL"])];
    route.start_url = Some("https://example.org/");
    let (conn, _log) = FakeConnection::new(sf_router(route));
    let report = br::inventory(&opened(conn, tmp.path()), "x").expect("inventory");
    assert_eq!(report["start_url"], json!("https://example.org/"));
}

#[test]
fn start_url_falls_back_to_depth_zero() {
    let tmp = tempdir();
    let mut route = base_route();
    route.url_columns = vec![("URLS", vec!["ENCODED_URL", "CRAWL_DEPTH"])];
    route.depth_zero = Some("https://example.org/");
    let (conn, log) = FakeConnection::new(sf_router(route));
    let report = br::inventory(&opened(conn, tmp.path()), "x").expect("inventory");
    assert_eq!(report["start_url"], json!("https://example.org/"));
    assert!(log
        .borrow()
        .sql
        .iter()
        .any(|s| s.contains("CRAWL_DEPTH = 0")));
}

#[test]
fn start_url_stays_absent_when_the_schema_has_no_candidate() {
    let tmp = tempdir();
    let (conn, _log) = FakeConnection::new(sf_router(base_route()));
    let report = br::inventory(&opened(conn, tmp.path()), "x").expect("inventory");
    assert!(report["start_url"].is_null());
    assert_eq!(report["notes"], json!([]));
}

#[test]
fn software_version_table_is_reported_when_present() {
    let tmp = tempdir();
    let mut route = base_route();
    route.tables.push(("APP", "SOFTWARE_VERSION"));
    route.version_row = Some(vec![vec![Col::Int(1), Col::Int(23)]]);
    let (conn, _log) = FakeConnection::new(sf_router(route));
    let report = br::inventory(&opened(conn, tmp.path()), "x").expect("inventory");
    assert_eq!(
        report["software_version"],
        json!({
            "table": "APP.SOFTWARE_VERSION",
            "row": {"RECID": "1", "MAJOR_VERSION": "23"},
        })
    );
}

#[test]
fn a_failing_count_becomes_a_note_not_an_error() {
    let tmp = tempdir();
    let mut route = base_route();
    route.counts.remove(&("APP".to_owned(), "LINKS".to_owned()));
    let (conn, _log) = FakeConnection::new(sf_router(route));
    let report = br::inventory(&opened(conn, tmp.path()), "x").expect("inventory");
    assert!(report["notes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n.as_str().unwrap().contains("APP.LINKS: row count failed")));
    assert_eq!(report["rows_counted"], json!(2));
}

#[test]
fn inspect_bundle_wraps_the_connection() {
    // `inspect_bundle` wraps `open_bundle` + `inventory` and drops the
    // connection; the same report fields come straight from `inventory` on
    // the fake connection.
    let tmp = tempdir();
    let (conn, _log) = FakeConnection::new(sf_router(base_route()));
    let report = br::inventory(&opened(conn, tmp.path()), "crawl.seospider").expect("inventory");
    assert_eq!(report["bundle"], json!("crawl.seospider"));
    assert_eq!(report["tables"][0]["name"], json!("APP.URLS"));
    assert_eq!(report["bundle_kind"], json!("directory"));
}

// --- jar and JVM discovery ---------------------------------------------------

fn write(path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, b"PK").expect("write");
}

#[test]
fn find_derby_jar_explicit_path_wins() {
    let tmp = tempdir();
    let jar = tmp.path().join("derby-10.17.1.0.jar");
    write(&jar);
    assert_eq!(br::find_derby_jar(std::slice::from_ref(&jar)), Some(jar));
}

#[test]
fn find_derby_jar_env_var() {
    let tmp = tempdir();
    let jar = tmp.path().join("derby.jar");
    write(&jar);
    assert_eq!(
        br::find_derby_jar_in(&[], Some(jar.to_str().unwrap()), &[]),
        Some(jar)
    );
}

#[test]
fn find_derby_jar_found_in_downloads() {
    let tmp = tempdir();
    let jar = tmp
        .path()
        .join("Downloads/db-derby-10.17.1.0-bin/lib/derby-10.17.1.0.jar");
    write(&jar);
    let pattern = tmp
        .path()
        .join("Downloads/db-derby-*-bin/lib/derby*.jar")
        .to_string_lossy()
        .into_owned();
    assert_eq!(br::find_derby_jar_in(&[], None, &[pattern]), Some(jar));
}

#[test]
fn find_derby_jar_prefers_the_newer_version() {
    let tmp = tempdir();
    let old = tmp.path().join("derby-10.14.2.0.jar");
    let new = tmp.path().join("derby-10.17.1.0.jar");
    write(&old);
    write(&new);
    assert_eq!(br::find_derby_jar(&[old, new.clone()]), Some(new));
}

#[test]
fn find_derby_jar_skips_client_and_source_jars() {
    let tmp = tempdir();
    let client = tmp.path().join("derbyclient-10.17.1.0.jar");
    let sources = tmp.path().join("derby-sources-10.17.1.0.jar");
    let engine = tmp.path().join("derby-10.17.1.0.jar");
    write(&client);
    write(&sources);
    write(&engine);
    assert_eq!(
        br::find_derby_jar(&[client, sources, engine.clone()]),
        Some(engine)
    );
}

#[test]
fn find_derby_jar_returns_none_without_candidates() {
    assert_eq!(br::find_derby_jar_in(&[], None, &[]), None);
}

#[test]
fn companion_jars_picks_up_derbyshared() {
    let tmp = tempdir();
    let engine = tmp.path().join("derby-10.17.1.0.jar");
    let shared = tmp.path().join("derbyshared-10.17.1.0.jar");
    write(&engine);
    write(&shared);
    assert_eq!(br::companion_jars(&engine), vec![shared]);
}

#[test]
fn find_default_jvm_env_wins() {
    let tmp = tempdir();
    let jvm = tmp.path().join("libjvm.dylib");
    write(&jvm);
    assert_eq!(
        br::find_default_jvm_in(Some(jvm.to_string_lossy().into_owned()), None, &[]),
        Some(jvm)
    );
}

#[test]
fn find_default_jvm_java_home() {
    let tmp = tempdir();
    let lib = tmp.path().join("lib/server/libjvm.so");
    write(&lib);
    assert_eq!(
        br::find_default_jvm_in(None, Some(tmp.path().to_string_lossy().into_owned()), &[]),
        Some(lib)
    );
}

#[test]
fn find_default_jvm_none_without_locations() {
    assert_eq!(br::find_default_jvm_in(None, None, &[]), None);
}

// --- bundle resolution --------------------------------------------------------

/// `_derby_dir`: a minimal Derby-looking directory.
fn derby_dir(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path.join("seg0")).expect("seg0");
    std::fs::write(path.join("seg0/c10.dat"), b"x").expect("dat");
    std::fs::write(path.join("service.properties"), "derby.version=10.17\n").expect("props");
    path.to_path_buf()
}

#[test]
fn find_db_dir_prefers_the_conventional_db_directory() {
    let tmp = tempdir();
    let bundle = tmp.path().join("crawl.seospider");
    derby_dir(&bundle.join("config/olddb"));
    let expected = derby_dir(&bundle.join("db"));
    std::fs::write(bundle.join("spiderconfig"), "config").expect("spiderconfig");
    assert_eq!(br::find_db_dir(&bundle).expect("db dir"), expected);
}

#[test]
fn find_db_dir_accepts_the_database_directory_itself() {
    let tmp = tempdir();
    let db = derby_dir(&tmp.path().join("db"));
    assert_eq!(br::find_db_dir(&db).expect("db dir"), db);
}

#[test]
fn find_db_dir_names_what_it_saw_when_nothing_matches() {
    let tmp = tempdir();
    let bundle = tmp.path().join("crawl.seospider");
    std::fs::create_dir_all(bundle.join("exports")).expect("exports");
    let err = br::find_db_dir(&bundle).expect_err("must fail");
    assert!(err.0.contains("exports"), "message: {}", err.0);
}

#[test]
fn materialize_extracts_a_zip_bundle() {
    let tmp = tempdir();
    let src = tmp.path().join("src");
    let db = derby_dir(&src.join("db"));
    std::fs::write(src.join("spiderconfig"), "config").expect("spiderconfig");
    let archive = tmp.path().join("crawl.seospider");
    {
        let file = std::fs::File::create(&archive).expect("archive");
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for entry in std::fs::read_dir(&src).expect("src") {
            let entry = entry.expect("entry").path();
            if entry.is_dir() {
                for inner in walk(&entry) {
                    let rel = inner.strip_prefix(&src).unwrap().to_path_buf();
                    if inner.is_dir() {
                        zip.add_directory(rel.to_string_lossy(), options)
                            .expect("dir entry");
                    } else {
                        zip.start_file(rel.to_string_lossy(), options)
                            .expect("file entry");
                        std::io::copy(&mut std::fs::File::open(&inner).unwrap(), &mut zip)
                            .expect("copy");
                    }
                }
            } else {
                zip.start_file(entry.file_name().unwrap().to_string_lossy(), options)
                    .expect("file entry");
                std::io::copy(&mut std::fs::File::open(&entry).unwrap(), &mut zip).expect("copy");
            }
        }
        zip.finish().expect("finish");
    }
    let _ = db;
    let temp = tmp.path().join("temp");
    std::fs::create_dir_all(&temp).expect("temp");
    let (resolved, kind, total) = br::materialize(&archive, &temp).expect("materialize");
    assert_eq!(kind, "archive");
    assert_eq!(resolved, temp.join("bundle").join("db"));
    assert!(total > 0);
    assert!(resolved.join("service.properties").is_file());
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path.clone());
            }
            out.push(path);
        }
    }
    out
}

#[test]
fn materialize_copies_a_directory_bundle_and_leaves_it_untouched() {
    let tmp = tempdir();
    let bundle = tmp.path().join("crawl.seospider");
    let db = derby_dir(&bundle.join("db"));
    let mut before: Vec<String> = walk(&db)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    before.sort();
    let temp = tmp.path().join("temp");
    std::fs::create_dir_all(&temp).expect("temp");
    let (resolved, kind, total) = br::materialize(&bundle, &temp).expect("materialize");
    assert_eq!(kind, "directory");
    assert_eq!(resolved, temp.join("db"));
    assert!(resolved.join("service.properties").is_file());
    assert!(total > 0);
    let mut after: Vec<String> = walk(&db)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    after.sort();
    assert_eq!(before, after, "nothing added in place");
    assert!(walk(&db)
        .iter()
        .all(|p| p.extension().is_none_or(|e| e != "lck")));
}

#[test]
fn materialize_rejects_a_non_zip_file() {
    let tmp = tempdir();
    let plain = tmp.path().join("crawl.seospider");
    std::fs::write(&plain, "not a zip").expect("write");
    let err = br::materialize(&plain, tmp.path()).expect_err("must fail");
    assert!(err.0.contains("not a ZIP"), "message: {}", err.0);
}

#[test]
fn materialize_reports_a_missing_bundle() {
    let tmp = tempdir();
    let err =
        br::materialize(&tmp.path().join("absent.seospider"), tmp.path()).expect_err("must fail");
    assert!(err.0.contains("not found"), "message: {}", err.0);
}

// --- failure instructions -----------------------------------------------------

#[test]
fn open_bundle_names_the_fix_when_no_jar_exists() {
    let tmp = tempdir();
    let no_jar = |_: &[PathBuf]| None;
    let Err(err) = br::open_bundle_in(&tmp.path().join("whatever"), None, None, &no_jar, &|| None)
    else {
        panic!("must fail")
    };
    assert!(err.0.contains("--derby-jar"));
    assert!(err.0.contains(br::DERBY_JAR_ENV));
    assert!(err.0.contains("https://db.apache.org/derby/releases"));
}

#[test]
fn open_bundle_names_the_extra_when_jpype_is_missing() {
    let tmp = tempdir();
    let jar = tmp.path().join("derby-10.17.1.0.jar");
    write(&jar);
    let found = jar.clone();
    let jar_finder = move |_: &[PathBuf]| Some(found.clone());
    let err = br::open_bundle_in(
        &tmp.path().join("whatever"),
        None,
        None,
        &jar_finder,
        &|| None,
    )
    .err()
    .expect("must fail");
    assert!(err.0.contains("spiderhead[derby]"), "message: {}", err.0);
}

#[test]
fn load_jpype_error_message_is_actionable() {
    let tmp = tempdir();
    let jar = tmp.path().join("derby-10.17.1.0.jar");
    write(&jar);
    let found = jar.clone();
    let jar_finder = move |_: &[PathBuf]| Some(found.clone());
    let err = br::open_bundle_in(
        &tmp.path().join("whatever"),
        None,
        None,
        &jar_finder,
        &|| None,
    )
    .err()
    .expect("must fail");
    assert!(err.0.contains("pip install"), "message: {}", err.0);
}
