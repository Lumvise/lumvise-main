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
    rows: Mutex<HashMap<(String, String), BTreeMap<String, serde_json::Value>>>,
    transactions: Mutex<HashMap<(String, String), Vec<serde_json::Value>>>,
    next_transaction: AtomicUsize,
    fail_next_commit: Mutex<bool>,
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
        let semantic: Arc<dyn SemanticPersistence> = persistence.clone();
        let relational: Arc<dyn RelationalPersistence> = persistence.clone();
        Self {
            persistence,
            semantic: AppCoreHostCapabilityBroker::new(semantic, relational),
            rows: Mutex::default(),
            transactions: Mutex::default(),
            next_transaction: AtomicUsize::default(),
            fail_next_commit: Mutex::default(),
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
        self.rows
            .lock()
            .expect("memory broker rows")
            .get(&(plugin_id.into(), table.into()))
            .map_or(0, BTreeMap::len)
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
        request: HostCapabilityRequest,
    ) -> Result<serde_json::Value, HostCapabilityError> {
        let operation = request.input["operation"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if operation == "begin_mutations" {
            let id = self.next_transaction.fetch_add(1, Ordering::Relaxed);
            let transaction_id = format!("test-transaction-{id}");
            self.transactions
                .lock()
                .expect("transactions")
                .insert((request.plugin_id, transaction_id.clone()), Vec::new());
            return Ok(serde_json::json!({"transaction_id": transaction_id}));
        }
        if matches!(
            operation.as_str(),
            "stage_mutations" | "commit_mutations" | "abort_mutations"
        ) {
            return self.invoke_transaction(request, &operation);
        }
        let table = request.input["table_name"].as_str().unwrap_or_default();
        let key = (request.plugin_id, table.to_owned());
        let mut tables = self.rows.lock().expect("memory broker rows");
        let rows = tables.entry(key).or_default();
        match operation.as_str() {
            "ensure_table" => Ok(serde_json::json!({"table_name": table})),
            "put_row" => {
                let row_key = request.input["row_key"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                let value = request.input["value"].clone();
                rows.insert(row_key.clone(), value.clone());
                Ok(serde_json::json!({"row_key": row_key, "value": value}))
            }
            "get_row" => {
                let row_key = request.input["row_key"].as_str().unwrap_or_default();
                let row = rows
                    .get(row_key)
                    .map(|value| serde_json::json!({"row_key": row_key, "value": value}));
                Ok(serde_json::json!({"row": row}))
            }
            "list_rows" => {
                let prefix = request.input["key_prefix"].as_str().unwrap_or_default();
                if table == "semantic_elements" && prefix.is_empty() {
                    *self
                        .unprefixed_element_lists
                        .lock()
                        .expect("element list counter") += 1;
                }
                if matches!(table, "semantic_relationships" | "semantic_artifacts")
                    && prefix.ends_with(':')
                {
                    *self
                        .full_relationship_or_artifact_scans
                        .lock()
                        .expect("context scan counter") += 1;
                }
                let after = request.input["after_key"].as_str();
                if after.is_some() {
                    *self.continuation_calls.lock().expect("continuation calls") += 1;
                }
                let requested = request.input["limit"].as_u64().unwrap_or(500) as usize;
                let limit = requested.min(2);
                let mut matched = rows
                    .iter()
                    .filter(|(row_key, _)| row_key.starts_with(prefix))
                    .filter(|(row_key, _)| after.is_none_or(|cursor| row_key.as_str() > cursor))
                    .take(limit + 1)
                    .collect::<Vec<_>>();
                let has_more = matched.len() > limit;
                matched.truncate(limit);
                let next = has_more.then(|| matched.last().expect("nonempty page").0.clone());
                Ok(serde_json::json!({
                    "rows": matched.into_iter().map(|(row_key, value)|
                        serde_json::json!({"row_key": row_key, "value": value})).collect::<Vec<_>>(),
                    "next_after_key": next
                }))
            }
            _ => Err(host_error("storage.plugin", "invalid_operation")),
        }
    }

    fn invoke_transaction(
        &self,
        request: HostCapabilityRequest,
        operation: &str,
    ) -> Result<serde_json::Value, HostCapabilityError> {
        let transaction_id = request.input["transaction_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let key = (request.plugin_id.clone(), transaction_id);
        if operation == "stage_mutations" {
            let staged = request.input["mutations"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            self.transactions
                .lock()
                .expect("transactions")
                .get_mut(&key)
                .ok_or_else(|| host_error("storage.plugin", "unknown_transaction"))?
                .extend(staged);
            return Ok(serde_json::json!({"staged": true}));
        }
        let mutations = self
            .transactions
            .lock()
            .expect("transactions")
            .remove(&key)
            .ok_or_else(|| host_error("storage.plugin", "unknown_transaction"))?;
        if operation == "abort_mutations" {
            return Ok(serde_json::json!({"aborted": true}));
        }
        let mut fail = self.fail_next_commit.lock().expect("commit failure switch");
        if std::mem::take(&mut *fail) {
            return Err(host_error("storage.plugin", "injected_commit_failure"));
        }
        let mut tables = self.rows.lock().expect("memory broker rows");
        for mutation in mutations {
            let table = mutation["table_name"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let row_key = mutation["row_key"].as_str().unwrap_or_default().to_owned();
            let rows = tables
                .entry((request.plugin_id.clone(), table))
                .or_default();
            match mutation["operation"].as_str().unwrap_or_default() {
                "put" => {
                    rows.insert(row_key, mutation["value"].clone());
                }
                "delete" => {
                    rows.remove(&row_key);
                }
                _ => return Err(host_error("storage.plugin", "invalid_mutation")),
            }
        }
        Ok(serde_json::json!({"committed": true}))
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
