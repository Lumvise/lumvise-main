use super::*;

#[test]
fn knowledge_transfer_requires_explicit_selection_and_is_idempotent() {
    const TARGET: &str = "knowledge-inheritance-host";
    let workspace = tempfile::tempdir().expect("inheritance workspace");
    let executable = std::fs::read(env!("CARGO_BIN_EXE_lumvise-plugin-knowledge"))
        .expect("read real Knowledge binary");
    let executable_hash = hex::encode(Sha256::digest(&executable));
    let manifest = package_manifest_source(TARGET, &executable_hash);
    let executable_path = manifest.targets[TARGET].clone();
    let signing_key = SigningKey::from_bytes(&[41; 32]);
    let archive_path = workspace.path().join("builtin.knowledge.lvp");
    build_package(
        &archive_path,
        BuildPackageRequest {
            manifest,
            files: BTreeMap::from([(executable_path, executable)]),
            signing_key: &signing_key,
        },
    )
    .expect("build real Knowledge package");
    let verified = verify_package(
        &archive_path,
        &signing_key.verifying_key(),
        &HostCompatibility::new(PACKAGE_PROTOCOL_VERSION, TARGET),
    )
    .expect("verify real Knowledge package");
    let installed = install_verified_package(&verified, &workspace.path().join("installed"))
        .expect("install real Knowledge package");
    let broker = Arc::new(MemoryStorageBroker::default());

    let mut source_element = test_element("shared-fn", "/source-project");
    source_element.content_fingerprint = Some("fp1:0000000000000001:shared-body".into());
    broker.sync_elements("/source-project", vec![source_element]);
    let mut target_element = test_element("shared-fn-twin", "/target-project");
    target_element.content_fingerprint = Some("fp1:0000000000000001:shared-body".into());
    broker.sync_elements("/target-project", vec![target_element]);

    let system = PluginSystem::with_broker_and_sandbox(
        fast_runtime_config(),
        broker.clone(),
        Arc::new(TestSubprocessSandbox),
    );
    system
        .install(&installed)
        .expect("catalog Knowledge package");
    system
        .start(PLUGIN_ID)
        .expect("start packaged Knowledge binary");

    for (artifact_id, title) in [
        ("source-artifact-a", "Shared A"),
        ("source-artifact-b", "Shared B"),
    ] {
        let metadata = if artifact_id == "source-artifact-a" {
            serde_json::json!({"files": {"image": {
                "contentRef": "canvas-file:source-artifact-a:image"
            }}})
        } else {
            serde_json::json!({})
        };
        assert_succeeds(
            &system,
            CREATE_EXPORT_ID,
            serde_json::json!({
                "artifact_id": artifact_id, "semantic_element_id": "shared-fn",
                "knowledge_type": "specification", "title": title,
                "content": format!("{title} knowledge"), "project_root": "/source-project",
                "metadata": metadata
            }),
        );
    }
    let blob = SemanticPersistence::execute(
        broker.persistence.as_ref(),
        SemanticOperation::ArtifactBlobPut {
            content_ref: "canvas-file:source-artifact-a:image".into(),
            artifact_id: "source-artifact-a".into(),
            media_type: "image/png".into(),
            content: vec![7, 8, 9],
        },
        &InvocationControl::sixty_seconds(),
    )
    .expect("seed source artifact image blob");
    assert!(matches!(blob, SemanticResult::ArtifactBlob(Some(_))));
    let trigger = serde_json::json!({
        "project_root": "/target-project", "base_revision": 0, "target_revision": 1,
        "changed": [{"entity_id": "shared-fn-twin", "entity_kind": "semantic_element",
            "disposition": "upserted"}]
    });
    assert_succeeds(&system, ELEMENT_TRIGGER_EXPORT_ID, trigger.clone());
    assert_eq!(
        broker.semantic_operation_count("project_roots"),
        0,
        "transfer must not enumerate every semantic project root"
    );
    assert_eq!(
        broker.semantic_operation_count("project_artifacts"),
        0,
        "transfer must not scan whole-project artifacts"
    );
    let target_before_selection = invoke_value(
        &system,
        LIST_EXPORT_ID,
        serde_json::json!({"semantic_element_id": "shared-fn-twin"}),
    );
    assert_eq!(
        target_before_selection["artifacts"]
            .as_array()
            .map(Vec::len),
        Some(0)
    );

    let project_roots_before = broker.semantic_operation_count("project_roots");
    let project_artifacts_before = broker.semantic_operation_count("project_artifacts");
    let candidate_before = broker.semantic_operation_count("candidate_source_elements");
    let preview = invoke_value(
        &system,
        PREVIEW_TRANSFER_EXPORT_ID,
        serde_json::json!({"project_root": "/target-project"}),
    );
    assert_eq!(preview["project_root"], "/target-project");
    assert_eq!(preview["existing_artifact_count"], 0);
    let candidates = preview["candidates"]
        .as_array()
        .expect("transfer candidates");
    assert_eq!(candidates.len(), 2);
    assert!(candidates.iter().all(|candidate| {
        candidate["source_project_root"] == "/source-project"
            && candidate["source_semantic_element_id"] == "shared-fn"
            && candidate["target_semantic_element_id"] == "shared-fn-twin"
            && candidate["exact_match"] == true
            && candidate["already_copied"] == false
    }));
    assert_eq!(
        broker.semantic_operation_count("project_roots"),
        project_roots_before,
        "preview must use bounded candidate lookup, not enumerate project roots"
    );
    assert_eq!(
        broker.semantic_operation_count("project_artifacts"),
        project_artifacts_before,
        "preview must not scan whole-project artifacts"
    );
    assert_eq!(
        broker.semantic_operation_count("candidate_source_elements"),
        candidate_before + 1,
        "preview must use the bounded cross-project candidate operation"
    );

    let selected_candidate = candidates
        .iter()
        .find(|candidate| candidate["source_artifact_id"] == "source-artifact-a")
        .expect("attachment source candidate");
    let selected_transfer_id = selected_candidate["transfer_id"]
        .as_str()
        .expect("transfer id")
        .to_owned();
    let skipped_transfer_id = candidates
        .iter()
        .find(|candidate| candidate["source_artifact_id"] == "source-artifact-b")
        .expect("unselected source candidate")["transfer_id"]
        .as_str()
        .expect("transfer id")
        .to_owned();
    let apply_input = serde_json::json!({
        "project_root": "/target-project", "transfer_ids": [&selected_transfer_id]
    });
    let applied = invoke_value(&system, APPLY_TRANSFER_EXPORT_ID, apply_input.clone());
    assert_eq!(
        applied["copied_artifact_ids"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(
        applied["already_copied_artifact_ids"]
            .as_array()
            .map(Vec::len),
        Some(0)
    );
    let replay = invoke_value(&system, APPLY_TRANSFER_EXPORT_ID, apply_input);
    assert_eq!(
        replay["copied_artifact_ids"].as_array().map(Vec::len),
        Some(0)
    );
    assert_eq!(
        replay["already_copied_artifact_ids"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );

    let target_after_selection = invoke_value(
        &system,
        LIST_EXPORT_ID,
        serde_json::json!({"semantic_element_id": "shared-fn-twin"}),
    );
    assert_eq!(
        target_after_selection["artifacts"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(
        target_after_selection["artifacts"][0]["semantic_element_id"],
        "shared-fn-twin"
    );
    assert_eq!(
        target_after_selection["artifacts"][0]["project_root"],
        "/target-project"
    );
    let copied_artifact_id = applied["copied_artifact_ids"][0]
        .as_str()
        .expect("copied attachment artifact id");
    let copied_content_ref =
        target_after_selection["artifacts"][0]["metadata"]["files"]["image"]["contentRef"]
            .as_str()
            .expect("copied image reference");
    assert_ne!(copied_content_ref, "canvas-file:source-artifact-a:image");
    let copied_blob = SemanticPersistence::execute(
        broker.persistence.as_ref(),
        SemanticOperation::ArtifactBlobGet {
            content_ref: copied_content_ref.into(),
        },
        &InvocationControl::sixty_seconds(),
    )
    .expect("read copied image blob");
    let SemanticResult::ArtifactBlob(Some(copied_blob)) = copied_blob else {
        panic!("expected copied artifact blob, got {copied_blob:?}");
    };
    assert_eq!(copied_blob.artifact_id, copied_artifact_id);
    assert_eq!(copied_blob.content, vec![7, 8, 9]);
    let source_blob = SemanticPersistence::execute(
        broker.persistence.as_ref(),
        SemanticOperation::ArtifactBlobGet {
            content_ref: "canvas-file:source-artifact-a:image".into(),
        },
        &InvocationControl::sixty_seconds(),
    )
    .expect("source image blob remains readable");
    assert!(matches!(
        source_blob,
        SemanticResult::ArtifactBlob(Some(blob))
            if blob.artifact_id == "source-artifact-a" && blob.content == vec![7, 8, 9]
    ));
    let source_after_selection = invoke_value(
        &system,
        LIST_EXPORT_ID,
        serde_json::json!({"semantic_element_id": "shared-fn"}),
    );
    assert_eq!(
        source_after_selection["artifacts"].as_array().map(Vec::len),
        Some(2)
    );

    let upserts_before_invalid = broker.semantic_operation_count("upsert_artifact");
    let invalid = system
        .invoke(
            PLUGIN_ID,
            APPLY_TRANSFER_EXPORT_ID,
            serde_json::json!({"project_root": "/target-project", "transfer_ids": ["unknown"]}),
        )
        .expect("invalid transfer id returns protocol outcome");
    assert!(matches!(invalid, WireOutcome::Failed { .. }));
    assert_eq!(
        broker.semantic_operation_count("upsert_artifact"),
        upserts_before_invalid
    );

    let next_trigger = serde_json::json!({
        "project_root": "/target-project", "base_revision": 1, "target_revision": 2,
        "changed": [{"entity_id": "shared-fn-twin", "entity_kind": "semantic_element",
            "disposition": "upserted"}]
    });
    assert_succeeds(&system, ELEMENT_TRIGGER_EXPORT_ID, next_trigger);
    let target_after_retrigger = invoke_value(
        &system,
        LIST_EXPORT_ID,
        serde_json::json!({"semantic_element_id": "shared-fn-twin"}),
    );
    assert_eq!(
        target_after_retrigger["artifacts"].as_array().map(Vec::len),
        Some(1)
    );
    let final_preview = invoke_value(
        &system,
        PREVIEW_TRANSFER_EXPORT_ID,
        serde_json::json!({"project_root": "/target-project"}),
    );
    let selected_state = final_preview["candidates"]
        .as_array()
        .expect("final candidates")
        .iter()
        .find(|candidate| candidate["transfer_id"] == selected_transfer_id)
        .expect("selected candidate remains in preview");
    let skipped_state = final_preview["candidates"]
        .as_array()
        .expect("final candidates")
        .iter()
        .find(|candidate| candidate["transfer_id"] == skipped_transfer_id)
        .expect("unselected candidate remains available");
    assert_eq!(selected_state["already_copied"], true);
    assert_eq!(skipped_state["already_copied"], false);

    system
        .stop(PLUGIN_ID)
        .expect("stop packaged Knowledge binary");
    system
        .uninstall(PLUGIN_ID)
        .expect("detach Knowledge package");
    make_tree_writable(workspace.path());
}
