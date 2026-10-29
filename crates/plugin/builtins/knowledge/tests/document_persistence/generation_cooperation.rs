use super::*;

fn second_instance(first: &DocumentBoundaryHost) -> DocumentBoundaryHost {
    let mut second =
        DocumentBoundaryHost::with_persistence_client(first.persistence.clone(), "client-b");
    second.generation = Some(FakeFunctionalJobs {
        client_instance_id: "client-b".into(),
        ..Default::default()
    });
    second
}

#[test]
fn a_different_instance_never_polls_or_resubmits_another_hosts_job() {
    let mut first = seeded();
    assert_eq!(ensure(&mut first, false)["status"], "pending");
    let mut second = second_instance(&first);
    let status = poll(&mut second);
    assert_eq!(status, json!({"pending":0, "completed":0}));
    let execution = second.generation.as_ref().unwrap();
    assert_eq!(execution.status_calls, 0);
    assert!(execution.submissions.is_empty());
    let finished = poll(&mut first);
    assert_eq!(
        finished["completed"],
        1,
        "{finished}; {}",
        failure_rows(&mut first)
    );
}

#[test]
fn a_completed_job_for_an_unselected_source_cannot_publish_old_context() {
    let mut host = seeded();
    ensure(&mut host, false);
    sources(&host, "fp1:0000000000000002:new-source");
    assert_eq!(poll(&mut host)["completed"], 0);
    assert_eq!(
        host.knowledge(
            "list_documents",
            json!({"project_root":ROOT, "document_kind":"context"})
        )
        .unwrap()["total"],
        0
    );
    assert_eq!(ensure(&mut host, false)["status"], "pending");
    assert_eq!(poll(&mut host)["completed"], 1);
    let generated = context(&mut host);
    assert!(
        generated["content"]
            .as_str()
            .unwrap()
            .contains("fp1:0000000000000002:new-source")
    );
    assert_eq!(
        host.knowledge(
            "list_documents",
            json!({"project_root":ROOT, "document_kind":"context"})
        )
        .unwrap()["total"],
        1
    );
}

#[test]
fn independently_generated_answers_keep_the_original_observed_absence() {
    let mut first = seeded();
    assert_eq!(ensure(&mut first, false)["status"], "pending");
    let mut second = second_instance(&first);
    assert_eq!(ensure(&mut second, false)["status"], "pending");
    assert_eq!(second.generation.as_ref().unwrap().submissions.len(), 1);
    let finished = poll(&mut second);
    assert_eq!(
        finished["completed"],
        1,
        "{finished}; {}",
        failure_rows(&mut second)
    );
    let accepted = context(&mut second);
    assert_eq!(poll(&mut first)["completed"], 0);
    assert_eq!(
        context(&mut first),
        accepted,
        "a later completion must not adopt the other instance's accepted base"
    );
}

fn failure_rows(host: &mut DocumentBoundaryHost) -> Value {
    host.host_call(
        "storage.plugin",
        json!({"operation":"list_rows",
        "table_name":"knowledge_functional_artifact_failures", "limit":100}),
    )
    .unwrap()
}

#[test]
fn lost_submit_acknowledgement_replays_the_original_request_and_document_base() {
    let mut first = seeded();
    first.generation.as_mut().unwrap().lose_submit_reply = true;
    ensure(&mut first, false);
    let mut second = second_instance(&first);
    ensure(&mut second, false);
    assert_eq!(poll(&mut second)["completed"], 1);
    let accepted = context(&mut second);
    assert_eq!(poll(&mut first)["completed"], 0);
    assert_eq!(poll(&mut first)["completed"], 0);
    let execution = first.generation.as_ref().unwrap();
    assert_eq!(execution.submissions.len(), 2);
    assert_eq!(execution.submissions[0], execution.submissions[1]);
    assert_eq!(execution.results.len(), 1);
    assert_eq!(context(&mut first), accepted);
}

#[test]
fn restart_resubmission_keeps_the_base_instead_of_adopting_another_instances_answer() {
    let mut first = seeded();
    ensure(&mut first, false);
    let request = first.generation.as_ref().unwrap().submissions[0].clone();
    let mut second = second_instance(&first);
    ensure(&mut second, false);
    assert_eq!(poll(&mut second)["completed"], 1);
    let accepted = context(&mut second);
    first.generation = Some(FakeFunctionalJobs::default());
    assert_eq!(poll(&mut first)["completed"], 0);
    assert_eq!(
        first.generation.as_ref().unwrap().submissions,
        vec![request]
    );
    assert_eq!(poll(&mut first)["completed"], 0);
    assert_eq!(context(&mut first), accepted);
}

#[test]
fn lost_publication_acknowledgement_reuses_the_frozen_proposal_after_a_later_edit() {
    let mut host = seeded();
    ensure(&mut host, false);
    host.drop_document_reply = true;
    let failed = host
        .knowledge(
            lumvise_plugin_knowledge::ARTIFACT_GENERATION_POLL_EXPORT_ID,
            json!({}),
        )
        .expect_err("accepted publication loses its reply");
    assert_eq!(failed.code, "lost_document_reply");
    let accepted = context(&mut host);
    let authored = edit(&mut host, &accepted, "A later human revision.");
    assert_eq!(poll(&mut host)["completed"], 1);
    assert_eq!(context(&mut host), authored);
    assert_eq!(host.generation.as_ref().unwrap().submissions.len(), 1);
}

struct ReplacingPendingJobHost {
    inner: DocumentBoundaryHost,
    replaced: bool,
}

impl HostCallTransport for ReplacingPendingJobHost {
    fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
        let finishing = input["mutations"].as_array().is_some_and(|mutations| {
            mutations.iter().any(|row| {
                row["operation"] == "delete"
                    && row["table_name"] == "knowledge_functional_artifact_jobs"
            })
        });
        if capability == "storage.plugin" && finishing && !self.replaced {
            let guard = &input["mutations"][0];
            let mut replacement = guard["expected"]["value"].clone();
            replacement["job_id"] = json!("replacement-job");
            self.inner.host_call(
                capability,
                json!({"operation":"put_row",
                "table_name":"knowledge_functional_artifact_jobs", "row_key":guard["row_key"],
                "value":replacement}),
            )?;
            self.replaced = true;
        }
        self.inner.host_call(capability, input)
    }
}

#[test]
fn a_stale_consumer_cannot_clear_replacement_pending_work() {
    let mut inner = seeded();
    ensure(&mut inner, false);
    let mut host = ReplacingPendingJobHost {
        inner,
        replaced: false,
    };
    let failed = KnowledgePlugin::default()
        .dispatch(
            lumvise_plugin_knowledge::ARTIFACT_GENERATION_POLL_EXPORT_ID,
            json!({}),
            &mut PluginContext::for_test(&mut host),
        )
        .expect_err("replacement invalidates the stale row guard");
    assert!(
        failed
            .message
            .contains("expected the required pre-batch row value"),
        "{failed:?}"
    );
    let saved = host
        .inner
        .host_call(
            "storage.plugin",
            json!({"operation":"list_rows",
        "table_name":"knowledge_functional_artifact_jobs", "limit":100}),
        )
        .unwrap();
    assert_eq!(saved["rows"][0]["value"]["job_id"], "replacement-job");
    assert!(host.replaced);
}

#[test]
fn generation_refuses_an_empty_client_scope_before_submitting_work() {
    let mut host = seeded();
    host.generation.as_mut().unwrap().client_instance_id.clear();
    let failed = host
        .knowledge(
            lumvise_plugin_knowledge::ARTIFACT_GENERATION_POLL_EXPORT_ID,
            json!({}),
        )
        .expect_err("an empty client cannot own durable work");
    assert_eq!(failed.code, "invalid_project_execution_scope");
    assert!(failed.message.contains("client_instance_id"));
    assert!(host.generation.as_ref().unwrap().submissions.is_empty());
}

#[test]
fn report_refresh_failure_retains_a_frozen_completed_proposal_for_retry() {
    let mut host = seeded();
    ensure(&mut host, false);
    host.fail_document_write = Some(1);
    host.knowledge(
        lumvise_plugin_knowledge::ARTIFACT_GENERATION_POLL_EXPORT_ID,
        json!({}),
    )
    .expect_err("context publication succeeds before report publication fails");
    let accepted = context(&mut host);
    let pending = host
        .host_call(
            "storage.plugin",
            json!({"operation":"list_rows",
        "table_name":"knowledge_functional_artifact_jobs", "limit":100}),
        )
        .unwrap();
    assert_eq!(pending["rows"].as_array().unwrap().len(), 1);
    assert!(pending["rows"][0]["value"]["proposal"].is_object());
    assert_eq!(poll(&mut host)["completed"], 1);
    assert_eq!(context(&mut host), accepted);
    assert_eq!(host.generation.as_ref().unwrap().submissions.len(), 1);
}

#[test]
fn pending_generation_is_fenced_to_its_original_project_lineage() {
    let mut host = seeded();
    ensure(&mut host, false);
    host.reported_project_identity = Some("a-different-project".into());
    let failed = host
        .knowledge(
            lumvise_plugin_knowledge::ARTIFACT_GENERATION_POLL_EXPORT_ID,
            json!({}),
        )
        .expect_err("a reused project route cannot receive old generation output");
    assert_eq!(failed.code, "knowledge_project_mismatch");
    host.reported_project_identity = None;
    assert_eq!(poll(&mut host)["completed"], 1);
}

#[test]
fn a_deleted_generated_context_is_held_before_another_execution_request() {
    let mut host = seeded();
    ensure(&mut host, false);
    assert_eq!(poll(&mut host)["completed"], 1);
    let document = context(&mut host);
    host.knowledge(
        "delete_document",
        json!({"project_root":ROOT,
        "document_id":document["document_id"], "expected_revision":document["revision"],
        "expected_semantic_element_id":document["semantic_element_id"]}),
    )
    .unwrap();
    let held = ensure(&mut host, true);
    assert_eq!(held["status"], "not_ready");
    assert_eq!(held["reason_code"], "generated_document_deleted");
    assert_eq!(host.generation.as_ref().unwrap().submissions.len(), 1);
}
