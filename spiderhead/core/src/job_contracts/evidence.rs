//! Job progress, status, source and result evidence records.

use super::*;

/// Counters of a job's crawl progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobProgress {
    /// URLs waiting.
    pub queued: u64,
    /// URLs being fetched.
    pub inflight: u64,
    /// URLs completed.
    pub done: u64,
    /// URLs excluded by scope rules.
    pub excluded: u64,
    /// Pages committed.
    pub pages: u64,
}

/// One job as the API reports it. Timestamps are kept as the wire strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobStatus {
    /// Job identifier, 1 to 128 characters.
    pub job_id: String,
    /// Project identifier, 1 to 64 characters.
    pub project_id: String,
    /// Lifecycle state.
    pub state: JobState,
    /// Creation time.
    pub created_at: String,
    /// Start time, when started.
    pub started_at: Option<String>,
    /// Finish time, when finished.
    pub finished_at: Option<String>,
    /// Why the job ended, at most 500 characters.
    pub finish_reason: Option<String>,
    /// Crawl counters.
    pub progress: JobProgress,
}

impl JobStatus {
    /// Check the identifier, reason and length bounds.
    pub fn validated(self) -> Result<Self, JobError> {
        check_text_length("job_id", &self.job_id, 1, 128)?;
        check_text_length("project_id", &self.project_id, 1, 64)?;
        if let Some(reason) = &self.finish_reason {
            check_text_length("finish_reason", reason, 0, 500)?;
        }
        Ok(self)
    }
}

/// One page of job listings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobList {
    /// Jobs on this page.
    pub items: Vec<JobStatus>,
    /// Offset of this page.
    pub offset: u64,
    /// Page size, 1 to 100.
    pub limit: u64,
    /// Whether another page exists.
    pub has_more: bool,
}

/// The four lifecycle states a scan source can report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    /// Still being written.
    Running,
    /// Stopped before finishing.
    Interrupted,
    /// Completed.
    Finished,
    /// Ended with an error.
    Failed,
}

/// Where a scan's data came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Crawled by this toolkit.
    Native,
    /// Imported from an older export format.
    LegacyImport,
    /// Re-analysed from earlier evidence.
    Reanalysis,
}

/// Identity and lifecycle of the scan a result came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanSource {
    /// Scan UUID.
    pub scan_uuid: String,
    /// Scan format tag, one of [`SCAN_FORMATS`].
    pub format_version: String,
    /// Where the data came from.
    pub source_kind: SourceKind,
    /// The scan this one was derived from, if any.
    pub parent_scan_uuid: Option<String>,
    /// Version of the writer.
    pub writer_version: String,
    /// Revision of the writer.
    pub writer_revision: String,
    /// Evidence revision counter.
    pub evidence_revision: u64,
    /// Creation time.
    pub created_at: String,
    /// Finish time, when finished.
    pub finished_at: Option<String>,
    /// Lifecycle state.
    pub lifecycle: Lifecycle,
    /// Why the scan ended.
    pub finish_reason: String,
    /// Whether the crawl stopped short of its frontier.
    pub crawl_partial: bool,
    /// Whether the retained corpus is partial.
    pub corpus_partial: bool,
}

impl ScanSource {
    /// Check that the format tag is one this build reads.
    pub fn validated(self) -> Result<Self, JobError> {
        if SCAN_FORMATS.contains(&self.format_version.as_str()) {
            Ok(self)
        } else {
            Err(JobError::Invalid("format_version is unsupported".into()))
        }
    }
}

/// Frontier counters of a scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrontierCounts {
    /// URLs waiting.
    pub queued: u64,
    /// URLs being fetched.
    pub inflight: u64,
    /// URLs completed.
    pub done: u64,
    /// URLs excluded.
    pub excluded: u64,
}

/// Whether the frontier could be read, and why not when it could not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanFrontier {
    /// `available` or `unavailable`.
    pub state: String,
    /// Reason text.
    pub reason: String,
    /// Counters, when available.
    pub counts: Option<FrontierCounts>,
}

/// Status-class counts of committed pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommittedPageOutcomes {
    /// Responses with a 2xx status.
    #[serde(rename = "2xx")]
    pub status_2xx: u64,
    /// Responses with a 3xx status.
    #[serde(rename = "3xx")]
    pub status_3xx: u64,
    /// Responses with a 4xx status.
    #[serde(rename = "4xx")]
    pub status_4xx: u64,
    /// Responses with a 5xx status.
    #[serde(rename = "5xx")]
    pub status_5xx: u64,
    /// Any other status.
    pub other: u64,
    /// Pages without a response.
    pub no_response: u64,
}

/// The evidence block the scan-status reader returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanEvidence {
    /// Scan identity.
    pub source: ScanSource,
    /// Frontier state.
    pub frontier: ScanFrontier,
    /// Committed page outcomes.
    pub committed_page_outcomes: CommittedPageOutcomes,
}

/// A produced artifact a client may fetch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactReference {
    /// Artifact identifier, 1 to 128 characters.
    pub artifact_id: String,
    /// Artifact kind, 1 to 64 characters.
    pub kind: String,
    /// Media type, 1 to 128 characters.
    pub media_type: String,
    /// Size in bytes.
    pub size_bytes: u64,
}

impl ArtifactReference {
    /// Check the identifier, kind and media-type bounds.
    pub fn validated(self) -> Result<Self, JobError> {
        check_text_length("artifact_id", &self.artifact_id, 1, 128)?;
        check_text_length("kind", &self.kind, 1, 64)?;
        check_text_length("media_type", &self.media_type, 1, 128)?;
        Ok(self)
    }
}

/// How complete a job's result is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    /// Everything was crawled and audited.
    Complete,
    /// Some of it was.
    Partial,
    /// Nothing usable was produced.
    Failed,
    /// Not attempted.
    Skipped,
}

/// The terminal result of a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobResult {
    /// The job this result belongs to.
    pub job: JobStatus,
    /// Coverage of the result.
    pub coverage: Coverage,
    /// Retained scan evidence, when there is any.
    pub evidence: Option<ScanEvidence>,
    /// Artifacts produced.
    #[serde(default)]
    pub artifacts: Vec<ArtifactReference>,
    /// Whether the audit could be produced.
    pub audit_available: bool,
    /// Why not, at most 500 characters; required when the audit is unavailable.
    pub audit_reason: String,
}

impl JobResult {
    /// A complete result needs evidence and an audit; an unavailable audit needs a reason.
    pub fn validated(self) -> Result<Self, JobError> {
        check_text_length("audit_reason", &self.audit_reason, 0, 500)?;
        if self.coverage == Coverage::Complete && (self.evidence.is_none() || !self.audit_available)
        {
            return Err(JobError::Invalid(
                "complete result requires retained scan evidence and an audit".into(),
            ));
        }
        if !self.audit_available && self.audit_reason.is_empty() {
            return Err(JobError::Invalid(
                "unavailable audit requires a reason".into(),
            ));
        }
        Ok(self)
    }
}
