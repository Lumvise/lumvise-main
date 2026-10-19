//! Knowledge's public document interface against the real Semantic/local boundary.

use std::sync::Arc;

use lumvise_app_core::AppCoreHostCapabilityBroker;
use lumvise_db_core::{LocalPersistence, RelationalPersistence, SemanticPersistence};
use lumvise_plugin_knowledge::KnowledgePlugin;
use lumvise_plugin_runtime::{HostCapabilityBroker, HostCapabilityRequest};
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use lumvise_plugin_semantic::SemanticPlugin;
use serde_json::{Value, json};

struct DocumentBoundaryHost {
    broker: Arc<AppCoreHostCapabilityBroker>,
    plugin_id: &'static str,
}

impl DocumentBoundaryHost {
    fn new() -> Self {
        Self::with_persistence(Arc::new(LocalPersistence::in_memory().unwrap()))
    }

    fn with_persistence(persistence: Arc<LocalPersistence>) -> Self {
        let semantic: Arc<dyn SemanticPersistence> = persistence.clone();
        let relational: Arc<dyn RelationalPersistence> = persistence;
        Self {
            broker: Arc::new(AppCoreHostCapabilityBroker::new(semantic, relational)),
            plugin_id: "builtin.knowledge",
        }
    }

    fn knowledge(&mut self, export: &str, input: Value) -> Result<Value, PluginError> {
        KnowledgePlugin::default().dispatch(export, input, &mut PluginContext::for_test(self))
    }

    fn semantic(&self, export: &str, input: Value) -> Result<Value, PluginError> {
        let mut host = Self {
            broker: self.broker.clone(),
            plugin_id: "builtin.semantic",
        };
        SemanticPlugin.dispatch(export, input, &mut PluginContext::for_test(&mut host))
    }
}

impl HostCallTransport for DocumentBoundaryHost {
    fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
        if capability == "plugin.invoke" && input["plugin_id"] == "builtin.semantic" {
            let output =
                self.semantic(input["export_id"].as_str().unwrap(), input["input"].clone())?;
            return Ok(json!({"output": output}));
        }
        self.broker
            .invoke(HostCapabilityRequest {
                plugin_id: self.plugin_id.into(),
                invocation_id: "document-boundary".into(),
                call_id: "document-host-call".into(),
                capability_id: capability.into(),
                required_version: if capability == "storage.semantic" {
                    "^1.4.0"
                } else {
                    "^1.0"
                }
                .into(),
                input,
            })
            .map_err(|error| PluginError::new("document_test_host_error", error.to_string(), false))
    }
}

fn report() -> Value {
    json!({"document_id": "report:architecture", "project_root": "/project", "document_kind": "report",
        "title": "Architecture", "content": "# A standalone report", "path": "Architecture/Overview",
        "references": [], "metadata": {"authored_by": "human"}})
}

fn identity() -> Value {
    json!({"document_id": "report:architecture", "project_root": "/project"})
}

#[test]
fn a_standalone_document_persists_through_the_real_semantic_owner_and_plugin_reopen() {
    let mut host = DocumentBoundaryHost::new();
    let created = host.knowledge("create_document", report()).unwrap();
    let reopened = host.knowledge("get_document", identity()).unwrap();
    let listed = host
        .knowledge("list_documents", json!({"project_root": "/project"}))
        .unwrap();
    assert_eq!(reopened, created);
    assert_eq!(listed["documents"][0], created["document"]);
    assert_eq!(reopened["document"]["path"], "Architecture/Overview");
}

#[test]
fn source_snapshot_replacement_preserves_independently_owned_documents() {
    let mut host = DocumentBoundaryHost::new();
    let created = host.knowledge("create_document", report()).unwrap();
    host.semantic("ingest_index_batch", source_batch()).unwrap();
    assert_eq!(host.knowledge("get_document", identity()).unwrap(), created);
}

fn source_batch() -> Value {
    json!({"provider_instance_id": "filesystem", "project_root": "/project",
        "semantic_sources": [{"semantic_source_id": "source:files", "kind": "repository", "name": "Sources", "root_path": "/project", "root_uri": "file:///project"}],
        "semantic_elements": [{"semantic_element_id": "file:lib", "semantic_source_id": "source:files", "path": "src/lib.rs",
            "semantic_element_type": "file", "semantic_element_name": "lib.rs", "parent_element_id": null,
            "start_line": 1, "end_line": 3, "content_fingerprint": "fp1:0000000000000001:source-v1", "metadata": {}}],
        "semantic_relationships": [], "semantic_artifacts": [], "replace_paths": [], "removed_paths": []
    })
}

#[test]
fn grouping_updates_and_deletion_use_the_same_real_document_record() {
    let mut host = DocumentBoundaryHost::new();
    host.knowledge("create_document", report()).unwrap();
    let changed = host.knowledge("update_document", json!({"document_id": "report:architecture", "project_root": "/project", "expected_revision": 1,
        "path": "Decisions", "content": "# Revised document"})).unwrap();
    assert_eq!(changed["document"]["revision"], 2);
    assert_eq!(changed["document"]["metadata"], report()["metadata"]);
    host.knowledge("delete_document", identity()).unwrap();
    assert_eq!(
        host.knowledge("list_documents", json!({"project_root": "/project"}))
            .unwrap()["total"],
        0
    );
}

#[test]
fn staged_full_source_publication_keeps_independent_documents() {
    let mut host = DocumentBoundaryHost::new();
    let created = host.knowledge("create_document", report()).unwrap();
    let mut batch = source_batch();
    batch["ingestion_job_id"] = json!("source-refresh");
    batch["ingestion_page_index"] = json!(0);
    batch["ingestion_page_count"] = json!(2);
    host.semantic("ingest_index_batch", batch.clone()).unwrap();
    batch["ingestion_page_index"] = json!(1);
    batch["semantic_elements"] = json!([]);
    host.semantic("ingest_index_batch", batch).unwrap();
    assert_eq!(host.knowledge("get_document", identity()).unwrap(), created);
}

#[test]
fn a_report_survives_closing_and_reopening_its_selected_local_persistence() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("documents.sqlite");
    let mut host =
        DocumentBoundaryHost::with_persistence(Arc::new(LocalPersistence::open(&path).unwrap()));
    let created = host.knowledge("create_document", report()).unwrap();
    drop(host);
    let mut reopened =
        DocumentBoundaryHost::with_persistence(Arc::new(LocalPersistence::open(&path).unwrap()));
    assert_eq!(
        reopened.knowledge("get_document", identity()).unwrap(),
        created
    );
}

#[test]
fn identical_empty_reports_keep_distinct_explicit_document_identities() {
    let mut host = DocumentBoundaryHost::new();
    let mut first = report();
    first["content"] = json!("");
    host.knowledge("create_document", first.clone()).unwrap();
    first["document_id"] = json!("report:another-architecture");
    host.knowledge("create_document", first).unwrap();
    assert_eq!(
        host.knowledge("list_documents", json!({"project_root": "/project"}))
            .unwrap()["total"],
        2
    );
}
