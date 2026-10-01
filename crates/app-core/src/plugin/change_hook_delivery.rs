//! Fixed-cadence delivery of graph change-hook batches to internal consumers.

use std::collections::{BTreeSet, HashSet};

use lumvise_db_core::{
    ChangeDisposition, ChangeHookRegistration, ChangeHookScope, RelationalOperation,
    RelationalResult, SemanticOperation, SemanticResult, StoredArtifactTextVector,
    StoredSemanticElementNameVector,
};
#[cfg(test)]
use lumvise_plugin_protocol::{
    CURRENT_PROTOCOL_VERSION, FrameCodec, MessageBody, StorageTriggerResponse, WireMessage,
    WireOutcome,
};
use lumvise_plugin_protocol::{
    StorageTriggerChanged, StorageTriggerDisposition, StorageTriggerRequest,
};
use lumvise_plugin_runtime::{BackgroundExportKind, PublishedBackgroundExport};
use lumvise_resource_routing::InvocationControl;
use serde_json::Value;
use tracing::error;

use crate::{AppCoreError, PluginEndpoints, Result};
const STORAGE_HOOK_PREFIX: &str = "storage-trigger:";
const VECTOR_HOOK_PREFIX: &str = "vector-reindex:";
const HOOK_BATCH_SIZE: usize = 128;

/// Observable result of one coordinator sweep. Delivery remains opaque to callers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangeHookCycleReport {
    pub hooks: Vec<ChangeHookRun>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeHookRun {
    pub hook_name: String,
    pub base_revision: i64,
    pub target_revision: i64,
    pub delivered: usize,
    pub retrying: usize,
    pub dead_lettered: usize,
    pub acknowledged: bool,
}
impl PluginEndpoints<'_> {
    /// Runs one deterministic change-hook coordinator cycle. This is public so
    /// embeddings and focused tests can drive the worker without sleeping.
    pub fn run_change_hook_cycle(&self, now_unix_seconds: i64) -> Result<ChangeHookCycleReport> {
        let exports = self.app.plugin_system().background_exports()?;
        self.ensure_internal_hooks(&exports)?;
        let registrations = self.change_hook_registrations()?;
        let mut report = ChangeHookCycleReport::default();
        for registration in registrations {
            // One registration stuck on unfixable data (e.g. an artifact whose
            // owning semantic element was deleted out from under it) must never
            // starve every other project's delivery forever: isolate the
            // failure and keep sweeping the rest of the list.
            if registration.hook_name.starts_with(VECTOR_HOOK_PREFIX) {
                let Some(vectorizer) = self.plugin_vectorizer()? else {
                    continue;
                };
                let hook_name = registration.hook_name.clone();
                match self.run_vector_hook(registration, vectorizer.as_ref()) {
                    Ok(run) => report.hooks.push(run),
                    Err(error) => log_hook_error(&hook_name, &error.to_string()),
                }
                continue;
            }
            let Some(export) = storage_hook_export(&exports, &registration) else {
                continue;
            };
            let hook_name = registration.hook_name.clone();
            match self.run_registered_hook(&exports, export, registration, now_unix_seconds) {
                Ok(run) => report.hooks.push(run),
                Err(error) => log_hook_error(&hook_name, &error.to_string()),
            }
        }
        Ok(report)
    }

    fn ensure_internal_hooks(&self, exports: &[PublishedBackgroundExport]) -> Result<()> {
        let roots = match self.app.semantic.execute(
            SemanticOperation::ProjectRoots,
            &InvocationControl::sixty_seconds(),
        )? {
            SemanticResult::ProjectRoots(roots) => roots,
            result => return Err(semantic_result_error("project roots", result)),
        };
        for root in &roots {
            self.register_hook(
                format!("{VECTOR_HOOK_PREFIX}{root}"),
                root.clone(),
                BTreeSet::new(),
            )?;
            for export in exports.iter().filter(external_storage_export) {
                let BackgroundExportKind::StorageTrigger { entity_kinds, .. } = &export.kind else {
                    continue;
                };
                self.register_hook(
                    format!(
                        "{STORAGE_HOOK_PREFIX}{}:{}:{}",
                        export.plugin_id, export.export_id, root
                    ),
                    root.clone(),
                    entity_kinds.iter().cloned().collect(),
                )?;
            }
        }
        let desired = roots
            .iter()
            .flat_map(|root| {
                let vector = format!("{VECTOR_HOOK_PREFIX}{root}");
                let storage = exports
                    .iter()
                    .filter(|export| {
                        matches!(export.kind, BackgroundExportKind::StorageTrigger { .. })
                    })
                    .map(move |export| {
                        format!(
                            "{STORAGE_HOOK_PREFIX}{}:{}:{root}",
                            export.plugin_id, export.export_id
                        )
                    });
                std::iter::once(vector).chain(storage)
            })
            .collect::<BTreeSet<_>>();
        let registrations = match self.app.semantic.execute(
            SemanticOperation::ChangeHookRegistrations,
            &InvocationControl::sixty_seconds(),
        )? {
            SemanticResult::ChangeHookRegistrations(registrations) => registrations,
            result => return Err(semantic_result_error("change hook registrations", result)),
        };
        for registration in registrations {
            if !desired.contains(&registration.hook_name) {
                self.deregister_hook(registration.hook_name)?;
            }
        }
        Ok(())
    }

    fn register_hook(
        &self,
        hook_name: String,
        project_root: String,
        entity_kinds: BTreeSet<String>,
    ) -> Result<()> {
        let result = self.app.semantic.execute(
            SemanticOperation::RegisterChangeHook {
                hook_name,
                scope: ChangeHookScope {
                    project_root,
                    entity_kinds,
                },
            },
            &InvocationControl::sixty_seconds(),
        )?;
        if matches!(result, SemanticResult::ChangeHookRegistration(_)) {
            Ok(())
        } else {
            Err(semantic_result_error("register change hook", result))
        }
    }

    fn deregister_hook(&self, hook_name: String) -> Result<()> {
        let result = self.app.semantic.execute(
            SemanticOperation::DeregisterChangeHook { hook_name },
            &InvocationControl::sixty_seconds(),
        )?;
        if matches!(result, SemanticResult::ChangeHookDeregistered { .. }) {
            Ok(())
        } else {
            Err(semantic_result_error("deregister change hook", result))
        }
    }
    fn change_hook_registrations(&self) -> Result<Vec<ChangeHookRegistration>> {
        match self.app.semantic.execute(
            SemanticOperation::ChangeHookRegistrations,
            &InvocationControl::sixty_seconds(),
        )? {
            SemanticResult::ChangeHookRegistrations(registrations) => Ok(registrations),
            result => Err(semantic_result_error("change hook registrations", result)),
        }
    }

    fn run_vector_hook(
        &self,
        registration: ChangeHookRegistration,
        vectorizer: &(dyn lumvise_db_core::ArtifactTextVectorizer + Send + Sync),
    ) -> Result<ChangeHookRun> {
        let batch = match self.app.semantic.execute(
            SemanticOperation::ChangeHookDirtyBatch {
                hook_name: registration.hook_name.clone(),
                maximum_elements: HOOK_BATCH_SIZE,
            },
            &InvocationControl::sixty_seconds(),
        )? {
            SemanticResult::ChangeHookBatch(batch) => batch,
            result => {
                return Err(semantic_result_error(
                    "vector change hook dirty batch",
                    result,
                ));
            }
        };
        let mut run = ChangeHookRun {
            hook_name: registration.hook_name,
            base_revision: batch.base_revision,
            target_revision: batch.target_revision,
            delivered: 0,
            retrying: 0,
            dead_lettered: 0,
            acknowledged: false,
        };
        let ids = batch
            .changed
            .iter()
            .map(|changed| changed.element_id.clone())
            .collect::<HashSet<_>>();
        let elements = match self.app.semantic.execute(
            SemanticOperation::ElementsByIds {
                project_root: registration.scope.project_root.clone(),
                semantic_element_ids: ids.clone(),
            },
            &InvocationControl::sixty_seconds(),
        )? {
            SemanticResult::Elements(elements) => elements,
            result => return Err(semantic_result_error("vector elements", result)),
        };
        let artifacts = match self.app.semantic.execute(
            SemanticOperation::ArtifactsForElements {
                semantic_element_ids: ids,
            },
            &InvocationControl::sixty_seconds(),
        )? {
            SemanticResult::Artifacts(artifacts) => artifacts,
            result => return Err(semantic_result_error("vector artifacts", result)),
        };
        let mut element_vectors = Vec::new();
        for element in elements
            .iter()
            .filter(|element| element.lifecycle == "active")
        {
            let source_text = element.name.trim();
            if source_text.is_empty() {
                continue;
            }
            let vector = vectorizer.vectorize_artifact_text(source_text)?;
            element_vectors.push(StoredSemanticElementNameVector {
                semantic_element_id: element.semantic_element_id.clone(),
                project_root: element.project_root.clone(),
                source_text: source_text.to_owned(),
                vector,
            });
        }
        // Removal batches retain artifacts on inactive owners. They remain knowledge,
        // but cannot produce searchable vectors until that owner becomes active again.
        let active_ids: HashSet<&str> = elements
            .iter()
            .filter(|element| element.lifecycle == "active")
            .map(|element| element.semantic_element_id.as_str())
            .collect();
        let mut artifact_vectors = Vec::new();
        for artifact in artifacts
            .into_iter()
            .filter(|artifact| active_ids.contains(artifact.semantic_element_id.as_str()))
        {
            let Some(source_text) = artifact
                .searchable_text
                .as_deref()
                .or(artifact.content.as_deref())
                .map(str::trim)
                .filter(|text| !text.is_empty())
            else {
                continue;
            };
            let vector = vectorizer.vectorize_artifact_text(source_text)?;
            artifact_vectors.push(StoredArtifactTextVector {
                artifact_id: artifact.artifact_id,
                semantic_element_id: artifact.semantic_element_id,
                source_text: source_text.to_owned(),
                vector,
            });
        }
        if !element_vectors.is_empty() {
            self.app.semantic.execute(
                SemanticOperation::StoreElementNameVectors {
                    project_root: registration.scope.project_root.clone(),
                    vectors: element_vectors,
                },
                &InvocationControl::sixty_seconds(),
            )?;
        }
        if !artifact_vectors.is_empty() {
            self.app.semantic.execute(
                SemanticOperation::StoreArtifactTextVectors {
                    project_root: registration.scope.project_root.clone(),
                    vectors: artifact_vectors,
                },
                &InvocationControl::sixty_seconds(),
            )?;
        }
        run.delivered = batch.changed.len();
        run.acknowledged = self.acknowledge_change_hook(batch)?;
        Ok(run)
    }

    fn acknowledge_change_hook(&self, batch: lumvise_db_core::ChangeBatch) -> Result<bool> {
        match self.app.semantic.execute(
            SemanticOperation::AcknowledgeChangeHook { batch },
            &InvocationControl::sixty_seconds(),
        )? {
            SemanticResult::ChangeHookAcknowledged { advanced } => Ok(advanced),
            result => Err(semantic_result_error("acknowledge change hook", result)),
        }
    }

    fn run_registered_hook(
        &self,
        exports: &[PublishedBackgroundExport],
        export: &PublishedBackgroundExport,
        registration: ChangeHookRegistration,
        now_unix_seconds: i64,
    ) -> Result<ChangeHookRun> {
        let batch = match self.app.semantic.execute(
            SemanticOperation::ChangeHookDirtyBatch {
                hook_name: registration.hook_name.clone(),
                maximum_elements: HOOK_BATCH_SIZE,
            },
            &InvocationControl::sixty_seconds(),
        )? {
            SemanticResult::ChangeHookBatch(batch) => batch,
            result => return Err(semantic_result_error("change hook dirty batch", result)),
        };
        let mut run = ChangeHookRun {
            hook_name: registration.hook_name.clone(),
            base_revision: batch.base_revision,
            target_revision: batch.target_revision,
            delivered: 0,
            retrying: 0,
            dead_lettered: 0,
            acknowledged: false,
        };
        let deliveries = if batch.changed.is_empty() {
            Vec::new()
        } else {
            let source_key = format!(
                "{}:{}:{}",
                registration.hook_name, batch.base_revision, batch.target_revision
            );
            let delivery_id = change_hook_delivery_id(export, &source_key);
            let payload = change_hook_payload(&registration.scope.project_root, &batch)?;
            vec![(source_key, delivery_id, payload)]
        };
        if !deliveries.is_empty() {
            let result = self.app.relational.execute(
                RelationalOperation::EnqueueChangeHookBatch {
                    plugin_id: export.plugin_id.clone(),
                    export_id: export.export_id.clone(),
                    now_unix_seconds,
                    deliveries: deliveries
                        .iter()
                        .map(|(source_key, _, payload)| (source_key.clone(), payload.clone()))
                        .collect(),
                },
                &InvocationControl::sixty_seconds(),
            )?;
            if !matches!(result, RelationalResult::ChangeHookBatchEnqueued { .. }) {
                return Err(relational_result_error("enqueue change-hook batch", result));
            }
            // Reuse the same 60-second invocation, lease, retry, and dead-letter
            // path as all durable background exports.
            self.process_due_background_kind(exports, now_unix_seconds, "change_hook")?;
        }
        for (_, delivery_id, _) in &deliveries {
            let delivery = match self.app.relational.execute(
                RelationalOperation::DeliveryDiagnostics {
                    delivery_id: delivery_id.clone(),
                },
                &InvocationControl::sixty_seconds(),
            )? {
                RelationalResult::BackgroundDelivery(delivery) => delivery,
                result => return Err(relational_result_error("change-hook diagnostics", result)),
            };
            match delivery.as_ref().map(|delivery| delivery.state.as_str()) {
                Some("completed") => run.delivered = batch.changed.len(),
                Some("dead_letter") => run.dead_lettered = batch.changed.len(),
                _ => run.retrying = batch.changed.len(),
            }
        }
        if run.retrying == 0 {
            let result = self.app.semantic.execute(
                SemanticOperation::AcknowledgeChangeHook { batch },
                &InvocationControl::sixty_seconds(),
            )?;
            run.acknowledged = match result {
                SemanticResult::ChangeHookAcknowledged { advanced } => advanced,
                result => return Err(semantic_result_error("acknowledge change hook", result)),
            };
        }
        Ok(run)
    }
}

fn log_hook_error(hook_name: &str, message: &str) {
    error!(
        target: "app-core::plugin_background_driver",
        export_kind = "change_hook_coordinator",
        hook_name = hook_name,
        message = message,
        "plugin background driver error"
    );
}
fn external_storage_export(export: &&PublishedBackgroundExport) -> bool {
    matches!(export.kind, BackgroundExportKind::StorageTrigger { .. })
}

fn storage_hook_export<'a>(
    exports: &'a [PublishedBackgroundExport],
    registration: &ChangeHookRegistration,
) -> Option<&'a PublishedBackgroundExport> {
    let suffix = registration.hook_name.strip_prefix(STORAGE_HOOK_PREFIX)?;
    let mut parts = suffix.splitn(3, ':');
    let plugin_id = parts.next()?;
    let export_id = parts.next()?;
    let root = parts.next()?;
    if root != registration.scope.project_root {
        return None;
    }
    exports.iter().find(|export| {
        export.plugin_id == plugin_id
            && export.export_id == export_id
            && matches!(export.kind, BackgroundExportKind::StorageTrigger { .. })
    })
}

fn change_hook_delivery_id(export: &PublishedBackgroundExport, source_key: &str) -> String {
    format!(
        "change_hook:{}:{}:{source_key}",
        export.plugin_id, export.export_id
    )
}

fn change_hook_payload(project_root: &str, batch: &lumvise_db_core::ChangeBatch) -> Result<Value> {
    let request = StorageTriggerRequest {
        project_root: project_root.to_owned(),
        base_revision: batch.base_revision,
        target_revision: batch.target_revision,
        changed: batch
            .changed
            .iter()
            .map(|changed| StorageTriggerChanged {
                entity_id: changed.element_id.clone(),
                entity_kind: changed.entity_kind.clone(),
                disposition: match changed.disposition {
                    ChangeDisposition::Upserted => StorageTriggerDisposition::Upserted,
                    ChangeDisposition::Removal => StorageTriggerDisposition::Removal,
                },
            })
            .collect(),
    };
    request.to_value().map_err(|error| {
        AppCoreError::invalid_value(error.to_string(), "valid StorageTrigger batch")
    })
}

fn semantic_result_error(operation: &str, result: SemanticResult) -> AppCoreError {
    AppCoreError::unsupported(
        operation,
        format!("unexpected semantic persistence result: {result:?}"),
    )
}

fn relational_result_error(operation: &str, result: RelationalResult) -> AppCoreError {
    AppCoreError::unsupported(
        operation,
        format!("unexpected relational persistence result: {result:?}"),
    )
}

#[cfg(test)]
mod tests {
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
        let decoded_request =
            StorageTriggerRequest::from_value(input).expect("decode typed request");
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
}
