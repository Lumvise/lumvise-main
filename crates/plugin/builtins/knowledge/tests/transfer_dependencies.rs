use std::sync::Arc;

use lumvise_app_core::AppCoreHostCapabilityBroker;
use lumvise_db_core::{
    LocalPersistence, RelationalPersistence, SemanticArtifact, SemanticElement, SemanticOperation,
    SemanticPersistence, SemanticResult,
};
use lumvise_plugin_knowledge::{
    APPLY_TRANSFER_EXPORT_ID, CREATE_EXPORT_ID, KnowledgePlugin, PLUGIN_ID,
    PREVIEW_TRANSFER_EXPORT_ID, UPDATE_EXPORT_ID,
};
use lumvise_plugin_runtime::{HostCapabilityBroker, HostCapabilityRequest};
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use lumvise_resource_routing::InvocationControl;
use serde_json::{Value, json};

struct TransferDependencyFakeHost {
    persistence: Arc<LocalPersistence>,
    broker: AppCoreHostCapabilityBroker,
    fail_final_artifact: Option<String>,
    writes: Vec<String>,
}

impl TransferDependencyFakeHost {
    fn new() -> Self {
        let persistence = Arc::new(LocalPersistence::in_memory().unwrap());
        let semantic: Arc<dyn SemanticPersistence> = persistence.clone();
        let relational: Arc<dyn RelationalPersistence> = persistence.clone();
        let host = Self {
            persistence,
            broker: AppCoreHostCapabilityBroker::new(semantic, relational),
            fail_final_artifact: None,
            writes: Vec::new(),
        };
        for (root, id) in [("/source", "original"), ("/target", "target")] {
            host.execute(SemanticOperation::SyncStructure {
                project_root: root.into(),
                elements: vec![matching_element(root, id)],
                relationships: Vec::new(),
            });
        }
        host
    }

    fn execute(&self, operation: SemanticOperation) -> SemanticResult {
        SemanticPersistence::execute(
            self.persistence.as_ref(),
            operation,
            &InvocationControl::sixty_seconds(),
        )
        .unwrap()
    }

    fn invoke(&mut self, export: &str, input: Value) -> Result<Value, PluginError> {
        KnowledgePlugin::default().dispatch(export, input, &mut PluginContext::for_test(self))
    }

    fn create_source(&mut self, artifact_id: &str, dependency: Option<&str>) {
        let dependencies = dependency
            .map(artifact_dependency)
            .into_iter()
            .collect::<Vec<_>>();
        self.invoke(
            CREATE_EXPORT_ID,
            json!({
                "artifact_id": artifact_id, "semantic_element_id": "original",
                "project_root": "/source", "knowledge_type": "definition",
                "title": artifact_id, "content": "Source knowledge", "dependencies": dependencies
            }),
        )
        .unwrap();
    }

    fn seed_dependencies(&mut self, cycle: bool) {
        self.create_source("source-B", None);
        self.create_source("source-A", Some("source-B"));
        if cycle {
            self.invoke(
                UPDATE_EXPORT_ID,
                json!({
                    "artifact_id": "source-B", "dependencies": [artifact_dependency("source-A")]
                }),
            )
            .unwrap();
        }
        self.writes.clear();
    }

    fn preview(&mut self) -> Value {
        self.invoke(
            PREVIEW_TRANSFER_EXPORT_ID,
            json!({"project_root": "/target"}),
        )
        .unwrap()
    }

    fn apply(&mut self, ids: &[String]) -> Result<Value, PluginError> {
        self.invoke(
            APPLY_TRANSFER_EXPORT_ID,
            json!({"project_root": "/target", "transfer_ids": ids}),
        )
    }

    fn artifact(&self, artifact_id: &str) -> SemanticArtifact {
        let result = self.execute(SemanticOperation::Artifact {
            artifact_id: artifact_id.into(),
        });
        let SemanticResult::Artifact(Some(artifact)) = result else {
            panic!("expected existing artifact `{artifact_id}`, got {result:?}");
        };
        artifact
    }

    fn fail_selected_final_save(
        &mut self,
        input: &Value,
        artifact_id: &str,
    ) -> Result<(), PluginError> {
        let is_final = input["artifact"]["metadata"]["knowledge"]["metadata"]["inheritance"]["transfer_pending"]
            != true;
        if !is_final || self.fail_final_artifact.as_deref() != Some(artifact_id) {
            return Ok(());
        }
        self.fail_final_artifact = None;
        Err(PluginError::new(
            "injected_final_write_failure",
            format!("final save `{artifact_id}` failed once; expected retry"),
            true,
        ))
    }

    fn call_semantic_broker(&self, input: Value) -> Result<Value, PluginError> {
        self.broker
            .invoke(HostCapabilityRequest {
                plugin_id: PLUGIN_ID.into(),
                invocation_id: "transfer-regression".into(),
                call_id: "transfer-host-call".into(),
                capability_id: "storage.semantic".into(),
                required_version: "^1.4.0".into(),
                input,
            })
            .map_err(|error| PluginError::new("host_call_failed", error.to_string(), false))
    }
}

impl HostCallTransport for TransferDependencyFakeHost {
    fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
        assert_eq!(capability, "storage.semantic");
        let is_upsert = input["operation"] == "upsert_artifact";
        let id = input["artifact"]["artifact_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if is_upsert {
            self.fail_selected_final_save(&input, &id)?;
        }
        let result = self.call_semantic_broker(input)?;
        if is_upsert {
            self.writes.push(id);
        }
        Ok(result)
    }
}

fn matching_element(project_root: &str, semantic_element_id: &str) -> SemanticElement {
    SemanticElement {
        project_root: project_root.into(),
        semantic_element_id: semantic_element_id.into(),
        semantic_source_id: "transfer-fixture".into(),
        path: "src/lib.rs".into(),
        element_kind: "function".into(),
        name: "shared".into(),
        parent_element_id: None,
        content_fingerprint: Some("fp1:0000000000000001:same".into()),
        start_line: None,
        end_line: None,
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({}),
    }
}

fn artifact_dependency(artifact_id: &str) -> Value {
    json!({"target": {"target_kind": "artifact", "artifact_id": artifact_id}})
}

fn selected_ids(preview: &Value) -> Vec<String> {
    preview["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|candidate| candidate["transfer_id"].as_str().unwrap().to_owned())
        .collect()
}

fn copy_id(preview: &Value, source: &str) -> String {
    preview["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["source_artifact_id"] == source)
        .unwrap()["transfer_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn assert_dependency(host: &TransferDependencyFakeHost, artifact: &str, target: &str) {
    let stored = host.artifact(artifact);
    assert_eq!(
        serde_json::to_value(stored.dependencies).unwrap(),
        json!([artifact_dependency(target)])
    );
    assert_ne!(
        stored.metadata["knowledge"]["metadata"]["inheritance"]["transfer_pending"],
        true
    );
}

#[test]
fn selected_dependencies_publish_even_when_dependent_copy_id_sorts_first() {
    let mut host = TransferDependencyFakeHost::new();
    host.seed_dependencies(false);
    let preview = host.preview();
    let copy_a = copy_id(&preview, "source-A");
    let copy_b = copy_id(&preview, "source-B");
    assert!(
        copy_a < copy_b,
        "fixture must exercise dependent-first order"
    );
    let result = host.apply(&selected_ids(&preview)).unwrap();
    assert_eq!(result["copied_artifact_ids"], json!([copy_a, copy_b]));
    assert_dependency(&host, &copy_a, &copy_b);
    assert_dependency(&host, "source-A", "source-B");
}

#[test]
fn selected_dependency_cycles_publish_with_destination_edges_and_intact_sources() {
    let mut host = TransferDependencyFakeHost::new();
    host.seed_dependencies(true);
    let preview = host.preview();
    let copy_a = copy_id(&preview, "source-A");
    let copy_b = copy_id(&preview, "source-B");
    host.apply(&selected_ids(&preview)).unwrap();
    assert_dependency(&host, &copy_a, &copy_b);
    assert_dependency(&host, &copy_b, &copy_a);
    assert_dependency(&host, "source-A", "source-B");
    assert_dependency(&host, "source-B", "source-A");
}

#[test]
fn interrupted_final_dependency_save_is_pending_and_retryable_without_recopying_completed() {
    let mut host = TransferDependencyFakeHost::new();
    host.seed_dependencies(true);
    let preview = host.preview();
    let selected = selected_ids(&preview);
    let copy_a = copy_id(&preview, "source-A");
    let copy_b = copy_id(&preview, "source-B");
    host.fail_final_artifact = Some(copy_b.clone());
    assert_eq!(
        host.apply(&selected).unwrap_err().code,
        "injected_final_write_failure"
    );
    assert_eq!(
        host.artifact(&copy_b).metadata["knowledge"]["metadata"]["inheritance"]["transfer_pending"],
        true
    );
    let after_failure = host.preview();
    let candidates = after_failure["candidates"].as_array().unwrap();
    assert_eq!(
        candidates
            .iter()
            .find(|candidate| candidate["transfer_id"] == copy_a)
            .unwrap()["already_copied"],
        true
    );
    assert_eq!(
        candidates
            .iter()
            .find(|candidate| candidate["transfer_id"] == copy_b)
            .unwrap()["already_copied"],
        false
    );
    host.writes.clear();
    let pending = candidates
        .iter()
        .filter(|candidate| candidate["already_copied"] == false)
        .map(|candidate| candidate["transfer_id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(pending, vec![copy_b.clone()]);
    let retried = host.apply(&pending).unwrap();
    assert_eq!(retried["copied_artifact_ids"], json!([copy_b]));
    assert_eq!(retried["already_copied_artifact_ids"], json!([]));
    assert!(host.writes.iter().all(|written| written == &copy_b));
    assert_dependency(&host, &copy_a, &copy_b);
    assert_dependency(&host, &copy_b, &copy_a);
}

#[test]
fn generic_destination_collision_fails_before_any_copy_write_and_preserves_existing_artifact() {
    let mut host = TransferDependencyFakeHost::new();
    host.seed_dependencies(false);
    let preview = host.preview();
    let copy_a = copy_id(&preview, "source-A");
    let collision = SemanticArtifact {
        artifact_id: copy_a.clone(),
        semantic_element_id: "original".into(),
        artifact_kind: "generic".into(),
        title: "Must survive".into(),
        content_ref: None,
        content: Some("Original content".into()),
        searchable_text: None,
        content_size_bytes: None,
        dependencies: Vec::new(),
        metadata: json!({"original": true}),
    };
    host.execute(SemanticOperation::UpsertArtifact {
        artifact: collision,
        media_type: "text/plain".into(),
    });
    let before = host.artifact(&copy_a);
    assert_eq!(
        host.apply(&selected_ids(&preview)).unwrap_err().code,
        "transfer_destination_conflict"
    );
    assert!(host.writes.is_empty());
    assert_eq!(host.artifact(&copy_a), before);
}
