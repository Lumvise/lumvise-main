//! Keeps one healthy MCP execution connection per project; explicit reconnects retain replay.
use super::*;

impl ProjectExecutionControlTransport {
    pub(crate) fn register(
        &self,
        service: &ProjectExecutionService,
        provider_id: &str,
        project_root: &str,
        capabilities: Vec<String>,
    ) -> Result<String, ProjectExecutionError> {
        validate_identifier(provider_id, "non-empty provider identifier")?;
        let mut connections = self.lock_connections()?;
        let previous = connections
            .get(provider_id)
            .map(|connection| connection.connection_token.as_str());
        let connection_token = new_opaque_id("project-provider")?;
        let (commands, receiver) = mpsc::sync_channel(PROVIDER_COMMAND_CAPACITY);
        service.register_with_admission(
            &connection_token,
            project_root,
            capabilities,
            commands,
            ProviderAdmission::Reconnect(previous),
        )?;
        connections.insert(
            provider_id.to_string(),
            ProviderConnection {
                connection_token: connection_token.clone(),
                project_root: normalized_project_root(project_root)?,
                commands: Arc::new(Mutex::new(receiver)),
            },
        );
        Ok(connection_token)
    }
}

impl ProjectExecutionService {
    /// Registers the live provider for one exact project root.
    ///
    /// A later registration replaces the previous connection for that project.
    pub fn register_provider(
        &self,
        provider_id: &str,
        project_root: &str,
        capabilities: impl IntoIterator<Item = String>,
        commands: mpsc::SyncSender<ProjectExecutionCommand>,
    ) -> Result<(), ProjectExecutionError> {
        self.register_with_admission(
            provider_id,
            project_root,
            capabilities,
            commands,
            ProviderAdmission::Replace,
        )
    }

    fn register_with_admission(
        &self,
        provider_id: &str,
        project_root: &str,
        capabilities: impl IntoIterator<Item = String>,
        commands: mpsc::SyncSender<ProjectExecutionCommand>,
        admission: ProviderAdmission<'_>,
    ) -> Result<(), ProjectExecutionError> {
        validate_identifier(provider_id, "non-empty provider identifier")?;
        let project_root = normalized_project_root(project_root)?;
        let capabilities = validated_capabilities(capabilities)?;
        let provider = RegisteredProvider {
            provider_id: provider_id.to_string(),
            capabilities,
            commands,
            lease_deadline: Instant::now() + PROVIDER_LEASE_DURATION,
        };
        let mut state = self.lock_state()?;
        expire_provider_leases(&mut state, Instant::now());
        admission.check(
            state.providers.get(&project_root),
            provider_id,
            &project_root,
        )?;
        redispatch_active_jobs(&state, &provider, &project_root)?;
        state.providers.insert(project_root, provider);
        Ok(())
    }
}

enum ProviderAdmission<'a> {
    Replace,
    Reconnect(Option<&'a str>),
}

impl ProviderAdmission<'_> {
    fn check(
        &self,
        current: Option<&RegisteredProvider>,
        candidate: &str,
        project: &str,
    ) -> Result<(), ProjectExecutionError> {
        let (Self::Reconnect(previous), Some(current)) = (self, current) else {
            return Ok(());
        };
        if *previous == Some(current.provider_id.as_str()) {
            return Ok(());
        }
        Err(ProjectExecutionError::InvalidRequest {
            value: format!("provider `{candidate}` for project `{project}`"),
            expected: "an unleased project or reconnection of its existing execution provider"
                .into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn submission(key: &str) -> ProjectExecutionRequest {
        ProjectExecutionRequest {
            requester_id: "caller".into(),
            project_root: "/project".into(),
            capability_id: "run".into(),
            idempotency_key: key.into(),
            input: serde_json::json!({"source": key}),
            priority: ProjectExecutionPriority::default(),
        }
    }

    #[test]
    fn another_mcp_client_cannot_displace_a_healthy_provider_or_replay_its_job() {
        let service = ProjectExecutionService::default();
        let transport = ProjectExecutionControlTransport::default();
        let token = transport
            .register(&service, "client-a", "/project", vec!["run".into()])
            .unwrap();
        let job = service.submit(submission("original")).unwrap();
        transport
            .next(&service, "client-a", &token, Duration::ZERO)
            .unwrap()
            .unwrap();
        for client in ["client-b", "client-c", "client-d"] {
            assert!(
                transport
                    .register(&service, client, "/project", vec!["run".into()])
                    .is_err()
            );
        }
        service.heartbeat(&token).unwrap();
        assert_eq!(
            transport
                .next(&service, "client-a", &token, Duration::ZERO)
                .unwrap(),
            None
        );
        service
            .record_result(&token, &job.job_id, Ok(Value::Null))
            .unwrap();
    }

    #[test]
    fn the_same_client_can_reconnect_and_replay_original_input() {
        let service = ProjectExecutionService::default();
        let transport = ProjectExecutionControlTransport::default();
        let original = transport
            .register(&service, "client", "/project", vec!["run".into()])
            .unwrap();
        let job = service.submit(submission("original")).unwrap();
        let replacement = transport
            .register(&service, "client", "/project", vec!["run".into()])
            .unwrap();
        assert_ne!(original, replacement);
        assert_eq!(
            transport
                .next(&service, "client", &replacement, Duration::ZERO)
                .unwrap(),
            Some(ProjectExecutionCommand::Execute {
                job_id: job.job_id,
                capability_id: "run".into(),
                input: serde_json::json!({"source":"original"})
            })
        );
        assert!(
            transport
                .next(&service, "client", &original, Duration::ZERO)
                .is_err()
        );
    }

    #[test]
    fn another_client_can_take_over_after_the_provider_lease_expires() {
        let service = ProjectExecutionService::default();
        let transport = ProjectExecutionControlTransport::default();
        transport
            .register(&service, "client-a", "/project", vec!["run".into()])
            .unwrap();
        service
            .expire_provider_leases_at(Instant::now() + PROVIDER_LEASE_DURATION)
            .unwrap();
        let token = transport
            .register(&service, "client-b", "/project", vec!["run".into()])
            .unwrap();
        service.submit(submission("after-failover")).unwrap();
        assert!(
            transport
                .next(&service, "client-b", &token, Duration::ZERO)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn different_projects_keep_independent_providers() {
        let service = ProjectExecutionService::default();
        let transport = ProjectExecutionControlTransport::default();
        let a = transport
            .register(&service, "a", "/a", vec!["run".into()])
            .unwrap();
        let b = transport
            .register(&service, "b", "/b", vec!["run".into()])
            .unwrap();
        service.heartbeat(&a).unwrap();
        service.heartbeat(&b).unwrap();
    }
}
