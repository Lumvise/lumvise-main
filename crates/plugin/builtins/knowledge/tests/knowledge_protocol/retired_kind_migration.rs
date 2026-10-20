use super::*;

#[test]
fn retired_kinds_require_explicit_conversion_and_polling_preserves_them() {
    let workspace = tempfile::tempdir().unwrap();
    let broker = Arc::new(MemoryStorageBroker::default());
    seed_retired_knowledge(&broker);
    let system = start_packaged_knowledge(&workspace, broker.clone());
    let semantic = signed_semantic_fixture(workspace.path(), &SigningKey::from_bytes(&[48; 32]));
    system.install(&semantic).unwrap();
    system.start("builtin.semantic").unwrap();
    let before = read_project_artifacts(&broker);
    for artifact_id in ["aaa-inactive-note", "retired-note"] {
        let outcome = system
            .invoke(
                PLUGIN_ID,
                GET_EXPORT_ID,
                serde_json::json!({"artifact_id":artifact_id}),
            )
            .unwrap();
        let WireOutcome::Failed { error } = outcome else {
            panic!("retired record requires conversion")
        };
        assert_eq!(error.code, "knowledge_conversion_required");
    }
    assert_succeeds(
        &system,
        ARTIFACT_GENERATION_POLL_EXPORT_ID,
        serde_json::json!({"delivery_id":"retired-kinds","scheduled_at_unix_seconds":0}),
    );
    assert_eq!(read_project_artifacts(&broker), before);
    let preview = invoke_value(
        &system,
        "convert_legacy_documents",
        serde_json::json!({"project_root":"/work/demo"}),
    );
    assert_eq!(preview["preview"], true);
    assert_eq!(preview["total"], 2);
    assert_eq!(read_project_artifacts(&broker), before);
    invoke_value(
        &system,
        "convert_legacy_documents",
        serde_json::json!({"project_root":"/work/demo","preview":false}),
    );
    for (id, status, revision, body) in [
        ("retired-note", "active", 3, "Retired body"),
        ("aaa-inactive-note", "resolved", 7, "Inactive body"),
    ] {
        let document = invoke_value(
            &system,
            "get_document",
            serde_json::json!({"project_root":"/work/demo","document_id":id}),
        );
        assert_eq!(document["document"]["content"], body);
        assert_eq!(document["document"]["document_kind"], "context");
        assert_eq!(document["document"]["status"], status);
        assert_eq!(document["document"]["revision"], revision);
    }
    assert_eq!(
        invoke_value(
            &system,
            "list_knowledge",
            serde_json::json!({"project_root":"/work/demo"})
        )["artifacts"],
        serde_json::json!([])
    );
    system.stop(PLUGIN_ID).unwrap();
    system.uninstall(PLUGIN_ID).unwrap();
    system.stop("builtin.semantic").unwrap();
    system.uninstall("builtin.semantic").unwrap();
    make_tree_writable(workspace.path());
}

#[test]
fn signed_current_artifact_schema_accepts_exactly_the_two_product_kinds() {
    let workspace = tempfile::tempdir().unwrap();
    let broker = Arc::new(MemoryStorageBroker::default());
    broker.ensure_element("file:types", "/work/demo");
    let system = start_packaged_knowledge(&workspace, broker);
    for kind in ["requirements", "specification"] {
        let artifact = invoke_value(
            &system,
            CREATE_EXPORT_ID,
            serde_json::json!({"artifact_id":kind,
            "semantic_element_id":"file:types","knowledge_type":kind,"title":kind,"content":"Body","project_root":"/work/demo"}),
        );
        assert_eq!(artifact["artifact"]["knowledge_type"], kind);
    }
    for kind in [
        "issue",
        "task_assignment",
        "definition",
        "annotation",
        "report",
        "decision",
        "checkpoint",
        "question",
        "plan",
        "manual_note",
        "derived_summary",
    ] {
        let error = system.invoke(PLUGIN_ID,CREATE_EXPORT_ID,serde_json::json!({"artifact_id":kind,
            "semantic_element_id":"file:types","knowledge_type":kind,"title":kind,"content":"Body"})).unwrap_err();
        assert!(matches!(
            error,
            lumvise_plugin_runtime::PluginRuntimeError::SchemaValidation { .. }
        ));
    }
    system.stop(PLUGIN_ID).unwrap();
    system.uninstall(PLUGIN_ID).unwrap();
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
