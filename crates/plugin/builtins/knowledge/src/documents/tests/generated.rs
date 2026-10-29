use super::*;
use crate::documents::{self, DocumentGenerator};
use lumvise_contracts::KnowledgeDocument;

fn proposal() -> KnowledgeDocument {
    documents::draft_document(serde_json::from_value(json!({"document_id":"generated:context","project_root":"/project",
        "document_kind":"context","title":"Source purpose","content":"Generated purpose", "tags":["cultivation-functional"],
        "metadata":{"job":"Original purpose","provenance":{"source":"file:a"}},
        "references":[{"target_kind":"semantic_element","semantic_element_id":"file:a"}]})).unwrap())
}

fn save(
    graph: &mut FakeDocumentGraph,
    proposal: KnowledgeDocument,
    revision: Option<u64>,
) -> Result<KnowledgeDocument, PluginError> {
    let observed = revision.map(|revision| {
        let current: KnowledgeDocument = serde_json::from_value(read(graph)).unwrap();
        let mut observed = documents::ObservedDocumentRevision::from(&current);
        observed.revision = revision;
        observed
    });
    documents::save_generated(
        &mut PluginContext::for_test(graph),
        &DocumentGenerator::FunctionalContext.owner("file:a"),
        proposal,
        observed,
    )
}

fn read(graph: &mut FakeDocumentGraph) -> Value {
    graph
        .call("get_document", identity("generated:context", "/project"))
        .unwrap()["document"]
        .clone()
}

#[test]
fn generated_content_uses_the_existing_document_owner_and_revision_without_an_artifact_copy() {
    let mut graph = FakeDocumentGraph::default();
    let created = save(&mut graph, proposal(), None).unwrap();
    assert_eq!(created.lifecycle.revision, 1);
    assert_eq!(read(&mut graph)["content"], "Generated purpose");
    let bodies = graph.artifacts.len();
    assert_eq!(save(&mut graph, proposal(), Some(1)).unwrap(), created);
    assert_eq!(graph.artifacts.len(), bodies);
    // The project root and the single generated document node.
    assert_eq!(graph.elements.len(), 2);
    graph.reject_unscoped_reads = true;
    let mut changed = proposal();
    changed.content = "New generated purpose".into();
    assert_eq!(
        save(&mut graph, changed, Some(1))
            .unwrap()
            .lifecycle
            .revision,
        2
    );
    assert_eq!(read(&mut graph)["content"], "New generated purpose");
}

#[test]
fn automatic_updates_preserve_authored_grouping_tags_and_unrelated_metadata() {
    let mut graph = FakeDocumentGraph::default();
    let created = save(&mut graph, proposal(), None).unwrap();
    let mut metadata = created.metadata.clone();
    metadata["human"] = json!({"color":"blue"});
    graph
        .call(
            "update_document",
            revision_request(
                &json!(created),
                json!({"path":"My group","tags":["human-tag"],"metadata":metadata}),
            ),
        )
        .unwrap();
    let mut changed = proposal();
    changed.content = "New generated purpose".into();
    let updated = save(&mut graph, changed, Some(2)).unwrap();
    assert_eq!(updated.path.as_deref(), Some("My group"));
    assert_eq!(updated.tags, ["human-tag"]);
    assert_eq!(updated.metadata["human"], json!({"color":"blue"}));
    assert_eq!(updated.content, "New generated purpose");
}

#[test]
fn authored_content_references_titles_and_owned_metadata_hold_automatic_replacement() {
    for patch in [
        json!({"content":"Human text"}),
        json!({"title":"Human title"}),
        json!({"references":[]}),
        json!({"metadata":{"job":"Human correction"}}),
    ] {
        let mut graph = FakeDocumentGraph::default();
        let created = save(&mut graph, proposal(), None).unwrap();
        let mut request = revision_request(&json!(created), patch.clone());
        if let Some(metadata) = patch.get("metadata") {
            let mut preserved = created.metadata.clone();
            preserved
                .as_object_mut()
                .unwrap()
                .extend(metadata.as_object().unwrap().clone());
            request["metadata"] = preserved;
        }
        let authored = graph.call("update_document", request).unwrap()["document"].clone();
        assert_eq!(
            save(&mut graph, proposal(), Some(2)).unwrap_err().code,
            "generated_document_authored"
        );
        assert_eq!(read(&mut graph), authored);
    }
}

#[test]
fn automatic_writes_require_the_current_revision_and_cannot_recreate_deleted_documents() {
    let mut graph = FakeDocumentGraph::default();
    let original = save(&mut graph, proposal(), None).unwrap();
    assert_eq!(
        save(&mut graph, proposal(), None).unwrap(),
        original,
        "an identical accepted operation replays before mutable-state validation"
    );
    let mut changed = proposal();
    changed.content = "A distinct automatic proposal".into();
    assert_eq!(
        save(&mut graph, changed.clone(), None).unwrap_err().code,
        "document_revision_conflict"
    );
    assert_eq!(
        save(&mut graph, changed.clone(), Some(0)).unwrap_err().code,
        "document_revision_conflict"
    );
    graph
        .call(
            "delete_document",
            revision_request(&json!(original), json!({})),
        )
        .unwrap();
    assert_eq!(
        save(&mut graph, changed, None).unwrap_err().code,
        "generated_document_deleted"
    );
    assert!(
        graph
            .call("get_document", identity("generated:context", "/project"))
            .is_err()
    );
}

#[test]
fn another_source_or_workflow_cannot_take_over_the_same_generated_identity() {
    let mut graph = FakeDocumentGraph::default();
    let original = save(&mut graph, proposal(), None).unwrap();
    let failure = documents::save_generated(
        &mut PluginContext::for_test(&mut graph),
        &DocumentGenerator::FunctionalContext.owner("file:b"),
        proposal(),
        Some(documents::ObservedDocumentRevision::from(&original)),
    )
    .unwrap_err();
    assert_eq!(failure.code, "generated_document_authored");
    assert_eq!(read(&mut graph), json!(original));
    let mut report = proposal();
    report.document_kind = lumvise_contracts::KnowledgeDocumentKind::Report;
    assert_eq!(
        save(&mut graph, report, Some(1)).unwrap_err().code,
        "invalid_generated_document"
    );
}

#[test]
fn unowned_documents_and_archived_generated_bodies_remain_preserved() {
    let mut graph = FakeDocumentGraph::default();
    let request = json!({"project_root":"/project","document_id":"generated:context","document_kind":"context","title":"Human context","content":"Keep this body"});
    graph.call("create_document", request).unwrap();
    assert_eq!(
        save(&mut graph, proposal(), Some(1)).unwrap_err().code,
        "generated_document_authored"
    );
    assert_eq!(read(&mut graph)["content"], "Keep this body");
    let mut fresh = FakeDocumentGraph::default();
    let created = save(&mut fresh, proposal(), None).unwrap();
    fresh
        .call(
            "update_document",
            revision_request(&json!(created), json!({"status":"archived"})),
        )
        .unwrap();
    assert_eq!(
        save(&mut fresh, proposal(), Some(2)).unwrap_err().code,
        "generated_document_inactive"
    );
    assert_eq!(read(&mut fresh)["status"], "archived");
    let mut expired = FakeDocumentGraph::default();
    save(&mut expired, proposal(), None).unwrap();
    // The lifecycle lives in the staged body; the node carries only its pointer.
    let body = expired
        .elements
        .values()
        .find(|row| row["element_kind"] == "virtual_document")
        .unwrap()["metadata"]["indexer_metadata"]["knowledge_document"]["artifact_id"]
        .as_str()
        .unwrap()
        .to_owned();
    expired.artifacts.get_mut(&body).unwrap()["metadata"]["knowledge_document"]["expires_at"] =
        json!("2000-01-01T00:00:00Z");
    expired
        .revision_artifact_overrides
        .insert(body.clone(), Some(expired.artifacts[&body].clone()));
    assert_eq!(
        save(&mut expired, proposal(), Some(1)).unwrap_err().code,
        "generated_document_inactive"
    );
    assert_eq!(read(&mut expired)["expires_at"], "2000-01-01T00:00:00Z");
}

#[test]
fn generation_cannot_overwrite_a_knowledge_artifact_with_the_same_identity() {
    let mut graph = FakeDocumentGraph::default();
    let original = json!({"artifact_id":"generated:context","artifact_kind":"specification","content":"Original authored constraints",
        "metadata":{"knowledge":{"project_root":"/project"}}});
    graph
        .artifacts
        .insert("generated:context".into(), original.clone());
    assert_eq!(
        save(&mut graph, proposal(), None).unwrap_err().code,
        "document_id_conflict"
    );
    assert_eq!(graph.writes, 0);
    assert_eq!(graph.artifacts["generated:context"], original);
    assert!(graph.elements.is_empty());
}
