//! Document behavior exercised through the plugin's public dispatch boundary.

use std::collections::BTreeMap;

use lumvise_plugin_sdk::{HostCallTransport, PluginApplication, PluginContext, PluginError};
use serde_json::{Value, json};

use crate::KnowledgePlugin;

#[derive(Default)]
struct FakeDocumentGraph {
    elements: BTreeMap<String, Value>,
    writes: usize,
    embeddings: FakeDocumentEmbeddings,
}

#[derive(Default)]
struct FakeDocumentEmbeddings {
    enabled: bool,
}

impl FakeDocumentEmbeddings {
    fn embed(&self, input: &Value) -> Result<Value, PluginError> {
        if !self.enabled {
            return Err(PluginError::unknown_capability("neural.embed"));
        }
        let vectors = input["texts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|text| {
                let text = text.as_str().unwrap();
                if text.contains("pipeline") || text.contains("connections") {
                    vec![1.0, 0.0]
                } else {
                    vec![0.0, 1.0]
                }
            })
            .collect::<Vec<_>>();
        Ok(json!({"vectors": vectors}))
    }
}

impl HostCallTransport for FakeDocumentGraph {
    fn host_call(&mut self, capability: &str, input: Value) -> Result<Value, PluginError> {
        match (capability, input["operation"].as_str()) {
            ("neural.embed", _) => self.embeddings.embed(&input),
            ("storage.semantic", Some("element")) => Ok(
                json!({"element": self.elements.get(input["semantic_element_id"].as_str().unwrap())}),
            ),
            ("storage.semantic", Some("project_roots")) => {
                Ok(json!({"project_roots": self.roots()}))
            }
            ("storage.semantic", Some("scoped_read")) => Ok(self.scoped(&input)),
            ("plugin.invoke", _) if input["export_id"] == "ingest_index_batch" => {
                self.ingest(&input["input"])
            }
            _ => Err(PluginError::unknown_capability(capability)),
        }
    }
}

impl FakeDocumentGraph {
    fn call(&mut self, export: &str, input: Value) -> Result<Value, PluginError> {
        let mut context = PluginContext::for_test(self);
        KnowledgePlugin::default().dispatch(export, input, &mut context)
    }

    fn roots(&self) -> Vec<String> {
        self.elements
            .values()
            .filter_map(|row| row["project_root"].as_str().map(str::to_owned))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn scoped(&self, input: &Value) -> Value {
        let root = &input["request"]["Structure"]["scope"]["project_root"];
        let elements: Vec<&Value> = self
            .elements
            .values()
            .filter(|row| &row["project_root"] == root && row["lifecycle"] != "inactive")
            .collect();
        json!({"project_root": root, "elements": elements, "relationships": []})
    }

    fn ingest(&mut self, batch: &Value) -> Result<Value, PluginError> {
        self.writes += 1;
        let removed = batch["removed_paths"].as_array().unwrap();
        for row in self.elements.values_mut() {
            if row["project_root"] == batch["project_root"] && removed.contains(&row["path"]) {
                row["lifecycle"] = json!("inactive");
            }
        }
        for element in batch["semantic_elements"].as_array().unwrap() {
            let stored = self.stored_element(batch, element);
            self.elements.insert(
                element["semantic_element_id"].as_str().unwrap().into(),
                stored,
            );
        }
        Ok(json!({"output": {"accepted": true}}))
    }

    fn stored_element(&self, batch: &Value, element: &Value) -> Value {
        let mut stored = element.clone();
        stored["project_root"] = batch["project_root"].clone();
        stored["element_kind"] = element["semantic_element_type"].clone();
        stored["name"] = element["semantic_element_name"].clone();
        stored["lifecycle"] = json!("active");
        stored["metadata"] = json!({"provider_instance_id": batch["provider_instance_id"],
            "indexer_metadata": element["metadata"]});
        stored
    }
}

fn draft(id: &str, root: &str) -> Value {
    json!({"document_id": id, "project_root": root, "document_kind": "report",
        "title": "System architecture", "content": "The ingestion pipeline links source knowledge.",
        "metadata": {"author": "human", "attachments": [{"id": "image-1"}]}})
}

fn identity(id: &str, root: &str) -> Value {
    json!({"document_id": id, "project_root": root})
}

#[test]
fn creates_and_reopens_a_report_without_any_source_element() {
    let mut graph = FakeDocumentGraph::default();
    let created = graph
        .call("create_document", draft("overview", "/project"))
        .unwrap();
    let opened = graph
        .call("get_document", identity("overview", "/project"))
        .unwrap();
    assert_eq!(opened, created);
    assert_eq!(opened["document"]["revision"], 1);
    assert_eq!(graph.elements.len(), 1);
    let element = graph.elements.values().next().unwrap();
    assert!(element["parent_element_id"].is_null());
    assert_eq!(element["element_kind"], "virtual_document");
}

#[test]
fn grouping_and_content_updates_preserve_unspecified_metadata_and_reject_stale_writes() {
    let mut graph = FakeDocumentGraph::default();
    graph
        .call("create_document", draft("overview", "/project"))
        .unwrap();
    let updated = graph
        .call(
            "update_document",
            json!({"document_id": "overview", "project_root": "/project",
        "expected_revision": 1, "path": "Architecture/Platform", "content": "Revised content"}),
        )
        .unwrap();
    let stale = graph
        .call(
            "update_document",
            json!({"document_id": "overview", "project_root": "/project",
        "expected_revision": 1, "content": "An obsolete draft"}),
        )
        .unwrap_err();
    assert_eq!(stale.code, "document_revision_conflict");
    assert_eq!(graph.writes, 2);
    assert_eq!(
        updated["document"]["metadata"],
        draft("overview", "/project")["metadata"]
    );
    assert_eq!(
        graph
            .call("get_document", identity("overview", "/project"))
            .unwrap(),
        updated
    );
}

#[test]
fn project_mismatch_does_not_expose_or_overwrite_another_document() {
    let mut graph = FakeDocumentGraph::default();
    graph
        .call("create_document", draft("overview", "/project-a"))
        .unwrap();
    let missing = graph
        .call("get_document", identity("overview", "/project-b"))
        .unwrap_err();
    let duplicate = graph
        .call("create_document", draft("overview", "/project-b"))
        .unwrap_err();
    assert_eq!(missing.code, "document_not_found");
    assert_eq!(duplicate.code, "document_exists");
    assert_eq!(graph.writes, 1);
}

#[test]
fn document_search_reaches_content_and_respects_project_kind_and_agent_space_scope() {
    let mut graph = FakeDocumentGraph::default();
    graph
        .call("create_document", draft("overview", "/project"))
        .unwrap();
    graph
        .call("create_document", draft("other", "/other"))
        .unwrap();
    graph
        .call(
            "create_document",
            draft("private", "agent-space://research"),
        )
        .unwrap();
    let found = graph
        .call(
            "search_documents",
            json!({"query": "ingestion pipeline", "project_root": "/project"}),
        )
        .unwrap();
    let global = graph.call("list_documents", json!({})).unwrap();
    let canvases = graph
        .call("list_documents", json!({"document_kind": "canvas"}))
        .unwrap();
    assert_eq!(found["documents"][0]["document_id"], "overview");
    assert_eq!(found["total"], 1);
    assert_eq!(found["mode"], "lexical");
    assert_eq!(global["total"], 2);
    assert_eq!(canvases["total"], 0);
}

#[test]
fn removing_a_citation_target_preserves_the_citing_document_and_broken_reference() {
    let mut graph = FakeDocumentGraph::default();
    graph
        .call("create_document", draft("target", "/project"))
        .unwrap();
    let mut citing = draft("citing", "/project");
    citing["references"] = json!([{"target_kind": "document", "document_id": "target"}]);
    graph.call("create_document", citing).unwrap();
    graph
        .call("delete_document", identity("target", "/project"))
        .unwrap();
    let remaining = graph
        .call("get_document", identity("citing", "/project"))
        .unwrap();
    assert_eq!(
        remaining["document"]["references"][0]["document_id"],
        "target"
    );
    assert_eq!(
        graph
            .call("list_documents", json!({"project_root": "/project"}))
            .unwrap()["total"],
        1
    );
    assert_eq!(
        graph
            .call("get_document", identity("target", "/project"))
            .unwrap_err()
            .code,
        "document_not_found"
    );
}

#[test]
fn invalid_and_duplicate_references_fail_before_any_write() {
    let mut graph = FakeDocumentGraph::default();
    let mut request = draft("overview", "/project");
    request["references"] = json!([{"target_kind": "document", "document_id": "same"},
        {"target_kind": "document", "document_id": "same"}]);
    assert_eq!(
        graph
            .call("create_document", request.clone())
            .unwrap_err()
            .code,
        "invalid_document_input"
    );
    request["references"] = json!([{"target_kind": "semantic_element", "semantic_element_id": ""}]);
    assert!(graph.call("create_document", request).is_err());
    assert_eq!(graph.writes, 0);
}

#[test]
fn archived_documents_remain_addressable_but_require_explicit_history_discovery() {
    let mut graph = FakeDocumentGraph::default();
    graph
        .call("create_document", draft("overview", "/project"))
        .unwrap();
    graph
        .call(
            "document.update",
            json!({"document_id": "overview", "project_root": "/project",
        "expected_revision": 1, "status": "archived"}),
        )
        .unwrap();
    assert_eq!(
        graph
            .call("list_documents", json!({"project_root": "/project"}))
            .unwrap()["total"],
        0
    );
    assert_eq!(
        graph
            .call(
                "list_documents",
                json!({"project_root": "/project", "include_inactive": true})
            )
            .unwrap()["total"],
        1
    );
    assert_eq!(
        graph
            .call("document.get", identity("overview", "/project"))
            .unwrap()["document"]["revision"],
        2
    );
}

#[test]
fn semantic_document_search_finds_related_content_without_a_literal_query_match() {
    let mut graph = FakeDocumentGraph::default();
    graph.embeddings.enabled = true;
    graph
        .call("create_document", draft("overview", "/project"))
        .unwrap();
    let mut other = draft("weather", "/project");
    other["content"] = json!("Tomorrow will be sunny.");
    graph.call("create_document", other).unwrap();
    let found = graph
        .call(
            "search_documents",
            json!({"query": "discover useful connections", "project_root": "/project"}),
        )
        .unwrap();
    assert_eq!(found["mode"], "vector");
    assert_eq!(found["documents"][0]["document_id"], "overview");
    assert_eq!(found["scores"]["overview"], 1.0);
    assert_eq!(found["scores"]["weather"], 0.0);
}

#[test]
fn signed_document_exports_publish_revision_and_search_score_contracts() {
    let manifest = crate::package_manifest_source("test-target", &"a".repeat(64));
    let update = manifest
        .exports
        .iter()
        .find(|export| export.id == "update_document")
        .unwrap();
    let search = manifest
        .exports
        .iter()
        .find(|export| export.id == "search_documents")
        .unwrap();
    assert!(
        update.input_schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("expected_revision"))
    );
    assert!(search.output_schema["properties"].get("scores").is_some());
    assert!(
        manifest
            .exports
            .iter()
            .any(|export| export.id == "document.get")
    );
}
