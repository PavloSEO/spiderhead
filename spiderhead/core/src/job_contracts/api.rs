//! API error bodies, principals, and the backend trait seams.

use super::*;

/// The detail of an API error body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiErrorDetail {
    /// Machine-readable code.
    pub code: String,
    /// Human-readable message.
    pub message: String,
}

/// The body of an API error response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiErrorResponse {
    /// The error detail.
    pub error: ApiErrorDetail,
}

/// An authenticated caller and the permissions it holds per project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// Subject identifier of the caller.
    pub subject: String,
    /// Permissions by project identifier.
    pub grants: BTreeMap<String, BTreeSet<Permission>>,
}

impl Principal {
    /// Whether the caller holds `permission` in `project_id`.
    pub fn allows(&self, project_id: &str, permission: Permission) -> bool {
        self.grants
            .get(project_id)
            .is_some_and(|granted| granted.contains(&permission))
    }
}

/// The outcome of a submission: the job and whether this call created it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitOutcome {
    /// The job.
    pub job: JobStatus,
    /// `false` when an identical earlier submission was returned.
    pub created: bool,
}

/// An already-authorised open file; the response owns closing its handle.
pub struct OpenedArtifact {
    /// The open file.
    pub handle: Box<dyn Read + Send>,
    /// Size in bytes.
    pub size_bytes: u64,
    /// Download file name.
    pub filename: String,
    /// Media type.
    pub media_type: String,
}

/// Storage of remote jobs. Implementations must enforce project isolation and idempotency
/// atomically, so a retried submission never creates a second job.
pub trait JobBackend {
    /// Create the job, or return the existing one for the same idempotency key.
    fn submit(
        &self,
        project_id: &str,
        subject: &str,
        idempotency_key: &str,
        fingerprint: &str,
        request: &ScanSubmission,
        effective_config: &Value,
    ) -> Result<SubmitOutcome, JobError>;
    /// One page of a project's jobs.
    fn list_jobs(&self, project_id: &str, offset: u64, limit: u64) -> Vec<JobStatus>;
    /// A job of a project, when it exists.
    fn get_job(&self, project_id: &str, job_id: &str) -> Option<JobStatus>;
    /// Request cancellation of a job, when it exists.
    fn cancel_job(&self, project_id: &str, job_id: &str) -> Option<JobStatus>;
    /// The terminal result of a job, when recorded.
    fn get_result(&self, project_id: &str, job_id: &str) -> Option<JobResult>;
    /// The on-disk path of an artifact, when it exists.
    fn artifact_path(&self, project_id: &str, job_id: &str, artifact_id: &str) -> Option<PathBuf>;
    /// An opened artifact, when it exists and is authorised.
    fn open_artifact(
        &self,
        project_id: &str,
        job_id: &str,
        artifact_id: &str,
    ) -> Option<OpenedArtifact>;
}

/// Decides whether a target may be scanned. Missing policy must deny submissions.
pub trait RemoteTargetPolicy {
    /// Return `Err` to refuse the submission before any I/O.
    fn authorize_submission(
        &self,
        project_id: &str,
        target_url: &str,
        effective_config: &Value,
    ) -> Result<(), JobError>;
}
