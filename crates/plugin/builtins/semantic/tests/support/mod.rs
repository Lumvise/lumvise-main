mod observations;
mod source_projection;
use source_projection::SourceDescriptorCommitGate;
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use ed25519_dalek::SigningKey;
use lumvise_app_core::AppCoreHostCapabilityBroker;
use lumvise_db_core::{
    LocalPersistence, ProjectSnapshotScope, RelationalPersistence, SemanticOperation,
    SemanticPersistence, SemanticResult, StoredArtifactTextVector, StoredSemanticElementNameVector,
};
use lumvise_plugin_package::{
    BuildPackageRequest, HostCompatibility, InstalledPlugin, build_package,
    install_verified_package, verify_package,
};
use lumvise_plugin_runtime::{
    HostCapabilityBroker, HostCapabilityError, HostCapabilityRequest, PluginInvocationClass,
    PluginInvocationContext, PluginInvocationRequest, PluginRuntimeConfig, PluginRuntimeError,
    PluginSandbox, PluginSandboxError, PluginSandboxRequest, PluginSystem,
};
use lumvise_resource_routing::InvocationControl;
use sha2::{Digest, Sha256};

use lumvise_plugin_semantic::{PACKAGE_PROTOCOL_VERSION, package_manifest_source};

pub trait ControlledTestInvoke {
    fn invoke(
        &self,
        plugin_id: &str,
        export_id: &str,
        input: serde_json::Value,
    ) -> Result<lumvise_plugin_protocol::WireOutcome, PluginRuntimeError>;
}

impl ControlledTestInvoke for PluginSystem {
    fn invoke(
        &self,
        plugin_id: &str,
        export_id: &str,
        input: serde_json::Value,
    ) -> Result<lumvise_plugin_protocol::WireOutcome, PluginRuntimeError> {
        let context = PluginInvocationContext::new(
            format!("semantic-test-{export_id}"),
            "semantic-test-owner",
            PluginInvocationClass::Foreground,
            std::time::Instant::now() + Duration::from_secs(3),
        );
        self.invoke_controlled(PluginInvocationRequest::new(
            plugin_id, export_id, input, context,
        ))
        .map_err(|error| error.into_runtime_error())
    }
}

pub struct TestSubprocessSandbox;

pub struct MemoryCapabilityBroker {
    persistence: Arc<LocalPersistence>,
    semantic: AppCoreHostCapabilityBroker,
    fail_next_commit: Mutex<bool>,
    source_commit_gate: Mutex<Option<SourceDescriptorCommitGate>>,
    pub embed_available: Mutex<bool>,
    embed_batch_sizes: Mutex<Vec<usize>>,
    stored_element_name_vectors:
        Mutex<HashMap<String, BTreeMap<String, StoredSemanticElementNameVector>>>,
    stored_artifact_text_vectors: Mutex<HashMap<String, StoredArtifactTextVector>>,
    continuation_calls: Mutex<usize>,
    unprefixed_element_lists: Mutex<usize>,
    full_relationship_or_artifact_scans: Mutex<usize>,
    project_snapshot_calls: AtomicUsize,
    semantic_operation_counts: Mutex<BTreeMap<String, usize>>,
    renderer_graph_calls: AtomicUsize,
}

impl Default for MemoryCapabilityBroker {
    fn default() -> Self {
        let persistence =
            Arc::new(LocalPersistence::in_memory().expect("in-memory local persistence"));
        Self::sharing(persistence)
    }
}

impl MemoryCapabilityBroker {
    fn sharing(persistence: Arc<LocalPersistence>) -> Self {
        let semantic: Arc<dyn SemanticPersistence> = persistence.clone();
        let relational: Arc<dyn RelationalPersistence> = persistence.clone();
        Self {
            persistence,
            semantic: AppCoreHostCapabilityBroker::new(semantic, relational),
            fail_next_commit: Mutex::default(),
            source_commit_gate: Mutex::default(),
            embed_available: Mutex::default(),
            embed_batch_sizes: Mutex::default(),
            stored_element_name_vectors: Mutex::default(),
            stored_artifact_text_vectors: Mutex::default(),
            continuation_calls: Mutex::default(),
            unprefixed_element_lists: Mutex::default(),
            full_relationship_or_artifact_scans: Mutex::default(),
            project_snapshot_calls: AtomicUsize::default(),
            semantic_operation_counts: Mutex::default(),
            renderer_graph_calls: AtomicUsize::default(),
        }
    }
}

impl MemoryCapabilityBroker {
    pub fn fail_next_commit(&self) {
        *self.fail_next_commit.lock().expect("commit failure switch") = true;
    }
    pub fn row_count(&self, plugin_id: &str, table: &str) -> usize {
        self.plugin_rows(plugin_id, table).len()
    }

    fn plugin_rows(&self, plugin: &str, table: &str) -> Vec<serde_json::Value> {
        self.read_plugin_page(plugin, serde_json::json!({"operation":"ensure_table", "table_name":table, "schema":{"type":"object"}}));
        let (mut records, mut after) = (Vec::new(), serde_json::Value::Null);
        loop {
            let page = self.read_plugin_page(
                plugin,
                serde_json::json!({"operation":"list_rows",
                "table_name":table, "key_prefix":null, "limit":500, "after_key":after}),
            );
            records.extend(
                page["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| row["value"].clone()),
            );
            after = page["next_after_key"].clone();
            if after.is_null() {
                return records;
            }
        }
    }

    fn read_plugin_page(&self, plugin: &str, input: serde_json::Value) -> serde_json::Value {
        self.semantic
            .invoke(HostCapabilityRequest {
                plugin_id: plugin.into(),
                invocation_id: "fixture-storage-read".into(),
                call_id: "fixture-storage-read".into(),
                capability_id: "storage.plugin".into(),
                required_version: "^1.0".into(),
                input,
            })
            .expect("portable relational fixture read")
    }

    pub fn reset_unprefixed_element_lists(&self) {
        *self
            .unprefixed_element_lists
            .lock()
            .expect("element list counter") = 0;
    }

    pub fn unprefixed_element_lists(&self) -> usize {
        *self
            .unprefixed_element_lists
            .lock()
            .expect("element list counter")
    }

    pub fn full_context_scans(&self) -> usize {
        *self
            .full_relationship_or_artifact_scans
            .lock()
            .expect("context scan counter")
    }

    pub fn reset_project_snapshot_calls(&self) {
        self.project_snapshot_calls.store(0, Ordering::Relaxed);
    }

    pub fn project_snapshot_calls(&self) -> usize {
        self.project_snapshot_calls.load(Ordering::Relaxed)
    }

    pub fn reset_semantic_operation_counts(&self) {
        self.semantic_operation_counts
            .lock()
            .expect("operation counts")
            .clear();
    }

    pub fn semantic_operation_count(&self, operation: &str) -> usize {
        self.semantic_operation_counts
            .lock()
            .expect("operation counts")
            .get(operation)
            .copied()
            .unwrap_or_default()
    }
    pub fn reset_renderer_graph_calls(&self) {
        self.renderer_graph_calls.store(0, Ordering::Relaxed);
    }

    pub fn renderer_graph_calls(&self) -> usize {
        self.renderer_graph_calls.load(Ordering::Relaxed)
    }

    pub fn artifact_text_vector_engine(&self, artifact_id: &str) -> Option<String> {
        self.stored_artifact_text_vectors
            .lock()
            .expect("stored artifact text vectors")
            .get(artifact_id)
            .map(|stored| stored.vector.engine_id.clone())
    }

    pub fn element_name_vector_engine(&self, semantic_element_id: &str) -> Option<String> {
        self.stored_element_name_vectors
            .lock()
            .expect("stored element name vectors")
            .values()
            .find_map(|vectors| {
                vectors
                    .get(semantic_element_id)
                    .map(|stored| stored.vector.engine_id.clone())
            })
    }

    pub fn semantic_counts(&self, project_root: &str) -> (usize, usize) {
        let result = SemanticPersistence::execute(
            self.persistence.as_ref(),
            SemanticOperation::ProjectSnapshot {
                scope: ProjectSnapshotScope::ProjectRoot(project_root.to_owned()),
                artifact_namespace: None,
            },
            &InvocationControl::sixty_seconds(),
        );
        match result {
            Ok(SemanticResult::ProjectSnapshot(snapshot)) => {
                (snapshot.elements.len(), snapshot.relationships.len())
            }
            Ok(other) => {
                panic!("expected ProjectSnapshot result for semantic count query, got {other:?}")
            }
            Err(error) => {
                panic!("expected ProjectSnapshot semantic count query to succeed: {error}")
            }
        }
    }

    pub fn embed_batch_sizes(&self) -> Vec<usize> {
        self.embed_batch_sizes
            .lock()
            .expect("embed batch sizes")
            .clone()
    }

    pub fn element_name_vectors(&self, project_root: &str) -> Vec<StoredSemanticElementNameVector> {
        self.stored_element_name_vectors
            .lock()
            .expect("stored element name vectors")
            .get(project_root)
            .map(|vectors| vectors.values().cloned().collect())
            .unwrap_or_default()
    }

    fn invoke_store_element_name_vectors(
        &self,
        request: HostCapabilityRequest,
    ) -> Result<serde_json::Value, HostCapabilityError> {
        let project_root = request.input["project_root"]
            .as_str()
            .ok_or_else(|| host_error("storage.semantic", "invalid_input"))?
            .to_owned();
        let vectors: Vec<StoredSemanticElementNameVector> =
            serde_json::from_value(request.input["vectors"].clone())
                .map_err(|_| host_error("storage.semantic", "invalid_input"))?;
        let mut previous = Vec::with_capacity(vectors.len());
        {
            let mut stored = self
                .stored_element_name_vectors
                .lock()
                .expect("stored element name vectors");
            let project_vectors = stored.entry(project_root.clone()).or_default();
            for vector in vectors {
                let semantic_element_id = vector.semantic_element_id.clone();
                previous.push((
                    semantic_element_id.clone(),
                    project_vectors.insert(semantic_element_id, vector),
                ));
            }
        }
        let result = self.semantic.invoke(request);
        if result.is_err() {
            let mut stored = self
                .stored_element_name_vectors
                .lock()
                .expect("stored element name vectors");
            if let Some(project_vectors) = stored.get_mut(&project_root) {
                for (semantic_element_id, prior) in previous.into_iter().rev() {
                    if let Some(prior) = prior {
                        project_vectors.insert(semantic_element_id, prior);
                    } else {
                        project_vectors.remove(&semantic_element_id);
                    }
                }
                if project_vectors.is_empty() {
                    stored.remove(&project_root);
                }
            }
        }
        result
    }

    fn invoke_store_artifact_text_vectors(
        &self,
        request: HostCapabilityRequest,
    ) -> Result<serde_json::Value, HostCapabilityError> {
        let vectors: Vec<StoredArtifactTextVector> =
            serde_json::from_value(request.input["vectors"].clone())
                .map_err(|_| host_error("storage.semantic", "invalid_input"))?;
        let mut previous = Vec::with_capacity(vectors.len());
        {
            let mut stored = self
                .stored_artifact_text_vectors
                .lock()
                .expect("stored artifact text vectors");
            for vector in vectors {
                let artifact_id = vector.artifact_id.clone();
                previous.push((artifact_id.clone(), stored.insert(artifact_id, vector)));
            }
        }
        let result = self.semantic.invoke(request);
        if result.is_err() {
            let mut stored = self
                .stored_artifact_text_vectors
                .lock()
                .expect("stored artifact text vectors");
            for (artifact_id, prior) in previous.into_iter().rev() {
                if let Some(prior) = prior {
                    stored.insert(artifact_id, prior);
                } else {
                    stored.remove(&artifact_id);
                }
            }
        }
        result
    }
}

impl HostCapabilityBroker for MemoryCapabilityBroker {
    fn invoke(
        &self,
        request: HostCapabilityRequest,
    ) -> Result<serde_json::Value, HostCapabilityError> {
        match request.capability_id.as_str() {
            "storage.plugin" => self.invoke_storage(request),
            "storage.semantic" => {
                if let Some(operation) = request.input["operation"].as_str() {
                    *self
                        .semantic_operation_counts
                        .lock()
                        .expect("operation counts")
                        .entry(operation.to_owned())
                        .or_default() += 1;
                }
                if request.input["operation"] == "elements_for_project" {
                    *self
                        .unprefixed_element_lists
                        .lock()
                        .expect("element list counter") += 1;
                }
                if request.input["operation"] == "project_snapshot" {
                    self.project_snapshot_calls.fetch_add(1, Ordering::Relaxed);
                }
                if request.input["operation"] == "project_renderer_graph" {
                    self.renderer_graph_calls.fetch_add(1, Ordering::Relaxed);
                }
                let mut fail = self.fail_next_commit.lock().expect("fail switch");
                if std::mem::take(&mut *fail) {
                    return Err(host_error("storage.semantic", "injected_commit_failure"));
                }
                if request.input["operation"] == "store_element_name_vectors" {
                    return self.invoke_store_element_name_vectors(request);
                }
                if request.input["operation"] == "store_artifact_text_vectors" {
                    return self.invoke_store_artifact_text_vectors(request);
                }
                self.semantic.invoke(request)
            }
            "semantic.snapshot" | "project.source" => self.semantic.invoke(request),
            "neural.embed" => self.invoke_embed(request),
            other => Err(host_error(other, "unsupported_capability")),
        }
    }
}

impl MemoryCapabilityBroker {
    fn invoke_storage(
        &self,
        mut request: HostCapabilityRequest,
    ) -> Result<serde_json::Value, HostCapabilityError> {
        if request.input["operation"] == "commit_mutations" {
            let gate = self.source_commit_gate.lock().unwrap().take();
            if let Some(gate) = gate {
                gate.pause()?;
            }
        }
        if request.input["operation"] == "list_rows" {
            self.observe_row_page(&request.input);
            request.input["limit"] =
                serde_json::json!(request.input["limit"].as_u64().unwrap_or(500).min(2));
        }
        self.semantic.invoke(request)
    }

    fn observe_row_page(&self, input: &serde_json::Value) {
        let table = input["table_name"].as_str().unwrap_or_default();
        let prefix = input["key_prefix"].as_str().unwrap_or_default();
        if table == "semantic_elements" && prefix.is_empty() {
            *self.unprefixed_element_lists.lock().unwrap() += 1;
        }
        if matches!(table, "semantic_relationships" | "semantic_artifacts") && prefix.ends_with(':')
        {
            *self.full_relationship_or_artifact_scans.lock().unwrap() += 1;
        }
        if input["after_key"].is_string() {
            *self.continuation_calls.lock().unwrap() += 1;
        }
    }

    fn invoke_embed(
        &self,
        request: HostCapabilityRequest,
    ) -> Result<serde_json::Value, HostCapabilityError> {
        if !*self.embed_available.lock().expect("embed switch") {
            return Err(host_error("neural.embed", "provider_unavailable"));
        }
        let texts = request.input["texts"]
            .as_array()
            .ok_or_else(|| host_error("neural.embed", "invalid_input"))?;
        self.embed_batch_sizes
            .lock()
            .expect("embed batch sizes")
            .push(texts.len());
        if texts.len() > 128 {
            return Err(host_error("neural.embed", "too_many_texts"));
        }
        let vectors = texts
            .iter()
            .map(|value| {
                let text = value.as_str().unwrap_or_default().to_ascii_lowercase();
                if let Some(index) = text
                    .strip_prefix("batch-element-")
                    .and_then(|value| value.parse::<usize>().ok())
                {
                    return vec![index as f32, 1.0, 0.0, 0.0];
                }
                let tokens = ["parser", "render", "graph", "semantic"];
                tokens
                    .iter()
                    .map(|token| if text.contains(token) { 1.0 } else { 0.0 })
                    .collect::<Vec<f32>>()
            })
            .collect::<Vec<_>>();
        Ok(
            serde_json::json!({"vectors": vectors, "engine_id": "test-fixture-embed", "model": "test-model"}),
        )
    }
}

impl PluginSandbox for TestSubprocessSandbox {
    fn prepare_command(
        &self,
        request: PluginSandboxRequest<'_>,
    ) -> Result<Command, PluginSandboxError> {
        Ok(Command::new(request.executable))
    }
}

pub fn signed_install(workspace: &Path, signing_key: &SigningKey) -> (PathBuf, InstalledPlugin) {
    const TARGET: &str = "semantic-integration-host";
    let executable = std::fs::read(env!("CARGO_BIN_EXE_lumvise-plugin-semantic"))
        .expect("read real Semantic binary");
    let digest = hex::encode(Sha256::digest(&executable));
    let manifest = package_manifest_source(TARGET, &digest);
    let executable_path = manifest.targets[TARGET].clone();
    let archive = workspace.join("builtin.semantic.lvp");
    build_package(
        &archive,
        BuildPackageRequest {
            manifest,
            files: BTreeMap::from([(executable_path, executable)]),
            signing_key,
        },
    )
    .expect("build Semantic package");
    let verified = verify_package(
        &archive,
        &signing_key.verifying_key(),
        &HostCompatibility::new(PACKAGE_PROTOCOL_VERSION, TARGET),
    )
    .expect("verify Semantic package");
    let installed = install_verified_package(&verified, &workspace.join("installed"))
        .expect("install Semantic package");
    (archive, installed)
}

pub fn system(broker: Arc<MemoryCapabilityBroker>) -> PluginSystem {
    PluginSystem::with_broker_and_sandbox(
        PluginRuntimeConfig::default().with_controlled_test_deadline(Duration::from_secs(5)),
        broker,
        Arc::new(TestSubprocessSandbox),
    )
}

fn host_error(capability_id: &str, code: &str) -> HostCapabilityError {
    HostCapabilityError::new(
        capability_id,
        code,
        format!("host capability `{capability_id}` failed with `{code}`"),
        false,
    )
}
