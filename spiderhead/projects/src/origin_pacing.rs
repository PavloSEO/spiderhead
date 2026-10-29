//! Cross-process origin pacing for explicit native captures.
//!
//! The store lives in the project when one is given, otherwise in the user state directory
//! (`SPIDERHEAD_CONFIG_DIR`, else `~/.config/spiderhead`), so a projectless crawl shares the
//! same per-host ceiling across processes.

use spiderhead_core::filesystem::{PrivateMode, StatCompat};
use std::path::{Path, PathBuf};

use crate::error::{io, value, ProjectsError, ProjectsResult};
use crate::workspace::load as workspace_load;

/// The shared per-host ceiling applied to public crawls.
pub const PUBLIC_PROJECT_MAX_REQUESTS_PER_SECOND: f64 = 2.0;
const MAX_STORED_WAIT_SECONDS: f64 = 60.0;
const NAME: &str = ".origin-pacing.sqlite";

/// `_origin`: hostname of the project origin target.
pub fn origin(target: &str) -> ProjectsResult<String> {
    let parsed =
        crate::urls::urlsplit(target).map_err(|_| value("project origin needs a hostname"))?;
    let host = parsed
        .hostname()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| value("project origin needs a hostname"))?;
    Ok(host)
}

fn user_state_dir() -> ProjectsResult<PathBuf> {
    let host = spiderhead_core::host::SystemHost;
    let raw = spiderhead_core::envvar::read_env(&host, "SPIDERHEAD_CONFIG_DIR");
    let xdg = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs_home().join(".config"));
    let root = match raw {
        Some(raw) => PathBuf::from(raw),
        None => {
            if xdg.join("seohead").exists() && !xdg.join("spiderhead").exists() {
                xdg.join("seohead")
            } else {
                xdg.join("spiderhead")
            }
        }
    };
    // `mkdir(parents=True, exist_ok=True, mode=0o700)`: the mode applies to a
    // newly created leaf only; an existing directory is left untouched.
    if !root.exists() {
        std::fs::create_dir_all(&root).map_err(ProjectsError::Io)?;
        let _ = spiderhead_core::filesystem::set_permissions_mode(&root, 0o700);
    }
    Ok(root)
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// `ProjectOriginPacer`: reserve one request turn across local processes for one host.
///
/// `directory=None` stores the schedule in the user state directory instead of a project.
pub struct ProjectOriginPacer {
    root: PathBuf,
    origin: String,
    max_requests_per_second: f64,
    interval_seconds: f64,
    path: PathBuf,
}

impl ProjectOriginPacer {
    /// `ProjectOriginPacer(directory, target, minimum_delay_seconds, max_requests_per_second)`.
    pub fn new(
        directory: Option<&Path>,
        target: &str,
        minimum_delay_seconds: f64,
        max_requests_per_second: f64,
    ) -> ProjectsResult<Self> {
        let root = match directory {
            None => user_state_dir()?,
            Some(dir) => workspace_load(dir)?.0,
        };
        if !minimum_delay_seconds.is_finite() || minimum_delay_seconds < 0.0 {
            return Err(value("minimum delay must be finite and nonnegative"));
        }
        if !max_requests_per_second.is_finite() || max_requests_per_second < 0.0 {
            return Err(value(
                "maximum request rate must be finite and nonnegative; zero disables pacing",
            ));
        }
        let interval = if max_requests_per_second != 0.0 {
            1.0 / max_requests_per_second
        } else {
            0.0
        };
        let path = root.join(NAME);
        Ok(Self {
            root,
            origin: origin(target)?,
            max_requests_per_second,
            interval_seconds: interval,
            path,
        })
    }

    /// `set_min_delay`: follow a live operator rate change with the same ceiling rule.
    pub fn set_min_delay(&mut self, min_delay_seconds: f64) {
        let rate = if min_delay_seconds > 0.0 {
            1.0 / min_delay_seconds
        } else {
            0.0
        };
        self.max_requests_per_second = if rate != 0.0 {
            PUBLIC_PROJECT_MAX_REQUESTS_PER_SECOND.max(rate)
        } else {
            0.0
        };
        self.interval_seconds = if self.max_requests_per_second != 0.0 {
            1.0 / self.max_requests_per_second
        } else {
            0.0
        };
    }

    fn ensure_private_store(&self) -> ProjectsResult<()> {
        if self.path.is_symlink() {
            return Err(value("project origin pacing store must not be a symlink"));
        }
        // `os.open(O_RDWR | O_CREAT | O_EXCL, 0o600)`; FileExistsError is tolerated.
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .private_mode(0o600)
            .open(&self.path)
        {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(ProjectsError::Io(e)),
        }
        let meta = io(std::fs::symlink_metadata(&self.path))?;
        if !meta.is_file() || meta.mode() & 0o077 != 0 {
            return Err(value("project origin pacing store must be private"));
        }
        Ok(())
    }

    fn connect(&self) -> ProjectsResult<rusqlite::Connection> {
        self.ensure_private_store()?;
        let con = rusqlite::Connection::open(&self.path).map_err(|e| value(e.to_string()))?;
        con.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| value(e.to_string()))?;
        con.execute_batch(
            "CREATE TABLE IF NOT EXISTS origin_turns \
             (origin TEXT PRIMARY KEY, next_at REAL NOT NULL)",
        )
        .map_err(|e| value(e.to_string()))?;
        let meta = io(std::fs::symlink_metadata(&self.path))?;
        if !meta.is_file() || meta.mode() & 0o077 != 0 {
            return Err(value("project origin pacing store must be private"));
        }
        Ok(con)
    }

    /// `reserve`: atomically reserve a wall-clock request slot and return its wait time.
    pub fn reserve(&self) -> ProjectsResult<f64> {
        if self.interval_seconds == 0.0 {
            return Ok(0.0);
        }
        let con = self.connect()?;
        con.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| value(e.to_string()))?;
        let result = (|| -> ProjectsResult<f64> {
            let now = crate::pytime::time_seconds();
            let prior: f64 = match con.query_row(
                "SELECT next_at FROM origin_turns WHERE origin=?1",
                rusqlite::params![self.origin],
                |row| row.get(0),
            ) {
                Ok(value) => value,
                Err(rusqlite::Error::QueryReturnedNoRows) => now,
                Err(e) => return Err(value(e.to_string())),
            };
            if !prior.is_finite() {
                return Err(value(
                    "project origin pacing store contains an invalid timestamp",
                ));
            }
            if prior - now > MAX_STORED_WAIT_SECONDS {
                return Err(value(
                    "project origin pacing store requests an excessive wait",
                ));
            }
            let slot = now.max(prior);
            con.execute(
                "INSERT INTO origin_turns(origin,next_at) VALUES(?1,?2) \
                 ON CONFLICT(origin) DO UPDATE SET next_at=excluded.next_at",
                rusqlite::params![self.origin, slot + self.interval_seconds],
            )
            .map_err(|e| value(e.to_string()))?;
            Ok((slot - now).max(0.0))
        })();
        match result {
            Ok(wait) => {
                con.execute_batch("COMMIT")
                    .map_err(|e| value(e.to_string()))?;
                Ok(wait)
            }
            Err(e) => {
                let _ = con.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// `wait_turn`: sleep until the reserved slot.
    pub fn wait_turn(&self) -> ProjectsResult<()> {
        let delay = self.reserve()?;
        if delay > 0.0 {
            std::thread::sleep(std::time::Duration::from_secs_f64(delay));
        }
        Ok(())
    }

    /// The bound origin this pacer reserves slots for.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// `interval_seconds`: the enforced spacing between reserved slots.
    pub fn interval_seconds(&self) -> f64 {
        self.interval_seconds
    }

    /// The resolved store path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The workspace root the store lives in.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Current shared ceiling.
    pub fn max_requests_per_second(&self) -> f64 {
        self.max_requests_per_second
    }
}
