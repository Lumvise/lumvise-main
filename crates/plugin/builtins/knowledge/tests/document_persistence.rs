//! Knowledge's public document interface against the real Semantic/local boundary.

use std::sync::Arc;

use lumvise_app_core::AppCoreHostCapabilityBroker;
use lumvise_db_core::{
    LocalPersistence, RelationalPersistence, SemanticOperation, SemanticPersistence, SemanticResult,
};
use lumvise_plugin_knowledge::KnowledgePlugin;
use lumvise_plugin_runtime::{
    HostCapabilityBroker, HostCapabilityRequest, PluginInvocationClass, PluginInvocationContext,
};
use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use lumvise_plugin_semantic::SemanticPlugin;
use lumvise_resource_routing::InvocationControl;
use serde_json::{Value, json};

#[path = "document_persistence/citations.rs"]
mod citations;
#[path = "document_persistence/delegation.rs"]
mod delegation;
#[path = "document_persistence/generation.rs"]
mod generation;
#[path = "document_persistence/inspection.rs"]
mod inspection;
#[path = "document_persistence/md_seed.rs"]
mod md_seed;
#[path = "document_persistence/qualified_citations.rs"]
mod qualified_citations;
#[path = "document_persistence/revisions.rs"]
mod revisions;
#[path = "support/semantic_seed.rs"]
mod semantic_seed;
#[path = "document_persistence/source_publication.rs"]
mod source_publication;
#[path = "document_persistence/write_receipts.rs"]
mod write_receipts;

struct DocumentBoundaryHost {
    broker: Arc<AppCoreHostCapabilityBroker>,
    plugin_id: &'static str,
    persistence: Arc<LocalPersistence>,
    fail_document_write: Option<usize>,
    fail_resource_write: Option<usize>,
    generation: Option<generation::FakeFunctionalJobs>,
    /// Successful outer writes in order: `artifact` for staged bodies, `publish` for index publications.
    writes: Vec<&'static str>,
    publication_inputs: Vec<Value>,
    drop_document_reply: bool,
    drop_citation_reply: bool,
    interleaved_call: Option<(&'static str, Value)>,
    before_list_versions: Option<Value>,
    reported_project_identity: Option<String>,
    missing_body: Option<String>,
    source_directory: Arc<tempfile::TempDir>,
    client_instance_id: String,
    invocation_context: PluginInvocationContext,
}

impl DocumentBoundaryHost {
    fn new() -> Self {
        Self::with_persistence(Arc::new(LocalPersistence::in_memory().unwrap()))
    }

    fn with_persistence(persistence: Arc<LocalPersistence>) -> Self {
        Self::with_persistence_client(persistence, "client-a")
    }

    fn with_persistence_client(persistence: Arc<LocalPersistence>, client: &str) -> Self {
        let semantic: Arc<dyn SemanticPersistence> = persistence.clone();
        let relational: Arc<dyn RelationalPersistence> = persistence.clone();
        Self {
            broker: Arc::new(
                AppCoreHostCapabilityBroker::new(semantic, relational)
                    .with_index_client(client.into())
                    .unwrap(),
            ),
            plugin_id: "builtin.knowledge",
            persistence,
            fail_document_write: None,
            fail_resource_write: None,
            generation: None,
            writes: Vec::new(),
            publication_inputs: Vec::new(),
            drop_document_reply: false,
            drop_citation_reply: false,
            interleaved_call: None,
            before_list_versions: None,
            reported_project_identity: None,
            missing_body: None,
            source_directory: Arc::new(tempfile::tempdir().unwrap()),
            client_instance_id: client.into(),
            invocation_context: PluginInvocationContext::new(
                "document-boundary",
                "document-test-owner",
                PluginInvocationClass::Foreground,
                std::time::Instant::now() + std::time::Duration::from_secs(60),
            ),
        }
    }

    fn knowledge(&mut self, export: &str, input: Value) -> Result<Value, PluginError> {
        let parent = self.invocation_context.clone();
        self.invocation_context = parent.clone().enter_plugin("builtin.knowledge", export);
        let result =
            KnowledgePlugin::default().dispatch(export, input, &mut PluginContext::for_test(self));
        self.invocation_context = parent;
        result
    }

    fn semantic(&self, export: &str, input: Value) -> Result<Value, PluginError> {
        let mut host = Self {
            broker: self.broker.clone(),
            plugin_id: "builtin.semantic",
            persistence: self.persistence.clone(),
            fail_document_write: None,
            fail_resource_write: None,
            generation: None,
            writes: Vec::new(),
            publication_inputs: Vec::new(),
            drop_document_reply: false,
            drop_citation_reply: false,
            interleaved_call: None,
            before_list_versions: None,
            reported_project_identity: None,
            missing_body: None,
            source_directory: self.source_directory.clone(),
            client_instance_id: self.client_instance_id.clone(),
            invocation_context: self
                .invocation_context
                .clone()
                .enter_plugin("builtin.semantic", export),
        };
        SemanticPlugin.dispatch(export, input, &mut PluginContext::for_test(&mut host))
    }

    fn seed_sources(&self, input: Value) -> Result<Value, PluginError> {
        self.semantic(
            "ingest_index_batch",
            semantic_seed::observe_fresh_source_input(self.persistence.as_ref(), input),
        )
    }

    fn seed_artifact(&self, artifact: Value) {
        let root = artifact["metadata"]["knowledge"]["project_root"]
            .as_str()
            .unwrap();
        semantic_seed::publish_initial_artifact(
            self.persistence.as_ref(),
            root,
            serde_json::from_value(artifact.clone()).unwrap(),
            "text/markdown",
        );
    }

    fn execute(&self, operation: SemanticOperation) -> SemanticResult {
        SemanticPersistence::execute(
            self.persistence.as_ref(),
            operation,
            &InvocationControl::sixty_seconds(),
        )
        .unwrap()
    }
}

impl HostCallTransport for DocumentBoundaryHost {
    fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
        if capability == "storage.semantic" && input["operation"] == "revision_selection_members" {
            if let Some(request) = self.before_list_versions.take() {
                self.knowledge("update_knowledge", request)?;
            }
        }
        if capability == "runtime.project_execution"
            && input["operation"] == "scope"
            && self.generation.is_none()
        {
            return Ok(json!({"client_instance_id":self.client_instance_id}));
        }
        if capability == "storage.semantic"
            && input["operation"] == "resolve_revision"
            && input["record"]["kind"] == "artifact"
            && self
                .missing_body
                .as_deref()
                .is_some_and(|id| input["record"]["record_id"] == id)
        {
            return Ok(json!({"revision":null}));
        }
        if capability == "runtime.project_execution"
            && let Some(generation) = self.generation.as_mut()
        {
            return generation.invoke(input);
        }
        if capability == "plugin.invoke" && input["plugin_id"] == "builtin.semantic" {
            fail_once(&mut self.fail_document_write)?;
            self.publication_inputs.push(input["input"].clone());
            if let Some((export, request)) = self.interleaved_call.take() {
                self.knowledge(export, request)?;
            }
            let output =
                self.semantic(input["export_id"].as_str().unwrap(), input["input"].clone())?;
            self.writes.push("publish");
            if std::mem::take(&mut self.drop_document_reply) {
                return Err(PluginError::new(
                    "lost_document_reply",
                    "accepted document reply was lost",
                    true,
                ));
            }
            return Ok(json!({"output": output}));
        }
        let upsert = capability == "storage.semantic"
            && matches!(
                input["operation"].as_str(),
                Some("upsert_artifact" | "publish_revisions")
            );
        if upsert {
            fail_once(&mut self.fail_resource_write)?;
        }
        let identity_read =
            capability == "storage.semantic" && input["operation"] == "project_identity";
        let citation_acquire =
            capability == "runtime.knowledge_scope" && input["request"]["operation"] == "acquire";
        let mut value = self
            .broker
            .invoke_controlled(
                HostCapabilityRequest {
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
                },
                &self.invocation_context,
            )
            .map_err(|error| {
                PluginError::new("document_test_host_error", error.to_string(), false)
            })?;
        if citation_acquire && std::mem::take(&mut self.drop_citation_reply) {
            return Err(PluginError::new(
                "lost_citation_reply",
                "accepted source citation reply was lost",
                true,
            ));
        }
        if identity_read && let Some(identity) = &self.reported_project_identity {
            value["identity"]["project_id"] = json!(identity);
        }
        if upsert {
            self.writes.push("artifact");
        }
        Ok(value)
    }
}

fn fail_once(remaining: &mut Option<usize>) -> Result<(), PluginError> {
    match remaining {
        Some(0) => {
            *remaining = None;
            Err(PluginError::new(
                "injected_write_failure",
                "test boundary write interrupted",
                true,
            ))
        }
        Some(count) => {
            *count -= 1;
            Ok(())
        }
        None => Ok(()),
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
    source_publication::publish(&host, source_batch());
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
    let created = host.knowledge("create_document", report()).unwrap();
    let changed = host.knowledge("update_document", json!({"document_id": "report:architecture", "project_root": "/project", "expected_revision": 1, "expected_semantic_element_id":created["document"]["semantic_element_id"],
        "path": "Decisions", "content": "# Revised document"})).unwrap();
    assert_eq!(changed["document"]["revision"], 2);
    assert_eq!(changed["document"]["metadata"], report()["metadata"]);
    host.knowledge("delete_document", revisions::deletion(&changed["document"]))
        .unwrap();
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
    let frozen = source_publication::freeze(&host, source_batch()).unwrap();
    let mut batch = serde_json::to_value(&frozen.batch).unwrap();
    batch["ingestion_job_id"] = json!(frozen.operation_id);
    batch["ingestion_page_index"] = json!(0);
    batch["ingestion_page_count"] = json!(2);
    host.semantic("ingest_index_batch", batch.clone()).unwrap();
    batch["ingestion_page_index"] = json!(1);
    batch["semantic_elements"] = json!([]);
    let accepted = host.semantic("ingest_index_batch", batch).unwrap();
    source_publication::acknowledge(&host, "filesystem", &accepted["publication"]["value"]);
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

#[path = "document_persistence/body_artifacts.rs"]
mod body_artifacts;
#[path = "document_persistence/canvas_attachments.rs"]
mod canvas_attachments;
