//! Immutable record history through the public semantic persistence boundary.
mod common;
use lumvise_db_core::{
    LocalPersistence, ProjectSnapshotScope, SemanticElement, SemanticOperation,
    SemanticPersistence, SemanticProjectSnapshot, SemanticResult,
};
use lumvise_resource_routing::InvocationControl;
use serde_json::json;

fn source_revision(body: &str) -> SemanticElement {
    SemanticElement {
        revision_dependencies: Vec::new(),
        project_root: "/cooperation".into(),
        semantic_element_id: "indexed-function".into(),
        semantic_source_id: "source".into(),
        path: "src/main.rs".into(),
        element_kind: "function".into(),
        name: "work".into(),
        parent_element_id: None,
        content_fingerprint: None,
        start_line: Some(1),
        end_line: Some(3),
        lifecycle: "active".into(),
        match_evidence: None,
        metadata: json!({ "body": body }),
    }
}

fn publish_source(persistence: &LocalPersistence, body: &str) {
    let mut publication = common::RevisionTestClient::new(persistence, "/cooperation", "local")
        .publication("source", vec![source_revision(body)]);
    publication.operation_id = format!("source:{}", publication.expected.generation);
    let result = persistence
        .execute(
            SemanticOperation::PublishRevisions(publication),
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    assert!(matches!(
        result,
        SemanticResult::RevisionPublication(
            lumvise_db_core::SemanticRevisionPublicationOutcome::Accepted(_)
        )
    ));
}

fn selected_graph(persistence: &LocalPersistence) -> SemanticProjectSnapshot {
    match persistence
        .execute(
            SemanticOperation::ProjectSnapshot {
                scope: ProjectSnapshotScope::ProjectRoot("/cooperation".into()),
                artifact_namespace: None,
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap()
    {
        SemanticResult::ProjectSnapshot(snapshot) => snapshot,
        other => panic!("Result {other:?}; expected selected project snapshot"),
    }
}

#[test]
fn updating_source_preserves_the_body_of_its_exact_prior_record_id() {
    let persistence = LocalPersistence::in_memory().unwrap();
    publish_source(&persistence, "first body");
    let previous = selected_graph(&persistence).elements.remove(0);
    publish_source(&persistence, "second body");

    let result = persistence
        .execute(
            SemanticOperation::Element {
                semantic_element_id: previous.semantic_element_id.clone(),
            },
            &InvocationControl::sixty_seconds(),
        )
        .unwrap();
    let SemanticResult::Element(Some(retained)) = result else {
        panic!("Result {result:?}; expected retained exact element revision");
    };
    assert_eq!(retained.metadata, previous.metadata);
    let current = selected_graph(&persistence).elements.remove(0);
    assert_ne!(current.semantic_element_id, previous.semantic_element_id);
    assert_eq!(current.metadata["body"], "second body");
}

#[test]
fn an_unchanged_source_record_reuses_its_existing_revision_id() {
    let persistence = LocalPersistence::in_memory().unwrap();
    publish_source(&persistence, "unchanged body");
    let previous = selected_graph(&persistence).elements.remove(0);
    publish_source(&persistence, "unchanged body");
    assert_eq!(selected_graph(&persistence).elements, vec![previous]);
}
