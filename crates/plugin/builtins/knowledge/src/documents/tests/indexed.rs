use super::*;
use crate::{documents, semantic_context::SemanticElement};

fn captured_artifacts(graph: &FakeDocumentGraph) -> Vec<Value> {
    graph.artifacts.values().cloned().collect()
}

fn indexed(graph: &FakeDocumentGraph) -> Vec<SemanticElement> {
    graph
        .elements
        .values()
        .cloned()
        .map(|record| serde_json::from_value(record).unwrap())
        .collect()
}

fn source_element() -> SemanticElement {
    serde_json::from_value(json!({"project_root":"/project","semantic_element_id":"file:source","semantic_source_id":"source",
        "path":"src/lib.rs","element_kind":"file","name":"lib.rs","parent_element_id":null,"content_fingerprint":null,
        "start_line":1,"end_line":2,"lifecycle":"active","metadata":{"provider_instance_id":"filesystem"}})).unwrap()
}

#[test]
fn one_captured_revision_partitions_documents_without_turning_them_into_generation_sources() {
    let mut graph = FakeDocumentGraph::default();
    let document = graph
        .call("create_document", draft("report", "/project"))
        .unwrap()["document"]
        .clone();
    let mut elements = indexed(&graph);
    elements.push(source_element());
    let artifacts = captured_artifacts(&graph);
    let partition =
        documents::partition_indexed_elements("/project", elements, &artifacts).unwrap();
    assert_eq!(partition.source_elements.len(), 1);
    assert_eq!(
        partition.source_elements[0].semantic_element_id,
        "file:source"
    );
    assert_eq!(partition.documents.len(), 1);
    assert_eq!(json!(partition.documents[0]), document);
}

#[test]
fn deleted_index_records_do_not_reappear_as_source_elements_or_visible_documents() {
    let mut graph = FakeDocumentGraph::default();
    graph
        .call("create_document", draft("report", "/project"))
        .unwrap();
    graph
        .call("delete_document", identity("report", "/project"))
        .unwrap();
    let artifacts = captured_artifacts(&graph);
    let partition =
        documents::partition_indexed_elements("/project", indexed(&graph), &artifacts).unwrap();
    assert!(partition.source_elements.is_empty());
    assert!(partition.documents.is_empty());
    assert_eq!(
        documents::known_source_documents(&mut PluginContext::for_test(&mut graph), "/project")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn batched_document_reads_use_exact_scope_and_do_not_read_unrequested_or_foreign_content() {
    let mut graph = FakeDocumentGraph::default();
    graph
        .call("create_document", draft("wanted", "/project"))
        .unwrap();
    graph
        .call("create_document", draft("other", "/project"))
        .unwrap();
    graph
        .call("create_document", draft("foreign", "/private"))
        .unwrap();
    graph.reject_unscoped_reads = true;
    let ids = std::collections::HashSet::from(["wanted".into(), "foreign".into()]);
    let documents =
        documents::find_documents(&mut PluginContext::for_test(&mut graph), "/project", &ids)
            .unwrap();
    assert_eq!(
        documents
            .iter()
            .map(|document| document.document_id.as_str())
            .collect::<Vec<_>>(),
        ["wanted"]
    );
    assert_eq!(
        graph.targeted_reads.last().unwrap()["project_root"],
        "/project"
    );
}

#[test]
fn malformed_or_falsely_owned_document_payloads_fail_instead_of_becoming_sources() {
    let mut graph = FakeDocumentGraph::default();
    graph
        .call("create_document", draft("report", "/project"))
        .unwrap();
    for corrupt in ["missing", "foreign_provider", "wrong_path"] {
        let mut element = indexed(&graph).remove(0);
        match corrupt {
            "missing" => element.metadata["indexer_metadata"]["knowledge_document"] = Value::Null,
            "foreign_provider" => element.metadata["provider_instance_id"] = json!("filesystem"),
            "wrong_path" => element.path = "src/other.rs".into(),
            _ => unreachable!(),
        }
        assert!(
            documents::partition_indexed_elements(
                "/project",
                vec![element],
                &captured_artifacts(&graph)
            )
            .is_err(),
            "{corrupt}"
        );
    }
}

#[test]
fn a_misrouted_snapshot_element_fails_before_any_foreign_body_is_exposed() {
    let mut element = source_element();
    element.project_root = "/private".into();
    assert!(documents::partition_indexed_elements("/project", vec![element], &[]).is_err());
}
