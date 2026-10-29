//! Stable remote scan request and evidence contracts, independent of job execution.
//!
//! Models reject unknown fields and out-of-range values. Datetimes stay as the wire strings;
//! scan-format tags are checked against `artifacts::SCAN_FORMATS`.
//!
//! Owned by other modules: `evidence_from_scan` (needs the scan status reader), the remote project
//! limits model, and the backend implementations behind [`JobBackend`] and [`RemoteTargetPolicy`].
//! The traits live here so those modules can name them.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::artifacts::SCAN_FORMATS;
use crate::common::canonical_json;
use crate::crawl_settings::{set_path, validate as validate_crawl_config};
use crate::host::Host;
use crate::settings_data::{defaults, MAX_REQUESTS_CEILING, MAX_URLS_CEILING};

/// A million pages at the default 2 requests/s needs more than five days, before bootstrap and
/// retry overhead. This is an explicit admission bound, not a default: 14 days in seconds.
pub const MAX_REMOTE_CRAWL_SECONDS: u64 = 14 * 24 * 60 * 60;

/// Longest accepted target URL, in characters.
const MAX_TARGET_CHARS: usize = 2_048;

/// Scopes a remote API token may hold for one project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Permission {
    /// Submit a scan.
    #[serde(rename = "scan:submit")]
    Submit,
    /// List scan jobs.
    #[serde(rename = "scan:list")]
    List,
    /// Read a scan job.
    #[serde(rename = "scan:read")]
    Read,
    /// Cancel a scan job.
    #[serde(rename = "scan:cancel")]
    Cancel,
    /// Read a scan result.
    #[serde(rename = "scan:result")]
    Result,
}

/// Lifecycle states of a remote job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Waiting for a worker.
    Queued,
    /// Being crawled.
    Running,
    /// A cancel was requested and not yet honoured.
    CancelRequested,
    /// Cancelled.
    Cancelled,
    /// Completed in full.
    Finished,
    /// Completed with partial coverage.
    Partial,
    /// Ended with an error.
    Failed,
}

/// Renderer selection of a remote scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderingMode {
    /// Plain HTML, no browser.
    #[default]
    Raw,
    /// Browser-rendered pages.
    Js,
}

/// Failures of the contract models and of the job backends built on them.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum JobError {
    /// A field broke its bound or format.
    #[error("{0}")]
    Invalid(String),
    /// The effective crawl settings were rejected by the shared validator.
    #[error("{0}")]
    Config(String),
    /// The same idempotency key was submitted with a different request body.
    #[error("the same idempotency key was submitted with a different request")]
    Conflict,
    /// The project queue has reached its configured admission limit.
    #[error("the project queue is full")]
    QueueFull,
    /// Trusted project resource limits reject the requested scan settings.
    #[error("{0}")]
    BudgetExceeded(String),
    /// A result was requested before a terminal outcome was recorded.
    #[error("the job has no terminal outcome yet")]
    NotReady,
    /// The target policy refused the submission.
    #[error("{0}")]
    PolicyDenied(String),
}

fn check_range(name: &str, value: u64, min: u64, max: u64) -> Result<(), JobError> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(JobError::Invalid(format!(
            "{name} must be between {min} and {max}"
        )))
    }
}

fn check_text_length(name: &str, text: &str, min: usize, max: usize) -> Result<(), JobError> {
    let length = text.chars().count();
    if (min..=max).contains(&length) {
        Ok(())
    } else {
        Err(JobError::Invalid(format!(
            "{name} must be {min} to {max} characters"
        )))
    }
}

/// A deliberately bounded, path-free subset of native crawl settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ScanOptions {
    /// URL budget, 1 to the global URL ceiling.
    pub max_urls: u64,
    /// Depth limit, 0 to 20.
    pub max_depth: u64,
    /// Request budget, 1 to the global request ceiling.
    pub max_requests: u64,
    /// Wall-clock budget in seconds, 1 to [`MAX_REMOTE_CRAWL_SECONDS`].
    pub max_crawl_seconds: u64,
    /// Parallel fetches, 1 to 4.
    pub concurrency: u64,
    /// Renderer selection.
    pub rendering_mode: RenderingMode,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            max_urls: 200,
            max_depth: 5,
            max_requests: 20_000,
            max_crawl_seconds: 3_600,
            concurrency: 1,
            rendering_mode: RenderingMode::Raw,
        }
    }
}

impl ScanOptions {
    /// Check every bound, returning the options unchanged when all hold.
    pub fn validated(self) -> Result<Self, JobError> {
        self.check()?;
        Ok(self)
    }

    /// Check every bound without consuming the options.
    pub fn check(&self) -> Result<(), JobError> {
        check_range("max_urls", self.max_urls, 1, MAX_URLS_CEILING)?;
        check_range("max_depth", self.max_depth, 0, 20)?;
        check_range("max_requests", self.max_requests, 1, MAX_REQUESTS_CEILING)?;
        check_range(
            "max_crawl_seconds",
            self.max_crawl_seconds,
            1,
            MAX_REMOTE_CRAWL_SECONDS,
        )?;
        check_range("concurrency", self.concurrency, 1, 4)
    }

    /// Resolve the options against the shared crawl settings, as the CLI handler does.
    ///
    /// The `js` renderer caps the escalation render time at 300 seconds, or at the crawl budget
    /// when that is shorter. The result passes the shared validator before it is returned.
    pub fn effective_config(&self, host: &dyn Host) -> Result<Value, JobError> {
        let mut config = defaults().clone();
        set_path(&mut config, "limits.max_urls", Value::from(self.max_urls));
        set_path(&mut config, "limits.max_depth", Value::from(self.max_depth));
        set_path(
            &mut config,
            "limits.max_requests",
            Value::from(self.max_requests),
        );
        set_path(
            &mut config,
            "limits.max_crawl_seconds",
            Value::from(self.max_crawl_seconds),
        );
        set_path(
            &mut config,
            "speed.concurrency",
            Value::from(self.concurrency),
        );
        let mode = match self.rendering_mode {
            RenderingMode::Raw => "raw",
            RenderingMode::Js => "js",
        };
        set_path(&mut config, "rendering.mode", Value::from(mode));
        if self.rendering_mode == RenderingMode::Js {
            set_path(
                &mut config,
                "rendering.escalation.max_render_seconds",
                Value::from(self.max_crawl_seconds.min(300)),
            );
        }
        validate_crawl_config(&config, host).map_err(|error| JobError::Config(error.0))?;
        Ok(config)
    }
}

/// A scan request as the remote API accepts it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanSubmission {
    /// Absolute HTTP(S) URL without credentials, fragment or control characters.
    pub target_url: String,
    /// Bounded crawl options.
    #[serde(default)]
    pub options: ScanOptions,
}

impl ScanSubmission {
    /// Check the target URL and the options. DNS, redirects and project allowlists are checked by
    /// the target policy, never inferred from URL syntax alone.
    pub fn validated(self) -> Result<Self, JobError> {
        check_text_length("target_url", &self.target_url, 10, MAX_TARGET_CHARS)?;
        check_target_syntax(&self.target_url)?;
        self.options.check()?;
        Ok(self)
    }

    /// SHA-256 over the canonical JSON of the request, the backend's idempotency fingerprint.
    pub fn fingerprint(&self) -> String {
        let body = serde_json::to_value(self).unwrap_or(Value::Null);
        Sha256::digest(canonical_json(&body).as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

fn check_target_syntax(value: &str) -> Result<(), JobError> {
    let message = "target_url must be an absolute HTTP(S) URL without credentials or fragment";
    if value != value.trim() || value.chars().any(|character| (character as u32) <= 32) {
        return Err(JobError::Invalid(
            "target_url must be an absolute HTTP(S) URL without whitespace".into(),
        ));
    }
    let url = url::Url::parse(value).map_err(|_| JobError::Invalid(message.into()))?;
    let authority = value.split_once("://").map_or("", |(_, rest)| {
        rest.split(['/', '?', '#']).next().unwrap_or("")
    });
    let credentials =
        authority.contains('@') || !url.username().is_empty() || url.password().is_some();
    let port_ok = url.port().is_none_or(|port| port >= 1);
    let valid = matches!(url.scheme(), "http" | "https")
        && !authority.is_empty()
        && url.host_str().is_some_and(|host| !host.is_empty())
        && !credentials
        && url.fragment().is_none()
        && port_ok;
    if valid {
        Ok(())
    } else {
        Err(JobError::Invalid(message.into()))
    }
}

mod api;
mod evidence;
pub use api::*;
pub use evidence::*;

#[cfg(test)]
mod tests;
