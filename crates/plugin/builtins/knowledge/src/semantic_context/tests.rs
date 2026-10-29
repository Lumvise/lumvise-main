//! Source-generation inputs from one captured semantic snapshot. Knowledge documents, their root and
//! their citation edges never become generation inputs, while source structure and visible
//! Knowledge artifacts stay. Canonical documents remain available from the same capture.

use super::*;
use crate::documents::test_graph::FakeDocumentGraph;
use lumvise_plugin_sdk::{HostCallTransport, PluginContext};
use serde_json::json;
use std::collections::BTreeSet;

const PROJECT: &str = "/project";
const SOURCE_IDS: [&str; 3] = ["file:src/a.rs", "file:src/z.rs", "folder:src"];

fn source(id: &str, path: &str, kind: &str, parent: Option<&str>) -> Value {
    json!({"semantic_element_id": id, "semantic_source_id": "source:files", "path": path,
        "semantic_element_type": kind, "semantic_element_name": path.rsplit('/').next().unwrap_or(path),
        "parent_element_id": parent, "start_line": 1, "end_line": 4,
        "content_fingerprint": format!("fp1:0000000000000001:{id}"), "metadata": {}})
}

/// Source structure, one visible Requirements artifact, and two documents: one cites a source
/// file and the folder, the other cites the first document.
fn seeded() -> FakeDocumentGraph {
    let mut graph = FakeDocumentGraph::default();
    let observation = crate::publication_observation::read(
        &mut PluginContext::for_test(&mut graph),
        PROJECT,
        json!({"kind":"partition", "partition":{"project_root":PROJECT,
        "provider_instance_id":"filesystem", "replace_paths":[]}}),
        &[],
    )
    .unwrap();
    graph
        .ingest(
            &json!({"provider_instance_id": "filesystem", "project_root": PROJECT,
            "revision_observation":observation,
            "replace_paths": [], "removed_paths": [], "semantic_sources": [],
            "semantic_elements": [
                source("folder:src", "src", "folder", None),
                source("file:src/a.rs", "src/a.rs", "file", Some("folder:src")),
                source("file:src/z.rs", "src/z.rs", "file", Some("folder:src"))],
            "semantic_relationships": [
                {"source_element_id": "folder:src", "target_element_id": "file:src/a.rs",
                    "relationship_kind": "contains", "relationship_label": "contains"},
                {"source_element_id": "folder:src", "target_element_id": "file:src/z.rs",
                    "relationship_kind": "contains", "relationship_label": "contains"},
                {"source_element_id": "file:src/a.rs", "target_element_id": "file:src/z.rs",
                    "relationship_kind": "semantic", "relationship_label": "calls"}],
            "semantic_artifacts": []}),
        )
        .unwrap();
    graph
        .seed_knowledge(
            json!({"project_root": PROJECT, "artifact_id": "requirement:search",
                "semantic_element_id": "file:src/a.rs", "knowledge_type": "requirements",
                "title": "Search requirement", "content": "Search must find sources."}),
        )
        .unwrap();
    graph
        .call(
            "create_document",
            json!({"document_id": "overview", "project_root": PROJECT, "document_kind": "report",
                "title": "Overview", "content": "Overview body",
                "references": [
                    {"target_kind": "semantic_element", "semantic_element_id": "file:src/a.rs"},
                    {"target_kind": "semantic_element", "semantic_element_id": "folder:src"}]}),
        )
        .unwrap();
    graph
        .call(
            "create_document",
            json!({"document_id": "second", "project_root": PROJECT, "document_kind": "context",
                "title": "Second", "content": "Second body",
                "references": [{"target_kind": "document", "document_id": "overview"}]}),
        )
        .unwrap();
    graph
}

fn element_ids<'a>(ids: impl Iterator<Item = &'a str>) -> BTreeSet<String> {
    ids.map(str::to_owned).collect()
}

fn root_id(graph: &FakeDocumentGraph) -> String {
    graph
        .elements
        .values()
        .find(|element| element["element_kind"] == "virtual_document_root")
        .unwrap()["semantic_element_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn assert_source_only(
    elements: &[SemanticElement],
    relationships: &[SemanticRelationship],
    root: &str,
) {
    assert_eq!(
        element_ids(
            elements
                .iter()
                .map(|element| element.semantic_element_id.as_str())
        ),
        SOURCE_IDS.iter().map(|id| id.to_string()).collect()
    );
    let source_ids: BTreeSet<&str> = SOURCE_IDS.into();
    for relationship in relationships {
        assert!(
            source_ids.contains(relationship.source_element_id.as_str()),
            "{relationship:?}"
        );
        assert!(
            source_ids.contains(relationship.target_element_id.as_str()),
            "{relationship:?}"
        );
        assert_ne!(relationship.source_element_id, root);
    }
    let mut kinds: Vec<&str> = relationships
        .iter()
        .map(|relationship| relationship.relationship_kind.as_str())
        .collect();
    kinds.sort();
    assert_eq!(kinds, ["contains", "contains", "semantic"]);
    let touching_a = relationships
        .iter()
        .filter(|relationship| {
            relationship.source_element_id == "file:src/a.rs"
                || relationship.target_element_id == "file:src/a.rs"
        })
        .count();
    assert_eq!(touching_a, 2, "source generation counts only source edges");
}

#[test]
fn generic_load_keeps_source_structure_and_visible_knowledge_but_no_document_inputs() {
    let mut graph = seeded();
    graph.artifact_reads = 0;
    graph.targeted_reads.clear();

    let semantic = load(&mut PluginContext::for_test(&mut graph), PROJECT, None).unwrap();

    assert_source_only(
        &semantic.elements,
        &semantic.relationships,
        &root_id(&graph),
    );
    assert_eq!(
        semantic
            .artifacts
            .iter()
            .map(|artifact| artifact.artifact_id.as_str())
            .collect::<Vec<_>>(),
        ["requirement:search"]
    );
    assert!(
        semantic
            .artifacts
            .iter()
            .all(|artifact| artifact.artifact_kind != "knowledge_document")
    );
    assert_eq!(
        graph.artifact_reads, 0,
        "the captured snapshot is the only input"
    );
    assert!(graph.targeted_reads.is_empty());
}

#[test]
fn projection_snapshot_separates_documents_from_source_inputs_in_one_capture() {
    let mut graph = seeded();
    graph.artifact_reads = 0;
    graph.targeted_reads.clear();

    let projection =
        load_projection_snapshot(&mut PluginContext::for_test(&mut graph), PROJECT).unwrap();

    assert_source_only(
        &projection.semantic.elements,
        &projection.semantic.relationships,
        &root_id(&graph),
    );
    assert_eq!(
        projection
            .artifacts
            .iter()
            .map(|artifact| artifact.artifact_id.as_str())
            .collect::<Vec<_>>(),
        ["requirement:search"]
    );
    let documents: Vec<(&str, &str)> = projection
        .documents
        .iter()
        .map(|document| (document.document_id.as_str(), document.content.as_str()))
        .collect();
    assert_eq!(
        documents,
        [("overview", "Overview body"), ("second", "Second body")]
    );
    assert_eq!(graph.artifact_reads, 0);
    assert!(graph.targeted_reads.is_empty());
}

#[test]
fn selective_subgraph_drops_document_neighbours_and_their_citation_edges() {
    let mut graph = seeded();
    let raw = graph
        .host_call(
            "storage.semantic",
            json!({"operation": "selective_subgraph", "project_root": PROJECT,
                "root_element_id": "folder:src", "artifact_namespace": "knowledge"}),
        )
        .unwrap();
    let raw_ids = element_ids(
        raw["subgraph"]["external_elements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|element| element["semantic_element_id"].as_str().unwrap()),
    );
    assert!(
        raw_ids.iter().any(|id| graph.elements.get(id).is_some_and(
            |row| row["metadata"]["indexer_metadata"]["knowledge_document"]["document_id"]
                == "overview"
        )),
        "the owner returns document neighbours; this test proves they are dropped"
    );

    graph.artifact_reads = 0;
    let subgraph = load_selective_subgraph(
        &mut PluginContext::for_test(&mut graph),
        PROJECT,
        "folder:src",
    )
    .unwrap()
    .expect("selective subgraph");

    assert_eq!(
        element_ids(
            subgraph
                .elements
                .iter()
                .map(|element| element.semantic_element_id.as_str())
        ),
        SOURCE_IDS.iter().map(|id| id.to_string()).collect()
    );
    assert!(subgraph.relationships.iter().all(|relationship| {
        !relationship
            .source_element_id
            .starts_with("knowledge-document:")
            && !relationship
                .target_element_id
                .starts_with("knowledge-document:")
    }));
    assert_eq!(subgraph.relationships.len(), 3);
    assert_eq!(
        subgraph
            .artifacts
            .iter()
            .map(|artifact| artifact.artifact_id.as_str())
            .collect::<Vec<_>>(),
        ["requirement:search"]
    );
    assert_eq!(graph.artifact_reads, 0);
}
