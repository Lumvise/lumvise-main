use super::*;
use lumvise_plugin_sdk::{HostCallTransport, PluginContext};

struct SubmitFakeHost {
    rows: std::collections::BTreeMap<(String, String), Value>,
    failing_elements: Vec<String>,
    job_counter: usize,
    /// Submissions beyond this count answer `host_capability_quota_exceeded`.
    quota_after: Option<usize>,
    /// Elements served by `storage.semantic` targeted reads.
    elements: Vec<SemanticElement>,
    /// Every accepted `runtime.project_execution` submit request.
    submit_requests: Vec<Value>,
}

impl SubmitFakeHost {
    fn rows_in(&self, table: &str) -> Vec<&Value> {
        self.rows
            .iter()
            .filter(|((stored_table, _), _)| stored_table == table)
            .map(|(_, value)| value)
            .collect()
    }
}

impl HostCallTransport for SubmitFakeHost {
    fn host_call(&mut self, capability_id: &str, input: Value) -> Result<Value, PluginError> {
        let operation = input["operation"].as_str().unwrap_or_default().to_owned();
        match (capability_id, operation.as_str()) {
            ("storage.plugin", "ensure_table") => Ok(json!({})),
            ("storage.plugin", "put_row") => {
                self.rows.insert(
                    (
                        input["table_name"].as_str().unwrap_or_default().to_string(),
                        input["row_key"].as_str().unwrap_or_default().to_string(),
                    ),
                    input["value"].clone(),
                );
                Ok(json!({}))
            }
            ("storage.plugin", "get_row") => Ok(json!({"row": self
                .rows
                .get(&(
                    input["table_name"].as_str().unwrap_or_default().to_string(),
                    input["row_key"].as_str().unwrap_or_default().to_string(),
                ))
                .cloned()
                .unwrap_or(Value::Null)})),
            ("storage.plugin", "list_rows") => {
                let table = input["table_name"].as_str().unwrap_or_default();
                let prefix = input.get("key_prefix").and_then(Value::as_str);
                let rows: Vec<Value> = self
                    .rows
                    .iter()
                    .filter(|((stored_table, key), _)| {
                        stored_table == table
                            && prefix
                                .as_deref()
                                .map_or(true, |prefix| key.starts_with(prefix))
                    })
                    .map(|(_, value)| json!({"value": value}))
                    .collect();
                Ok(json!({"rows": rows}))
            }
            ("storage.plugin", "mutate_rows") => {
                for mutation in input["mutations"].as_array().unwrap_or(&Vec::new()) {
                    if mutation["operation"].as_str() == Some("delete") {
                        self.rows.remove(&(
                            mutation["table_name"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string(),
                            mutation["row_key"].as_str().unwrap_or_default().to_string(),
                        ));
                    }
                }
                Ok(json!({}))
            }
            ("runtime.project_execution", "status") => Err(PluginError::new(
                "host_capability_unavailable",
                "runtime.project_execution unavailable: job was not found",
                true,
            )),
            ("storage.semantic", "elements_by_ids_including_inactive") => {
                Ok(json!({"elements": self.elements}))
            }
            ("runtime.project_execution", "submit")
                if self
                    .quota_after
                    .is_some_and(|limit| self.job_counter >= limit) =>
            {
                Err(PluginError::new(
                    QUOTA_EXCEEDED,
                    "runtime.project_execution quota exceeded: actual 257; maximum 256",
                    false,
                ))
            }
            ("runtime.project_execution", "submit") => {
                let target = &input["input"]["semantic_element_id"];
                let element_id = target.as_str().unwrap_or_default().to_string();
                if self.failing_elements.contains(&element_id) {
                    return Err(PluginError::new(
                        "host_capability_execution_failed",
                        format!(
                            "runtime.project_execution operation failed: invalid project execution request `failed to read semantic element path `src/encoding.rs`: stream did not contain valid UTF-8`; expected valid local semantic artifact task"
                        ),
                        false,
                    ));
                }
                self.job_counter += 1;
                self.submit_requests.push(input.clone());
                Ok(json!({"job_id": format!("job-{}", self.job_counter)}))
            }
            other => Err(PluginError::unknown_capability(&format!("{}", other.0))),
        }
    }
}

fn generation_element(id: &str, path: &str) -> SemanticElement {
    SemanticElement {
        project_root: "/project".into(),
        semantic_element_id: id.into(),
        semantic_source_id: "src-1".into(),
        path: path.into(),
        element_kind: "function".into(),
        name: id.into(),
        parent_element_id: None,
        content_fingerprint: Some("fp-1".into()),
        start_line: Some(1),
        end_line: Some(10),
        lifecycle: "active".into(),
        metadata: json!({}),
    }
}

#[test]
fn a_submit_time_rejection_records_a_failure_and_does_not_poison_the_batch() {
    let broken_element = generation_element("fn:corrupted", "src/encoding.rs");
    let text_element = generation_element("fn:text", "src/lib.rs");
    let mut host = SubmitFakeHost {
        rows: Default::default(),
        failing_elements: vec!["fn:corrupted".into()],
        job_counter: 0,
        quota_after: None,
        elements: Vec::new(),
        submit_requests: Vec::new(),
    };

    // The element whose source fails the UTF-8 read comes first: the old
    // abort-on-error loop never reached the text element and dead-lettered
    // every delivery.
    let submission = {
        let mut context = PluginContext::for_test(&mut host);
        submit_missing(
            &[broken_element, text_element],
            &[],
            false,
            SubmitPriority::Background,
            &mut context,
        )
        .expect("the batch survives one invalid target")
    };
    assert_eq!(submission.submitted, 1);
    assert!(!submission.provider_unavailable);

    let failures: Vec<ArtifactGenerationFailure> = {
        let mut context = PluginContext::for_test(&mut host);
        storage::list(&mut context, FAILURES, None).expect("failures listed")
    };
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].semantic_element_id, "fn:corrupted");
    assert_eq!(
        failures[0].reason_code,
        "submit_host_capability_execution_failed"
    );
    assert!(failures[0].reason.contains("src/encoding.rs"));

    // Later polls skip the failed element instead of failing again, and
    // the already-submitted element stays pending rather than resubmitting.
    let resubmission = {
        let mut context = PluginContext::for_test(&mut host);
        submit_missing(
            &[
                generation_element("fn:corrupted", "src/encoding.rs"),
                generation_element("fn:text", "src/lib.rs"),
            ],
            &[],
            false,
            SubmitPriority::Background,
            &mut context,
        )
        .expect("second submission succeeds")
    };
    assert_eq!(resubmission.submitted, 0);
}

#[test]
fn a_full_execution_queue_throttles_without_recording_failures() {
    let mut host = SubmitFakeHost {
        rows: Default::default(),
        failing_elements: Vec::new(),
        job_counter: 0,
        quota_after: Some(1),
        elements: Vec::new(),
        submit_requests: Vec::new(),
    };
    let elements = [
        generation_element("fn:a", "src/a.rs"),
        generation_element("fn:b", "src/b.rs"),
        generation_element("fn:c", "src/c.rs"),
    ];
    let submission = {
        let mut context = PluginContext::for_test(&mut host);
        submit_missing(
            &elements,
            &[],
            false,
            SubmitPriority::Background,
            &mut context,
        )
        .expect("throttled batch")
    };
    assert_eq!(submission.submitted, 1);
    assert!(host.rows_in(FAILURES).is_empty());

    // Capacity frees up: the remaining elements submit on the next round.
    host.quota_after = None;
    let submission = {
        let mut context = PluginContext::for_test(&mut host);
        submit_missing(
            &elements,
            &[],
            false,
            SubmitPriority::Background,
            &mut context,
        )
        .expect("resumed batch")
    };
    assert_eq!(submission.submitted, 2);
}

#[test]
fn a_rejected_resubmission_drops_the_stale_job() {
    let element = generation_element("fn:corrupted", "src/encoding.rs");
    let mut host = SubmitFakeHost {
        rows: Default::default(),
        failing_elements: vec!["fn:corrupted".into()],
        job_counter: 0,
        quota_after: None,
        elements: vec![element.clone()],
        submit_requests: Vec::new(),
    };
    let stale = PendingArtifactJob {
        job_id: "job-lost-on-restart".into(),
        project_root: element.project_root.clone(),
        semantic_element_id: element.semantic_element_id.clone(),
        content_fingerprint: "fp-1".into(),
        priority: SubmitPriority::Background,
        lost_resubmits: 0,
    };
    {
        let mut context = PluginContext::for_test(&mut host);
        storage::put(&mut context, JOBS, &pending_key(&stale), &stale).expect("job stored");
        poll(&mut context, Some("/project")).expect("poll survives the rejection");
    }
    assert!(
        host.rows_in(JOBS).is_empty(),
        "stale job must not be retried forever"
    );
    assert_eq!(host.rows_in(FAILURES).len(), 1);
}

#[test]
fn binary_sources_are_never_generation_targets() {
    assert_eq!(
        target_input(&generation_element(
            "file:icon",
            "crates/frontend-core/icons/icon.ico"
        )),
        None
    );
    assert!(!needs_generation(
        &generation_element("file:icon", "crates/frontend-core/icons/icon.ico"),
        &[]
    ));
    // Uppercase extensions and text extensions are handled symmetrically.
    assert!(binary_source("assets/LOGO.PNG"));
    assert!(!binary_source("src/lib.rs"));
}

#[test]
fn terminal_project_execution_can_be_retried_by_a_later_c4_request() {
    assert_eq!(
        job_disposition("failed").expect("known status"),
        JobDisposition::TerminalFailure
    );
    assert_eq!(
        job_disposition("cancelled").expect("known status"),
        JobDisposition::TerminalFailure
    );
}

#[test]
fn active_project_execution_remains_pending() {
    assert_eq!(
        job_disposition("queued").expect("known status"),
        JobDisposition::Pending
    );
    assert_eq!(
        job_disposition("running").expect("known status"),
        JobDisposition::Pending
    );
}

#[test]
fn user_requests_submit_interactive_and_refresh_submits_background() {
    let mut host = SubmitFakeHost {
        rows: Default::default(),
        failing_elements: Vec::new(),
        job_counter: 0,
        quota_after: None,
        elements: Vec::new(),
        submit_requests: Vec::new(),
    };
    {
        let mut context = PluginContext::for_test(&mut host);
        submit_missing(
            &[generation_element("fn:a", "src/a.rs")],
            &[],
            false,
            SubmitPriority::Interactive,
            &mut context,
        )
        .expect("interactive submission");
    };
    assert_eq!(host.submit_requests.len(), 1);
    assert_eq!(host.submit_requests[0]["priority"], json!("interactive"));
    assert_eq!(
        host.submit_requests[0]["input"]["artifact_id"],
        json!("knowledge-cultivation-functional-fn-a")
    );

    {
        let mut context = PluginContext::for_test(&mut host);
        submit_missing(
            &[generation_element("fn:b", "src/b.rs")],
            &[],
            false,
            SubmitPriority::Background,
            &mut context,
        )
        .expect("background submission");
    };
    assert_eq!(host.submit_requests.len(), 2);
    assert_eq!(host.submit_requests[1]["priority"], json!("background"));
}

#[test]
fn resubmit_keeps_the_stored_priority() {
    let element = generation_element("fn:a", "src/a.rs");
    let mut host = SubmitFakeHost {
        rows: Default::default(),
        failing_elements: Vec::new(),
        job_counter: 0,
        quota_after: None,
        elements: vec![element.clone()],
        submit_requests: Vec::new(),
    };
    let lost = PendingArtifactJob {
        job_id: "job-lost".into(),
        project_root: element.project_root.clone(),
        semantic_element_id: element.semantic_element_id.clone(),
        content_fingerprint: "fp-1".into(),
        priority: SubmitPriority::Interactive,
        lost_resubmits: 0,
    };
    {
        let mut context = PluginContext::for_test(&mut host);
        storage::put(&mut context, JOBS, &pending_key(&lost), &lost).expect("job stored");
        poll(&mut context, Some("/project")).expect("poll resubmits the lost job");
    }
    assert_eq!(host.submit_requests.len(), 1);
    assert_eq!(host.submit_requests[0]["priority"], json!("interactive"));
}

#[test]
fn a_repeatedly_lost_job_dead_letters_instead_of_resubmitting_forever() {
    let element = generation_element("fn:a", "src/a.rs");
    let mut host = SubmitFakeHost {
        rows: Default::default(),
        failing_elements: Vec::new(),
        job_counter: 0,
        quota_after: None,
        elements: vec![element.clone()],
        submit_requests: Vec::new(),
    };
    let lost = PendingArtifactJob {
        job_id: "job-lost".into(),
        project_root: element.project_root.clone(),
        semantic_element_id: element.semantic_element_id.clone(),
        content_fingerprint: "fp-1".into(),
        priority: SubmitPriority::Background,
        lost_resubmits: 0,
    };
    {
        let mut context = PluginContext::for_test(&mut host);
        storage::put(&mut context, JOBS, &pending_key(&lost), &lost).expect("job stored");
        // Each of the first three losses resubmits; the fourth loss is the cap.
        for _ in 1..=3 {
            poll(&mut context, Some("/project")).expect("poll resubmits");
        }
        poll(&mut context, Some("/project")).expect("poll survives the cap");
    }
    assert_eq!(
        host.job_counter, 3,
        "the capped element must not resubmit again"
    );
    assert!(
        host.rows_in(JOBS).is_empty(),
        "dead-lettered job row deleted"
    );
    let failures = host.rows_in(FAILURES);
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0]["reason_code"], json!("generation_lost"));
    assert_eq!(
        failures[0]["reason"],
        json!("artifact generation job was lost 4 times; retry to resubmit")
    );
}

#[test]
fn old_stored_jobs_without_new_fields_deserialize_with_defaults() {
    let legacy: PendingArtifactJob = serde_json::from_str(
        r#"{"job_id":"job-9","project_root":"/project",
            "semantic_element_id":"fn:a","content_fingerprint":"fp-1"}"#,
    )
    .expect("legacy job row still loads");
    assert_eq!(legacy.priority, SubmitPriority::Background);
    assert_eq!(legacy.lost_resubmits, 0);
}
