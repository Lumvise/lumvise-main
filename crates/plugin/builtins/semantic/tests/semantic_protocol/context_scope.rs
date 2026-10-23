use super::*;
use lumvise_plugin_runtime::{
    HostCapabilityBroker, HostCapabilityError, HostCapabilityRequest, PluginRuntimeConfig,
    PluginSystem,
};
use serde_json::Value;
use std::collections::HashSet;

#[test]
fn semantic_context_returns_complete_snapshot_with_metadata() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[49; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(Arc::clone(&broker));
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    let mut batch = index_batch();
    batch["semantic_elements"][1]["content_fingerprint"] =
        serde_json::json!("fp1:0000000000000001:parse");
    batch["semantic_elements"][1]["metadata"] = serde_json::json!({
        "signature": "fn parse(input: &str) -> Result<Tree>"
    });
    batch["semantic_relationships"][1]["target_locator"] = serde_json::json!("crate::render");
    batch["semantic_artifacts"] = serde_json::json!([{
        "artifact_id": "parse-source", "semantic_element_id": "fn:parse",
        "artifact_kind": "source", "title": "Parse source",
        "content": "fn parse(input: &str) -> Result<Tree>",
        "searchable_text": "parse input tree", "content_size_bytes": 42,
        "metadata": {"language": "rust"}
    }]);
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, batch)
            .expect("ingest context fixture"),
    );

    broker.reset_project_snapshot_calls();
    let elements = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEMANTIC_CONTEXT_EXPORT_ID,
                serde_json::json!({"project_root": "/work/demo", "record_kind": "elements"}),
            )
            .expect("complete element snapshot"),
    );
    assert_eq!(elements["record_kind"], "elements");
    assert_eq!(elements["elements"].as_array().map(Vec::len), Some(3));
    assert!(elements["commit_version"].as_i64().unwrap_or_default() > 0);
    assert!(elements["published_at"].is_string());
    assert!(!elements.as_object().unwrap().contains_key("next_after_key"));

    let relationships = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEMANTIC_CONTEXT_EXPORT_ID,
                serde_json::json!({"project_root": "/work/demo", "record_kind": "relationships"}),
            )
            .expect("complete relationship snapshot"),
    );
    let relationship_records = relationships["relationships"]
        .as_array()
        .expect("relationships");
    assert_eq!(relationship_records.len(), 3);
    assert!(
        relationship_records
            .iter()
            .any(|item| item["metadata"]["target_locator"] == "crate::render")
    );
    assert!(
        !relationships
            .as_object()
            .unwrap()
            .contains_key("next_after_key")
    );

    let artifacts = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEMANTIC_CONTEXT_EXPORT_ID,
                serde_json::json!({"project_root": "/work/demo", "record_kind": "artifacts"}),
            )
            .expect("complete artifact snapshot"),
    );
    assert_eq!(artifacts["artifacts"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        artifacts["artifacts"][0]["content"],
        "fn parse(input: &str) -> Result<Tree>"
    );
    assert_eq!(broker.unprefixed_element_lists(), 0);
    assert_eq!(broker.full_context_scans(), 0);
    assert_eq!(broker.project_snapshot_calls(), 2);
    assert_eq!(broker.semantic_operation_count("project_artifacts"), 1);
    runtime.stop(PLUGIN_ID).expect("stop");
}

struct ContextReadBroker {
    inner: Arc<MemoryCapabilityBroker>,
    structure_scopes: std::sync::Mutex<Vec<Value>>,
    structure_element_counts: std::sync::Mutex<Vec<usize>>,
    artifact_records: std::sync::Mutex<Option<Value>>,
    semantic_operations: std::sync::Mutex<Vec<String>>,
    change_structure_revision: bool,
    change_final_revision: bool,
    mismatch_structure_project: bool,
}

impl ContextReadBroker {
    fn new(inner: Arc<MemoryCapabilityBroker>) -> Self {
        Self {
            inner,
            structure_scopes: std::sync::Mutex::default(),
            structure_element_counts: std::sync::Mutex::default(),
            artifact_records: std::sync::Mutex::default(),
            semantic_operations: std::sync::Mutex::default(),
            change_structure_revision: false,
            change_final_revision: false,
            mismatch_structure_project: false,
        }
    }

    fn reset(&self) {
        self.structure_scopes.lock().unwrap().clear();
        self.structure_element_counts.lock().unwrap().clear();
        self.artifact_records.lock().unwrap().take();
        self.semantic_operations.lock().unwrap().clear();
        self.inner.reset_project_snapshot_calls();
        self.inner.reset_semantic_operation_counts();
    }
}

impl HostCapabilityBroker for ContextReadBroker {
    fn invoke(
        &self,
        request: HostCapabilityRequest,
    ) -> Result<serde_json::Value, HostCapabilityError> {
        let operation = request.input["operation"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let is_semantic = request.capability_id == "storage.semantic";
        let is_structure = is_semantic && operation == "scoped_read";
        let change_final_revision = self.change_final_revision
            && is_semantic
            && operation == "semantic_revision"
            && self
                .semantic_operations
                .lock()
                .unwrap()
                .iter()
                .filter(|operation| operation.as_str() == "semantic_revision")
                .count()
                == 1;
        if is_semantic {
            self.semantic_operations
                .lock()
                .unwrap()
                .push(operation.clone());
        }
        if is_structure {
            self.structure_scopes
                .lock()
                .unwrap()
                .push(request.input["request"]["Structure"]["scope"].clone());
        }
        let mut output = self.inner.invoke(request)?;
        if is_structure {
            self.structure_element_counts
                .lock()
                .unwrap()
                .push(output["elements"].as_array().map_or(0, Vec::len));
            if self.change_structure_revision {
                output["commit_version"] =
                    serde_json::json!(output["commit_version"].as_i64().unwrap() + 1);
            }
            if self.mismatch_structure_project {
                output["project_root"] = serde_json::json!("/work/foreign");
            }
        }
        if change_final_revision {
            output["commit_version"] =
                serde_json::json!(output["commit_version"].as_i64().unwrap() + 1);
        }
        if is_semantic && operation == "project_artifacts" {
            *self.artifact_records.lock().unwrap() = Some(output["artifacts"].clone());
        }
        Ok(output)
    }
}

#[test]
fn unscoped_artifact_context_uses_one_owner_and_preserves_all_records() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[75; 32]));
    let inner = Arc::new(MemoryCapabilityBroker::default());
    let broker = Arc::new(ContextReadBroker::new(Arc::clone(&inner)));
    let runtime = PluginSystem::with_broker_and_sandbox(
        PluginRuntimeConfig::default(),
        broker.clone(),
        Arc::new(crate::support::TestSubprocessSandbox),
    );
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    let mut batch = index_batch();
    batch["semantic_artifacts"] = serde_json::json!([{
        "artifact_id": "full-context-record", "semantic_element_id": "fn:parse",
        "artifact_kind": "report", "title": "Full context record",
        "content_ref": "knowledge://artifact/full-context-record",
        "content": "complete artifact body", "searchable_text": "search body",
        "content_size_bytes": 22, "metadata": {"nested": {"preserved": true}}
    }, {
        "artifact_id": "other-owner-record", "semantic_element_id": "fn:render",
        "artifact_kind": "report", "title": "Other owner record",
        "content_ref": "knowledge://artifact/other-owner-record",
        "content": "complete second body", "searchable_text": "second searchable body",
        "content_size_bytes": 20, "metadata": {"owner": "render", "preserved": true}
    }]);
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, batch)
            .expect("ingest"),
    );
    broker.reset();

    let result = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEMANTIC_CONTEXT_EXPORT_ID,
                serde_json::json!({"project_root":"/work/demo", "record_kind":"artifacts"}),
            )
            .expect("context"),
    );

    let records = broker.artifact_records.lock().unwrap().clone().unwrap();
    assert_eq!(
        result["artifacts"],
        indexed_artifact_records(&records, "/work/demo")
    );
    assert_eq!(result["elements"], serde_json::json!([]));
    assert_eq!(result["relationships"], serde_json::json!([]));
    let first_owner = result["artifacts"][0]["semantic_element_id"]
        .as_str()
        .unwrap();
    assert_eq!(
        broker.structure_scopes.lock().unwrap()[0],
        serde_json::json!({
            "semantic_element": first_owner
        })
    );
    assert_eq!(
        broker.structure_element_counts.lock().unwrap().as_slice(),
        [1]
    );
    assert_eq!(inner.project_snapshot_calls(), 0);
    assert_eq!(inner.semantic_operation_count("semantic_revision"), 2);
    assert_eq!(inner.semantic_operation_count("project_artifacts"), 1);
    assert_eq!(
        broker.semantic_operations.lock().unwrap().as_slice(),
        [
            "project_roots",
            "semantic_revision",
            "project_artifacts",
            "scoped_read",
            "semantic_revision"
        ]
    );

    runtime.stop(PLUGIN_ID).expect("stop");
}

#[test]
fn unscoped_artifact_context_preserves_empty_and_unknown_roots() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[76; 32]));
    let inner = Arc::new(MemoryCapabilityBroker::default());
    let broker = Arc::new(ContextReadBroker::new(Arc::clone(&inner)));
    let runtime = PluginSystem::with_broker_and_sandbox(
        PluginRuntimeConfig::default(),
        broker.clone(),
        Arc::new(crate::support::TestSubprocessSandbox),
    );
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, index_batch())
            .expect("ingest"),
    );
    broker.reset();

    let empty = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEMANTIC_CONTEXT_EXPORT_ID,
                serde_json::json!({"project_root":"/work/demo", "record_kind":"artifacts"}),
            )
            .expect("empty artifact context"),
    );
    assert_eq!(empty["artifacts"], serde_json::json!([]));
    assert!(empty["commit_version"].as_i64().unwrap() > 0);
    assert!(
        empty["published_at"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert_eq!(
        broker.structure_scopes.lock().unwrap().as_slice(),
        [serde_json::json!({"project_root": "/work/demo"})]
    );
    assert_eq!(inner.semantic_operation_count("semantic_revision"), 2);

    broker.reset();
    let unknown = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEMANTIC_CONTEXT_EXPORT_ID,
                serde_json::json!({"project_root":"/work/unknown", "record_kind":"artifacts"}),
            )
            .expect("unknown project"),
    );
    assert_eq!(unknown["artifacts"], serde_json::json!([]));
    assert_eq!(unknown["elements"], serde_json::json!([]));
    assert_eq!(unknown["relationships"], serde_json::json!([]));
    assert!(unknown["commit_version"].as_i64().unwrap() > 0);
    assert!(
        unknown["published_at"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert_eq!(inner.project_snapshot_calls(), 1);
    assert_eq!(
        broker.semantic_operations.lock().unwrap().as_slice(),
        ["project_roots", "project_snapshot"]
    );
    runtime.stop(PLUGIN_ID).expect("stop");
}

fn indexed_artifact_records(records: &Value, project_root: &str) -> Value {
    Value::Array(
        records
            .as_array()
            .unwrap()
            .iter()
            .map(|record| {
                serde_json::json!({
                    "project_root": project_root,
                    "artifact_id": record["artifact_id"],
                    "semantic_element_id": record["semantic_element_id"],
                    "artifact_kind": record["artifact_kind"],
                    "title": record["title"],
                    "content_ref": record["content_ref"],
                    "content": record["content"],
                    "searchable_text": record["searchable_text"],
                    "content_size_bytes": record["content_size_bytes"],
                    "metadata": record["metadata"]
                })
            })
            .collect(),
    )
}

#[test]
fn unscoped_artifact_context_rejects_project_mismatch() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[77; 32]));
    let inner = Arc::new(MemoryCapabilityBroker::default());
    let mut test_broker = ContextReadBroker::new(Arc::clone(&inner));
    test_broker.mismatch_structure_project = true;
    let broker = Arc::new(test_broker);
    let runtime = PluginSystem::with_broker_and_sandbox(
        PluginRuntimeConfig::default(),
        broker.clone(),
        Arc::new(crate::support::TestSubprocessSandbox),
    );
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, artifact_context_batch())
            .expect("ingest"),
    );
    broker.reset();

    let error = failure(
        runtime
            .invoke(
                PLUGIN_ID,
                SEMANTIC_CONTEXT_EXPORT_ID,
                serde_json::json!({"project_root":"/work/demo", "record_kind":"artifacts"}),
            )
            .expect("context"),
    );

    assert_eq!(error.code, "invalid_semantic_input");
    assert!(error.message.contains("active semantic project"));
    assert_eq!(inner.semantic_operation_count("semantic_revision"), 1);
    assert_eq!(
        broker.structure_scopes.lock().unwrap()[0]["semantic_element"],
        "fn:parse"
    );
    runtime.stop(PLUGIN_ID).expect("stop");
}

#[test]
fn unscoped_artifact_context_rejects_revision_change_before_owner_structure() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[78; 32]));
    let inner = Arc::new(MemoryCapabilityBroker::default());
    let mut test_broker = ContextReadBroker::new(Arc::clone(&inner));
    test_broker.change_structure_revision = true;
    let broker = Arc::new(test_broker);
    let runtime = PluginSystem::with_broker_and_sandbox(
        PluginRuntimeConfig::default(),
        broker.clone(),
        Arc::new(crate::support::TestSubprocessSandbox),
    );
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, artifact_context_batch())
            .expect("ingest"),
    );
    broker.reset();

    let error = failure(
        runtime
            .invoke(
                PLUGIN_ID,
                SEMANTIC_CONTEXT_EXPORT_ID,
                serde_json::json!({"project_root":"/work/demo", "record_kind":"artifacts"}),
            )
            .expect("context"),
    );

    assert_eq!(error.code, "invalid_semantic_input");
    assert!(error.message.contains("unchanged semantic revision"));
    assert!(error.message.contains("->"));
    assert_eq!(inner.semantic_operation_count("semantic_revision"), 1);
    assert_eq!(
        broker.structure_scopes.lock().unwrap()[0]["semantic_element"],
        "fn:parse"
    );
    assert_eq!(
        broker.semantic_operations.lock().unwrap().as_slice(),
        [
            "project_roots",
            "semantic_revision",
            "project_artifacts",
            "scoped_read"
        ]
    );
    runtime.stop(PLUGIN_ID).expect("stop");
}

#[test]
fn unscoped_artifact_context_rejects_final_revision_change() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[79; 32]));
    let inner = Arc::new(MemoryCapabilityBroker::default());
    let mut test_broker = ContextReadBroker::new(Arc::clone(&inner));
    test_broker.change_final_revision = true;
    let broker = Arc::new(test_broker);
    let runtime = PluginSystem::with_broker_and_sandbox(
        PluginRuntimeConfig::default(),
        broker.clone(),
        Arc::new(crate::support::TestSubprocessSandbox),
    );
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, artifact_context_batch())
            .expect("ingest"),
    );
    broker.reset();

    let error = failure(
        runtime
            .invoke(
                PLUGIN_ID,
                SEMANTIC_CONTEXT_EXPORT_ID,
                serde_json::json!({"project_root":"/work/demo", "record_kind":"artifacts"}),
            )
            .expect("context"),
    );

    assert_eq!(error.code, "invalid_semantic_input");
    assert!(error.message.contains("unchanged semantic revision"));
    assert!(error.message.contains("->"));
    assert_eq!(inner.semantic_operation_count("semantic_revision"), 2);
    assert_eq!(
        broker.structure_scopes.lock().unwrap()[0]["semantic_element"],
        "fn:parse"
    );
    assert_eq!(
        broker.semantic_operations.lock().unwrap().as_slice(),
        [
            "project_roots",
            "semantic_revision",
            "project_artifacts",
            "scoped_read",
            "semantic_revision"
        ]
    );
    runtime.stop(PLUGIN_ID).expect("stop");
}

fn artifact_context_batch() -> Value {
    let mut batch = index_batch();
    batch["semantic_artifacts"] = serde_json::json!([{
        "artifact_id": "parse-artifact", "semantic_element_id": "fn:parse",
        "artifact_kind": "report", "title": "Parse artifact", "content": "complete body"
    }]);
    batch
}

#[test]
fn context_scope_honors_root_descendants_and_record_kind() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[73; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(Arc::clone(&broker));
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    let mut batch = index_batch();
    batch["semantic_elements"][1]["metadata"] = serde_json::json!({"anchor_selector":{"kind":"pdf_text","page":2,"exact":"Calibration passage"}});
    // Containment relationships and parent links both define descendant scope.
    batch["semantic_elements"][2]["parent_element_id"] = serde_json::json!("fn:parse");
    batch["semantic_elements"].as_array_mut().unwrap().push(serde_json::json!({
        "semantic_source_id": "source-main", "semantic_element_id": "fn:outside",
        "path": "src/outside.rs", "semantic_element_type": "function", "semantic_element_name": "outside"
    }));
    batch["semantic_artifacts"] = serde_json::json!([{
        "artifact_id": "parse-source", "semantic_element_id": "fn:parse",
        "artifact_kind": "source", "title": "Parse source", "content": "parse source"
    }, {
        "artifact_id": "outside-source", "semantic_element_id": "fn:outside",
        "artifact_kind": "source", "title": "Outside source", "content": "outside source"
    }]);
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, batch)
            .expect("ingest"),
    );
    for (kind, descendants, expected) in [
        ("elements", false, 1),
        ("elements", true, 3),
        ("relationships", false, 1),
        ("relationships", true, 4),
        ("artifacts", false, 0),
        ("artifacts", true, 1),
    ] {
        broker.reset_project_snapshot_calls();
        let result = success(runtime.invoke(PLUGIN_ID, SEMANTIC_CONTEXT_EXPORT_ID,
            serde_json::json!({"project_root": "/work/demo", "root_element_id": "file:parser",
                "record_kind": kind, "include_descendants": descendants})).expect("context"));
        assert_eq!(
            result[kind].as_array().unwrap().len(),
            expected,
            "{kind}, descendants={descendants}"
        );
        for other in ["elements", "relationships", "artifacts"]
            .into_iter()
            .filter(|other| *other != kind)
        {
            assert_eq!(result[other], serde_json::json!([]), "unrequested {other}");
        }
        assert!(result["commit_version"].as_i64().unwrap() > 0);
        if kind == "elements" {
            assert_eq!(
                broker.project_snapshot_calls(),
                0,
                "scoped element context must not read the whole project"
            );
            if descendants {
                let child = result["elements"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["semantic_element_id"] == "fn:parse")
                    .unwrap();
                assert_eq!(
                    child["metadata"]["indexer_metadata"]["anchor_selector"]["exact"],
                    "Calibration passage"
                );
            }
        }
    }
    let unknown = failure(
        runtime
            .invoke(
                PLUGIN_ID,
                SEMANTIC_CONTEXT_EXPORT_ID,
                serde_json::json!({"project_root": "/work/demo", "root_element_id": "missing:root",
            "record_kind": "elements"}),
            )
            .expect("unknown root"),
    );
    assert!(unknown.message.contains("missing:root"));
    let mut cycle = index_batch();
    cycle["semantic_relationships"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "source_element_id": "fn:parse", "target_element_id": "file:parser",
            "relationship_kind": "contains", "relationship_label": "contains"
        }));
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, cycle)
            .expect("cyclic containment"),
    );
    let cyclic = success(
        runtime
            .invoke(
                PLUGIN_ID,
                SEMANTIC_CONTEXT_EXPORT_ID,
                serde_json::json!({"project_root": "/work/demo", "root_element_id": "file:parser",
            "record_kind": "elements"}),
            )
            .expect("default descendant scope terminates"),
    );
    let ids: HashSet<&str> = cyclic["elements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|element| element["semantic_element_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains("fn:parse"));
    assert!(!ids.contains("fn:outside"));
    runtime.stop(PLUGIN_ID).expect("stop");
}

#[test]
fn deep_element_context_preserves_all_descendants_and_project_isolation() {
    let workspace = tempfile::tempdir().expect("workspace");
    let (_, installed) = signed_install(workspace.path(), &SigningKey::from_bytes(&[74; 32]));
    let broker = Arc::new(MemoryCapabilityBroker::default());
    let runtime = system(Arc::clone(&broker));
    runtime.install(&installed).expect("install");
    runtime.start(PLUGIN_ID).expect("start");
    let mut batch = index_batch();
    batch["semantic_elements"] = serde_json::json!((0..=130).map(|index| serde_json::json!({
        "semantic_source_id":"source-main", "semantic_element_id":format!("section:{index}"),
        "path":"deep.md", "semantic_element_type":"markdown_section", "semantic_element_name":format!("Section {index}"),
        "parent_element_id":(index > 0).then(|| format!("section:{}", index - 1))
    })).collect::<Vec<_>>());
    batch["semantic_relationships"] = serde_json::json!([]);
    success(
        runtime
            .invoke(PLUGIN_ID, INGEST_EXPORT_ID, batch)
            .expect("ingest deep tree"),
    );
    broker.reset_project_snapshot_calls();
    let result = success(runtime.invoke(PLUGIN_ID, SEMANTIC_CONTEXT_EXPORT_ID,
        serde_json::json!({"project_root":"/work/demo", "root_element_id":"section:0", "record_kind":"elements"})).expect("deep context"));
    assert_eq!(result["elements"].as_array().unwrap().len(), 131);
    assert_eq!(
        broker.project_snapshot_calls(),
        1,
        "depth boundary must fall back to a complete snapshot"
    );
    let wrong_project = failure(runtime.invoke(PLUGIN_ID, SEMANTIC_CONTEXT_EXPORT_ID,
        serde_json::json!({"project_root":"/work/other", "root_element_id":"section:0", "record_kind":"elements"})).expect("foreign root"));
    assert!(wrong_project.message.contains("requested project"));
    runtime.stop(PLUGIN_ID).expect("stop");
}
