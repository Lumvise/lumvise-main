use super::*;
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use lumvise_plugin_semantic::SemanticPlugin;
use serde_json::{Value, json};

struct SemanticDocumentHost<'a>(&'a MemoryStorageBroker);

impl HostCallTransport for SemanticDocumentHost<'_> {
    fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
        if capability == "storage.semantic" {
            self.0
                .take_operation_fault(input["operation"].as_str().unwrap_or_default())
                .map_err(|error| {
                    PluginError::new("document_protocol_host", error.to_string(), false)
                })?;
        }
        self.0
            .semantic
            .invoke(HostCapabilityRequest {
                plugin_id: "builtin.semantic".into(),
                invocation_id: "document-protocol".into(),
                call_id: "document-storage".into(),
                capability_id: capability.into(),
                required_version: if capability == "storage.semantic" {
                    "^1.4.0"
                } else {
                    "^1.0"
                }
                .into(),
                input,
            })
            .map_err(|error| PluginError::new("document_protocol_host", error.to_string(), false))
    }
}

pub(super) fn ingest(
    broker: &MemoryStorageBroker,
    input: Value,
) -> Result<Value, HostCapabilityError> {
    let mut transport = SemanticDocumentHost(broker);
    SemanticPlugin
        .dispatch(
            "ingest_index_batch",
            input,
            &mut PluginContext::for_test(&mut transport),
        )
        .map(|output| json!({"output": output}))
        .map_err(|error| {
            HostCapabilityError::new("plugin.invoke", error.code, error.message, error.retryable)
        })
}

struct DocumentProcess {
    plugin: KnowledgePluginProcess,
    broker: Arc<MemoryStorageBroker>,
}

impl DocumentProcess {
    fn launch(broker: Arc<MemoryStorageBroker>) -> Self {
        let mut plugin = KnowledgePluginProcess::launch();
        let ready = plugin.exchange(MessageBody::HostHello {
            session_id: "documents".into(),
            host_id: "documents-test".into(),
            package_digest: "test-package".into(),
        });
        assert!(matches!(ready.body, MessageBody::PluginReady { .. }));
        Self { plugin, broker }
    }

    fn call(&mut self, export: &str, input: Value) -> Value {
        let mut response = self.plugin.exchange(MessageBody::HostInvoke {
            session_id: "documents".into(),
            invocation_id: export.into(),
            capability_id: export.into(),
            input,
        });
        loop {
            match response.body {
                MessageBody::PluginResult {
                    outcome: WireOutcome::Succeeded { value },
                    ..
                } => return value,
                MessageBody::PluginHostCall {
                    session_id,
                    invocation_id,
                    call_id,
                    capability_id,
                    input,
                } => {
                    let value = self
                        .broker
                        .invoke(HostCapabilityRequest {
                            plugin_id: PLUGIN_ID.into(),
                            invocation_id,
                            call_id: call_id.clone(),
                            capability_id,
                            required_version: "^1.4.0".into(),
                            input,
                        })
                        .expect("document host request");
                    response = self.plugin.exchange(MessageBody::HostHostResult {
                        session_id,
                        call_id,
                        outcome: WireOutcome::Succeeded { value },
                    });
                }
                result => panic!("document export {export} failed: {result:?}"),
            }
        }
    }

    /// Like `call`, but returns the plugin's failure code instead of panicking.
    fn try_call(&mut self, export: &str, input: Value) -> Result<Value, String> {
        let mut response = self.plugin.exchange(MessageBody::HostInvoke {
            session_id: "documents".into(),
            invocation_id: export.into(),
            capability_id: export.into(),
            input,
        });
        loop {
            match response.body {
                MessageBody::PluginResult {
                    outcome: WireOutcome::Succeeded { value },
                    ..
                } => return Ok(value),
                MessageBody::PluginResult {
                    outcome: WireOutcome::Failed { error },
                    ..
                } => return Err(error.code),
                MessageBody::PluginHostCall {
                    session_id,
                    invocation_id,
                    call_id,
                    capability_id,
                    input,
                } => {
                    let outcome = match self.broker.invoke(HostCapabilityRequest {
                        plugin_id: PLUGIN_ID.into(),
                        invocation_id,
                        call_id: call_id.clone(),
                        capability_id,
                        required_version: "^1.4.0".into(),
                        input,
                    }) {
                        Ok(value) => WireOutcome::Succeeded { value },
                        Err(error) => WireOutcome::Failed {
                            error: lumvise_plugin_protocol::PluginWireError {
                                code: "host_capability_failed".into(),
                                message: error.to_string(),
                                details: None,
                                retryable: false,
                            },
                        },
                    };
                    response = self.plugin.exchange(MessageBody::HostHostResult {
                        session_id,
                        call_id,
                        outcome,
                    });
                }
                result => panic!("document export {export} did not reach a result: {result:?}"),
            }
        }
    }

    fn stop(mut self) {
        let result = self.plugin.exchange(MessageBody::HostShutdown {
            session_id: "documents".into(),
            reason: Some("test complete".into()),
        });
        assert!(matches!(result.body, MessageBody::PluginStopped { .. }));
        self.plugin.wait_for_success();
    }
}

#[test]
fn exact_document_write_acknowledgements_cross_the_real_protobuf_boundary() {
    let broker = Arc::new(MemoryStorageBroker::default());
    let mut process = DocumentProcess::launch(broker);
    let created = process.call(
        "create_document",
        json!({"operation_id":"protobuf-document-create",
        "project_root":"/project","document_id":"acknowledged-report","document_kind":"report",
        "title":"Report","content":"Body stays with the document owner"}),
    );
    let reply = process.call(
        "get_document_write_receipt",
        json!({"project_root":"/project",
        "operation_id":"protobuf-document-create","action":"create"}),
    );
    let typed: lumvise_contracts::KnowledgeDocumentWriteReceipt =
        serde_json::from_value(reply).unwrap();
    assert_eq!(
        typed.state,
        lumvise_contracts::KnowledgeWriteState::Accepted
    );
    assert_eq!(
        typed.document_revision.unwrap().semantic_element_id,
        created["document"]["semantic_element_id"]
    );
    assert!(typed.commit_version.unwrap() > 0);
    assert_eq!(
        typed.lifecycle_revision,
        created["document"]["revision"].as_u64()
    );
    process.stop();
}

#[test]
fn exact_artifact_write_acknowledgements_cross_the_real_protobuf_boundary() {
    let broker = Arc::new(MemoryStorageBroker::default());
    let mut process = DocumentProcess::launch(broker);
    let document = process.call(
        "create_document",
        json!({"project_root":"/project",
        "document_id":"artifact-owner","document_kind":"report","title":"Owner","content":"Body"}),
    );
    let created = process.call("create_knowledge",json!({"operation_id":"protobuf-artifact-create",
        "project_root":"/project","artifact_id":"protobuf-artifact","semantic_element_id":document["document"]["semantic_element_id"],
        "knowledge_type":"specification","title":"Spec","content":"Original artifact"}));
    let reply = process.call(
        "get_artifact_write_receipt",
        json!({"project_root":"/project",
        "operation_id":"protobuf-artifact-create","action":"create"}),
    );
    let typed: lumvise_contracts::KnowledgeArtifactWriteReceipt =
        serde_json::from_value(reply).unwrap();
    assert_eq!(
        typed.state,
        lumvise_contracts::KnowledgeWriteState::Accepted
    );
    assert_eq!(
        typed.artifact_revision.unwrap().artifact_id,
        created["artifact"]["artifact_id"]
    );
    assert_eq!(
        typed.lifecycle_revision,
        created["artifact"]["revision"].as_u64()
    );
    assert!(typed.commit_version.unwrap() > 0);
    process.stop();
}

#[test]
fn independent_documents_round_trip_through_the_real_protobuf_process() {
    let broker = Arc::new(MemoryStorageBroker::default());
    let mut process = DocumentProcess::launch(broker.clone());
    let identity = json!({"document_id": "report:independent", "project_root": "/project"});
    let created = process.call(
        "create_document",
        json!({
            "document_id": "report:independent", "project_root": "/project", "document_kind": "report",
            "title": "Project architecture", "content": "# Independent knowledge", "path": "Technical/Overview"
        }),
    );
    assert_eq!(process.call("get_document", identity.clone()), created);
    let updated = process.call(
        "update_document",
        json!({
            "document_id": "report:independent", "project_root": "/project", "expected_revision": 1,
            "expected_semantic_element_id":created["document"]["semantic_element_id"],
            "content": "# Revised architecture"
        }),
    );
    assert_eq!(updated["document"]["revision"], 2);
    assert_ne!(
        updated["document"]["semantic_element_id"],
        created["document"]["semantic_element_id"]
    );
    assert_eq!(
        process.call("list_documents", json!({"project_root": "/project"}))["total"],
        1
    );
    let search = process.call(
        "search_documents",
        json!({"project_root": "/project", "query": "architecture"}),
    );
    assert_eq!(search["documents"][0]["document_id"], "report:independent");
    // Create publishes its project root and then its pointer; the update publishes only its pointer.
    assert_eq!(broker.plugin_invoke_count("ingest_index_batch"), 3);
    let mut removal = identity;
    removal["expected_revision"] = updated["document"]["revision"].clone();
    removal["expected_semantic_element_id"] = updated["document"]["semantic_element_id"].clone();
    process.call("delete_document", removal);
    assert_eq!(
        process.call("list_documents", json!({"project_root": "/project"}))["total"],
        0
    );
    process.stop();
}

#[test]
fn a_failed_body_stage_through_the_real_protobuf_process_keeps_the_previous_body() {
    let broker = Arc::new(MemoryStorageBroker::default());
    let mut process = DocumentProcess::launch(broker.clone());
    let identity = json!({"document_id": "report:process", "project_root": "/project"});
    let created = process.call(
        "create_document",
        json!({
            "document_id": "report:process", "project_root": "/project", "document_kind": "report",
            "title": "Process body", "content": "# Process body marker", "path": "Technical/Process"
        }),
    );
    let revised = json!({
        "document_id": "report:process", "project_root": "/project",
        "expected_revision": 1, "expected_semantic_element_id":created["document"]["semantic_element_id"], "content": "# Revised"
    });
    broker.fail_operation_after("commit_project_snapshot", 0);
    assert!(
        process
            .try_call("update_document", revised.clone())
            .is_err()
    );
    assert_eq!(process.call("get_document", identity.clone()), created);
    let retried = process.call("update_document", revised);
    assert_eq!(retried["document"]["revision"], 2);
    assert_eq!(
        process.call("get_document", identity)["document"]["content"],
        "# Revised"
    );
    process.stop();
}

#[test]
fn a_failed_pointer_publication_through_the_real_protobuf_process_keeps_the_previous_body() {
    let broker = Arc::new(MemoryStorageBroker::default());
    let mut process = DocumentProcess::launch(broker.clone());
    let identity = json!({"document_id": "report:process", "project_root": "/project"});
    let created = process.call(
        "create_document",
        json!({
            "document_id": "report:process", "project_root": "/project", "document_kind": "report",
            "title": "Process body", "content": "# Process body marker", "path": "Technical/Process"
        }),
    );
    let revised = json!({
        "document_id": "report:process", "project_root": "/project",
        "expected_revision": 1, "expected_semantic_element_id":created["document"]["semantic_element_id"], "content": "# Revised"
    });
    broker.fail_operation_after("ingest_index_batch", 0);
    assert!(
        process
            .try_call("update_document", revised.clone())
            .is_err()
    );
    assert_eq!(process.call("get_document", identity.clone()), created);
    let retried = process.call("update_document", revised);
    assert_eq!(retried["document"]["revision"], 2);
    process.stop();
}
