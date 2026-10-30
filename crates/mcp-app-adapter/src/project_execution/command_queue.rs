//! Keeps native jobs serial while allowing cancellation messages to reach the active job.
use super::ProjectExecutionCommand;
use std::collections::VecDeque;

#[derive(Default)]
pub(super) struct ProjectCommandQueue {
    pending: VecDeque<ProjectExecutionCommand>,
}

impl ProjectCommandQueue {
    pub(super) fn pop(&mut self) -> Option<ProjectExecutionCommand> {
        self.pending.pop_front()
    }

    pub(super) fn observe(&mut self, command: ProjectExecutionCommand, active: &str) -> bool {
        match command {
            ProjectExecutionCommand::Cancel { job_id } => {
                self.pending.retain(|command| {
                    !matches!(command,
                    ProjectExecutionCommand::Execute { job_id: queued, .. } if queued == &job_id)
                });
                job_id == active
            }
            ProjectExecutionCommand::Execute { ref job_id, .. } if job_id == active => false,
            command => {
                self.pending.push_back(command);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn execution(job_id: &str) -> ProjectExecutionCommand {
        ProjectExecutionCommand::Execute {
            job_id: job_id.into(),
            capability_id: "test".into(),
            input: Value::Null,
        }
    }

    #[test]
    fn cancellation_reaches_the_running_job_without_starting_queued_work() {
        let mut queue = ProjectCommandQueue::default();
        assert!(!queue.observe(execution("next"), "active"));
        assert!(queue.observe(
            ProjectExecutionCommand::Cancel {
                job_id: "active".into()
            },
            "active"
        ));
        assert!(
            matches!(queue.pop(), Some(ProjectExecutionCommand::Execute { job_id, .. }) if job_id == "next")
        );
        assert!(queue.pop().is_none());
    }

    #[test]
    fn cancelling_a_queued_job_preserves_other_jobs_in_order() {
        let mut queue = ProjectCommandQueue::default();
        for job in ["first", "cancelled", "last"] {
            queue.observe(execution(job), "active");
        }
        assert!(!queue.observe(
            ProjectExecutionCommand::Cancel {
                job_id: "cancelled".into()
            },
            "active"
        ));
        for expected in ["first", "last"] {
            assert!(
                matches!(queue.pop(), Some(ProjectExecutionCommand::Execute { job_id, .. }) if job_id == expected)
            );
        }
        assert!(queue.pop().is_none());
    }

    #[test]
    fn redispatching_the_active_job_does_not_queue_another_execution() {
        let mut queue = ProjectCommandQueue::default();
        assert!(!queue.observe(execution("active"), "active"));
        assert!(queue.pop().is_none());
    }
}
