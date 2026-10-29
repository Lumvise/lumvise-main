use super::*;

#[test]
fn retired_records_are_rejected_without_rewriting_their_content() {
    let workspace = tempfile::tempdir().unwrap();
    let broker = Arc::new(MemoryStorageBroker::default());
    broker.ensure_element("file:retired", "/work/demo");
    let mut original = test_artifact("retired-note", "file:retired", "Keep this body");
    original.artifact_kind = "manual_note".into();
    broker.upsert_artifact(original.clone(), "text/markdown");
    let system = start_packaged_knowledge(&workspace, broker.clone());
    let outcome = system
        .invoke(
            PLUGIN_ID,
            GET_EXPORT_ID,
            serde_json::json!({"artifact_id":"retired-note"}),
        )
        .unwrap();
    assert!(
        matches!(outcome, WireOutcome::Failed { error } if error.code == "unsupported_knowledge_format")
    );
    let stored = SemanticPersistence::execute(
        broker.persistence.as_ref(),
        SemanticOperation::Artifact {
            artifact_id: "retired-note".into(),
        },
        &InvocationControl::sixty_seconds(),
    )
    .unwrap();
    let SemanticResult::Artifact(Some(stored)) = stored else {
        panic!("expected unchanged artifact")
    };
    assert_eq!(stored, original);
    assert!(
        system
            .invoke(
                PLUGIN_ID,
                "convert_legacy_documents",
                serde_json::json!({"project_root":"/work/demo"})
            )
            .is_err()
    );
    system.stop(PLUGIN_ID).unwrap();
    system.uninstall(PLUGIN_ID).unwrap();
    make_tree_writable(workspace.path());
}

#[test]
fn current_media_annotations_keep_their_exact_region_selector() {
    let workspace = tempfile::tempdir().unwrap();
    let broker = Arc::new(MemoryStorageBroker::default());
    broker.ensure_element("file:image", "/work/demo");
    let system = start_packaged_knowledge(&workspace, broker);
    let selector =
        serde_json::json!({"kind":"image_region","x":0.3,"y":0.3,"width":0.4,"height":0.4});
    let created = invoke_value(
        &system,
        CREATE_EXPORT_ID,
        serde_json::json!({"artifact_id":"image-region-note",
        "semantic_element_id":"file:image","project_root":"/work/demo","knowledge_type":"media",
        "title":"Island region","content":"Region body","metadata":{"anchor_selector":selector}}),
    );
    let read = invoke_value(
        &system,
        GET_EXPORT_ID,
        serde_json::json!({"artifact_id":created["artifact"]["artifact_id"]}),
    );
    assert_eq!(read, created);
    assert_eq!(read["artifact"]["metadata"]["anchor_selector"], selector);
    system.stop(PLUGIN_ID).unwrap();
    system.uninstall(PLUGIN_ID).unwrap();
    make_tree_writable(workspace.path());
}

#[test]
fn signed_current_artifact_schema_accepts_exactly_the_three_product_kinds() {
    let workspace = tempfile::tempdir().unwrap();
    let broker = Arc::new(MemoryStorageBroker::default());
    broker.ensure_element("file:types", "/work/demo");
    let system = start_packaged_knowledge(&workspace, broker);
    for kind in ["requirements", "specification", "media"] {
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
