//! Split out of `gsc_inspection_history`.

use super::*;

pub(super) fn invalid<T>(message: impl Into<String>) -> Result<T, HistoryError> {
    Err(HistoryError::Invalid(message.into()))
}

#[cfg(unix)]
pub(super) fn unix_mode(meta: &fs::Metadata) -> u32 {
    std::os::unix::fs::MetadataExt::mode(meta)
}

#[cfg(not(unix))]
pub(super) fn unix_mode(_meta: &fs::Metadata) -> u32 {
    0o600
}

#[cfg(unix)]
pub(super) fn unix_nlink(meta: &fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::nlink(meta)
}

#[cfg(not(unix))]
pub(super) fn unix_nlink(_meta: &fs::Metadata) -> u64 {
    1
}

pub(super) fn regular_unaliased_private(meta: &fs::Metadata) -> bool {
    meta.is_file() && unix_nlink(meta) == 1 && is_private_mode(unix_mode(meta))
}

/// `Path.absolute()` — prepend the cwd without resolving links.
pub(super) fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    }
}

/// `uuid.uuid4()` — a v4 UUID string (OS randomness).
pub(super) fn uuid4() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        return String::new();
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// `_path` — `<project>/gsc-inspection.sqlite` behind the manifest checks.
pub(super) fn history_path(project: &Path) -> Result<PathBuf, HistoryError> {
    let meta = project.symlink_metadata().ok();
    if meta.as_ref().map(|m| m.is_symlink()).unwrap_or(false) || !project.is_dir() {
        return invalid("project must be a regular directory");
    }
    let manifest = project.join("project.json");
    let ok = manifest
        .symlink_metadata()
        .map(|m| m.is_file() && !m.file_type().is_symlink() && unix_nlink(&m) == 1)
        .unwrap_or(false);
    if !ok {
        return invalid("project requires a regular, unaliased project.json");
    }
    Ok(project.join(DB_NAME))
}

/// `_quota_path` — `<config root>/spiderhead/gsc-inspection-quota.sqlite`, private dir.
pub(super) fn quota_path(root: &Path) -> Result<PathBuf, HistoryError> {
    let directory = root.join("spiderhead");
    if directory
        .symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        return invalid("GSC inspection quota directory must not be a symlink");
    }
    if !directory.exists() {
        fs::create_dir_all(&directory)?;
        #[cfg(unix)]
        fs::set_permissions(
            &directory,
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )?;
    }
    let meta = directory.metadata()?;
    if !meta.is_dir() {
        // `mkdir(exist_ok=True)` raises `FileExistsError` on a non-directory.
        return Err(HistoryError::Io(std::io::Error::from_raw_os_error(17)));
    }
    if !is_private_mode(unix_mode(&meta)) {
        return invalid("GSC inspection quota directory must be private");
    }
    Ok(directory.join("gsc-inspection-quota.sqlite"))
}

/// `os.open(path, O_CREAT|O_EXCL|O_RDWR, 0o600)` — create the DB file privately once.
pub(super) fn create_private(path: &Path) -> Result<(), HistoryError> {
    if path.symlink_metadata().is_err() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .mode(0o600)
                .open(path)?;
        }
        #[cfg(not(unix))]
        fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path)?;
    }
    Ok(())
}

pub(super) fn table_names(con: &Connection) -> Result<Vec<String>, HistoryError> {
    let names = con
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(names)
}

pub(super) fn set_of(items: &[&str]) -> std::collections::BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// `_connect` — open/verify the history database, creating the schema on a fresh file.
pub(super) fn connect(path: &Path, create: bool) -> Result<Connection, HistoryError> {
    if create {
        create_private(path)?;
    }
    let meta = path.symlink_metadata()?;
    if !regular_unaliased_private(&meta) {
        return invalid("GSC inspection history must be a regular unaliased file");
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
    let check = (|| -> Result<(), HistoryError> {
        let tables: std::collections::BTreeSet<String> = table_names(&con)?.into_iter().collect();
        if tables.is_empty() && create && meta.len() == 0 {
            con.execute_batch(
                "CREATE TABLE metadata(format TEXT PRIMARY KEY);
                 CREATE TABLE inspections(id INTEGER PRIMARY KEY,site_url TEXT NOT NULL,url TEXT NOT NULL,
                    inspected_at TEXT NOT NULL,state TEXT NOT NULL,index_status TEXT,amp_result TEXT,
                    rich_results_result TEXT);
                 CREATE INDEX inspections_site_time ON inspections(site_url,inspected_at DESC,id DESC);",
            )?;
            con.execute("INSERT INTO metadata VALUES (?)", [FORMAT])?;
        } else if tables != set_of(&["metadata", "inspections"]) {
            return invalid("existing database is not GSC inspection history");
        } else {
            let formats: Vec<String> = con
                .prepare("SELECT format FROM metadata")?
                .query_map([], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            if formats != [FORMAT.to_string()] {
                return invalid("unsupported GSC inspection history format");
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

/// `_connect_quota` — open/verify the reservation ledger (v1 → v2 adds `urls`).
pub(super) fn connect_quota(path: &Path) -> Result<Connection, HistoryError> {
    create_private(path)?;
    let meta = path.symlink_metadata()?;
    if !regular_unaliased_private(&meta) {
        return invalid("GSC inspection quota must be a regular unaliased file");
    }
    let con =
        spiderhead_core::sqlite::open_writer(path, spiderhead_core::sqlite::DEFAULT_TIMEOUT, &[])?;
    con.execute_batch("PRAGMA trusted_schema=OFF")?;
    con.execute_batch("BEGIN IMMEDIATE")?;
    let check = (|| -> Result<(), HistoryError> {
        let tables: std::collections::BTreeSet<String> = table_names(&con)?.into_iter().collect();
        if tables.is_empty() && meta.len() == 0 {
            con.execute_batch(
                "CREATE TABLE metadata(format TEXT PRIMARY KEY);
                 CREATE TABLE reservations(id TEXT PRIMARY KEY,site_url TEXT NOT NULL,
                    reserved_at TEXT NOT NULL,count INTEGER NOT NULL,state TEXT NOT NULL,
                    urls TEXT NOT NULL DEFAULT '[]');
                 CREATE INDEX reservations_site_time ON reservations(site_url,reserved_at);",
            )?;
            con.execute("INSERT INTO metadata VALUES (?)", [QUOTA_FORMAT])?;
        } else if tables != set_of(&["metadata", "reservations"]) {
            return invalid("existing database is not a GSC inspection quota ledger");
        } else {
            let formats: Vec<String> = con
                .prepare("SELECT format FROM metadata")?
                .query_map([], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            let columns: std::collections::BTreeSet<String> = con
                .prepare("PRAGMA table_info(reservations)")?
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<_, _>>()?;
            if formats == ["spiderhead.gsc-inspection-quota.v1".to_string()] {
                if !columns.contains("urls") {
                    con.execute_batch(
                        "ALTER TABLE reservations ADD COLUMN urls TEXT NOT NULL DEFAULT '[]'",
                    )?;
                }
                con.execute("UPDATE metadata SET format=?", [QUOTA_FORMAT])?;
            } else if formats != [QUOTA_FORMAT.to_string()] {
                return invalid("unsupported GSC inspection quota format");
            } else if !columns.contains("urls") {
                return invalid("GSC quota ledger is missing its URL population binding");
            }
        }
        Ok(())
    })();
    if check.is_err() {
        let _ = con.execute_batch("ROLLBACK");
    }
    check?;
    Ok(con)
}
