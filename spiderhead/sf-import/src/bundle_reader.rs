//! crawl bundle backed by an embedded Apache Derby database.
//!
//! The filesystem side (jar/JVM discovery, bundle materialization, safe copy)
//! is direct Rust. The JDBC side goes through a [`JvmBridge`] seam: reference
//! reaches Derby through `jpype` and `java.sql.DriverManager`; the Rust port
//! keeps identical call order, timeouts and error texts while the caller
//! supplies whatever actually hosts the JVM (tests inject fakes, exactly like
//! the reference fake-JDBC layer). The user's bundle is never written to: the
//! database directory is copied to a temporary location and opened read-only.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde_json::{json, Map, Value};
use thiserror::Error;

/// JDBC driver class the bridge must resolve before connecting.
pub const DERBY_DRIVER_CLASS: &str = "org.apache.derby.jdbc.EmbeddedDriver";
/// Row counting is capped so a pathological bundle cannot turn `--inspect`
/// into an hour of full scans; tables past the cap are still listed, just not
/// counted.
pub const ROW_COUNT_LIMIT: usize = 20;
/// Per-statement timeout applied to every JDBC query.
pub const QUERY_TIMEOUT_SECONDS: i64 = 15;
/// Copying a multi-GB bundle into a small temp filesystem fails mid-copy with
/// an opaque ENOSPC; refusing up front names the actual problem.
pub const MAX_COPY_BYTES: u64 = 10 * 1024 * 1024 * 1024;

/// Environment variable pinning the Derby engine jar.
pub const DERBY_JAR_ENV: &str = "SPIDERHEAD_DERBY_JAR";
/// Environment variable pinning the JVM library.
pub const JVM_ENV: &str = "SPIDERHEAD_JVM";

// The catalog stores unquoted identifiers upper-cased and quoted ones
// verbatim; every name read back from SYS.SYSTABLES is used verbatim in
// quoted identifiers.
const TABLES_SQL: &str = "SELECT S.SCHEMANAME, T.TABLENAME FROM SYS.SYSTABLES T \
     JOIN SYS.SYSSCHEMAS S ON S.SCHEMAID = T.SCHEMAID \
     WHERE T.TABLETYPE = 'T' ORDER BY S.SCHEMANAME, T.TABLENAME";
const URL_COLUMNS_SQL: &str = "SELECT T.TABLENAME, C.COLUMNNAME FROM SYS.SYSCOLUMNS C \
     JOIN SYS.SYSTABLES T ON C.REFERENCEID = T.TABLEID \
     WHERE T.TABLETYPE = 'T' AND C.COLUMNNAME IN ('ROOT_URL', 'ENCODED_URL', 'CRAWL_DEPTH')";

/// `_JAR_INSTRUCTIONS`: the actionable message when no derby.jar exists.
pub const JAR_INSTRUCTIONS: &str = "derby.jar was not found on this machine. Take any one of:\n  \
     1. pass it explicitly:       --derby-jar /path/to/derby.jar\n  \
     2. or export the path:       SPIDERHEAD_DERBY_JAR=/path/to/derby.jar\n  \
     3. or download Apache Derby (any 10.x, DIB zip) from \
     https://db.apache.org/derby/releases, unzip, and leave lib/derby.jar in \
     ~/Downloads (searched automatically)\n  \
     4. an installed Screaming Frog ships its own derby.jar (searched automatically)";
/// `_JVM_INSTRUCTIONS`: the actionable message when no JVM can host the jar.
pub const JVM_INSTRUCTIONS: &str = "no Java virtual machine found to run derby.jar. Checked \
     SPIDERHEAD_JVM, JAVA_HOME, the bundled Screaming Frog JRE, and the \
     standard JDK locations. Install a JDK 17+ or set SPIDERHEAD_JVM to the \
     JVM library (libjvm.dylib / libjvm.so / jvm.dll).";
/// `_JPYPE_INSTRUCTIONS`: names the optional extra that carries the bridge.
pub const JPYPE_INSTRUCTIONS: &str =
    "reading a saved crawl needs the JVM bridge: pip install \"spiderhead[derby]\"";

/// `BundleError`: a user-facing reason why a saved-crawl bundle could not be
/// read.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct BundleError(pub String);

fn err(message: impl Into<String>) -> BundleError {
    BundleError(message.into())
}

/// A JDBC-layer failure carrying the optional `SQLState` the Derby exception
/// exposes (`getSQLState()`); needed by [`connection_error`] triage.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct ConnectError {
    /// `str(exception)` text.
    pub message: String,
    /// `getSQLState()` value, empty when absent.
    pub sql_state: String,
}

/// The JDBC `ResultSet` surface the inventory drives.
pub trait JdbcResultSet {
    /// Advance to the next row; `false` at the end.
    fn next(&mut self) -> Result<bool, ConnectError>;
    /// `getString(index)` — `None` for SQL NULL (Java null).
    fn get_string(&mut self, index: usize) -> Result<Option<String>, ConnectError>;
    /// `getLong(index)`.
    fn get_long(&mut self, index: usize) -> Result<i64, ConnectError>;
    /// `getMetaData().getColumnLabel(i)` for every column, in order.
    fn column_labels(&self) -> Vec<String>;
}

/// The JDBC `Statement` surface (always closed after use in reference).
pub trait JdbcStatement {
    /// `setQueryTimeout` — recorded before every query.
    fn set_query_timeout(&mut self, seconds: i64);
    /// `executeQuery`.
    fn execute_query(&mut self, sql: &str) -> Result<Box<dyn JdbcResultSet>, ConnectError>;
    /// `close` — failures are suppressed by callers anyway.
    fn close(&mut self);
}

/// The JDBC `Connection` surface, read-only.
pub trait JdbcConnection {
    /// `createStatement`.
    fn create_statement(&self) -> Box<dyn JdbcStatement>;
    /// `getMetaData().getDatabaseProductVersion()`.
    fn database_product_version(&self) -> String;
    /// `setReadOnly` — applied best-effort after connecting.
    fn set_read_only(&mut self, value: bool);
    /// `close` — releases the JDBC handle on the copied database.
    fn close(&mut self);
}

/// The `jpype`/`java.sql` seam: JVM lifecycle plus the two connect paths the
/// reference module uses. Tests inject fakes exactly like the reference layer does.
pub trait JvmBridge {
    /// `jpype.isJVMStarted()`.
    fn is_jvm_started(&self) -> bool;
    /// `jpype.startJVM(jvm, classpath=...)` — `Err` is the OSError/RuntimeError
    /// text reference would surface.
    fn start_jvm(&self, jvm: &Path, classpath: &[String]) -> Result<(), String>;
    /// Whether `jpype.JClass(name)` resolves (TypeError/OSError otherwise).
    fn class_loadable(&self, name: &str) -> bool;
    /// `java.lang.System.setProperty`; errors are suppressed by design.
    fn set_property(&self, key: &str, value: &str);
    /// `DriverManager.getConnection(url)`.
    fn driver_manager_connect(&self, url: &str) -> Result<Box<dyn JdbcConnection>, ConnectError>;
    /// `DerbyDriver().connect(url, None)` — the explicit fallback.
    fn driver_connect(&self, url: &str) -> Result<Box<dyn JdbcConnection>, ConnectError>;
}

// --- jar and JVM discovery ---------------------------------------------------

/// `Path.home()` / `os.path.expanduser("~")` equivalent.
fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `Path(raw).expanduser()`: `~`/`~/x` expand to the home directory;
/// `~user/...` is left as-is (pwd lookup has no std equivalent).
fn expand_user(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if text == "~" {
        return home_dir();
    }
    if let Some(rest) = text.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    path.to_path_buf()
}

/// `_derby_version`: `(10, 17, 1)` from `derby-10.17.1.0.jar`; plain
/// `derby.jar` sorts oldest. `re.search` semantics — the pattern only needs
/// to appear at the end of the name.
fn derby_version(path: &Path) -> Vec<u64> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Some(pos) = name.rfind("derby-") else {
        return Vec::new();
    };
    let tail = &name[pos + 6..];
    let Some(digits) = tail.strip_suffix(".jar") else {
        return Vec::new();
    };
    // `\d+(?:\.\d+)*` — every dot-separated part must be nonempty digits.
    let parts: Vec<&str> = digits.split('.').collect();
    if parts
        .iter()
        .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    {
        parts
            .iter()
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect()
    } else {
        Vec::new()
    }
}

/// `_is_engine_jar`: the embedded engine jar, not a client/server/sources/
/// javadoc sibling.
fn is_engine_jar(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !name.starts_with("derby") || !name.ends_with(".jar") {
        return false;
    }
    for excluded in ["derbyclient", "derbynet", "derbyrun", "derbywar"] {
        if name.starts_with(excluded) {
            return false;
        }
    }
    !name.contains("sources") && !name.contains("javadoc") && !name.contains("tests")
}

/// `_search_globs`: every pattern worth scanning for derby.jar, newest
/// version wins.
pub fn search_globs() -> Vec<String> {
    let home = home_dir();
    vec![
        "/Applications/Screaming Frog SEO Spider.app/Contents/Java/lib/derby*.jar".to_owned(),
        "/Applications/ScreamingFrogSEOSpider.app/Contents/Java/lib/derby*.jar".to_owned(),
        "C:/Program Files/Screaming Frog SEO Spider/lib/derby*.jar".to_owned(),
        "C:/Program Files (x86)/Screaming Frog SEO Spider/lib/derby*.jar".to_owned(),
        "/opt/screamingfrogseospider/lib/derby*.jar".to_owned(),
        home.join("Downloads/db-derby-*-bin/lib/derby*.jar")
            .to_string_lossy()
            .into_owned(),
        home.join("Downloads/derby*/lib/derby*.jar")
            .to_string_lossy()
            .into_owned(),
        home.join(".m2/repository/org/apache/derby/derby/*/derby-*.jar")
            .to_string_lossy()
            .into_owned(),
        "/opt/homebrew/opt/derby/lib/derby*.jar".to_owned(),
        "/usr/local/opt/derby/lib/derby*.jar".to_owned(),
        "/usr/share/java/derby*.jar".to_owned(),
    ]
}

/// `_pick_newest`: newest Derby engine among valid candidates; `None` when
/// nothing is usable. reference `max` returns the FIRST element on key ties.
fn pick_newest(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut best: Option<PathBuf> = None;
    let mut best_key: Option<(Vec<u64>, f64)> = None;
    for path in paths {
        if !path.is_file() || !is_engine_jar(path) {
            continue;
        }
        let mtime = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let key = (derby_version(path), mtime);
        if best_key.as_ref().is_none_or(|b| key > *b) {
            best_key = Some(key);
            best = Some(path.clone());
        }
    }
    best
}

/// Expand one glob pattern, suppressing errors like `suppress(OSError)` does
/// around `glob.glob`.
fn glob_paths(pattern: &str) -> Vec<PathBuf> {
    glob::glob(pattern)
        .map(|entries| entries.flatten().collect())
        .unwrap_or_default()
}

/// `find_derby_jar`: locate a usable derby.jar — explicit paths, then env,
/// then known installs.
pub fn find_derby_jar(paths: &[PathBuf]) -> Option<PathBuf> {
    find_derby_jar_in(
        paths,
        std::env::var(DERBY_JAR_ENV).ok().as_deref(),
        &search_globs(),
    )
}

/// `find_derby_jar` with the env value and glob patterns injected — the seam
/// the reference tests reach by monkeypatching `_search_globs`.
pub fn find_derby_jar_in(
    paths: &[PathBuf],
    env_value: Option<&str>,
    patterns: &[String],
) -> Option<PathBuf> {
    let explicit: Vec<PathBuf> = paths
        .iter()
        .filter(|raw| !raw.as_os_str().is_empty())
        .map(|raw| expand_user(raw))
        .collect();
    if let Some(found) = pick_newest(&explicit) {
        return Some(found);
    }
    let env_paths: Vec<PathBuf> = env_value
        .unwrap_or("")
        .split(if cfg!(windows) { ';' } else { ':' })
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .map(|raw| expand_user(Path::new(raw)))
        .collect();
    if let Some(found) = pick_newest(&env_paths) {
        return Some(found);
    }
    let mut discovered: Vec<PathBuf> = Vec::new();
    for pattern in patterns {
        discovered.extend(glob_paths(pattern));
    }
    pick_newest(&discovered)
}

/// `companion_jars`: Derby 10.15+ splits shared classes into derbyshared.jar
/// next to the engine.
pub fn companion_jars(jar: &Path) -> Vec<PathBuf> {
    let parent = jar.parent().unwrap_or_else(|| Path::new(""));
    let mut found: Vec<PathBuf> = Vec::new();
    for name in ["derbyshared*.jar", "derbytools*.jar"] {
        for path in glob_paths(&parent.join(name).to_string_lossy()) {
            if path.is_file() {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// `_jvm_globs`: standard per-OS JVM library locations, plus the bundled JRE.
pub fn jvm_globs() -> Vec<String> {
    vec![
        "/Applications/Screaming Frog SEO Spider.app/Contents/jre/lib/server/libjvm.dylib"
            .to_owned(),
        "/Applications/ScreamingFrogSEOSpider.app/Contents/jre/lib/server/libjvm.dylib".to_owned(),
        "C:/Program Files/Screaming Frog SEO Spider/jre/bin/server/jvm.dll".to_owned(),
        "/Library/Java/JavaVirtualMachines/*/Contents/Home/lib/server/libjvm.dylib".to_owned(),
        "/usr/lib/jvm/*/lib/server/libjvm.so".to_owned(),
        "/usr/lib/jvm/*/jre/lib/amd64/server/libjvm.so".to_owned(),
        "C:/Program Files/Java/*/bin/server/jvm.dll".to_owned(),
        "C:/Program Files/Java/jre*/bin/server/jvm.dll".to_owned(),
    ]
}

/// `find_default_jvm`: a JVM library able to host derby.jar, without
/// installing anything new.
pub fn find_default_jvm() -> Option<PathBuf> {
    find_default_jvm_in(
        std::env::var(JVM_ENV).ok(),
        std::env::var("JAVA_HOME").ok(),
        &jvm_globs(),
    )
}

/// `find_default_jvm` with the env values and glob patterns injected.
pub fn find_default_jvm_in(
    jvm_env: Option<String>,
    java_home: Option<String>,
    patterns: &[String],
) -> Option<PathBuf> {
    if let Some(direct) = jvm_env.filter(|d| !d.is_empty()) {
        let path = PathBuf::from(&direct);
        if path.is_file() {
            return Some(path);
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(home) = java_home.filter(|h| !h.is_empty()) {
        let base = expand_user(Path::new(&home));
        for suffix in [
            "lib/server/libjvm.dylib",
            "lib/server/libjvm.so",
            "bin/server/jvm.dll",
            "jre/lib/server/libjvm.dylib",
            "jre/bin/server/jvm.dll",
        ] {
            candidates.push(base.join(suffix));
        }
    }
    for pattern in patterns {
        candidates.extend(glob_paths(pattern));
    }
    candidates.into_iter().find(|candidate| candidate.is_file())
}

// --- bundle resolution -------------------------------------------------------

/// `_is_zip`: a ZIP archive carries the `PK` magic.
fn is_zip(path: &Path) -> bool {
    let Ok(mut handle) = std::fs::File::open(path) else {
        return false;
    };
    use std::io::Read;
    let mut magic = [0u8; 2];
    handle.read(&mut magic).unwrap_or(0) == 2 && magic == *b"PK"
}

/// `looks_like_derby_db`: a Derby database directory always carries
/// `service.properties`.
pub fn looks_like_derby_db(path: &Path) -> bool {
    path.is_dir() && path.join("service.properties").is_file()
}

/// `_db_dir_score`: prefer the conventional names when several Derby
/// directories coexist.
fn db_dir_score(path: &Path) -> (u8, String) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let rank = if name == "db" || name == "sql" {
        0
    } else if name.starts_with("results") {
        1
    } else {
        2
    };
    (rank, path.to_string_lossy().replace('\\', "/"))
}

/// `_find_db_dir`: the Derby database directory inside a bundle, searched to
/// a bounded depth.
pub fn find_db_dir(root: &Path) -> Result<PathBuf, BundleError> {
    if looks_like_derby_db(root) {
        return Ok(root.to_path_buf());
    }
    let mut depth = 0usize;
    let mut matches: Vec<PathBuf> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while !stack.is_empty() && depth < 4 {
        let mut next_level: Vec<PathBuf> = Vec::new();
        for directory in &stack {
            // `with suppress(OSError)` wraps the whole per-directory loop.
            let Ok(entries) = std::fs::read_dir(directory) else {
                continue;
            };
            for entry in entries {
                let Ok(entry) = entry else { break };
                let child = entry.path();
                if !child.is_dir() || child.is_symlink() {
                    continue;
                }
                if looks_like_derby_db(&child) {
                    matches.push(child);
                } else if depth < 3 {
                    next_level.push(child);
                }
            }
        }
        stack = next_level;
        depth += 1;
    }
    if !matches.is_empty() {
        return Ok(matches
            .into_iter()
            .min_by(|a, b| db_dir_score(a).cmp(&db_dir_score(b)))
            .unwrap_or_else(|| root.to_path_buf()));
    }
    let mut entries: Vec<String> = Vec::new();
    // reference lets the iterdir OSError propagate raw.
    let listing = std::fs::read_dir(root).map_err(|e| err(e.to_string()))?;
    for entry in listing {
        let Ok(entry) = entry else { break };
        if entry.path().is_dir() {
            entries.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    entries.sort();
    entries.truncate(10);
    let shown = if entries.is_empty() {
        "none".to_owned()
    } else {
        format!(
            "[{}]",
            entries
                .iter()
                .map(|name| spiderhead_parse::py_str::py_repr(name))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    Err(err(format!(
        "no Derby database found inside {} (looked for a directory with \
         service.properties, depth <= 4). Top-level entries: {shown}",
        root.display()
    )))
}

/// `_materialize`: a writable copy of the Derby database under `temp`; the
/// bundle is not touched. Returns the database directory, how the bundle was
/// delivered, and its size.
pub fn materialize(
    bundle: &Path,
    temp: &Path,
) -> Result<(PathBuf, &'static str, u64), BundleError> {
    if bundle.is_file() {
        if !is_zip(bundle) {
            return Err(err(format!(
                "{} is a file but not a ZIP archive: a saved crawl is a \
                 .seospider directory or archive",
                bundle.display()
            )));
        }
        let extract_root = temp.join("bundle");
        let file = std::fs::File::open(bundle).map_err(|e| err(e.to_string()))?;
        let mut archive = zip::ZipArchive::new(file).map_err(|e| err(e.to_string()))?;
        let mut total: u64 = 0;
        for index in 0..archive.len() {
            total = total.saturating_add(archive.by_index(index).map(|f| f.size()).unwrap_or(0));
        }
        if total > MAX_COPY_BYTES {
            return Err(err(format!(
                "{} is too large to extract safely ({total} bytes; the \
                 temporary location must have room for the whole bundle)",
                bundle.display()
            )));
        }
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).map_err(|e| err(e.to_string()))?;
            let Some(name) = entry.enclosed_name() else {
                continue;
            };
            let target = extract_root.join(name);
            if entry.is_dir() {
                std::fs::create_dir_all(&target).map_err(|e| err(e.to_string()))?;
                continue;
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| err(e.to_string()))?;
            }
            let mut out = std::fs::File::create(&target).map_err(|e| err(e.to_string()))?;
            std::io::copy(&mut entry, &mut out).map_err(|e| err(e.to_string()))?;
        }
        return Ok((find_db_dir(&extract_root)?, "archive", total));
    }
    if !bundle.is_dir() {
        return Err(err(format!("saved crawl not found: {}", bundle.display())));
    }
    let source = find_db_dir(bundle)?;
    let total = dir_size(&source);
    if total > MAX_COPY_BYTES {
        return Err(err(format!(
            "the crawl database inside {} is {total} bytes — larger than the \
             {MAX_COPY_BYTES}-byte copy guard. Free space at the temporary \
             location or inspect a copy of the bundle manually.",
            bundle.display()
        )));
    }
    let destination = temp.join("db");
    copy_tree(&source, &destination)?;
    Ok((destination, "directory", total))
}

/// `sum(item.stat().st_size for item in source.rglob("*") if item.is_file()
/// and not item.is_symlink())` — symlinks are neither counted nor descended.
fn dir_size(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_symlink() {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                total = total.saturating_add(entry.metadata().map(|m| m.len()).unwrap_or(0));
            }
        }
    }
    total
}

/// `shutil.copytree(source, destination, ignore=ignore_patterns("derby.log"))`:
/// recursive copy, symlink contents followed, basename `derby.log` skipped.
fn copy_tree(source: &Path, destination: &Path) -> Result<(), BundleError> {
    std::fs::create_dir_all(destination).map_err(|e| err(e.to_string()))?;
    let entries = std::fs::read_dir(source).map_err(|e| err(e.to_string()))?;
    for entry in entries {
        let entry = entry.map_err(|e| err(e.to_string()))?;
        let name = entry.file_name();
        if name == "derby.log" {
            continue;
        }
        let from = entry.path();
        let to = destination.join(&name);
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| err(e.to_string()))?;
        }
    }
    Ok(())
}

// --- JDBC access -------------------------------------------------------------

/// `mkdtemp(prefix="spiderhead-sf-")` equivalent.
fn make_temp_dir() -> Result<PathBuf, BundleError> {
    tempfile::Builder::new()
        .prefix("spiderhead-sf-")
        .tempdir()
        .map(|d| d.keep())
        .map_err(|e| err(e.to_string()))
}

/// `_connect`: boot the JVM once, then open the copied database read-only.
/// `jvm_finder` is the reference tests' monkeypatch seam for `find_default_jvm`.
fn connect(
    jar: &Path,
    companions: &[PathBuf],
    db_dir: &Path,
    jpype: &dyn JvmBridge,
    jvm_finder: &dyn Fn() -> Option<PathBuf>,
) -> Result<Box<dyn JdbcConnection>, BundleError> {
    let mut classpath = vec![jar.to_string_lossy().into_owned()];
    classpath.extend(companions.iter().map(|p| p.to_string_lossy().into_owned()));
    if !jpype.is_jvm_started() {
        let Some(jvm) = jvm_finder() else {
            return Err(err(JVM_INSTRUCTIONS));
        };
        // ignoreUnrecognized keeps older JVMs happy about the newer access flag.
        jpype.start_jvm(&jvm, &classpath).map_err(|e| {
            err(format!(
                "could not start the JVM at {}: {e}. Set {JVM_ENV} to a JVM \
                 library matching derby.jar's Java level.",
                jvm.display()
            ))
        })?;
    } else if !jpype.class_loadable(DERBY_DRIVER_CLASS) {
        // A JVM that started without derby.jar on its classpath cannot gain it later.
        return Err(err(
            "a JVM is already running in this process without derby.jar on its \
             classpath; Derby cannot be added to a started JVM. Run the \
             inspection in a fresh process.",
        ));
    }

    // Keep Derby's diagnostic log out of the working directory and out of the bundle.
    if let Some(parent) = db_dir.parent() {
        jpype.set_property(
            "derby.stream.error.file",
            &parent.join("derby.log").to_string_lossy(),
        );
    }

    let resolved = db_dir
        .canonicalize()
        .unwrap_or_else(|_| db_dir.to_path_buf());
    let url = format!("jdbc:derby:directory:{}", resolved.display());
    // Ensure the driver class is initialized; some hosted JVMs skip JDBC autoload.
    let _ = jpype.class_loadable(DERBY_DRIVER_CLASS);
    match jpype.driver_manager_connect(&url) {
        Ok(connection) => Ok(connection),
        Err(manager_error) => jpype
            .driver_connect(&url)
            .map_err(|_| err(connection_error(db_dir, &manager_error))),
    }
}

/// `_connection_error`: turn the common Derby boot failures into a sentence
/// that names the fix.
fn connection_error(db_dir: &Path, error: &ConnectError) -> String {
    let state = &error.sql_state;
    let text = &error.message;
    let lowered = text.to_lowercase();
    if lowered.contains("no suitable driver") {
        return "the Derby driver is not registered in this JVM (Derby was likely \
                shut down earlier in this process). Run the inspection in a \
                fresh process."
            .to_owned();
    }
    if state.starts_with("XJ040") || lowered.contains("locked") {
        return format!(
            "the crawl database at {} is locked: it is likely still open in \
             Screaming Frog, or another process holds a Derby boot lock. Close \
             it there (or save the crawl once more) and retry.",
            db_dir.display()
        );
    }
    if lowered.contains("recovery") {
        return format!(
            "the crawl database at {} was not shut down cleanly and needs \
             write recovery, which a read-only inspection cannot do. Open and \
             save the crawl once in Screaming Frog, then retry.",
            db_dir.display()
        );
    }
    if state.starts_with("XBM0") || state.starts_with("XJ004") {
        return format!(
            "{} is not a Derby database directory ({state}): {text}",
            db_dir.display()
        );
    }
    format!(
        "could not open the crawl database at {}: {text}",
        db_dir.display()
    )
}

/// `BundleConnection`: an open, read-only JDBC connection to the copy of a
/// saved crawl's database.
pub struct BundleConnection {
    /// The live JDBC connection through the bridge.
    pub connection: Box<dyn JdbcConnection>,
    /// The copied database directory under `temp`.
    pub db_dir: PathBuf,
    /// Bundle size measured at materialization.
    pub db_bytes: u64,
    /// `"directory"` | `"archive"`.
    pub kind: String,
    temp: PathBuf,
    bridge: Option<Rc<dyn JvmBridge>>,
}

impl BundleConnection {
    /// Direct construction — the `_opened` helper the tests build by hand.
    pub fn new(
        connection: Box<dyn JdbcConnection>,
        db_dir: PathBuf,
        db_bytes: u64,
        temp: PathBuf,
        kind: &str,
    ) -> Self {
        Self {
            connection,
            db_dir,
            db_bytes,
            kind: kind.to_owned(),
            temp,
            bridge: None,
        }
    }

    /// `close()`: release the connection, shut down the copied database so
    /// Derby releases its boot lock, and remove the temp copy. Every step is
    /// suppressed, exactly like the reference — the work happens in `Drop`.
    pub fn close(self) {}

    fn close_inner(&mut self) {
        self.connection.close();
        // `getConnection("...;shutdown=true")` — the thrown XJ015 is Derby's
        // shutdown success signal; never shut the whole driver down.
        if let Some(bridge) = &self.bridge {
            let url = format!(
                "jdbc:derby:directory:{};shutdown=true",
                self.db_dir.display()
            );
            let _ = bridge.driver_manager_connect(&url);
        }
        let _ = std::fs::remove_dir_all(&self.temp);
    }
}

impl Drop for BundleConnection {
    fn drop(&mut self) {
        self.close_inner();
    }
}

/// `open_bundle`: copy a saved crawl's Derby database to a temp dir and
/// connect read-only. `bridge=None` is the reference missing `jpype` import.
pub fn open_bundle(
    bundle: &Path,
    derby_jar: Option<&Path>,
    bridge: Option<Rc<dyn JvmBridge>>,
) -> Result<BundleConnection, BundleError> {
    open_bundle_in(
        bundle,
        derby_jar,
        bridge,
        &find_derby_jar,
        &find_default_jvm,
    )
}

/// `open_bundle` with the jar and JVM lookups injected — the seams the reference
/// tests get by monkeypatching `_search_globs`, `find_derby_jar` and
/// `find_default_jvm`.
pub fn open_bundle_in(
    bundle: &Path,
    derby_jar: Option<&Path>,
    bridge: Option<Rc<dyn JvmBridge>>,
    jar_finder: &dyn Fn(&[PathBuf]) -> Option<PathBuf>,
    jvm_finder: &dyn Fn() -> Option<PathBuf>,
) -> Result<BundleConnection, BundleError> {
    let explicit: Vec<PathBuf> = derby_jar.map(|p| vec![p.to_path_buf()]).unwrap_or_default();
    let Some(jar) = jar_finder(&explicit) else {
        return Err(err(JAR_INSTRUCTIONS));
    };
    // `_load_jpype()`: the optional JVM bridge must exist before anything else runs.
    let Some(jpype) = bridge else {
        return Err(err(JPYPE_INSTRUCTIONS));
    };
    let source = expand_user(bundle);
    let temp = make_temp_dir()?;
    // Any failure after mkdtemp removes the temp copy before propagating.
    let assembled = materialize(&source, &temp).and_then(|(db_dir, kind, total)| {
        connect(
            &jar,
            &companion_jars(&jar),
            &db_dir,
            jpype.as_ref(),
            jvm_finder,
        )
        .map(|connection| (db_dir, kind, total, connection))
    });
    let (db_dir, kind, total, mut connection) = match assembled {
        Ok(parts) => parts,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&temp);
            return Err(e);
        }
    };
    connection.set_read_only(true);
    Ok(BundleConnection {
        connection,
        db_dir,
        db_bytes: total,
        kind: kind.to_owned(),
        temp,
        bridge: Some(jpype),
    })
}

// --- inventory ----------------------------------------------------------------

/// `_scalar`: first cell of the first row, or `None` when the query finds
/// nothing.
fn scalar(conn: &dyn JdbcConnection, sql: &str) -> Result<Option<i64>, ConnectError> {
    let mut statement = conn.create_statement();
    let result = (|statement: &mut dyn JdbcStatement| -> Result<Option<i64>, ConnectError> {
        statement.set_query_timeout(QUERY_TIMEOUT_SECONDS);
        let mut result = statement.execute_query(sql)?;
        if !result.next()? {
            return Ok(None);
        }
        Ok(Some(result.get_long(1)?))
    })(statement.as_mut());
    statement.close();
    result
}

/// `_rows`: every row as `str(getString(i))` per column — a NULL cell becomes
/// the literal `"None"`, exactly as reference `str(None)` does.
fn rows(
    conn: &dyn JdbcConnection,
    sql: &str,
    columns: usize,
) -> Result<Vec<Vec<String>>, ConnectError> {
    let mut statement = conn.create_statement();
    let result = (|statement: &mut dyn JdbcStatement| -> Result<Vec<Vec<String>>, ConnectError> {
        statement.set_query_timeout(QUERY_TIMEOUT_SECONDS);
        let mut result = statement.execute_query(sql)?;
        let mut collected: Vec<Vec<String>> = Vec::new();
        while result.next()? {
            let mut row = Vec::with_capacity(columns);
            for i in 0..columns {
                row.push(
                    result
                        .get_string(i + 1)?
                        .unwrap_or_else(|| "None".to_owned()),
                );
            }
            collected.push(row);
        }
        Ok(collected)
    })(statement.as_mut());
    statement.close();
    result
}

/// `_user_tables`: every user table as `(schema, name)` exactly as the
/// catalog spells it.
fn user_tables(conn: &dyn JdbcConnection) -> Result<Vec<(String, String)>, ConnectError> {
    Ok(rows(conn, TABLES_SQL, 2)?
        .into_iter()
        .map(|row| (row[0].clone(), row[1].clone()))
        .collect())
}

/// `_count_rows`: `COUNT(*)` per table, at most [`ROW_COUNT_LIMIT`] of them.
fn count_rows(conn: &dyn JdbcConnection, tables: &[(String, String)]) -> (Vec<Value>, Vec<String>) {
    // reference keys `counted` by the (schema, name) tuple.
    let mut counted: std::collections::HashMap<(String, String), i64> =
        std::collections::HashMap::new();
    let mut notes: Vec<String> = Vec::new();
    for (schema, name) in tables.iter().take(ROW_COUNT_LIMIT) {
        match scalar(
            conn,
            &format!("SELECT COUNT(*) FROM \"{schema}\".\"{name}\""),
        ) {
            Ok(Some(total)) => {
                counted.insert((schema.clone(), name.clone()), total);
            }
            Ok(None) => {}
            Err(e) => notes.push(format!("{schema}.{name}: row count failed ({})", e.message)),
        }
    }
    if tables.len() > ROW_COUNT_LIMIT {
        notes.push(format!(
            "row counts capped at {ROW_COUNT_LIMIT} tables; {} more listed without counts",
            tables.len() - ROW_COUNT_LIMIT
        ));
    }
    let rows = tables
        .iter()
        .map(|(schema, name)| {
            let key = (schema.clone(), name.clone());
            json!({
                "name": format!("{schema}.{name}"),
                "rows": counted.get(&key).copied().map_or(Value::Null, |t| json!(t)),
            })
        })
        .collect();
    (rows, notes)
}

/// `_probe_software_version`: the software-version row, when a table that
/// looks like one exists. Any user table whose name contains VERSION is
/// probed for a single row and reported as-is.
fn probe_software_version(conn: &dyn JdbcConnection, tables: &[(String, String)]) -> Option<Value> {
    for (schema, name) in tables {
        if !name.to_uppercase().contains("VERSION") {
            continue;
        }
        let mut statement = conn.create_statement();
        let probed = (|statement: &mut dyn JdbcStatement| -> Result<Option<Value>, ConnectError> {
            statement.set_query_timeout(QUERY_TIMEOUT_SECONDS);
            let mut result = statement.execute_query(&format!(
                "SELECT * FROM \"{schema}\".\"{name}\" FETCH FIRST 1 ROWS ONLY"
            ))?;
            if !result.next()? {
                return Ok(None);
            }
            let labels = result.column_labels();
            let mut row = Map::new();
            for (i, label) in labels.iter().enumerate() {
                row.insert(
                    label.clone(),
                    result
                        .get_string(i + 1)?
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                );
            }
            Ok(Some(
                json!({"table": format!("{schema}.{name}"), "row": row}),
            ))
        })(statement.as_mut());
        statement.close();
        match probed {
            Ok(Some(found)) => return Some(found),
            _ => continue,
        }
    }
    None
}

/// `_probe_start_url`: the crawl's start URL, best-effort from the frontier
/// or URL dictionary. Anything missing or renamed simply yields `None`.
fn probe_start_url(conn: &dyn JdbcConnection) -> Option<String> {
    let Ok(column_rows) = rows(conn, URL_COLUMNS_SQL, 2) else {
        return None;
    };
    let mut column_map: std::collections::BTreeMap<String, std::collections::HashSet<String>> =
        std::collections::BTreeMap::new();
    for row in &column_rows {
        column_map
            .entry(row[0].clone())
            .or_default()
            .insert(row[1].clone());
    }
    let values = |sql: &str| -> Vec<String> {
        // `[row[0] for row in _rows(...) if row[0]]` — only empty strings drop out.
        rows(conn, sql, 1)
            .map(|found| {
                found
                    .into_iter()
                    .map(|row| row[0].clone())
                    .filter(|cell| !cell.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    };

    let mut root_tables: Vec<&String> = column_map
        .iter()
        .filter(|(_, cols)| cols.contains("ROOT_URL"))
        .map(|(table, _)| table)
        .collect();
    root_tables.sort();
    for table in root_tables {
        let found = values(&format!(
            "SELECT DISTINCT ROOT_URL FROM \"{table}\" \
             WHERE ROOT_URL IS NOT NULL FETCH FIRST 3 ROWS ONLY"
        ));
        if !found.is_empty() {
            return Some(found[0].clone());
        }
    }

    let mut waiting: Vec<&String> = column_map
        .iter()
        .filter(|(table, cols)| {
            cols.contains("ENCODED_URL") && table.to_uppercase().contains("WAITING")
        })
        .map(|(table, _)| table)
        .collect();
    waiting.sort();
    for table in waiting {
        let found = values(&format!(
            "SELECT DISTINCT ENCODED_URL FROM \"{table}\" \
             WHERE ENCODED_URL IS NOT NULL FETCH FIRST 3 ROWS ONLY"
        ));
        if !found.is_empty() {
            return Some(found[0].clone());
        }
    }

    let mut with_depth: Vec<&String> = column_map
        .iter()
        .filter(|(_, cols)| cols.contains("ENCODED_URL") && cols.contains("CRAWL_DEPTH"))
        .map(|(table, _)| table)
        .collect();
    // `key=lambda name: (name.upper() != "URLS", name)` — URLS first, then sorted.
    with_depth.sort_by(|a, b| {
        (a.to_uppercase() != "URLS", (*a).clone()).cmp(&(b.to_uppercase() != "URLS", (*b).clone()))
    });
    for table in with_depth {
        let found = values(&format!(
            "SELECT DISTINCT ENCODED_URL FROM \"{table}\" \
             WHERE CRAWL_DEPTH = 0 AND ENCODED_URL IS NOT NULL FETCH FIRST 3 ROWS ONLY"
        ));
        if !found.is_empty() {
            return Some(found[0].clone());
        }
    }
    None
}

/// `_inventory`: build the JSON inventory from an open [`BundleConnection`].
pub fn inventory(opened: &BundleConnection, bundle_label: &str) -> Result<Value, ConnectError> {
    let conn = opened.connection.as_ref();
    let tables = user_tables(conn)?;
    let (rows, notes) = count_rows(conn, &tables);
    let rows_counted = rows.iter().filter(|item| !item["rows"].is_null()).count();
    Ok(json!({
        "bundle": bundle_label,
        "bundle_kind": opened.kind,
        "derby_version": conn.database_product_version(),
        "start_url": probe_start_url(conn).map_or(Value::Null, Value::String),
        "software_version": probe_software_version(conn, &tables).unwrap_or(Value::Null),
        "tables": rows,
        "tables_total": tables.len(),
        "rows_counted": rows_counted,
        "db_bytes": opened.db_bytes,
        "notes": notes,
    }))
}

/// `inspect_bundle`: open a saved crawl and return its inventory — tables,
/// counts, versions, start URL.
pub fn inspect_bundle(
    bundle: &Path,
    derby_jar: Option<&Path>,
    bridge: Option<Rc<dyn JvmBridge>>,
) -> Result<Value, BundleError> {
    let opened = open_bundle(bundle, derby_jar, bridge)?;
    let label = bundle.to_string_lossy().into_owned();
    let report = inventory(&opened, &label).map_err(|e| err(e.message));
    opened.close();
    report
}
