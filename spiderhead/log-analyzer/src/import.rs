//! Importing server logs into a fresh retained artifact under a project's `logs` folder.

use std::path::{Path, PathBuf};

use spiderhead_core::filesystem::{PrivateMode, StatCompat};

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::bots::detect_bot;
use crate::clock::{iso_date, iso_utc, iso_utc_micros};
use crate::error::{os_message, LogsError, Result};
use crate::format::{detect_format, ident_tokens, LineParser};
use crate::reader::{open_log, Lines};
use crate::store::{
    cleanup_stale_imports, collect_files, has_requests, import_metadata, latest_source,
    open_log_source, positive, url_section, APPLICATION_ID, MAX_LINE_CHARS, REQUESTS_SCHEMA,
    RESERVE_BYTES, SCHEMA,
};

/// An unfinished artifact older than this is removed when the next import starts.
pub const STALE_IMPORT_SECONDS: f64 = 3600.0;
use crate::text::strip;

/// A project as the importer needs to see it.
#[derive(Debug, Clone)]
pub struct ProjectManifest {
    /// Project directory; imports land in `<root>/logs`.
    pub root: PathBuf,
    /// Stable project identifier recorded in the artifact.
    pub project_uuid: String,
    /// Origin the project audits, e.g. `https://example.test`.
    pub target: String,
}

/// Resolves a project reference to its manifest. The projects layer implements this; the
/// log store only asks.
pub trait ProjectManifestLoader {
    /// Load the manifest of `project`, or describe why it is not a project.
    fn load(&self, project: &str) -> std::result::Result<ProjectManifest, String>;
}

/// Everything an import needs from its environment.
pub struct ImportContext<'a> {
    /// Project resolution.
    pub loader: &'a dyn ProjectManifestLoader,
    /// Free bytes on the filesystem holding a path.
    pub free_space: &'a dyn Fn(&Path) -> std::io::Result<u64>,
    /// Wall-clock Unix seconds, stamped into the artifact header.
    pub now: &'a dyn Fn() -> f64,
}

/// Bounds of one import.
#[derive(Debug, Clone, Copy)]
pub struct ImportOptions {
    /// Total non-skipped lines read across all files (1..=100_000_000).
    pub max_lines: i64,
    /// Directory entries inspected (1..=10_000).
    pub max_files: i64,
    /// Lines used to detect each file's format (1..=1000).
    pub sample_size: i64,
    /// Keep one row per request in `requests`, with aggregates built on top.
    pub keep_requests: bool,
    /// Add to the project's newest artifact instead of creating a new one. Rows already
    /// stored (same line, same occurrence) are skipped, so importing a file twice in
    /// separate calls changes nothing. Without a retained artifact a new one is created.
    pub append: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            max_lines: 10_000_000,
            max_files: 1000,
            sample_size: 50,
            keep_requests: true,
            append: false,
        }
    }
}

const VISIT_INSERT: &str = "INSERT INTO visits VALUES(?,?,?,?,?,?,?,1,?,?)
 ON CONFLICT(bot,ip,url,date,status) DO UPDATE SET
 visits=visits+1,bytes=bytes+excluded.bytes,
 last_visit=CASE WHEN last_visit IS NULL THEN excluded.last_visit
 WHEN excluded.last_visit>last_visit THEN excluded.last_visit ELSE last_visit END";
const IP_INSERT: &str = "INSERT INTO bot_ips VALUES(?,?,?,1,?)
 ON CONFLICT(bot,ip) DO UPDATE SET visits=visits+1,
 last_visit=CASE WHEN last_visit IS NULL THEN excluded.last_visit
 WHEN excluded.last_visit>last_visit THEN excluded.last_visit ELSE last_visit END";
const BATCH: usize = 10_000;

type Visit = (
    String,
    String,
    String,
    String,
    String,
    i64,
    String,
    i64,
    Option<String>,
);
type IpHit = (String, String, String, Option<String>);

struct Request {
    hash: String,
    ts: Option<String>,
    date: String,
    ip: String,
    bot: String,
    family: String,
    method: String,
    url: String,
    status: i64,
    bytes: i64,
    referer: Option<String>,
    user_agent: Option<String>,
    source: String,
}

fn row_hash(line: &str, ordinal: u32) -> String {
    let mut digest = Sha256::new();
    digest.update(line.as_bytes());
    digest.update([0u8]);
    digest.update(ordinal.to_string().as_bytes());
    digest.finalize()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn flush(
    con: &mut Connection,
    visits: &mut Vec<Visit>,
    ips: &mut Vec<IpHit>,
    requests: &mut Vec<Request>,
) -> Result<()> {
    // A savepoint nests inside the outer transaction used by append mode.
    let tx = con.savepoint()?;
    if !requests.is_empty() {
        let mut insert =
            tx.prepare_cached("INSERT OR IGNORE INTO requests VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)")?;
        for r in requests.iter() {
            insert.execute(params![
                r.hash,
                r.ts,
                r.date,
                r.ip,
                r.bot,
                r.family,
                r.method,
                r.url,
                r.status,
                r.bytes,
                r.referer,
                r.user_agent,
                r.source
            ])?;
        }
    }
    {
        let mut insert = tx.prepare_cached(VISIT_INSERT)?;
        for v in visits.iter() {
            insert.execute(params![v.0, v.1, v.2, v.3, v.4, v.5, v.6, v.7, v.8])?;
        }
        let mut insert = tx.prepare_cached(IP_INSERT)?;
        for h in ips.iter() {
            insert.execute(params![h.0, h.1, h.2, h.3])?;
        }
    }
    tx.commit()?;
    visits.clear();
    ips.clear();
    requests.clear();
    Ok(())
}

fn error_document(error: &LogsError) -> Value {
    json!({"ok": false, "error": error.to_string()})
}

/// Import `path` (a log file or a directory of them) into a new artifact of `project`.
///
/// Source digests cover the decoded text consumed, not compressed bytes. Apache timestamps
/// without an offset and IIS timestamps are read as UTC. A failed import leaves no partial
/// artifact behind; the outcome is a `{"ok": ...}` document either way.
pub fn import_logs(
    ctx: &ImportContext<'_>,
    path: &str,
    project: &str,
    options: ImportOptions,
) -> Value {
    let mut artifact: Option<PathBuf> = None;
    match import_inner(ctx, path, project, options, &mut artifact) {
        Ok(result) => result,
        Err(error) => {
            if let Some(file) = artifact {
                let _ = std::fs::remove_file(&file);
                let _ = std::fs::remove_file(format!("{}-journal", file.display()));
            }
            error_document(&error)
        }
    }
}

fn import_inner(
    ctx: &ImportContext<'_>,
    path: &str,
    project: &str,
    o: ImportOptions,
    artifact: &mut Option<PathBuf>,
) -> Result<Value> {
    positive(o.max_lines, "max_lines", 100_000_000)?;
    positive(o.max_files, "max_files", 10_000)?;
    positive(o.sample_size, "sample_size", 1000)?;
    let manifest = ctx.loader.load(project).map_err(LogsError::Value)?;
    let files = collect_files(&std::path::absolute(path)?, o.max_files as usize)?;
    if (ctx.free_space)(&manifest.root)? < RESERVE_BYTES {
        return Err(LogsError::value(
            "log import requires at least 10 GiB of free disk reserve",
        ));
    }
    let directory = manifest.root.join("logs");
    let unsafe_dir = std::fs::symlink_metadata(&directory)
        .is_ok_and(|m| m.file_type().is_symlink() || !m.is_dir());
    if unsafe_dir {
        return Err(LogsError::value("project logs directory is unsafe"));
    }
    if !directory.exists() {
        std::fs::DirBuilder::new()
            .private_mode(0o700)
            .create(&directory)?;
    }
    let cleaned = cleanup_stale_imports(&manifest.root, (ctx.now)(), STALE_IMPORT_SECONDS)?;
    let target = if o.append {
        latest_source(&manifest.root).ok()
    } else {
        None
    };
    let (file, import_id, previous) = match target {
        Some(path) => {
            let previous = import_metadata(&open_log_source(&path)?)?;
            let file = PathBuf::from(&path);
            let id = file
                .file_stem()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (file, id, Some(previous))
        }
        None => {
            let import_id = uuid::Uuid::new_v4().to_string();
            let file = directory.join(format!("{import_id}.sqlite"));
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .private_mode(0o600)
                .open(&file)?;
            *artifact = Some(file.clone());
            (file, import_id, None)
        }
    };
    let mut con = Connection::open(&file)?;
    if previous.is_none() {
        con.execute_batch(&format!("PRAGMA application_id={APPLICATION_ID}"))?;
        con.execute_batch("PRAGMA cache_size=-8192")?;
        con.execute_batch("PRAGMA temp_store=FILE")?;
        con.execute_batch(SCHEMA)?;
    } else {
        // Rolled back on any error: a failed append leaves the artifact as it was.
        con.execute_batch("BEGIN IMMEDIATE")?;
    }
    let keep = o.keep_requests || has_requests(&con);
    if keep {
        con.execute_batch(REQUESTS_SCHEMA)?;
    }
    let appending = previous.is_some();
    let mut s = Stats::default();
    let mut variants: indexmap::IndexMap<String, i64> = indexmap::IndexMap::new();
    let mut imported = 0usize;
    for source in &files {
        if s.total >= o.max_lines as u64 {
            s.truncated = true;
            break;
        }
        let env = FileEnv {
            ctx,
            directory: &directory,
            keep,
            appending,
        };
        import_file(&env, &mut con, source, o, &mut s, &mut variants)?;
        imported += 1;
    }
    let period = json!({"from": s.first, "to": s.last});
    let mut result = json!({
        "ok": true,
        "source": file.to_string_lossy(),
        "import_id": import_id,
        "format": "logs.v1",
        "project_uuid": manifest.project_uuid,
        "project_target": manifest.target,
        "lines": {
            "total": s.total, "parsed": s.parsed, "skipped": s.skipped, "truncated": s.truncated,
            "unknown_time": s.unknown_time, "unknown_user_agent": s.unknown_agent,
        },
        "period": period,
        "complete": !s.truncated && s.skipped == 0 && (s.parsed > 0 || appending && s.duplicates > 0),
        "bot_detection_complete": !s.truncated && s.skipped == 0 && s.unknown_agent == 0 && (s.parsed > 0 || appending && s.duplicates > 0),
        "files": {"imported": imported, "selected": files.len()},
        "log_format_variants": variants,
        "timezone": "UTC; missing Apache offsets and IIS timestamps assume UTC",
        "bot_identity": "user-agent claim; DNS verification is separate",
        "created_at": iso_utc_micros((ctx.now)()),
    });
    if s.duplicates > 0 {
        result["lines"]["duplicate"] = json!(s.duplicates);
    }
    if keep {
        result["requests"] = json!(true);
    }
    if !cleaned.is_empty() {
        result["cleaned_stale_imports"] = json!(cleaned.len());
    }
    if let Some(prev) = previous {
        let added = result["lines"].clone();
        let mut merged = merge_metadata(&prev, &result);
        merged["appended"] = json!({"parsed": added["parsed"], "duplicate": added.get("duplicate").cloned().unwrap_or(json!(0))});
        merged["updated_at"] = result["created_at"].clone();
        con.execute(
            "UPDATE metadata SET value=? WHERE key='import'",
            params![serde_json::to_string(&merged).map_err(|e| LogsError::value(e.to_string()))?],
        )?;
        con.execute_batch("COMMIT")?;
        return Ok(merged);
    }
    con.execute(
        "INSERT INTO metadata VALUES('import',?)",
        params![serde_json::to_string(&result).map_err(|e| LogsError::value(e.to_string()))?],
    )?;
    Ok(result)
}

/// Combine the stored import header with the header of an appended run.
fn merge_metadata(prev: &Value, run: &Value) -> Value {
    let mut out = prev.clone();
    let n = |v: &Value, k: &str| v["lines"].get(k).and_then(Value::as_u64).unwrap_or(0);
    for key in [
        "total",
        "parsed",
        "skipped",
        "unknown_time",
        "unknown_user_agent",
        "duplicate",
    ] {
        let sum = n(prev, key) + n(run, key);
        if sum > 0 || prev["lines"].get(key).is_some() {
            out["lines"][key] = json!(sum);
        }
    }
    out["lines"]["truncated"] = json!(
        prev["lines"]["truncated"].as_bool().unwrap_or(false)
            || run["lines"]["truncated"].as_bool().unwrap_or(false)
    );
    let pick = |a: &Value, b: &Value, min: bool| match (a.as_str(), b.as_str()) {
        (Some(x), Some(y)) => json!(if (x < y) == min { x } else { y }),
        (Some(x), None) | (None, Some(x)) => json!(x),
        _ => Value::Null,
    };
    out["period"] = json!({
        "from": pick(&prev["period"]["from"], &run["period"]["from"], true),
        "to": pick(&prev["period"]["to"], &run["period"]["to"], false),
    });
    for key in ["complete", "bot_detection_complete"] {
        out[key] =
            json!(prev[key].as_bool().unwrap_or(false) && run[key].as_bool().unwrap_or(false));
    }
    for key in ["imported", "selected"] {
        let sum =
            prev["files"][key].as_u64().unwrap_or(0) + run["files"][key].as_u64().unwrap_or(0);
        out["files"][key] = json!(sum);
    }
    if let (Some(a), Some(b)) = (
        out["log_format_variants"].as_object().cloned(),
        run["log_format_variants"].as_object(),
    ) {
        let mut merged = a;
        for (k, v) in b {
            let sum = merged.get(k).and_then(Value::as_i64).unwrap_or(0) + v.as_i64().unwrap_or(0);
            merged.insert(k.clone(), json!(sum));
        }
        out["log_format_variants"] = Value::Object(merged);
    }
    out["requests"] = json!(true);
    out
}

fn fingerprint(line: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    line.hash(&mut hasher);
    hasher.finish()
}

#[derive(Default)]
struct Stats {
    /// Lines already supplied by an earlier file of this import.
    duplicates: u64,
    /// Fingerprint multiset of earlier files: a repeated request inside one file is real
    /// traffic, a line an earlier file already supplied is overlap (copied or re-exported file).
    earlier: std::collections::HashMap<u64, u32>,
    total: u64,
    parsed: u64,
    skipped: u64,
    unknown_time: u64,
    unknown_agent: u64,
    truncated: bool,
    first: Option<String>,
    last: Option<String>,
}

fn identity(path: &Path) -> std::io::Result<(u64, u64, u64, i64, i64)> {
    let meta = std::fs::symlink_metadata(path)?;
    let id = spiderhead_core::filesystem::link_identity(path)?;
    Ok((id.0, id.1, meta.size(), meta.mtime(), meta.mtime_nsec()))
}

/// Per-run constants of [`import_file`].
struct FileEnv<'a> {
    ctx: &'a ImportContext<'a>,
    directory: &'a Path,
    /// Store one row per request.
    keep: bool,
    /// Writing into an existing artifact: skip rows already stored.
    appending: bool,
}

fn import_file(
    env: &FileEnv<'_>,
    con: &mut Connection,
    source: &Path,
    o: ImportOptions,
    s: &mut Stats,
    variants: &mut indexmap::IndexMap<String, i64>,
) -> Result<()> {
    let FileEnv {
        ctx,
        directory,
        keep,
        appending,
    } = *env;
    let mut current: std::collections::HashMap<u64, u32> = std::collections::HashMap::new();
    let before = identity(source)?;
    let mut digest = Sha256::new();
    let mut count = 0i64;
    let mut complete = true;
    let handle = open_log(source).map_err(|e| LogsError::Io(os_message(&e, None)))?;
    let mut stream = Lines::capped(handle, MAX_LINE_CHARS);
    let room = (o.sample_size as u64).min(o.max_lines as u64 - s.total);
    let mut sample: Vec<String> = Vec::new();
    for _ in 0..room {
        match stream.next() {
            Some(line) => sample.push(line?),
            None => break,
        }
    }
    let Some(format) = detect_format(sample.iter().map(String::as_str)) else {
        let name = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        return Err(LogsError::value(format!(
            "Unrecognized log format in {name}"
        )));
    };
    for line in &sample {
        if let Some(tokens) = ident_tokens(line) {
            *variants
                .entry(format!("{}-{}-ident", format.name(), 1 + tokens))
                .or_insert(0) += 1;
        }
    }
    let mut parser = LineParser::for_format(format);
    let mut visits: Vec<Visit> = Vec::new();
    let mut ips: Vec<IpHit> = Vec::new();
    let mut requests: Vec<Request> = Vec::new();
    let source_name = source.to_string_lossy().into_owned();
    for line in sample.into_iter().map(Ok).chain(stream) {
        let line = line?;
        if s.total >= o.max_lines as u64 {
            complete = false;
            s.truncated = true;
            break;
        }
        digest.update(line.as_bytes());
        count += 1;
        s.total += 1;
        if strip(&line).is_empty() || line.starts_with('#') {
            // IIS directives are consumed by the stateful parser.
            parser.parse(&line)?;
            continue;
        }
        let print = fingerprint(&line);
        let used = current.entry(print).or_insert(0);
        *used += 1;
        if s.earlier.get(&print).is_some_and(|n| n >= used) {
            s.duplicates += 1;
            continue;
        }
        let hash = keep.then(|| row_hash(&line, *used));
        if appending {
            if let Some(h) = &hash {
                let seen = con
                    .prepare_cached("SELECT 1 FROM requests WHERE row_hash=?")?
                    .exists(params![h])?;
                if seen {
                    s.duplicates += 1;
                    continue;
                }
            }
        }
        let row = parser.parse(line.trim_end_matches('\n'))?;
        let Some(row) = row.filter(|r| (100..=599).contains(&r.status)) else {
            s.skipped += 1;
            continue;
        };
        s.parsed += 1;
        let bot = detect_bot(row.user_agent.as_deref());
        let (name, family) = match &bot {
            Some(b) => (b.name, b.family),
            None if row.user_agent.is_some() => ("human", "human"),
            None => ("unknown", "unknown"),
        };
        if row.user_agent.is_none() {
            s.unknown_agent += 1;
        }
        let stamp = row.time.map(iso_utc);
        let day = row.time.map(iso_date).unwrap_or_default();
        match &stamp {
            Some(st) => {
                if s.first.as_ref().is_none_or(|f| st < f) {
                    s.first = Some(st.clone());
                }
                if s.last.as_ref().is_none_or(|l| st > l) {
                    s.last = Some(st.clone());
                }
            }
            None => s.unknown_time += 1,
        }
        if let Some(hash) = hash {
            requests.push(Request {
                hash,
                ts: stamp.clone(),
                date: day.clone(),
                ip: row.ip.clone(),
                bot: name.into(),
                family: family.into(),
                method: row.method.clone(),
                url: row.path.clone(),
                status: row.status,
                bytes: row.bytes.max(0),
                referer: row.referer.clone(),
                user_agent: row.user_agent.clone(),
                source: source_name.clone(),
            });
        }
        visits.push((
            name.into(),
            family.into(),
            row.ip.clone(),
            row.path.clone(),
            day,
            row.status,
            url_section(&row.path)?,
            row.bytes.max(0),
            stamp.clone(),
        ));
        if bot.is_some() {
            ips.push((name.into(), family.into(), row.ip, stamp));
        }
        if visits.len() >= BATCH {
            flush(con, &mut visits, &mut ips, &mut requests)?;
            if (ctx.free_space)(directory)? < RESERVE_BYTES {
                return Err(LogsError::value(
                    "log import stopped to preserve 10 GiB disk reserve",
                ));
            }
        }
    }
    flush(con, &mut visits, &mut ips, &mut requests)?;
    for (print, n) in current {
        let slot = s.earlier.entry(print).or_insert(0);
        *slot = (*slot).max(n);
    }
    if before != identity(source)? {
        return Err(LogsError::value("log source changed during import"));
    }
    let hex: String = digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    con.execute(
        "INSERT OR REPLACE INTO sources VALUES(?,?,?,?,?,?,?,?)",
        params![
            source.to_string_lossy(),
            before.2 as i64,
            before.3 * 1_000_000_000 + before.4,
            hex,
            if complete {
                "decoded_text"
            } else {
                "decoded_text_prefix"
            },
            format.name(),
            count,
            i64::from(complete),
        ],
    )?;
    Ok(())
}
