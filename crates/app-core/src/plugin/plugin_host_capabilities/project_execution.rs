//! Owns the project execution host adapter and its process-local job routing.
//! `PluginHostServices::project_execution` is the entrypoint; `scope` reads the saved startup identity.
use super::*;

impl PluginHostServices {
    pub(super) fn project_execution(
        &self,
        plugin_id: &str,
        input: Value,
    ) -> Result<Value, HostCapabilityError> {
        match decode(PROJECT_EXECUTION, input)? {
            ProjectExecutionHostRequest::Scope {} => self.project_execution_scope(),
            ProjectExecutionHostRequest::Submit(request) => {
                self.project_execution_job(|service| service.submit(request.for_plugin(plugin_id)))
            }
            ProjectExecutionHostRequest::Status { job_id } => {
                self.project_execution_job(|service| service.status(plugin_id, &job_id))
            }
            ProjectExecutionHostRequest::Cancel { job_id } => {
                self.project_execution_job(|service| service.cancel(plugin_id, &job_id))
            }
        }
    }

    fn project_execution_scope(&self) -> Result<Value, HostCapabilityError> {
        let client = self.index_publication_client.get().ok_or_else(|| {
            unavailable(
                PROJECT_EXECUTION,
                "client_instance_id is absent; expected the saved startup client identity",
            )
        })?;
        Ok(json!({"client_instance_id": client}))
    }

    fn project_execution_job(
        &self,
        call: impl FnOnce(
            &crate::ProjectExecutionService,
        ) -> Result<crate::ProjectExecutionJob, crate::ProjectExecutionError>,
    ) -> Result<Value, HostCapabilityError> {
        let service = self
            .project_execution
            .lock()
            .map_err(|_| failed(PROJECT_EXECUTION, "project execution slot mutex poisoned"))?
            .clone()
            .ok_or_else(|| unavailable(PROJECT_EXECUTION, "project execution is not installed"))?;
        let job = call(&service).map_err(project_execution_error)?;
        serde_json::to_value(job).map_err(|error| failed(PROJECT_EXECUTION, &error.to_string()))
    }
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum ProjectExecutionHostRequest {
    Scope {},
    Submit(ProjectExecutionSubmitRequest),
    Status { job_id: String },
    Cancel { job_id: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectExecutionSubmitRequest {
    project_root: String,
    capability_id: String,
    idempotency_key: String,
    input: Value,
    #[serde(default)]
    priority: Option<crate::ProjectExecutionPriority>,
}

impl ProjectExecutionSubmitRequest {
    fn for_plugin(self, plugin_id: &str) -> crate::ProjectExecutionRequest {
        crate::ProjectExecutionRequest {
            requester_id: plugin_id.into(),
            project_root: self.project_root,
            capability_id: self.capability_id,
            idempotency_key: self.idempotency_key,
            input: self.input,
            priority: self.priority.unwrap_or_default(),
        }
    }
}

pub(super) fn project_execution_error(error: crate::ProjectExecutionError) -> HostCapabilityError {
    match error {
        crate::ProjectExecutionError::CapacityExceeded { actual, maximum } => {
            quota(PROJECT_EXECUTION, actual, maximum)
        }
        crate::ProjectExecutionError::ProviderQueueFull { .. } => HostCapabilityError::new(
            PROJECT_EXECUTION,
            "host_capability_quota_exceeded",
            format!("{PROJECT_EXECUTION} quota exceeded: {error}"),
            true,
        ),
        crate::ProjectExecutionError::ProviderUnavailable { .. }
        | crate::ProjectExecutionError::JobNotFound { .. } => {
            unavailable(PROJECT_EXECUTION, &error.to_string())
        }
        error => failed(PROJECT_EXECUTION, &error.to_string()),
    }
}
