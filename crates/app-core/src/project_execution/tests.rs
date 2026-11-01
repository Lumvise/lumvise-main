use super::*;

#[test]
fn delivered_terminal_jobs_evict_oldest_first_with_idempotency() {
    let mut state = ProjectExecutionState::default();
    for index in 0..MAX_RETAINED_JOBS {
        let job_id = format!("job-{index}");
        state.jobs.insert(job_id.clone(), terminal_job(&job_id));
        mark_terminal(&mut state, &job_id);
        state.delivered.insert(job_id.clone());
        state.idempotency.insert(
            (
                "caller".into(),
                "/project".into(),
                "capability".into(),
                job_id.clone(),
            ),
            job_id,
        );
    }

    discard_terminal_jobs(&mut state);

    assert_eq!(state.jobs.len(), MAX_RETAINED_JOBS - 1);
    assert_eq!(state.idempotency.len(), MAX_RETAINED_JOBS - 1);
    // Oldest delivered terminal job goes first.
    assert!(!state.jobs.contains_key("job-0"));
    assert!(state.jobs.contains_key("job-1"));
    assert!(
        state
            .jobs
            .contains_key(&format!("job-{}", MAX_RETAINED_JOBS - 1))
    );
}

#[test]
fn undelivered_terminal_jobs_survive_retention_cap_until_hard_cap() {
    let mut state = ProjectExecutionState::default();
    for index in 0..1_100 {
        let job_id = format!("job-{index}");
        state.jobs.insert(job_id.clone(), terminal_job(&job_id));
        mark_terminal(&mut state, &job_id);
    }

    // No delivered terminal job: nothing is evicted below the hard cap.
    discard_terminal_jobs(&mut state);
    assert_eq!(state.jobs.len(), 1_100);
    for index in 0..1_100 {
        assert!(state.jobs.contains_key(&format!("job-{index}")));
    }

    // At the undelivered hard cap the oldest terminal job is evicted.
    discard_terminal_jobs_with(&mut state, 1_100, 1_100);
    assert_eq!(state.jobs.len(), 1_099);
    assert!(!state.jobs.contains_key("job-0"));
    assert!(state.jobs.contains_key("job-1"));
}

#[test]
fn undelivered_failed_jobs_stay_readable_and_evict_after_delivery() {
    let service = ProjectExecutionService::default();
    let (sender, receiver) = mpsc::sync_channel(PROVIDER_COMMAND_CAPACITY);
    let sender = Arc::new(sender);
    service
        .register_provider("provider", "/project", ["run".into()], (*sender).clone())
        .unwrap();
    let mut jobs = Vec::new();
    // 255 per wave keeps under MAX_ACTIVE_JOBS; expiring the lease between
    // waves terminalizes the wave without anyone reading its status.
    for wave in 0..5 {
        let wave_size = if wave == 4 { 80 } else { 255 };
        for _ in 0..wave_size {
            jobs.push(
                service
                    .submit(request("/project", &format!("job-{}", jobs.len())))
                    .unwrap(),
            );
            receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("provider command for each submission");
        }
        service
            .expire_provider_leases_at(Instant::now() + PROVIDER_LEASE_DURATION)
            .unwrap();
        // Expired the lease removed the provider; reconnect for next wave.
        service
            .register_provider("provider", "/project", ["run".into()], (*sender).clone())
            .unwrap();
    }

    // None of the failed jobs is evicted before its status was read.
    for job in &jobs {
        assert_eq!(
            service.status("caller", &job.job_id).unwrap().status,
            ProjectExecutionStatus::Failed
        );
    }

    // At the retention cap the next submission evicts delivered jobs
    // oldest-first (the whole first wave is older than any later wave);
    // within a wave the terminal order follows HashMap iteration, so
    // assert per-wave, not per-index.
    let extra = service.submit(request("/project", "extra")).unwrap();
    receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("provider command for the extra submission");
    let mut evicted = 0;
    for (index, job) in jobs.iter().enumerate() {
        let gone = service.status("caller", &job.job_id).is_err();
        assert!(
            !gone || index < 255,
            "a wave-2 job should never evict before wave-1 jobs: {index}"
        );
        evicted += usize::from(gone);
    }
    assert_eq!(evicted, 77);
    assert!(service.status("caller", &jobs[255].job_id).is_ok());
    assert_eq!(
        service.status("caller", &extra.job_id).unwrap().status,
        ProjectExecutionStatus::Queued
    );
}

#[test]
fn provider_queue_is_bounded_and_isolated() {
    let service = ProjectExecutionService::default();
    let (full_sender, _full_receiver) = mpsc::sync_channel(PROVIDER_COMMAND_CAPACITY);
    service
        .register_provider("provider-a", "/a", ["run".into()], full_sender)
        .unwrap();
    for index in 0..PROVIDER_COMMAND_CAPACITY {
        service
            .submit(request("/a", &format!("a-{index}")))
            .unwrap();
    }
    assert!(matches!(
        service.submit(request("/a", "overflow")),
        Err(ProjectExecutionError::ProviderQueueFull { .. })
    ));

    let (other_sender, other_receiver) = mpsc::sync_channel(1);
    service
        .register_provider("provider-b", "/b", ["run".into()], other_sender)
        .unwrap();
    service.submit(request("/b", "progress")).unwrap();
    assert!(matches!(
        other_receiver.recv().unwrap(),
        ProjectExecutionCommand::Execute { .. }
    ));
}

#[test]
fn lease_expiry_and_cancellation_are_terminal_against_late_results() {
    let service = ProjectExecutionService::default();
    let (sender, _receiver) = mpsc::sync_channel(4);
    service
        .register_provider("provider", "/project", ["run".into()], sender)
        .unwrap();
    let expired = service.submit(request("/project", "expired")).unwrap();
    service
        .expire_provider_leases_at(Instant::now() + PROVIDER_LEASE_DURATION)
        .unwrap();
    assert_eq!(
        service.status("caller", &expired.job_id).unwrap().status,
        ProjectExecutionStatus::Failed
    );

    assert!(matches!(
        service.record_result("provider", &expired.job_id, Ok(Value::Null)),
        Err(ProjectExecutionError::TerminalJob { .. })
    ));
    let (sender, _receiver) = mpsc::sync_channel(4);
    service
        .register_provider("provider-2", "/project", ["run".into()], sender)
        .unwrap();
    let cancelled = service.submit(request("/project", "cancelled")).unwrap();
    service.cancel("caller", &cancelled.job_id).unwrap();
    assert!(matches!(
        service.record_result("provider-2", &cancelled.job_id, Ok(Value::Null)),
        Err(ProjectExecutionError::TerminalJob { .. })
    ));
}

fn request(project_root: &str, key: &str) -> ProjectExecutionRequest {
    ProjectExecutionRequest {
        requester_id: "caller".into(),
        project_root: project_root.into(),
        capability_id: "run".into(),
        idempotency_key: key.into(),
        input: Value::Null,
        priority: ProjectExecutionPriority::default(),
    }
}

fn terminal_job(job_id: &str) -> ProjectExecutionJob {
    ProjectExecutionJob {
        job_id: job_id.into(),
        requester_id: "caller".into(),
        project_root: "/project".into(),
        capability_id: "capability".into(),
        status: ProjectExecutionStatus::Succeeded,
        progress: None,
        output: Some(Value::Null),
        error: None,
    }
}
