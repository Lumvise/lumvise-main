//! Maps the execution broker's lifecycle into workspace activity without
//! exposing provider inputs or retaining generated response content.

use super::{
    ProjectExecutionJob, ProjectExecutionRequest, ProjectExecutionState, ProjectExecutionStatus,
};
use crate::workspace_activity::{ActivityEntry, ActivityKind, ActivityStatus};

impl ProjectExecutionState {
    pub(super) fn record_activity(
        &self,
        request: &ProjectExecutionRequest,
        job: &ProjectExecutionJob,
    ) {
        if request.capability_id != "semantic.generate_functional_artifacts.v1" {
            return;
        }
        let title = match (
            request.input["name"].as_str(),
            request.input["path"].as_str(),
        ) {
            (Some(name), Some(path)) => format!("{name} · {path}"),
            _ => "Generate functional knowledge".into(),
        };
        self.activity.record(ActivityEntry {
            id: job.job_id.clone(),
            project_root: Some(job.project_root.clone()),
            title,
            kind: ActivityKind::Knowledge,
            status: ActivityStatus::Queued,
            semantic_element_id: request.input["semantic_element_id"]
                .as_str()
                .map(str::to_owned),
            artifact_id: request.input["artifact_id"].as_str().map(str::to_owned),
            detail: None,
        });
    }

    pub(super) fn publish_activity(&self, job_id: &str) {
        let Some(job) = self.jobs.get(job_id) else {
            return;
        };
        let status = match job.status {
            ProjectExecutionStatus::Queued => ActivityStatus::Queued,
            ProjectExecutionStatus::Running => ActivityStatus::Running,
            ProjectExecutionStatus::Succeeded => ActivityStatus::Succeeded,
            ProjectExecutionStatus::Failed => ActivityStatus::Failed,
            ProjectExecutionStatus::Cancelled => ActivityStatus::Cancelled,
        };
        let detail = job.error.as_ref().or(job.progress.as_ref()).cloned();
        self.activity.update(job_id, status, detail);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ProjectExecutionPriority, ProjectExecutionService};
    use serde_json::json;
    use std::sync::mpsc;
    use std::time::Duration;

    fn request(id: &str) -> ProjectExecutionRequest {
        ProjectExecutionRequest {
            requester_id: "builtin.knowledge".into(),
            project_root: "/repo".into(),
            capability_id: "semantic.generate_functional_artifacts.v1".into(),
            idempotency_key: id.into(),
            priority: ProjectExecutionPriority::Interactive,
            input: json!({"semantic_element_id": "function:parse", "artifact_id": "functional:parse", "name": "parse", "path": "src/parser.rs"}),
        }
    }

    #[test]
    fn execution_broker_publishes_queue_progress_result_and_cancellation() {
        let service = ProjectExecutionService::default();
        let (sender, _receiver) = mpsc::sync_channel(4);
        service
            .register_provider("provider", "/repo", [request("1").capability_id], sender)
            .unwrap();
        let activity = service.lock_state().unwrap().activity.clone();
        let job = service.submit(request("1")).unwrap();
        let entry = activity
            .wait_for_changes("/repo", None, Duration::ZERO)
            .entries
            .unwrap()
            .remove(0);
        assert_eq!(entry.status, ActivityStatus::Queued);
        assert_eq!(entry.semantic_element_id.as_deref(), Some("function:parse"));
        assert_eq!(entry.artifact_id.as_deref(), Some("functional:parse"));
        service
            .record_progress("provider", &job.job_id, "generating".into())
            .unwrap();
        assert_eq!(
            activity
                .wait_for_changes("/repo", None, Duration::ZERO)
                .entries
                .unwrap()[0]
                .status,
            ActivityStatus::Running
        );
        service
            .record_result(
                "provider",
                &job.job_id,
                Ok(json!({"private_response": "not retained"})),
            )
            .unwrap();
        service.status("builtin.knowledge", &job.job_id).unwrap();
        assert_eq!(
            activity
                .wait_for_changes("/repo", None, Duration::ZERO)
                .entries
                .unwrap()[0]
                .status,
            ActivityStatus::Succeeded
        );
        let cancelled = service.submit(request("2")).unwrap();
        service
            .cancel("builtin.knowledge", &cancelled.job_id)
            .unwrap();
        assert_eq!(
            activity
                .wait_for_changes("/repo", None, Duration::ZERO)
                .entries
                .unwrap()[0]
                .status,
            ActivityStatus::Cancelled
        );
    }
}
