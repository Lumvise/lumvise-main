//! Document behavior exercised through the plugin's public dispatch boundary.

use lumvise_plugin_sdk::{PluginContext, PluginError};
use serde_json::{Value, json};

#[path = "tests/artifact_body.rs"]
mod artifact_body;
#[path = "tests/generated.rs"]
mod generated;
#[path = "tests/indexed.rs"]
mod indexed;
#[path = "tests/maintenance.rs"]
mod maintenance;
#[path = "tests/spaces.rs"]
mod spaces;

use super::test_graph::*;

fn draft(id: &str, root: &str) -> Value {
    json!({"document_id": id, "project_root": root, "document_kind": "report",
        "title": "System architecture", "content": "The ingestion pipeline links source knowledge.",
        "metadata": {"author": "human", "attachments": [{"id": "image-1"}]}})
}

fn identity(id: &str, root: &str) -> Value {
    json!({"document_id": id, "project_root": root})
}

fn revision_request(document: &Value, patch: Value) -> Value {
    let mut request = json!({"document_id":document["document_id"], "project_root":document["project_root"],
        "expected_revision":document["revision"], "expected_semantic_element_id":document["semantic_element_id"]});
    request
        .as_object_mut()
        .unwrap()
        .extend(patch.as_object().unwrap().clone());
    request
}

#[test]
fn document_reads_use_project_scoped_queries_and_keep_adapter_qualified_identities() {
    let mut graph = FakeDocumentGraph::default();
    let created = graph
        .call("create_document", draft("remote-doc", "/remote"))
        .unwrap();
    let mut element = graph
        .elements
        .remove(created["document"]["semantic_element_id"].as_str().unwrap())
        .unwrap();
    element["semantic_element_id"] = json!("adapter-owned-qualified-id");
    graph
        .elements
        .insert("adapter-owned-qualified-id".into(), element);
    graph.reject_unscoped_reads = true;
    let result = graph
        .call("get_document", identity("remote-doc", "/remote"))
        .unwrap();
    assert_eq!(
        result["document"]["semantic_element_id"],
        "adapter-owned-qualified-id"
    );
    assert_eq!(
        graph.targeted_reads.last().unwrap()["project_root"],
        "/remote"
    );
    assert!(
        graph
            .call("get_document", identity("remote-doc", "/other"))
            .is_err()
    );
    // The first document publishes its project root, then its pointer; the body is staged between them.
    assert_eq!(graph.writes, 2);
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
    // The project root and the document node; the body lives in an artifact, not in either element.
    assert_eq!(graph.elements.len(), 2);
    let element = graph
        .elements
        .values()
        .find(|element| element["element_kind"] == "virtual_document")
        .unwrap();
    assert!(element["parent_element_id"].is_null());
}

#[test]
fn grouping_and_content_updates_preserve_unspecified_metadata_and_reject_stale_writes() {
    let mut graph = FakeDocumentGraph::default();
    let created = graph
        .call("create_document", draft("overview", "/project"))
        .unwrap()["document"]
        .clone();
    let updated = graph
        .call(
            "update_document",
            revision_request(
                &created,
                json!({"path": "Architecture/Platform", "content": "Revised content"}),
            ),
        )
        .unwrap();
    let stale = graph
        .call(
            "update_document",
            revision_request(&created, json!({"content": "An obsolete draft"})),
        )
        .unwrap_err();
    assert_eq!(stale.code, "document_revision_conflict");
    // Create publishes root and pointer; the accepted update publishes its pointer; the stale write publishes nothing.
    assert_eq!(graph.writes, 3);
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
fn matching_document_names_remain_isolated_between_project_lineages() {
    let mut graph = FakeDocumentGraph::default();
    let first = graph
        .call("create_document", draft("overview", "/project-a"))
        .unwrap();
    let missing = graph
        .call("get_document", identity("overview", "/project-b"))
        .unwrap_err();
    let second = graph
        .call("create_document", draft("overview", "/project-b"))
        .unwrap();
    assert_eq!(missing.code, "document_not_found");
    assert_ne!(
        first["document"]["semantic_element_id"],
        second["document"]["semantic_element_id"]
    );
    let mut stale = revision_request(
        &first["document"],
        json!({"content":"Wrong project's edit"}),
    );
    stale["project_root"] = json!("/project-b");
    assert_eq!(
        graph.call("update_document", stale).unwrap_err().code,
        "document_revision_conflict"
    );
    assert_eq!(
        graph
            .call("get_document", identity("overview", "/project-a"))
            .unwrap(),
        first
    );
    assert_eq!(
        graph
            .call("get_document", identity("overview", "/project-b"))
            .unwrap(),
        second
    );
}

#[test]
fn document_search_reaches_content_and_respects_project_kind_and_agent_space_scope() {
    let mut graph = FakeDocumentGraph::default();
    graph.memory_scope = Some(spaces::fixture_scope(
        "agent-space://research",
        lumvise_contracts::KnowledgeSpaceKind::Agent,
    ));
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
fn listing_returns_each_project_its_live_documents_by_kind_and_status() {
    let mut graph = FakeDocumentGraph::default();
    let mut created = std::collections::BTreeMap::new();
    for (id, kind, root) in [
        ("report", "report", "/project"),
        ("context", "context", "/project"),
        ("canvas", "canvas", "/project"),
        ("gone", "report", "/project"),
        ("old", "report", "/project"),
        ("foreign", "report", "/other"),
    ] {
        let mut request = draft(id, root);
        request["document_kind"] = json!(kind);
        created.insert(
            id,
            graph.call("create_document", request).unwrap()["document"].clone(),
        );
    }
    graph
        .call(
            "delete_document",
            revision_request(&created["gone"], json!({})),
        )
        .unwrap();
    graph
        .call(
            "document.update",
            revision_request(&created["old"], json!({"status": "archived"})),
        )
        .unwrap();
    let ids = |graph: &mut FakeDocumentGraph, filter: Value| -> Vec<String> {
        graph.call("list_documents", filter).unwrap()["documents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|document| document["document_id"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(
        ids(&mut graph, json!({"project_root": "/project"})),
        ["canvas", "context", "report"]
    );
    assert_eq!(
        ids(
            &mut graph,
            json!({"project_root": "/project", "document_kind": "canvas"})
        ),
        ["canvas"]
    );
    assert_eq!(
        ids(
            &mut graph,
            json!({"project_root": "/project", "include_inactive": true})
        ),
        ["canvas", "context", "old", "report"]
    );
    assert_eq!(
        ids(&mut graph, json!({"project_root": "/other"})),
        ["foreign"]
    );
    assert_eq!(
        ids(&mut graph, json!({})),
        ["canvas", "context", "foreign", "report"]
    );
}

#[test]
fn listing_selects_documents_by_path_prefix_and_never_scans_the_project_structure() {
    let mut graph = FakeDocumentGraph::default();
    graph
        .call("create_document", draft("overview", "/project"))
        .unwrap();
    graph.scoped_read_kinds.clear();
    let listed = graph
        .call("list_documents", json!({"project_root": "/project"}))
        .unwrap();
    assert_eq!(listed["total"], 1);
    assert_eq!(graph.scoped_read_kinds, ["PathPrefix"]);
}

#[test]
fn removing_a_citation_target_preserves_the_citing_document_and_broken_reference() {
    let mut graph = FakeDocumentGraph::default();
    let target = graph
        .call("create_document", draft("target", "/project"))
        .unwrap()["document"]
        .clone();
    let mut citing = draft("citing", "/project");
    citing["references"] = json!([{"target_kind": "document", "document_id": "target"}]);
    graph.call("create_document", citing).unwrap();
    graph
        .call("delete_document", revision_request(&target, json!({})))
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
    let created = graph
        .call("create_document", draft("overview", "/project"))
        .unwrap()["document"]
        .clone();
    graph
        .call(
            "document.update",
            revision_request(&created, json!({"status": "archived"})),
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
    assert_eq!(found["documents"].as_array().unwrap().len(), 1);
    assert!(found["scores"].get("weather").is_none());
}

#[test]
fn document_search_bounds_memory_results_and_batches_their_payload_reads() {
    let mut graph = FakeDocumentGraph::default();
    for index in 0..24 {
        graph
            .call(
                "create_document",
                draft(&format!("memory-{index:02}"), "/project"),
            )
            .unwrap();
    }
    let before = graph.artifact_reads;
    let result = graph
        .call(
            "search_documents",
            json!({"project_root":"/project", "query":"pipeline"}),
        )
        .unwrap();
    assert_eq!(result["documents"].as_array().unwrap().len(), 10);
    assert_eq!(graph.artifact_reads - before, 1);
    assert_eq!(result["documents"][0]["document_id"], "memory-00");
    assert_eq!(result["documents"][9]["document_id"], "memory-09");
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
