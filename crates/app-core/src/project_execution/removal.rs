//! Project-scoped cancellation when a workspace graph is removed.

use super::{ProjectExecutionError, ProjectExecutionService, ProjectExecutionStatus};

impl ProjectExecutionService {
    /// Cancels retained work for a removed project, including undelivered results.
    pub(crate) fn cancel_project(&self, project_root: &str) -> Result<(), ProjectExecutionError> {
        let jobs = self
            .lock_state()?
            .jobs
            .values()
            .filter(|job| {
                job.project_root == project_root && job.status != ProjectExecutionStatus::Cancelled
            })
            .map(|job| (job.requester_id.clone(), job.job_id.clone()))
            .collect::<Vec<_>>();
        for (requester, job_id) in jobs {
            self.cancel(&requester, &job_id)?;
        }
        Ok(())
    }
}
