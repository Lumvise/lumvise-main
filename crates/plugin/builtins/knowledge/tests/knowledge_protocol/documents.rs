use super::*;
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use lumvise_plugin_semantic::SemanticPlugin;
use serde_json::{Value, json};

struct SemanticDocumentHost<'a>(&'a MemoryStorageBroker);

impl HostCallTransport for SemanticDocumentHost<'_> {
    fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
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
            "content": "# Revised architecture"
        }),
    );
    assert_eq!(updated["document"]["revision"], 2);
    assert_eq!(
        process.call("list_documents", json!({"project_root": "/project"}))["total"],
        1
    );
    let search = process.call(
        "search_documents",
        json!({"project_root": "/project", "query": "architecture"}),
    );
    assert_eq!(search["documents"][0]["document_id"], "report:independent");
    assert_eq!(broker.plugin_invoke_count("ingest_index_batch"), 2);
    process.call("delete_document", identity);
    assert_eq!(
        process.call("list_documents", json!({"project_root": "/project"}))["total"],
        0
    );
    process.stop();
}

#[test]
fn legacy_conversion_preview_and_apply_round_trip_through_the_real_protobuf_process() {
    let broker = Arc::new(MemoryStorageBroker::default());
    ingest(&broker, json!({"provider_instance_id": "filesystem", "project_root": "/project",
        "semantic_sources": [{"semantic_source_id": "source:files", "kind": "repository", "name": "Sources", "root_path": "/project", "root_uri": "file:///project"}],
        "semantic_elements": [{"semantic_element_id": "file:lib", "semantic_source_id": "source:files", "path": "src/lib.rs",
            "semantic_element_type": "file", "semantic_element_name": "lib.rs", "parent_element_id": null,
            "start_line": 1, "end_line": 3, "content_fingerprint": "fp1:0000000000000001:source-v1", "metadata": {}}],
        "semantic_relationships": [], "semantic_artifacts": [], "replace_paths": [], "removed_paths": []
    })).unwrap();
    let mut process = DocumentProcess::launch(broker.clone());
    let mut historical = test_artifact("report:legacy", "file:lib", "Existing report");
    historical.artifact_kind = "report".into();
    historical.content = Some("# Keep this text".into());
    historical.metadata["knowledge"]["project_root"] = json!("/project");
    broker.upsert_artifact(historical, "text/markdown");
    let preview = process.call(
        "convert_legacy_documents",
        json!({"project_root": "/project"}),
    );
    assert_eq!(preview["preview"], true);
    assert_eq!(preview["total"], 1);
    assert_eq!(
        process.call("list_documents", json!({"project_root": "/project"}))["total"],
        0
    );
    let applied = process.call(
        "convert_legacy_documents",
        json!({"project_root": "/project", "preview": false}),
    );
    assert_eq!(applied["preview"], false);
    assert_eq!(applied["conversions"][0]["document_id"], "report:legacy");
    process.stop();
    let mut reopened = DocumentProcess::launch(broker);
    let document = reopened.call(
        "get_document",
        json!({"project_root": "/project", "document_id": "report:legacy"}),
    );
    assert_eq!(document["document"]["content"], "# Keep this text");
    assert_eq!(
        reopened.call(
            "convert_legacy_documents",
            json!({"project_root": "/project", "preview": false})
        )["total"],
        0
    );
    reopened.stop();
}
