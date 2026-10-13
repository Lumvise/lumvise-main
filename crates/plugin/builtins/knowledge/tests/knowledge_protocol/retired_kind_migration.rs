use super::*;

#[test]
fn retired_knowledge_kinds_read_converted_and_leave_the_contract() {
    let workspace = tempfile::tempdir().expect("retired kind workspace");
    let broker = Arc::new(MemoryStorageBroker::default());
    seed_retired_knowledge(&broker);
    let system = start_packaged_knowledge(&workspace, broker.clone());

    let before = read_project_artifacts(&broker);
    assert_eq!(before.len(), 2);
    assert_eq!(before[0]["artifact_id"], "aaa-inactive-note");
    assert_stored_kinds(&broker, "manual_note", "manual_note");
    assert_public_read_maps_retired_kinds(&system);

    assert_succeeds(
        &system,
        ARTIFACT_GENERATION_POLL_EXPORT_ID,
        serde_json::json!({"delivery_id": "retired-kinds", "scheduled_at_unix_seconds": 0}),
    );

    assert_migration_preserved_records(&broker, &before);
    assert_public_read_preserves_lifecycles(&system);
    assert_replacement_contract_still_applies(&system);

    system
        .stop(PLUGIN_ID)
        .expect("stop packaged Knowledge binary");
    system
        .uninstall(PLUGIN_ID)
        .expect("detach Knowledge package");
    make_tree_writable(workspace.path());
}

fn seed_retired_knowledge(broker: &MemoryStorageBroker) {
    seed_inactive_retired_note(broker);
    seed_active_retired_note(broker);
}

fn seed_inactive_retired_note(broker: &MemoryStorageBroker) {
    broker.ensure_element("file:inactive", "/work/demo");
    let mut artifact = test_artifact("aaa-inactive-note", "file:inactive", "Inactive note");
    artifact.artifact_kind = "manual_note".into();
    artifact.content = Some("Inactive body".into());
    artifact.metadata["knowledge"]["lifecycle"] = serde_json::json!({
        "status": "resolved", "revision": 7,
        "created_at": "2026-09-01T00:00:00+00:00",
        "updated_at": "2026-09-02T00:00:00+00:00"
    });
    broker.upsert_artifact(artifact, "text/markdown");
    set_element_inactive(broker, "file:inactive", "/work/demo");
    preserve_inactive_owner(broker, "file:inactive");
    assert_eq!(
        owner_lifecycle(broker, "file:inactive", "/work/demo"),
        "inactive"
    );
}

fn seed_active_retired_note(broker: &MemoryStorageBroker) {
    broker.ensure_element("file:retired", "/work/demo");
    let mut artifact = test_artifact("retired-note", "file:retired", "Retired note");
    artifact.artifact_kind = "manual_note".into();
    artifact.content = Some("Retired body".into());
    artifact.metadata["knowledge"]["lifecycle"] = serde_json::json!({
        "status": "active", "revision": 3,
        "created_at": "2026-09-03T00:00:00+00:00",
        "updated_at": "2026-09-04T00:00:00+00:00"
    });
    broker.upsert_artifact(artifact, "text/markdown");
}

fn set_element_inactive(
    broker: &MemoryStorageBroker,
    semantic_element_id: &str,
    project_root: &str,
) {
    let mut element = test_element(semantic_element_id, project_root);
    element.lifecycle = "inactive".into();
    broker.sync_elements(project_root, vec![element]);
}

fn preserve_inactive_owner(broker: &MemoryStorageBroker, semantic_element_id: &str) {
    broker
        .preserved_inactive_owner_ids
        .lock()
        .expect("preserved inactive owners")
        .insert(semantic_element_id.to_owned());
}

fn owner_lifecycle(
    broker: &MemoryStorageBroker,
    semantic_element_id: &str,
    project_root: &str,
) -> String {
    let result = SemanticPersistence::execute(
        broker.persistence.as_ref(),
        SemanticOperation::ElementsByIdsIncludingInactive {
            project_root: project_root.to_owned(),
            semantic_element_ids: HashSet::from([semantic_element_id.to_owned()]),
        },
        &InvocationControl::sixty_seconds(),
    );
    match result {
        Ok(SemanticResult::Elements(elements)) => elements
            .first()
            .expect("seed owner exists")
            .lifecycle
            .clone(),
        Ok(other) => panic!("expected targeted elements while reading seed owner, got {other:?}"),
        Err(error) => panic!(
            "expected targeted elements operation to succeed while reading seed owner: {error}"
        ),
    }
}

fn read_project_artifacts(broker: &MemoryStorageBroker) -> Vec<serde_json::Value> {
    broker
        .invoke(HostCapabilityRequest {
            plugin_id: PLUGIN_ID.into(),
            invocation_id: "retired-kinds".into(),
            call_id: "retired-kinds-project-read".into(),
            capability_id: "storage.semantic".into(),
            required_version: "^1.4.0".into(),
            input: serde_json::json!({
                "operation": "project_artifacts", "project_root": "/work/demo",
                "artifact_namespace": "knowledge"
            }),
        })
        .expect("project Knowledge artifact read")["artifacts"]
        .as_array()
        .expect("project artifact array")
        .clone()
}

fn read_stored_kind(broker: &MemoryStorageBroker, artifact_id: &str) -> serde_json::Value {
    broker
        .invoke(HostCapabilityRequest {
            plugin_id: PLUGIN_ID.into(),
            invocation_id: "retired-kinds".into(),
            call_id: "retired-kinds-read".into(),
            capability_id: "storage.semantic".into(),
            required_version: "^1.4.0".into(),
            input: serde_json::json!({"operation": "artifact", "artifact_id": artifact_id}),
        })
        .expect("raw artifact read")["artifact"]["artifact_kind"]
        .clone()
}

fn assert_stored_kinds(broker: &MemoryStorageBroker, inactive: &str, active: &str) {
    assert_eq!(read_stored_kind(broker, "aaa-inactive-note"), inactive);
    assert_eq!(read_stored_kind(broker, "retired-note"), active);
}

fn assert_public_read_maps_retired_kinds(system: &PluginSystem) {
    for artifact_id in ["aaa-inactive-note", "retired-note"] {
        let result = invoke_value(
            system,
            GET_EXPORT_ID,
            serde_json::json!({"artifact_id": artifact_id}),
        );
        assert_eq!(result["artifact"]["knowledge_type"], "annotation");
    }
}

fn assert_migration_preserved_records(broker: &MemoryStorageBroker, before: &[serde_json::Value]) {
    assert_stored_kinds(broker, "manual_note", "annotation");
    let after = read_project_artifacts(broker);
    assert_eq!(after.len(), before.len());
    for artifact_id in ["aaa-inactive-note", "retired-note"] {
        let old = artifact_snapshot(before, artifact_id);
        let new = artifact_snapshot(&after, artifact_id);
        assert_eq!(old["content"], new["content"]);
        assert_eq!(
            old["metadata"]["knowledge"]["lifecycle"],
            new["metadata"]["knowledge"]["lifecycle"]
        );
    }
}

fn artifact_snapshot<'a>(
    artifacts: &'a [serde_json::Value],
    artifact_id: &str,
) -> &'a serde_json::Value {
    artifacts
        .iter()
        .find(|artifact| artifact["artifact_id"] == artifact_id)
        .expect("artifact snapshot")
}

fn assert_public_read_preserves_lifecycles(system: &PluginSystem) {
    assert_read_lifecycle(
        system,
        "retired-note",
        "active",
        3,
        "2026-09-03T00:00:00+00:00",
        "2026-09-04T00:00:00+00:00",
    );
    assert_read_lifecycle(
        system,
        "aaa-inactive-note",
        "resolved",
        7,
        "2026-09-01T00:00:00+00:00",
        "2026-09-02T00:00:00+00:00",
    );
}

fn assert_read_lifecycle(
    system: &PluginSystem,
    artifact_id: &str,
    status: &str,
    revision: u64,
    created_at: &str,
    updated_at: &str,
) {
    let result = invoke_value(
        system,
        GET_EXPORT_ID,
        serde_json::json!({"artifact_id": artifact_id}),
    );
    assert_eq!(result["artifact"]["knowledge_type"], "annotation");
    assert_eq!(result["artifact"]["status"], status);
    assert_eq!(result["artifact"]["revision"], revision);
    assert_eq!(result["artifact"]["created_at"], created_at);
    assert_eq!(result["artifact"]["updated_at"], updated_at);
}

fn assert_replacement_contract_still_applies(system: &PluginSystem) {
    let listed = invoke_value(
        system,
        LIST_EXPORT_ID,
        serde_json::json!({"semantic_element_id": "file:retired"}),
    );
    assert_eq!(listed["artifacts"][0]["knowledge_type"], "annotation");

    let retired_create = system
        .invoke(
            PLUGIN_ID,
            CREATE_EXPORT_ID,
            serde_json::json!({
                "artifact_id": "new-note", "semantic_element_id": "file:retired",
                "knowledge_type": "manual_note", "title": "New note", "content": "Body"
            }),
        )
        .expect_err("signed schema rejects a retired Knowledge kind");
    assert!(retired_create.to_string().contains("manual_note"));

    let plan = invoke_value(
        system,
        CREATE_EXPORT_ID,
        serde_json::json!({
            "artifact_id": "new-plan", "semantic_element_id": "file:retired",
            "knowledge_type": "plan", "title": "Plan", "content": "Steps"
        }),
    );
    assert_eq!(plan["artifact"]["knowledge_type"], "plan");
}
