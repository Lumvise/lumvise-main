use std::{
    collections::{BTreeMap, HashMap},
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
    APPLY_TRANSFER_EXPORT_ID, ASSISTANT_FIND_ELEMENTS_EXPORT_ID, ASSISTANT_GET_ELEMENT_EXPORT_ID,
    ASSISTANT_GET_EXPORT_ID, ASSISTANT_LIST_EXPORT_ID, ASSISTANT_SEARCH_EXPORT_ID,
    CREATE_EXPORT_ID, DEBUG_C4_EXPORT_ID, DELETE_EXPORT_ID, ELEMENT_TRIGGER_EXPORT_ID,
    ENSURE_C4_EXPORT_ID, FIND_ELEMENTS_EXPORT_ID, GET_CULTIVATION_RUN_EXPORT_ID,
    GET_ELEMENT_EXPORT_ID, GET_EXPORT_ID, HTTP_C4_ACTION_EXPORT_ID, HTTP_C4_DEBUG_EXPORT_ID,
    HTTP_C4_EXPORT_ID, HTTP_CREATE_ARTIFACT_EXPORT_ID, HTTP_DELETE_ARTIFACT_EXPORT_ID,
    HTTP_EVENTS_EXPORT_ID, HTTP_EXPORT_EXPORT_ID, HTTP_MANIFEST_EXPORT_ID, HTTP_PAGE_EXPORT_ID,
    HTTP_PROJECTION_ARTIFACTS_EXPORT_ID, HTTP_RESOLVE_TARGET_EXPORT_ID, HTTP_SETUP_EXPORT_ID,
    HTTP_SYNC_EXPORT_ID, HTTP_UPDATE_ARTIFACT_EXPORT_ID, HTTP_WRITE_EXPORT_ID, LIST_EXPORT_ID,
    MANIFEST_EXPORT_ID, PACKAGE_PROTOCOL_VERSION, PLUGIN_ID, PREVIEW_TRANSFER_EXPORT_ID,
    PROJECTION_EXPORT_ID, REBUILD_EXPORT_ID, RUN_CULTIVATION_EXPORT_ID, SEARCH_EXPORT_ID,
    UPDATE_EXPORT_ID, package_manifest_source,
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
    rows: Mutex<HashMap<(String, String), BTreeMap<String, serde_json::Value>>>,
    semantic_operations: Mutex<BTreeMap<String, usize>>,
    vector_write_sizes: Mutex<Vec<usize>>,
    plugin_invoke_operations: Mutex<BTreeMap<String, usize>>,
    failure: Mutex<Option<(String, String)>>,
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
            rows: Mutex::default(),
            semantic_operations: Mutex::default(),
            vector_write_sizes: Mutex::default(),
            plugin_invoke_operations: Mutex::default(),
            failure: Mutex::default(),
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
            "plugin.invoke" => self.plugin_invoke(request),
            "neural.embed" => Ok(embed_response(&request.input)),
            "runtime.background_job" => Ok(serde_json::json!({
                "job_id": "knowledge-job-1", "status": "accepted"
            })),
            other => Err(host_error(other, "unsupported_capability")),
        }
    }
}

impl MemoryStorageBroker {
    fn semantic_storage(
        &self,
        request: HostCapabilityRequest,
    ) -> Result<serde_json::Value, HostCapabilityError> {
        let operation = request.input["operation"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        *self
            .semantic_operations
            .lock()
            .expect("semantic operation counts")
            .entry(operation)
            .or_default() += 1;
        if request.input["operation"] == "upsert_artifact" {
            let owner = request.input["artifact"]["semantic_element_id"]
                .as_str()
                .unwrap_or_default();
            let project_root = request.input["artifact"]["metadata"]["knowledge"]["project_root"]
                .as_str()
                .unwrap_or("/work/demo");
            self.ensure_element(owner, project_root);
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
            .entry(export_id)
            .or_default() += 1;
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
            SemanticOperation::Element {
                semantic_element_id: semantic_element_id.to_owned(),
            },
            &InvocationControl::sixty_seconds(),
        );
        match result {
            Ok(SemanticResult::Element(Some(_))) => return,
            Ok(SemanticResult::Element(None)) => {}
            Ok(other) => panic!("expected Element result while checking seed owner, got {other:?}"),
            Err(error) => {
                panic!("expected Element operation to succeed while checking seed owner: {error}")
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

#[test]
fn manifest_source_declares_storage_backed_knowledge_mcp_exports() {
    let digest = "a".repeat(64);
    let manifest = package_manifest_source("aarch64-apple-darwin", &digest);

    assert_eq!(manifest.plugin_id, PLUGIN_ID);
    assert_eq!(manifest.protocol.min, PACKAGE_PROTOCOL_VERSION);
    assert_eq!(manifest.protocol.max, PACKAGE_PROTOCOL_VERSION);
    assert_eq!(manifest.exports[0].id, MANIFEST_EXPORT_ID);
    assert!(
        !manifest
            .exports
            .iter()
            .any(|export| matches!(export.surface, ExportSurface::View { .. }))
    );
    assert_eq!(manifest.files.len(), 1);
    assert_eq!(manifest.host_capabilities[0].id, "storage.plugin");
    assert_eq!(manifest.host_capabilities[0].version, "^1.0");
    assert!(
        manifest
            .exports
            .iter()
            .any(|export| export.id == CREATE_EXPORT_ID)
    );
    assert!(
        manifest
            .exports
            .iter()
            .any(|export| export.id == DELETE_EXPORT_ID)
    );
    assert!(
        manifest
            .exports
            .iter()
            .any(|export| export.id == SEARCH_EXPORT_ID)
    );
    assert!(manifest.exports.iter().any(|export| {
        export.id == HTTP_EVENTS_EXPORT_ID
            && matches!(
                export.surface,
                lumvise_plugin_package::ExportSurface::HttpRoute {
                    stream_mode: HttpStreamMode::ServerSentEvents,
                    ..
                }
            )
    }));
    let sync = manifest
        .exports
        .iter()
        .find(|export| export.id == HTTP_SYNC_EXPORT_ID)
        .expect("Knowledge sync export");
    assert!(matches!(sync.surface, ExportSurface::HttpRoute { .. }));
    assert!(manifest.exports.iter().any(|export| {
        export.id == ELEMENT_TRIGGER_EXPORT_ID
            && matches!(&export.surface,
                ExportSurface::StorageTrigger { event_kinds, entity_kinds, .. }
                    if !event_kinds.is_empty() && entity_kinds.is_empty())
    }));
    assert!(manifest.exports.iter().all(|export| {
        export.input_schema["additionalProperties"] == false
            && export.output_schema["additionalProperties"] == false
    }));
    let global_ids = manifest
        .exports
        .iter()
        .filter(|export| {
            matches!(
                export.surface,
                lumvise_plugin_package::ExportSurface::McpTool
            )
        })
        .map(|export| export.id.as_str())
        .collect::<Vec<_>>();
    assert!(!global_ids.iter().any(|id| id.starts_with("knowledge.")));
    assert!(
        manifest
            .exports
            .iter()
            .filter(|export| {
                matches!(
                    export.surface,
                    lumvise_plugin_package::ExportSurface::ScopedMcpTool { .. }
                )
            })
            .all(|export| {
                export.input_schema["properties"]["mcp_owner_id"]["type"] == "string"
                    && export.input_schema["properties"]["plugin_id"]["type"] == "string"
                    && export.input_schema["properties"]["session_id"]["type"] == "string"
            })
    );
}

#[test]
fn real_binary_serves_declared_export_rejects_unknown_and_stops() {
    let expected_exports = package_manifest_source("test-target", &"a".repeat(64))
        .exports
        .into_iter()
        .map(|export| export.id)
        .collect::<Vec<_>>();
    let mut plugin = KnowledgePluginProcess::launch();
    let session_id = "knowledge-session";
    let package_digest = "signed-package-digest";

    let ready = plugin.exchange(MessageBody::HostHello {
        session_id: session_id.into(),
        host_id: "knowledge-test-host".into(),
        package_digest: package_digest.into(),
    });
    assert!(matches!(
        ready.body,
        MessageBody::PluginReady {
            session_id: ready_session,
            plugin_id,
            package_digest: ready_digest,
        } if ready_session == session_id && plugin_id == PLUGIN_ID && ready_digest == package_digest
    ));

    let result = plugin.exchange(MessageBody::HostInvoke {
        session_id: session_id.into(),
        invocation_id: "manifest-invocation".into(),
        capability_id: MANIFEST_EXPORT_ID.into(),
        input: serde_json::json!({}),
    });
    assert!(matches!(
        result.body,
        MessageBody::PluginResult {
            outcome: WireOutcome::Succeeded { value },
            ..
        } if value["plugin_id"] == PLUGIN_ID
            && value["protocol"] == PACKAGE_PROTOCOL_VERSION
            && value["exports"] == serde_json::json!(expected_exports)
    ));

    let unknown = plugin.exchange(MessageBody::HostInvoke {
        session_id: session_id.into(),
        invocation_id: "unknown-invocation".into(),
        capability_id: "knowledge.missing".into(),
        input: serde_json::json!({}),
    });
    assert!(matches!(
        unknown.body,
        MessageBody::PluginResult {
            outcome: WireOutcome::Failed { error },
            ..
        } if error.code == "unknown_capability" && !error.retryable
    ));

    let stopped = plugin.exchange(MessageBody::HostShutdown {
        session_id: session_id.into(),
        reason: Some("test complete".into()),
    });
    assert!(matches!(
        stopped.body,
        MessageBody::PluginStopped { session_id: stopped_session, .. }
            if stopped_session == session_id
    ));
    plugin.wait_for_success();
}

#[test]
fn signed_knowledge_package_runs_through_plugin_system_lifecycle() {
    const TARGET: &str = "knowledge-integration-host";
    let workspace = tempfile::tempdir().expect("package workspace");
    let executable = std::fs::read(env!("CARGO_BIN_EXE_lumvise-plugin-knowledge"))
        .expect("read real Knowledge binary");
    let executable_hash = hex::encode(Sha256::digest(&executable));
    let manifest = package_manifest_source(TARGET, &executable_hash);
    let executable_path = manifest.targets[TARGET].clone();
    let signing_key = SigningKey::from_bytes(&[23; 32]);
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
    broker.seed_projection_graph();
    broker
        .rows
        .lock()
        .expect("seed foreign namespace")
        .entry(("other.plugin".into(), "knowledge_artifacts".into()))
        .or_default()
        .insert(
            "foreign".into(),
            serde_json::json!({
                "artifact_id": "foreign", "semantic_element_id": "element-a",
                "knowledge_type": "definition", "title": "Foreign",
                "content": "Must stay isolated", "tags": [], "metadata": {}
            }),
        );
    seed_paged_knowledge(&broker);
    let system = PluginSystem::with_broker_and_sandbox(
        fast_runtime_config(),
        broker.clone(),
        Arc::new(TestSubprocessSandbox),
    );

    let semantic = signed_semantic_fixture(workspace.path(), &SigningKey::from_bytes(&[24; 32]));
    system.install(&semantic).expect("catalog Semantic fixture");
    system
        .start("builtin.semantic")
        .expect("start Semantic fixture");

    system
        .install(&installed)
        .expect("catalog Knowledge package");
    system
        .start(PLUGIN_ID)
        .expect("start packaged Knowledge binary");
    let scoped = system
        .scoped_exports("assistant_session")
        .expect("assistant-scoped exports");
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].exports.len(), 8);
    for name in ["knowledge.create", "knowledge.update"] {
        assert!(scoped[0].exports.iter().any(|export| export.id == name));
    }
    assert!(
        scoped[0]
            .exports
            .iter()
            .all(|export| export.id.starts_with("knowledge."))
    );
    assert!(
        system
            .scoped_exports("other_scope")
            .expect("unrelated scope")
            .is_empty()
    );
    let outcome = system
        .invoke(PLUGIN_ID, MANIFEST_EXPORT_ID, serde_json::json!({}))
        .expect("invoke packaged Knowledge command");
    assert!(matches!(
        outcome,
        WireOutcome::Succeeded { value }
            if value["plugin_id"] == PLUGIN_ID
                && value["exports"][0] == MANIFEST_EXPORT_ID
    ));

    let missing_element = system
        .invoke(
            PLUGIN_ID,
            CREATE_EXPORT_ID,
            serde_json::json!({
                "artifact_id": "missing-element", "knowledge_type": "definition",
                "title": "Missing", "content": "Element absent"
            }),
        )
        .expect_err("signed schema rejects a missing semantic element id");
    assert!(missing_element.to_string().contains("semantic_element_id"));

    let invalid_kind = system
        .invoke(
            PLUGIN_ID,
            CREATE_EXPORT_ID,
            serde_json::json!({
                "artifact_id": "invalid", "semantic_element_id": "element-a",
                "knowledge_type": "invalid_kind", "title": "Invalid", "content": "Invalid"
            }),
        )
        .expect_err("signed schema rejects an invalid Knowledge kind");
    assert!(invalid_kind.to_string().contains("invalid_kind"));

    let update_missing = system
        .invoke(
            PLUGIN_ID,
            UPDATE_EXPORT_ID,
            serde_json::json!({
                "artifact_id": "absent", "title": "Still absent"
            }),
        )
        .expect("structured missing update failure");
    assert!(matches!(update_missing, WireOutcome::Failed { .. }));

    let get_semantic_tree_before = broker.plugin_invoke_count("get_semantic_tree");
    for (artifact_id, title) in [("artifact-z", "Zeta"), ("artifact-a", "Alpha")] {
        let created = system
            .invoke(
                PLUGIN_ID,
                CREATE_EXPORT_ID,
                serde_json::json!({
                    "artifact_id": artifact_id, "semantic_element_id": "element-a",
                    "knowledge_type": "definition", "title": title,
                    "content": "Shared lexical content", "tags": ["rust"],
                    "project_root": "/project"
                }),
            )
            .expect("create Knowledge");
        assert!(
            matches!(created, WireOutcome::Succeeded { .. }),
            "create failed: {created:?}"
        );
    }

    let titled = invoke_value(
        &system,
        SEARCH_EXPORT_ID,
        serde_json::json!({"query": "Zeta", "project_root": "/project", "limit": 1}),
    );
    assert_eq!(titled["mode"], "vector");
    assert_eq!(titled["results"][0]["artifact_id"], "artifact-z");
    let cold_writes = broker.vector_write_sizes.lock().unwrap().clone();
    assert!(
        cold_writes.iter().any(|size| *size > 1),
        "vectors must use batch writes: {cold_writes:?}"
    );
    assert!(cold_writes.iter().all(|size| *size <= 64));
    let cached = invoke_value(
        &system,
        SEARCH_EXPORT_ID,
        serde_json::json!({"query": "Zeta", "project_root": "/project", "limit": 1}),
    );
    assert_eq!(cached, titled);
    assert_eq!(
        *broker.vector_write_sizes.lock().unwrap(),
        cold_writes,
        "warm search must reuse stored vectors without writes"
    );

    broker.sync_elements("/other", vec![test_element("element-other", "/other")]);
    let mismatched = system.invoke(PLUGIN_ID, CREATE_EXPORT_ID, serde_json::json!({
        "artifact_id":"wrong-project", "semantic_element_id":"element-a", "project_root":"/other",
        "knowledge_type":"definition", "title":"Invalid scope", "content":"Must not appear in either project"
    })).unwrap();
    assert!(
        matches!(mismatched, WireOutcome::Failed { error } if error.code == "knowledge_project_mismatch")
    );
    let pathed = system
        .invoke(
            PLUGIN_ID,
            "knowledge.create",
            serde_json::json!({
                "artifact_id": "artifact-path", "semantic_element_id": "element-other",
                "knowledge_type": "definition", "title": "Pathed",
                "content": "Pathed unique content", "tags": ["vault"],
                "path": "notes/deep.md", "project_root": "/other",
                "mcp_owner_id": "workspace", "plugin_id": "builtin.knowledge", "session_id": "voice-edit"
            }),
        )
        .expect("create pathed Knowledge");
    assert!(
        matches!(&pathed, WireOutcome::Succeeded { value } if value["artifact"]["path"] == "notes/deep.md"),
        "create must echo the knowledge-space path: {pathed:?}"
    );

    system.stop(PLUGIN_ID).expect("restart stop");
    system.start(PLUGIN_ID).expect("restart start");
    let persisted = system
        .invoke(
            PLUGIN_ID,
            GET_EXPORT_ID,
            serde_json::json!({"artifact_id": "artifact-a"}),
        )
        .expect("get persisted Knowledge");
    assert!(
        matches!(persisted, WireOutcome::Succeeded { value } if value["artifact"]["title"] == "Alpha")
    );
    let persisted_pathed = system
        .invoke(
            PLUGIN_ID,
            GET_EXPORT_ID,
            serde_json::json!({"artifact_id": "artifact-path"}),
        )
        .expect("get persisted pathed Knowledge");
    assert!(
        matches!(persisted_pathed, WireOutcome::Succeeded { ref value } if value["artifact"]["path"] == "notes/deep.md"),
        "knowledge-space path must survive a plugin restart: {persisted_pathed:?}"
    );
    let moved = system
        .invoke(
            PLUGIN_ID,
            "knowledge.update",
            serde_json::json!({"artifact_id": "artifact-path", "path": "decisions/moved.md",
                "mcp_owner_id": "workspace", "plugin_id": "builtin.knowledge", "session_id": "voice-edit"}),
        )
        .expect("update pathed Knowledge");
    assert!(
        matches!(&moved, WireOutcome::Succeeded { value } if value["artifact"]["path"] == "decisions/moved.md"),
        "update must replace the knowledge-space path: {moved:?}"
    );
    let project_reads = broker.semantic_operation_count("project_artifacts");
    let root_reads = broker.semantic_operation_count("project_roots");
    let scoped = invoke_value(
        &system,
        "list_knowledge",
        serde_json::json!({"project_root":"/project"}),
    );
    assert!(!scoped["artifacts"].as_array().unwrap().is_empty());
    assert!(
        scoped["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|artifact| artifact["project_root"] == "/project")
    );
    assert_eq!(
        broker.semantic_operation_count("project_artifacts"),
        project_reads + 1,
        "scoped list must read only the requested project's artifacts"
    );
    assert_eq!(
        broker.semantic_operation_count("project_roots"),
        root_reads,
        "scoped list must not enumerate unrelated projects"
    );
    let projection = system
        .invoke(
            PLUGIN_ID,
            PROJECTION_EXPORT_ID,
            serde_json::json!({"projectRoot": "/project", "changesSince": 0}),
        )
        .expect("invoke signed Knowledge projection Command");
    assert!(
        matches!(&projection, WireOutcome::Succeeded { value }
        if value["spaces"][0]["spaceId"] == "project"
            && value["elements"][0]["sourceId"] == "/project"
            && value["elements"][0]["elementId"] == "file:a"
            && value["elements"][0]["children"][0]["elementId"] == "element-a"),
        "unexpected projection outcome: {projection:?}"
    );

    broker.ensure_element("element-b", "/project");
    let updated = system
        .invoke(
            PLUGIN_ID,
            UPDATE_EXPORT_ID,
            serde_json::json!({
                "artifact_id": "artifact-a", "title": "Alpha Updated",
                "semantic_element_id": "element-b"
            }),
        )
        .expect("update persisted Knowledge");
    assert!(matches!(updated, WireOutcome::Succeeded { value }
            if value["artifact"]["title"] == "Alpha Updated"
                && value["artifact"]["semantic_element_id"] == "element-b"));

    let listed = system
        .invoke(
            PLUGIN_ID,
            LIST_EXPORT_ID,
            serde_json::json!({"semantic_element_id": "element-b"}),
        )
        .expect("list Knowledge");
    assert!(matches!(listed, WireOutcome::Succeeded { value }
        if value["artifacts"].as_array().is_some_and(|items| items.len() == 1)
            && value["artifacts"][0]["artifact_id"] == "artifact-a"));
    assert_eq!(
        broker.plugin_invoke_count("get_semantic_tree"),
        get_semantic_tree_before,
        "Knowledge create/update/list identity validation must use storage.semantic Element \
         directly, never plugin.invoke get_semantic_tree"
    );

    let searched = system
        .invoke(
            PLUGIN_ID,
            SEARCH_EXPORT_ID,
            serde_json::json!({
                "query": "rust", "project_root": "/project", "semantic_element_id": "element-a"
            }),
        )
        .expect("search Knowledge");
    assert!(matches!(searched, WireOutcome::Succeeded { value }
        if value["results"].as_array().is_some_and(|items| items.len() == 1)
            && value["results"][0]["artifact_id"] == "artifact-z"));

    let paged = system
        .invoke(
            PLUGIN_ID,
            SEARCH_EXPORT_ID,
            serde_json::json!({"query": "target", "semantic_element_id": "page-two-owner"}),
        )
        .expect("search across bounded storage pages");
    assert!(matches!(paged, WireOutcome::Succeeded { value }
        if value["results"][0]["artifact_id"] == "zzzz-page-two-target"));

    let extra_field = system
        .invoke(
            PLUGIN_ID,
            GET_EXPORT_ID,
            serde_json::json!({"artifact_id": "artifact-a", "unexpected": true}),
        )
        .expect_err("signed input schema rejects unexpected fields");
    assert!(extra_field.to_string().contains("unexpected"));

    broker.put_unrelated_graph_artifact("retired-untyped-artifact", "element-a");
    assert_extended_exports(&system, &broker);
    let vector_rows = broker.rows.lock().expect("Knowledge vector rows");
    let rebuilt = vector_rows
        .get(&(PLUGIN_ID.into(), "knowledge_vectors".into()))
        .expect("persisted Knowledge vector table");
    assert!(rebuilt.len() >= 2);
    assert!(rebuilt.contains_key("artifact-a"));
    assert!(rebuilt.contains_key("artifact-z"));
    drop(vector_rows);

    broker.deny("storage.semantic");
    let denied = system
        .invoke(
            PLUGIN_ID,
            GET_ELEMENT_EXPORT_ID,
            serde_json::json!({"semantic_element_id": "element-a"}),
        )
        .expect("permission denial is a protocol result");
    assert!(matches!(denied, WireOutcome::Failed { error } if error.code == "permission_denied"));
    broker.allow_all();

    broker.fail("storage.semantic", "storage_backend_unavailable");
    let missing_target = system
        .invoke(
            PLUGIN_ID,
            GET_ELEMENT_EXPORT_ID,
            serde_json::json!({"semantic_element_id": "element-a"}),
        )
        .expect("storage failure is a protocol result");
    assert!(
        matches!(missing_target, WireOutcome::Failed { error } if error.code == "storage_backend_unavailable")
    );
    broker.allow_all();

    broker.deny("neural.embed");
    let lexical = invoke_value(
        &system,
        SEARCH_EXPORT_ID,
        serde_json::json!({"query": "rust", "semantic_element_id": "element-a"}),
    );
    assert_eq!(lexical["mode"], "lexical");
    broker.fail("neural.embed", "host_capability_unavailable");
    let unavailable_rebuild = invoke_value(
        &system,
        REBUILD_EXPORT_ID,
        serde_json::json!({"project_root": "/project"}),
    );
    assert_eq!(unavailable_rebuild["job"]["status"], "unavailable");
    assert_eq!(unavailable_rebuild["job"]["vectors_rebuilt"], 0);
    broker.allow_all();

    broker.deny("storage.semantic");
    let storage_denied = system
        .invoke(
            PLUGIN_ID,
            GET_EXPORT_ID,
            serde_json::json!({"artifact_id": "artifact-a"}),
        )
        .expect("storage denial is a protocol result");
    assert!(
        matches!(storage_denied, WireOutcome::Failed { error } if error.code == "permission_denied")
    );
    broker.allow_all();
    system
        .stop(PLUGIN_ID)
        .expect("stop packaged Knowledge binary");
    system
        .uninstall(PLUGIN_ID)
        .expect("detach Knowledge package");
    system
        .stop("builtin.semantic")
        .expect("stop Semantic fixture");
    system
        .uninstall("builtin.semantic")
        .expect("detach Semantic fixture");
    assert!(!system.is_installed(PLUGIN_ID).expect("runtime catalog"));

    make_tree_writable(workspace.path());
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
        protocol: ProtocolRange { min: 1, max: 1 },
        targets: BTreeMap::from([(TARGET.into(), executable_path.clone())]),
        files: BTreeMap::from([(executable_path.clone(), digest)]),
        exports: [
            "get_semantic_tree",
            "search_semantic_elements",
            "semantic_graph",
            "semantic_context",
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
        host_capabilities: Vec::new(),
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
        &HostCompatibility::new(1, TARGET),
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
                "knowledge_type": "definition", "title": title,
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

#[test]
fn storage_trigger_synthesizes_md_nucleus_artifacts_under_knowledge_root() {
    const TARGET: &str = "knowledge-md-nucleus-host";
    let workspace = tempfile::tempdir().expect("md nucleus workspace");
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
    let broker = Arc::new(MemoryStorageBroker::default());
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

    let mut md_file = test_element("md-file-auth", "/project");
    md_file.path = "knowledge/decisions/auth.md".into();
    md_file.element_kind = "file".into();
    md_file.name = "auth.md".into();
    broker.sync_elements("/project", vec![md_file]);

    let upserted_trigger = serde_json::json!({
        "project_root": "/project", "base_revision": 0, "target_revision": 1,
        "changed": [{"entity_id": "md-file-auth", "entity_kind": "file",
            "disposition": "upserted"}]
    });
    let outcome = system
        .invoke(PLUGIN_ID, ELEMENT_TRIGGER_EXPORT_ID, upserted_trigger)
        .expect("invoke storage trigger for md file");
    assert!(matches!(outcome, WireOutcome::Succeeded { .. }));

    let artifact_id = format!(
        "knowledge-nucleus-{}",
        hex::encode(Sha256::digest(b"knowledge/decisions/auth.md"))
    );
    let fetched = invoke_value(
        &system,
        GET_EXPORT_ID,
        serde_json::json!({"artifact_id": artifact_id}),
    );
    let artifact = &fetched["artifact"];
    assert_eq!(artifact["semantic_element_id"], "md-file-auth");
    assert_eq!(artifact["knowledge_type"], "definition");
    assert_eq!(artifact["title"], "auth");
    assert_eq!(artifact["path"], "knowledge/decisions/auth.md");
    assert_eq!(artifact["project_root"], "/project");
    assert_eq!(
        artifact["tags"],
        serde_json::json!(["nucleus", "md-nucleus"])
    );
    assert_eq!(
        artifact["metadata"]["nucleus"]["target_path"], "knowledge/decisions/auth.md",
        "canvas projections route md nuclei through metadata.nucleus.target_path"
    );
    assert_eq!(
        artifact["metadata"]["nucleus"]["target_element_id"],
        "md-file-auth"
    );

    // Idempotent reconciliation: a re-delivered upsert batch must not churn
    // the stored artifact.
    let upsert_artifacts_before = broker.semantic_operation_count("upsert_artifact");
    assert_succeeds(
        &system,
        ELEMENT_TRIGGER_EXPORT_ID,
        serde_json::json!({
            "project_root": "/project", "base_revision": 1, "target_revision": 2,
            "changed": [{"entity_id": "md-file-auth", "entity_kind": "file",
                "disposition": "upserted"}]
        }),
    );
    assert_eq!(
        broker.semantic_operation_count("upsert_artifact"),
        upsert_artifacts_before,
        "an unchanged md file must not rewrite its nucleus artifact"
    );

    // The database is the Markdown authority: re-indexing may not replace an
    // authored nucleus with the ingestion placeholder, title, or seed metadata.
    assert_succeeds(
        &system,
        UPDATE_EXPORT_ID,
        serde_json::json!({"artifact_id": artifact_id,
            "title": "Authentication decisions", "content": "# Decisions\n\n[Canvas](lumvise://canvas/c1)",
            "tags": ["nucleus", "md-nucleus", "reviewed"],
            "metadata": {"nucleus": artifact["metadata"]["nucleus"], "reviewed": true}}),
    );
    let authored = invoke_value(
        &system,
        GET_EXPORT_ID,
        serde_json::json!({"artifact_id": artifact_id}),
    );
    let writes_before_reindex = broker.semantic_operation_count("upsert_artifact");
    assert_succeeds(
        &system,
        ELEMENT_TRIGGER_EXPORT_ID,
        serde_json::json!({
            "project_root": "/project", "base_revision": 1, "target_revision": 2,
            "changed": [{"entity_id": "md-file-auth", "entity_kind": "file", "disposition": "upserted"}]
        }),
    );
    assert_eq!(
        invoke_value(
            &system,
            GET_EXPORT_ID,
            serde_json::json!({"artifact_id": artifact_id})
        ),
        authored
    );
    assert_eq!(
        broker.semantic_operation_count("upsert_artifact"),
        writes_before_reindex
    );

    // Non-md files and md files outside the knowledge root produce no nuclei.
    let mut plain_file = test_element("plain-knowledge-file", "/project");
    plain_file.path = "knowledge/notes.txt".into();
    plain_file.element_kind = "file".into();
    plain_file.name = "notes.txt".into();
    let mut outside_md = test_element("outside-md-file", "/project");
    outside_md.path = "docs/decisions/auth.md".into();
    outside_md.element_kind = "file".into();
    outside_md.name = "auth.md".into();
    broker.sync_elements("/project", vec![plain_file, outside_md]);
    assert_succeeds(
        &system,
        ELEMENT_TRIGGER_EXPORT_ID,
        serde_json::json!({
            "project_root": "/project", "base_revision": 2, "target_revision": 3,
            "changed": [
                {"entity_id": "plain-knowledge-file", "entity_kind": "file",
                    "disposition": "upserted"},
                {"entity_id": "outside-md-file", "entity_kind": "file",
                    "disposition": "upserted"}
            ]
        }),
    );
    for element_id in ["plain-knowledge-file", "outside-md-file"] {
        let listed = invoke_value(
            &system,
            LIST_EXPORT_ID,
            serde_json::json!({"semantic_element_id": element_id}),
        );
        assert!(
            listed["artifacts"]
                .as_array()
                .is_some_and(|items| items.is_empty()),
            "element `{element_id}` must not gain a nucleus artifact"
        );
    }

    // A source tombstone cannot delete authored database Markdown.
    let mut tombstone = test_element("md-file-auth", "/project");
    tombstone.path = "knowledge/decisions/auth.md".into();
    tombstone.element_kind = "file".into();
    tombstone.name = "auth.md".into();
    tombstone.lifecycle = "inactive".into();
    broker.sync_elements("/project", vec![tombstone.clone()]);
    assert_succeeds(
        &system,
        ELEMENT_TRIGGER_EXPORT_ID,
        serde_json::json!({
            "project_root": "/project", "base_revision": 3, "target_revision": 4,
            "changed": [{"entity_id": "md-file-auth", "entity_kind": "file",
                "disposition": "removal"}]
        }),
    );
    assert_eq!(
        invoke_value(
            &system,
            GET_EXPORT_ID,
            serde_json::json!({"artifact_id": artifact_id})
        ),
        authored
    );

    // An untouched ingestion seed still follows source deletion. Restoring the
    // exact original seed exercises that independent cleanup branch.
    assert_succeeds(
        &system,
        UPDATE_EXPORT_ID,
        serde_json::json!({
            "artifact_id": artifact_id, "title": artifact["title"], "content": artifact["content"],
            "tags": artifact["tags"], "metadata": artifact["metadata"]
        }),
    );
    broker.sync_elements("/project", vec![tombstone]);
    assert_succeeds(
        &system,
        ELEMENT_TRIGGER_EXPORT_ID,
        serde_json::json!({
            "project_root": "/project", "base_revision": 4, "target_revision": 5,
            "changed": [{"entity_id": "md-file-auth", "entity_kind": "file", "disposition": "removal"}]
        }),
    );
    let outcome = system
        .invoke(
            PLUGIN_ID,
            GET_EXPORT_ID,
            serde_json::json!({"artifact_id": artifact_id}),
        )
        .expect("invoke get after nucleus removal");
    assert!(
        matches!(outcome, WireOutcome::Failed { ref error } if error.code == "knowledge_not_found"),
        "removed nucleus must fail closed: {outcome:?}"
    );

    system
        .stop(PLUGIN_ID)
        .expect("stop packaged Knowledge binary");
    system
        .uninstall(PLUGIN_ID)
        .expect("detach Knowledge package");
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
        artifact_kind: "definition".into(),
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

fn assert_http_knowledge_crud_matches_direct(system: &PluginSystem) {
    let artifact_id = "artifact:http/v2";
    let create_body = serde_json::json!({
        "artifact_id": artifact_id,
        "semantic_element_id": "element-a",
        "knowledge_type": "decision",
        "title": "HTTP v2",
        "content": "String identifiers survive the HTTP envelope.",
        "project_root": "/project"
    });
    let created = invoke_value(
        system,
        HTTP_CREATE_ARTIFACT_EXPORT_ID,
        http_envelope("/api/knowledge/artifacts", create_body),
    );
    assert_eq!(created["artifact"]["artifact_id"], artifact_id);
    assert_eq!(
        created,
        invoke_value(
            system,
            GET_EXPORT_ID,
            serde_json::json!({"artifact_id": artifact_id}),
        )
    );

    let updated = invoke_value(
        system,
        HTTP_UPDATE_ARTIFACT_EXPORT_ID,
        http_envelope(
            "/api/knowledge/artifacts/update",
            serde_json::json!({"artifact_id": artifact_id, "title": "HTTP v2 updated"}),
        ),
    );
    assert_eq!(updated["artifact"]["title"], "HTTP v2 updated");
    assert_eq!(
        updated,
        invoke_value(
            system,
            GET_EXPORT_ID,
            serde_json::json!({"artifact_id": artifact_id}),
        )
    );

    let direct_not_found = system
        .invoke(
            PLUGIN_ID,
            UPDATE_EXPORT_ID,
            serde_json::json!({"artifact_id": "artifact:missing", "title": "Missing"}),
        )
        .expect("direct missing update is a structured failure");
    let http_not_found = system
        .invoke(
            PLUGIN_ID,
            HTTP_UPDATE_ARTIFACT_EXPORT_ID,
            http_envelope(
                "/api/knowledge/artifacts/update",
                serde_json::json!({"artifact_id": "artifact:missing", "title": "Missing"}),
            ),
        )
        .expect("HTTP missing update is a structured failure");
    assert!(
        matches!(direct_not_found, WireOutcome::Failed { ref error } if error.code == "knowledge_not_found")
    );
    assert!(
        matches!(http_not_found, WireOutcome::Failed { ref error } if error.code == "knowledge_not_found")
    );

    let direct_mismatch = system
        .invoke(
            PLUGIN_ID,
            DELETE_EXPORT_ID,
            serde_json::json!({"artifact_id": artifact_id, "project_root": "/other"}),
        )
        .expect("direct project mismatch is a structured failure");
    let http_mismatch = system
        .invoke(
            PLUGIN_ID,
            HTTP_DELETE_ARTIFACT_EXPORT_ID,
            http_envelope(
                "/api/knowledge/artifacts/delete",
                serde_json::json!({"artifact_id": artifact_id, "project_root": "/other"}),
            ),
        )
        .expect("HTTP project mismatch is a structured failure");
    assert!(
        matches!(direct_mismatch, WireOutcome::Failed { ref error } if error.code == "knowledge_project_mismatch")
    );
    assert!(
        matches!(http_mismatch, WireOutcome::Failed { ref error } if error.code == "knowledge_project_mismatch")
    );

    let deleted = invoke_value(
        system,
        HTTP_DELETE_ARTIFACT_EXPORT_ID,
        http_envelope(
            "/api/knowledge/artifacts/delete",
            serde_json::json!({"artifact_id": artifact_id, "project_root": "/project"}),
        ),
    );
    assert_eq!(
        deleted,
        serde_json::json!({"artifact_id": artifact_id, "deleted": true})
    );
    let direct_deleted_again = invoke_value(
        system,
        DELETE_EXPORT_ID,
        serde_json::json!({"artifact_id": artifact_id, "project_root": "/project"}),
    );
    let http_deleted_again = invoke_value(
        system,
        HTTP_DELETE_ARTIFACT_EXPORT_ID,
        http_envelope(
            "/api/knowledge/artifacts/delete",
            serde_json::json!({"artifact_id": artifact_id, "project_root": "/project"}),
        ),
    );
    assert_eq!(direct_deleted_again, http_deleted_again);
    assert_eq!(http_deleted_again["deleted"], false);
}

fn http_envelope(path: &str, body: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "method": "POST",
        "path": path,
        "path_parameters": {},
        "query": {},
        "body": body,
        "body_size_bytes": 0
    })
}

fn assert_extended_exports(system: &PluginSystem, broker: &MemoryStorageBroker) {
    let get_semantic_tree_before = broker.plugin_invoke_count("get_semantic_tree");
    assert_http_knowledge_crud_matches_direct(system);
    assert_succeeds(
        system,
        FIND_ELEMENTS_EXPORT_ID,
        serde_json::json!({"query": "Element"}),
    );
    assert_succeeds(
        system,
        GET_ELEMENT_EXPORT_ID,
        serde_json::json!({"semantic_element_id": "element-a"}),
    );
    assert_succeeds(
        system,
        ASSISTANT_FIND_ELEMENTS_EXPORT_ID,
        serde_json::json!({"query": "Element"}),
    );
    assert_succeeds(
        system,
        ASSISTANT_GET_ELEMENT_EXPORT_ID,
        serde_json::json!({"semantic_element_id": "element-a"}),
    );
    assert_succeeds(
        system,
        ASSISTANT_GET_EXPORT_ID,
        serde_json::json!({"artifact_id": "artifact-a"}),
    );
    assert_succeeds(
        system,
        ASSISTANT_LIST_EXPORT_ID,
        serde_json::json!({"semantic_element_id": "element-a"}),
    );
    assert_eq!(
        broker.plugin_invoke_count("get_semantic_tree"),
        get_semantic_tree_before,
        "Knowledge HTTP create/update and get_element/list identity exports must use \
         storage.semantic Element directly, never plugin.invoke get_semantic_tree"
    );
    assert_succeeds(
        system,
        ASSISTANT_SEARCH_EXPORT_ID,
        serde_json::json!({"query": "rust"}),
    );
    let rebuilt = invoke_value(
        system,
        REBUILD_EXPORT_ID,
        serde_json::json!({"project_root": "/project"}),
    );
    assert_eq!(rebuilt["job"]["status"], "completed");
    assert_eq!(rebuilt["job"]["vectors_rebuilt"], 2); // artifact-z and artifact-a; artifact-path belongs to /other.

    let cultivation = invoke_value(
        system,
        RUN_CULTIVATION_EXPORT_ID,
        serde_json::json!({
            "mode": "cultivate_from_scratch", "project_root": "/project"
        }),
    );
    let run_id = cultivation["run"]["run_id"].as_str().expect("run id");
    assert_succeeds(
        system,
        GET_CULTIVATION_RUN_EXPORT_ID,
        serde_json::json!({"run_id": run_id}),
    );
    assert_succeeds(
        system,
        ENSURE_C4_EXPORT_ID,
        serde_json::json!({
            "project_root": "/project", "target_id": "element-a"
        }),
    );
    let report_before = invoke_value(
        system,
        GET_EXPORT_ID,
        serde_json::json!({"artifact_id": "knowledge-cultivation-report-c4-architecture-scoped-file-a"}),
    );
    assert_succeeds(
        system,
        DEBUG_C4_EXPORT_ID,
        serde_json::json!({
            "project_root": "/project", "target_id": "element-a"
        }),
    );

    let get_input = serde_json::json!({
        "method": "GET", "path": "/api/knowledge/export",
        "path_parameters": {}, "query": {"sourceId": "/project"},
        "body": null, "body_size_bytes": 0
    });
    for export in [
        HTTP_MANIFEST_EXPORT_ID,
        HTTP_SETUP_EXPORT_ID,
        HTTP_EXPORT_EXPORT_ID,
        HTTP_PROJECTION_ARTIFACTS_EXPORT_ID,
        HTTP_C4_ACTION_EXPORT_ID,
        HTTP_EVENTS_EXPORT_ID,
    ] {
        assert_succeeds(system, export, get_input.clone());
    }
    let mut page_input = get_input.clone();
    page_input["query"]["elementId"] = serde_json::json!("file:a");
    assert_succeeds(system, HTTP_PAGE_EXPORT_ID, page_input);
    let sync_input = serde_json::json!({
        "method": "POST", "path": "/api/knowledge/sync",
        "path_parameters": {}, "query": {},
        "body": {"schemaVersion": 1, "sourceId": "/project", "appliedRevision": null,
            "contentHashes": {}}, "body_size_bytes": 90
    });
    let snapshots_before_sync = broker.semantic_operation_count("project_snapshot");
    let project_delta = invoke_value(system, HTTP_SYNC_EXPORT_ID, sync_input.clone());
    assert_eq!(project_delta["sourceId"], "/project");
    assert_eq!(project_delta["schemaVersion"], 1);
    assert_eq!(project_delta["fullReset"], true);
    assert_eq!(project_delta["changedPages"][0]["elementId"], "file:a");
    assert!(
        project_delta["generatedAt"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert_eq!(
        broker.semantic_operation_count("project_snapshot"),
        snapshots_before_sync + 1
    );
    let cached_delta = invoke_value(system, HTTP_SYNC_EXPORT_ID, sync_input);
    assert_eq!(cached_delta["syncToken"], project_delta["syncToken"]);
    assert_eq!(
        broker.semantic_operation_count("project_snapshot"),
        snapshots_before_sync + 1
    );
    assert_succeeds(
        system,
        HTTP_C4_EXPORT_ID,
        serde_json::json!({
            "method": "POST", "path": "/api/knowledge/c4-nucleus", "query": {},
            "path_parameters": {}, "body": {"project_root": "/project", "target_id": "element-a"},
            "body_size_bytes": 52
        }),
    );
    assert_succeeds(
        system,
        HTTP_C4_DEBUG_EXPORT_ID,
        serde_json::json!({
            "method": "GET", "path": "/api/knowledge/c4-debug", "path_parameters": {},
            "query": {"project_root": "/project", "target_id": "element-a"},
            "body": null, "body_size_bytes": 0
        }),
    );
    assert_succeeds(
        system,
        HTTP_WRITE_EXPORT_ID,
        serde_json::json!({
            "method": "POST", "path": "/api/knowledge/write", "path_parameters": {}, "query": {},
            "body": {"artifact_id": "artifact-http", "semantic_element_id": "element-a",
                "knowledge_type": "annotation", "title": "HTTP", "content": "Written"},
            "body_size_bytes": 128
        }),
    );
    let report_after = invoke_value(
        system,
        GET_EXPORT_ID,
        serde_json::json!({"artifact_id": "knowledge-cultivation-report-c4-architecture-scoped-file-a"}),
    );
    assert_eq!(
        report_before["artifact"]["content"],
        report_after["artifact"]["content"]
    );
    let report_id = "knowledge-cultivation-report-c4-architecture-scoped-file-a";
    let wrong_project = system
        .invoke(
            PLUGIN_ID,
            DELETE_EXPORT_ID,
            serde_json::json!({"artifact_id": report_id, "project_root": "/other"}),
        )
        .expect("project-scoped delete failure");
    assert!(matches!(wrong_project, WireOutcome::Failed { .. }));
    let deleted = invoke_value(
        system,
        DELETE_EXPORT_ID,
        serde_json::json!({"artifact_id": report_id, "project_root": "/project"}),
    );
    assert_eq!(deleted["deleted"], true);
    let deleted_again = invoke_value(
        system,
        DELETE_EXPORT_ID,
        serde_json::json!({"artifact_id": report_id, "project_root": "/project"}),
    );
    assert_eq!(deleted_again["deleted"], false);
    let deletion_delta = invoke_value(
        system,
        HTTP_SYNC_EXPORT_ID,
        serde_json::json!({
            "method": "POST", "path": "/api/knowledge/sync",
            "path_parameters": {}, "query": {},
            "body": {"schemaVersion": 1, "sourceId": "/project",
                "appliedRevision": project_delta["targetRevision"],
                "contentHashes": project_delta["pageHashes"]},
            "body_size_bytes": 512
        }),
    );
    assert!(
        deletion_delta["deletedPageIds"]
            .as_array()
            .expect("deleted page ids")
            .iter()
            .any(|page_id| page_id == report_id)
    );
    assert_succeeds(
        system,
        HTTP_RESOLVE_TARGET_EXPORT_ID,
        serde_json::json!({
            "method": "POST", "path": "/api/knowledge/resolve-target", "path_parameters": {}, "query": {},
            "body": {"semantic_element_id": "element-a"}, "body_size_bytes": 35
        }),
    );
    assert_succeeds(
        system,
        HTTP_C4_EXPORT_ID,
        serde_json::json!({
            "method": "POST", "path": "/api/knowledge/c4-nucleus", "query": {},
            "path_parameters": {}, "body": {"project_root": "/project", "target_id": "element-a"},
            "body_size_bytes": 52
        }),
    );
    let project_artifacts_before = broker.semantic_operation_count("project_artifacts");
    let project_roots_before = broker.semantic_operation_count("project_roots");
    let live_events_before = broker.row_count(PLUGIN_ID, "knowledge_live_events");
    let snapshots_before_trigger = broker.semantic_operation_count("project_snapshot");
    let selective_before = broker.semantic_operation_count("selective_subgraph");
    let targeted_before = broker.semantic_operation_count("elements_by_ids_including_inactive");
    let element_trigger = serde_json::json!({
        "project_root": "/project", "base_revision": 0, "target_revision": 4,
        "changed": [
            {"entity_id": "element-a", "entity_kind": "semantic_element", "disposition": "upserted"},
            {"entity_id": "element-a", "entity_kind": "semantic_element", "disposition": "upserted"},
            {"entity_id": "missing", "entity_kind": "semantic_element", "disposition": "upserted"}
        ]
    });
    assert_succeeds(system, ELEMENT_TRIGGER_EXPORT_ID, element_trigger.clone());
    assert_eq!(
        broker.semantic_operation_count("project_snapshot"),
        snapshots_before_trigger
    );
    assert_eq!(
        broker.semantic_operation_count("project_roots"),
        project_roots_before,
        "StorageTrigger inheritance must derive candidates from bounded identity keys, \
         never enumerate every semantic project root"
    );
    assert_eq!(
        broker.semantic_operation_count("project_artifacts"),
        project_artifacts_before,
        "StorageTrigger must not scan project artifacts for implicit transfer"
    );
    assert!(broker.semantic_operation_count("selective_subgraph") > selective_before);
    let live_events_after = broker.row_count(PLUGIN_ID, "knowledge_live_events");
    assert!(live_events_after > live_events_before);
    assert_eq!(
        broker.semantic_operation_count("elements_by_ids_including_inactive"),
        targeted_before + 3,
        "StorageTrigger resolves cultivation anchors including parent traversal (2) and \
         md-nucleus changed-element identity (1), without automatic inheritance"
    );
    assert_succeeds(system, ELEMENT_TRIGGER_EXPORT_ID, element_trigger.clone());
    assert_eq!(
        broker.row_count(PLUGIN_ID, "knowledge_live_events"),
        live_events_after
    );
    broker.fail("storage.semantic", "injected_failure");
    let failed = system.invoke(PLUGIN_ID, ELEMENT_TRIGGER_EXPORT_ID, element_trigger);
    assert!(matches!(failed, Ok(WireOutcome::Failed { .. })));
    broker.allow_all();
    assert_succeeds(
        system,
        ELEMENT_TRIGGER_EXPORT_ID,
        serde_json::json!({
            "project_root": "/project",
            "base_revision": 0,
            "target_revision": 3,
            "changed": (1..=3).map(|index| serde_json::json!({
                "entity_id": format!("artifact-{index}"),
                "entity_kind": "semantic_element",
                "disposition": "upserted"
            })).collect::<Vec<_>>()
        }),
    );
    let mut first_poll = get_input.clone();
    first_poll["max_events"] = serde_json::json!(2);
    let first_events = invoke_value(system, HTTP_EVENTS_EXPORT_ID, first_poll);
    assert_eq!(first_events["events"].as_array().map(Vec::len), Some(2));
    assert_eq!(first_events["events"][0]["data"]["sourceId"], "/project");
    assert_eq!(first_events["next_cursor"], "3:artifact-2");
    assert_eq!(first_events["done"], false);
    let mut second_poll = get_input;
    second_poll["cursor"] = first_events["next_cursor"].clone();
    second_poll["max_events"] = serde_json::json!(2);
    let second_events = invoke_value(system, HTTP_EVENTS_EXPORT_ID, second_poll);
    assert!(second_events["events"].is_array());
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
