#[path = "knowledge_protocol/lifecycle.rs"]
mod lifecycle;
#[path = "knowledge_protocol/manifest_process.rs"]
mod manifest_process;
#[path = "knowledge_protocol/transfer.rs"]
mod transfer;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, Mutex},
};

use ed25519_dalek::SigningKey;
use lumvise_app_core::AppCoreHostCapabilityBroker;
use lumvise_db_core::{
    LocalPersistence, ProjectSnapshotScope, RelationalPersistence, SemanticArtifact,
    SemanticElement, SemanticOperation, SemanticPersistence, SemanticResult,
};
use lumvise_plugin_knowledge::{
    APPLY_TRANSFER_EXPORT_ID, ARTIFACT_GENERATION_POLL_EXPORT_ID,
    ASSISTANT_FIND_ELEMENTS_EXPORT_ID, ASSISTANT_GET_ELEMENT_EXPORT_ID, ASSISTANT_GET_EXPORT_ID,
    ASSISTANT_LIST_EXPORT_ID, ASSISTANT_SEARCH_EXPORT_ID, CREATE_EXPORT_ID, DEBUG_C4_EXPORT_ID,
    DELETE_EXPORT_ID, ELEMENT_TRIGGER_EXPORT_ID, ENSURE_C4_EXPORT_ID, FIND_ELEMENTS_EXPORT_ID,
    GET_CULTIVATION_RUN_EXPORT_ID, GET_ELEMENT_EXPORT_ID, GET_EXPORT_ID, HTTP_C4_ACTION_EXPORT_ID,
    HTTP_C4_DEBUG_EXPORT_ID, HTTP_C4_EXPORT_ID, HTTP_CREATE_ARTIFACT_EXPORT_ID,
    HTTP_DELETE_ARTIFACT_EXPORT_ID, HTTP_EVENTS_EXPORT_ID, HTTP_EXPORT_EXPORT_ID,
    HTTP_MANIFEST_EXPORT_ID, HTTP_PAGE_EXPORT_ID, HTTP_PROJECTION_ARTIFACTS_EXPORT_ID,
    HTTP_RESOLVE_TARGET_EXPORT_ID, HTTP_SETUP_EXPORT_ID, HTTP_SYNC_EXPORT_ID,
    HTTP_UPDATE_ARTIFACT_EXPORT_ID, HTTP_WRITE_EXPORT_ID, LIST_EXPORT_ID, MANIFEST_EXPORT_ID,
    PACKAGE_PROTOCOL_VERSION, PLUGIN_ID, PREVIEW_TRANSFER_EXPORT_ID, PROJECTION_EXPORT_ID,
    REBUILD_EXPORT_ID, RUN_CULTIVATION_EXPORT_ID, SEARCH_EXPORT_ID, UPDATE_EXPORT_ID,
    package_manifest_source,
};
use lumvise_plugin_package::{
    BuildPackageRequest, ExecutionMode, ExportDescriptor, ExportSurface, HostCompatibility,
    HttpStreamMode, InstalledPlugin, PluginManifest, ProtocolRange, PublisherIdentity,
    build_package, install_verified_package, verify_package,
};
use lumvise_plugin_protocol::{
    CURRENT_PROTOCOL_VERSION, FrameCodec, MessageBody, WireMessage, WireOutcome,
};
use lumvise_plugin_runtime::{
    HostCapabilityBroker, HostCapabilityError, HostCapabilityRequest, PluginSandbox,
    PluginSandboxError, PluginSandboxRequest, PluginSystem,
};
use lumvise_resource_routing::InvocationControl;
use sha2::{Digest, Sha256};

#[path = "knowledge_protocol/canonical_identity.rs"]
mod canonical_identity;
#[path = "knowledge_protocol/documents.rs"]
mod documents;
#[path = "knowledge_protocol/retired_kind_migration.rs"]
mod retired_kind_migration;
#[path = "knowledge_protocol/search_cache.rs"]
mod search_cache;
mod support;

use support::{ControlledTestInvoke, fast_runtime_config, make_tree_writable};

struct KnowledgePluginProcess {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    codec: FrameCodec,
}

struct TestSubprocessSandbox;

struct MemoryStorageBroker {
    persistence: Arc<LocalPersistence>,
    semantic: AppCoreHostCapabilityBroker,
    preserved_inactive_owner_ids: Mutex<HashSet<String>>,
    rows: Mutex<HashMap<(String, String), BTreeMap<String, serde_json::Value>>>,
    semantic_operations: Mutex<BTreeMap<String, usize>>,
    vector_write_sizes: Mutex<Vec<usize>>,
    vector_reads: Mutex<Vec<(String, String)>>,
    embedding_vector: Mutex<Option<Vec<f32>>>,
    embedding_inputs: Mutex<Vec<Vec<String>>>,
    plugin_invoke_operations: Mutex<BTreeMap<String, usize>>,
    failure: Mutex<Option<(String, String)>>,
    canonical_artifact_id: Mutex<Option<String>>,
    operation_fault: Mutex<Option<(String, usize)>>,
}

impl Default for MemoryStorageBroker {
    fn default() -> Self {
        let persistence =
            Arc::new(LocalPersistence::in_memory().expect("in-memory local persistence"));
        let semantic: Arc<dyn SemanticPersistence> = persistence.clone();
        let relational: Arc<dyn RelationalPersistence> = persistence.clone();
        Self {
            persistence,
            semantic: AppCoreHostCapabilityBroker::new(semantic, relational),
            preserved_inactive_owner_ids: Mutex::default(),
            rows: Mutex::default(),
            semantic_operations: Mutex::default(),
            vector_write_sizes: Mutex::default(),
            vector_reads: Mutex::default(),
            embedding_vector: Mutex::default(),
            embedding_inputs: Mutex::default(),
            plugin_invoke_operations: Mutex::default(),
            failure: Mutex::default(),
            canonical_artifact_id: Mutex::default(),
            operation_fault: Mutex::default(),
        }
    }
}

impl HostCapabilityBroker for MemoryStorageBroker {
    fn invoke(
        &self,
        request: HostCapabilityRequest,
    ) -> Result<serde_json::Value, HostCapabilityError> {
        if let Some((_, code)) = self
            .failure
            .lock()
            .expect("host capability failure")
            .as_ref()
            .filter(|(id, _)| id == &request.capability_id)
        {
            return Err(host_error(&request.capability_id, code));
        }
        match request.capability_id.as_str() {
            "storage.plugin" => self.storage(request),
            "storage.semantic" => self.semantic_storage(request),
            "fixture.semantic.ingest" => documents::ingest(self, request.input),
            "plugin.invoke" => self.plugin_invoke(request),
            "neural.embed" => self.embedding_response(&request.input),
            "runtime.background_job" => Ok(serde_json::json!({
                "job_id": "knowledge-job-1", "status": "accepted"
            })),
            other => Err(host_error(other, "unsupported_capability")),
        }
    }
}

impl MemoryStorageBroker {
    fn embedding_response(
        &self,
        input: &serde_json::Value,
    ) -> Result<serde_json::Value, HostCapabilityError> {
        let texts = input["texts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|text| text.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        if texts.iter().any(|text| text.len() > 32 * 1024)
            || texts.iter().map(String::len).sum::<usize>() > 256 * 1024
        {
            return Err(host_error("neural.embed", "invalid_embedding_input"));
        }
        self.embedding_inputs.lock().unwrap().push(texts);
        let Some(vector) = self.embedding_vector.lock().unwrap().clone() else {
            return Ok(embed_response(input));
        };
        let count = input["texts"].as_array().map_or(0, Vec::len);
        Ok(serde_json::json!({"vectors": vec![vector; count]}))
    }

    fn semantic_storage(
        &self,
        mut request: HostCapabilityRequest,
    ) -> Result<serde_json::Value, HostCapabilityError> {
        let operation = request.input["operation"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        self.take_operation_fault(&operation)?;
        *self
            .semantic_operations
            .lock()
            .expect("semantic operation counts")
            .entry(operation)
            .or_default() += 1;
        if request.input["operation"] == "upsert_artifact"
            && request.input["artifact"]["metadata"]["knowledge"].is_object()
        {
            let owner = request.input["artifact"]["semantic_element_id"]
                .as_str()
                .unwrap_or_default();
            let project_root = request.input["artifact"]["metadata"]["knowledge"]["project_root"]
                .as_str()
                .unwrap_or("/work/demo");
            self.ensure_element(owner, project_root);
            if let Some(id) = self.canonical_artifact_id.lock().unwrap().as_ref() {
                request.input["artifact"]["artifact_id"] = serde_json::json!(id);
            }
        }
        self.semantic.invoke(request)
    }

    fn semantic_operation_count(&self, operation: &str) -> usize {
        self.semantic_operations
            .lock()
            .expect("semantic operation counts")
            .get(operation)
            .copied()
            .unwrap_or_default()
    }

    fn plugin_invoke(
        &self,
        request: HostCapabilityRequest,
    ) -> Result<serde_json::Value, HostCapabilityError> {
        let export_id = request.input["export_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        *self
            .plugin_invoke_operations
            .lock()
            .expect("plugin.invoke operation counts")
            .entry(export_id.clone())
            .or_default() += 1;
        self.take_operation_fault(&export_id)?;
        if request.input["plugin_id"] == "builtin.semantic"
            && request.input["export_id"] == "ingest_index_batch"
        {
            return documents::ingest(self, request.input["input"].clone());
        }
        Ok(semantic_response(&request.input))
    }

    fn plugin_invoke_count(&self, export_id: &str) -> usize {
        self.plugin_invoke_operations
            .lock()
            .expect("plugin.invoke operation counts")
            .get(export_id)
            .copied()
            .unwrap_or_default()
    }

    /// Fails the next call of one operation once, after `successes` calls. Example: `fail_operation_after("upsert_artifact", 0)`.
    fn fail_operation_after(&self, operation: &str, successes: usize) {
        *self.operation_fault.lock().expect("operation fault") =
            Some((operation.into(), successes));
    }

    fn take_operation_fault(&self, operation: &str) -> Result<(), HostCapabilityError> {
        let mut fault = self.operation_fault.lock().expect("operation fault");
        let Some((name, remaining)) = fault.as_mut() else {
            return Ok(());
        };
        if name.as_str() != operation {
            return Ok(());
        }
        if *remaining == 0 {
            *fault = None;
            return Err(host_error(operation, "injected_write_failure"));
        }
        *remaining -= 1;
        Ok(())
    }

    fn row_count(&self, plugin_id: &str, table: &str) -> usize {
        self.rows
            .lock()
            .expect("memory broker rows")
            .get(&(plugin_id.to_owned(), table.to_owned()))
            .map_or(0, BTreeMap::len)
    }
    fn active_project_structure(
        &self,
        project_root: &str,
    ) -> (
        Vec<SemanticElement>,
        Vec<lumvise_db_core::SemanticRelationship>,
    ) {
        let result = SemanticPersistence::execute(
            self.persistence.as_ref(),
            SemanticOperation::ProjectSnapshot {
                scope: ProjectSnapshotScope::ProjectRoot(project_root.to_owned()),
                artifact_namespace: None,
            },
            &InvocationControl::sixty_seconds(),
        );
        match result {
            Ok(SemanticResult::ProjectSnapshot(snapshot)) => (
                snapshot
                    .elements
                    .into_iter()
                    .filter(|element| element.lifecycle != "inactive")
                    .collect(),
                snapshot.relationships,
            ),
            Ok(other) => panic!(
                "expected ProjectSnapshot result while loading active project structure, got {other:?}"
            ),
            Err(error) => {
                panic!("expected ProjectSnapshot operation to succeed while seeding: {error}")
            }
        }
    }

    fn sync_elements(&self, project_root: &str, incoming: Vec<SemanticElement>) {
        let (mut elements, relationships) = self.active_project_structure(project_root);
        for element in incoming {
            if let Some(existing) = elements
                .iter_mut()
                .find(|existing| existing.semantic_element_id == element.semantic_element_id)
            {
                *existing = element;
            } else {
                elements.push(element);
            }
        }
        let result = SemanticPersistence::execute(
            self.persistence.as_ref(),
            SemanticOperation::SyncStructure {
                project_root: project_root.to_owned(),
                elements,
                relationships,
            },
            &InvocationControl::sixty_seconds(),
        );
        match result {
            Ok(SemanticResult::SyncStructure(_)) => {}
            Ok(other) => panic!(
                "expected SyncStructure result while seeding semantic elements, got {other:?}"
            ),
            Err(error) => {
                panic!("expected SyncStructure operation to succeed while seeding: {error}")
            }
        }
    }

    fn ensure_element(&self, semantic_element_id: &str, project_root: &str) {
        let result = SemanticPersistence::execute(
            self.persistence.as_ref(),
            SemanticOperation::ElementsByIdsIncludingInactive {
                project_root: project_root.to_owned(),
                semantic_element_ids: HashSet::from([semantic_element_id.to_owned()]),
            },
            &InvocationControl::sixty_seconds(),
        );
        match result {
            Ok(SemanticResult::Elements(elements))
                if elements.first().is_some_and(|element| {
                    element.lifecycle != "inactive"
                        || self
                            .preserved_inactive_owner_ids
                            .lock()
                            .expect("preserved inactive owners")
                            .contains(semantic_element_id)
                }) =>
            {
                return;
            }
            Ok(SemanticResult::Elements(_)) => {}
            Ok(other) => {
                panic!("expected targeted elements while checking seed owner, got {other:?}")
            }
            Err(error) => {
                panic!(
                    "expected targeted elements operation to succeed while checking seed owner: {error}"
                )
            }
        }
        self.sync_elements(
            project_root,
            vec![test_element(semantic_element_id, project_root)],
        );
    }

    fn upsert_artifact(&self, artifact: SemanticArtifact, media_type: &str) {
        let expected_artifact_id = artifact.artifact_id.clone();
        let result = SemanticPersistence::execute(
            self.persistence.as_ref(),
            SemanticOperation::UpsertArtifact {
                artifact,
                media_type: media_type.to_owned(),
            },
            &InvocationControl::sixty_seconds(),
        );
        match result {
            Ok(SemanticResult::UpsertedArtifact { artifact_id })
                if artifact_id == expected_artifact_id => {}
            Ok(SemanticResult::UpsertedArtifact { artifact_id }) => panic!(
                "expected UpsertedArtifact for `{expected_artifact_id}`, got `{artifact_id}`"
            ),
            Ok(other) => {
                panic!("expected UpsertedArtifact result while seeding artifact, got {other:?}")
            }
            Err(error) => {
                panic!("expected UpsertArtifact operation to succeed while seeding: {error}")
            }
        }
    }

    fn put_knowledge(&self, artifact_id: &str, semantic_element_id: &str, title: &str) {
        self.ensure_element(semantic_element_id, "/work/demo");
        let mut artifact = test_artifact(artifact_id, semantic_element_id, title);
        artifact.content = Some(title.to_owned());
        self.upsert_artifact(artifact, "text/markdown");
    }

    fn seed_projection_graph(&self) {
        let mut file = test_element("file:a", "/project");
        file.path = "src/a.rs".into();
        file.element_kind = "file".into();
        file.name = "a.rs".into();
        let mut child = test_element("element-a", "/project");
        child.path = "src/a.rs".into();
        child.name = "element_a".into();
        child.parent_element_id = Some("file:a".into());
        self.sync_elements("/project", vec![file, child]);
    }

    fn put_unrelated_graph_artifact(&self, artifact_id: &str, semantic_element_id: &str) {
        self.ensure_element(semantic_element_id, "/project");
        self.upsert_artifact(
            SemanticArtifact {
                artifact_id: artifact_id.into(),
                semantic_element_id: semantic_element_id.into(),
                artifact_kind: "knowledge".into(),
                title: "Retired untyped artifact".into(),
                content_ref: None,
                content: Some("legacy".into()),
                searchable_text: None,
                content_size_bytes: None,
                metadata: serde_json::json!({}),
                dependencies: vec![],
            },
            "text/plain",
        );
    }

    fn deny(&self, capability_id: &str) {
        self.fail(capability_id, "permission_denied");
    }

    fn fail(&self, capability_id: &str, code: &str) {
        *self.failure.lock().expect("host capability failure") =
            Some((capability_id.into(), code.into()));
    }

    fn allow_all(&self) {
        *self.failure.lock().expect("host capability failure") = None;
    }

    fn storage(
        &self,
        request: HostCapabilityRequest,
    ) -> Result<serde_json::Value, HostCapabilityError> {
        let operation = request.input["operation"].as_str().unwrap_or_default();
        let table = request.input["table_name"].as_str().unwrap_or_default();
        if table == "knowledge_vectors" && matches!(operation, "get_row" | "list_rows") {
            self.vector_reads.lock().unwrap().push((
                operation.to_owned(),
                request.input["row_key"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            ));
        }
        if operation == "put_row" && table == "knowledge_vectors" {
            self.vector_write_sizes.lock().unwrap().push(1);
        }
        if operation == "mutate_rows" {
            let vectors = request.input["mutations"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|mutation| mutation["table_name"] == "knowledge_vectors")
                .count();
            if vectors > 0 {
                self.vector_write_sizes.lock().unwrap().push(vectors);
            }
        }
        let key = (request.plugin_id.clone(), table.to_owned());
        let mut tables = self.rows.lock().expect("memory broker rows");
        let rows = tables.entry(key).or_default();
        match operation {
            "ensure_table" => {
                Ok(serde_json::json!({"table_name": table, "schema": request.input["schema"]}))
            }
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
            "list_rows" => Ok(list_rows(rows, &request.input)),
            "trim_rows_by_key" => {
                let retained = request.input["retained_rows"].as_u64().unwrap_or_default() as usize;
                while rows.len() > retained {
                    let oldest = rows.keys().next().cloned().expect("non-empty rows");
                    rows.remove(&oldest);
                }
                Ok(serde_json::json!({"retained_rows": rows.len()}))
            }
            "mutate_rows" => {
                for mutation in request.input["mutations"].as_array().into_iter().flatten() {
                    let table_name = mutation["table_name"].as_str().unwrap_or_default();
                    let rows = tables
                        .entry((request.plugin_id.clone(), table_name.to_owned()))
                        .or_default();
                    let row_key = mutation["row_key"].as_str().unwrap_or_default().to_owned();
                    match mutation["operation"].as_str().unwrap_or_default() {
                        "put" => {
                            rows.insert(row_key, mutation["value"].clone());
                        }
                        "delete" => {
                            rows.remove(&row_key);
                        }
                        _ => return Err(host_error("storage.plugin", "invalid_operation")),
                    }
                }
                Ok(serde_json::json!({"rows_put": 0, "rows_deleted": 0}))
            }
            _ => Err(host_error("storage.plugin", "invalid_operation")),
        }
    }
}

fn list_rows(
    rows: &BTreeMap<String, serde_json::Value>,
    input: &serde_json::Value,
) -> serde_json::Value {
    let prefix = input["key_prefix"].as_str().unwrap_or_default();
    let after = input["after_key"].as_str().unwrap_or_default();
    let limit = input["limit"].as_u64().unwrap_or(500) as usize;
    let matching = rows
        .iter()
        .filter(|(key, _)| key.starts_with(prefix) && key.as_str() > after)
        .collect::<Vec<_>>();
    let next = (matching.len() > limit).then(|| matching[limit - 1].0.clone());
    let values = matching
        .into_iter()
        .take(limit)
        .map(|(row_key, value)| serde_json::json!({"row_key": row_key, "value": value}))
        .collect::<Vec<_>>();
    serde_json::json!({"rows": values, "next_after_key": next})
}

fn semantic_response(input: &serde_json::Value) -> serde_json::Value {
    let export = input["export_id"].as_str().unwrap_or_default();
    let requested_element_id = input["input"]["semantic_element_id"]
        .as_str()
        .unwrap_or("element-a");
    let element = serde_json::json!({
        "semantic_element_id": requested_element_id, "element_kind": "function",
        "name": "Element A", "path": "src/a.rs", "parent_element_id": null,
        "start_line": 1, "end_line": 8
    });
    let output = match export {
        "get_semantic_tree" => serde_json::json!({
            "project_root": "/project", "roots": [{"element": element, "children": []}]
        }),
        "search_semantic_elements" => serde_json::json!({
            "mode": "lexical_fallback", "results": [{"element": element, "score": 1.0}]
        }),
        "semantic_graph" => serde_json::json!({
            "projectRoot": "/project", "nodes": [{
                "id": "element-a", "label": "Element A", "kind": "function",
                "path": "src/a.rs", "summary": "Function A"
            }], "edges": []
        }),
        "semantic_context" => serde_json::json!({
            "elements": [{
                "project_root": "/project", "semantic_element_id": "file:src/a.rs",
                "semantic_source_id": "source-main", "path": "src/a.rs",
                "element_kind": "file", "name": "a.rs", "parent_element_id": null,
                "content_fingerprint": "fp1:0000000000000001:file", "start_line": 1, "end_line": 8,
                "lifecycle": "active", "metadata": {"indexer_metadata": {}}
            }, {
                "project_root": "/project", "semantic_element_id": "element-a",
                "semantic_source_id": "source-main", "path": "src/a.rs",
                "element_kind": "function", "name": "element_a", "parent_element_id": "file:src/a.rs",
                "content_fingerprint": "fp1:0000000000000002:a", "start_line": 1, "end_line": 8,
                "lifecycle": "active", "metadata": {"indexer_metadata": {
                    "signature": "pub fn element_a(input: &str) -> Result<()>"
                }}
            }],
            "relationships": [],
            "artifacts": [{
                "project_root": "/project", "artifact_id": "source-element-a",
                "semantic_element_id": "element-a", "artifact_kind": "source",
                "title": "element_a source", "content_ref": null,
                "content": "pub fn element_a(input: &str) -> Result<()> { serde_json::json!({}); Ok(()) }",
                "searchable_text": "element a", "content_size_bytes": 80,
                "metadata": {"language": "rust"}
            }],
            "next_after_element_id": null
        }),
        _ => serde_json::json!({}),
    };
    serde_json::json!({"output": output})
}

fn embed_response(input: &serde_json::Value) -> serde_json::Value {
    let count = input["texts"].as_array().map_or(0, Vec::len);
    serde_json::json!({"vectors": (0..count).map(|_| vec![1.0, 0.5]).collect::<Vec<_>>()})
}

fn host_error(capability_id: &str, code: &str) -> HostCapabilityError {
    HostCapabilityError::new(
        capability_id,
        code,
        format!("host capability `{capability_id}` failed with `{code}`"),
        false,
    )
}

impl PluginSandbox for TestSubprocessSandbox {
    fn prepare_command(
        &self,
        request: PluginSandboxRequest<'_>,
    ) -> Result<Command, PluginSandboxError> {
        Ok(Command::new(request.executable))
    }
}

impl KnowledgePluginProcess {
    fn launch() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_lumvise-plugin-knowledge"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("launch real Knowledge plugin binary");
        let input = child.stdin.take().expect("plugin stdin");
        let output = child.stdout.take().expect("plugin stdout");
        Self {
            child,
            input,
            output,
            codec: FrameCodec::default(),
        }
    }

    fn exchange(&mut self, body: MessageBody) -> WireMessage {
        self.codec
            .write_to(
                &mut self.input,
                &WireMessage {
                    protocol: CURRENT_PROTOCOL_VERSION,
                    body,
                },
            )
            .expect("write protocol frame");
        self.codec
            .read_from(&mut self.output)
            .expect("read protocol frame")
    }

    fn wait_for_success(mut self) {
        drop(self.input);
        assert!(self.child.wait().expect("wait for plugin").success());
    }
}

fn signed_semantic_fixture(workspace: &Path, signing_key: &SigningKey) -> InstalledPlugin {
    const TARGET: &str = "knowledge-semantic-fixture-host";
    let executable = std::fs::read(env!("CARGO_BIN_EXE_knowledge-test-semantic-plugin"))
        .expect("read Semantic fixture binary");
    let digest = hex::encode(Sha256::digest(&executable));
    let executable_path = format!("bin/knowledge-test-semantic-plugin-{TARGET}");
    let manifest = PluginManifest {
        schema_version: 1,
        publisher: PublisherIdentity {
            publisher_id: "lumvise.test".into(),
            key_id: "lumvise.test.semantic.1".into(),
        },
        plugin_id: "builtin.semantic".into(),
        plugin_version: "0.1.0".into(),
        protocol: ProtocolRange {
            min: PACKAGE_PROTOCOL_VERSION,
            max: PACKAGE_PROTOCOL_VERSION,
        },
        targets: BTreeMap::from([(TARGET.into(), executable_path.clone())]),
        files: BTreeMap::from([(executable_path.clone(), digest)]),
        exports: [
            "get_semantic_tree",
            "search_semantic_elements",
            "semantic_graph",
            "semantic_context",
            "ingest_index_batch",
        ]
        .into_iter()
        .map(|id| ExportDescriptor {
            description: String::new(),
            id: id.into(),
            name: id.into(),
            surface: ExportSurface::McpTool,
            input_schema: serde_json::json!({"type": "object"}),
            output_schema: serde_json::json!({"type": "object"}),
            admission: None,
            execution: ExecutionMode::Foreground,
        })
        .collect(),
        host_capabilities: vec![lumvise_plugin_package::HostCapabilityRequirement {
            id: "fixture.semantic.ingest".into(),
            version: "^1.0".into(),
        }],
    };
    let archive = workspace.join("semantic-fixture.lvp");
    build_package(
        &archive,
        BuildPackageRequest {
            manifest,
            files: BTreeMap::from([(executable_path, executable)]),
            signing_key,
        },
    )
    .expect("build Semantic fixture package");
    let verified = verify_package(
        &archive,
        &signing_key.verifying_key(),
        &HostCompatibility::new(PACKAGE_PROTOCOL_VERSION, TARGET),
    )
    .expect("verify Semantic fixture package");
    install_verified_package(&verified, &workspace.join("semantic-installed"))
        .expect("install Semantic fixture package")
}

#[test]
fn signed_runtime_degrades_plugin_when_output_breaks_declared_contract() {
    const TARGET: &str = "knowledge-invalid-output-host";
    let workspace = tempfile::tempdir().expect("invalid output workspace");
    let executable = std::fs::read(env!("CARGO_BIN_EXE_lumvise-plugin-knowledge"))
        .expect("read Knowledge binary");
    let executable_hash = hex::encode(Sha256::digest(&executable));
    let mut manifest = package_manifest_source(TARGET, &executable_hash);
    let executable_path = manifest.targets[TARGET].clone();
    let descriptor = manifest
        .exports
        .iter_mut()
        .find(|export| export.id == MANIFEST_EXPORT_ID)
        .expect("manifest descriptor");
    descriptor.output_schema = serde_json::json!({
        "type": "object", "required": ["impossible"],
        "properties": {"impossible": {"const": true}}, "additionalProperties": false
    });
    let signing_key = SigningKey::from_bytes(&[29; 32]);
    let archive = workspace.path().join("invalid-output.lvp");
    build_package(
        &archive,
        BuildPackageRequest {
            manifest,
            files: BTreeMap::from([(executable_path, executable)]),
            signing_key: &signing_key,
        },
    )
    .expect("build invalid-output package");
    let verified = verify_package(
        &archive,
        &signing_key.verifying_key(),
        &HostCompatibility::new(PACKAGE_PROTOCOL_VERSION, TARGET),
    )
    .expect("verify signed invalid-output package");
    let installed = install_verified_package(&verified, &workspace.path().join("installed"))
        .expect("install invalid-output package");
    let system = PluginSystem::with_broker_and_sandbox(
        fast_runtime_config(),
        Arc::new(MemoryStorageBroker::default()),
        Arc::new(TestSubprocessSandbox),
    );
    system.install(&installed).expect("catalog package");
    system.start(PLUGIN_ID).expect("start package");

    let error = system
        .invoke(PLUGIN_ID, MANIFEST_EXPORT_ID, serde_json::json!({}))
        .expect_err("invalid output fails closed");

    assert!(error.to_string().contains("impossible"));
    assert!(!system.is_active(PLUGIN_ID).expect("degraded state"));
    make_tree_writable(workspace.path());
}

fn seed_paged_knowledge(broker: &MemoryStorageBroker) {
    for index in 0..510 {
        broker.put_knowledge(&format!("bulk-{index:04}"), "bulk-owner", "Bulk");
    }
    broker.put_knowledge("zzzz-page-two-target", "page-two-owner", "Target");
}

fn test_element(semantic_element_id: &str, project_root: &str) -> SemanticElement {
    SemanticElement {
        project_root: project_root.into(),
        semantic_element_id: semantic_element_id.into(),
        semantic_source_id: "semantic-fixture".into(),
        path: format!("src/{semantic_element_id}.rs"),
        element_kind: "function".into(),
        name: semantic_element_id.into(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: None,
        end_line: None,
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: serde_json::json!({}),
    }
}

fn test_artifact(artifact_id: &str, semantic_element_id: &str, title: &str) -> SemanticArtifact {
    SemanticArtifact {
        artifact_id: artifact_id.into(),
        semantic_element_id: semantic_element_id.into(),
        artifact_kind: "specification".into(),
        title: title.into(),
        content_ref: None,
        content: None,
        searchable_text: None,
        content_size_bytes: None,
        dependencies: vec![],
        metadata: serde_json::json!({"knowledge": {
            "tags": [], "metadata": {}, "project_root": "/work/demo"
        }}),
    }
}

fn assert_succeeds(system: &PluginSystem, export_id: &str, input: serde_json::Value) {
    let outcome = system
        .invoke(PLUGIN_ID, export_id, input)
        .expect("invoke export");
    assert!(
        matches!(outcome, WireOutcome::Succeeded { .. }),
        "export `{export_id}` failed: {outcome:?}"
    );
}

fn invoke_value(
    system: &PluginSystem,
    export_id: &str,
    input: serde_json::Value,
) -> serde_json::Value {
    match system
        .invoke(PLUGIN_ID, export_id, input)
        .expect("invoke export")
    {
        WireOutcome::Succeeded { value } => value,
        outcome => panic!("export `{export_id}` failed: {outcome:?}"),
    }
}

#[test]
fn knowledge_lifecycle_hides_inactive_records_and_rejects_stale_writes() {
    let workspace = tempfile::tempdir().expect("lifecycle workspace");
    let broker = Arc::new(MemoryStorageBroker::default());
    broker.ensure_element("file:lifecycle", "/work/demo");
    broker.put_knowledge("legacy-record", "file:lifecycle", "Legacy record");
    let system = start_packaged_knowledge(&workspace, broker.clone());

    let created = invoke_value(
        &system,
        CREATE_EXPORT_ID,
        serde_json::json!({
            "artifact_id": "lifecycle-decision", "semantic_element_id": "file:lifecycle",
            "knowledge_type": "specification", "title": "Lifecycle decision", "content": "Keep history.",
            "tags": [], "dependencies": [], "metadata": {}, "project_root": "/work/demo",
            "expires_at": "2099-01-01T00:00:00Z"
        }),
    );
    assert_eq!(created["artifact"]["status"], "active");
    assert_eq!(created["artifact"]["revision"], 1);
    assert_eq!(
        created["artifact"]["expires_at"],
        "2099-01-01T00:00:00+00:00"
    );

    let stale = system
        .invoke(
            PLUGIN_ID,
            UPDATE_EXPORT_ID,
            serde_json::json!({"artifact_id": "lifecycle-decision", "title": "Stale", "expected_revision": 0}),
        )
        .expect("invoke stale update");
    assert!(
        matches!(stale, WireOutcome::Failed { error } if error.code == "knowledge_revision_conflict")
    );

    let resolved = invoke_value(
        &system,
        UPDATE_EXPORT_ID,
        serde_json::json!({
            "artifact_id": "lifecycle-decision", "status": "resolved", "expected_revision": 1
        }),
    );
    assert_eq!(resolved["artifact"]["status"], "resolved");
    assert_eq!(resolved["artifact"]["revision"], 2);
    assert!(resolved["artifact"].get("expires_at").is_none());

    let list = |include_inactive: Option<bool>| {
        invoke_value(
            &system,
            LIST_EXPORT_ID,
            serde_json::json!({"semantic_element_id": "file:lifecycle", "include_inactive": include_inactive}),
        )
    };
    assert_eq!(artifact_ids(&list(None)), ["legacy-record"]);
    assert_eq!(
        artifact_ids(&list(Some(true))),
        ["legacy-record", "lifecycle-decision"]
    );

    let search = |include_inactive: Option<bool>| {
        invoke_value(
            &system,
            SEARCH_EXPORT_ID,
            serde_json::json!({
                "query": "Lifecycle decision", "project_root": "/work/demo",
                "include_inactive": include_inactive
            }),
        )
    };
    let hidden = search(None);
    assert!(
        hidden["results"]
            .as_array()
            .expect("search results")
            .iter()
            .all(|result| result["artifact_id"] != "lifecycle-decision")
    );
    let shown = search(Some(true));
    assert!(
        shown["results"]
            .as_array()
            .expect("search results")
            .iter()
            .any(|result| result["artifact_id"] == "lifecycle-decision"
                && result["status"] == "resolved")
    );

    let legacy = invoke_value(
        &system,
        GET_EXPORT_ID,
        serde_json::json!({"artifact_id": "legacy-record"}),
    );
    assert_eq!(legacy["artifact"]["status"], "active");
    assert_eq!(legacy["artifact"]["revision"], 0);

    system
        .stop(PLUGIN_ID)
        .expect("stop packaged Knowledge binary");
    system
        .uninstall(PLUGIN_ID)
        .expect("detach Knowledge package");
    make_tree_writable(workspace.path());
}

fn start_packaged_knowledge(
    workspace: &tempfile::TempDir,
    broker: Arc<MemoryStorageBroker>,
) -> PluginSystem {
    const TARGET: &str = "knowledge-lifecycle-host";
    let executable = std::fs::read(env!("CARGO_BIN_EXE_lumvise-plugin-knowledge"))
        .expect("read real Knowledge binary");
    let executable_hash = hex::encode(Sha256::digest(&executable));
    let manifest = package_manifest_source(TARGET, &executable_hash);
    let executable_path = manifest.targets[TARGET].clone();
    let signing_key = SigningKey::from_bytes(&[43; 32]);
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
    let system = PluginSystem::with_broker_and_sandbox(
        fast_runtime_config(),
        broker,
        Arc::new(TestSubprocessSandbox),
    );
    system
        .install(&installed)
        .expect("catalog Knowledge package");
    system
        .start(PLUGIN_ID)
        .expect("start packaged Knowledge binary");
    system
}

fn artifact_ids(list: &serde_json::Value) -> Vec<String> {
    list["artifacts"]
        .as_array()
        .expect("artifact list")
        .iter()
        .map(|artifact| {
            artifact["artifact_id"]
                .as_str()
                .expect("artifact id")
                .to_owned()
        })
        .collect()
}
