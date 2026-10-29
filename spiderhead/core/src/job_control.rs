//! Small in-process progress contract shared by resumable local jobs.
//!
//! Persistence and resume belong to each job owner.

use thiserror::Error;

/// The closed set of job states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    /// Work is progressing.
    Running,
    /// A pause was requested and not yet honoured.
    PauseRequested,
    /// Paused.
    Paused,
    /// A stop was requested and not yet honoured.
    StopRequested,
    /// Stopped by request.
    Cancelled,
    /// Completed.
    Finished,
    /// Ended with an error.
    Failed,
}

impl JobState {
    /// Parse the wire name of a state, or `None` for an unknown name.
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "running" => Self::Running,
            "pause_requested" => Self::PauseRequested,
            "paused" => Self::Paused,
            "stop_requested" => Self::StopRequested,
            "cancelled" => Self::Cancelled,
            "finished" => Self::Finished,
            "failed" => Self::Failed,
            _ => return None,
        })
    }
}

/// Validation failure for a progress event.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct ProgressError(pub String);

/// One bounded progress event, validated on construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobProgressUpdate {
    /// Job identifier, 1 to 128 characters.
    pub job_id: String,
    /// Job state.
    pub state: JobState,
    /// Stage name, 1 to 64 characters.
    pub stage: String,
    /// Completed units, or unknown.
    pub completed: Option<u64>,
    /// Total units, or unknown.
    pub total: Option<u64>,
    /// Optional reason, at most 500 characters.
    pub reason: Option<String>,
}

impl JobProgressUpdate {
    /// Build a validated event. Counts are nonnegative integers; `None` means unknown.
    pub fn new(
        job_id: &str,
        state: &str,
        stage: &str,
        completed: Option<i64>,
        total: Option<i64>,
        reason: Option<&str>,
    ) -> Result<Self, ProgressError> {
        if job_id.is_empty() || job_id.chars().count() > 128 {
            return Err(ProgressError(
                "job_id must be non-empty text no longer than 128 characters".into(),
            ));
        }
        let state = JobState::parse(state)
            .ok_or_else(|| ProgressError("job state is unsupported".into()))?;
        if stage.is_empty() || stage.chars().count() > 64 {
            return Err(ProgressError(
                "stage must be non-empty text no longer than 64 characters".into(),
            ));
        }
        let count = |value: Option<i64>, name: &str| match value {
            None => Ok(None),
            Some(n) if n >= 0 => Ok(Some(n as u64)),
            Some(_) => Err(ProgressError(format!(
                "{name} must be a nonnegative integer or None"
            ))),
        };
        let completed = count(completed, "completed")?;
        let total = count(total, "total")?;
        if reason.is_some_and(|text| text.chars().count() > 500) {
            return Err(ProgressError(
                "reason must be text no longer than 500 characters or None".into(),
            ));
        }
        Ok(Self {
            job_id: job_id.to_owned(),
            state,
            stage: stage.to_owned(),
            completed,
            total,
            reason: reason.map(str::to_owned),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_unknown_total_and_missing_count() {
        let update = JobProgressUpdate::new("job-1", "running", "crawl", None, None, None).unwrap();
        assert_eq!(update.completed, None);
        assert_eq!(update.total, None);
        assert_eq!(update.state, JobState::Running);
    }

    #[test]
    fn rejects_ambiguous_or_unbounded_fields() {
        let long_id = "x".repeat(129);
        let long_stage = "s".repeat(65);
        let long_reason = "r".repeat(501);
        assert!(JobProgressUpdate::new("", "running", "crawl", None, None, None).is_err());
        assert!(JobProgressUpdate::new(&long_id, "running", "crawl", None, None, None).is_err());
        assert!(JobProgressUpdate::new("j", "bogus", "crawl", None, None, None).is_err());
        assert!(JobProgressUpdate::new("j", "running", "", None, None, None).is_err());
        assert!(JobProgressUpdate::new("j", "running", &long_stage, None, None, None).is_err());
        assert!(JobProgressUpdate::new("j", "running", "crawl", Some(-1), None, None).is_err());
        assert!(JobProgressUpdate::new("j", "running", "crawl", None, Some(-3), None).is_err());
        assert!(
            JobProgressUpdate::new("j", "running", "crawl", None, None, Some(&long_reason))
                .is_err()
        );
    }
}
