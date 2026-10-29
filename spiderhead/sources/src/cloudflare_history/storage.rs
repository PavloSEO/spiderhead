//! SQLite storage helpers for Cloudflare history: path checks, connections and table listing.
//! Paths are inspected with lstat, so symlinks are seen rather than silently followed.
use super::*;

pub(super) fn invalid<T>(message: impl Into<String>) -> Result<T, CfhError> {
    Err(CfhError::Invalid(message.into()))
}

/// Stat without following links; errors keep the `[Errno N] ...: '<path>'` shape.
pub(super) fn lstat_py(path: &Path) -> std::io::Result<std::fs::Metadata> {
    path.symlink_metadata().map_err(|err| {
        let (errno, text) = match err.kind() {
            std::io::ErrorKind::NotFound => (2, "No such file or directory".to_owned()),
            std::io::ErrorKind::PermissionDenied => (13, "Permission denied".to_owned()),
            _ => (err.raw_os_error().unwrap_or(5), err.to_string()),
        };
        std::io::Error::new(
            err.kind(),
            format!("[Errno {errno}] {text}: '{}'", path.display()),
        )
    })
}

/// The history file inside a verified project directory.
pub(super) fn path_arg(project: &Value) -> Result<PathBuf, CfhError> {
    let root = PathBuf::from(project.as_str().unwrap_or_default());
    if root.is_symlink() || !root.is_dir() {
        return invalid("project must be a regular directory");
    }
    let manifest = root.join("project.json");
    let Ok(info) = manifest.symlink_metadata() else {
        return invalid("project requires a regular, unaliased project.json");
    };
    if info.file_type().is_symlink()
        || !info.is_file()
        || spiderhead_core::filesystem::link_identity(&manifest)
            .map(|i| i.2 != 1)
            .unwrap_or(true)
    {
        return invalid("project requires a regular, unaliased project.json");
    }
    Ok(root.join(DB_NAME))
}

/// True for any directory entry, including a dangling link.
pub(super) fn lexists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

pub(super) fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    }
}

pub(super) fn table_names(con: &Connection) -> Result<Vec<String>, CfhError> {
    let names = con
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(names)
}

/// `_connect` — open/verify the history database, creating the schema on a fresh file.
pub(super) fn connect(path: &Path, create: bool) -> Result<Connection, CfhError> {
    if create && !lexists(path) {
        use spiderhead_core::filesystem::PrivateMode;
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .private_mode(0o600)
            .open(path);
    }
    let info = lstat_py(path)?;
    use spiderhead_core::filesystem::{link_identity, StatCompat};
    let unaliased = link_identity(path).map(|i| i.2 == 1).unwrap_or(false);
    if !info.is_file() || !unaliased || !is_private_mode(info.mode()) {
        return invalid("Cloudflare history must be a private regular unaliased file");
    }
    let con = if create {
        spiderhead_core::sqlite::open_writer(path, spiderhead_core::sqlite::DEFAULT_TIMEOUT, &[])?
    } else {
        spiderhead_core::sqlite::open_readonly(
            &absolute(path),
            spiderhead_core::sqlite::DEFAULT_TIMEOUT,
            &[],
        )?
    };
    con.execute_batch("PRAGMA trusted_schema=OFF")?;
    con.execute_batch(if create { "BEGIN IMMEDIATE" } else { "BEGIN" })?;
    let check = (|| -> Result<(), CfhError> {
        let tables: std::collections::BTreeSet<String> = table_names(&con)?.into_iter().collect();
        if tables.is_empty() && create && info.len() == 0 {
            con.execute_batch(
                "CREATE TABLE cloudflare_meta(format TEXT PRIMARY KEY);
                 CREATE TABLE cloudflare_snapshots (id INTEGER PRIMARY KEY, zone_id TEXT NOT NULL,
                    zone TEXT NOT NULL, observed_at TEXT NOT NULL, fingerprint TEXT NOT NULL,
                    period_from TEXT NOT NULL, period_to TEXT NOT NULL, state TEXT NOT NULL,
                    sampled INTEGER NOT NULL, truncated_days TEXT NOT NULL, payload TEXT NOT NULL,
                    UNIQUE(zone_id, observed_at, fingerprint));
                 CREATE INDEX cloudflare_snapshots_zone_period
                    ON cloudflare_snapshots(zone_id, period_from, period_to, id);",
            )?;
            con.execute("INSERT INTO cloudflare_meta VALUES (?)", [FORMAT])?;
        } else if tables
            != ["cloudflare_meta", "cloudflare_snapshots"]
                .iter()
                .map(|s| s.to_string())
                .collect::<std::collections::BTreeSet<_>>()
        {
            return invalid("existing database is not a Cloudflare history database");
        } else {
            let formats: Vec<String> = con
                .prepare("SELECT format FROM cloudflare_meta")?
                .query_map([], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            if formats != [FORMAT.to_string()] {
                return invalid("unsupported Cloudflare history format");
            }
        }
        if !create {
            con.execute_batch("PRAGMA query_only=ON")?;
        }
        Ok(())
    })();
    if check.is_err() {
        let _ = con.execute_batch("ROLLBACK");
    }
    check?;
    Ok(con)
}

/// Canonical JSON: sorted keys, no extra spaces, non-ASCII kept as-is.
pub(super) fn compact_dumps(value: &Value) -> String {
    crate::pyjson::dumps_opts_compact(value, true, false)
}

pub(super) fn member<'a>(value: &'a Value, key: &str) -> Result<&'a Value, CfhError> {
    match value.get(key) {
        Some(v) => Ok(v),
        None => Err(CfhError::KeyErr(format!("'{key}'"))),
    }
}
