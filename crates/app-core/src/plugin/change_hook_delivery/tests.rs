use super::*;
use lumvise_db_core::{
    ArtifactTextVector, ArtifactTextVectorizer, SemanticArtifact, SemanticElement,
};
use serde_json::json;

struct CountingSemanticPersistence {
    inner: std::sync::Arc<dyn lumvise_db_core::SemanticPersistence>,
    dirty_batches: std::sync::atomic::AtomicUsize,
    element_batches: std::sync::atomic::AtomicUsize,
    single_elements: std::sync::atomic::AtomicUsize,
}

impl lumvise_db_core::SemanticPersistence for CountingSemanticPersistence {
    fn execute(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> lumvise_db_core::Result<SemanticResult> {
        use std::sync::atomic::Ordering::Relaxed;
        match &operation {
            SemanticOperation::ChangeHookDirtyBatch { .. } => {
                self.dirty_batches.fetch_add(1, Relaxed);
            }
            SemanticOperation::ElementsByIds { .. } => {
                self.element_batches.fetch_add(1, Relaxed);
            }
            SemanticOperation::Element { .. } => {
                self.single_elements.fetch_add(1, Relaxed);
            }
            _ => {}
        }
        self.inner.execute(operation, control)
    }
    fn readiness(&self) -> lumvise_db_core::Result<lumvise_db_core::SemanticReadiness> {
        self.inner.readiness()
    }
}

#[test]
fn unavailable_vectorizer_preserves_pending_work_without_scanning_then_reads_one_batch() {
    use std::sync::{Arc, atomic::Ordering::Relaxed};
    let mut app = crate::AppCore::in_memory().unwrap();
    execute(
        &app,
        SemanticOperation::SyncStructure {
            project_root: "/repo".into(),
            elements: vec![element("queued")],
            relationships: vec![],
        },
    );
    let counted = Arc::new(CountingSemanticPersistence {
        inner: Arc::clone(&app.semantic),
        dirty_batches: Default::default(),
        element_batches: Default::default(),
        single_elements: Default::default(),
    });
    app.semantic = counted.clone();
    for cycle in 0..3 {
        assert!(
            app.plugin_endpoints()
                .run_change_hook_cycle(cycle)
                .unwrap()
                .hooks
                .is_empty()
        );
    }
    assert_eq!(counted.dirty_batches.load(Relaxed), 0);
    app.plugin_endpoints()
        .set_plugin_vectorizer(Box::new(TestVectorizer))
        .unwrap();
    let report = app.plugin_endpoints().run_change_hook_cycle(4).unwrap();
    assert!(
        report
            .hooks
            .iter()
            .any(|run| run.delivered == 1 && run.acknowledged)
    );
    assert_eq!(counted.element_batches.load(Relaxed), 1);
    assert_eq!(counted.single_elements.load(Relaxed), 0);
}

struct TestVectorizer;

impl ArtifactTextVectorizer for TestVectorizer {
    fn vectorize_artifact_text(
        &self,
        text: &str,
        _task: lumvise_db_core::TextEmbeddingTask,
    ) -> lumvise_db_core::Result<ArtifactTextVector> {
        Ok(ArtifactTextVector {
            engine_id: "test-vectorizer".into(),
            model: Some("test-model".into()),
            dimensions: 2,
            vector: vec![text.len() as f32, 1.0],
            normalized: false,
            metadata: json!({}),
        })
    }
}

fn element(name: &str) -> SemanticElement {
    SemanticElement {
        revision_dependencies: Vec::new(),
        project_root: "/repo".into(),
        semantic_element_id: "element-1".into(),
        semantic_source_id: "source-1".into(),
        path: "src/lib.rs".into(),
        element_kind: "function".into(),
        name: name.into(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: Some(1),
        end_line: Some(1),
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({}),
    }
}

fn artifact(content: &str) -> SemanticArtifact {
    SemanticArtifact {
        artifact_id: "artifact-1".into(),
        semantic_element_id: "element-1".into(),
        artifact_kind: "documentation".into(),
        title: "Doc".into(),
        content_ref: None,
        content: Some(content.into()),
        searchable_text: None,
        content_size_bytes: Some(content.len()),
        dependencies: Vec::new(),
        metadata: json!({}),
    }
}

fn execute(app: &crate::AppCore, operation: SemanticOperation) -> SemanticResult {
    app.semantic
        .execute(operation, &InvocationControl::sixty_seconds())
        .expect("semantic operation")
}

#[test]
fn vector_hook_backfills_reconciles_and_is_idempotent() {
    let app = crate::AppCore::in_memory().expect("app");
    execute(
        &app,
        SemanticOperation::SyncStructure {
            project_root: "/repo".into(),
            elements: vec![element("old name")],
            relationships: vec![],
        },
    );
    execute(
        &app,
        SemanticOperation::UpsertArtifact {
            artifact: artifact("old body"),
            media_type: "text/plain".into(),
        },
    );
    app.plugin_endpoints()
        .set_plugin_vectorizer(Box::new(TestVectorizer))
        .expect("vectorizer");

    let backfill = app
        .plugin_endpoints()
        .run_change_hook_cycle(1)
        .expect("backfill cycle");
    let vector_run = backfill
        .hooks
        .iter()
        .find(|run| run.hook_name.starts_with(VECTOR_HOOK_PREFIX))
        .expect("vector hook");
    assert!(vector_run.acknowledged);
    assert!(vector_run.delivered >= 1);

    let registrations = execute(&app, SemanticOperation::ChangeHookRegistrations);
    let SemanticResult::ChangeHookRegistrations(registrations) = registrations else {
        panic!("change-hook registrations");
    };
    assert!(registrations.iter().any(|registration| {
        registration.hook_name.starts_with(VECTOR_HOOK_PREFIX) && registration.watermark >= 0
    }));

    let element_search = execute(
        &app,
        SemanticOperation::SearchElementNameVectors {
            project_root: "/repo".into(),
            query: vec![8.0, 1.0],
            k: 1,
            engine_id: "test-vectorizer".into(),
            model: Some("test-model".into()),
        },
    );
    assert!(
        matches!(element_search, SemanticResult::ElementVectorSearch(results) if !results.is_empty())
    );
    let artifact_search = execute(
        &app,
        SemanticOperation::SearchArtifactTextVectors {
            project_root: "/repo".into(),
            query: vec![8.0, 1.0],
            k: 1,
            engine_id: "test-vectorizer".into(),
            model: Some("test-model".into()),
        },
    );
    assert!(
        matches!(artifact_search, SemanticResult::ArtifactVectorSearch(results) if !results.is_empty())
    );

    let idle = app
        .plugin_endpoints()
        .run_change_hook_cycle(2)
        .expect("idempotent cycle");
    let idle_vector = idle
        .hooks
        .iter()
        .find(|run| run.hook_name.starts_with(VECTOR_HOOK_PREFIX))
        .expect("vector hook");
    assert_eq!(idle_vector.delivered, 0);
    assert!(idle_vector.acknowledged);

    execute(
        &app,
        SemanticOperation::SyncStructure {
            project_root: "/repo".into(),
            elements: vec![element("new name")],
            relationships: vec![],
        },
    );
    let changed = app
        .plugin_endpoints()
        .run_change_hook_cycle(3)
        .expect("changed cycle");
    let changed_vector = changed
        .hooks
        .iter()
        .find(|run| run.hook_name.starts_with(VECTOR_HOOK_PREFIX))
        .expect("vector hook");
    assert!(changed_vector.delivered >= 1);
    assert!(changed_vector.acknowledged);
}

fn element_in(root: &str, id: &str, name: &str) -> SemanticElement {
    SemanticElement {
        revision_dependencies: Vec::new(),
        project_root: root.into(),
        semantic_element_id: id.into(),
        semantic_source_id: "source-1".into(),
        path: "src/lib.rs".into(),
        element_kind: "function".into(),
        name: name.into(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: Some(1),
        end_line: Some(1),
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({}),
    }
}

fn artifact_in(id: &str, element_id: &str, content: &str) -> SemanticArtifact {
    SemanticArtifact {
        artifact_id: id.into(),
        semantic_element_id: element_id.into(),
        artifact_kind: "documentation".into(),
        title: "Doc".into(),
        content_ref: None,
        content: Some(content.into()),
        searchable_text: None,
        content_size_bytes: Some(content.len()),
        dependencies: Vec::new(),
        metadata: json!({}),
    }
}

/// Fails `StoreArtifactTextVectors` for one poisoned project root (mirroring
/// an artifact whose owning semantic element was deleted out from under
/// it), passing every other operation through unchanged.
struct FaultInjectingSemanticPersistence {
    inner: std::sync::Arc<dyn lumvise_db_core::SemanticPersistence>,
    poisoned_root: String,
}

impl lumvise_db_core::SemanticPersistence for FaultInjectingSemanticPersistence {
    fn execute(
        &self,
        operation: SemanticOperation,
        control: &InvocationControl,
    ) -> lumvise_db_core::Result<SemanticResult> {
        if let SemanticOperation::StoreArtifactTextVectors { project_root, .. } = &operation
            && project_root == &self.poisoned_root
        {
            return Err(lumvise_db_core::DbError::invalid_value(
                project_root.clone(),
                "existing owning semantic element for artifact (test fault injection)",
            ));
        }
        self.inner.execute(operation, control)
    }

    fn readiness(&self) -> lumvise_db_core::Result<lumvise_db_core::SemanticReadiness> {
        self.inner.readiness()
    }
}

#[test]
fn removed_owner_keeps_knowledge_without_poisoning_vector_hook() {
    let app = crate::AppCore::in_memory().unwrap();
    execute(
        &app,
        SemanticOperation::SyncStructure {
            project_root: "/repo".into(),
            elements: vec![element("old name"), element_in("/repo", "keeper", "keeper")],
            relationships: vec![],
        },
    );
    execute(
        &app,
        SemanticOperation::UpsertArtifact {
            artifact: artifact("retained knowledge"),
            media_type: "text/plain".into(),
        },
    );
    app.plugin_endpoints()
        .set_plugin_vectorizer(Box::new(TestVectorizer))
        .unwrap();
    app.plugin_endpoints().run_change_hook_cycle(1).unwrap();
    execute(
        &app,
        SemanticOperation::SyncStructure {
            project_root: "/repo".into(),
            elements: vec![element_in("/repo", "keeper", "keeper")],
            relationships: vec![],
        },
    );
    let report = app.plugin_endpoints().run_change_hook_cycle(2).unwrap();
    let run = report
        .hooks
        .iter()
        .find(|run| run.hook_name == format!("{VECTOR_HOOK_PREFIX}/repo"))
        .unwrap();
    assert!(
        run.acknowledged,
        "removal must advance the vector watermark"
    );
    let retained = execute(
        &app,
        SemanticOperation::ArtifactsForElements {
            semantic_element_ids: HashSet::from(["element-1".into()]),
        },
    );
    assert!(matches!(retained, SemanticResult::Artifacts(records) if records.len() == 1));
    let idle = app.plugin_endpoints().run_change_hook_cycle(3).unwrap();
    assert_eq!(
        idle.hooks
            .iter()
            .find(|run| run.hook_name == format!("{VECTOR_HOOK_PREFIX}/repo"))
            .unwrap()
            .delivered,
        0
    );
}

#[test]
fn one_broken_project_vector_hook_does_not_block_another_projects_delivery() {
    use std::sync::Arc;
    let mut app = crate::AppCore::in_memory().expect("app");
    execute(
        &app,
        SemanticOperation::SyncStructure {
            project_root: "/broken".into(),
            elements: vec![element_in("/broken", "broken-element", "broken name")],
            relationships: vec![],
        },
    );
    execute(
        &app,
        SemanticOperation::UpsertArtifact {
            artifact: artifact_in("broken-artifact", "broken-element", "broken body"),
            media_type: "text/plain".into(),
        },
    );
    execute(
        &app,
        SemanticOperation::SyncStructure {
            project_root: "/healthy".into(),
            elements: vec![element_in("/healthy", "healthy-element", "healthy name")],
            relationships: vec![],
        },
    );
    execute(
        &app,
        SemanticOperation::UpsertArtifact {
            artifact: artifact_in("healthy-artifact", "healthy-element", "healthy body"),
            media_type: "text/plain".into(),
        },
    );
    app.plugin_endpoints()
        .set_plugin_vectorizer(Box::new(TestVectorizer))
        .expect("vectorizer");
    app.semantic = Arc::new(FaultInjectingSemanticPersistence {
        inner: Arc::clone(&app.semantic),
        poisoned_root: "/broken".to_string(),
    });

    let report = app
        .plugin_endpoints()
        .run_change_hook_cycle(1)
        .expect("cycle must not abort when one project's hook fails");

    let healthy_hook_name = format!("{VECTOR_HOOK_PREFIX}/healthy");
    let healthy_run = report
        .hooks
        .iter()
        .find(|run| run.hook_name == healthy_hook_name)
        .expect("healthy project's vector hook still ran in the same cycle");
    assert!(healthy_run.delivered >= 1);
    assert!(healthy_run.acknowledged);

    let broken_hook_name = format!("{VECTOR_HOOK_PREFIX}/broken");
    assert!(
        report
            .hooks
            .iter()
            .all(|run| run.hook_name != broken_hook_name),
        "broken project's failing run must not be reported as delivered"
    );
}
#[test]
fn storage_trigger_route_preserves_typed_batch_through_host_invoke_frame() {
    let app = crate::AppCore::in_memory().expect("app");
    execute(
        &app,
        SemanticOperation::SyncStructure {
            project_root: "/repo".into(),
            elements: vec![element("batch name")],
            relationships: vec![],
        },
    );
    let hook_name = "storage-trigger:test.storage:/repo".to_owned();
    execute(
        &app,
        SemanticOperation::RegisterChangeHook {
            hook_name: hook_name.clone(),
            scope: ChangeHookScope {
                project_root: "/repo".into(),
                entity_kinds: BTreeSet::from(["function".to_owned()]),
            },
        },
    );
    let batch = match execute(
        &app,
        SemanticOperation::ChangeHookDirtyBatch {
            hook_name,
            maximum_elements: HOOK_BATCH_SIZE,
        },
    ) {
        SemanticResult::ChangeHookBatch(batch) => batch,
        other => panic!("expected dirty batch, got {other:?}"),
    };
    assert!(
        !batch.changed.is_empty(),
        "fixture batch must contain a change"
    );

    let payload = change_hook_payload("/repo", &batch).expect("typed route payload");
    let request = StorageTriggerRequest::from_value(payload.clone()).expect("typed request");
    assert_eq!(request.project_root, "/repo");
    assert_eq!(request.base_revision, batch.base_revision);
    assert_eq!(request.target_revision, batch.target_revision);
    assert_eq!(request.changed.len(), batch.changed.len());
    assert_eq!(request.changed[0].entity_id, "element-1");
    assert_eq!(request.changed[0].entity_kind, "function");
    assert_eq!(
        request.changed[0].disposition,
        StorageTriggerDisposition::Upserted
    );

    let runtime_request = crate::plugin::invocation::background_invocation_request(
        "test.storage",
        "trigger",
        payload,
        "delivery-1",
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    );
    let message = WireMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        body: MessageBody::HostInvoke {
            session_id: "session-1".into(),
            invocation_id: runtime_request.context().request_id().into(),
            capability_id: "trigger".into(),
            input: runtime_request.input().clone(),
        },
    };
    let codec = FrameCodec::default();
    let decoded = codec
        .decode(&codec.encode(&message).expect("encode host invoke"))
        .expect("decode host invoke");
    let MessageBody::HostInvoke { input, .. } = decoded.body else {
        panic!("expected host invoke");
    };
    let decoded_request = StorageTriggerRequest::from_value(input).expect("decode typed request");
    assert_eq!(decoded_request, request);

    let response = WireMessage {
        protocol: CURRENT_PROTOCOL_VERSION,
        body: MessageBody::PluginResult {
            session_id: "session-1".into(),
            invocation_id: "delivery-1".into(),
            outcome: WireOutcome::Succeeded {
                value: StorageTriggerResponse { acknowledged: true }
                    .to_value()
                    .expect("encode typed response"),
            },
        },
    };
    let decoded_response = codec
        .decode(&codec.encode(&response).expect("encode plugin result"))
        .expect("decode plugin result");
    let MessageBody::PluginResult {
        outcome: WireOutcome::Succeeded { value },
        ..
    } = decoded_response.body
    else {
        panic!("expected successful plugin result");
    };
    assert_eq!(
        StorageTriggerResponse::from_value(value).expect("decode typed response"),
        StorageTriggerResponse { acknowledged: true }
    );
}

mod vector_progress;
