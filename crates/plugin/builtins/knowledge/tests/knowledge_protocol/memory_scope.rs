//! Signed plugin calls exercise the host authority and real persistence across restart.
use super::*;
use lumvise_plugin_runtime::{
    PluginInvocationClass, PluginInvocationContext, PluginInvocationRequest,
};
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

struct ScopedMemoryBroker {
    host: AppCoreHostCapabilityBroker,
    nested_paths: Mutex<Vec<Vec<String>>>,
    lose_publication_reply: AtomicBool,
}

impl ScopedMemoryBroker {
    fn open(path: &Path) -> Self {
        let persistence = Arc::new(LocalPersistence::open(path).unwrap());
        Self {
            host: AppCoreHostCapabilityBroker::new(persistence.clone(), persistence),
            nested_paths: Mutex::default(),
            lose_publication_reply: AtomicBool::new(false),
        }
    }

    fn bind(&self, session: &str, active: bool) {
        self.host.invoke_controlled(request("builtin.assistant", "runtime.knowledge_scope", json!({
            "operation":"bind_assistant", "session_id":session, "session_epoch":1, "active":active
        })), &scope(session)).unwrap();
    }
}

impl HostCapabilityBroker for ScopedMemoryBroker {
    fn invoke(&self, request: HostCapabilityRequest) -> Result<Value, HostCapabilityError> {
        Err(host_error(
            &request.capability_id,
            "controlled_scope_required",
        ))
    }

    fn invoke_controlled(
        &self,
        request: HostCapabilityRequest,
        context: &PluginInvocationContext,
    ) -> Result<Value, HostCapabilityError> {
        if request.capability_id != "fixture.semantic.ingest" {
            return self.host.invoke_controlled(request, context);
        }
        self.nested_paths.lock().unwrap().push(
            context
                .plugin_chain()
                .iter()
                .map(|frame| format!("{}:{}", frame.plugin_id, frame.export_id))
                .collect(),
        );
        let mut transport = ScopedSemanticTransport {
            host: &self.host,
            context,
        };
        let output = lumvise_plugin_semantic::SemanticPlugin
            .dispatch(
                "ingest_index_batch",
                request.input,
                &mut PluginContext::for_test(&mut transport),
            )
            .map_err(|error| {
                HostCapabilityError::new(
                    "fixture.semantic.ingest",
                    error.code,
                    error.message,
                    error.retryable,
                )
            })?;
        if self.lose_publication_reply.swap(false, Ordering::SeqCst) {
            return Err(host_error(
                "fixture.semantic.ingest",
                "lost_publication_reply",
            ));
        }
        Ok(json!({"output":output}))
    }
}

struct ScopedSemanticTransport<'a> {
    host: &'a AppCoreHostCapabilityBroker,
    context: &'a PluginInvocationContext,
}

impl HostCallTransport for ScopedSemanticTransport<'_> {
    fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
        self.host
            .invoke_controlled(request("builtin.semantic", capability, input), self.context)
            .map_err(|error| PluginError::new("scoped_semantic_host", error.to_string(), false))
    }
}

fn request(plugin: &str, capability: &str, input: Value) -> HostCapabilityRequest {
    HostCapabilityRequest {
        plugin_id: plugin.into(),
        invocation_id: "memory-proof".into(),
        call_id: "memory-call".into(),
        capability_id: capability.into(),
        required_version: "^1.0".into(),
        input,
    }
}

fn scope(session: &str) -> PluginInvocationContext {
    PluginInvocationContext::new(
        "memory-proof",
        "builtin.assistant",
        PluginInvocationClass::Foreground,
        Instant::now() + Duration::from_secs(60),
    )
    .with_route_identity(session, Some("assistant_session".into()))
    .with_scope_epoch(Some(1))
}

fn start(workspace: &tempfile::TempDir, broker: Arc<ScopedMemoryBroker>) -> PluginSystem {
    let system = start_packaged_knowledge(workspace, broker);
    let installed = signed_semantic_fixture(workspace.path(), &SigningKey::from_bytes(&[43; 32]));
    system.install(&installed).unwrap();
    system.start("builtin.semantic").unwrap();
    system
}

fn call(system: &PluginSystem, session: &str, export: &str, input: Value) -> WireOutcome {
    system
        .invoke_controlled(PluginInvocationRequest::new(
            PLUGIN_ID,
            export,
            input,
            scope(session),
        ))
        .unwrap_or_else(|error| panic!("runtime {export}: {error:?}"))
}

fn output(outcome: WireOutcome) -> Value {
    match outcome {
        WireOutcome::Succeeded { value } => value,
        other => panic!("expected successful memory call, got {other:?}"),
    }
}

fn stop(system: PluginSystem, workspace: &tempfile::TempDir) {
    system.stop(PLUGIN_ID).unwrap();
    system.stop("builtin.semantic").unwrap();
    drop(system);
    make_tree_writable(workspace.path());
}

#[test]
fn signed_memory_survives_conversations_and_disk_reopen_and_rejects_raw_access() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("memory.db");
    let broker = Arc::new(ScopedMemoryBroker::open(&path));
    let workspace = tempfile::tempdir().unwrap();
    let system = start(&workspace, broker.clone());
    broker.bind("first", true);
    let space = output(call(
        &system,
        "first",
        "resolve_document_space",
        json!({"purpose":"self"}),
    ));
    let root = space["space"]["project_root"].as_str().unwrap();
    let created = output(call(
        &system,
        "first",
        "create_document",
        json!({
            "project_root":root, "document_id":"preferences", "document_kind":"context",
            "title":"Communication", "content":"Use concise replies", "operation_id":"remember-preferences"
        }),
    ));
    let identity = json!({"project_root":root, "document_id":"preferences"});
    broker.bind("first", false);
    assert!(matches!(
        call(&system, "first", "get_document", identity.clone()),
        WireOutcome::Failed { .. }
    ));
    broker.bind("second", true);
    assert_eq!(
        created,
        output(call(&system, "second", "get_document", identity.clone()))
    );
    assert_eq!(
        output(call(&system, "second", "list_documents", json!({})))["total"],
        0
    );
    assert_eq!(
        output(call(
            &system,
            "second",
            "list_documents",
            json!({"project_root":root})
        ))["documents"][0],
        created["document"]
    );
    assert_raw_batch_is_filtered(&broker, &created["document"]);
    assert_raw_access_denied(&broker, root, &created["document"]);
    assert!(broker.nested_paths.lock().unwrap().iter().any(|path| path
        == &vec![
            "builtin.knowledge:create_document".to_string(),
            "builtin.semantic:ingest_index_batch".to_string()
        ]));
    stop(system, &workspace);
    drop(broker);

    let reopened = Arc::new(ScopedMemoryBroker::open(&path));
    let next_workspace = tempfile::tempdir().unwrap();
    let next = start(&next_workspace, reopened.clone());
    assert!(matches!(
        call(&next, "second", "get_document", identity.clone()),
        WireOutcome::Failed { .. }
    ));
    reopened.bind("third", true);
    assert_eq!(
        space,
        output(call(
            &next,
            "third",
            "resolve_document_space",
            json!({"purpose":"self"})
        ))
    );
    assert_eq!(
        created,
        output(call(&next, "third", "get_document", identity))
    );
    stop(next, &next_workspace);
}

fn assert_raw_batch_is_filtered(broker: &ScopedMemoryBroker, document: &Value) {
    let id = raw_document_body_id(broker, document);
    for plugin in ["builtin.semantic", "builtin.knowledge"] {
        let input = json!({"operation":"artifacts_by_ids", "artifact_ids":[id]});
        let hidden = broker
            .host
            .invoke_controlled(request(plugin, "storage.semantic", input), &scope("second"))
            .unwrap();
        assert_eq!(hidden["artifacts"], json!([]));
    }
}

fn raw_document_body_id(broker: &ScopedMemoryBroker, document: &Value) -> Value {
    let selected = broker
        .host
        .invoke(request(
            "builtin.semantic",
            "storage.semantic",
            json!({"operation":"element", "semantic_element_id":document["semantic_element_id"]}),
        ))
        .unwrap();
    let id =
        &selected["element"]["metadata"]["indexer_metadata"]["knowledge_document"]["artifact_id"];
    assert!(
        id.is_string(),
        "expected exact document body id: {document}"
    );
    id.clone()
}

fn assert_raw_access_denied(broker: &ScopedMemoryBroker, root: &str, document: &Value) {
    let input =
        json!({"operation":"element", "semantic_element_id":document["semantic_element_id"]});
    let hidden = broker
        .host
        .invoke_controlled(
            request("builtin.semantic", "storage.semantic", input),
            &scope("second"),
        )
        .unwrap();
    assert!(hidden["element"].is_null());
    for input in [
        json!({"operation":"project_artifacts", "project_root":root, "artifact_namespace":null}),
        json!({"operation":"project_snapshot", "scope":{"project_root":root}, "artifact_namespace":null}),
        json!({"operation":"search_element_candidates", "project_root":root, "query":"Communication", "limit":10}),
    ] {
        assert!(
            broker
                .host
                .invoke_controlled(
                    request("builtin.semantic", "storage.semantic", input),
                    &scope("second")
                )
                .is_err()
        );
    }
    assert!(
        broker
            .host
            .invoke_controlled(
                request(
                    "builtin.semantic",
                    "semantic.snapshot",
                    json!({"operation":"create", "project_root":root})
                ),
                &scope("second")
            )
            .is_err()
    );
}

#[test]
fn two_signed_instances_resolve_one_space_concurrently() {
    let directory = tempfile::tempdir().unwrap();
    let broker = Arc::new(ScopedMemoryBroker::open(
        &directory.path().join("memory.db"),
    ));
    broker.bind("left", true);
    broker.bind("right", true);
    let left_workspace = tempfile::tempdir().unwrap();
    let right_workspace = tempfile::tempdir().unwrap();
    let left = start(&left_workspace, broker.clone());
    let right = start(&right_workspace, broker.clone());
    let (a, b) = std::thread::scope(|threads| {
        let a = threads.spawn(|| {
            output(call(
                &left,
                "left",
                "resolve_document_space",
                json!({"purpose":"self"}),
            ))
        });
        let b = threads.spawn(|| {
            output(call(
                &right,
                "right",
                "resolve_document_space",
                json!({"purpose":"self"}),
            ))
        });
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_eq!(a, b);
    let rows = broker
        .host
        .invoke(request(
            "builtin.knowledge",
            "storage.semantic",
            json!({
                "operation":"project_snapshot", "scope":{"project_root":a["space"]["project_root"]}
            }),
        ))
        .unwrap();
    assert_eq!(
        rows["elements"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["element_kind"] == "virtual_document_space")
            .count(),
        1
    );
    stop(left, &left_workspace);
    stop(right, &right_workspace);
}

#[test]
fn private_memory_retries_recover_receipts_and_reject_stale_bases() {
    let directory = tempfile::tempdir().unwrap();
    let broker = Arc::new(ScopedMemoryBroker::open(
        &directory.path().join("memory.db"),
    ));
    broker.bind("writer", true);
    let workspace = tempfile::tempdir().unwrap();
    let system = start(&workspace, broker.clone());
    let space = output(call(
        &system,
        "writer",
        "resolve_document_space",
        json!({"purpose":"self"}),
    ));
    let root = &space["space"]["project_root"];
    let request = json!({"project_root":root, "document_id":"decision", "document_kind":"context",
        "title":"Original", "content":"Keep original base", "operation_id":"initial"});
    broker.lose_publication_reply.store(true, Ordering::SeqCst);
    assert!(matches!(
        call(&system, "writer", "create_document", request.clone()),
        WireOutcome::Failed { .. }
    ));
    let saved = output(call(&system, "writer", "create_document", request.clone()));
    assert_eq!(
        saved,
        output(call(&system, "writer", "create_document", request))
    );
    let current = &saved["document"];
    let edit = json!({"project_root":root, "document_id":"decision", "title":"Human edit",
        "expected_revision":current["revision"], "expected_semantic_element_id":current["semantic_element_id"]});
    let accepted = output(call(&system, "writer", "update_document", edit.clone()));
    let mut stale = edit;
    stale["title"] = json!("Stale overwrite");
    assert!(matches!(
        call(&system, "writer", "update_document", stale),
        WireOutcome::Failed { .. }
    ));
    assert_eq!(
        accepted,
        output(call(
            &system,
            "writer",
            "get_document",
            json!({"project_root":root, "document_id":"decision"})
        ))
    );
    stop(system, &workspace);
}
